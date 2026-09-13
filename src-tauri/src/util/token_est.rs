//! Token estimation: chars/4 + 10% headroom ([docs/p0-plan](../../../docs/p0-plan.md) §11).
//! Provider usage reports are used only for display calibration, not in the estimate itself (P0 simplification).

pub fn est_tokens_text(s: &str) -> u64 {
    let chars = s.chars().count() as u64;
    // CJK characters are denser; coarse per-class adjustment: 1 CJK char ≈ 0.6 tokens
    let cjk = s
        .chars()
        .filter(|c| {
            let c = *c as u32;
            (0x4E00..=0x9FFF).contains(&c) || (0x3000..=0x30FF).contains(&c)
        })
        .count() as u64;
    let ascii_chars = chars.saturating_sub(cjk);
    let base = ascii_chars / 4 + (cjk * 3) / 5;
    base + base / 10 // 10% headroom
}

pub fn est_tokens_message(msg: &crate::core::types::Message) -> u64 {
    use crate::core::types::Content;
    let mut total = 8; // role overhead
    for c in &msg.content {
        total += match c {
            Content::Text { text } => est_tokens_text(text),
            Content::Thinking { text } => est_tokens_text(text),
            Content::ToolUse { name, args, .. } => {
                est_tokens_text(name) + est_tokens_text(&args.to_string())
            }
            Content::ToolResult { content, .. } => est_tokens_text(content),
            Content::Image { .. } => 1600, // rough per-image estimate
        };
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_vs_cjk() {
        assert_eq!(est_tokens_text(""), 0);
        assert_eq!(est_tokens_text("abcdefgh"), 2); // 8/4
        let zh = est_tokens_text("你好世界"); // 4 cjk → 4*3/5=2, +10%
        assert!((2..=4).contains(&zh));
        assert!(est_tokens_text("你好世界") > 0);
    }

    #[test]
    fn cjk_density_and_headroom_exact() {
        // 4 CJK chars: base = 4*3/5 = 2, headroom floor(2/10) = 0
        assert_eq!(est_tokens_text("你好世界"), 2);
        // Pure ASCII: chars/4 with 10% headroom on the base
        assert_eq!(est_tokens_text("a"), 0); // 1/4 = 0
        assert_eq!(est_tokens_text("abcd"), 1); // 4/4 = 1, +0 headroom
        assert_eq!(est_tokens_text("abcdefghij"), 2); // 10/4 = 2, +0
        assert_eq!(est_tokens_text("x".repeat(80).as_str()), 22); // 20 + 2
    }

    #[test]
    fn message_estimates_carry_role_overhead() {
        use crate::core::types::{Content, Message};
        // Empty message still costs the role overhead
        let empty = Message {
            role: crate::core::types::Role::User,
            content: vec![],
            created_at: None,
        };
        assert_eq!(est_tokens_message(&empty), 8);

        let text = Message::user_text("abcdefgh"); // 8 + 2
        assert_eq!(est_tokens_message(&text), 10);

        let use_tool = Message {
            role: crate::core::types::Role::Assistant,
            content: vec![Content::ToolUse {
                id: "t1".into(),
                name: "read".into(),
                args: serde_json::json!({"path": "a.txt"}),
            }],
            created_at: None,
        };
        let expected = 8
            + est_tokens_text("read")
            + est_tokens_text(&serde_json::json!({"path": "a.txt"}).to_string());
        assert_eq!(est_tokens_message(&use_tool), expected);

        let result = Message::tool_results(vec![Content::ToolResult {
            tool_use_id: "t1".into(),
            content: "abcdefgh".into(),
            is_error: false,
        }]);
        assert_eq!(est_tokens_message(&result), 10);

        let image = Message {
            role: crate::core::types::Role::User,
            content: vec![Content::Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            }],
            created_at: None,
        };
        assert_eq!(est_tokens_message(&image), 8 + 1600);
    }
}
