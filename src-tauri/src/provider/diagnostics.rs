//! Bounded, content-free evidence for responses that assemble to no content.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StreamDiagnostics {
    pub http_status: u16,
    pub request_id: Option<String>,
    pub chunks: u64,
    pub bytes: u64,
    pub events: BTreeMap<String, u64>,
    pub block_types: BTreeMap<String, u64>,
    pub content_bytes: u64,
    pub tool_blocks: u64,
    pub unrecognized_values: u64,
    pub non_json_events: u64,
    pub stop_reason: Option<String>,
    pub completed: bool,
}

impl StreamDiagnostics {
    pub fn set_response(&mut self, response: &reqwest::Response) {
        self.http_status = response.status().as_u16();
        self.request_id = ["request-id", "x-request-id", "x-minimax-request-id"]
            .iter()
            .find_map(|name| response.headers().get(*name)?.to_str().ok())
            .map(|id| {
                id.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || "-_.:".contains(*c))
                    .take(128)
                    .collect()
            });
    }

    pub fn chunk(&mut self, bytes: usize) {
        self.chunks += 1;
        self.bytes += bytes as u64;
    }

    pub fn event(&mut self, name: &str, value: &Value) {
        // Only fixed vocabulary enters logs; unknown names and arbitrary values are counted.
        let name = match name {
            "message_start"
            | "message_delta"
            | "message_stop"
            | "ping"
            | "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
            | "chat.chunk"
            | "[DONE]"
            | "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.incomplete"
            | "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.delta"
            | "response.output_text.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done" => name,
            _ => "other",
        };
        *self.events.entry(name.into()).or_default() += 1;
        self.observe(value, 0);
    }

    fn observe(&mut self, value: &Value, depth: usize) {
        if depth > 16 {
            self.unrecognized_values += 1;
            return;
        }
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    match key.as_str() {
                        "tool_calls" => {
                            if let Some(calls) = value.as_array() {
                                self.tool_blocks += calls.len() as u64;
                            }
                        }
                        "stop_reason" | "finish_reason" | "reason" => {
                            if let Some(reason) = value.as_str() {
                                self.stop_reason = Some(
                                    match reason {
                                        "end_turn" | "tool_use" | "stop_sequence"
                                        | "max_tokens" | "stop" | "tool_calls" | "length"
                                        | "content_filter" | "max_output_tokens" | "pause_turn"
                                        | "refusal" => reason,
                                        _ => "other",
                                    }
                                    .into(),
                                );
                            }
                        }
                        "type" => {
                            if let Some(kind) = value.as_str() {
                                let kind = match kind {
                                    "text" | "thinking" | "tool_use" | "text_delta"
                                    | "thinking_delta" | "input_json_delta" | "signature_delta"
                                    | "function_call" | "function" | "output_text"
                                    | "reasoning" | "message" | "refusal" => kind,
                                    // Event types are already counted separately.
                                    kind if kind.starts_with("response.")
                                        || matches!(
                                            kind,
                                            "message_start"
                                                | "message_delta"
                                                | "message_stop"
                                                | "content_block_start"
                                                | "content_block_delta"
                                                | "content_block_stop"
                                        ) =>
                                    {
                                        "event"
                                    }
                                    _ => "other",
                                };
                                *self.block_types.entry(kind.into()).or_default() += 1;
                                if matches!(kind, "tool_use" | "function_call") {
                                    self.tool_blocks += 1;
                                }
                            }
                        }
                        "text" | "thinking" | "content" | "reasoning_content" | "delta"
                        | "partial_json" | "arguments" => {
                            if let Some(text) = value.as_str() {
                                self.content_bytes += text.len() as u64;
                            }
                        }
                        // Known metadata must not look like unparsed content.
                        "id" | "call_id" | "name" | "model" | "role" | "object" | "signature"
                        | "status" | "service_tier" | "system_fingerprint" => {}
                        _ => {
                            if value.as_str().is_some_and(|s| !s.is_empty()) {
                                self.unrecognized_values += 1;
                            }
                        }
                    }
                    if value.is_object() || value.is_array() {
                        self.observe(value, depth + 1);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.observe(item, depth + 1);
                }
            }
            Value::String(text) if !text.is_empty() => self.unrecognized_values += 1,
            _ => {}
        }
    }

    /// Evidence labels, not a claim that the upstream or parser is definitively at fault.
    pub fn empty_evidence(&self) -> &'static str {
        if self.content_bytes > 0 || self.tool_blocks > 0 {
            "content_not_assembled"
        } else if self.non_json_events > 0
            || self.unrecognized_values > 0
            || self.events.contains_key("other")
            || self.block_types.contains_key("other")
        {
            "unrecognized_shape"
        } else {
            "no_content_observed"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn distinguishes_empty_from_unassembled_and_unknown_without_logging_content() {
        let mut d = StreamDiagnostics::default();
        d.event("message_stop", &json!({"type":"message_stop"}));
        assert_eq!(d.empty_evidence(), "no_content_observed");
        d.event(
            "content_block_start",
            &json!({"content_block":{"type":"text","text":"secret body"}}),
        );
        assert_eq!(d.empty_evidence(), "content_not_assembled");
        assert!(!serde_json::to_string(&d).unwrap().contains("secret"));
        let mut d = StreamDiagnostics::default();
        d.event(
            "secret event",
            &json!({"type":"secret type","payload":"secret value"}),
        );
        assert_eq!(d.empty_evidence(), "unrecognized_shape");
        assert!(!serde_json::to_string(&d).unwrap().contains("secret"));
        assert_eq!(d.events.len(), 1);
    }

    #[test]
    fn chat_tool_structure_with_missing_identity_is_not_an_empty_upstream_reply() {
        let mut d = StreamDiagnostics::default();
        d.event(
            "chat.chunk",
            &json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,"function":{"name":"read"}
        }]},"finish_reason":"stop"}]}),
        );
        assert_eq!(d.tool_blocks, 1);
        assert_eq!(d.content_bytes, 0);
        assert_eq!(d.empty_evidence(), "content_not_assembled");
    }
}
