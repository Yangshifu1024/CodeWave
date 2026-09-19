//! 可脚本化的假 LSP server（**零外部依赖改写**：只用 std + 本包已依赖的 serde_json）。
//!
//! 用途：集成测试经 `CARGO_BIN_EXE_codewave-fake-lsp` 启动，按环境变量演出各种故障与边界。
//! 环境变量一览（全部可选；不设 `CW_FAKE_LSP=1` 时本程序立即退出 0，避免被误当真 server）：
//!
//! | 变量 | 作用 |
//! |---|---|
//! | `CW_FAKE_LSP=1` | 真正跑协议；否则立即退出 0 |
//! | `CW_FAKE_LSP_LOG=<路径>` | 每个事件追加一行（`pid N` / `recv <method>` / `resp <id> <json>` / `bye`） |
//! | `CW_FAKE_LSP_MODE` | `normal`(默认) / `crash_after_init` / `garbage` / `exit_immediately` / `silent` |
//! | `CW_FAKE_LSP_DIAG=<json>` | didOpen/didChange 后推送的 `publishDiagnostics.params`：给数组则包成 params，给对象则原样发 |
//! | `CW_FAKE_LSP_DIAG_SEQ=<json 数组的数组>` | 逐次推送不同 payload（第 n 次取第 n 轮，末轮重复）——追加旋钮，用于「基线 3 条 → 改后 4 条」 |
//! | `CW_FAKE_LSP_DELAY_MS=<n>` | 推诊断前延迟（作用于 didOpen） |
//! | `CW_FAKE_LSP_EMPTY_FIRST_MS=<n>` | `didOpen` 后**先推一个空集**（占位）、等 n ms 再推 `DIAG`/`SEQ` 的真实 payload（复现冷启动「先空集、后真实」；占位空集不消耗 `SEQ` 轮次） |
//! | `CW_FAKE_LSP_EMPTY_TIMES=<n>` | `didOpen`/`didChange` 先推 n 个**空集占位**再推真实 payload（同上不消耗 `SEQ`；日志 `push empty-set`） |
//! | `CW_FAKE_LSP_ALWAYS_EMPTY=1` | **永不**推真实 payload，每次 `didOpen`/`didChange` 只推空集（复现全干净项目 / 占位流水线） |
//! | `CW_FAKE_LSP_DELAY_MS_CHANGE=<n>` | 仅 didChange 的延迟（追加旋钮；缺省回落 `DELAY_MS`） |
//! | `CW_FAKE_LSP_FRAGMENT=1` | 每条帧拆 3 段写（段间 sleep 10ms），守护分片 framing |
//! | `CW_FAKE_LSP_SERVER_REQUESTS=1` | `initialized` 后主动发 4 个请求（验证客户端「必须应答」） |
//! | `CW_FAKE_LSP_NO_LENGTH=1` | 推送诊断时故意漏掉 `Content-Length` |

use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    if std::env::var("CW_FAKE_LSP").as_deref() != Ok("1") {
        std::process::exit(0);
    }
    let cfg = Config::from_env();
    log(&cfg, &format!("pid {}", std::process::id()));
    match cfg.mode.as_str() {
        "exit_immediately" => {
            log(&cfg, "mode exit_immediately");
            return;
        }
        "garbage" => {
            // 吐非协议字节：`\r\n\r\n` 之前没有 Content-Length → 客户端应报 BadHeader 而不是挂死
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let _ = out.write_all(b"X-not-lsp\r\n\r\n{\"jsonrpc\":\"2.0\"}");
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(50));
            log(&cfg, "mode garbage");
            return;
        }
        _ => {}
    }

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut uri = String::new();
    let mut push_index = 0usize;
    loop {
        match read_frame(&mut reader) {
            Ok(Some(msg)) => {
                if handle(&cfg, &msg, &mut uri, &mut push_index) {
                    return;
                }
            }
            Ok(None) => {
                log(&cfg, "eof");
                return;
            }
            Err(e) => {
                log(&cfg, &format!("bad-frame {e}"));
                return;
            }
        }
    }
}

struct Config {
    mode: String,
    log: Option<PathBuf>,
    payload: Option<Value>,
    seq: Vec<Value>,
    delay_ms: u64,
    delay_change_ms: u64,
    /// `didOpen` 后先推空集、再等这么久才推真实 payload（0 = 关闭）
    empty_first_ms: u64,
    /// `didOpen`/`didChange` 先推这么多个空集占位再推真实 payload（0 = 关闭）
    empty_times: usize,
    /// 永不给真实 payload（只推空集）
    always_empty: bool,
    fragment: bool,
    server_requests: bool,
    no_length: bool,
}

