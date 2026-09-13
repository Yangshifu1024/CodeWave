//! 防御性历史修复管线（[docs/p0-plan](../../../../docs/p0-plan.md) §8.2）：
//! sanitize（去 system、修截断 args、折叠重复；图片剥离按变体拆分）
//! → trim（预算内按轮边界裁剪）→
//! repair（tool_use/tool_result 配对不变量）。全部纯函数。

use crate::core::types::{Content, Message, Role};
use crate::util::token_est::est_tokens_message;

/// 保存前处理，返回警告列表。图片 payload 保留——重开会话后消息附件仍可显示
/// （token 估算按每图 1600，与会话未关闭时同语义；存储侧有 gzip + 8MB 上限兜底）。
pub fn prepare_for_save(mut msgs: Vec<Message>) -> Vec<Message> {
    let w = sanitize_for_save(&mut msgs);
    if !w.is_empty() {
        tracing::debug!("sanitize warnings: {w:?}");
    }
    repair(&mut msgs);
    msgs
}

/// 加载后处理。
pub fn prepare_on_load(mut msgs: Vec<Message>) -> Vec<Message> {
    repair(&mut msgs);
    msgs
}

/// sanitize（provider 修复变体）：在保存变体之上，把图片 payload 换成占位文本——
/// BadRequest 重试兜底依赖剥图减负。会话保存请用 [`sanitize_for_save`]。
pub fn sanitize(msgs: &mut Vec<Message>) -> Vec<String> {
    sanitize_inner(msgs, true)
}

/// sanitize（会话保存变体）：去 system、去 thinking、修截断工具参数、折叠重复
/// tool_use；图片 payload 保留。
pub fn sanitize_for_save(msgs: &mut Vec<Message>) -> Vec<String> {
    sanitize_inner(msgs, false)
}

/// sanitize 共同主体：`strip_images` 区分保存变体（保图）与 provider 修复变体（剥图）。
fn sanitize_inner(msgs: &mut Vec<Message>, strip_images: bool) -> Vec<String> {
    let mut warnings = Vec::new();
    msgs.retain(|m| m.role != Role::System);
    for m in msgs.iter_mut() {
        m.content = m
            .content
            .iter()
            .filter_map(|c| match c {
                Content::Image { media_type, .. } if strip_images => {
                    warnings.push("图片 payload 已剥离".into());
                    Some(Content::Text {
                        text: format!("[image {media_type} omitted]"),
                    })
                }
                Content::Thinking { .. } => None,
                other => Some(other.clone()),
            })
            .collect();
        // 折叠同 id 重复 tool_use（保留首个）；修截断 args
        let mut seen_ids = std::collections::HashSet::new();
        let mut fixed: Vec<Content> = Vec::new();
        for c in &m.content {
            match c {
                Content::ToolUse { id, name, args } => {
                    if !seen_ids.insert(id.clone()) {
                        warnings.push(format!("重复 tool_use 已折叠：{id}"));
                        continue;
                    }
                    match ensure_parseable_args(args) {
                        Ok(v) => fixed.push(Content::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            args: v,
                        }),
                        Err(raw) => {
                            warnings.push(format!("tool_use {id} 参数无法修复，已丢弃"));
                            tracing::debug!("丢弃截断 args（{name}）：{raw}");
                        }
                    }
                }
                other => fixed.push(other.clone()),
            }
        }
        m.content = fixed;
    }
    warnings
}

/// 尝试解析 args；失败则尝试补括号；仍失败返回 Err（原始字符串）。
fn ensure_parseable_args(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    match args {
        v @ serde_json::Value::Object(_) | v @ serde_json::Value::Array(_) => Ok(v.clone()),
        serde_json::Value::String(s) => parse_or_salvage(s).ok_or_else(|| s.clone()),
        serde_json::Value::Null => Ok(serde_json::json!({})),
        other => Ok(serde_json::json!({ "value": other })),
    }
}

