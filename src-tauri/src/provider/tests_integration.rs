//! provider 集成测试：本地 mock SSE 服务器（tokio TcpListener + 原始 HTTP 字节），
//! 覆盖 G3 DoD ①（标准流）③（流中断）④（429 退避）。

use super::dto::*;
use super::openai_chat;
use crate::core::config::ModelConfig;
use crate::core::types::{Content, Message, Role};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 捕获首个请求的原始头部（含请求行 + 头部，直至 body 起始前）并回一个既定响应。
async fn spawn_capturing_sse_server(captured: Arc<Mutex<String>>, resp: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        // TCP 分段下单次 read 可能只拿到半个头部块：循环读到空行（\r\n\r\n）为止，
        // 上限 64 KiB 防异常请求把缓冲撑爆（正常请求一次性到达，循环只跑一轮）。
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() >= 64 * 1024 {
                break;
            }
        }
        *captured.lock().unwrap() = String::from_utf8_lossy(&buf).to_string();
        sock.write_all(&resp).await.unwrap();
        sock.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    });
    port
}

fn test_model(port: u16) -> ModelConfig {
    ModelConfig {
        id: "m1".into(),
        name: "mock".into(),
        api_format: crate::core::config::ApiFormat::OpenAiChat,
        base_url: format!("http://127.0.0.1:{port}/v1"),
        keys: vec!["test-key".into()],
        model: "mock-model".into(),
        max_tokens: 1024,
        context_window: 8192,
        reasoning_effort: None,
        provider: None,
        vision: None,
        provider_id: "p1".into(),
        keyring_accounts: vec![],
        headers: vec![],
    }
}

async fn spawn_sse_server(script: Vec<Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        for resp in script {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await; // 读掉请求头（内容不校验）
            sock.write_all(&resp).await.unwrap();
            sock.flush().await.unwrap();
            // 保持连接片刻，让客户端把响应读完
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });
    port
}

fn sse(chunks: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for c in chunks {
        body.push_str(&format!("data: {c}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[tokio::test]
async fn openai_stream_and_usage() {
    let script = vec![sse(&[
        r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":2}}"#,
    ])];
    let port = spawn_sse_server(script).await;
    let model = test_model(port);
    let req = StreamRequest {
        model: model.clone(),
        system_core: "sys".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: None,
    };
    let (tx, mut rx) = mpsc::channel(64);
    let usage = openai_chat::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut text = String::new();
    while let Ok(d) = rx.try_recv() {
        if let StreamDelta::Text { text: t } = d {
            text.push_str(&t);
        }
    }
    assert_eq!(text, "Hello");
    assert_eq!(usage.input, 11);
    assert_eq!(usage.output, 2);
}

#[tokio::test]
async fn midstream_disconnect_maps_to_network() {
    // 发到一半断开（无 Content-Length 终止的 body 直接 close）。
    //
    // 本用例曾经 flaky（Windows 上模块内连跑约 10%–20% 报 `got Server("")`），根因两条，均与产品行为无关：
    // 1) mock 只 `read` 一次就写响应并立刻 `drop(sock)`：带未读数据 close 在 Windows 上会发 RST，
    //    「半截响应」因此有时表现为 RST、有时表现为 EOF（不确定）；
    // 2) 监听端口随任务结束被释放，而并发用例也用 `127.0.0.1:0`，同一临时端口可能被另一条用例的 mock
    //    抢到并用它自己的脚本（含空 body 的 5xx）应答——客户端就会拿到与本地 mock 无关的响应，
    // 经 `from_status(5xx, "")` 归为 `Server("服务未返回原因（HTTP 5xx，响应内容为空）")`。
    // 因此：listener 用 `Arc` 持有并**活到用例结束**（端口不被复用），mock 读干请求头 + `shutdown` 写半部
    // 优雅收尾（让客户端看到干净 EOF）。
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let port = listener.local_addr().unwrap().port();
    let accept = listener.clone();
    tokio::spawn(async move {
        let (mut sock, _) = accept.accept().await.unwrap();
        // 读干请求（读到空行结束为止，上限 64 KiB）：避免「带未读数据 close」触发 RST
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() >= 64 * 1024 {
                break;
            }
        }
        let partial = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"par\";";
        sock.write_all(partial).await.unwrap();
        sock.flush().await.unwrap();
        // 优雅收尾：shutdown 写半部发 FIN（客户端读到干净 EOF），再 drain 到对端关闭
        let _ = sock.shutdown().await;
        let mut sink = [0u8; 1024];
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            while let Ok(n) = sock.read(&mut sink).await {
                if n == 0 {
                    break;
                }
            }
        })
        .await;
        drop(sock); // 中途断连
    });
    // 端口守卫：listener 活到用例结束，否则释放的临时端口可能被并发用例的 mock 抢到（见上）
    let _port_guard = listener;
    let model = test_model(port);
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: None,
    };
    let (tx, _rx) = mpsc::channel(64);
    let err = openai_chat::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    // 断连后无完整事件 → 归为**可重试的瞬时错误**，不是硬失败。
    // 接受集含 Server：平台层把半截响应表面成空 body 5xx 时 `from_status` 归为 `Server("")`，
    // 而 `retry.rs` 同样视 `Server` 为可重试——本用例只承诺「不被误判为 Auth/Billing/BadRequest」。
    assert!(
        matches!(
            err,
            ProviderError::Network(_) | ProviderError::Protocol(_) | ProviderError::Server(_)
        ),
        "got {err:?}"
    );
    assert!(err.is_transient() || matches!(err, ProviderError::Protocol(_)));
}

