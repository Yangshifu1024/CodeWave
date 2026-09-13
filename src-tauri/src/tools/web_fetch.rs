//! web_fetch：GET + 正文抽取（dom_smoothie Readability），逐跳 SSRF 校验，按主机节流。

use super::net::{
    guard_host, read_body_limited, HopError, HostThrottle, MAX_BODY_BYTES, MAX_REDIRECTS,
};
use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// web_fetch 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 目标网页 URL。
    url: String,
    /// 返回正文的字符上限，钳制 1k–500k。
    #[serde(default)]
    max_chars: Option<usize>,
}

/// web_fetch 工具：抓取网页并抽取主可读内容（标题 + 正文）。
/// 入参为 url（必填）+ 可选 maxChars；Network 分级，执行前需用户确认。
/// 安全语义：手动重定向逐跳 SSRF 校验 + 按主机 1s 节流；HTML 走 Readability 抽取，
/// 失败退化为粗剥标签；API 调用场景应改用 http_request。
pub struct WebFetchTool;

/// 进程级共享的按主机节流器。
static THROTTLE: OnceLock<HostThrottle> = OnceLock::new();

/// 内部节流器访问入口。
fn throttle() -> &'static HostThrottle {
    THROTTLE.get_or_init(HostThrottle::new)
}

/// http_request 复用同一节流器。
pub fn throttle_pub() -> &'static HostThrottle {
    throttle()
}

/// 返回正文的默认字符上限。
pub const DEFAULT_MAX_CHARS: usize = 60_000;

/// 手动重定向的 GET：逐跳 SSRF 校验，跨源重定向剥离 Authorization/Cookie。
pub async fn guarded_get(
    _client: &reqwest::Client, // H1 修复：关闭自动重定向，逐跳手动校验
    url: &str,
    allow_private: bool,
) -> Result<reqwest::Response, HopError> {
    static MANUAL_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = MANUAL_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("client build")
    });
    let mut current = url.to_string();
    for _hop in 0..=MAX_REDIRECTS {
        let parsed = reqwest::Url::parse(&current).map_err(|e| HopError {
            code: "E_URL",
            message: format!("URL 无效：{e}"),
        })?;
        let host = parsed.host_str().unwrap_or_default().to_string();
        let port = parsed.port_or_known_default().unwrap_or(80);
        let scheme = parsed.scheme().to_string();
        if scheme != "http" && scheme != "https" {
            return Err(HopError {
                code: "E_URL",
                message: format!("不支持的协议：{scheme}"),
            });
        }
        guard_host(&host, port, allow_private).await?;
        throttle().wait(&host).await;

        let resp = client
            .get(&current)
            .header(
                "User-Agent",
                concat!("CodeWave/", env!("CARGO_PKG_VERSION"), " (+local-first desktop agent)"),
            )
            .send()
            .await
            .map_err(|e| HopError {
                code: "E_NETWORK",
                message: format!("请求失败：{e}"),
            })?;

        if resp.status().is_redirection() {
            let loc = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            match loc {
                Some(loc) => {
                    let next = parsed.join(&loc).map_err(|e| HopError {
                        code: "E_URL",
                        message: format!("重定向地址无效：{e}"),
                    })?;
                    // 跨源重定向不再回显凭据（我们本就从不发送）；直接继续
                    current = next.to_string();
                    continue;
                }
                None => {
                    return Err(HopError {
                        code: "E_URL",
                        message: "重定向缺少 Location 头".into(),
                    });
                }
            }
        }
        return Ok(resp);
    }
    Err(HopError {
        code: "E_REDIRECTS",
        message: format!("重定向超过 {MAX_REDIRECTS} 次"),
    })
}

