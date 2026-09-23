//! Anthropic Messages 协议适配器（手写薄层，[docs/technical-design](../../../docs/technical-design.md) §4.2.2）。
//! 显式声明 prompt cache 断点（最多 4 个，当前用 3）：tools 段末尾 + system 稳定主块 +
//! 最后一条消息的末尾 content block；另有历史代际断点（cache_gen_index）复用消息末块标注。

use super::dto::*;
use super::sse::{SseEvent, SseParser};
use crate::core::config::ModelConfig;
use crate::core::types::{Content, Message, Role};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------- 请求构造 ----------

/// 构造 Anthropic Messages 请求体：消息转换 + prompt cache 断点标注 + 相邻同角色合并 + tools/thinking 段。
pub fn build_body(req: &StreamRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for (i, m) in req.messages.iter().enumerate() {
        let before = messages.len();
        convert_message(m, &mut messages);
        // 历史代际断点：标注该内部消息产出的最后一条 wire 消息的末块。
        // 内部消息可能产出 0 条 wire 消息（如仅 thinking 被滤空）——此时静默跳过，防止错标到更早位置
        if req.cache_gen_index == Some(i) && messages.len() > before {
            if let Some(blocks) = messages
                .last_mut()
                .and_then(|m| m["content"].as_array_mut())
            {
                if let Some(last_block) = blocks.last_mut() {
                    last_block["cache_control"] = json!({ "type": "ephemeral" });
                }
            }
        }
    }
    // cache 断点：最后一条「非瞬态」消息的末尾 content block
    //（瞬态尾消息——如 <current-plan-transient>——排在断点之后，保证前缀字节稳定可命中缓存）
    if let Some(last) = messages.iter_mut().rev().find(|m| !is_transient(m)) {
        if let Some(blocks) = last["content"].as_array_mut() {
            if let Some(last_block) = blocks.last_mut() {
                if last_block.get("cache_control").is_none() {
                    last_block["cache_control"] = json!({ "type": "ephemeral" });
                }
            }
        }
    }

    // M2 修复：合并相邻同角色消息（Anthropic 强制 user/assistant 交替；
    // plan 快照注入与连续 tool_result 回合都会产生相邻 user 消息）
    merge_adjacent(&mut messages);

    // system 拆双块：稳定主块带断点（切档等 system_extra 变化只失效可变段之后的前缀，
    // tools + 主块的缓存条目照常命中）；可变段不打断点。extra 为空时保持单块形态
    let mut system_blocks = vec![json!({
        "type": "text",
        "text": req.system_core,
        "cache_control": { "type": "ephemeral" }
    })];
    if !req.system_extra.is_empty() {
        system_blocks.push(json!({ "type": "text", "text": format!("\n{}", req.system_extra) }));
    }

    let mut body = json!({
        "model": req.model.model,
        "max_tokens": req.model.max_tokens,
        "stream": true,
        "system": system_blocks,
        "messages": messages,
    });
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": serde_json::from_str::<Value>(&t.schema_json).unwrap_or(json!({})),
                })
            })
            .collect();
        // tools 断点：tools 段位于请求前缀最前且字节稳定（按名排序），独立缓存条目保证
        // 稳定主块跨 run 变更（如项目指令文件编辑）时 tools 段仍命中
        if let Some(last_tool) = tools.last_mut() {
            last_tool["cache_control"] = json!({ "type": "ephemeral" });
        }
        body["tools"] = Value::Array(tools);
    }
    // 请求级 reasoning effort → extended thinking（budget 按 max_tokens 比例取值，夹取到 [1024, max_tokens-1]）。
    // ⚠ 已知风险（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）：Thinking block 不回放进历史，多轮工具循环在部分端点可能 400——
    // 已通过 e2e_real_glm thinking 变体验证；失败时回退为本协议不发 thinking。
    if let Some(effort) = req.reasoning_effort {
        if let Some(budget) = thinking_budget(req.model.max_tokens, effort) {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    body
}

/// Anthropic thinking 预算：pct × max_tokens，下限 1024、上限 max_tokens - 1；
/// 空间不足（max_tokens 太小）时返回 None，即本协议不发 thinking。
fn thinking_budget(max_tokens: u32, effort: crate::core::prefs::EffortLevel) -> Option<u32> {
    if max_tokens <= 1025 {
        return None;
    }
    let pct = (max_tokens as f32 * effort.anthropic_ratio()) as u32;
    Some(pct.clamp(1024, max_tokens - 1))
}

/// 合并相邻同角色消息：把后一条的 content blocks 拼接到前一条尾部，满足 Anthropic 的 user/assistant 交替约束。
fn merge_adjacent(messages: &mut Vec<Value>) {
    let mut merged: Vec<Value> = Vec::with_capacity(messages.len());
    for m in messages.drain(..) {
        if let Some(last) = merged.last_mut() {
            if last["role"] == m["role"] {
                if let (Some(a), Some(b)) =
                    (last["content"].as_array_mut(), m["content"].as_array())
                {
                    a.extend(b.iter().cloned());
                    continue;
                }
            }
        }
        merged.push(m);
    }
    *messages = merged;
}

/// 判断消息是否为「瞬态尾消息」（末尾 block 以 `<current-plan-transient>` 开头）：
/// 此类消息不参与 prompt cache 断点标注，保证缓存前缀稳定。
fn is_transient(m: &Value) -> bool {
    m["content"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|b| b["text"].as_str())
        .map(|t| t.starts_with("<current-plan-transient>"))
        .unwrap_or(false)
}

/// 单条内部消息 → Anthropic wire 格式（可能产出一条或多条消息），追加到 `out`。
fn convert_message(m: &Message, out: &mut Vec<Value>) {
    match m.role {
        Role::System | Role::Tool => {
            // tool result 按 Anthropic 语义折叠进 user 消息的 tool_result block
            let blocks: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => Some(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                        "is_error": is_error,
                    })),
                    _ => None,
                })
                .collect();
            if !blocks.is_empty() {
                out.push(json!({ "role": "user", "content": blocks }));
            }
        }
        Role::User => {
            let blocks: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Text { text } => Some(json!({ "type": "text", "text": text })),
                    Content::Image { media_type, data } => Some(json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": media_type, "data": data }
                    })),
                    _ => None,
                })
                .collect();
            if !blocks.is_empty() {
                out.push(json!({ "role": "user", "content": blocks }));
            }
        }
        Role::Assistant => {
            let blocks: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::Text { text } if !text.is_empty() => {
                        Some(json!({ "type": "text", "text": text }))
                    }
                    // P0：Thinking block 不回放（Anthropic 要求签名，签名校验导致无法原样重放）
                    Content::ToolUse { id, name, args } => {
                        Some(json!({ "type": "tool_use", "id": id, "name": name, "input": args }))
                    }
                    _ => None,
                })
                .collect();
            if !blocks.is_empty() {
                out.push(json!({ "role": "assistant", "content": blocks }));
            }
        }
    }
}

