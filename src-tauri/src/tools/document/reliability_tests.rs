//! End-to-end tests call the production tools/transaction, not substitute writers.
use super::{backup, edit::EditDocumentTool, faults, transaction, write::WriteDocumentTool};
use crate::tools::{Tool, ToolCtx, ToolKind, pathutil::WriteRoots};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn setup() -> (ToolCtx, tempfile::TempDir, tempfile::TempDir) {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = WriteRoots::new(ws.path().to_path_buf(), dd.path().to_path_buf());
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        &uuid::Uuid::new_v4().to_string(),
        roots.workspace,
        None,
        vec![],
        None,
        vec![],
    );
    (context(core, rt), ws, dd)
}
fn context(
    core: Arc<crate::core::agent::AgentCore>,
    rt: Arc<crate::core::agent::SessionRuntime>,
) -> ToolCtx {
    ToolCtx {
        core,
        rt,
        batch_id: "document-test".into(),
        call_index: 0,
        call_key: "document-test:0".into(),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}
fn sample(dir: &Path, ext: &str) -> (PathBuf, Value) {
    let path = dir.join(format!("sample.{ext}"));
    if ext == "docx" {
        let spec =
            serde_json::from_value(json!({"blocks":[{"type":"paragraph","text":"original"}]}))
                .unwrap();
        std::fs::write(&path, super::write_word::generate(&spec).unwrap()).unwrap();
        (
            path.clone(),
            json!({"path":path,"textEdits":[{"find":"original","replace":"updated"}]}),
        )
    } else {
        let mut book = umya_spreadsheet::new_file();
        book.sheet_by_name_mut("Sheet1")
            .unwrap()
            .cell_mut("A1")
            .set_value_string("original");
        umya_spreadsheet::writer::xlsx::write(&book, &path).unwrap();
        (
            path.clone(),
            json!({"path":path,"edits":[{"sheet":"Sheet1","cell":"A1","text":"updated"}]}),
        )
    }
}
fn create_args(path: &Path, ext: &str) -> Value {
    if ext == "docx" {
        json!({"path":path,"docx":{"blocks":[{"type":"paragraph","text":"new"}]}})
    } else {
        json!({"path":path,"sheets":[{"name":"S","rows":[["new"]]}]})
    }
}

#[tokio::test]
async fn document_backup_failure_keeps_original_in_all_edit_paths() {
    let cases = [
        ("source_read", "E_SOURCE_READ"),
        ("snapshot", "E_IO"),
        ("backup_dir", "E_BACKUP_FAILED"),
        ("backup_open", "E_BACKUP_FAILED"),
        ("backup_write", "E_BACKUP_FAILED"),
        ("backup_flush", "E_BACKUP_FAILED"),
        ("backup_sync", "E_BACKUP_FAILED"),
        ("backup_verify", "E_BACKUP_FAILED"),
        ("commit", "E_IO"),
    ];
    for ext in ["xlsx", "xlsm", "docx"] {
        for (point, code) in cases {
            let (ctx, ws, dd) = setup();
            let (path, args) = sample(ws.path(), ext);
            let before = std::fs::read(&path).unwrap();
            let _fault = faults::install(&path, point, || {
                Err(std::io::Error::other("injected I/O failure"))
            });
            let result = EditDocumentTool.run(&ctx, args.clone()).await;
            assert!(!result.ok, "{ext}/{point}: {result:?}");
            assert_eq!(result.error.as_ref().unwrap().code, code, "{ext}/{point}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{ext}/{point}");
            assert!(ctx.core.store.load_artifacts(&ctx.rt.id).is_empty());
            let compact = crate::tools::compact::compact_for_model(
                ToolKind::FileWrite,
                "edit_document",
                &result,
                false,
            );
            assert!(compact.contains(code));
            assert!(result.warnings.is_empty());
            let backups = backup::list_for(dd.path(), &path);
            if point == "commit" {
                assert_eq!(backups.len(), 1);
                assert_eq!(std::fs::read(&backups[0]).unwrap(), before);
                assert!(compact.contains(&backups[0].to_string_lossy().to_string()));
            } else {
                assert!(backups.is_empty(), "partial backup exposed: {ext}/{point}");
            }
            if backup::dir(dd.path()).exists() {
                assert_eq!(
                    std::fs::read_dir(backup::dir(dd.path())).unwrap().count(),
                    backups.len(),
                    "temporary leaked: {ext}/{point}"
                );
            }
            // All error paths dropped their path lock; the next attempt can commit.
            assert!(EditDocumentTool.run(&ctx, args).await.ok);
        }
    }
}

#[tokio::test]
async fn document_success_receipt_is_reversible_and_model_visible() {
    for ext in ["xlsx", "xlsm", "docx"] {
        let (ctx, ws, dd) = setup();
        let (path, args) = sample(ws.path(), ext);
        let before = std::fs::read(&path).unwrap();
        let out = EditDocumentTool.run(&ctx, args).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["backupStatus"], "saved");
        let saved = Path::new(out.data["backup"].as_str().unwrap());
        assert_eq!(std::fs::read(saved).unwrap(), before);
        let edited = std::fs::read(&path).unwrap();
        assert_ne!(edited, before);
        let model = crate::tools::compact::compact_for_model(
            ToolKind::FileWrite,
            "edit_document",
            &out,
            false,
        );
        assert!(model.contains("backupStatus"));
        let restored = transaction::restore(dd.path(), &ctx.write_roots(), &path, saved)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let current = Path::new(restored["currentBackup"].as_str().unwrap());
        assert_eq!(std::fs::read(current).unwrap(), edited);
        transaction::restore(dd.path(), &ctx.write_roots(), &path, current)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), edited);
    }
}

