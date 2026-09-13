//! batch_read：read 的兼容别名（为历史会话保留，已标记 deprecated）。

use super::read::ReadTool;
use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde_json::Value;

/// batch_read 工具：wire 契约与 `read` 完全一致，全部逻辑直接委托给 ReadTool。
/// 保留它只是为了让旧会话回放时不因工具名缺失而报错，新对话应使用 read。
pub struct BatchReadTool;

#[async_trait::async_trait]
impl Tool for BatchReadTool {
    fn name(&self) -> &'static str {
        "batch_read"
    }
    fn description(&self) -> &'static str {
        "read 的废弃别名（仅为兼容保留）。请改用 read。"
    }
    fn schema(&self) -> &'static str {
        ReadTool.schema()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        ReadTool.run(ctx, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 共享的测试上下文，连同工作区 TempDir 一起返回（保活以维持工作区可写）。
    fn ctx() -> (ToolCtx, tempfile::TempDir) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "batch-read-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        let tool_ctx = ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        (tool_ctx, ws)
    }

    /// 兼容别名：wire 契约必须与 `read` 逐字一致。
    #[test]
    fn schema_name_and_kind_mirror_read() {
        assert_eq!(BatchReadTool.name(), "batch_read");
        assert_eq!(BatchReadTool.schema(), ReadTool.schema());
        assert_eq!(BatchReadTool.kind(), ToolKind::ReadOnly);
    }

    #[tokio::test]
    async fn delegates_read_batch_semantics() {
        let (ctx, _ws) = ctx();
        std::fs::write(ctx.workspace().join("a.txt"), "line1\nline2\n").unwrap();
        let out = BatchReadTool
            .run(
                &ctx,
                json!({"files": [{"path": "a.txt"}, {"path": "a.txt", "startLine": 2}]}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        let files = out.data["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[0]["kind"], "text");
        assert!(files[0]["content"].as_str().unwrap().contains("line1"));
        assert!(files[0]["version"].is_string(), "version token present");
        assert_eq!(files[1]["start_line"], 2, "startLine filtering honored");
        // 文本读取不占用多模态通道
        assert!(out.extra_model_content.is_empty());
    }

    #[tokio::test]
    async fn read_error_paths_surface_unchanged() {
        let (ctx, _ws) = ctx();
        // 空批次
        let out = BatchReadTool.run(&ctx, json!({"files": []})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        // 文件不存在
        let out = BatchReadTool
            .run(&ctx, json!({"files": [{"path": "nope.txt"}]}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_NOT_FOUND");
        // 工作区之外
        let outside = tempfile::tempdir().unwrap();
        let f = outside.path().join("x.txt");
        std::fs::write(&f, b"x").unwrap();
        let out = BatchReadTool
            .run(&ctx, json!({"files": [{"path": f.to_str().unwrap()}]}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_PATH_OUTSIDE");
    }
}