// ---------- 响应解析（纯函数） ----------

/// Anthropic SSE 流的事件累积器：按 block index 记录块类型与结束态，
/// 从 `message_start` / `message_delta` 提取 token 用量。
#[derive(Debug, Default)]
pub struct AnAccum {
    /// block index → 块类型（"text" / "tool_use" / "thinking" 等），用于在块结束时判断是否补发 ToolCallEnd
    block_kinds: std::collections::HashMap<u64, String>,
    /// 已发出 ToolCallEnd 的 block index 集合，保证每块只收尾一次
    ended: std::collections::HashSet<u64>,
    /// 是否已收到 message_stop（流正常收尾的标志）
    pub finished: bool,
    /// 从 SSE 事件累计的 token 用量（input / output / cache 读写）
    pub usage: RunUsage,
}

/// 处理单个 SSE event。流结束时返回 Ok(false)。
pub fn handle_event(
    acc: &mut AnAccum,
    event: Option<&str>,
    data: &str,
    deltas: &mut Vec<StreamDelta>,
) -> Result<bool, ProviderError> {
    let v: Value = serde_json::from_str(data)
        .map_err(|e| ProviderError::Protocol(format!("anthropic event json: {e}")))?;

    match event.unwrap_or("") {
        "ping" => return Ok(true),
        "error" => {
            return Err(ProviderError::Server(format!(
                "stream error: {}",
                v["error"]["message"].as_str().unwrap_or(&v.to_string())
            )));
        }
        _ => {}
    }

    let typ = v["type"].as_str().unwrap_or("");
    match typ {
        "message_start" => {
            let u = &v["message"]["usage"];
            acc.usage.input = u["input_tokens"].as_u64().unwrap_or(0);
            acc.usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
            acc.usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
        }
        "content_block_start" => {
            let index = v["index"].as_u64().unwrap_or(0);
            let block = &v["content_block"];
            let kind = block["type"].as_str().unwrap_or("").to_string();
            if kind == "tool_use" {
                deltas.push(StreamDelta::ToolCallBegin {
                    index: index as usize,
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                });
            }
            acc.block_kinds.insert(index, kind);
        }
        "content_block_delta" => {
            let index = v["index"].as_u64().unwrap_or(0);
            let d = &v["delta"];
            match d["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    let t = d["text"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        deltas.push(StreamDelta::Text {
                            text: t.to_string(),
                        });
                    }
                }
                "thinking_delta" => {
                    let t = d["thinking"].as_str().unwrap_or("");
                    if !t.is_empty() {
                        deltas.push(StreamDelta::Reasoning {
                            text: t.to_string(),
                        });
                    }
                }
                "input_json_delta" => {
                    let f = d["partial_json"].as_str().unwrap_or("");
                    if !f.is_empty() {
                        deltas.push(StreamDelta::ToolCallArgsDelta {
                            index: index as usize,
                            fragment: f.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            let index = v["index"].as_u64().unwrap_or(0);
            if acc.block_kinds.get(&index).map(|s| s.as_str()) == Some("tool_use")
                && acc.ended.insert(index)
            {
                deltas.push(StreamDelta::ToolCallEnd {
                    index: index as usize,
                });
            }
        }
        "message_delta" => {
            if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                acc.usage.output = o;
            }
            let reason = v["delta"]["stop_reason"].as_str().unwrap_or("");
            if reason == "max_tokens" {
                deltas.push(StreamDelta::Text {
                    text: super::dto::MAX_TOKENS_NOTICE.into(),
                });
            }
        }
        "message_stop" => {
            acc.finished = true;
            return Ok(false);
        }
        "error" => {}
        _ => {}
    }
    Ok(true)
}

