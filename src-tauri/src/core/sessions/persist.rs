//! 会话历史的**落盘形态**（[docs/session-history-limits](../../../../docs/session-history-limits.md)）：
//! 与内存 / wire 用的 [`Content`] 分开的一套 DTO，唯一的差别是图片。
//!
//! 为什么要另立 DTO：`Content` 同时是内存模型与 wire 模型——给它加可选字段会撞三协议
//! 请求体的字节断言，加新变体要改 5-6 处 match 穷尽点（三协议 + `repair::sanitize_inner`
//! + `core/agent/stream.rs`）。独立 DTO 让 **wire 与内存零改动**，前端也完全看不到引用形态。
//!
//! 图片的两种落盘形态：
//! - `image_blob`（正常路径）：base64 原文在 `sessions/<owner>.imgblob/<blob>`，历史里只留引用；
//! - `image_inline`（兜底）：外置失败 / 单图超限时原样内联；**本批之前的旧历史也长这样**
//!   （旧数据的 tag 是 `image`，靠 serde alias 兼容，因此旧会话照常可读、无不可逆迁移）。
//!
//! 读回时 blob 缺失（被手工删掉 / 磁盘损坏）降级为占位文本——不报错、不 panic、
//! 不阻断会话加载（[docs/session-restore-fidelity](../../../../docs/session-restore-fidelity.md)）。

use crate::core::sessions::{SessionStore, image_blobs};
use crate::core::types::{Content, Message, Role};
use serde::{Deserialize, Serialize};

/// 一条消息的落盘形态（字段名与 [`Message`] 同构：`role` / `content` / `created_at`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedMessage {
    /// 消息角色
    pub role: Role,
    /// 有序内容块（图片可能是引用形态）
    pub content: Vec<PersistedContent>,
    /// 入转录时间（UTC RFC3339）；旧转录无此字段（serde default）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// 内容块的落盘形态（`type` 标签与 [`Content`] 同构，图片除外）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PersistedContent {
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
    /// 内联：旧数据（tag 为 `image`，靠 alias 兼容），或外置失败 / 超限时的兜底
    #[serde(alias = "image")]
    ImageInline {
        media_type: String,
        data: String,
    },
    /// 外置：base64 存在 `sessions/<owner>.imgblob/<blob>`
    ImageBlob {
        media_type: String,
        blob: String,
    },
}

/// 内存历史 → 落盘形态：图片写 blob 并换成引用（失败 / 超限保持内联）。
/// 返回 `(落盘消息, 本次引用的 blob id 集合)`，引用集合供历史写成功后的 GC 使用。
pub fn to_persisted(
    store: &SessionStore,
    owner: &str,
    msgs: &[Message],
) -> (Vec<PersistedMessage>, Vec<String>) {
    let mut referenced: Vec<String> = Vec::new();
    let out = msgs
        .iter()
        .map(|m| PersistedMessage {
            role: m.role,
            content: m
                .content
                .iter()
                .map(|c| to_persisted_content(store, owner, c, &mut referenced))
                .collect(),
            created_at: m.created_at.clone(),
        })
        .collect();
    (out, referenced)
}

/// 单个内容块的转换（图片走外置；其余原样搬运）。
fn to_persisted_content(
    store: &SessionStore,
    owner: &str,
    c: &Content,
    referenced: &mut Vec<String>,
) -> PersistedContent {
    match c {
        Content::Text { text } => PersistedContent::Text { text: text.clone() },
        Content::Thinking { text } => PersistedContent::Thinking { text: text.clone() },
        Content::ToolUse { id, name, args } => PersistedContent::ToolUse {
            id: id.clone(),
            name: name.clone(),
            args: args.clone(),
        },
        Content::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => PersistedContent::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
            is_error: *is_error,
        },
        Content::Image { media_type, data } => match image_blobs::write(store, owner, data) {
            Some(blob) => {
                if !referenced.iter().any(|r| r == &blob) {
                    referenced.push(blob.clone());
                }
                PersistedContent::ImageBlob {
                    media_type: media_type.clone(),
                    blob,
                }
            }
            None => PersistedContent::ImageInline {
                media_type: media_type.clone(),
                data: data.clone(),
            },
        },
    }
}

/// 落盘形态 → 内存历史：按引用读回 base64。
/// blob 缺失降级为占位文本（`[image <media_type> 丢失]`）——**不报错、不 panic、不阻断加载**。
pub fn from_persisted(
    store: &SessionStore,
    owner: &str,
    msgs: Vec<PersistedMessage>,
) -> Vec<Message> {
    msgs.into_iter()
        .map(|m| Message {
            role: m.role,
            content: m
                .content
                .into_iter()
                .map(|c| from_persisted_content(store, owner, c))
                .collect(),
            created_at: m.created_at,
        })
        .collect()
}