impl Config {
    fn from_env() -> Config {
        let num = |key: &str| -> u64 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0)
        };
        let json_env = |key: &str| -> Option<Value> {
            std::env::var(key)
                .ok()
                .and_then(|v| serde_json::from_str::<Value>(v.trim()).ok())
        };
        let seq = match json_env("CW_FAKE_LSP_DIAG_SEQ") {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        };
        let delay_ms = num("CW_FAKE_LSP_DELAY_MS");
        Config {
            mode: std::env::var("CW_FAKE_LSP_MODE").unwrap_or_else(|_| "normal".into()),
            log: std::env::var("CW_FAKE_LSP_LOG").ok().map(PathBuf::from),
            payload: json_env("CW_FAKE_LSP_DIAG"),
            seq,
            delay_ms,
            delay_change_ms: match std::env::var("CW_FAKE_LSP_DELAY_MS_CHANGE") {
                Ok(v) => v.trim().parse().unwrap_or(delay_ms),
                Err(_) => delay_ms,
            },
            empty_first_ms: num("CW_FAKE_LSP_EMPTY_FIRST_MS"),
            empty_times: num("CW_FAKE_LSP_EMPTY_TIMES") as usize,
            always_empty: std::env::var("CW_FAKE_LSP_ALWAYS_EMPTY").as_deref() == Ok("1"),
            fragment: std::env::var("CW_FAKE_LSP_FRAGMENT").as_deref() == Ok("1"),
            server_requests: std::env::var("CW_FAKE_LSP_SERVER_REQUESTS").as_deref() == Ok("1"),
            no_length: std::env::var("CW_FAKE_LSP_NO_LENGTH").as_deref() == Ok("1"),
        }
    }

    /// 第 `i` 次推送用的 payload（`SEQ` 用尽后停在末轮）。
    fn payload_for(&self, i: usize) -> Option<Value> {
        if !self.seq.is_empty() {
            return self.seq.get(i.min(self.seq.len() - 1)).cloned();
        }
        self.payload.clone()
    }
}

/// 处理一条入站消息；返回 `true` = 该退出。
fn handle(cfg: &Config, msg: &Value, uri: &mut String, push_index: &mut usize) -> bool {
    let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
        // 客户端对我们请求的应答
        if let Some(id) = msg.get("id") {
            let result = msg.get("result").cloned().unwrap_or(Value::Null);
            log(cfg, &format!("resp {id} {result}"));
        }
        return false;
    };
    log(cfg, &format!("recv {method}"));
    match method {
        "initialize" => {
            send_frame(
                cfg,
                &json!({
                    "jsonrpc": "2.0",
                    "id": msg.get("id").cloned().unwrap_or(Value::Null),
                    "result": {
                        "capabilities": {
                            "textDocumentSync": 1,
                            "workspace": { "workspaceFolders": { "supported": true } }
                        },
                        "serverInfo": { "name": "codewave-fake-lsp", "version": "0.1.0" }
                    }
                }),
            );
            if cfg.mode == "crash_after_init" {
                log(cfg, "mode crash_after_init");
                return true;
            }
        }
        "initialized" => {
            if cfg.server_requests {
                send_server_requests(cfg);
            }
        }
        "textDocument/didOpen" => {
            if let Some(u) = msg
                .get("params")
                .and_then(|p| p.get("textDocument"))
                .and_then(|t| t.get("uri"))
                .and_then(|u| u.as_str())
            {
                *uri = u.to_string();
            }
            if cfg.empty_first_ms > 0 {
                push_empty_placeholder(cfg, uri);
                std::thread::sleep(Duration::from_millis(cfg.empty_first_ms));
            }
            push_diagnostics(cfg, uri, push_index, cfg.delay_ms);
        }
        "textDocument/didChange" => {
            push_diagnostics(cfg, uri, push_index, cfg.delay_change_ms);
        }
        "shutdown" => {
            send_frame(
                cfg,
                &json!({
                    "jsonrpc": "2.0",
                    "id": msg.get("id").cloned().unwrap_or(Value::Null),
                    "result": Value::Null
                }),
            );
        }
        "exit" => {
            log(cfg, "bye");
            return true;
        }
        _ => {
            if let Some(id) = msg.get("id") {
                send_frame(
                    cfg,
                    &json!({ "jsonrpc": "2.0", "id": id.clone(), "result": Value::Null }),
                );
            }
        }
    }
    false
}

