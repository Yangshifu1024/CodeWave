//! OpenAI Responses 协议适配器（[docs/p1-plan](../../../docs/p1-plan.md) §3.3）：`POST {base}/responses`，stream:true。
//! 事件映射：output_text.delta→Text；reasoning_summary_text.delta→Reasoning；
//! output_item.added(function_call)→ToolCallBegin；function_call_arguments.delta→ArgsDelta；
//! completed/incomplete→usage（incomplete 且 reason=max_output_tokens → 截断尾注，
//! [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)）。cache 路由：会话级稳定的 prompt_cache_key。

use super::dto::*;
use super::sse::{SseEvent, SseParser};
use crate::core::config::ModelConfig;
use crate::core::types::{Content, Message, Role};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------- 请求构造 ----------

/// 构造 Responses 请求体：instructions 承载 system + input 转换 + prompt_cache_key / tools / reasoning 段。
pub fn build_body(req: &StreamRequest) -> Value {
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        convert_message(m, &mut input);
    }
    let mut body = json!({
        "model": req.model.model,
        "stream": true,
        "instructions": req.system_full(),
        "input": input,
        "max_output_tokens": req.model.max_tokens,
        // L13：不在 OpenAI 侧存储会话数据
        "store": false,
    });
    if let Some(ck) = &req.cache_key {
        body["prompt_cache_key"] = json!(ck);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": serde_json::from_str::<Value>(&t.schema_json).unwrap_or(json!({})),
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    // 会话/模型级 reasoning effort（Max 非本协议标准枚举，显式降级为 high）
    if let Some(effort) = req.reasoning_effort {
        body["reasoning"] = json!({ "effort": effort.to_openai() });
    }
    body
}

/// 单条内部消息 → Responses input 数组元素（assistant 拆为 message/function_call，tool result 为 function_call_output），追加到 `out`。
fn convert_message(m: &Message, out: &mut Vec<Value>) {
    match m.role {
        Role::System => { /* system 由 instructions 承载 */ }
        Role::User => {
            let parts: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Text { text } => Some(json!({ "type": "input_text", "text": text })),
                    Content::Image { media_type, data } => Some(json!({
                        "type": "input_image",
                        "image_url": format!("data:{media_type};base64,{data}")
                    })),
                    _ => None,
                })
                .collect();
            if !parts.is_empty() {
                out.push(json!({ "role": "user", "content": parts }));
            }
        }
        Role::Assistant => {
            for c in &m.content {
                match c {
                    Content::Text { text } if !text.is_empty() => {
                        out.push(json!({
                            "type": "message", "role": "assistant",
                            "content": [{ "type": "output_text", "text": text }]
                        }));
                    }
                    Content::ToolUse { id, name, args } => {
                        out.push(json!({
                            "type": "function_call", "call_id": id,
                            "name": name, "arguments": args.to_string()
                        }));
                    }
                    _ => {}
                }
            }
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
                        "type": "function_call_output", "call_id": tool_use_id,
                        "output": if *is_error { format!("[error] {content}") } else { content.clone() }
                    }));
                }
            }
        }
    }
}

// ---------- 响应解析（纯函数） ----------

/// Responses SSE 流的累积器：以 call_id 关联 function_call 的 index 与收尾态。
#[derive(Debug, Default)]
pub struct OrAccum {
    /// function_call call_id → 输出序号（按到达顺序分配）
    call_index: std::collections::HashMap<String, usize>,
    /// 下一个可分配的工具调用序号
    next_index: usize,
    /// 已补发 ToolCallEnd 的序号集合，保证只收尾一次
    ended: std::collections::HashSet<usize>,
    /// 流是否已正常收尾（completed/incomplete）
    pub finished: bool,
    /// completed/incomplete 事件提取的 token 用量
    pub usage: RunUsage,
}

