//! OpenAI Chat Completions 协议适配器（兼容 Ollama / DeepSeek / LM Studio 等一切兼容服务）。
//! 解析逻辑抽成纯函数 `handle_chunk` 便于单测；HTTP/SSE 外壳在 `stream`。

use super::dto::*;
use super::sse::{SseEvent, SseParser};
use crate::core::config::ModelConfig;
use crate::core::types::{Content, Message, Role};
use futures::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------- 请求构造 ----------

/// 构造 Chat Completions 请求体：system 置顶 + 消息转换 + tools / reasoning_effort 段；
/// 推理系列模型自动把 max_tokens 换成 max_completion_tokens。
pub fn build_body(req: &StreamRequest) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": req.system_full() })];
    for m in &req.messages {
        convert_message(m, &mut messages);
    }
    let mut body = json!({
        "model": req.model.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": req.model.max_tokens,
    });
    // L13：OpenAI 较新的推理模型（o1/o3/o4/gpt-5 系列）不接受 max_tokens，
    // 必须改用 max_completion_tokens 字段
    if needs_max_completion_tokens(&req.model.model) {
        let v = body["max_tokens"].take();
        let obj = body.as_object_mut().expect("body is object");
        obj.remove("max_tokens");
        obj.insert("max_completion_tokens".into(), v);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": serde_json::from_str::<Value>(&t.schema_json).unwrap_or(json!({})),
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    // 会话/模型级 reasoning effort（Max 非本协议标准枚举，显式降级为 high）
    if let Some(effort) = req.reasoning_effort {
        body["reasoning_effort"] = json!(effort.to_openai());
    }
    body
}

/// L13：o1/o3/o4/gpt-5 系列模型要求用 max_completion_tokens 而非 max_tokens。
fn needs_max_completion_tokens(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
        || lower.starts_with("gpt-5")
}

/// 单条内部消息 → Chat Completions wire 格式（tool result 拆成 role=tool 消息），追加到 `out`。
fn convert_message(m: &Message, out: &mut Vec<Value>) {
    match m.role {
        Role::System => { /* system 已在顶层注入 */ }
        Role::User => {
            let images: Vec<&Content> = m
                .content
                .iter()
                .filter(|c| matches!(c, Content::Image { .. }))
                .collect();
            if images.is_empty() {
                out.push(json!({ "role": "user", "content": m.text_joined() }));
            } else {
                let parts: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text { text } => Some(json!({ "type": "text", "text": text })),
                        Content::Image { media_type, data } => Some(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{media_type};base64,{data}") }
                        })),
                        _ => None,
                    })
                    .collect();
                out.push(json!({ "role": "user", "content": parts }));
            }
        }
        Role::Assistant => {
            let text = m.text_joined();
            let tool_uses: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::ToolUse { id, name, args } => Some(json!({
                        "id": id, "type": "function",
                        "function": { "name": name, "arguments": args.to_string() }
                    })),
                    _ => None,
                })
                .collect();
            // 空 assistant 消息不上 wire（缺陷修复）：Chat Completions 不接受 content 为
            // null 且无 tool_calls 的 assistant 消息（400 Invalid assistant message）。
            // 历史侧已在 repair（丢空消息）与出网副本两层拦截，这里是最后一道 wire 门。
            // 判定只看 text / tool_uses：仅含思考块的消息同样整条丢弃，不能因为「有 thinking」而放行。
            if text.is_empty() && tool_uses.is_empty() {
                return;
            }
            // reasoning_content 回传（缺陷修复，[docs/reasoning-content-passthrough](../../../docs/reasoning-content-passthrough.md)）：
            // OpenAI 兼容的 thinking 上游（如经中转的 DeepSeek 系模型）要求多轮对话中历史
            // assistant 消息把该轮产生的思维链原样带回，缺失会 400（The reasoning_content in
            // the thinking mode must be passed back to the API）。按内容块到达顺序收集非空
            // 思考块，以 "\n\n" 拼接。
            let reasoning = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Thinking { text } if !text.is_empty() => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut v = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
            if !tool_uses.is_empty() {
                v["tool_calls"] = Value::Array(tool_uses);
            }
            // 没收集到思考块时不写该键（不发空串 / null）：纯文本与工具调用消息的 wire 字节保持不变
            if !reasoning.is_empty() {
                v["reasoning_content"] = json!(reasoning);
            }
            out.push(v);
        }
        Role::Tool => {
            for c in &m.content {
                if let Content::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = c
                {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": if *is_error { format!("[error] {content}") } else { content.clone() },
                    }));
                }
            }
        }
    }
}