#[tokio::test]
async fn document_restore_requires_current_backup_at_every_fault() {
    for point in [
        "source_read",
        "snapshot",
        "backup_dir",
        "backup_open",
        "backup_write",
        "backup_flush",
        "backup_sync",
        "backup_verify",
        "commit",
    ] {
        let (ctx, ws, dd) = setup();
        let (path, _) = sample(ws.path(), "docx");
        let before = std::fs::read(&path).unwrap();
        let selected = backup::save(dd.path(), &path, b"old version").unwrap();
        let _fault = faults::install(&path, point, || Err(std::io::Error::other("restore fault")));
        let error = transaction::restore(dd.path(), &ctx.write_roots(), &path, &selected)
            .await
            .unwrap_err();
        assert!(matches!(
            error.code,
            "E_SOURCE_READ" | "E_BACKUP_FAILED" | "E_IO"
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before, "{point}");
        assert_eq!(std::fs::read(&selected).unwrap(), b"old version");
        assert!(ctx.core.store.load_artifacts(&ctx.rt.id).is_empty());
    }
}

#[tokio::test]
async fn document_external_change_is_rejected_without_losing_backup() {
    for ext in ["xlsx", "xlsm", "docx"] {
        let (ctx, ws, dd) = setup();
        let (path, args) = sample(ws.path(), ext);
        let original = std::fs::read(&path).unwrap();
        let changed = path.clone();
        let _hook = faults::install(&path, "before_compare", move || {
            std::fs::write(changed, b"external change")
        });
        let out = EditDocumentTool.run(&ctx, args).await;
        assert_eq!(out.error.unwrap().code, "E_DOCUMENT_CHANGED");
        assert_eq!(std::fs::read(&path).unwrap(), b"external change");
        assert_eq!(
            std::fs::read(backup::latest_for(dd.path(), &path).unwrap()).unwrap(),
            original
        );
        assert!(ctx.core.store.load_artifacts(&ctx.rt.id).is_empty());
    }
}

#[tokio::test]
async fn document_cancel_before_commit_preserves_original() {
    for ext in ["xlsx", "xlsm", "docx"] {
        let (ctx, ws, dd) = setup();
        let (path, args) = sample(ws.path(), ext);
        let before = std::fs::read(&path).unwrap();
        let cancel = ctx.cancel.clone();
        let _hook = faults::install(&path, "before_compare", move || {
            cancel.cancel();
            Ok(())
        });
        let out = EditDocumentTool.run(&ctx, args).await;
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read(backup::latest_for(dd.path(), &path).unwrap()).unwrap(),
            before
        );
        assert!(ctx.core.store.load_artifacts(&ctx.rt.id).is_empty());
    }
}