/// 处理单个 SSE event（Responses 协议的事件名即 wire 的 event/type 字段）。
/// 流结束时返回 Ok(false)。
pub fn handle_event(
    acc: &mut OrAccum,
    event: Option<&str>,
    data: &str,
    deltas: &mut Vec<StreamDelta>,
) -> Result<bool, ProviderError> {
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Ok(true), // 非 JSON 数据（如空行）直接忽略
    };
    let name = event.unwrap_or_else(|| v["type"].as_str().unwrap_or(""));

    match name {
        "response.output_text.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    deltas.push(StreamDelta::Text {
                        text: t.to_string(),
                    });
                }
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(t) = v["delta"].as_str() {
                if !t.is_empty() {
                    deltas.push(StreamDelta::Reasoning {
                        text: t.to_string(),
                    });
                }
            }
        }
        "response.output_item.added" => {
            let item = &v["item"];
            if item["type"].as_str() == Some("function_call") {
                let call_id = item["call_id"].as_str().unwrap_or("").to_string();
                let index = acc.next_index;
                acc.next_index += 1;
                acc.call_index.insert(call_id.clone(), index);
                deltas.push(StreamDelta::ToolCallBegin {
                    index,
                    id: call_id,
                    name: item["name"].as_str().unwrap_or("").to_string(),
                });
            }
        }
        "response.function_call_arguments.delta" => {
            let call_id = v["call_id"].as_str().unwrap_or("");
            if let Some(&index) = acc.call_index.get(call_id) {
                if let Some(frag) = v["delta"].as_str() {
                    if !frag.is_empty() {
                        deltas.push(StreamDelta::ToolCallArgsDelta {
                            index,
                            fragment: frag.to_string(),
                        });
                    }
                }
            }
        }
        "response.output_item.done" => {
            let item = &v["item"];
            if item["type"].as_str() == Some("function_call") {
                let call_id = item["call_id"].as_str().unwrap_or("");
                if let Some(&index) = acc.call_index.get(call_id) {
                    if acc.ended.insert(index) {
                        deltas.push(StreamDelta::ToolCallEnd { index });
                    }
                }
            }
        }
        "response.completed" | "response.incomplete" => {
            let u = &v["response"]["usage"];
            acc.usage.input = u["input_tokens"].as_u64().unwrap_or(0);
            acc.usage.output = u["output_tokens"].as_u64().unwrap_or(0);
            acc.usage.cache_read = u["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0);
            // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)：输出超限截断 → 用户可见尾注（与 anthropic
            // 协议对齐）；其余 incomplete 原因保持无尾注（留扩展空间，不误报）
            if name == "response.incomplete"
                && v["response"]["incomplete_details"]["reason"].as_str()
                    == Some("max_output_tokens")
            {
                deltas.push(StreamDelta::Text {
                    text: MAX_TOKENS_NOTICE.into(),
                });
            }
            acc.finished = true;
            return Ok(false);
        }
        "response.failed" => {
            return Err(ProviderError::Server(format!(
                "responses failed: {}",
                v["response"]["error"]["message"]
                    .as_str()
                    .unwrap_or(&v.to_string())
            )));
        }
        "error" => {
            return Err(ProviderError::Server(format!(
                "stream error: {}",
                v["message"].as_str().unwrap_or(&v.to_string())
            )));
        }
        _ => {}
    }
    Ok(true)
}

// ---------- 流式外壳 ----------