// ---------- 响应解析（纯函数） ----------

/// Chat Completions SSE 流的累积器：记录工具调用身份与收尾态，从 usage 帧提取 token 用量。
#[derive(Debug, Default)]
pub struct OcAccum {
    /// 规范 index → (调用 id, 工具名)，finish_reason=tool_calls 时按此补发 ToolCallEnd
    pub tool_ids: std::collections::HashMap<usize, (String, String)>,
    /// 已补发 ToolCallEnd 的规范 index 集合，保证只收尾一次
    pub ended: std::collections::HashSet<usize>,
    /// 调用 id → 规范 index（协议 index 缺失/重复时按 id 唯一化）
    pub canonical: std::collections::HashMap<String, usize>,
    /// 原始 index 槽位 → 该槽位最近一次开始的规范 index（无 id 续帧的映射依据）
    pub raw_map: std::collections::HashMap<usize, usize>,
    /// 流是否已正常收尾（finish_reason 或 [DONE]）
    pub finished: bool,
    /// usage 帧提取的 token 用量
    pub usage: RunUsage,
}

/// 处理单个 SSE data 载荷并产出 deltas。流结束时返回 Ok(false)。
pub fn handle_chunk(
    acc: &mut OcAccum,
    data: &str,
    deltas: &mut Vec<StreamDelta>,
) -> Result<bool, ProviderError> {
    if data.trim() == "[DONE]" {
        acc.finished = true;
        return Ok(false);
    }
    let v: Value = serde_json::from_str(data)
        .map_err(|e| ProviderError::Protocol(format!("openai chunk json: {e}")))?;

    if let Some(err) = v.get("error") {
        return Err(ProviderError::Server(format!(
            "stream error: {}",
            err.as_str().unwrap_or(&err.to_string())
        )));
    }

    if let Some(u) = v.get("usage") {
        acc.usage.input = u["prompt_tokens"].as_u64().unwrap_or(0);
        acc.usage.output = u["completion_tokens"].as_u64().unwrap_or(0);
        acc.usage.cache_read = u["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);
    }

    let Some(choice) = v["choices"].get(0) else {
        return Ok(true); // 仅含 usage 的帧
    };
    let delta = &choice["delta"];
    if let Some(t) = delta["content"].as_str() {
        if !t.is_empty() {
            deltas.push(StreamDelta::Text {
                text: t.to_string(),
            });
        }
    }
    if let Some(t) = delta["reasoning_content"].as_str() {
        if !t.is_empty() {
            deltas.push(StreamDelta::Reasoning {
                text: t.to_string(),
            });
        }
    }
    if let Some(tcs) = delta["tool_calls"].as_array() {
        for tc in tcs {
            let raw = tc["index"].as_u64().unwrap_or(0) as usize;
            // 规范化 index：以调用 id 为唯一身份分配顺序 index；无 id 的续帧映射到该原始
            // 槽位最近开始的调用。GLM 等兼容端点的流式 tool_calls 可能缺 index（恒为 0），
            // 此前会把同批多个调用在装配层并进同一张卡。
            let index = match tc["id"].as_str() {
                Some(id) => {
                    let next = acc.canonical.len();
                    let c = *acc.canonical.entry(id.to_string()).or_insert(next);
                    acc.raw_map.insert(raw, c);
                    c
                }
                None => acc.raw_map.get(&raw).copied().unwrap_or(raw),
            };
            if let (Some(id), Some(name)) = (tc["id"].as_str(), tc["function"]["name"].as_str()) {
                acc.tool_ids
                    .insert(index, (id.to_string(), name.to_string()));
                deltas.push(StreamDelta::ToolCallBegin {
                    index,
                    id: id.to_string(),
                    name: name.to_string(),
                });
            }
            if let Some(frag) = tc["function"]["arguments"].as_str() {
                if !frag.is_empty() {
                    deltas.push(StreamDelta::ToolCallArgsDelta {
                        index,
                        fragment: frag.to_string(),
                    });
                }
            }
        }
    }
    if let Some(fr) = choice["finish_reason"].as_str() {
        if fr == "tool_calls" {
            let mut idxs: Vec<usize> = acc.tool_ids.keys().copied().collect();
            idxs.sort_unstable();
            for i in idxs {
                if acc.ended.insert(i) {
                    deltas.push(StreamDelta::ToolCallEnd { index: i });
                }
            }
            acc.finished = true;
        } else if fr == "stop" {
            acc.finished = true;
        } else if fr == "length" {
            // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)：输出超限截断 → 用户可见尾注
            //（与 anthropic 协议对齐），不再无声地按正常结束收场
            deltas.push(StreamDelta::Text {
                text: MAX_TOKENS_NOTICE.into(),
            });
            acc.finished = true;
        }
    }
    Ok(true)
}

// ---------- 流式外壳 ----------

/// 发起 Chat Completions 流式请求并驱动 SSE 解析：请求构造 → 发送（响应取消）→
/// 逐 chunk 喂解析器 → 事件转 StreamDelta 下发 → 截断判定，返回 token 用量。
pub async fn stream(
    client: &reqwest::Client,
    model: &ModelConfig,
    key: Option<String>,
    req: StreamRequest,
    tx: mpsc::Sender<StreamDelta>,
    cancel: CancellationToken,
) -> Result<RunUsage, ProviderError> {
    let key = key.filter(|k| !k.is_empty());
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let session_id = req.session_id.clone();
    let body = build_body(&req);

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(k) = &key {
        request = request.header("Authorization", format!("Bearer {k}"));
    }
    // 自定义请求头在协议/鉴权头之后应用（保留名被忽略，UA 可覆盖），[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)
    request = crate::provider::headers::apply_request_headers(
        request,
        &model.headers,
        session_id.as_deref(),
    );
    let resp = tokio::select! {
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        r = request.send() => r.map_err(|e| ProviderError::Network(e.to_string()))?,
    };
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(ProviderError::from_status(status, &body_text));
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut acc = OcAccum::default();

    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            item = stream.next() => match item {
                Some(Ok(b)) => b,
                Some(Err(e)) => return Err(ProviderError::Network(e.to_string())),
                None => break,
            },
        };
        let mut events: Vec<SseEvent> = Vec::new();
        parser.feed(&chunk, &mut events);
        for ev in events {
            let mut deltas = Vec::new();
            let cont = handle_chunk(&mut acc, &ev.data, &mut deltas)?;
            for d in deltas {
                if tx.send(d).await.is_err() {
                    return Err(ProviderError::Cancelled);
                }
            }
            if !cont {
                break;
            }
        }
        if acc.finished {
            break;
        }
    }

    // 流中断语义（[docs/p0-plan](../../../docs/p0-plan.md) §4.5）：EOF 却未收到 finish_reason/[DONE] → 可重试的截断错误
    if !acc.finished {
        return Err(ProviderError::Network(
            "SSE 流被截断（未收到完成标志）".into(),
        ));
    }

    Ok(acc.usage)
}

