use super::tests::{node_cfg, wait_ready};
use super::*;
use crate::core::agent::{AgentCore, test_support::make_core};
use crate::core::trust::{TrustBackend, TrustError, TrustStore};
use crate::mcp::{
    confirm_launch_review, global_scope, launch_key, launch_operation, launch_version, load_scope,
    prepare_launch_review, save_configuration, scope_version, set_launch_trust,
};
use crate::tools::pathutil::WriteRoots;
use serde_json::json;
use std::sync::Mutex as StdMutex;

#[derive(Default)]
struct Backend {
    entries: StdMutex<HashMap<String, String>>,
    unavailable: AtomicBool,
    unconfirmed_write: AtomicBool,
    pause_write: StdMutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: StdMutex<bool>,
    wake: std::sync::Condvar,
}
impl Backend {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
impl TrustBackend for Backend {
    fn read(&self, key: &str) -> Result<Option<String>, TrustError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(TrustError::Unavailable);
        }
        Ok(self.entries.lock().unwrap().get(key).cloned())
    }
    fn write(&self, key: &str, value: &str) -> Result<(), TrustError> {
        let pause = self.pause_write.lock().unwrap().take();
        if let Some(entered) = pause {
            let _ = entered.send(());
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
        self.entries
            .lock()
            .unwrap()
            .insert(key.into(), value.into());
        if self.unconfirmed_write.swap(false, Ordering::SeqCst) {
            self.unavailable.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}
struct Release(Arc<Backend>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn core_at(dir: &std::path::Path, backend: Arc<Backend>) -> Arc<AgentCore> {
    let mut core = make_core(&WriteRoots {
        workspace: dir.into(),
        data_dir: dir.into(),
        project_dir: None,
        extra: vec![],
    });
    Arc::get_mut(&mut core).unwrap().mcp = Arc::new(McpManager::with_authority(
        16,
        Arc::new(TrustStore::new(dir.into(), backend)),
    ));
    core
}
fn save_fixture(dir: &std::path::Path, cfg: &McpServerConfig) {
    std::fs::write(
        global_scope(dir).path,
        json!({"mcpServers":{"fixture":cfg}}).to_string(),
    )
    .unwrap();
}
async fn approve(core: &AgentCore, action: ReviewAction) -> LaunchReviewReceipt {
    let source = global_scope(&core.data_dir);
    let cfg = load_scope(&source).servers.remove(0).1;
    let review = prepare_launch_review(
        core,
        &source,
        None,
        "fixture",
        &launch_version(&cfg),
        action,
    )
    .await
    .unwrap();
    confirm_launch_review(core, &source, None, "fixture", &review.id)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remembered_launch_restores_real_server_rebinds_policy_and_cross_instance_revoke_blocks_dispatch()
 {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let mut cfg = node_cfg();
    let call_log = dir.path().join("calls.log");
    cfg.env.insert(
        "CODEWAVE_TEST_CALL_LOG".into(),
        call_log.display().to_string(),
    );
    save_fixture(dir.path(), &cfg);
    let a = core_at(dir.path(), backend.clone());
    assert!(
        approve(&a, ReviewAction::Remember)
            .await
            .status
            .unwrap()
            .remembered
    );
    let b = core_at(dir.path(), backend);
    let source = global_scope(dir.path());
    let key = launch_key(&b, &source, None, "fixture").unwrap();
    b.mcp
        .warm(
            "s",
            vec![(key.clone(), cfg.clone())],
            reqwest::Client::new(),
        )
        .await;
    wait_ready(&b.mcp, "s", 1).await;
    let tools = b.mcp.tool_snapshot_for("s").await;
    let echo = tools
        .iter()
        .find(|tool| tool.function_name.ends_with("__echo"))
        .unwrap();
    let binding = b
        .mcp
        .request_binding("s", &echo.function_name)
        .await
        .unwrap();
    let updated = b.mcp.allow_binding("s", &binding).await.unwrap();
    assert!(updated.cfg.always_allow);
    assert!(b.mcp.launch_status(&key, &updated.cfg).remembered);
    let output = b
        .mcp
        .call_bound(
            "s",
            &updated,
            json!({"text":"persistent"}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output.text, "echo: persistent");
    assert!(approve(&a, ReviewAction::Revoke).await.confirmed);
    assert!(
        b.mcp
            .call_bound(
                "s",
                &updated,
                json!({"text":"must-not-run"}),
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    b.mcp.revalidate_all_authority().await;
    assert_eq!(
        std::fs::read_to_string(&call_log).unwrap().lines().count(),
        1
    );
    assert!(
        !std::fs::read_to_string(&call_log)
            .unwrap()
            .contains("must-not-run")
    );
    assert!(b.mcp.tool_snapshot_for("s").await.is_empty());
    assert!(
        b.mcp
            .ensure(&key, updated.cfg.clone(), reqwest::Client::new(), "s")
            .await
            .is_err()
    );
    assert_eq!(b.mcp.status_for("s").await[0].state, McpState::Stopped);
    // The compatibility revoke must not be resurrected by reading settings.
    approve(&a, ReviewAction::Remember).await;
    set_launch_trust(
        &a,
        &source,
        None,
        "fixture",
        &launch_version(&updated.cfg),
        false,
    )
    .await
    .unwrap();
    assert!(
        !a.mcp
            .current_launch_status(&key, &updated.cfg)
            .await
            .trusted
    );
    b.mcp.disconnect_all().await;
}

#[tokio::test]
async fn saved_change_and_restore_do_not_resurrect_an_old_remembered_version() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let cfg = node_cfg();
    save_fixture(dir.path(), &cfg);
    let core = core_at(dir.path(), backend);
    approve(&core, ReviewAction::Remember).await;
    let source = global_scope(dir.path());
    let original = std::fs::read_to_string(&source.path).unwrap();
    let changed = json!({"mcpServers":{"fixture":{"command":"changed"}}}).to_string();
    save_configuration(&core, &source, &changed, &scope_version(&original))
        .await
        .unwrap();
    save_configuration(&core, &source, &original, &scope_version(&changed))
        .await
        .unwrap();
    let key = launch_key(&core, &source, None, "fixture").unwrap();
    assert!(!core.mcp.current_launch_status(&key, &cfg).await.trusted);
    let prepared = prepare_launch_review(
        &core,
        &source,
        None,
        "fixture",
        &launch_version(&cfg),
        ReviewAction::Remember,
    )
    .await
    .unwrap();
    std::fs::write(&source.path, changed).unwrap();
    assert!(
        confirm_launch_review(&core, &source, None, "fixture", &prepared.id)
            .await
            .is_err()
    );
    std::fs::write(&source.path, original).unwrap();
    assert!(
        confirm_launch_review(&core, &source, None, "fixture", &prepared.id)
            .await
            .is_err(),
        "mismatched confirmation must stay consumed"
    );
}

#[tokio::test]
async fn unavailable_persistent_authority_does_not_create_a_once_grant_without_new_user_review() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let cfg = node_cfg();
    save_fixture(dir.path(), &cfg);
    let core = core_at(dir.path(), backend.clone());
    approve(&core, ReviewAction::Remember).await;
    let source = global_scope(dir.path());
    let key = launch_key(&core, &source, None, "fixture").unwrap();
    backend.unavailable.store(true, Ordering::SeqCst);
    assert!(!core.mcp.current_launch_status(&key, &cfg).await.trusted);
    assert!(
        prepare_launch_review(
            &core,
            &source,
            None,
            "fixture",
            &launch_version(&cfg),
            ReviewAction::Remember
        )
        .await
        .is_err()
    );
    let once = approve(&core, ReviewAction::Once).await;
    assert!(once.status.as_ref().unwrap().trusted && !once.status.unwrap().remembered);
    let restarted = core_at(dir.path(), backend);
    assert!(
        !restarted
            .mcp
            .current_launch_status(&key, &cfg)
            .await
            .trusted
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_commit_cancels_a_once_review_accepted_while_native_write_waits() {
    for unknown in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(Backend::default());
        let _release = Release(backend.clone());
        let cfg = node_cfg();
        save_fixture(dir.path(), &cfg);
        let core = core_at(dir.path(), backend.clone());
        approve(&core, ReviewAction::Remember).await;
        let source = global_scope(dir.path());
        let review = prepare_launch_review(
            &core,
            &source,
            None,
            "fixture",
            &launch_version(&cfg),
            ReviewAction::Revoke,
        )
        .await
        .unwrap();
        let operation_id = review.id.clone();
        backend.unconfirmed_write.store(unknown, Ordering::SeqCst);
        let (entered, started) = tokio::sync::oneshot::channel();
        *backend.pause_write.lock().unwrap() = Some(entered);
        let task = tokio::spawn({
            let core = core.clone();
            let source = source.clone();
            async move { confirm_launch_review(&core, &source, None, "fixture", &review.id).await }
        });
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        let once = tokio::time::timeout(Duration::from_secs(1), approve(&core, ReviewAction::Once))
            .await
            .unwrap();
        assert!(once.status.unwrap().trusted);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(200),
                super::super::CONFIG_COMMIT.lock()
            )
            .await
            .is_ok(),
            "keyring I/O held the config mutex"
        );
        backend.release();
        let receipt = task.await.unwrap().unwrap();
        if unknown {
            assert!(!receipt.confirmed && !receipt.resolved);
            backend.unavailable.store(false, Ordering::SeqCst);
            let receipt = launch_operation(&core, &source, None, "fixture", &operation_id)
                .await
                .unwrap();
            assert!(receipt.confirmed && receipt.resolved);
            assert!(!receipt.status.unwrap().trusted);
            // Re-reading the resolved receipt must not revoke a later explicit Once.
            assert!(
                approve(&core, ReviewAction::Once)
                    .await
                    .status
                    .unwrap()
                    .trusted
            );
            assert!(
                launch_operation(&core, &source, None, "fixture", &operation_id)
                    .await
                    .unwrap()
                    .status
                    .unwrap()
                    .trusted
            );
            approve(&core, ReviewAction::Revoke).await;
        } else {
            assert!(receipt.confirmed && receipt.resolved);
        }
        let key = launch_key(&core, &source, None, "fixture").unwrap();
        assert!(!core.mcp.current_launch_status(&key, &cfg).await.trusted);
    }
}

#[tokio::test]
async fn revoke_invalidates_prepared_once_and_remember_reviews() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let cfg = node_cfg();
    save_fixture(dir.path(), &cfg);
    let core = core_at(dir.path(), backend);
    let source = global_scope(dir.path());
    let mut tickets = Vec::new();
    for action in [ReviewAction::Once, ReviewAction::Remember] {
        tickets.push(
            prepare_launch_review(
                &core,
                &source,
                None,
                "fixture",
                &launch_version(&cfg),
                action,
            )
            .await
            .unwrap(),
        );
    }
    assert!(approve(&core, ReviewAction::Revoke).await.confirmed);
    for ticket in tickets {
        assert!(
            confirm_launch_review(&core, &source, None, "fixture", &ticket.id)
                .await
                .is_err()
        );
    }
    let key = launch_key(&core, &source, None, "fixture").unwrap();
    assert!(!core.mcp.current_launch_status(&key, &cfg).await.trusted);
}

#[tokio::test]
async fn unknown_commit_has_a_read_only_receipt_and_cannot_be_confirmed_twice() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let cfg = node_cfg();
    save_fixture(dir.path(), &cfg);
    let core = core_at(dir.path(), backend.clone());
    let source = global_scope(dir.path());
    let review = prepare_launch_review(
        &core,
        &source,
        None,
        "fixture",
        &launch_version(&cfg),
        ReviewAction::Remember,
    )
    .await
    .unwrap();
    backend.unconfirmed_write.store(true, Ordering::SeqCst);
    let unknown = confirm_launch_review(&core, &source, None, "fixture", &review.id)
        .await
        .unwrap();
    assert!(!unknown.confirmed && !unknown.resolved);
    assert!(
        confirm_launch_review(&core, &source, None, "fixture", &review.id)
            .await
            .is_err()
    );
    backend.unavailable.store(false, Ordering::SeqCst);
    let receipt = launch_operation(&core, &source, None, "fixture", &review.id)
        .await
        .unwrap();
    assert!(receipt.confirmed && receipt.resolved && receipt.status.unwrap().remembered);
    let values: Vec<serde_json::Value> = backend
        .entries
        .lock()
        .unwrap()
        .values()
        .map(|value| serde_json::from_str(value).unwrap())
        .collect();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["revision"], 1);
    assert!(
        core.mcp.all_status().await.is_empty(),
        "receipt inspection launched a process without a visible session"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_retirement_preserves_a_new_configuration_approved_before_reconcile() {
    for background in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cfg = node_cfg();
        save_fixture(dir.path(), &cfg);
        let core = core_at(dir.path(), Arc::new(Backend::default()));
        approve(&core, ReviewAction::Remember).await;
        let key = launch_key(&core, &global_scope(dir.path()), None, "fixture").unwrap();
        let permit = core.mcp.launch.connection(&key, &cfg).unwrap();
        let ready = McpManager::spawn_connect(&key, &cfg, reqwest::Client::new(), &permit)
            .await
            .unwrap();
        {
            let mut inner = core.mcp.inner.lock().await;
            let mut entry = blank_entry(&cfg, 0);
            entry.generation = 1;
            entry.launch = Some(permit);
            entry.sessions.insert("s".into());
            inner.entries.insert(key.clone(), entry);
            inner
                .sessions
                .insert("s".into(), HashSet::from([key.clone()]));
        }
        let mut next = cfg;
        next.env.insert("NEW_REVIEW".into(), "1".into());
        save_fixture(dir.path(), &next);
        assert!(
            approve(&core, ReviewAction::Once)
                .await
                .status
                .unwrap()
                .trusted
        );
        if background {
            core.mcp.revalidate_authority("s").await;
        }
        assert!(!core.mcp.set_ready(&key, ready, "s", 1).await);
        assert!(core.mcp.current_launch_status(&key, &next).await.trusted);
        core.mcp.disconnect_all().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_during_real_handshake_cannot_publish_a_late_ready_server() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let mut cfg = node_cfg();
    let pid = dir.path().join("started.pid");
    cfg.env
        .insert("CODEWAVE_TEST_PID_FILE".into(), pid.display().to_string());
    cfg.env
        .insert("CODEWAVE_TEST_INIT_DELAY_MS".into(), "1000".into());
    save_fixture(dir.path(), &cfg);
    let a = core_at(dir.path(), backend.clone());
    approve(&a, ReviewAction::Remember).await;
    let b = core_at(dir.path(), backend);
    let key = launch_key(&b, &global_scope(dir.path()), None, "fixture").unwrap();
    let pending_once = prepare_launch_review(
        &b,
        &global_scope(dir.path()),
        None,
        "fixture",
        &launch_version(&cfg),
        ReviewAction::Once,
    )
    .await
    .unwrap();
    b.mcp
        .register_session("s", &[(key.clone(), cfg.clone())])
        .await;
    let task = tokio::spawn({
        let manager = b.mcp.clone();
        let key = key.clone();
        async move { manager.ensure(&key, cfg, reqwest::Client::new(), "s").await }
    });
    tokio::time::timeout(Duration::from_secs(4), async {
        while !pid.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(b.mcp.status_for("s").await[0].state, McpState::Starting);
    approve(&a, ReviewAction::Revoke).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(4), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(b.mcp.status_for("s").await[0].state, McpState::Stopped);
    assert!(
        confirm_launch_review(
            &b,
            &global_scope(dir.path()),
            None,
            "fixture",
            &pending_once.id
        )
        .await
        .is_err()
    );
    assert!(b.mcp.tool_snapshot_for("s").await.is_empty());
    b.mcp.disconnect_all().await;
}