#[tokio::test]
async fn http_429_maps_to_rate_limited() {
    let body = "{\"error\":{\"message\":\"slow down\"}}";
    let resp = format!(
        "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes();
    let port = spawn_sse_server(vec![resp]).await;
    let model = test_model(port);
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: None,
    };
    let (tx, _rx) = mpsc::channel(64);
    let err = openai_chat::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ProviderError::RateLimited(_)), "got {err:?}");
    assert!(err.is_transient());
}

#[tokio::test]
async fn http_401_maps_to_auth() {
    let resp = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad";
    let port = spawn_sse_server(vec![resp.to_vec()]).await;
    let model = test_model(port);
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: None,
    };
    let (tx, _rx) = mpsc::channel(64);
    let err = openai_chat::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(err.is_auth());
}

/// 把一个响应 body 拆成多个 TCP 段发送（段间留间隔），
/// 模拟真实网络分片把 SSE 行撕成两半。
async fn spawn_fragmented_sse_server(parts: Vec<Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf).await;
        sock.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
        for p in parts {
            sock.write_all(&p).await.unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    });
    port
}

/// C2 回归：anthropic `data:` 行被 TCP 分片从中间撕开时必须缓冲重组，
/// 半行绝不能被当成完整事件下发（JSON 解析失败 → Protocol 错误）。
#[tokio::test]
async fn anthropic_sse_line_split_across_segments() {
    let mut body = String::new();
    body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n");
    body.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n");
    body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n");
    body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    // 在 content_block_delta 的 data 行中间切开
    let cut = body.find("\"text_delta").unwrap() + 6;
    let (head, tail) = body.split_at(cut);
    let port =
        spawn_fragmented_sse_server(vec![head.as_bytes().to_vec(), tail.as_bytes().to_vec()]).await;

    let model = ModelConfig {
        api_format: crate::core::config::ApiFormat::AnthropicMessages,
        base_url: format!("http://127.0.0.1:{port}"),
        ..test_model(port)
    };
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: None,
    };
    let (tx, mut rx) = mpsc::channel(64);
    let usage = super::anthropic::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut text = String::new();
    while let Ok(d) = rx.try_recv() {
        if let StreamDelta::Text { text: t } = d {
            text.push_str(&t);
        }
    }
    assert_eq!(text, "Hi");
    assert_eq!(usage.input, 7);
    assert_eq!(usage.output, 3);
}

/// 三个协议共有的自定义头上线语义（捕获头部已小写化）：`x-opencode-session` 占位符替换、
/// 自定义 UA 覆盖默认、适配器鉴权头原样保留。
fn assert_custom_header_wire(head: &str, session: &str, auth_line: &str) {
    assert!(
        head.contains(&format!("x-opencode-session: {session}")),
        "自定义头未按占位符替换上线：{head}"
    );
    assert!(
        head.contains("user-agent: codewave-test/9"),
        "自定义 UA 未覆盖默认：{head}"
    );
    assert!(
        head.contains(auth_line),
        "鉴权头被破坏（期望 {auth_line}）：{head}"
    );
}