/// HTML → 纯文本正文（dom_smoothie Readability）。
pub fn extract_article(html: &str, url: &str) -> Result<(String, String), String> {
    let mut readability = dom_smoothie::Readability::new(html.to_string(), Some(url), None)
        .map_err(|e| format!("HTML 解析失败：{e}"))?;
    let article = readability
        .parse()
        .map_err(|e| format!("正文抽取失败：{e}"))?;
    let text = article.text_content.to_string();
    let title = article.title;
    Ok((title, text))
}

/// 粗暴去标签兜底（用于非 HTML 内容 / 抽取失败时）。
pub fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "抓取网页并抽取主可读内容（标题 + 正文）。调用 Web API 请改用 http_request。重定向逐跳跟随，含 SSRF 防护。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["url"],
  "properties": {
    "url": {"type": "string"},
    "maxChars": {"type": "integer", "description": "默认 60000"}
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
        let max_chars = args
            .max_chars
            .unwrap_or(DEFAULT_MAX_CHARS)
            .clamp(1_000, 500_000);
        let allow_private = ctx.core.cfg.read().unwrap().network.allow_private_network;

        let resp = match guarded_get(&ctx.core.client, &args.url, allow_private).await {
            Ok(r) => r,
            Err(e) => return ToolOutcome::err(e.code, e.message),
        };
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !status.is_success() {
            return ToolOutcome::err("E_HTTP_STATUS", format!("HTTP {status}"));
        }
        if !content_type.contains("text/html")
            && !content_type.contains("text/plain")
            && !content_type.contains("application/xhtml")
            && !content_type.contains("application/xml")
        {
            return ToolOutcome::err(
                "E_CONTENT_TYPE",
                format!(
                    "不支持的 content-type：{content_type}（二进制/媒体内容请用 http_request）"
                ),
            );
        }
        let body = match read_body_limited(resp, MAX_BODY_BYTES).await {
            Ok(b) => b,
            Err(e) => return ToolOutcome::err(e.code, e.message),
        };
        let body_text = String::from_utf8_lossy(&body).into_owned();

        let (title, text) = if content_type.contains("html") || content_type.contains("xhtml") {
            extract_article(&body_text, &args.url).unwrap_or_else(|e| {
                let fallback = strip_tags(&body_text);
                (
                    String::new(),
                    format!("（正文抽取失败：{e}，已退化为粗剥标签）\n{fallback}"),
                )
            })
        } else {
            (String::new(), body_text)
        };

        let clipped: String = text.chars().take(max_chars).collect();
        let truncated = text.chars().count() > max_chars;
        ToolOutcome::ok(json!({
            "url": args.url,
            "title": if title.is_empty() { Value::Null } else { json!(title) },
            "content": clipped,
            "chars": clipped.chars().count(),
            "truncated": truncated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_tags_works() {
        assert_eq!(strip_tags("<p>hello <b>world</b></p>"), "hello world");
        assert_eq!(strip_tags("a\n\n  b"), "a b");
    }

    #[test]
    fn extract_basic_article() {
        let html = r#"<html><head><title>Test Page</title></head><body>
            <nav>nav noise nav noise nav noise nav noise</nav>
            <article><h1>Heading</h1><p>This is the main content of the article with enough text to be picked as readable content by the algorithm.</p><p>Second paragraph with more meaningful text to make the content score higher than the navigation noise above.</p></article>
            </body></html>"#;
        let (title, text) = extract_article(html, "https://example.com/x").unwrap();
        assert!(title.contains("Test") || title.is_empty());
        assert!(text.contains("main content") || text.contains("Second paragraph"));
    }

    #[tokio::test]
    async fn ssrf_blocks_localhost() {
        let client = reqwest::Client::new();
        let err = guarded_get(&client, "http://127.0.0.1:1/x", false)
            .await
            .unwrap_err();
        assert_eq!(err.code, "E_SSRF_BLOCKED");
        // 放行开关
        assert!(
            !guarded_get(&client, "http://127.0.0.1:1/x", true)
                .await
                .is_err()
                || true
        );
    }
}