#[tokio::test]
async fn document_lock_wait_is_cancellable_without_backup_or_write() {
    for ext in ["xlsx", "docx"] {
        let (ctx, ws, dd) = setup();
        let (path, args) = sample(ws.path(), ext);
        let before = std::fs::read(&path).unwrap();
        let guard = crate::tools::writelock::acquire_all(std::slice::from_ref(&path), None)
            .await
            .unwrap();
        let cancel = ctx.cancel.clone();
        let run = tokio::spawn(async move { EditDocumentTool.run(&ctx, args).await });
        tokio::task::yield_now().await;
        assert!(!run.is_finished());
        cancel.cancel();
        let out = tokio::time::timeout(std::time::Duration::from_secs(1), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(backup::list_for(dd.path(), &path).is_empty());
        drop(guard);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn document_managed_edits_serialize_and_read_latest_snapshot() {
    let (first, ws, dd) = setup();
    let (path, args) = sample(ws.path(), "xlsx");
    let second_rt = first.core.get_or_create_session(
        "second-main",
        ws.path().to_path_buf(),
        None,
        vec![],
        None,
        vec![],
    );
    let second = context(first.core.clone(), second_rt);
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let held = barrier.clone();
    let _hook = faults::install(&path, "before_compare", move || {
        let _ = entered_tx.send(());
        held.wait();
        Ok(())
    });
    let first_run = tokio::spawn(async move { EditDocumentTool.run(&first, args).await });
    entered_rx.await.unwrap();
    let second_args = json!({"path":path,"edits":[{"sheet":"Sheet1","cell":"B1","text":"second"}]});
    let second_run = tokio::spawn(async move { EditDocumentTool.run(&second, second_args).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!second_run.is_finished());
    barrier.wait();
    assert!(first_run.await.unwrap().ok);
    assert!(second_run.await.unwrap().ok);
    let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
    assert_eq!(book.sheet_by_name("Sheet1").unwrap().value("A1"), "updated");
    assert_eq!(book.sheet_by_name("Sheet1").unwrap().value("B1"), "second");
    assert_eq!(backup::list_for(dd.path(), &path).len(), 2);
}

#[tokio::test]
async fn document_create_race_never_overwrites() {
    for ext in ["xlsx", "docx"] {
        let (ctx, ws, _) = setup();
        let path = ws.path().join(format!("new.{ext}"));
        let external = path.clone();
        let _hook = faults::install(&path, "before_create", move || {
            std::fs::write(external, b"external owner")
        });
        let out = WriteDocumentTool.run(&ctx, create_args(&path, ext)).await;
        assert_eq!(out.error.unwrap().code, "E_EXISTS");
        assert_eq!(std::fs::read(path).unwrap(), b"external owner");
        assert!(ctx.core.store.load_artifacts(&ctx.rt.id).is_empty());
    }
}

#[tokio::test]
async fn document_two_main_creates_publish_exactly_once() {
    for ext in ["xlsx", "docx"] {
        let (ctx, ws, _) = setup();
        let other = context(
            ctx.core.clone(),
            ctx.core.get_or_create_session(
                "other",
                ws.path().to_path_buf(),
                None,
                vec![],
                None,
                vec![],
            ),
        );
        let path = ws.path().join(format!("new.{ext}"));
        let (a, b) = tokio::join!(
            WriteDocumentTool.run(&ctx, create_args(&path, ext)),
            WriteDocumentTool.run(&other, create_args(&path, ext))
        );
        assert_ne!(a.ok, b.ok);
        let failed = if a.ok { b } else { a };
        assert_eq!(failed.error.unwrap().code, "E_EXISTS");
        assert!(zip::ZipArchive::new(std::fs::File::open(path).unwrap()).is_ok());
    }
}

#[tokio::test(start_paused = true)]
async fn document_restore_busy_is_bounded_and_independent_of_run_cancel() {
    let (ctx, ws, dd) = setup();
    let path = ws.path().join("a.docx");
    std::fs::write(&path, b"current").unwrap();
    let selected = backup::save(dd.path(), &path, b"previous").unwrap();
    let held = crate::tools::writelock::acquire_all(std::slice::from_ref(&path), None)
        .await
        .unwrap();
    let error = transaction::restore(dd.path(), &ctx.write_roots(), &path, &selected)
        .await
        .unwrap_err();
    assert_eq!(error.code, "E_FILE_BUSY");
    assert_eq!(
        transaction::RESTORE_LOCK_TIMEOUT,
        std::time::Duration::from_secs(2)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"current");
    drop(held);
    ctx.cancel.cancel();
    assert!(
        transaction::restore(dd.path(), &ctx.write_roots(), &path, &selected)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn document_restore_missing_target_never_clobbers_concurrent_creation() {
    let (ctx, ws, dd) = setup();
    let path = ws.path().join("a.docx");
    let selected = backup::save(dd.path(), &path, b"previous").unwrap();
    let out = transaction::restore(dd.path(), &ctx.write_roots(), &path, &selected)
        .await
        .unwrap();
    assert_eq!(out["backupStatus"], "not_needed_missing_target");
    assert!(out["currentBackup"].is_null());
    assert_eq!(std::fs::read(&path).unwrap(), b"previous");
    std::fs::remove_file(&path).unwrap();
    let concurrent = path.clone();
    let _hook = faults::install(&path, "before_create", move || {
        std::fs::write(concurrent, b"concurrent")
    });
    assert_eq!(
        transaction::restore(dd.path(), &ctx.write_roots(), &path, &selected)
            .await
            .unwrap_err()
            .code,
        "E_EXISTS"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"concurrent");
}

#[tokio::test]
async fn document_restore_rejects_other_target_and_pins_oldest_version() {
    let (ctx, ws, dd) = setup();
    let path = ws.path().join("a.docx");
    let other = ws.path().join("b.docx");
    std::fs::write(&path, b"current").unwrap();
    let wrong = backup::save(dd.path(), &other, b"wrong").unwrap();
    assert_eq!(
        transaction::restore(dd.path(), &ctx.write_roots(), &path, &wrong)
            .await
            .unwrap_err()
            .code,
        "E_BACKUP_INVALID"
    );
    let first = backup::save(dd.path(), &path, b"first").unwrap();
    for i in 0..4 {
        backup::save(dd.path(), &path, &[i]).unwrap();
    }
    let out = transaction::restore(dd.path(), &ctx.write_roots(), &path, &first)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    assert_eq!(std::fs::read(&first).unwrap(), b"first");
    assert_eq!(
        std::fs::read(out["currentBackup"].as_str().unwrap()).unwrap(),
        b"current"
    );
    assert_eq!(
        backup::list_for(dd.path(), &path).len(),
        backup::KEEP_PER_FILE
    );
}

#[cfg(unix)]
#[tokio::test]
async fn document_aliases_share_lock_and_retargeting_is_rejected() {
    let (ctx, ws, dd) = setup();
    let (path, _) = sample(ws.path(), "docx");
    let alias = ws.path().join("alias.docx");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let held = crate::tools::writelock::acquire_all(std::slice::from_ref(&path), None)
        .await
        .unwrap();
    let cancel = ctx.cancel.clone();
    let alias_for_run = alias.clone();
    let run = tokio::spawn(async move {
        EditDocumentTool
            .run(
                &ctx,
                json!({"path":alias_for_run,"textEdits":[{"find":"original","replace":"alias"}]}),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!run.is_finished());
    cancel.cancel();
    assert_eq!(run.await.unwrap().error.unwrap().code, "E_CANCELLED");
    drop(held);
    let roots = WriteRoots::new(ws.path().to_path_buf(), dd.path().to_path_buf());
    let target = transaction::LockedTarget::acquire(&roots, &alias, None)
        .await
        .unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let prepared = transaction::prepare_existing(&target, dd.path(), &cancel).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let other = outside.path().join("outside.docx");
    std::fs::write(&other, b"untouched").unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    let error = transaction::commit_prepared(&target, prepared, dd.path(), b"bad", &cancel, &[])
        .err()
        .unwrap();
    assert_eq!(error.code, "E_PATH_OUTSIDE");
    assert_eq!(std::fs::read(other).unwrap(), b"untouched");
}

#[tokio::test]
async fn document_claim_policy_matches_runtime_role_for_all_write_tools() {
    use crate::core::agent::SessionRuntime;
    use crate::tools::claims;
    for role in ["main", "sub", "nested", "task"] {
        let (parent, ws, dd) = setup();
        let sub = SessionRuntime::new_sub(&parent.rt, uuid::Uuid::new_v4().to_string());
        let rt = match role {
            "main" => parent.rt.clone(),
            "sub" => sub.clone(),
            "nested" => SessionRuntime::new_sub(&sub, uuid::Uuid::new_v4().to_string()),
            _ => SessionRuntime::new_task(
                uuid::Uuid::new_v4().to_string(),
                dd.path().to_path_buf(),
                ws.path().to_path_buf(),
                dd.path().to_path_buf(),
            ),
        };
        assert_eq!(claims::requires_file_claim(&rt), role != "main");
        if role == "task" {
            assert!(rt.root_session_id.is_none());
        }
        let ctx = context(parent.core.clone(), rt);
        let owner = uuid::Uuid::new_v4().to_string();
        let _guard = claims::ReleaseGuard::arm(&owner);
        let _runtime_guard = claims::ReleaseGuard::arm(&ctx.rt.id);
        for ext in ["xlsx", "xlsm", "docx"] {
            let (path, args) = sample(ws.path(), ext);
            claims::claim(&owner, std::slice::from_ref(&path)).unwrap();
            let out = EditDocumentTool.run(&ctx, args).await;
            assert_eq!(out.ok, role == "main", "{role}/{ext}: {out:?}");
            if !out.ok {
                assert_eq!(out.error.unwrap().code, "E_FILE_CLAIMED");
            }
        }
        for ext in ["xlsx", "docx"] {
            let path = ws.path().join(format!("create.{ext}"));
            claims::claim(&owner, std::slice::from_ref(&path)).unwrap();
            let out = WriteDocumentTool.run(&ctx, create_args(&path, ext)).await;
            assert_eq!(out.ok, role == "main", "{role}/{ext}: {out:?}");
            if !out.ok {
                assert_eq!(out.error.unwrap().code, "E_FILE_CLAIMED");
            }
        }
        let path = ws.path().join("ordinary.txt");
        claims::claim(&owner, std::slice::from_ref(&path)).unwrap();
        let out = crate::tools::create::CreateTool
            .run(&ctx, json!({"path":path,"content":"original"}))
            .await;
        assert_eq!(out.ok, role == "main", "{role}: {out:?}");
        if !out.ok {
            assert_eq!(out.error.unwrap().code, "E_FILE_CLAIMED");
        }
        std::fs::write(&path, "original").unwrap();
        let out = crate::tools::edit::EditTool.run(&ctx, json!({"files":[{"path":path,"changes":[{"oldText":"original","newText":"changed"}]}]})).await;
        assert_eq!(out.ok, role == "main", "{role}: {out:?}");
        if !out.ok {
            assert_eq!(out.error.unwrap().code, "E_FILE_CLAIMED");
        }
        let out = crate::tools::delete::DeleteTool
            .run(&ctx, json!({"path":path}))
            .await;
        assert_eq!(out.ok, role == "main", "{role}: {out:?}");
        if !out.ok {
            assert_eq!(out.error.unwrap().code, "E_FILE_CLAIMED");
        }
    }
}

#[tokio::test]
async fn document_claim_lasts_across_calls_but_main_never_leaks_claims() {
    use crate::tools::claims;
    let (ctx, ws, dd) = setup();
    let (path, args) = sample(ws.path(), "xlsx");
    assert!(EditDocumentTool.run(&ctx, args.clone()).await.ok);
    // A main-session edit must not leave a semantic claim behind.
    let probe = uuid::Uuid::new_v4().to_string();
    let probe_guard = claims::ReleaseGuard::arm(&probe);
    claims::claim(&probe, std::slice::from_ref(&path)).unwrap();
    drop(probe_guard);
    let sub =
        crate::core::agent::SessionRuntime::new_sub(&ctx.rt, uuid::Uuid::new_v4().to_string());
    let guard = claims::ReleaseGuard::arm(&sub.id);
    let sub_ctx = context(ctx.core.clone(), sub);
    assert!(EditDocumentTool.run(&sub_ctx, args.clone()).await.ok);
    assert!(EditDocumentTool.run(&sub_ctx, args).await.ok);
    assert!(claims::claim(&probe, std::slice::from_ref(&path)).is_err());
    let task = crate::core::agent::SessionRuntime::new_task(
        uuid::Uuid::new_v4().to_string(),
        dd.path().to_path_buf(),
        ws.path().to_path_buf(),
        dd.path().to_path_buf(),
    );
    let task_ctx = context(ctx.core.clone(), task);
    let out = crate::tools::delete::DeleteTool
        .run(&task_ctx, json!({"path":ws.path(),"recursive":true}))
        .await;
    // Root deletion is independently forbidden before claiming; an ordinary parent
    // directory claim directly verifies the ancestor isolation invariant instead.
    assert!(!out.ok);
    assert!(claims::claim(&probe, &[ws.path().to_path_buf()]).is_err());
    drop(guard);
    claims::claim(&probe, std::slice::from_ref(&path)).unwrap();
    claims::release(&probe);
}

#[tokio::test]
async fn document_failed_backup_does_not_prune_old_versions() {
    let (ctx, ws, dd) = setup();
    let (path, args) = sample(ws.path(), "xlsx");
    let old: Vec<_> = (0..6)
        .map(|i| backup::save(dd.path(), &path, &[i]).unwrap())
        .collect();
    let _fault = faults::install(&path, "backup_sync", || {
        Err(std::io::Error::other("sync failure"))
    });
    assert!(!EditDocumentTool.run(&ctx, args.clone()).await.ok);
    assert_eq!(backup::list_for(dd.path(), &path), old);
    let out = EditDocumentTool.run(&ctx, args).await;
    assert!(out.ok);
    assert_eq!(backup::list_for(dd.path(), &path).len(), 5);
    assert!(Path::new(out.data["backup"].as_str().unwrap()).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn document_restore_rejects_symlink_backup_even_inside_backup_directory() {
    let (ctx, ws, dd) = setup();
    let path = ws.path().join("a.docx");
    std::fs::write(&path, b"current").unwrap();
    let selected = backup::save(dd.path(), &path, b"old").unwrap();
    let outside = ws.path().join("outside.bak");
    std::fs::write(&outside, b"outside").unwrap();
    std::fs::remove_file(&selected).unwrap();
    std::os::unix::fs::symlink(outside, &selected).unwrap();
    assert_eq!(
        transaction::restore(dd.path(), &ctx.write_roots(), &path, &selected)
            .await
            .unwrap_err()
            .code,
        "E_BACKUP_INVALID"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"current");
}
