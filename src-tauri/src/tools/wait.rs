//! wait 工具：可取消的 1–3600 秒等待（Interactive：批次内唯一调用）。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde_json::Value;

/// wait 工具：被动等待指定秒数（如等 dev server 启动）。
/// 入参为 seconds（1–3600）+ reason；Interactive 分级——必须独占批次；
/// 等待期间用户可随时取消（E_CANCELLED）。
pub struct WaitTool;

#[async_trait::async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &'static str {
        "wait"
    }
    fn description(&self) -> &'static str {
        "被动等待 1–3600 秒（如等待 dev server 启动）。用户可随时取消。必须是所在轮次中唯一的工具调用。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["seconds", "reason"],
  "properties": {
    "seconds": {"type": "integer", "minimum": 1, "maximum": 3600},
    "reason": {"type": "string", "description": "在等待什么"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Interactive
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let secs = match args["seconds"].as_u64() {
            Some(s) if (1..=3600).contains(&s) => s,
            _ => return ToolOutcome::err("E_ARGS", "seconds 必须在 1–3600"),
        };
        let reason = args["reason"].as_str().unwrap_or_default().to_string();
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {
                super::ToolOutcome::ok(serde_json::json!({ "waited_seconds": secs, "reason": reason }))
            }
            _ = ctx.cancel.cancelled() => {
                super::ToolOutcome::err("E_CANCELLED", "等待被用户取消")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ToolCtx {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "wait-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// 暂停的虚拟时钟下 1–3600s 等待瞬时完成。
    #[tokio::test(start_paused = true)]
    async fn waits_full_duration_and_reports_reason() {
        let ctx = ctx();
        let out = WaitTool
            .run(&ctx, json!({"seconds": 3600, "reason": "dev server boot"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["waited_seconds"], 3600);
        assert_eq!(out.data["reason"], "dev server boot");
    }

    #[tokio::test]
    async fn reason_defaults_to_empty_string() {
        let ctx = ctx();
        let out = WaitTool.run(&ctx, json!({"seconds": 1})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["reason"], "");
    }

    #[tokio::test]
    async fn seconds_outside_1_3600_rejected() {
        let ctx = ctx();
        for bad in [0u64, 3601, 9999] {
            let out = WaitTool.run(&ctx, json!({"seconds": bad})).await;
            assert_eq!(out.error.unwrap().code, "E_ARGS", "seconds={bad}");
        }
        // 缺失 / 非数值的 seconds
        let out = WaitTool.run(&ctx, json!({"reason": "x"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        let out = WaitTool.run(&ctx, json!({"seconds": "5"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }

    #[tokio::test]
    async fn cancel_beats_the_wait() {
        let ctx = ctx();
        ctx.cancel.cancel();
        let out = WaitTool
            .run(&ctx, json!({"seconds": 3600, "reason": "never finishes"}))
            .await;
        let err = out.error.unwrap();
        assert_eq!(err.code, "E_CANCELLED");
    }

    /// 等待进行中的取消同样生效（不只是预先取消的 token）。
    #[tokio::test(start_paused = true)]
    async fn cancel_mid_wait_interrupts() {
        let ctx = ctx();
        let cancel = ctx.cancel.clone();
        let task = tokio::spawn(async move {
            WaitTool
                .run(&ctx, json!({"seconds": 3600, "reason": "interrupt me"}))
                .await
        });
        cancel.cancel();
        let out = task.await.unwrap();
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
    }
}
