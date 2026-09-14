//! provider 层协议无关 DTO（[docs/technical-design](../../../docs/technical-design.md) §4.2.1）。

use crate::core::config::ModelConfig;
use crate::core::types::Message;
use serde::{Deserialize, Serialize};

/// 单个工具的定义：以 `name` + `description` + JSON Schema 描述供模型调用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    /// 工具名（与工具注册表一致，模型按名调用）
    pub name: String,
    /// 工具功能描述（进入模型 prompt）
    pub description: String,
    /// 严格 JSON Schema 字符串（描述入参结构）
    pub schema_json: String,
}

/// 输出超出 max_tokens 被截断时注入回复流尾部的统一提示
/// （[docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)：三协议共用同一常量，防止文案漂移）。
pub const MAX_TOKENS_NOTICE: &str = "\n[注意：回复因 max_tokens 被截断]";

/// 一次 LLM 流式请求的完整入参。
// Serialize：供会话 verbose 日志持久化完整请求文本（session_verbose）
#[derive(Debug, Clone, Serialize)]
pub struct StreamRequest {
    /// 目标模型配置（含 api_format、base_url、max_tokens、keys）
    pub model: ModelConfig,
    /// system prompt 稳定主块（六层组装产物；run 内字节冻结，跨 run 才可能变化）
    pub system_core: String,
    /// system prompt 可变尾段（plan-mode 等模式注入；允许 run 内切换）。
    /// Anthropic 侧单独成块且不打断点：切档只失效可变段之后的前缀，稳定主块缓存照常命中
    pub system_extra: String,
    /// 对话历史（含 tool_use / tool_result）
    pub messages: Vec<Message>,
    /// 本请求可用的工具定义列表
    pub tools: Vec<ToolDef>,
    /// Responses 协议的会话级 cache 路由 key（cache-first 用）
    pub cache_key: Option<String>,
    /// Anthropic 历史代际断点的消息下标（次新代前缀缓存条目；None = 历史不足不启用）
    pub cache_gen_index: Option<usize>,
    /// 请求级 reasoning effort（会话级覆盖 → 模型配置，None = 不发送；协议差异由各适配器处理）
    pub reasoning_effort: Option<crate::core::prefs::EffortLevel>,
}

impl StreamRequest {
    /// system 全文拼接（core + "\n" + extra；与拆分前的单字符串组装字节一致）。
    /// openai_chat / openai_responses 单字符串协议使用；anthropic 用双块承载。
    pub fn system_full(&self) -> String {
        if self.system_extra.is_empty() {
            self.system_core.clone()
        } else {
            format!("{}\n{}", self.system_core, self.system_extra)
        }
    }

    /// 供会话日志使用的脱敏序列化（[docs/session-logging-report](../../../docs/session-logging-report.md)）：所有 key 替换为 `***`，保证明文 key 不落盘
    /// （AGENTS.md 约束「API key 不落明文」；日志可导出排障、无保留期清理）。
    pub fn redacted_json(&self) -> String {
        let mut model = self.model.clone();
        model.keys = model.keys.iter().map(|_| "***".to_string()).collect();
        let clone = StreamRequest {
            model,
            system_core: self.system_core.clone(),
            system_extra: self.system_extra.clone(),
            messages: self.messages.clone(),
            tools: self.tools.clone(),
            cache_key: self.cache_key.clone(),
            cache_gen_index: self.cache_gen_index,
            reasoning_effort: self.reasoning_effort,
        };
        serde_json::to_string(&clone).unwrap_or_else(|_| "<serialize-failed>".into())
    }
}

/// 流式增量帧：三协议适配器统一产出的 delta 流，主循环按序消费。
/// `ToolCallBegin/ArgsDelta/End` 以 `index` 关联同一次工具调用。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    /// 正文文本增量
    Text {
        /// 本段增量文本
        text: String,
    },
    /// 思考过程（thinking/reasoning）增量
    Reasoning {
        /// 本段思考文本
        text: String,
    },
    /// 工具调用开始
    ToolCallBegin {
        /// 流内工具调用序号
        index: usize,
        /// 供应商返回的调用 id
        id: String,
        /// 工具名
        name: String,
    },
    /// 工具入参 JSON 的增量片段
    ToolCallArgsDelta {
        /// 关联的调用序号
        index: usize,
        /// 入参 JSON 片段（需按序拼接）
        fragment: String,
    },
    /// 工具调用结束（入参收齐）
    ToolCallEnd {
        /// 关联的调用序号
        index: usize,
    },
}

