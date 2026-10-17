use super::*;
use crate::tools::{Tool, ToolCtx, create::CreateTool, delete::DeleteTool, edit::EditTool};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, WriteRoots) {
    let workspace = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut roots = WriteRoots::new(workspace.path().to_path_buf(), data.path().to_path_buf());
    roots.project_dir = Some(workspace.path().join(".codewave"));
    (workspace, data, roots)
}

fn context(roots: &WriteRoots) -> ToolCtx {
    let core = crate::core::agent::test_support::make_core(roots);
    let rt = core.get_or_create_session(
        "protected-tools",
        roots.workspace.clone(),
        roots.project_dir.as_ref().map(|_| "project".into()),
        vec![],
        roots.project_dir.clone(),
        roots
            .extra
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    );
    rt.prefs.lock().unwrap().approval_mode = crate::core::prefs::ApprovalMode::FullAccess;
    ToolCtx {
        core,
        rt,
        batch_id: "protected".into(),
        call_index: 0,
        call_key: "protected:0".into(),
        provider_call_id: String::new(),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

#[test]
fn protects_actual_scopes_without_reserving_artifacts_or_lookalike_names() {
    let (ws, dd, mut roots) = fixture();
    let project = roots.project_dir.as_ref().unwrap();
    for path in [
        "config.json",
        "config.bak",
        "credential-commits.json",
        "mcp.json",
        "running.marker",
        "ui-state.json",
        "ui-state.json.corrupt",
        "ui-state.json.v3.bak",
        "projects/directories.json",
        "sessions/a.imgblob/hash",
        "sessions/a.toolres/call.json",
        "histories/subs/parent/child/0001.jsonl",
        "stats/2026-10-17.json",
        "locks/config.lock",
        "instances/alive/owner",
        "trust/index.json",
        "credential-leases/key",
    ] {
        assert_eq!(
            check_write(&roots, &dd.path().join(path)).unwrap_err().0,
            ERROR_CODE,
            "{path}"
        );
    }
    for path in [
        "project.json",
        "mcp.json",
        "tasks/task-id.json",
        "locks/project",
        "trust/index.json",
    ] {
        assert_eq!(
            check_write(&roots, &project.join(path)).unwrap_err().0,
            ERROR_CODE,
            "{path}"
        );
    }
    for path in [
        "memories/learned.md",
        "skills/demo/SKILL.md",
        "tmp/output.json",
        "logs/run.log",
        "tasks/output.json",
        "projects/report.json",
        "config.json.notes",
        "ui-state.json.version.bak",
    ] {
        assert!(check_write(&roots, &dd.path().join(path)).is_ok(), "{path}");
    }
    for path in [
        "memory/note.md",
        "skills/demo/SKILL.md",
        "lessons.md",
        "tasks/plan.md",
        "tasks/output/report.json",
        "temps/result.json",
    ] {
        assert!(check_write(&roots, &project.join(path)).is_ok(), "{path}");
    }
    assert!(check_write(&roots, ws.path()).is_err());
    assert!(check_direct_write(&roots, ws.path()).is_ok());
    assert!(check_write(&roots, project).is_err());
    assert!(check_direct_write(&roots, &project.join("mcp.json")).is_err());
    roots.project_dir = None;
    assert!(check_write(&roots, &ws.path().join(".codewave/project.json")).is_ok());
    assert!(check_write(&roots, &ws.path().join("nested/.codewave/config.json")).is_ok());
}

#[cfg(unix)]
#[test]
fn protects_symlink_entries_destinations_missing_targets_and_directory_ancestors() {
    use std::os::unix::fs::symlink;
    let (ws, dd, mut roots) = fixture();
    let external = tempfile::tempdir().unwrap();
    roots.extra.push(external.path().to_path_buf());
    let target = external.path().join("future-config");
    symlink(&target, dd.path().join("config.json")).unwrap();
    assert!(
        check_write(&roots, &target).is_err(),
        "a dangling state link still reserves its destination"
    );
    assert!(check_write(&roots, external.path()).is_err());
    assert!(check_write(&roots, &dd.path().join("config.json")).is_err());
    std::fs::remove_file(dd.path().join("config.json")).unwrap();
    std::fs::create_dir(dd.path().join("subdir")).unwrap();
    symlink(dd.path().join("subdir"), ws.path().join("link")).unwrap();
    assert!(!dd.path().join("config.json").exists());
    assert_eq!(
        check_write(&roots, &ws.path().join("link/../config.json"))
            .unwrap_err()
            .0,
        ERROR_CODE
    );
    symlink(dd.path(), ws.path().join("global-alias")).unwrap();
    assert!(
        check_write(
            &roots,
            &ws.path()
                .join("global-alias/sessions/new.toolres/call.json")
        )
        .is_err()
    );
    // Read access keeps its existing roots semantics.
    std::fs::write(dd.path().join("config.json"), b"readable").unwrap();
    assert!(
        crate::tools::pathutil::resolve_read(
            &roots,
            &ws.path().join("link/../config.json").to_string_lossy()
        )
        .is_ok()
    );
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn missing_case_variants_cannot_create_internal_state() {
    let (_ws, dd, roots) = fixture();
    for path in [
        dd.path().join("MCP.JSON"),
        dd.path().join("TRUST/new.json"),
        dd.path().join("UI-STATE.JSON.V3.BAK"),
        roots.project_dir.as_ref().unwrap().join("TASKS/New.JSON"),
    ] {
        assert!(!path.exists());
        assert_eq!(
            check_write(&roots, &path).unwrap_err().0,
            ERROR_CODE,
            "{}",
            path.display()
        );
    }
}

#[tokio::test]
async fn real_file_tools_deny_internal_state_in_full_access_without_partial_edits() {
    let (ws, dd, roots) = fixture();
    let ctx = context(&roots);
    let config = dd.path().join("config.json");
    std::fs::write(&config, b"original").unwrap();
    std::fs::write(ws.path().join("safe.txt"), b"safe").unwrap();
    let create = CreateTool
        .run(
            &ctx,
            json!({"path": config, "content":"overwrite", "overwrite":true}),
        )
        .await;
    let edit = EditTool
        .run(
            &ctx,
            json!({"files":[
                {"path":"safe.txt", "changes":[{"oldText":"safe","newText":"changed"}]},
                {"path":config,"changes":[{"oldText":"original","newText":"changed"}]}
            ]}),
        )
        .await;
    let delete = DeleteTool
        .run(&ctx, json!({"path": dd.path(), "recursive":true}))
        .await;
    for outcome in [create, edit, delete] {
        assert!(!outcome.ok, "{outcome:?}");
        assert_eq!(outcome.error.unwrap().code, ERROR_CODE);
    }
    assert_eq!(std::fs::read(config).unwrap(), b"original");
    assert_eq!(std::fs::read(ws.path().join("safe.txt")).unwrap(), b"safe");
    // Both global user content and project artifacts remain available to tools.
    for path in [
        dd.path().join("memories/lesson.md"),
        ws.path().join(".codewave/tasks/plan.md"),
    ] {
        let result = CreateTool
            .run(&ctx, json!({"path":path,"content":"allowed"}))
            .await;
        assert!(result.ok, "{result:?}");
        assert_eq!(std::fs::read(path).unwrap(), b"allowed");
    }
}

#[tokio::test]
async fn real_document_write_edit_and_restore_share_internal_state_gate() {
    use crate::tools::document::{edit::EditDocumentTool, transaction, write::WriteDocumentTool};
    let (_ws, dd, roots) = fixture();
    let ctx = context(&roots);
    let path = dd.path().join("histories/internal.docx");
    let created = WriteDocumentTool
        .run(
            &ctx,
            json!({"path":path,"docx":{"blocks":[{"type":"paragraph","text":"original"}]}}),
        )
        .await;
    assert_eq!(created.error.unwrap().code, ERROR_CODE);
    assert!(!path.exists());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"original").unwrap();
    let edited = EditDocumentTool
        .run(
            &ctx,
            json!({"path":path,"textEdits":[{"find":"original","replace":"changed"}]}),
        )
        .await;
    assert_eq!(edited.error.unwrap().code, ERROR_CODE);
    let restored = transaction::restore(
        dd.path(),
        &roots,
        &path,
        &dd.path().join("irrelevant-backup"),
    )
    .await
    .unwrap_err();
    assert_eq!(restored.code, ERROR_CODE);
    assert_eq!(std::fs::read(path).unwrap(), b"original");
}

#[cfg(unix)]
#[tokio::test]
async fn file_tools_recheck_a_target_rebound_while_waiting_for_its_lock() {
    use std::os::unix::fs::symlink;
    let (ws, dd, roots) = fixture();
    let ctx = context(&roots);
    let safe = ws.path().join("safe.txt");
    let config = dd.path().join("config.json");
    let alias = ws.path().join("alias.txt");
    std::fs::write(&safe, b"original").unwrap();
    std::fs::write(&config, b"host-state").unwrap();
    for tool in [
        &CreateTool as &dyn Tool,
        &EditTool as &dyn Tool,
        &DeleteTool as &dyn Tool,
    ] {
        symlink(&safe, &alias).unwrap();
        let guards = crate::tools::writelock::acquire_all(std::slice::from_ref(&alias), None)
            .await
            .unwrap();
        let args = match tool.name() {
            "create" => json!({"path":alias,"content":"changed","overwrite":true}),
            "edit" => {
                json!({"files":[{"path":alias,"changes":[{"oldText":"original","newText":"changed"}]}]})
            }
            _ => json!({"path":alias}),
        };
        let operation = tool.run(&ctx, args);
        tokio::pin!(operation);
        // Poll reaches the held production lock, proving the first check already
        // accepted the ordinary destination. No timing-based sleep is needed.
        assert!(futures::poll!(&mut operation).is_pending());
        std::fs::remove_file(&alias).unwrap();
        symlink(&config, &alias).unwrap();
        drop(guards);
        let outcome = operation.await;
        assert_eq!(outcome.error.unwrap().code, ERROR_CODE, "{}", tool.name());
        assert_eq!(std::fs::read(&config).unwrap(), b"host-state");
        assert_eq!(std::fs::read(&safe).unwrap(), b"original");
        std::fs::remove_file(&alias).unwrap();
    }
}
