//! suggest 工具：1–4 条后续建议 chip；成功执行即结束本次 run（run 循环特判）。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{json, Value};

/// suggest 工具入参。
#[derive(Deserialize)]
pub struct Args {
    /// 建议文本列表（1–4 条，每条截取前 80 字符）。
    items: Vec<String>,
}

/// suggest 工具：向用户呈现 1–4 条可点击的后续建议。
/// 入参为 items 字符串数组；Interactive 分级——必须独占批次且作为收尾动作调用；
/// 经 run:suggestions 事件透出，前端渲染为 chip；commit 授权建议按约定置顶。
pub struct SuggestTool;

#[async_trait::async_trait]
impl Tool for SuggestTool {
    fn name(&self) -> &'static str {
        "suggest"
    }
    fn description(&self) -> &'static str {
        "向用户提供 1–4 条可点击的后续建议。请在完成工作后作为收尾动作调用。必须是所在轮次中唯一的工具调用。按优先级排序：当完成的工作改动了代码或文档时，把「授权 commit」的建议放第一条（本产品中 git 操作归用户所有）。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["items"],
  "properties": {
    "items": {"type": "array", "minItems": 1, "maxItems": 4, "items": {"type": "string"}}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Interactive
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        if args.items.is_empty() || args.items.len() > 4 {
            return ToolOutcome::err("E_ARGS", "items 需要 1–4 条");
        }
        if args.items.iter().any(|s| s.trim().is_empty()) {
            return ToolOutcome::err("E_ARGS", "建议不能为空");
        }
        let items: Vec<String> = prioritize_commit_first(
            args.items
                .iter()
                .map(|s| s.trim().chars().take(80).collect())
                .collect(),
        );
        ctx.core.sink.emit(
            &ctx.rt.id,
            "run:suggestions",
            json!({ "session": ctx.rt.id, "items": items }),
        );
        ToolOutcome::ok(json!({ "suggestions": items }))
    }
}

/// 建议 chip 排序约定：commit 授权建议置顶（产品约定：git 操作归用户所有，
/// 收尾后授权 commit 是最常见的下一步）；其余保持相对顺序。确定性重排，不依赖模型自觉。
fn prioritize_commit_first(mut items: Vec<String>) -> Vec<String> {
    if let Some(pos) = items
        .iter()
        .position(|s| s.to_lowercase().contains("commit"))
    {
        let item = items.remove(pos);
        items.insert(0, item);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_suggestion_moves_to_first() {
        let items = vec![
            "按清单手动验证".into(),
            "授权 commit 提交本次改动".into(),
            "补一条回归测试".into(),
        ];
        let out = prioritize_commit_first(items);
        assert!(
            out[0].contains("commit"),
            "commit 建议必须置顶，实际：{out:?}"
        );
        assert_eq!(out[1], "按清单手动验证");
        assert_eq!(out[2], "补一条回归测试");
    }

    #[test]
    fn no_commit_keeps_order() {
        let items = vec!["先看效果".into(), "稍后再定".into()];
        let out = prioritize_commit_first(items.clone());
        assert_eq!(out, items, "无 commit 条目时顺序不变");
    }

    #[test]
    fn commit_first_is_stable_and_case_insensitive() {
        // 已在首位：保持稳定（remove+insert 不改变位置）
        let items = vec!["Commit later".into(), "other".into()];
        let out = prioritize_commit_first(items.clone());
        assert_eq!(out, items);
        // 大小写不敏感
        let out2 = prioritize_commit_first(vec!["a".into(), "COMMIT NOW".into()]);
        assert_eq!(out2[0], "COMMIT NOW");
    }
}