/// 截断 args 打捞（[docs/p0-plan](../../../../docs/p0-plan.md) §6.3.2）：扫描字符串，补齐缺失的 `}` `]` 与尾逗号。
pub fn parse_or_salvage(s: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        return Some(v);
    }
    let mut in_string = false;
    let mut escape = false;
    let mut stack: Vec<u8> = Vec::new();
    for &b in s.as_bytes() {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape = true,
            b'"' => in_string = !in_string,
            b'{' | b'[' if !in_string => stack.push(b),
            b'}' | b']' if !in_string => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_string {
        return None; // 字符串中部截断；内容语义不可信
    }
    let mut repaired = s.trim_end().to_string();
    while repaired.ends_with(',') {
        repaired.pop();
    }
    for closer in stack.iter().rev() {
        repaired.push(match closer {
            b'{' => '}',
            _ => ']',
        });
    }
    serde_json::from_str(&repaired).ok()
}

/// repair：剥离孤儿 tool_result；为悬空 tool_use 补 [interrupted] 结果。
pub fn repair(msgs: &mut Vec<Message>) {
    // 收集全部 tool_use id
    let mut use_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in msgs.iter() {
        for c in &m.content {
            if let Content::ToolUse { id, .. } = c {
                use_ids.insert(id.clone());
            }
        }
    }
    // 移除孤儿结果
    for m in msgs.iter_mut() {
        m.content.retain(|c| match c {
            Content::ToolResult { tool_use_id, .. } => use_ids.contains(tool_use_id),
            _ => true,
        });
    }
    // 已有结果的 id 集合
    let mut answered: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in msgs.iter() {
        for c in &m.content {
            if let Content::ToolResult { tool_use_id, .. } = c {
                answered.insert(tool_use_id.clone());
            }
        }
    }
    // 悬空 tool_use：紧随其后补一条 Tool 消息（只补缺失的）
    let mut patched: Vec<Message> = Vec::with_capacity(msgs.len());
    for m in msgs.drain(..) {
        let missing: Vec<String> = m
            .content
            .iter()
            .filter_map(|c| match c {
                Content::ToolUse { id, .. } if !answered.contains(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        patched.push(m);
        let interrupted: Vec<Content> = missing
            .into_iter()
            .map(|id| Content::ToolResult {
                tool_use_id: id,
                content: "[interrupted] 工具调用被中断，未产生结果".into(),
                is_error: true,
            })
            .collect();
        if !interrupted.is_empty() {
            patched.push(Message::tool_results(interrupted));
        }
    }
    *msgs = patched;
}

/// trim：预算内从头按用户轮边界裁剪，保留最后 `keep_last` 轮。
/// 一轮 = 一条 User 消息及其后的 Assistant/Tool 消息。
pub fn trim(msgs: &mut Vec<Message>, budget_tokens: u64, keep_last: usize) {
    let total = |v: &[Message]| -> u64 { v.iter().map(est_tokens_message).sum() };
    // 找出全部轮起始下标
    let starts: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();
    if starts.len() <= keep_last {
        return;
    }
    // 每次调用只删一轮：主体经递归 `return trim(..)` 重入并重算轮边界
    // （clippy::never_loop 已确认循环体从不迭代）。
    if total(msgs) > budget_tokens && starts.len() > keep_last {
        // 删掉第一轮：从 starts[0] 到 starts[1] 之前（keep_last=0 且仅一轮超预算时
        // 没有 starts[1]——全裁而非索引 panic）
        let cut_end = starts.get(1).copied().unwrap_or(msgs.len());
        msgs.drain(..cut_end);
        trim(msgs, budget_tokens, keep_last) // 重算轮边界
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_use(id: &str) -> Content {
        Content::ToolUse {
            id: id.into(),
            name: "read".into(),
            args: json!({"files":[]}),
        }
    }
    fn tool_result(id: &str) -> Content {
        Content::ToolResult {
            tool_use_id: id.into(),
            content: "r".into(),
            is_error: false,
        }
    }

    // 用例 1：悬空 tool_use → 补 interrupted 结果
    #[test]
    fn dangling_tool_use_gets_interrupted_result() {
        let mut msgs = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![Content::Text { text: "x".into() }, tool_use("t1")],
                created_at: None,
            },
        ];
        repair(&mut msgs);
        assert_eq!(msgs.len(), 3);
        match &msgs[2].content[0] {
            Content::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                assert_eq!(tool_use_id, "t1");
                assert!(is_error);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // 用例 2：孤儿 tool_result → 移除
    #[test]
    fn orphan_tool_result_removed() {
        let mut msgs = vec![
            Message::user_text("q"),
            Message::tool_results(vec![tool_result("ghost")]),
        ];
        repair(&mut msgs);
        assert_eq!(msgs[1].content.len(), 0);
    }

    // 用例 3：截断工具参数 → 修复或丢弃（历史保持可解析）
    #[test]
    fn truncated_args_salvaged_or_dropped() {
        let mut msgs = vec![Message {
            role: Role::Assistant,
            created_at: None,
            content: vec![Content::ToolUse {
                id: "t".into(),
                name: "edit".into(),
                args: serde_json::Value::String(r#"{"files": [{"path": "a.py"}), "x": 1"#.into()),
            }],
        }];
        sanitize(&mut msgs);
        repair(&mut msgs);
        // 修复管线必须保证：每个 tool_use 要么参数合法，要么被丢弃并补了 interrupted 结果
        for m in &msgs {
            for c in &m.content {
                if let Content::ToolUse { args, .. } = c {
                    assert!(args.is_object());
                }
            }
        }
    }

    #[test]
    fn salvage_balances_brackets() {
        let v = parse_or_salvage(r#"{"a": {"b": 1"#);
        assert!(v.is_some());
        let v = parse_or_salvage(r#"{"a": [1, 2,"#);
        assert!(v.is_some());
        let v = parse_or_salvage(r#"{"a": "unclosed"#);
        assert!(v.is_none()); // 字符串中部截断不可信
    }

    // [docs/arithmetic-audit](../../../../docs/arithmetic-audit.md)#6：keep_last=0 且仅一轮超预算时没有 starts[1]——必须全裁
    // 而非索引 panic
    #[test]
    fn trim_keep_last_zero_single_round_no_panic() {
        let mut msgs = vec![Message::user_text("x".repeat(4096))];
        trim(&mut msgs, 16, 0);
        assert!(msgs.is_empty());
    }

    // 用例 4：超预算 → 按轮边界裁剪且配对完整
    #[test]
    fn trim_respects_round_boundaries_and_pairs() {
        let big = "x".repeat(60_000); // 每段 ≈16.5k tokens
        let mut msgs = Vec::new();
        for round in 0..8 {
            msgs.push(Message::user_text(format!("round{round} {big}")));
            msgs.push(Message {
                role: Role::Assistant,
                created_at: None,
                content: vec![
                    Content::Text { text: big.clone() },
                    tool_use(&format!("t{round}")),
                ],
            });
            msgs.push(Message::tool_results(vec![tool_result(&format!(
                "t{round}"
            ))]));
        }
        trim(&mut msgs, 256 * 1024 / 4, 2); // ≈64k token 预算
                                            // 剩余轮次在预算内，每轮配对完整
        let use_ids: std::collections::HashSet<String> = msgs
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|c| match c {
                Content::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        for m in &msgs {
            for c in &m.content {
                if let Content::ToolResult { tool_use_id, .. } = c {
                    assert!(use_ids.contains(tool_use_id), "trim 破坏了配对");
                }
            }
        }
        assert!(msgs.len() < 24);
    }

    // 用例 6（8MB 上限在 sessions/store 测试）；sanitize 变体：保存保图、provider 修复剥图：
    #[test]
    fn sanitize_image_handling_split_by_variant() {
        let base = || {
            vec![
                Message {
                    role: Role::System,
                    content: vec![Content::Text { text: "sys".into() }],
                    created_at: None,
                },
                Message::user_text("q"),
                Message {
                    role: Role::User,
                    content: vec![Content::Image {
                        media_type: "image/png".into(),
                        data: "AAAA".into(),
                    }],
                    created_at: None,
                },
            ]
        };

        // 会话保存变体：system 被去、图片 payload 保留（重开后附件仍显示）
        let mut msgs = base();
        sanitize_for_save(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert!(
            matches!(&msgs[1].content[0], Content::Image { media_type, .. } if media_type == "image/png")
        );

        // provider 修复变体（BadRequest 重试兜底）：图片变占位文本
        let mut msgs = base();
        sanitize(&mut msgs);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[1].content[0], Content::Text { text } if text.contains("omitted")));
    }
}