/// 单个内容块的回读。
fn from_persisted_content(store: &SessionStore, owner: &str, c: PersistedContent) -> Content {
    match c {
        PersistedContent::Text { text } => Content::Text { text },
        PersistedContent::Thinking { text } => Content::Thinking { text },
        PersistedContent::ToolUse { id, name, args } => Content::ToolUse { id, name, args },
        PersistedContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => Content::ToolResult {
            tool_use_id,
            content,
            is_error,
        },
        PersistedContent::ImageInline { media_type, data } => Content::Image { media_type, data },
        PersistedContent::ImageBlob { media_type, blob } => {
            match image_blobs::read(store, owner, &blob) {
                Some(data) => Content::Image { media_type, data },
                None => {
                    tracing::warn!("图片 blob 缺失（{owner}/{blob}），降级为占位文本");
                    Content::Text {
                        text: format!("[image {media_type} 丢失]"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (SessionStore::new(dir.path().to_path_buf()), dir)
    }

    fn rich_history() -> Vec<Message> {
        vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking {
                        text: "先看图".into(),
                    },
                    Content::Text {
                        text: "答案".into(),
                    },
                    Content::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        args: json!({"files": ["a.png"]}),
                    },
                ],
                created_at: Some("2026-01-01T00:00:00+00:00".into()),
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "ok".into(),
                is_error: true,
            }]),
            Message {
                role: Role::User,
                content: vec![Content::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                }],
                created_at: None,
            },
        ]
    }

    #[test]
    fn roundtrip_is_equivalent_and_images_go_to_blobs() {
        let (store, _dir) = store();
        let msgs = rich_history();
        let (persisted, referenced) = to_persisted(&store, "s1", &msgs);
        // 图片已外置：落盘形态里是引用，且引用集合有且仅有一个 id
        assert_eq!(referenced.len(), 1);
        assert!(matches!(
            &persisted[3].content[0],
            PersistedContent::ImageBlob { media_type, blob }
                if media_type == "image/png" && blob == &referenced[0]
        ));
        // 读回：与原始内存历史逐字节等价（text / thinking / tool_use / tool_result / image 全覆盖）
        let back = from_persisted(&store, "s1", persisted);
        assert_eq!(back, msgs);
    }

    #[test]
    fn inline_fallback_when_blob_write_fails() {
        let (store, _dir) = store();
        // owner 非法 → 外置失败 → 保持内联（不报错、不丢图）
        let msgs = rich_history();
        let (persisted, referenced) = to_persisted(&store, "../evil", &msgs);
        assert!(referenced.is_empty());
        assert!(matches!(
            &persisted[3].content[0],
            PersistedContent::ImageInline { data, .. } if data == "AAAA"
        ));
        let back = from_persisted(&store, "../evil", persisted);
        assert_eq!(back, msgs);
    }

    /// 旧数据兼容：本批之前的历史里图片是 `{"type":"image",...}`（内联 base64）。
    /// 这类文件必须照常可读——否则升级即等于旧会话打不开。
    #[test]
    fn legacy_inline_image_json_still_parses() {
        let raw = r#"[
            {"role":"user","content":[{"type":"text","text":"q"}]},
            {"role":"user","content":[{"type":"image","media_type":"image/png","data":"AAAA"}]}
        ]"#;
        let persisted: Vec<PersistedMessage> = serde_json::from_str(raw).unwrap();
        assert!(matches!(
            &persisted[1].content[0],
            PersistedContent::ImageInline { data, .. } if data == "AAAA"
        ));
        let (store, _dir) = store();
        let back = from_persisted(&store, "s1", persisted);
        assert!(matches!(&back[1].content[0], Content::Image { data, .. } if data == "AAAA"));
    }

    #[test]
    fn persisted_wire_shape_uses_snake_case_tags() {
        let (store, _dir) = store();
        let (persisted, _) = to_persisted(&store, "s1", &rich_history());
        let s = serde_json::to_string(&persisted).unwrap();
        assert!(s.contains(r#""type":"text""#));
        assert!(s.contains(r#""type":"thinking""#));
        assert!(s.contains(r#""type":"tool_use""#));
        assert!(s.contains(r#""type":"tool_result""#));
        assert!(s.contains(r#""type":"image_blob""#));
        // 外置后历史里**不再出现 base64 原文**
        assert!(!s.contains("AAAA"));
        // 解析回来仍是同一份（标签与结构自洽）
        let again: Vec<PersistedMessage> = serde_json::from_str(&s).unwrap();
        assert_eq!(again, persisted);
    }

    #[test]
    fn missing_blob_degrades_to_placeholder_text() {
        let (store, _dir) = store();
        let (persisted, referenced) = to_persisted(&store, "s1", &rich_history());
        // blob 被手工删掉 / 磁盘损坏
        std::fs::remove_file(store.image_blobs_dir("s1").join(&referenced[0])).unwrap();
        let back = from_persisted(&store, "s1", persisted);
        // 不 panic、不报错：该图降级为占位文本，其余内容完好
        assert!(
            matches!(&back[3].content[0], Content::Text { text } if text == "[image image/png 丢失]")
        );
        assert_eq!(back[0], Message::user_text("q"));
        assert!(matches!(&back[1].content[0], Content::Thinking { text } if text == "先看图"));
    }

    #[test]
    fn identical_images_share_one_blob() {
        let (store, _dir) = store();
        let img = |data: &str| Content::Image {
            media_type: "image/png".into(),
            data: data.into(),
        };
        let msgs = vec![Message {
            role: Role::User,
            content: vec![img("AAAA"), img("AAAA"), img("BBBB")],
            created_at: None,
        }];
        let (_, referenced) = to_persisted(&store, "s1", &msgs);
        assert_eq!(referenced.len(), 2, "同内容去重、不同内容各一份");
    }
}