/// 一次请求的 token 用量（各维度独立累计）。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RunUsage {
    /// 输入 token 数
    pub input: u64,
    /// 输出 token 数
    pub output: u64,
    /// 缓存命中（读）token 数
    pub cache_read: u64,
    /// 缓存写入 token 数
    pub cache_write: u64,
}

impl std::ops::Add for RunUsage {
    type Output = RunUsage;
    /// 逐维度相加，供多次请求用量汇总。
    fn add(self, o: RunUsage) -> RunUsage {
        RunUsage {
            input: self.input + o.input,
            output: self.output + o.output,
            cache_read: self.cache_read + o.cache_read,
            cache_write: self.cache_write + o.cache_write,
        }
    }
}

/// provider 层统一错误：按重试语义分类（`is_transient` 判可重试），
/// `kind_tag` 随 run:error 事件下发供前端归因。
#[derive(Debug, thiserror::Error, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderError {
    /// 认证失败（HTTP 401/403），key 会被冷却 30 分钟
    #[error("认证失败：{0}")]
    Auth(String),
    /// 限流或瞬时错误（HTTP 408/429），可重试
    #[error("限流/瞬时错误：{0}")]
    RateLimited(String),
    /// 服务端错误（HTTP 5xx），可重试
    #[error("服务端错误：{0}")]
    Server(String),
    /// 请求被拒绝（HTTP 400/413/422），重试无意义
    #[error("请求被拒绝：{message}")]
    BadRequest {
        /// 供应商返回的可读原因
        message: String,
        /// 是否建议提示用户检查 prompt/入参（如上下文超限类）
        sanitize_hint: bool,
    },
    /// 网络层错误（连接失败/流中断），可重试
    #[error("网络错误：{0}")]
    Network(String),
    /// 计费/余额类（HTTP 402）：账户级永久错误，重试无意义，直接失败（错误文案直接透出真实原因）
    #[error("计费/余额错误：{0}")]
    Billing(String),
    /// 用户主动取消，属正常路径
    #[error("已取消")]
    Cancelled,
    /// 协议解析错误（响应形态不符合预期）
    #[error("协议错误：{0}")]
    Protocol(String),
}

impl ProviderError {
    /// 是否认证失败（key 冷却依据）。
    pub fn is_auth(&self) -> bool {
        matches!(self, ProviderError::Auth(_))
    }
    /// 是否瞬时错误（RateLimited/Server/Network，可重试）。
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited(_) | ProviderError::Server(_) | ProviderError::Network(_)
        )
    }
    /// 是否请求被拒（重试无意义，需要用户侧修正）。
    pub fn is_bad_request(&self) -> bool {
        matches!(self, ProviderError::BadRequest { .. })
    }

    /// run:error 事件载荷用的稳定分类标签（[docs/auth-error-guidance](../../../docs/auth-error-guidance.md)）；与 serde 变体标签一一对应。
    pub fn kind_tag(&self) -> &'static str {
        match self {
            ProviderError::Auth(_) => "auth",
            ProviderError::RateLimited(_) => "rate_limited",
            ProviderError::Server(_) => "server",
            ProviderError::BadRequest { .. } => "bad_request",
            ProviderError::Network(_) => "network",
            ProviderError::Billing(_) => "billing",
            ProviderError::Cancelled => "cancelled",
            ProviderError::Protocol(_) => "protocol",
        }
    }

    /// HTTP 状态码 → 错误分类映射：错误详情优先取供应商自带的 error.message（见下方 `api_error_message`），
    /// 取不到则回退原始 body 片段（截前 500 字符）。
    pub fn from_status(status: reqwest::StatusCode, body: &str) -> Self {
        let snippet: String = body.chars().take(500).collect();
        let code = status.as_u16();
        // [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：优先取供应商自带 error.message
        //（可读的一行原因，如智谱 type 1001），其余形态保留原始 JSON 片段。
        let detail = match api_error_message(body) {
            Some(m) => format!("{m} (HTTP {code})"),
            None => snippet.clone(),
        };
        match code {
            401 | 403 => ProviderError::Auth(detail),
            402 => ProviderError::Billing(detail),
            408 | 429 => ProviderError::RateLimited(detail),
            400 | 413 | 422 => ProviderError::BadRequest {
                message: detail,
                sanitize_hint: true,
            },
            500..=599 => ProviderError::Server(detail),
            _ => ProviderError::Protocol(format!("HTTP {code}: {snippet}")),
        }
    }
}

