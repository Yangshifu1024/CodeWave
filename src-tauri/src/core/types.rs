//! 核心消息表示（协议无关中间表示，[docs/technical-design](../../../docs/technical-design.md) §4.2.1）。
//! 序列化字节稳定要求：字段顺序由 struct 定义固定，列表显式排序。

use serde::{Deserialize, Serialize};

/// 会话 id 的类型别名（当前即 String，收敛命名便于契约阅读）。
pub type SessionId = String;

/// 消息角色（serde 小写形态与前端契约一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 协议无关的消息内容块（中间表示；serde 以 `type` 标签区分，snake_case 即前后端 wire 契约）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Image {
        media_type: String,
        data: String,
    },
}

/// 会话转录的一条消息：角色 + 有序内容块（+ 入转录时间）。
/// 序列化要求字节稳定：字段顺序由 struct 定义固定，列表显式排序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
    /// 入转录时间（UTC RFC3339）。只在首次进入会话转录时经 `stamped()` 盖章；
    /// 存量旧转录无此字段（serde default None）→ 前端渲染留空，绝不伪造时间。
    /// None 跳过序列化：provider 请求体与既有断言零变化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl Message {
    /// 入转录前盖章：created_at 为空时补当前 UTC 时间。
    /// 协议转换 / 请求体组装路径绝不调用（保 provider 请求字节稳定）。
    pub fn stamped(mut self) -> Self {
        if self.created_at.is_none() {
            self.created_at = Some(chrono::Utc::now().to_rfc3339());
        }
        self
    }

    /// 构造一条纯文本用户消息（created_at 留空，入转录时盖章）。
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![Content::Text { text: text.into() }],
            created_at: None,
        }
    }

    /// 构造一条工具结果消息（Tool 角色，内容为一组 ToolResult 块）。
    pub fn tool_results(results: Vec<Content>) -> Self {
        Message {
            role: Role::Tool,
            content: results,
            created_at: None,
        }
    }

    /// 拼接全部 Text 块。
    pub fn text_joined(&self) -> String {
        let mut out = String::new();
        for c in &self.content {
            if let Content::Text { text } = c {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    /// 首个 Text 块（供会话标题等场景）。
    pub fn first_text(&self) -> Option<&str> {
        self.content.iter().find_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }

    /// 收集本消息中的全部 ToolUse 块（保持原顺序）。
    pub fn tool_uses(&self) -> Vec<&Content> {
        self.content
            .iter()
            .filter(|c| matches!(c, Content::ToolUse { .. }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_stable() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                Content::Text { text: "hi".into() },
                Content::ToolUse {
                    id: "t1".into(),
                    name: "read".into(),
                    args: serde_json::json!({"files":[]}),
                },
            ],
            created_at: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(r#""type":"text""#));
        assert!(s.contains(r#""type":"tool_use""#));
        let m2: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn helpers() {
        let m = Message::user_text("hi");
        assert_eq!(m.text_joined(), "hi");
        let tr = Message::tool_results(vec![Content::ToolResult {
            tool_use_id: "t".into(),
            content: "ok".into(),
            is_error: false,
        }]);
        assert_eq!(tr.role, Role::Tool);
    }
}