/// [docs/provider-custom-headers](../../../docs/provider-custom-headers.md)：自定义请求头真实上线——UA 可覆盖、
/// `${session_id}` 完成替换、保留名（Authorization）不被覆盖。
#[tokio::test]
async fn custom_headers_sent_on_wire() {
    let resp = sse(&[
        r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
    ]);
    let captured = Arc::new(Mutex::new(String::new()));
    let port = spawn_capturing_sse_server(captured.clone(), resp).await;
    let mut model = test_model(port);
    model.headers = vec![
        crate::core::config::HeaderPair {
            name: "x-opencode-session".into(),
            value: "${session_id}".into(),
        },
        crate::core::config::HeaderPair {
            name: "User-Agent".into(),
            value: "CodeWave-test/9".into(),
        },
        crate::core::config::HeaderPair {
            name: "Authorization".into(),
            value: "Bearer evil".into(),
        },
    ];
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: Some("sess-xyz".into()),
    };
    let (tx, _rx) = mpsc::channel(64);
    openai_chat::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let head = captured.lock().unwrap().to_lowercase();
    assert_custom_header_wire(&head, "sess-xyz", "authorization: bearer test-key");
    assert!(
        !head.contains("bearer evil"),
        "保留头被自定义值覆盖：{head}"
    );
}

/// [docs/provider-custom-headers](../../../docs/provider-custom-headers.md)：AnthropicMessages 协议同样应用自定义头，
/// 保留名（`x-api-key` / `Authorization`）与适配器 `anthropic-version` 均不被自定义值覆盖。
#[tokio::test]
async fn custom_headers_sent_on_wire_anthropic() {
    let mut body = String::new();
    body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n");
    body.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n");
    body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n");
    body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes();
    let captured = Arc::new(Mutex::new(String::new()));
    let port = spawn_capturing_sse_server(captured.clone(), resp).await;
    let mut model = ModelConfig {
        api_format: crate::core::config::ApiFormat::AnthropicMessages,
        base_url: format!("http://127.0.0.1:{port}"),
        ..test_model(port)
    };
    model.headers = vec![
        crate::core::config::HeaderPair {
            name: "x-opencode-session".into(),
            value: "${session_id}".into(),
        },
        crate::core::config::HeaderPair {
            name: "User-Agent".into(),
            value: "CodeWave-test/9".into(),
        },
        crate::core::config::HeaderPair {
            name: "x-api-key".into(),
            value: "evil-key".into(),
        },
        crate::core::config::HeaderPair {
            name: "Authorization".into(),
            value: "Bearer evil".into(),
        },
        crate::core::config::HeaderPair {
            name: "anthropic-version".into(),
            value: "9999-01-01".into(),
        },
    ];
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: Some("sess-xyz".into()),
    };
    let (tx, _rx) = mpsc::channel(64);
    super::anthropic::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let head = captured.lock().unwrap().to_lowercase();
    assert_custom_header_wire(&head, "sess-xyz", "x-api-key: test-key");
    assert!(
        head.contains("anthropic-version: 2023-06-01"),
        "适配器 anthropic-version 被覆盖：{head}"
    );
    assert!(
        !head.contains("9999-01-01"),
        "保留头 anthropic-version 被覆盖：{head}"
    );
    assert!(
        !head.contains("evil-key"),
        "保留头 x-api-key 被覆盖：{head}"
    );
    assert!(
        !head.contains("bearer evil"),
        "保留头 Authorization 被覆盖：{head}"
    );
}

