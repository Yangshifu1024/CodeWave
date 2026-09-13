//! provider 集成测试：本地 mock SSE 服务器（tokio TcpListener + 原始 HTTP 字节），
//! 覆盖 G3 DoD ①（标准流）③（流中断）④（429 退避）。

use super::dto::*;
use super::openai_chat;
use crate::core::config::ModelConfig;
use crate::core::types::Message;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
        system: "sys".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
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
    // 发到一半断开（无 Content-Length 终止的 body 直接 close）
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf).await;
        let partial = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"par";
        sock.write_all(partial).await.unwrap();
        sock.flush().await.unwrap();
        drop(sock); // 中途断连
    });

    let model = test_model(port);
    let req = StreamRequest {
        model: model.clone(),
        system: "s".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
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
    // 断连后无完整事件 → EOF 归为可重试的 Network/空响应
    assert!(
        matches!(err, ProviderError::Network(_) | ProviderError::Protocol(_)),
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
        system: "s".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
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
        system: "s".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
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
        system: "s".into(),
        messages: vec![Message::user_text("hi")],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
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