/// 发起 Responses 流式请求并驱动 SSE 解析：请求构造 → 发送（响应取消）→
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
    let url = format!("{}/responses", model.base_url.trim_end_matches('/'));
    let body = build_body(&req);

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);
    if let Some(k) = &key {
        request = request.header("Authorization", format!("Bearer {k}"));
    }
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
    let mut acc = OrAccum::default();

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
            let cont = handle_event(&mut acc, ev.event.as_deref(), &ev.data, &mut deltas)?;
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

    if !acc.finished {
        return Err(ProviderError::Network(
            "SSE 流被截断（未收到 response.completed）".into(),
        ));
    }
    Ok(acc.usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(acc: &mut OrAccum, event: &str, data: &str) -> (bool, Vec<StreamDelta>) {
        let mut d = Vec::new();
        let cont = handle_event(acc, Some(event), data, &mut d).unwrap();
        (cont, d)
    }

    #[test]
    fn full_flow() {
        let mut acc = OrAccum::default();
        let (_, d) = feed(&mut acc, "response.output_text.delta", r#"{"delta":"Hi"}"#);
        assert!(matches!(&d[0], StreamDelta::Text { text } if text == "Hi"));

        let (_, d) = feed(
            &mut acc,
            "response.output_item.added",
            r#"{"item":{"type":"function_call","call_id":"call_1","name":"grep"}}"#,
        );
        assert!(
            matches!(d[0], StreamDelta::ToolCallBegin { index: 0, ref name, .. } if name == "grep")
        );

        let (_, d) = feed(
            &mut acc,
            "response.function_call_arguments.delta",
            r#"{"call_id":"call_1","delta":"{\"pa"}"#,
        );
        assert!(
            matches!(&d[0], StreamDelta::ToolCallArgsDelta { fragment, .. } if fragment == "{\"pa")
        );

        let (_, d) = feed(
            &mut acc,
            "response.output_item.done",
            r#"{"item":{"type":"function_call","call_id":"call_1"}}"#,
        );
        assert!(matches!(d[0], StreamDelta::ToolCallEnd { index: 0 }));

        let (cont, _) = feed(
            &mut acc,
            "response.completed",
            r#"{"response":{"usage":{"input_tokens":50,"output_tokens":7,"input_tokens_details":{"cached_tokens":30}}}}"#,
        );
        assert!(!cont);
        assert_eq!(acc.usage.input, 50);
        assert_eq!(acc.usage.cache_read, 30);
        assert_eq!(acc.usage.output, 7);
    }

    #[test]
    fn body_shape_and_cache_key() {
        let req = StreamRequest {
            model: ModelConfig {
                model: "gpt-x".into(),
                max_tokens: 512,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            cache_key: Some("sess-1".into()),
            reasoning_effort: None,
            messages: vec![
                Message::user_text("q"),
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolUse {
                        id: "c1".into(),
                        name: "read".into(),
                        args: json!({}),
                    }],
                    created_at: None,
                },
                Message::tool_results(vec![Content::ToolResult {
                    tool_use_id: "c1".into(),
                    content: "ok".into(),
                    is_error: false,
                }]),
            ],
            tools: vec![ToolDef {
                name: "read".into(),
                description: "d".into(),
                schema_json: r#"{"type":"object"}"#.into(),
            }],
        };
        let body = build_body(&req);
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["prompt_cache_key"], "sess-1");
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 512);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[test]
    fn request_level_effort_with_max_downgrade() {
        let mut req = StreamRequest {
            model: ModelConfig {
                model: "gpt-x".into(),
                max_tokens: 512,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            cache_key: None,
            reasoning_effort: Some(crate::core::prefs::EffortLevel::Max),
            messages: vec![Message::user_text("q")],
            tools: vec![],
        };
        assert_eq!(build_body(&req)["reasoning"]["effort"], "high");
        req.reasoning_effort = Some(crate::core::prefs::EffortLevel::Medium);
        assert_eq!(build_body(&req)["reasoning"]["effort"], "medium");
        req.reasoning_effort = None;
        assert!(build_body(&req).get("reasoning").is_none());
    }

    /// 协议语义钉死：Role::Tool 消息中的非 ToolResult 块（如 Text）不产生任何 function_call_output——
    /// 「往 Tool 消息推 Text 块给模型」这条通道不可达，任何要提醒模型的内容必须并入 ToolResult content。
    #[test]
    fn tool_role_non_toolresult_blocks_dropped() {
        let req = StreamRequest {
            model: ModelConfig {
                model: "gpt-x".into(),
                max_tokens: 512,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            cache_key: None,
            reasoning_effort: None,
            messages: vec![Message {
                role: Role::Tool,
                content: vec![
                    Content::ToolResult {
                        tool_use_id: "t1".into(),
                        content: "正文".into(),
                        is_error: false,
                    },
                    Content::Text {
                        text: "should-not-reach".into(),
                    },
                ],
                created_at: None,
            }],
            tools: vec![],
        };
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
        let req = StreamRequest {
            model: ModelConfig {
                model: "gpt-x".into(),
                max_tokens: 512,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            cache_key: None,
            reasoning_effort: None,
            messages: vec![Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "写\n计划提醒：当前计划没有进行中条目，完成后请用 plan 工具标记状态。".into(),
                is_error: false,
            }])],
            tools: vec![],
        };
        let body = build_body(&req);
        assert!(
            body.to_string().contains("计划提醒：当前计划没有进行中条目"),
            "ToolResult content 尾部的提醒必须出现在请求体：{body}"
        );
    }

    #[test]
    fn incomplete_max_output_tokens_notice() {
        // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)：response.incomplete（reason=max_output_tokens）必须产出用户可见
        // 尾注（与 anthropic 协议对齐）；usage 照常记录
        let mut acc = OrAccum::default();
        let (cont, d) = feed(
            &mut acc,
            "response.incomplete",
            r#"{"response":{"incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":10,"output_tokens":512}}}"#,
        );
        assert!(!cont);
        assert!(acc.finished);
        assert_eq!(acc.usage.output, 512);
        assert_eq!(
            &d[0],
            &StreamDelta::Text {
                text: crate::provider::dto::MAX_TOKENS_NOTICE.into()
            }
        );
    }

    #[test]
    fn incomplete_other_reason_and_completed_no_notice() {
        // 防漂移：其余 incomplete 原因与正常 completed 保持无尾注
        let mut acc = OrAccum::default();
        let (_, d) = feed(
            &mut acc,
            "response.incomplete",
            r#"{"response":{"incomplete_details":{"reason":"content_filter"},"usage":{}}}"#,
        );
        assert!(d.is_empty());
        let mut acc = OrAccum::default();
        let (_, d) = feed(
            &mut acc,
            "response.completed",
            r#"{"response":{"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        );
        assert!(d.is_empty());
        assert_eq!(acc.usage.output, 2);
    }
}