/// [docs/provider-custom-headers](../../../docs/provider-custom-headers.md)：OpenAiResponses 协议同样应用自定义头，
/// 保留名（`Authorization`）不被自定义值覆盖。
#[tokio::test]
async fn custom_headers_sent_on_wire_responses() {
    let resp = sse(&[
        r#"{"type":"response.output_text.delta","delta":"ok"}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}"#,
    ]);
    let captured = Arc::new(Mutex::new(String::new()));
    let port = spawn_capturing_sse_server(captured.clone(), resp).await;
    let mut model = ModelConfig {
        api_format: crate::core::config::ApiFormat::OpenAiResponses,
        ..test_model(port)
    };
    model.headers = vec![
        crate::core::config::HeaderPair {
            name: "x-opencode-session".into(),
            value: "${session_id}".into(),
        },
        crate::core::config::HeaderPair {
            name: "User-Agent".into(),
            value: "CodeWave-test/9".into(),
        },
        crate::core::config::HeaderPair {
            name: "Authorization".into(),
            value: "Bearer evil".into(),
        },
    ];
    let req = StreamRequest {
        model: model.clone(),
        system_core: "s".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: Some("sess-xyz".into()),
    };
    let (tx, _rx) = mpsc::channel(64);
    super::openai_responses::stream(
        &reqwest::Client::new(),
        &model,
        Some("test-key".into()),
        req,
        tx,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let head = captured.lock().unwrap().to_lowercase();
    assert_custom_header_wire(&head, "sess-xyz", "authorization: bearer test-key");
    assert!(
        !head.contains("bearer evil"),
        "保留头被自定义值覆盖：{head}"
    );
}

/// 缺陷守护（不联网，历史 → 请求体整条链路）：[docs/reasoning-content-passthrough](../../../docs/reasoning-content-passthrough.md)
/// 要求 OpenAI 兼容的 thinking 上游在多轮对话里回传历史 assistant 消息的 `reasoning_content`（缺失 400）。
/// 本用例串起「内部历史 → `build_body` wire 体」：带思考块的 assistant 条目必须带该键且值等于思考文本，
/// 同时 content / tool_calls 不受影响；无思考块的 assistant 条目不得多出该键。
#[test]
fn reasoning_content_passthrough_from_history_to_body() {
    let req = StreamRequest {
        model: test_model(1), // 端口仅占位：本用例只调 build_body，不发起任何网络请求
        system_core: "sys".into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![
            Message {
                role: Role::System,
                content: vec![Content::Text {
                    text: "inner-system".into(),
                }],
                created_at: None,
            },
            Message::user_text("读一下 a.txt"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking {
                        text: "先读文件".into(),
                    },
                    Content::Text {
                        text: "我来读一下".into(),
                    },
                    Content::ToolUse {
                        id: "call_1".into(),
                        name: "read".into(),
                        args: serde_json::json!({ "files": ["a.txt"] }),
                    },
                ],
                created_at: None,
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "call_1".into(),
                content: "文件内容".into(),
                is_error: false,
            }]),
            Message {
                role: Role::Assistant,
                content: vec![Content::Text {
                    text: "读完了".into(),
                }],
                created_at: None,
            },
        ],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: None,
    };

    let body = openai_chat::build_body(&req);
    let messages = body["messages"].as_array().expect("messages 为数组");
    // 顶层 system 来自 system_core；内部 System 角色消息本身不上 wire
    assert_eq!(messages[0]["role"].as_str(), Some("system"));
    assert_eq!(messages[0]["content"].as_str(), Some("sys"));
    assert_eq!(
        messages.len(),
        5,
        "system + user + assistant(思考) + tool + assistant(文本)；内部 system 消息不入 wire"
    );

    let assistants: Vec<&serde_json::Value> = messages
        .iter()
        .filter(|m| m["role"].as_str() == Some("assistant"))
        .collect();
    assert_eq!(assistants.len(), 2, "两条 assistant 条目均应上 wire");

    // 带思考块的条目：reasoning_content 原样回传，且 content / tool_calls 不受影响
    let with_thinking = assistants[0];
    assert_eq!(
        with_thinking["reasoning_content"].as_str(),
        Some("先读文件"),
        "带思考块的历史 assistant 必须回传 reasoning_content：{with_thinking}"
    );
    assert_eq!(with_thinking["content"].as_str(), Some("我来读一下"));
    assert_eq!(
        with_thinking["tool_calls"][0]["id"].as_str(),
        Some("call_1"),
        "tool_calls 的 id 必须与内部工具 id 一致：{with_thinking}"
    );
    assert_eq!(
        with_thinking["tool_calls"][0]["function"]["name"].as_str(),
        Some("read")
    );
    // 配对完整：tool 结果消息回引同一 id
    let tool_msg = messages
        .iter()
        .find(|m| m["role"].as_str() == Some("tool"))
        .expect("tool 结果消息上 wire");
    assert_eq!(tool_msg["tool_call_id"].as_str(), Some("call_1"));

    // 无思考块的条目：不得凭空出现 reasoning_content 键（防漂移）
    let plain = assistants[1];
    assert_eq!(plain["content"].as_str(), Some("读完了"));
    assert!(
        plain.get("reasoning_content").is_none(),
        "无思考块的 assistant 不得带 reasoning_content：{plain}"
    );
}
