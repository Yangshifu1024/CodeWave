//! suggest 工具：1–4 条后续建议 chip；成功执行即结束本次 run（run 循环特判）。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

/// suggest 工具入参。
#[derive(Deserialize)]
pub struct Args {
    /// 建议文本列表（1–4 条，每条截取前 80 字符）。
    items: Vec<String>,
}

/// 计划纪律硬门（[docs/main-run-finish-with-pending-todos](../../../../docs/main-run-finish-with-pending-todos.md) §8.1）：
/// 主会话 + 本 run 碰过计划 + 计划仍有未完成项 → 不得以 suggest 宣告完成。
///
/// 为什么要 gate（与 `drive::text_turn_action` 的 `todos_pending` 同源、但走另一条路径）：
/// suggest 是模型显式的「我做完了，这是后续建议」信号，经 `BatchOutcome.suggest_items`
/// 让 `drive_agent` 置 `batch_done` 而正常成功收尾——**完全绕过** `calls.is_empty()` 分支里的
/// 收尾判定。二者若只修其一，模型改用 suggest 收尾就能绕开未完成计划。
///
/// 为什么拦截点在本工具内、而不是 `drive.rs` 的 `batch_done` 判定：
/// `run()` 在 emit `run:suggestions` **之前**就返回 ok（事件已发，前端 chips 已出现），
/// 到 `drive.rs` 判定时为时已晚。故必须前置到发事件之前。
///
/// 三个条件缺一不可：`is_main_session`（子代理/任务运行的收尾语义不同）、
/// `plan_called_this_run`（豁免上个 run 留下的陈旧计划，见 §8.2）、`has_pending`。
///
/// 返回终结性错误而非静默丢弃：模型需要知道下一步该做什么（和 `tools::batch` 的
/// plan 纪律门同范式——错误文案明示重试无法解除、须换推进方式）。
fn plan_pending_blocks_suggest(ctx: &ToolCtx) -> Option<ToolOutcome> {
    if !ctx.rt.is_main_session {
        return None;
    }
    if !ctx.rt.plan_called_this_run.load(Ordering::SeqCst) {
        return None;
    }
    if !crate::tools::plan::has_pending(&ctx.rt.todos.lock().unwrap()) {
        return None;
    }
    Some(ToolOutcome::err(
        "E_PLAN_PENDING",
        "当前计划仍有未完成项，暂不能以 suggest 收尾（建议已丢弃）。\
         请继续调用工具推进剩余待办；若这些待办其实已完成或不再需要，先用 plan 更新计划\
         （标 completed / 删除无关项），再调用 suggest 收尾。",
    ))
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
        "向用户提供 1–4 条可点击的后续建议；完成工作后作为收尾动作调用，可与 read/grep/list_files 等只读工具同批（[docs/suggest-mixed-batch](../../../docs/suggest-mixed-batch.md)，与 ask/wait 不同——这两个仍独占）。

入参 `items` 是 **`string[]`**（1–4 条字符串，每条 ≤80 字符、不能为空）。

**正确调用**：
{\"items\":[\"授权 commit 提交本次改动\",\"本地 tauri dev 验证\",\"去控制台 revoke key\"]}

**错误调用**（会立即报 `E_ARGS: invalid type: map, expected a string`）：
{\"items\":[{\"id\":\"a\",\"label\":\"A\",\"description\":\"...\",\"recommended\":true}]}
错误根因：把 `ask.options` 的对象形态（带 id/label/description/recommended）塞给了 `suggest`——两者不同。要结构化选项 + 用户点击，请改用 `ask` 而不是复用 `suggest` 的对象形态。

按优先级排序：当完成的工作改动了代码或文档时，把「授权 commit」的建议放第一条（本产品中 git 操作归用户所有）。"
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
        if let Some(blocked) = plan_pending_blocks_suggest(ctx) {
            return blocked;
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
    use serde_json::json;
    use std::sync::Arc;

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

    /// suggest 工具的门需要真实的 ToolCtx（rt + core），复用 core::agent::test_support 夹具。
    #[allow(clippy::type_complexity)]
    fn ctx_for(
        name: &str,
    ) -> (
        Arc<crate::core::agent::AgentCore>,
        Arc<crate::core::agent::SessionRuntime>,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session(name, roots.workspace.clone(), None, vec![], None, vec![]);
        // get_or_create_session 置的是 is_main_session，但测试要分别覆盖主/子两种情形
        (core, rt, ws, dd)
    }

    fn pending_todo() -> crate::tools::plan::Todo {
        crate::tools::plan::Todo {
            title: "补单测".into(),
            status: crate::tools::plan::TodoStatus::InProgress,
        }
    }

    /// §8.1 回归锚点：主会话 + 本 run 碰过计划 + 计划未收尾 → suggest 被拒（E_PLAN_PENDING）。
    /// 判别力：任一条件不满足即放行。
    #[tokio::test]
    async fn suggest_blocked_when_plan_pending_this_run() {
        let (core, rt, _ws, _dd) = ctx_for("suggest-gate-block");
        *rt.todos.lock().unwrap() = vec![pending_todo()];
        rt.plan_called_this_run.store(true, Ordering::SeqCst);
        let ctx = ToolCtx {
            core,
            rt: rt.clone(),
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let out = SuggestTool
            .run(&ctx, json!({"items": ["授权 commit"]}))
            .await;
        assert!(!out.ok, "计划未收尾时不得以 suggest 收尾：{out:?}");
        assert_eq!(
            out.error.as_ref().map(|e| e.code.clone()),
            Some("E_PLAN_PENDING".into())
        );
        let msg = out.error.unwrap().message;
        assert!(msg.contains("plan"), "错误须给出 plan 出口：{msg}");
        assert!(
            msg.contains("补单测") || msg.contains("未完成项"),
            "错误须说明原因：{msg}"
        );
    }

    /// 三个豁免面各自独立（判别力：逐一置位后必须放行）。
    #[tokio::test]
    async fn suggest_allowed_when_gate_conditions_not_met() {
        // ① 计划已全完成
        {
            let (core, rt, _ws, _dd) = ctx_for("suggest-gate-all-done");
            *rt.todos.lock().unwrap() = vec![crate::tools::plan::Todo {
                title: "已完成".into(),
                status: crate::tools::plan::TodoStatus::Completed,
            }];
            rt.plan_called_this_run.store(true, Ordering::SeqCst);
            let ctx = ToolCtx {
                core,
                rt,
                batch_id: "b".into(),
                call_index: 0,
                call_key: "b:0".into(),
                cancel: tokio_util::sync::CancellationToken::new(),
            };
            let out = SuggestTool.run(&ctx, json!({"items": ["再看看"]})).await;
            assert!(out.ok, "① 计划已全完成应放行：{out:?}");
        }
        // ② 陈旧计划（本 run 没碰计划）
        {
            let (core, rt, _ws, _dd) = ctx_for("suggest-gate-stale");
            *rt.todos.lock().unwrap() = vec![pending_todo()];
            // plan_called_this_run 保持 false
            let ctx = ToolCtx {
                core,
                rt,
                batch_id: "b".into(),
                call_index: 0,
                call_key: "b:0".into(),
                cancel: tokio_util::sync::CancellationToken::new(),
            };
            let out = SuggestTool.run(&ctx, json!({"items": ["再看看"]})).await;
            assert!(out.ok, "② 陈旧计划应放行：{out:?}");
        }
        // ③ 无计划
        {
            let (core, rt, _ws, _dd) = ctx_for("suggest-gate-no-plan");
            rt.plan_called_this_run.store(true, Ordering::SeqCst);
            let ctx = ToolCtx {
                core,
                rt,
                batch_id: "b".into(),
                call_index: 0,
                call_key: "b:0".into(),
                cancel: tokio_util::sync::CancellationToken::new(),
            };
            let out = SuggestTool.run(&ctx, json!({"items": ["再看看"]})).await;
            assert!(out.ok, "③ 无计划应放行：{out:?}");
        }
        // ④ 子代理即使碰过计划且未收尾也不拦（收尾语义不同：子代理靠 <report> 收尾）
        {
            let (core, parent, _ws, _dd) = ctx_for("suggest-gate-sub");
            // 用 new_sub 造真正的子代理 runtime（is_main_session=false）——
            // 不能改父 rt 的字段：它已被 core 的会话表共享，Arc::get_mut 拿不到 &mut。
            let sub = crate::core::agent::SessionRuntime::new_sub(&parent, "sub_gate".into());
            assert!(!sub.is_main_session, "new_sub 应是非主会话 runtime");
            *sub.todos.lock().unwrap() = vec![pending_todo()];
            sub.plan_called_this_run.store(true, Ordering::SeqCst);
            let ctx = ToolCtx {
                core,
                rt: sub,
                batch_id: "b".into(),
                call_index: 0,
                call_key: "b:0".into(),
                cancel: tokio_util::sync::CancellationToken::new(),
            };
            let out = SuggestTool.run(&ctx, json!({"items": ["再看看"]})).await;
            assert!(out.ok, "④ 子代理应放行：{out:?}");
        }
    }
}