// ---------- 流式外壳 ----------

/// 发起 Anthropic Messages 流式请求并驱动 SSE 解析：请求构造 → 发送（响应取消）→
/// 逐 chunk 喂解析器 → 事件转 StreamDelta 下发 → EOF 收尾与截断判定，返回 token 用量。
pub async fn stream(
    client: &reqwest::Client,
    model: &ModelConfig,
    key: Option<String>,
    req: StreamRequest,
    tx: mpsc::Sender<StreamDelta>,
    cancel: CancellationToken,
) -> Result<RunUsage, ProviderError> {
    let key = key.filter(|k| !k.is_empty()).unwrap_or_default();
    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let session_id = req.session_id.clone();
    let body = build_body(&req);

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("anthropic-version", "2023-06-01");
    if !key.is_empty() {
        req = req.header("x-api-key", &key);
    }
    // 自定义请求头在协议/鉴权头之后应用（保留名被忽略，UA 可覆盖），[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)
    req =
        crate::provider::headers::apply_request_headers(req, &model.headers, session_id.as_deref());
    let resp = tokio::select! {
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        r = req.json(&body).send() => r.map_err(|e| ProviderError::Network(e.to_string()))?,
    };

    let status = resp.status();
    if !status.is_success() {
        // 读错误响应体失败（连接在半截处断）与「上游真的返回空体」必须区分：
        // 否则错误文案为空，用户只看到「请求被拒绝：」而不知道原因（会话 6bca80f4 现场）。
        let body_text = match resp.text().await {
            Ok(t) => t,
            Err(e) => format!("(读取错误响应体失败：{e})"),
        };
        return Err(ProviderError::from_status(status, &body_text));
    }

    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut acc = AnAccum::default();

    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            item = stream.next() => match item {
                Some(Ok(b)) => b,
                Some(Err(e)) => return Err(ProviderError::Network(e.to_string())),
                None => break,
            },
        };
        // C2：只在 chunk 边界喂入、绝不调用 finish——半行 / 未完结事件必须缓冲到 EOF
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

    // EOF：把解析器因缺少尾部空行而暂存的事件全部下发（C2 修复——此前流式循环每个 chunk 都调
    // finish，TCP 分片把 SSE 行撕成两半时，半行被当成完整事件下发 → Protocol 错误）
    let mut events: Vec<SseEvent> = Vec::new();
    parser.finish(&mut events);
    for ev in events {
        let mut deltas = Vec::new();
        handle_event(&mut acc, ev.event.as_deref(), &ev.data, &mut deltas)?;
        for d in deltas {
            if tx.send(d).await.is_err() {
                return Err(ProviderError::Cancelled);
            }
        }
    }

    // 流中断语义：EOF 却未收到 message_stop → 视为可重试的截断错误
    if !acc.finished {
        return Err(ProviderError::Network(
            "SSE 流被截断（未收到 message_stop）".into(),
        ));
    }

    Ok(acc.usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(acc: &mut AnAccum, event: &str, data: &str) -> (bool, Vec<StreamDelta>) {
        let mut d = Vec::new();
        let cont = handle_event(acc, Some(event), data, &mut d).unwrap();
        (cont, d)
    }

    #[test]
    fn full_tool_call_flow() {
        let mut acc = AnAccum::default();
        let (_, d) = feed(
            &mut acc,
            "message_start",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":50,"cache_creation_input_tokens":10}}}"#,
        );
        assert!(d.is_empty());
        assert_eq!(acc.usage.input, 100);
        assert_eq!(acc.usage.cache_read, 50);

        let (_, d) = feed(
            &mut acc,
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"grep"}}"#,
        );
        assert!(
            matches!(d[0], StreamDelta::ToolCallBegin { index: 0, ref name, .. } if name == "grep")
        );

        let (_, d) = feed(
            &mut acc,
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
        );
        assert!(
            matches!(&d[0], StreamDelta::ToolCallArgsDelta { fragment, .. } if fragment == "{\"pa")
        );

        let (_, d) = feed(
            &mut acc,
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        );
        assert!(matches!(d[0], StreamDelta::ToolCallEnd { index: 0 }));

        let (_, d) = feed(
            &mut acc,
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
        );
        let (_, d2) = feed(
            &mut acc,
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"done"}}"#,
        );
        assert!(matches!(&d2[0], StreamDelta::Text { text } if text == "done"));
        let _ = d;

        let (cont, d) = feed(&mut acc, "message_stop", r#"{"type":"message_stop"}"#);
        assert!(!cont);
        assert!(acc.finished);
        assert!(d.is_empty());
    }

    #[test]
    fn max_tokens_notice() {
        let mut acc = AnAccum::default();
        let (_, d) = feed(
            &mut acc,
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":99}}"#,
        );
        assert_eq!(acc.usage.output, 99);
        assert!(matches!(&d[0], StreamDelta::Text { text } if text.contains("截断")));
    }

    // 缺陷修复守护：空 assistant 消息不得上 wire（Anthropic 侧天然跳过空 blocks，
    // 此用例防止未来重构无声破坏——会话 d9941c4b 的 400 就是这类空消息造成的）。
    #[test]
    fn empty_assistant_never_emits_wire_message() {
        let req = StreamRequest {
            model: ModelConfig::default(),
            system_core: "s".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            messages: vec![
                Message::user_text("q"),
                Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                    created_at: None,
                },
            ],
            tools: vec![],
            cache_key: None,
            reasoning_effort: None,
            session_id: None,
        };
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "空 assistant 消息不应产出 wire 消息");
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn consecutive_same_role_merged() {
        let req = StreamRequest {
            model: ModelConfig::default(),
            system_core: "s".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            messages: vec![
                Message::user_text("a"),
                Message::user_text("b"), // 注入/快照产生的相邻 user
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text { text: "r".into() }],
                    created_at: None,
                },
            ],
            tools: vec![],
            cache_key: None,
            reasoning_effort: None,
            session_id: None,
        };
        let body = build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "相邻 user 应合并");
        // 合并后最后一条消息上的 cache 断点依然生效
        assert_eq!(
            msgs.last().unwrap()["content"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn cache_breakpoint_and_body_shape() {
        let req = StreamRequest {
            model: ModelConfig {
                model: "claude-x".into(),
                max_tokens: 4096,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            messages: vec![
                Message::user_text("q"),
                Message::tool_results(vec![Content::ToolResult {
                    tool_use_id: "t".into(),
                    content: "r".into(),
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
        };
        let body = build_body(&req);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        // tools 断点：末位工具带 cache_control（tools 段独立缓存条目，主块跨 run 变更时仍命中）
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.last().unwrap()["cache_control"]["type"], "ephemeral");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
        // extra 为空 → system 保持单块（稳定主块）
        assert_eq!(body["system"].as_array().unwrap().len(), 1);
        // 最后一条消息（tool_result 折叠进 user）的末尾 block 打断点
        let blocks = body["messages"].as_array().unwrap();
        let last = blocks.last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(
            last["content"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
            "ephemeral"
        );
    }

    /// [docs/prompt-caching-hardening]：system 双块与历史代际断点——extra 非空时 system 拆两块
    /// 且只有稳定主块带断点；cache_gen_index 指向的内部消息末块标代际断点，与末条消息断点互不覆盖。
    #[test]
    fn system_extra_block_and_gen_breakpoint() {
        let req = StreamRequest {
            model: ModelConfig {
                model: "claude-x".into(),
                max_tokens: 4096,
                ..Default::default()
            },
            system_core: "CORE".into(),
            system_extra: "PLAN-EXTRA".into(),
            cache_gen_index: Some(0),
            messages: vec![
                Message::user_text("old-0"),
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text {
                        text: "mid-1".into(),
                    }],
                    created_at: None,
                },
                Message::user_text("recent-2"),
            ],
            tools: vec![],
            cache_key: None,
            reasoning_effort: None,
            session_id: None,
        };
        let body = build_body(&req);
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 2, "extra 非空 → system 双块");
        assert_eq!(system[0]["text"], "CORE");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(system[1]["text"], "\nPLAN-EXTRA");
        assert!(
            system[1].get("cache_control").is_none(),
            "可变段不打断点：切档只失效其后前缀"
        );

        let blocks = body["messages"].as_array().unwrap();
        // 消息 0（old-0）末块 = 代际断点
        assert_eq!(
            blocks[0]["content"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
            "ephemeral"
        );
        // 中间消息（mid-1）不带断点
        assert!(
            blocks[1]["content"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .get("cache_control")
                .is_none()
        );
        // 消息 2（recent-2，最后一条非瞬态）末块 = 末条断点
        let last = blocks.last().unwrap();
        assert_eq!(
            last["content"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
            "ephemeral"
        );
    }

    /// 协议语义钉死：Role::Tool 消息中的非 ToolResult 块（如 Text）在请求体中不出现——
    /// 「往 Tool 消息推 Text 块给模型」这条通道不可达，任何要提醒模型的内容必须并入 ToolResult content。
    #[test]
    fn tool_role_non_toolresult_blocks_dropped() {
        let req = StreamRequest {
            model: ModelConfig {
                model: "claude-x".into(),
                max_tokens: 4096,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
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
            cache_key: None,
            reasoning_effort: None,
            session_id: None,
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
                model: "claude-x".into(),
                max_tokens: 4096,
                ..Default::default()
            },
            system_core: "sys".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            messages: vec![Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "写\n计划提醒：当前计划没有进行中条目，完成后请用 plan 工具标记状态。"
                    .into(),
                is_error: false,
            }])],
            tools: vec![],
            cache_key: None,
            reasoning_effort: None,
            session_id: None,
        };
        let body = build_body(&req);
        assert!(
            body.to_string()
                .contains("计划提醒：当前计划没有进行中条目"),
            "ToolResult content 尾部的提醒必须出现在请求体：{body}"
        );
    }

    #[test]
    fn error_event_maps_to_server() {
        let mut acc = AnAccum::default();
        let mut d = Vec::new();
        let r = handle_event(
            &mut acc,
            Some("error"),
            r#"{"type":"error","error":{"message":"overloaded"}}"#,
            &mut d,
        );
        assert!(matches!(r, Err(ProviderError::Server(_))));
    }

    #[test]
    fn thinking_budget_mapping_and_clamp() {
        // [docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)：effort → extended thinking，budget = pct × max_tokens，夹取到 [1024, max_tokens-1]
        let mut req = StreamRequest {
            model: ModelConfig {
                model: "claude-x".into(),
                max_tokens: 4096,
                ..Default::default()
            },
            system_core: "s".into(),
            system_extra: String::new(),
            cache_gen_index: None,
            messages: vec![Message::user_text("q")],
            tools: vec![],
            cache_key: None,
            reasoning_effort: Some(crate::core::prefs::EffortLevel::Max),
            session_id: None,
        };
        let body = build_body(&req);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 3276); // 80% × 4096
        req.reasoning_effort = Some(crate::core::prefs::EffortLevel::Low);
        assert_eq!(build_body(&req)["thinking"]["budget_tokens"], 1024); // 20% 低于下限 → 抬到 1024
        // max_tokens 空间不足：不发 thinking
        req.model.max_tokens = 1024;
        assert!(build_body(&req).get("thinking").is_none());
        // 未配置 effort：不发
        req.model.max_tokens = 8192;
        req.reasoning_effort = None;
        assert!(build_body(&req).get("thinking").is_none());
    }
}
