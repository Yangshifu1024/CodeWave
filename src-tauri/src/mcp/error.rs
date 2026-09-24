//! MCP 错误分类：把「靠错误文本子串判定」换成结构化类别（历史审查 M10）。
//!
//! 重连与重放策略**只**依赖 [`McpError::is_connection_class`]：只有连接类错误
//! （spawn 失败 / 握手失败或超时）才允许自动重连并重放**只读**调用；调用类错误
//! 可能已有副作用，绝不重放。取消优先于重连判定——被取消的调用不再重放。

use serde::Serialize;

/// 错误类别（事件载荷 `kind` 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorKind {
    /// 配置错：形状非法、传输歧义、不支持旧式 SSE、缺少必要字段、工具不可见
    Config,
    /// 子进程启动失败（命令不存在 / 无权限 / cwd 不存在）
    Spawn,
    /// 握手失败或超时（initialize / tools/list）
    Handshake,
    /// 工具调用失败（含 server 侧 is_error 与调用超时）
    Call,
    /// 被用户取消（run 取消 / 会话关闭 / 停止按钮）
    Cancelled,
}

impl McpErrorKind {
    /// 线缆用的稳定字符串（前端按它分类展示）。
    pub fn as_str(self) -> &'static str {
        match self {
            McpErrorKind::Config => "config",
            McpErrorKind::Spawn => "spawn",
            McpErrorKind::Handshake => "handshake",
            McpErrorKind::Call => "call",
            McpErrorKind::Cancelled => "cancelled",
        }
    }
}

/// 一条结构化 MCP 错误。
///
/// `hint` 给用户可操作的修正建议；`server_message` 保留 server 侧原文（排障用）。
/// 三个可选字段**始终序列化**（缺省为 `null`），与前端契约 `McpErrorPayload` 的
/// `string | null` 对齐——用 `skip_serializing_if` 会让字段消失成 `undefined`。
#[derive(Debug, Clone, Serialize)]
pub struct McpError {
    /// 错误类别
    pub kind: McpErrorKind,
    /// 面向用户/模型的错误正文
    pub message: String,
    /// 可操作建议（有则前端展示在展开区）
    pub hint: Option<String>,
    /// 调用类错误的 server 侧原始文本
    pub server_message: Option<String>,
}

impl McpError {
    fn new(kind: McpErrorKind, message: String) -> Self {
        McpError {
            kind,
            message,
            hint: None,
            server_message: None,
        }
    }

    /// 配置错。
    pub fn config(msg: impl Into<String>) -> Self {
        Self::new(McpErrorKind::Config, msg.into())
    }

    /// 配置错 + 可操作建议。
    pub fn config_hint(msg: impl Into<String>, hint: impl Into<String>) -> Self {
        let mut e = Self::config(msg);
        e.hint = Some(hint.into());
        e
    }

    /// 子进程启动失败。
    pub fn spawn(msg: impl Into<String>) -> Self {
        Self::new(McpErrorKind::Spawn, msg.into())
    }

    /// 握手失败或超时。
    pub fn handshake(msg: impl Into<String>) -> Self {
        Self::new(McpErrorKind::Handshake, msg.into())
    }

    /// 工具调用失败。
    pub fn call(msg: impl Into<String>) -> Self {
        Self::new(McpErrorKind::Call, msg.into())
    }

    /// 被取消。
    pub fn cancelled() -> Self {
        Self::new(McpErrorKind::Cancelled, "MCP 调用已取消".to_string())
    }

    /// 附加可操作建议（链式）。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 附加 server 侧原文（链式）。
    pub fn with_server_message(mut self, msg: impl Into<String>) -> Self {
        self.server_message = Some(msg.into());
        self
    }

    /// 是否连接类错误：**只有它**允许自动重连 + 重放只读调用。
    ///
    /// 取代原先对 `"channel closed"` / `"send failed"` 的文本子串匹配——
    /// 那种做法把业务错误与超时也判成可重连，会重放非幂等工具。
    pub fn is_connection_class(&self) -> bool {
        matches!(self.kind, McpErrorKind::Spawn | McpErrorKind::Handshake)
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for McpError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_wire_strings_are_stable() {
        assert_eq!(McpErrorKind::Config.as_str(), "config");
        assert_eq!(McpErrorKind::Spawn.as_str(), "spawn");
        assert_eq!(McpErrorKind::Handshake.as_str(), "handshake");
        assert_eq!(McpErrorKind::Call.as_str(), "call");
        assert_eq!(McpErrorKind::Cancelled.as_str(), "cancelled");
        for (k, s) in [
            (McpErrorKind::Config, "config"),
            (McpErrorKind::Spawn, "spawn"),
            (McpErrorKind::Handshake, "handshake"),
            (McpErrorKind::Call, "call"),
            (McpErrorKind::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(s));
        }
        // 前端按小写下划线字符串分类，序列化必须与 as_str 一致
    }

    #[test]
    fn only_connection_class_allows_replay() {
        assert!(McpError::spawn("x").is_connection_class());
        assert!(McpError::handshake("x").is_connection_class());
        // 这三类绝不重放：可能已有副作用 / 用户主动取消 / 配置问题重连也无用
        assert!(!McpError::call("x").is_connection_class());
        assert!(!McpError::cancelled().is_connection_class());
        assert!(!McpError::config("x").is_connection_class());
    }

    #[test]
    fn payload_always_serializes_optional_fields_as_null() {
        let v = serde_json::to_value(McpError::call("boom")).unwrap();
        assert_eq!(v["kind"], "call");
        assert_eq!(v["message"], "boom");
        // 契约是 `string | null`，字段必须存在且为 null（不能省略）
        assert!(v.get("hint").is_some());
        assert!(v["hint"].is_null());
        assert!(v.get("server_message").is_some());
        assert!(v["server_message"].is_null());
    }

    #[test]
    fn builders_attach_hint_and_server_message() {
        let e = McpError::config("bad").with_hint("fix it");
        assert_eq!(e.hint.as_deref(), Some("fix it"));
        let e = McpError::call("fail").with_server_message("server said no");
        assert_eq!(e.server_message.as_deref(), Some("server said no"));
        assert_eq!(e.to_string(), "fail");
        let e = McpError::config_hint("bad", "fix it");
        assert_eq!(e.hint.as_deref(), Some("fix it"));
        assert_eq!(e.kind, McpErrorKind::Config);
    }
}
