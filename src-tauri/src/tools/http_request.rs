//! http_request：通用 REST 调用（结构化预览：状态码 / 脱敏响应头 / 截断响应体），同套 SSRF 守卫。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

/// http_request 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 目标 URL（可带 query 由 query 参数补充）。
    url: String,
    /// HTTP 方法，默认 GET。
    #[serde(default)]
    method: Option<String>,
    /// 请求头。
    #[serde(default)]
    headers: Option<std::collections::HashMap<String, String>>,
    /// query 参数（自动 urlencode 后拼接到 URL）。
    #[serde(default)]
    query: Option<std::collections::HashMap<String, String>>,
    /// 请求体：JSON 值（字符串同样按 JSON 字符串发送）。
    #[serde(default)]
    body: Option<Value>,
    /// 超时秒数，默认 60、钳制 1–120。
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// 允许的 HTTP 方法白名单（排除 TRACE / CONNECT 等）。
const ALLOWED_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
/// JSON 响应体的预览字符上限。
const JSON_PREVIEW_LIMIT: usize = 24 * 1024;
/// 文本响应体的预览字符上限。
const TEXT_PREVIEW_LIMIT: usize = 96 * 1024;

/// http_request 工具：对 Web API 发起通用 HTTP 请求（JSON 友好）。
/// 入参为 url（必填）+ method/headers/query/body/timeoutSeconds；Network 分级。
/// 安全语义：统一请求执行器逐跳校验/固定地址并节流，重定向手动逐跳跟随（关闭自动重定向），
/// 响应头脱敏（set-cookie / www-authenticate 置 [redacted]），响应体按类型截断预览。
pub struct HttpRequestTool;

#[async_trait::async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &'static str {
        "http_request"
    }
    fn description(&self) -> &'static str {
        "向 Web API 发起 HTTP 请求（JSON 友好）。方法支持 GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS。响应预览：状态行、脱敏后的响应头、截断的响应体。读取网页请改用 web_fetch。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["url"],
  "properties": {
    "url": {"type": "string"},
    "method": {"type": "string", "description": "默认 GET"},
    "headers": {"type": "object", "additionalProperties": {"type": "string"}},
    "query": {"type": "object", "additionalProperties": {"type": "string"}},
    "body": {"description": "请求体：JSON 值，字符串同样按 JSON 字符串编码"},
    "timeoutSeconds": {"type": "integer", "description": "总预算秒数，默认 60，范围 1–120"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let method = args
            .method
            .unwrap_or_else(|| "GET".into())
            .to_ascii_uppercase();
        if !ALLOWED_METHODS.contains(&method.as_str()) {
            return ToolOutcome::err("E_ARGS", format!("不允许的方法 {method}"));
        }

        if ctx
            .rt
            .delivery_review_assignment_id
            .lock()
            .unwrap()
            .is_some()
            || ctx
                .rt
                .delivery_verification_task_id
                .lock()
                .unwrap()
                .is_some()
        {
            return ToolOutcome::err(
                "E_DUTY_READONLY",
                "独立审查/验证职责不开放通用 HTTP API 工具，请使用 web_fetch 读取",
            );
        }
        let _effect = if matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
            None
        } else {
            match ctx.acquire_effect(&[], true) {
                Ok(guard) => Some(guard),
                Err(message) => return ToolOutcome::err("E_WORKFLOW_OWNERSHIP", message),
            }
        };

        let mut url = match reqwest::Url::parse(&args.url) {
            Ok(url) => url,
            Err(_) => return ToolOutcome::err("E_URL", "URL 无效"),
        };
        if let Some(query) = &args.query {
            let mut pairs: Vec<_> = query.iter().collect();
            pairs.sort_by_key(|(key, _)| *key);
            let extra = pairs
                .into_iter()
                .map(|(key, value)| format!("{}={}", urlencode(key), urlencode(value)))
                .collect::<Vec<_>>()
                .join("&");
            if !extra.is_empty() {
                let query = url
                    .query()
                    .filter(|q| !q.is_empty())
                    .map(|query| format!("{query}&{extra}"))
                    .unwrap_or(extra);
                url.set_query(Some(&query));
            }
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in args.headers.unwrap_or_default() {
            let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                return ToolOutcome::err("E_ARGS", "请求头名称无效");
            };
            let Ok(value) = reqwest::header::HeaderValue::from_str(&value) else {
                return ToolOutcome::err("E_ARGS", "请求头值无效");
            };
            // The URL and HTTP stack own connection routing and framing.
            if matches!(
                name.as_str(),
                "host" | "content-length" | "transfer-encoding" | "proxy-authorization"
            ) {
                return ToolOutcome::err("E_ARGS", "请求头不能覆盖连接目标或消息边界");
            }
            headers.insert(name, value);
        }
        let cfg = ctx.core.cfg.read().unwrap().clone();
        let request = super::net::request::HttpRequest {
            url,
            method: method.parse().unwrap(),
            headers,
            body: args.body,
        };
        let timeout =
            std::time::Duration::from_secs(args.timeout_seconds.unwrap_or(60).clamp(1, 120));
        let resp = match super::net::request::execute(&cfg, request, &ctx.cancel, timeout).await {
            Ok(response) => response,
            Err(error) => return ToolOutcome::err(error.code, error.message),
        };
        let status = resp.status;
        // 响应头脱敏
        let mut headers_out = serde_json::Map::new();
        for (k, v) in &resp.headers {
            let key = k.as_str().to_ascii_lowercase();
            if matches!(key.as_str(), "set-cookie" | "www-authenticate") {
                headers_out.insert(key, json!("[redacted]"));
            } else {
                headers_out.insert(key, json!(v.to_str().unwrap_or("<binary>")));
            }
        }
        let content_type = headers_out
            .get("content-type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let bytes = resp.body;

        let body_preview: Value;
        let mut binary = false;
        if content_type.contains("json") || content_type.contains("javascript") {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) => {
                    let pretty = serde_json::to_string_pretty(&v).unwrap_or_default();
                    body_preview = clip(&pretty, JSON_PREVIEW_LIMIT);
                }
                Err(_) => body_preview = clip(&String::from_utf8_lossy(&bytes), JSON_PREVIEW_LIMIT),
            }
        } else if content_type.starts_with("text/")
            || content_type.contains("xml")
            || content_type.contains("html")
        {
            body_preview = clip(&String::from_utf8_lossy(&bytes), TEXT_PREVIEW_LIMIT);
        } else {
            binary = true;
            body_preview = Value::Null;
        }

        ToolOutcome::ok(json!({
            "status": status.as_u16(),
            "status_text": status.canonical_reason().unwrap_or(""),
            "headers": Value::Object(headers_out),
            "body": body_preview,
            "binary": binary,
            "binary_size": if binary { json!(bytes.len()) } else { Value::Null },
            "binary_mime": if binary { json!(content_type) } else { Value::Null },
        }))
    }
}

