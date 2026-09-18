//! create 工具：创建文件（父目录自动补建）；目标已存在且未声明 overwrite 时拒绝执行。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{json, Value};

/// create 工具入参。
#[derive(Deserialize)]
pub struct Args {
    /// 工作区相对路径。
    path: String,
    /// 文件完整内容（默认空串）。
    #[serde(default)]
    content: String,
    /// 目标已存在时是否允许覆盖，默认 false。
    #[serde(default)]
    overwrite: bool,
}

/// create 工具：在工作区创建新文件。
/// 入参为 path + content + 可选 overwrite；FileWrite 分级，ConfirmEach 档下经审批（approval_detail 提供 diff/预览），
/// 写路径经 fence 解析与写根校验，创建区之外的写入按 confirm_outside_create 策略确认。
/// 成功写入后登记会话产物并做 post-write 校验（问题以 warnings 透出，不影响结果）。
pub struct CreateTool;

#[async_trait::async_trait]
impl Tool for CreateTool {
    fn name(&self) -> &'static str {
        "create"
    }
    fn description(&self) -> &'static str {
        "在工作区创建新文件（父目录自动创建）。文件已存在时失败，除非传 overwrite=true。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["path", "content"],
  "properties": {
    "path": {"type": "string"},
    "content": {"type": "string"},
    "overwrite": {"type": "boolean", "description": "默认 false"}
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
        // 严格文件隔离（[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）：
        // 非主 runtime 写前认领目标；兄弟任务已认领则拒绝并指引跳过上报
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
        // 进程级写互斥（[docs/tools-optimization-and-gap-fill-plan](../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 2）：
        // E_EXISTS 检查与写入共享同一把锁，防止跨 runtime 的并发 create/edit 交错；
        // 等锁期间监听取消（批次取消盲区修复），取消则不执行写入
        let _guard =
            match crate::tools::writelock::acquire_all(std::slice::from_ref(&resolved), Some(&ctx.cancel)).await {
                Ok(g) => g,
                Err(_) => {
                    return ToolOutcome::err("E_CANCELLED", "命令被用户取消");
                }
            };
        if resolved.exists() && !args.overwrite {
            return ToolOutcome::err(
                "E_EXISTS",
                format!("{} 已存在；覆盖请传 overwrite=true", args.path),
            );
        }
        if let Some(parent) = resolved.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutcome::err("E_IO", format!("创建目录失败：{e}"));
            }
        }
        // 写前基线（[docs/lsp-post-write-diagnostics](../../../docs/lsp-post-write-diagnostics.md)）：必须在写入**之前**取——
        // 此刻盘上仍是旧内容（新文件 = 读不到 → 基线视为空集），与写后诊断成对才算得出「本次新增」；
        // 未覆盖类型 / 临时会话 / 开关关闭 / server 未就绪时返回 None（本轮不回喂任何诊断）
        let vcfg = ctx.core.cfg.read().unwrap().validation.clone();
        let prev_content = std::fs::read(&resolved)
            .ok()
            .map(|b| crate::tools::read::read_text_content(&b));
        let target = super::validation::WriteTarget::new(
            resolved.clone(),
            args.path.clone(),
            prev_content,
        );
        let baseline = super::validation::baseline(ctx, &target, &vcfg).await;
        match crate::util::atomic::atomic_write(&resolved, args.content.as_bytes()) {
            Ok(()) => {
                // [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：产物登记（子代理归属主会话；路径规范化，
                // 同一文件的相对/绝对两种写法仍去重为一条；task runtime 跳过——该场景产物无消费方，避免边车泄漏；
                // 登记失败仅记日志，不影响工具结果）
                if !ctx.rt.is_task_runtime {
                    let canonical = crate::tools::pathutil::canonical_best_effort(&resolved)
                        .to_string_lossy()
                        .into_owned();
                    let owner = ctx
                        .rt
                        .root_session_id
                        .clone()
                        .unwrap_or_else(|| ctx.rt.id.clone());
                    if let Err(e) = ctx.core.store.append_artifact(
                        &owner,
                        &canonical,
                        crate::core::sessions::ArtifactOp::Create,
                    ) {
                        tracing::warn!("产物登记失败（create {}）：{e}", args.path);
                    }
                }
                // 写后语义校验：与写前基线配对（LSP 差集），文案经 warnings 透出；
                // 不影响写入结果本身（跳过必须如实带原因，绝不渲染成「通过」）
                let checked = super::validation::check(ctx, &target, baseline.as_ref(), &vcfg).await;
                let summary =
                    super::validation::summarize(std::slice::from_ref(&checked), &vcfg);
                let mut out =
                    ToolOutcome::ok(json!({ "path": args.path, "bytes": args.content.len() }));
                if !summary.is_empty() {
                    out.warnings.push(summary);
                }
                out
            }
            Err(e) => ToolOutcome::err("E_IO", format!("写入失败：{e}")),
        }
    }

    /// ConfirmEach 审批详情：目标已存在 → 新旧内容 diff；新文件 → 标题 + 内容预览（前 2000 字符）
    async fn approval_detail(&self, ctx: &ToolCtx, args: &Value) -> Option<String> {
        let a: Args = serde_json::from_value(args.clone()).ok()?;
        let roots = ctx.write_roots();
        let resolved = super::pathutil::resolve_write(&roots, &a.path).ok()?;
        if !resolved.exists() {
            let total = a.content.chars().count();
            let head: String = a.content.chars().take(2000).collect();
            let more = if total > 2000 { "\n…（截断预览）" } else { "" };
            return Some(format!("新文件 {}（{total} 字符）\n{head}{more}", a.path));
        }
        let old = String::from_utf8_lossy(&std::fs::read(&resolved).ok()?).into_owned();
        Some(format!(
            "### {}（覆盖已有文件）\n{}",
            a.path,
            crate::tools::edit::unified_diff(&old, &a.content)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_flow() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
        let ctx = ToolCtx {
            core: core.clone(),
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let tool = CreateTool;
        let out = tool
            .run(&ctx, serde_json::json!({"path":"a/b/c.txt","content":"hi"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read(ws.path().join("a/b/c.txt")).unwrap(), b"hi");
        // [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：创建成功即登记（绝对路径，count=1）
        {
            let items = core.store.load_artifacts("t");
            assert_eq!(items.len(), 1);
            assert!(items[0].path.ends_with("a/b/c.txt") || items[0].path.ends_with("a\\b\\c.txt"));
            assert_eq!(items[0].first_op, crate::core::sessions::ArtifactOp::Create);
            assert_eq!(items[0].count, 1);
        }
        // 重复创建被拒
        let out = tool
            .run(&ctx, serde_json::json!({"path":"a/b/c.txt","content":"x"}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_EXISTS");
        // 失败调用不登记
        assert_eq!(core.store.load_artifacts("t").len(), 1);
        // overwrite 覆盖 → 合并进同一路径条目，count=2
        let out = tool
            .run(
                &ctx,
                serde_json::json!({"path":"a/b/c.txt","content":"x","overwrite":true}),
            )
            .await;
        assert!(out.ok);
        let items = core.store.load_artifacts("t");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].count, 2);
        assert_eq!(items[0].last_op, crate::core::sessions::ArtifactOp::Create);
    }

    /// 在临时工作区上新建干净的 core/runtime（返回 TempDir 以保活）。
    fn setup(id: &str) -> (ToolCtx, tempfile::TempDir, tempfile::TempDir) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session(id, roots.workspace.clone(), None, vec![], None, vec![]);
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
    async fn approval_detail_new_file_previews_content() {
        let (ctx, ws, _dd) = setup("t2");
        let tool = CreateTool;

        // 新文件 → 标题 + 字符数 + 内容预览
        let detail = tool
            .approval_detail(
                &ctx,
                &json!({"path": "new.txt", "content": "body here"}),
            )
            .await
            .expect("new file must produce a detail");
        assert!(detail.contains("new.txt"));
        assert!(detail.contains("9 字符"));
        assert!(detail.contains("body here"));

        // 已有文件 → 新旧内容 diff
        std::fs::write(ws.path().join("new.txt"), "old line\n").unwrap();
        let detail = tool
            .approval_detail(
                &ctx,
                &json!({"path": "new.txt", "content": "new line\n"}),
            )
            .await
            .expect("overwrite must produce a diff detail");
        assert!(detail.contains("覆盖已有文件"));
        assert!(detail.contains("-old line"));
        assert!(detail.contains("+new line"));
    }

    #[tokio::test]
    async fn approval_detail_returns_none_for_unusable_args() {
        let (ctx, _ws, _dd) = setup("t3");
        let tool = CreateTool;
        // 无法解码的入参
        assert!(tool.approval_detail(&ctx, &json!({"nope": 1})).await.is_none());
        // 所有写根之外的路径解析失败
        let out = tempfile::tempdir().unwrap();
        let f = out.path().join("x.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(
            tool.approval_detail(&ctx, &json!({"path": f.to_str().unwrap(), "content": ""}))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn run_rejects_malformed_args() {
        let (ctx, _ws, _dd) = setup("t4");
        let out = CreateTool.run(&ctx, json!({"path": 123})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }

    /// 写后语义校验的接线（[docs/lsp-post-write-diagnostics](../../../docs/lsp-post-write-diagnostics.md)）：
    /// 三条早退路径（临时会话 / 未覆盖类型 / JSON 内置）必须各自给出如实文案——
    /// 不静默、也绝不把「没跑」渲染成「通过」。
    #[tokio::test]
    async fn run_reports_semantic_check_outcome_honestly() {
        let (ctx, _ws, _dd) = setup("t5");
        // 临时会话（无项目）→ 不做语义校验（不拉起 server）
        let out = CreateTool
            .run(&ctx, json!({"path": "src/main.rs", "content": "fn main() {}\n"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.warnings, vec!["（临时会话不做语义校验）".to_string()]);
        // 未覆盖类型 → 明说该类型不做语义校验（带扩展名）
        let out = CreateTool
            .run(&ctx, json!({"path": "ui/App.vue", "content": "<template/>"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.warnings, vec!["（该文件类型不做语义校验：vue）".to_string()]);
        // JSON 走内置解析：坏 JSON 回喂错误（文案里没有「通过」）
        let out = CreateTool
            .run(&ctx, json!({"path": "cfg/bad.json", "content": "{broken"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert!(out.warnings[0].contains("JSON 解析失败"), "{:?}", out.warnings);
        assert!(!out.warnings[0].contains("校验通过"), "{:?}", out.warnings);
        // 好 JSON → 通过（内置路径同样给出结论）
        let out = CreateTool
            .run(&ctx, json!({"path": "cfg/good.json", "content": "{\"a\":1}"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.warnings, vec!["（写入后语义校验通过：JSON）".to_string()]);
    }
}