/// [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：从错误 body 提取供应商自带的 `error.message`
///（`{"error":{"message":"..."}}` 形态，OpenAI 兼容与 Anthropic 皆同）；形态不符或 message 为空返回 None。
fn api_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let msg = v.get("error")?.get("message")?.as_str()?.trim();
    (!msg.is_empty()).then(|| msg.to_string())
}

/// 装配块：text/thinking 按到达顺序穿插（相邻同类段合并）；
/// Tool(usize) 记录工具调用在穿插序列中的真实位置（值为 Assembled::tool_calls 的下标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmBlock {
    /// 正文文本段
    Text(String),
    /// 思考过程段
    Thinking(String),
    /// 工具调用占位（payload 为 tool_calls 下标）
    Tool(usize),
}

/// 把 SSE 流式 delta 重新装配为一条 assistant Message（主循环消费）。
#[derive(Debug, Default)]
pub struct Assembled {
    /// 按到达顺序排列的装配块（text/thinking 穿插 + 工具调用占位）
    pub blocks: Vec<AsmBlock>,
    /// 本回合的工具调用（含拼接完成的原始入参 JSON）
    pub tool_calls: Vec<AssembledToolCall>,
}

impl Assembled {
    /// 追加正文文本增量；与前一段同类则原地拼接，否则新开一段。
    pub fn push_text(&mut self, s: &str) {
        match self.blocks.last_mut() {
            Some(AsmBlock::Text(buf)) => buf.push_str(s),
            _ => self.blocks.push(AsmBlock::Text(s.to_string())),
        }
    }

    /// 追加思考文本增量；与前一段同类则原地拼接，否则新开一段。
    pub fn push_thinking(&mut self, s: &str) {
        match self.blocks.last_mut() {
            Some(AsmBlock::Thinking(buf)) => buf.push_str(s),
            _ => self.blocks.push(AsmBlock::Thinking(s.to_string())),
        }
    }

    /// 全空判定（空响应检测）。
    pub fn is_empty(&self) -> bool {
        self.tool_calls.is_empty()
            && self.blocks.iter().all(|b| match b {
                AsmBlock::Text(t) | AsmBlock::Thinking(t) => t.is_empty(),
                AsmBlock::Tool(_) => true, // Tool 块不影响空判定（身份信息在 tool_calls）
            })
    }

    /// 拼接全部文本段（本次 run 的最终回复文本；与旧版单字符串拼接语义一致）。
    pub fn joined_text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            if let AsmBlock::Text(t) = b {
                out.push_str(t);
            }
        }
        out
    }
}

/// 一次装配完成的工具调用：入参 JSON 片段已按序拼成 `args_raw`，由主循环解析执行。
#[derive(Debug)]
pub struct AssembledToolCall {
    /// 流内工具调用序号（对应 ToolCallBegin/End 的 index）
    pub index: usize,
    /// 供应商返回的调用 id
    pub id: String,
    /// 工具名
    pub name: String,
    /// 拼接完成的原始入参 JSON 文本
    pub args_raw: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HTTP 状态码 → 错误分类：402 计费单独归入 Billing 变体（不重试），文案透出真实原因。
    #[test]
    fn from_status_classifies_billing_402() {
        let err = ProviderError::from_status(
            reqwest::StatusCode::from_u16(402).unwrap(),
            r#"{"error":{"message":"Insufficient Balance"}}"#,
        );
        assert!(matches!(err, ProviderError::Billing(_)), "got {err:?}");
        assert!(err.to_string().contains("计费/余额错误"), "{err}");
        assert!(err.to_string().contains("Insufficient Balance"), "{err}");
        // Adjacent-branch regression: 401/403 → Auth and 429 → RateLimited are unaffected
        assert!(matches!(
            ProviderError::from_status(reqwest::StatusCode::from_u16(401).unwrap(), "x"),
            ProviderError::Auth(_)
        ));
        assert!(matches!(
            ProviderError::from_status(reqwest::StatusCode::from_u16(429).unwrap(), "x"),
            ProviderError::RateLimited(_)
        ));
    }

