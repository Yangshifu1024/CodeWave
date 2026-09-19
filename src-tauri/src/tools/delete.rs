//! delete 工具：危险路径守卫 + 递归删除选项。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

/// delete 工具入参。
#[derive(Deserialize)]
pub struct Args {
    /// 工作区相对路径（也接受写根内的绝对路径）。
    path: String,
    /// 删除非空目录必须显式置 true，默认 false。
    #[serde(default)]
    recursive: bool,
}

/// delete 工具：删除写根内的文件或目录。
/// 入参为 path + 可选 recursive；FileWrite 分级，ConfirmEach 档下经审批。
/// 安全语义：系统路径、各写根本体及其 .git 一律拒绝（E_DELETE_FORBIDDEN），
/// 危险路径判定独立于 fence，在工具内先行拦截；与 edit / create 共享进程级写互斥。
pub struct DeleteTool;

#[async_trait::async_trait]
impl Tool for DeleteTool {
    fn name(&self) -> &'static str {
        "delete"
    }
    fn description(&self) -> &'static str {
        "删除工作区内的文件或目录。系统路径、工作区根本身与 .git 一律拒绝。删除非空目录必须传 recursive=true。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["path"],
  "properties": {
    "path": {"type": "string"},
    "recursive": {"type": "boolean", "description": "默认 false"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::FileWrite
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let roots = ctx.write_roots();
        let resolved = match super::pathutil::resolve_write(&roots, &args.path) {
            Ok(p) => p,
            Err((c, m)) => return ToolOutcome::err(&c, m),
        };
        // H2：所有写根本身（含 extra）与各写根下的 .git 都在保护名单内
        let all_roots: Vec<&std::path::Path> = std::iter::once(roots.workspace.as_path())
            .chain(roots.extra.iter().map(|p| p.as_path()))
            .collect();
        if super::pathutil::is_dangerous_delete_multi(&resolved, &all_roots) {
            return ToolOutcome::err(
                "E_DELETE_FORBIDDEN",
                format!("拒绝删除危险路径：{}", args.path),
            );
        }
        // 严格文件隔离（[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）：
        // 非主 runtime 删除前认领目标（目录认领经祖先匹配覆盖其下文件）；兄弟已认领则拒绝
        if !ctx.rt.is_main_session {
            if let Err(conflicts) =
                crate::tools::claims::claim(&ctx.rt.id, std::slice::from_ref(&resolved))
            {
                return ToolOutcome::err(
                    "E_FILE_CLAIMED",
                    crate::tools::claims::denial_message(&conflicts),
                );
            }
        }
        // 进程级写互斥（[docs/tools-optimization-and-gap-fill-plan](../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 2）：与 edit/create 的写路径互斥；
        // 等锁期间监听取消（批次取消盲区修复），取消则不执行删除
        let _guard = match crate::tools::writelock::acquire_all(
            std::slice::from_ref(&resolved),
            Some(&ctx.cancel),
        )
        .await
        {
            Ok(g) => g,
            Err(_) => {
                return ToolOutcome::err("E_CANCELLED", "命令被用户取消");
            }
        };
        let meta = match std::fs::symlink_metadata(&resolved) {
            Ok(m) => m,
            Err(e) => return ToolOutcome::err("E_NOT_FOUND", format!("{}: {e}", args.path)),
        };
        let result = if meta.is_dir() {
            let empty = std::fs::read_dir(&resolved)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false);
            if empty {
                std::fs::remove_dir(&resolved)
            } else if args.recursive {
                std::fs::remove_dir_all(&resolved)
            } else {
                return ToolOutcome::err("E_NOT_EMPTY", "目录非空，需 recursive=true");
            }
        } else {
            std::fs::remove_file(&resolved)
        };
        match result {
            Ok(()) => ToolOutcome::ok(json!({ "deleted": args.path })),
            Err(e) => ToolOutcome::err("E_IO", format!("删除失败：{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 以全新临时工作区为根的 core + runtime；可附带 extra 写根。
    fn setup(extra_roots: Vec<String>) -> (ToolCtx, tempfile::TempDir, tempfile::TempDir) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "delete-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            extra_roots,
        );
        let ctx = ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        (ctx, ws, dd)
    }

    #[tokio::test]
    async fn deletes_file_and_reports_relative_path() {
        let (ctx, ws, _dd) = setup(vec![]);
        std::fs::write(ws.path().join("gone.txt"), b"x").unwrap();
        let out = DeleteTool.run(&ctx, json!({"path": "gone.txt"})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["deleted"], "gone.txt");
        assert!(!ws.path().join("gone.txt").exists());
    }

    #[tokio::test]
    async fn empty_dir_needs_no_recursive_non_empty_requires_it() {
        let (ctx, ws, _dd) = setup(vec![]);
        std::fs::create_dir_all(ws.path().join("empty")).unwrap();
        let out = DeleteTool.run(&ctx, json!({"path": "empty"})).await;
        assert!(out.ok, "{out:?}");
        assert!(!ws.path().join("empty").exists());

        std::fs::create_dir_all(ws.path().join("full")).unwrap();
        std::fs::write(ws.path().join("full/f.txt"), b"x").unwrap();
        let out = DeleteTool.run(&ctx, json!({"path": "full"})).await;
        assert_eq!(out.error.unwrap().code, "E_NOT_EMPTY");
        assert!(ws.path().join("full/f.txt").exists());

        let out = DeleteTool
            .run(&ctx, json!({"path": "full", "recursive": true}))
            .await;
        assert!(out.ok, "{out:?}");
        assert!(!ws.path().join("full").exists());
    }

    #[tokio::test]
    async fn missing_path_maps_to_e_not_found() {
        let (ctx, _ws, _dd) = setup(vec![]);
        let out = DeleteTool.run(&ctx, json!({"path": "no/such/file"})).await;
        assert_eq!(out.error.unwrap().code, "E_NOT_FOUND");
    }

    #[tokio::test]
    async fn outside_and_system_paths_rejected() {
        let (ctx, _ws, _dd) = setup(vec![]);
        let out = tempfile::tempdir().unwrap();
        let outside = out.path().join("victim.txt");
        std::fs::write(&outside, b"y").unwrap();
        // 所有写根之外的绝对路径 → 先被包含性检查拦下
        let out = DeleteTool
            .run(&ctx, json!({"path": outside.to_str().unwrap()}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_PATH_OUTSIDE");
        assert!(outside.exists(), "outside file must survive");
        // 缺参
        let out = DeleteTool.run(&ctx, json!({})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }

    #[tokio::test]
    async fn workspace_root_and_git_are_protected() {
        let (ctx, ws, _dd) = setup(vec![]);
        // 工作区根本身（相对 "." 解析到根）
        let out = DeleteTool.run(&ctx, json!({"path": "."})).await;
        assert_eq!(out.error.unwrap().code, "E_DELETE_FORBIDDEN");
        // 根的绝对路径形式同样拒绝
        let out = DeleteTool
            .run(&ctx, json!({"path": ws.path().to_str().unwrap()}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_DELETE_FORBIDDEN");
        // .git 及其下所有内容
        std::fs::create_dir_all(ws.path().join(".git")).unwrap();
        std::fs::write(ws.path().join(".git/HEAD"), b"ref").unwrap();
        let out = DeleteTool.run(&ctx, json!({"path": ".git"})).await;
        assert_eq!(out.error.unwrap().code, "E_DELETE_FORBIDDEN");
        let out = DeleteTool.run(&ctx, json!({"path": ".git/HEAD"})).await;
        assert_eq!(out.error.unwrap().code, "E_DELETE_FORBIDDEN");
        assert!(
            ws.path().join(".git/HEAD").exists(),
            ".git contents must survive"
        );
    }

    #[tokio::test]
    async fn extra_root_and_its_git_are_protected_h2() {
        let extra = tempfile::tempdir().unwrap();
        let extra_canon = std::fs::canonicalize(extra.path()).unwrap();
        let (ctx, _ws, _dd) = setup(vec![extra_canon.to_string_lossy().into_owned()]);
        std::fs::create_dir_all(extra.path().join(".git")).unwrap();
        std::fs::write(extra.path().join(".git/index"), b"x").unwrap();
        // extra 写根本身在保护名单内（评审 H2）
        let out = DeleteTool
            .run(&ctx, json!({"path": extra_canon.to_str().unwrap()}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_DELETE_FORBIDDEN");
        // 其 .git 同样受保护
        let out = DeleteTool
            .run(
                &ctx,
                json!({"path": extra.path().join(".git/index").to_str().unwrap()}),
            )
            .await;
        assert_eq!(out.error.unwrap().code, "E_DELETE_FORBIDDEN");
        assert!(extra.path().join(".git/index").exists());
        // extra 写根内的普通内容仍可删除
        std::fs::write(extra.path().join("ok.txt"), b"z").unwrap();
        let out = DeleteTool
            .run(
                &ctx,
                json!({"path": extra.path().join("ok.txt").to_str().unwrap()}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert!(!extra.path().join("ok.txt").exists());
    }
}