/// 连接超时常量（代理/建连阶段用）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(acc: &mut OcAccum, data: &str) -> (bool, Vec<StreamDelta>) {
        let mut d = Vec::new();
        let cont = handle_chunk(acc, data, &mut d).unwrap();
        (cont, d)
    }

    #[test]
    fn text_and_reasoning() {
        let mut acc = OcAccum::default();
        let (cont, d) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"content":"Hi","reasoning_content":"think"}}]}"#,
        );
        assert!(cont);
        assert!(d.contains(&StreamDelta::Text { text: "Hi".into() }));
        assert!(d.contains(&StreamDelta::Reasoning {
            text: "think".into()
        }));
    }

    #[test]
    fn tool_call_lifecycle() {
        let mut acc = OcAccum::default();
        let (_, d1) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":""}}]}}]}"#,
        );
        assert!(matches!(d1[0], StreamDelta::ToolCallBegin { index: 0, ref id, .. } if id == "c1"));
        let (_, d2) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"fi"}}]}}]}"#,
        );
        assert!(
            matches!(&d2[0], StreamDelta::ToolCallArgsDelta { fragment, .. } if fragment == "{\"fi")
        );
        let (_, d3) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        assert!(matches!(d3[0], StreamDelta::ToolCallEnd { index: 0 }));
        assert!(acc.finished);
    }

    #[test]
    fn duplicate_indexes_normalized_by_call_id() {
        // GLM 等端点缺 index（恒为 0）：多个调用必须拿到不同 index，否则同批并进一张卡
        let mut acc = OcAccum::default();
        let (_, d1) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":""}}]}}]}"#,
        );
        let (_, d2) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"files\":[]}"}}]}}]}"#,
        );
        let (_, d3) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c2","function":{"name":"grep","arguments":""}}]}}]}"#,
        );
        let (_, d4) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pattern\":\"x\"}"}}]}}]}"#,
        );
        assert!(matches!(&d1[0], StreamDelta::ToolCallBegin { index: 0, id, .. } if id == "c1"));
        assert!(matches!(
            &d2[0],
            StreamDelta::ToolCallArgsDelta { index: 0, .. }
        ));
        assert!(matches!(&d3[0], StreamDelta::ToolCallBegin { index: 1, id, .. } if id == "c2"));
        assert!(matches!(
            &d4[0],
            StreamDelta::ToolCallArgsDelta { index: 1, .. }
        ));
        let (_, d5) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        let ends: Vec<usize> = d5
            .iter()
            .filter_map(|d| match d {
                StreamDelta::ToolCallEnd { index } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(ends, vec![0, 1]);
    }

    #[test]
    fn usage_and_done() {
        let mut acc = OcAccum::default();
        let (cont, _) = feed(
            &mut acc,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":4}}}"#,
        );
        assert!(cont);
        assert_eq!(acc.usage.input, 10);
        assert_eq!(acc.usage.cache_read, 4);
        let (cont, _) = feed(&mut acc, "[DONE]");
        assert!(!cont);
    }

    fn test_request() -> StreamRequest {
        StreamRequest {
            model: ModelConfig {
                model: "gpt-x".into(),
                max_tokens: 999,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            messages: vec![
                Message::user_text("hello"),
                Message::tool_results(vec![Content::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "data".into(),
                    is_error: false,
                }]),
            ],
            tools: vec![ToolDef {
                name: "read".into(),
                description: "d".into(),
                schema_json: r#"{"type":"object"}"#.into(),
            }],
            cache_key: None,
            reasoning_effort: None,
            session_id: None,
        }
    }

    // 缺陷修复：content 为 null 且无 tool_calls 的 assistant 消息不得上 wire（上游 400）
    #[test]
    fn skips_assistant_without_text_or_tool_calls() {
        let mut req = test_request();
        req.messages = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: Vec::new(),
                created_at: None,
            },
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert!(
            msgs.iter().all(|m| !(m["role"] == "assistant"
                && m["content"].is_null()
                && m.get("tool_calls").is_none())),
            "空 assistant 消息不应出现在请求体里"
        );
        // 合法消息不受影响
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["role"], "user");
    }

    // 不变量：每个带上 wire 的 tool_call 都必须有对应的 role=tool 应答消息
    #[test]
    fn tool_calls_are_answered_on_wire() {
        let mut req = test_request();
        req.messages = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Text { text: "ok".into() },
                    Content::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        args: json!({ "files": [] }),
                    },
                ],
                created_at: None,
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "data".into(),
                is_error: false,
            }]),
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        let mut calls: Vec<String> = Vec::new();
        let mut answers: Vec<String> = Vec::new();
        for m in msgs {
            if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    calls.push(tc["id"].as_str().unwrap().to_string());
                }
            }
            if m["role"] == "tool" {
                answers.push(m["tool_call_id"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(calls, vec!["t1".to_string()]);
        for c in &calls {
            assert!(answers.contains(c), "tool_call {c} 未获应答");
        }
    }

    #[test]
    fn request_body_shape() {
        let req = test_request();
        let body = build_body(&req);
        assert_eq!(body["model"], "gpt-x");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "t1");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        assert_eq!(body["max_tokens"], 999);
    }

    #[test]
    fn body_uses_max_completion_tokens_for_reasoning_models() {
        let mut req = test_request();
        req.model.model = "o3-mini".into();
        let body = build_body(&req);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 999);
        req.model.model = "gpt-5".into();
        let body = build_body(&req);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["max_completion_tokens"], 999);
    }

    #[test]
    fn body_still_uses_max_tokens_for_normal_models() {
        let mut req = test_request();
        req.model.model = "gpt-4o".into();
        let body = build_body(&req);
        assert_eq!(body["max_tokens"], 999);
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn request_level_effort_with_max_downgrade() {
        // [docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)：请求级 effort 覆盖；Max 非本协议标准枚举 → 降级为 high
        let mut req = test_request();
        req.reasoning_effort = Some(crate::core::prefs::EffortLevel::Max);
        assert_eq!(build_body(&req)["reasoning_effort"], "high");
        req.reasoning_effort = Some(crate::core::prefs::EffortLevel::Low);
        assert_eq!(build_body(&req)["reasoning_effort"], "low");
        // 请求级 None：模型配置里的 effort 不再由协议层直发（由编排层决议后以请求级传入）
        req.reasoning_effort = None;
        req.model.reasoning_effort = Some("high".into());
        assert!(build_body(&req).get("reasoning_effort").is_none());
    }

    /// 协议语义钉死：Role::Tool 消息中的非 ToolResult 块（如 Text）不产生任何 role=tool 消息——
    /// 「往 Tool 消息推 Text 块给模型」这条通道不可达，任何要提醒模型的内容必须并入 ToolResult content。
    #[test]
    fn tool_role_non_toolresult_blocks_dropped() {
        let mut req = test_request();
        req.messages.push(Message {
            role: Role::Tool,
            content: vec![
                Content::ToolResult {
                    tool_use_id: "t2".into(),
                    content: "正文".into(),
                    is_error: false,
                },
                Content::Text {
                    text: "should-not-reach".into(),
                },
            ],
            created_at: None,
        });
        let body = build_body(&req);
        assert!(
            !body.to_string().contains("should-not-reach"),
            "Tool 消息中的 Text 块必须被协议转换丢弃：{body}"
        );
        assert!(
            body.to_string().contains("正文"),
            "ToolResult 正文应保留：{body}"
        );
    }

    /// plan 软提醒可达性：追加在 ToolResult content 尾部的提醒必须原样进入请求体（坐实修复链路）。
    #[test]
    fn tool_result_content_reaches_wire_verbatim() {
        let mut req = test_request();
        req.messages
            .push(Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t2".into(),
                content: "写\n计划提醒：当前计划没有进行中条目，完成后请用 plan 工具标记状态。"
                    .into(),
                is_error: false,
            }]));
        let body = build_body(&req);
        assert!(
            body.to_string()
                .contains("计划提醒：当前计划没有进行中条目"),
            "ToolResult content 尾部的提醒必须出现在请求体：{body}"
        );
    }

    #[test]
    fn finish_reason_length_truncation_notice() {
        // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)：finish_reason="length" 必须产出用户可见尾注
        //（与 anthropic 协议对齐）并把累积器标记为已结束
        let mut acc = OcAccum::default();
        let (cont, d1) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"content":"partial an"}}]}"#,
        );
        assert!(cont);
        assert!(matches!(&d1[0], StreamDelta::Text { text } if text == "partial an"));
        let (cont, d2) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        );
        assert!(cont);
        assert!(acc.finished);
        assert_eq!(
            &d2[0],
            &StreamDelta::Text {
                text: crate::provider::dto::MAX_TOKENS_NOTICE.into()
            }
        );
    }

    #[test]
    fn finish_reason_stop_no_notice() {
        // 防漂移：正常 "stop" 必须保持无尾注
        let mut acc = OcAccum::default();
        let (cont, d) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        );
        assert!(cont);
        assert!(acc.finished);
        assert!(d.is_empty());
    }

    #[test]
    fn length_mid_tool_call_notice_and_args_intact() {
        // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md) 评审跟进："length" 落在工具调用中途时，尾注必须以 Text 注入
        // 且不动已收集的工具帧——装配层在流结束后从 Begin/ArgsDelta 重组工具调用
        //（ToolCallEnd 在彼处是空操作），截断入参的抢救路径因此保持可用。
        let mut acc = OcAccum::default();
        let (_, d1) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":""}}]}}]}"#,
        );
        assert!(
            matches!(&d1[0], StreamDelta::ToolCallBegin { index: 0, id, name } if id == "c1" && name == "read")
        );
        let (_, d2) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"fi"}}]}}]}"#,
        );
        assert!(matches!(
            &d2[0],
            StreamDelta::ToolCallArgsDelta { index: 0, fragment } if fragment == "{\"fi"
        ));
        let (_, d3) = feed(
            &mut acc,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
        );
        // 尾注以 Text 出现在工具帧之后；已收集的入参分毫未动
        assert_eq!(
            &d3[0],
            &StreamDelta::Text {
                text: crate::provider::dto::MAX_TOKENS_NOTICE.into()
            }
        );
        assert!(acc.finished);
        assert_eq!(acc.tool_ids.get(&0), Some(&("c1".into(), "read".into())));
    }

    // 缺陷修复（[docs/reasoning-content-passthrough](../../../docs/reasoning-content-passthrough.md)）：
    // 历史 assistant 消息里的思考块必须以 reasoning_content 原样回传，否则 OpenAI 兼容的
    // thinking 上游返回 400；同一消息的 content / tool_calls 输出保持原样。
    #[test]
    fn assistant_thinking_becomes_reasoning_content() {
        // 思考文本与工具调用 id 刻意取不同值：若装配层把工具 id 误当思考（或反之），
        // 本用例必须失败。同值（旧版两处都用 "t1"）会让该缺陷逃逸。
        let mut req = test_request();
        req.messages = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking {
                        text: "reason-t1".into(),
                    },
                    Content::Text { text: "hi".into() },
                    Content::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        args: json!({ "files": [] }),
                    },
                ],
                created_at: None,
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "data".into(),
                is_error: false,
            }]),
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        assert_eq!(assistant["reasoning_content"], "reason-t1");
        assert_eq!(assistant["content"], "hi");
        assert_eq!(assistant["tool_calls"][0]["id"], "t1");
    }

    #[test]
    fn assistant_reasoning_content_joins_thinking_blocks_in_order() {
        let mut req = test_request();
        req.messages = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking {
                        text: "first".into(),
                    },
                    Content::Text {
                        text: "answer".into(),
                    },
                    Content::Thinking {
                        text: "second".into(),
                    },
                ],
                created_at: None,
            },
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        assert_eq!(assistant["reasoning_content"], "first\n\nsecond");
        assert_eq!(assistant["content"], "answer");
    }

    /// 防漂移：无思考块（或仅空思考块）的 assistant 消息不得出现 reasoning_content 键。
    #[test]
    fn assistant_without_thinking_has_no_reasoning_content_key() {
        let mut req = test_request();
        req.messages = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![Content::Text { text: "hi".into() }],
                created_at: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![Content::Thinking {
                    text: String::new(),
                }],
                created_at: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Text { text: "ok".into() },
                    Content::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        args: json!({ "files": [] }),
                    },
                ],
                created_at: None,
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "data".into(),
                is_error: false,
            }]),
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        let mut saw = 0;
        for m in msgs {
            if m["role"] == "assistant" {
                saw += 1;
                assert!(
                    m.get("reasoning_content").is_none(),
                    "无思考块的 assistant 消息不得带 reasoning_content：{m}"
                );
            }
        }
        assert_eq!(saw, 2, "纯文本 / 含工具调用的 assistant 消息都应在 wire 上");
    }

    /// 守卫语义钉死：仅含思考块的 assistant 消息整条不上 wire（thinking 不足以放行）。
    #[test]
    fn assistant_with_only_thinking_stays_off_wire() {
        let mut req = test_request();
        req.messages = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![Content::Thinking {
                    text: "only-thought".into(),
                }],
                created_at: None,
            },
        ];
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|m| m["role"] != "assistant"));
        assert!(
            !body.to_string().contains("only-thought"),
            "仅含思考块的消息不上 wire：{body}"
        );
    }
}