    /// [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：body 带供应商自带 error.message 时替换原始 JSON 片段
    ///（智谱 type-1001 形态：用户看到可读原因 + 状态码）。
    #[test]
    fn from_status_extracts_api_error_message() {
        let err = ProviderError::from_status(
            reqwest::StatusCode::from_u16(401).unwrap(),
            r#"{"error":{"message":"Header中未收到Authorization参数，无法进行身份验证。","type":"1001"}}"#,
        );
        assert!(matches!(err, ProviderError::Auth(_)), "got {err:?}");
        let text = err.to_string();
        assert!(text.contains("Header中未收到Authorization参数"), "{text}");
        assert!(text.contains("(HTTP 401)"), "{text}");
        assert!(!text.contains("{\"error\""), "原始 JSON 不应出现在文案：{text}");
        // 非 string / 缺失 message 时保留原始片段路径
        let fallback = ProviderError::from_status(
            reqwest::StatusCode::from_u16(403).unwrap(),
            r#"{"error":{"message":123}}"#,
        );
        assert!(fallback.to_string().contains("{\"error\""), "{fallback}");
    }

    /// [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：非 JSON body（网关/HTML 页面）原样回退到原始片段。
    #[test]
    fn from_status_keeps_snippet_for_non_json_body() {
        let err = ProviderError::from_status(
            reqwest::StatusCode::from_u16(401).unwrap(),
            "plain gateway error",
        );
        assert!(matches!(err, ProviderError::Auth(_)), "got {err:?}");
        assert!(err.to_string().contains("plain gateway error"), "{err}");
    }

    /// [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：run:error 载荷的分类标签与 serde 变体标签一一对应。
    #[test]
    fn kind_tags_are_stable() {
        assert_eq!(ProviderError::Auth("x".into()).kind_tag(), "auth");
        assert_eq!(ProviderError::Billing("x".into()).kind_tag(), "billing");
        assert_eq!(ProviderError::Network("x".into()).kind_tag(), "network");
        assert_eq!(ProviderError::Cancelled.kind_tag(), "cancelled");
    }

    /// [docs/session-logging-report](../../../docs/session-logging-report.md) 回归：verbose 会话日志中的完整请求文本必须脱敏——明文 key 绝不能落盘。
    #[test]
    fn redacted_json_masks_keys() {
        let model = crate::core::config::ModelConfig {
            keys: vec!["sk-plaintext-SECRET-1".into(), "sk-plaintext-SECRET-2".into()],
            ..Default::default()
        };
        let req = StreamRequest {
            model,
            system_core: "sys".into(),
            system_extra: String::new(),
            messages: vec![crate::core::types::Message::user_text("hi")],
            tools: vec![],
            cache_key: None,
            cache_gen_index: None,
            reasoning_effort: None,
        };
        let body = req.redacted_json();
        assert!(
            !body.contains("sk-plaintext"),
            "明文 key 泄漏进日志：{body}"
        );
        assert!(body.contains("\"***\""), "脱敏标记缺失：{body}");
        assert!(
            body.contains("\"system_core\":\"sys\""),
            "其余字段应原样保留：{body}"
        );
    }

    /// system_full 拼接字节：extra 为空时等于 core（与拆分前单字符串一致），非空时以单个换行衔接。
    #[test]
    fn system_full_joins_core_and_extra() {
        let base = StreamRequest {
            model: crate::core::config::ModelConfig::default(),
            system_core: "CORE".into(),
            system_extra: String::new(),
            messages: vec![],
            tools: vec![],
            cache_key: None,
            cache_gen_index: None,
            reasoning_effort: None,
        };
        assert_eq!(base.system_full(), "CORE");
        let with_extra = StreamRequest {
            system_extra: "EXTRA".into(),
            ..base
        };
        assert_eq!(with_extra.system_full(), "CORE\nEXTRA");
    }

    #[test]
    fn assembled_blocks_merge_and_preserve_order() {
        let mut a = Assembled::default();
        a.push_thinking("think1");
        a.push_text("hello ");
        a.push_text("world");
        a.push_thinking("think2");
        assert_eq!(
            a.blocks,
            vec![
                AsmBlock::Thinking("think1".into()),
                AsmBlock::Text("hello world".into()),
                AsmBlock::Thinking("think2".into()),
            ]
        );
        assert_eq!(a.joined_text(), "hello world");
        assert!(!a.is_empty());

        // 同类保持单段：连续 push 原地合并；空字符串段算「结构非空但文本为空」
        let mut b = Assembled::default();
        b.push_text("");
        assert!(b.is_empty(), "空文本段视为空响应");
        b.push_text("x");
        assert!(!b.is_empty());
    }
}