/// 按字符数截断为 JSON 字符串值（超限时追加截断提示与总字符数）。
fn clip(s: &str, limit: usize) -> Value {
    let n = s.chars().count();
    if n <= limit {
        json!(s)
    } else {
        let clipped: String = s.chars().take(limit).collect();
        json!(format!("{clipped}\n…[截断，共 {n} 字符]"))
    }
}

/// query 值的百分号编码（保留 RFC 3986 unreserved 字符）。
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    type Received = Arc<Mutex<Vec<String>>>;

    struct HttpFixture {
        // Keep the port reserved until the client and worker have both finished.
        _listener: Arc<TcpListener>,
        worker: std::thread::JoinHandle<()>,
    }

    impl HttpFixture {
        fn join(self) -> std::thread::Result<()> {
            self.worker.join()
        }
    }

    /// 一次性串行 HTTP 服务：对前 N 个连接各回一个响应，并记录每个原始请求（头 + 体）。
    /// `mk_responses` 拿到绑定地址，重定向目标可引用它而避免端口竞态。
    fn start_server(
        mk_responses: impl FnOnce(SocketAddr) -> Vec<Vec<u8>>,
    ) -> (SocketAddr, Received, HttpFixture) {
        let listener = Arc::new(TcpListener::bind("127.0.0.1:0").unwrap());
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let responses = mk_responses(addr);
        let received: Received = Arc::new(Mutex::new(Vec::new()));
        let recv = received.clone();
        let server_listener = listener.clone();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(65);
            for resp in responses {
                let mut stream = loop {
                    match server_listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "HTTP fixture accept timed out"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("HTTP fixture accept failed: {error}"),
                    }
                };
                // macOS accept can inherit O_NONBLOCK from the polling listener.
                // The request reader below uses blocking I/O and socket timeouts.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                recv.lock().unwrap().push(read_request(&mut stream));
                let _ = stream.write_all(&resp);
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        (
            addr,
            received,
            HttpFixture {
                _listener: listener,
                worker: handle,
            },
        )
    }

    /// 从流中读取一个 HTTP 请求（头 + Content-Length 定界的体）。
    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                let cl: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                if buf.len() >= pos + 4 + cl {
                    return String::from_utf8_lossy(&buf).into_owned();
                }
            }
            let n = stream
                .read(&mut tmp)
                .expect("HTTP fixture request read failed");
            assert_ne!(n, 0, "HTTP fixture received an incomplete request");
            buf.extend_from_slice(&tmp[..n]);
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    fn response(status_line: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut out = format!("{status_line}\r\n");
        for (k, v) in headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
        out.push_str("Connection: close\r\n\r\n");
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn make_ctx(core: Arc<crate::core::agent::AgentCore>) -> ToolCtx {
        let ws = tempfile::tempdir().unwrap();
        let rt = core.get_or_create_session(
            "http-req-test",
            std::fs::canonicalize(ws.path()).unwrap(),
            None,
            vec![],
            None,
            vec![],
        );
        ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            provider_call_id: String::new(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn core_with(allow_private: bool) -> Arc<crate::core::agent::AgentCore> {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
            project_dir: None,
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        {
            let mut cfg = core.cfg.write().unwrap();
            cfg.network.allow_private_network = allow_private;
            cfg.proxy = Some(crate::core::config::ProxyConfig {
                mode: crate::core::config::ProxyMode::None,
                url: String::new(),
            });
        }
        core
    }

    #[tokio::test]
    async fn workflow_http_write_is_owned_but_get_remains_readonly() {
        use crate::core::workflow::dispatch;
        use std::collections::BTreeMap;
        let ctx = make_ctx(core_with(true));
        let (addr, requests, server) =
            start_server(|_| vec![response("HTTP/1.1 200 OK", &[], b"written")]);
        let out = HttpRequestTool
            .run(
                &ctx,
                json!({"url":format!("http://{addr}/"),"method":"POST"}),
            )
            .await;
        server.join().unwrap();
        assert!(out.ok, "ordinary root POST remains available: {out:?}");
        assert!(requests.lock().unwrap()[0].starts_with("POST "));
        let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&ctx.rt);
        let owner = crate::core::delivery::DeliveryService::identity_of(&ctx.rt);
        let unit = dispatch::spawn_unit("http-child", &["workspace:child.txt".into()]);
        let leases = dispatch::admission(
            &owner,
            &owner.root_run_id,
            dispatch::current_generation(&owner.root_run_id),
            &[(unit, vec![])],
            &roots,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        let _lease = dispatch::LeaseGuard(leases.into_iter().next().unwrap());
        // No server is needed: the write must be refused before opening a socket.
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let out = HttpRequestTool
                .run(&ctx, json!({"url":"http://127.0.0.1:1/","method":method}))
                .await;
            assert_eq!(
                out.error.as_ref().unwrap().code,
                "E_WORKFLOW_OWNERSHIP",
                "{out:?}"
            );
        }
        let (addr, _, server) = start_server(|_| vec![response("HTTP/1.1 200 OK", &[], b"read")]);
        let out = HttpRequestTool
            .run(
                &ctx,
                json!({"url":format!("http://{addr}/"),"method":"GET"}),
            )
            .await;
        server.join().unwrap();
        assert!(out.ok, "GET remains available beside a child: {out:?}");
    }

    #[tokio::test]
    async fn review_and_verification_do_not_gain_generic_http_capability() {
        let ctx = make_ctx(core_with(true));
        *ctx.rt.delivery_review_assignment_id.lock().unwrap() = Some("assignment".into());
        let out = HttpRequestTool
            .run(&ctx, json!({"url":"http://127.0.0.1:1/","method":"GET"}))
            .await;
        assert_eq!(out.error.as_ref().unwrap().code, "E_DUTY_READONLY");
        *ctx.rt.delivery_review_assignment_id.lock().unwrap() = None;
        *ctx.rt.delivery_verification_task_id.lock().unwrap() = Some("task".into());
        let out = HttpRequestTool
            .run(&ctx, json!({"url":"http://127.0.0.1:1/","method":"POST"}))
            .await;
        assert_eq!(out.error.as_ref().unwrap().code, "E_DUTY_READONLY");
    }

    #[test]
    fn clip_passthrough_and_truncation() {
        let short = json!("hello");
        assert_eq!(clip("hello", 10), short);
        let long = "x".repeat(100);
        let clipped = clip(&long, 10);
        let s = clipped.as_str().unwrap();
        assert!(s.starts_with("xxxxxxxxxx\n"));
        assert!(s.contains("…[截断，共 100 字符]"));
        // 按字符而非字节：多字节字符单独计数
        let uni = "你".repeat(30);
        let clipped = clip(&uni, 5);
        assert!(clipped.as_str().unwrap().starts_with("你你你你你\n"));
    }

    #[test]
    fn urlencode_escapes_reserved_and_keeps_unreserved() {
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("k=1&v"), "k%3D1%26v");
        assert_eq!(urlencode("safe-_.~"), "safe-_.~");
        assert_eq!(urlencode("中"), "%E4%B8%AD");
    }

    #[tokio::test]
    async fn redirect_303_switches_post_to_get_without_replaying_body() {
        let ctx = make_ctx(core_with(true));
        let (addr, received, server) = start_server(|_| {
            vec![
                response("HTTP/1.1 303 See Other", &[("Location", "/result")], b""),
                response(
                    "HTTP/1.1 200 OK",
                    &[("Content-Type", "text/plain")],
                    b"done",
                ),
            ]
        });
        let out = HttpRequestTool
            .run(
                &ctx,
                json!({
                    "url": format!("http://{addr}/submit"), "method": "POST",
                    "headers": {"content-type": "application/json"},
                    "body": {"private": "payload"}
                }),
            )
            .await;
        server.join().unwrap();
        assert!(out.ok, "{out:?}");
        let requests = received.lock().unwrap();
        assert!(requests[1].starts_with("GET /result "), "{}", requests[1]);
        assert!(!requests[1].contains("payload"));
        assert!(!requests[1].to_ascii_lowercase().contains("content-type:"));
    }

    #[tokio::test]
    async fn cross_origin_redirect_does_not_forward_custom_credentials() {
        let ctx = make_ctx(core_with(true));
        let (target, received, target_server) = start_server(|_| {
            vec![response(
                "HTTP/1.1 200 OK",
                &[("Content-Type", "text/plain")],
                b"done",
            )]
        });
        let (source, _, source_server) = start_server(|_| {
            vec![response(
                "HTTP/1.1 302 Found",
                &[("Location", &format!("http://{target}/result"))],
                b"",
            )]
        });
        let out = HttpRequestTool.run(&ctx, json!({
            "url": format!("http://{source}/start"),
            "headers": {"authorization": "Bearer secret", "cookie": "session=secret", "x-api-key": "custom-secret"}
        })).await;
        source_server.join().unwrap();
        target_server.join().unwrap();
        assert!(out.ok, "{out:?}");
        let request = &received.lock().unwrap()[0].to_ascii_lowercase();
        assert!(
            !request.contains("secret"),
            "credentials escaped: {request}"
        );
        assert!(!request.contains("authorization:"));
        assert!(!request.contains("cookie:"));
        assert!(!request.contains("x-api-key:"));
    }

    #[tokio::test]
    async fn cancelled_request_does_not_start_network_work() {
        let ctx = make_ctx(core_with(true));
        ctx.cancel.cancel();
        let out = HttpRequestTool
            .run(&ctx, json!({"url": "http://127.0.0.1:9/"}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
    }

    #[tokio::test]
    async fn args_and_method_validation() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        let tool = HttpRequestTool;
        // 缺 url → E_ARGS
        let out = tool.run(&ctx, json!({})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        // 不允许的方法
        let out = tool
            .run(
                &ctx,
                json!({"url": "http://example.com/", "method": "TRACE"}),
            )
            .await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        // 无法解析的 URL
        let out = tool.run(&ctx, json!({"url": "not a url"})).await;
        assert_eq!(out.error.unwrap().code, "E_URL");
    }

    #[tokio::test]
    async fn ssrf_guard_blocks_private_target() {
        // allow_private_network = false（默认）：回环目标在拨号前即被拒绝
        let core = core_with(false);
        let ctx = make_ctx(core);
        let tool = HttpRequestTool;
        let out = tool.run(&ctx, json!({"url": "http://127.0.0.1:9/x"})).await;
        let err = out.error.unwrap();
        assert_eq!(err.code, "E_SSRF_BLOCKED");
        assert!(err.message.contains("SSRF"));
    }

    #[tokio::test]
    async fn json_post_roundtrip_with_headers_query_and_redaction() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        let body = br#"{"ok":true,"n":1}"#;
        let (addr, received, handle) = start_server(|_addr| {
            vec![response(
                "HTTP/1.1 200 OK",
                &[
                    ("Content-Type", "application/json"),
                    ("Set-Cookie", "sid=secret; HttpOnly"),
                    ("WWW-Authenticate", "Basic realm=secret"),
                ],
                body,
            )]
        });
        let tool = HttpRequestTool;
        let out = tool
            .run(
                &ctx,
                json!({
                    "url": format!("http://{addr}/api?existing=1#fragment"),
                    "method": "post",
                    "headers": {"x-token": "secret-token"},
                    "query": {"a": "b c", "k": "中", "a&b": "v=1#end"},
                    "body": {"p": 1},
                }),
            )
            .await;
        handle.join().unwrap();
        assert!(out.ok, "{out:?}");
        let req = &received.lock().unwrap()[0];
        // 小写方法归一为大写；query 已拼接并编码
        assert!(req.starts_with("POST /api?"), "raw request: {req}");
        assert!(req.contains("a=b%20c"), "raw request: {req}");
        assert!(req.contains("k=%E4%B8%AD"), "raw request: {req}");
        assert!(req.contains("existing=1"), "raw request: {req}");
        assert!(req.contains("a%26b=v%3D1%23end"), "raw request: {req}");
        assert!(
            !req.contains("#fragment"),
            "fragments must not enter HTTP request targets"
        );
        assert!(req.contains("x-token: secret-token"), "raw request: {req}");
        assert!(req.contains(r#"{"p":1}"#), "raw request: {req}");
        // Outcome 形态
        let data = out.data;
        assert_eq!(data["status"], 200);
        assert_eq!(data["status_text"], "OK");
        assert_eq!(data["headers"]["set-cookie"], "[redacted]");
        assert_eq!(data["headers"]["www-authenticate"], "[redacted]");
        assert_eq!(data["headers"]["content-type"], "application/json");
        assert_eq!(data["binary"], json!(false));
        assert_eq!(data["binary_size"], Value::Null);
        // JSON 体以 pretty 形式重新序列化
        let body_str = data["body"].as_str().unwrap();
        assert!(body_str.contains("\"ok\": true"));
        assert_eq!(data["binary_mime"], Value::Null);
    }

    #[tokio::test]
    async fn redirect_hop_followed_then_final_response() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        let (addr, received, handle) = start_server(|_addr| {
            vec![
                response("HTTP/1.1 302 Found", &[("Location", "/final?x=1")], b""),
                response(
                    "HTTP/1.1 200 OK",
                    &[("Content-Type", "text/plain")],
                    b"landed",
                ),
            ]
        });
        let tool = HttpRequestTool;
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/start")}))
            .await;
        handle.join().unwrap();
        assert!(out.ok, "{out:?}");
        let reqs = received.lock().unwrap();
        assert_eq!(reqs.len(), 2, "redirect must be followed hop by hop");
        assert!(reqs[0].starts_with("GET /start"), "raw requests: {reqs:?}");
        assert!(
            reqs[1].starts_with("GET /final?x=1"),
            "Location must be joined onto the base URL"
        );
        assert_eq!(out.data["body"], "landed");
    }

    #[tokio::test]
    async fn redirect_chain_overflow_maps_to_e_redirects() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        // 永远重定向到自己：11 跳（MAX_REDIRECTS 循环上界）后 E_REDIRECTS
        let (addr, _received, handle) = start_server(|addr| {
            let loc = format!("http://{addr}/loop");
            (0..=super::super::net::MAX_REDIRECTS)
                .map(|_| response("HTTP/1.1 302 Found", &[("Location", loc.as_str())], b""))
                .collect()
        });
        let tool = HttpRequestTool;
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/loop")}))
            .await;
        handle.join().unwrap();
        assert!(!out.ok);
        assert_eq!(out.error.unwrap().code, "E_REDIRECTS");
    }

    #[tokio::test]
    async fn redirect_to_invalid_location_maps_to_e_url() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        let (addr, _received, handle) = start_server(|_addr| {
            vec![response(
                "HTTP/1.1 302 Found",
                // 未闭合的方括号在拼接时会被 URL 解析器拒绝
                &[("Location", "http://[")],
                b"",
            )]
        });
        let tool = HttpRequestTool;
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/x")}))
            .await;
        handle.join().unwrap();
        assert!(!out.ok);
        assert_eq!(out.error.unwrap().code, "E_URL");
    }

    #[tokio::test]
    async fn text_html_and_binary_bodies() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        let tool = HttpRequestTool;

        // text/plain 体保持原始字符串
        let (addr, _r, handle) = start_server(|_addr| {
            vec![response(
                "HTTP/1.1 200 OK",
                &[("Content-Type", "text/plain; charset=utf-8")],
                "plain 文本".as_bytes(),
            )]
        });
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/t")}))
            .await;
        handle.join().unwrap();
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["body"], "plain 文本");
        assert_eq!(out.data["binary"], json!(false));

        // 未知的二进制 content type → binary 标志 + size/mime，body 为 null
        let (addr, _r, handle) = start_server(|_addr| {
            vec![response(
                "HTTP/1.1 200 OK",
                &[("Content-Type", "application/octet-stream")],
                &[0xFF, 0x00, 0x12],
            )]
        });
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/b")}))
            .await;
        handle.join().unwrap();
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["binary"], json!(true));
        assert_eq!(out.data["body"], Value::Null);
        assert_eq!(out.data["binary_size"], 3);
        assert_eq!(out.data["binary_mime"], "application/octet-stream");

        // 声明超大的 Content-Length（50MB 上限）→ 读取前即 E_TOO_LARGE
        let (addr, _r, handle) = start_server(|_addr| {
            vec![{
                let mut out =
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n".to_vec();
                out.extend_from_slice(b"Content-Length: 60000000\r\nConnection: close\r\n\r\n");
                out
            }]
        });
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/big")}))
            .await;
        handle.join().unwrap();
        assert!(!out.ok);
        assert_eq!(out.error.unwrap().code, "E_TOO_LARGE");
    }

    #[tokio::test]
    async fn non_json_error_status_is_reported_not_failed() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        let (addr, _r, handle) = start_server(|_addr| {
            vec![response(
                "HTTP/1.1 404 Not Found",
                &[("Content-Type", "text/plain")],
                b"missing",
            )]
        });
        let tool = HttpRequestTool;
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/x")}))
            .await;
        handle.join().unwrap();
        assert!(
            out.ok,
            "HTTP-level errors surface as status, not tool failure"
        );
        assert_eq!(out.data["status"], 404);
        assert_eq!(out.data["status_text"], "Not Found");
        assert_eq!(out.data["body"], "missing");
    }

    #[tokio::test]
    async fn connection_refused_maps_to_e_network() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        // Reserve a bound, non-listening port so another fixture cannot steal it.
        let probe = tokio::net::TcpSocket::new_v4().unwrap();
        probe.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = probe.local_addr().unwrap();
        let tool = HttpRequestTool;
        let out = tool
            .run(&ctx, json!({"url": format!("http://{addr}/x")}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_NETWORK");
    }

    #[tokio::test]
    async fn timeout_seconds_clamped_and_maps_to_e_network() {
        let core = core_with(true);
        let ctx = make_ctx(core);
        // timeoutSeconds=0 clamps to one second, including any throttle wait.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            // The total budget may expire in throttling, before any connection.
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        drop(stream);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        });
        let tool = HttpRequestTool;
        let started = std::time::Instant::now();
        let out = tool
            .run(
                &ctx,
                json!({"url": format!("http://{addr}/slow"), "timeoutSeconds": 0}),
            )
            .await;
        let elapsed = started.elapsed();
        server.join().unwrap();
        assert_eq!(out.error.unwrap().code, "E_NETWORK");
        // 宽松上界：证明 1s 下限生效（60s 默认值即使算上并行测试的节流竞争也会超界）
        assert!(
            elapsed < std::time::Duration::from_secs(50),
            "timeoutSeconds=0 must clamp to the 1s floor (not the 60s default), took {elapsed:?}"
        );
    }
}