/// `initialized` 后的四个服务端主动请求（id 固定 1..4，便于测试按 id 断言应答）。
fn send_server_requests(cfg: &Config) {
    let requests = [
        (
            1,
            "workspace/configuration",
            json!({ "items": [{ "section": "typescript" }, { "section": "editor" }] }),
        ),
        (
            2,
            "client/registerCapability",
            json!({ "registrations": [] }),
        ),
        (
            3,
            "window/workDoneProgress/create",
            json!({ "token": "fake-token" }),
        ),
        (
            4,
            "window/showMessageRequest",
            json!({ "type": 1, "message": "fake server says hi" }),
        ),
    ];
    for (id, method, params) in requests {
        send_frame(
            cfg,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
    }
}

/// 推一个**空集占位**（`EMPTY_TIMES` / `ALWAYS_EMPTY` 用；不消耗 `SEQ` 轮次）。
///
/// 日志用 `push empty-set`：与 `EMPTY_FIRST_MS` 的 `push empty-first placeholder` 区分开，
/// 测试可以直接数行数。
fn push_empty_set(cfg: &Config, uri: &str) {
    if cfg.mode == "silent" {
        return;
    }
    send_frame(
        cfg,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": [] }
        }),
    );
    log(cfg, "push empty-set");
}

/// 推一个**占位空集**（不消耗 `SEQ` 轮次；复现冷启动「空集先到、真诊断后到」）。
fn push_empty_placeholder(cfg: &Config, uri: &str) {
    if cfg.mode == "silent" {
        return;
    }
    send_frame(
        cfg,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": [] }
        }),
    );
    log(cfg, "push empty-first placeholder");
}

/// 推送 `publishDiagnostics`（payload 为数组则包成 params，为对象则原样并补 uri）。
fn push_diagnostics(cfg: &Config, uri: &str, push_index: &mut usize, delay_ms: u64) {
    if cfg.mode == "silent" {
        log(cfg, "silent-skip");
        return;
    }
    if cfg.always_empty {
        // 「全干净项目」：server 一直在出声，但内容永远是空集（占位与真干净在协议上无法区分）
        for _ in 0..cfg.empty_times.max(1) {
            push_empty_set(cfg, uri);
        }
        return;
    }
    for _ in 0..cfg.empty_times {
        push_empty_set(cfg, uri);
    }
    let Some(payload) = cfg.payload_for(*push_index) else {
        return;
    };
    let round = *push_index;
    *push_index += 1;
    let params = match payload {
        Value::Array(items) => json!({
            "uri": uri,
            "diagnostics": items,
            "version": (round + 1) as i64
        }),
        Value::Object(mut obj) => {
            if !obj.contains_key("uri") {
                obj.insert("uri".into(), json!(uri));
            }
            Value::Object(obj)
        }
        other => other,
    };
    if delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
    let message = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": params
    });
    if cfg.no_length {
        // 故意漏 Content-Length：客户端必须报 BadHeader 而不是挂死
        let body = serde_json::to_vec(&message).unwrap_or_default();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(b"Content-Type: application/vscode-jsonrpc\r\n\r\n");
        let _ = out.write_all(&body);
        let _ = out.flush();
    } else {
        send_frame(cfg, &message);
    }
    log(cfg, &format!("push diagnostics round {round}"));
}

/// 写一条 `Content-Length` 帧（`FRAGMENT=1` 时拆 3 段，段间 sleep 10ms）。
fn send_frame(cfg: &Config, value: &Value) {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    bytes.extend_from_slice(&body);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if cfg.fragment {
        let chunk = (bytes.len() + 2) / 3;
        let chunk = chunk.max(1);
        for part in bytes.chunks(chunk) {
            let _ = out.write_all(part);
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(10));
        }
    } else {
        let _ = out.write_all(&bytes);
        let _ = out.flush();
    }
}

/// 读一条帧（`Content-Length` framing；EOF 返回 `Ok(None)`）。
fn read_frame<R: BufRead>(reader: &mut R) -> std::io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                len = value.trim().parse::<usize>().ok();
            }
        }
    }
    let Some(len) = len else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "缺少 Content-Length",
        ));
    };
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

/// 追加一行日志（每行独立开关文件，测试读得到）。
fn log(cfg: &Config, line: &str) {
    let Some(path) = &cfg.log else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}
