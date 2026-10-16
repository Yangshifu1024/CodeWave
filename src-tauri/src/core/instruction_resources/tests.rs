use super::review::{self, Action};
use super::*;
use crate::core::agent::{AgentCore, SessionRuntime, test_support};
use crate::core::trust::{TrustBackend, TrustError};
use crate::tools::{Tool, ToolCtx};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct Backend {
    entries: Mutex<HashMap<String, String>>,
    unavailable: AtomicBool,
    unknown: AtomicBool,
}
impl TrustBackend for Backend {
    fn read(&self, key: &str) -> Result<Option<String>, TrustError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(TrustError::Unavailable);
        }
        Ok(self.entries.lock().unwrap().get(key).cloned())
    }
    fn write(&self, key: &str, value: &str) -> Result<(), TrustError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.into(), value.into());
        if self.unknown.swap(false, Ordering::SeqCst) {
            self.unavailable.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

fn core_at(
    workspace: &Path,
    data: &Path,
    backend: Arc<Backend>,
) -> (Arc<AgentCore>, Arc<SessionRuntime>) {
    let mut core = test_support::make_core(&crate::tools::pathutil::WriteRoots {
        workspace: workspace.into(),
        data_dir: data.into(),
        project_dir: None,
        extra: vec![],
    });
    Arc::get_mut(&mut core).unwrap().instructions =
        InstructionTrust::with_store(Arc::new(TrustStore::new(data.into(), backend)));
    let rt = core.get_or_create_session(
        "resource-session",
        workspace.into(),
        None,
        vec![],
        None,
        vec![],
    );
    (core, rt)
}

fn fixtures(workspace: &Path, data: &Path) {
    std::fs::create_dir_all(workspace.join(".codewave")).unwrap();
    for (path, text) in [
        ("AGENTS.md", "INSTRUCTION_MARKER"),
        ("CODEGRAPH.md", "GRAPH_MARKER"),
        (".codewave/lessons.md", "LESSON_MARKER"),
    ] {
        std::fs::write(workspace.join(path), text).unwrap();
    }
    std::fs::create_dir_all(data.join("memories")).unwrap();
    std::fs::write(
        data.join("memories/example.md"),
        "---\nname: example\ndescription: MEMORY_MARKER\n---\nMemory body",
    )
    .unwrap();
    std::fs::create_dir_all(data.join("skills/demo")).unwrap();
    std::fs::write(
        data.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: SKILL_INDEX_MARKER\n---\nSKILL_BODY_MARKER",
    )
    .unwrap();
}

async fn approve(
    core: &AgentCore,
    rt: &SessionRuntime,
    name: &str,
    action: Action,
) -> review::Receipt {
    let scope = ResourceScope::runtime(rt);
    let resource = collect(core, &scope)
        .resources
        .into_iter()
        .find(|resource| resource.name == name)
        .unwrap();
    let ticket = review::prepare(
        core,
        Some(&rt.id),
        &resource.id,
        &resource.origin.version,
        action,
    )
    .await
    .unwrap();
    if resource.kind == InstructionKind::Skill {
        assert!(
            ticket.text.contains("SKILL_INDEX_MARKER") || ticket.text.contains("changed"),
            "skill metadata omitted from the reviewed projection"
        );
    }
    review::confirm(core, Some(&rt.id), &ticket.id)
        .await
        .unwrap()
}

fn ctx(core: Arc<AgentCore>, rt: Arc<SessionRuntime>) -> ToolCtx {
    ToolCtx {
        core,
        rt,
        batch_id: "b".into(),
        call_index: 0,
        call_key: "b:0".into(),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

#[tokio::test]
async fn untrusted_resources_are_absent_from_the_real_request_and_skill_dispatch() {
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fixtures(ws.path(), data.path());
    let (core, rt) = core_at(ws.path(), data.path(), Arc::new(Backend::default()));
    let request = test_support::request(&core, &rt).await;
    for marker in [
        "INSTRUCTION_MARKER",
        "GRAPH_MARKER",
        "LESSON_MARKER",
        "MEMORY_MARKER",
        "SKILL_INDEX_MARKER",
    ] {
        assert!(!request.system_core.contains(marker));
    }
    let output = crate::tools::skill::SkillTool
        .run(
            &ctx(core.clone(), rt.clone()),
            serde_json::json!({"skill":"demo"}),
        )
        .await;
    assert_eq!(output.error.unwrap().code, "E_RESOURCE_UNTRUSTED");
    assert!(core.instructions.local.lock().unwrap().is_empty());
    let output = crate::tools::read::ReadTool
        .run(
            &ctx(core.clone(), rt),
            serde_json::json!({"files":[{"path":ws.path().join("AGENTS.md").display().to_string()}]}),
        )
        .await;
    assert!(output.ok, "{output:?}");
    assert!(
        core.instructions.local.lock().unwrap().is_empty(),
        "ordinary read granted instruction authority"
    );
}

#[tokio::test]
async fn session_approval_freezes_exact_wire_bytes_and_estimates_the_same_prefix() {
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fixtures(ws.path(), data.path());
    let (core, rt) = core_at(ws.path(), data.path(), Arc::new(Backend::default()));
    for name in ["AGENTS.md", "code-graph", "lessons", "example", "demo"] {
        assert!(approve(&core, &rt, name, Action::Once).await.confirmed);
    }
    rt.running.store(true, Ordering::SeqCst);
    let before = test_support::request(&core, &rt).await;
    for marker in [
        "INSTRUCTION_MARKER",
        "GRAPH_MARKER",
        "LESSON_MARKER",
        "MEMORY_MARKER",
        "SKILL_INDEX_MARKER",
    ] {
        assert!(before.system_core.contains(marker));
    }
    std::fs::write(ws.path().join("AGENTS.md"), "UNREVIEWED_REPLACEMENT").unwrap();
    assert_eq!(
        test_support::request(&core, &rt).await.system_core,
        before.system_core
    );
    let estimate = crate::core::context::breakdown(&core, &rt).await;
    assert_eq!(
        estimate.system_tokens,
        crate::util::token_est::est_tokens_text(&before.system_core)
    );
    assert!(
        crate::tools::skill::SkillTool
            .run(
                &ctx(core.clone(), rt.clone()),
                serde_json::json!({"skill":"demo"})
            )
            .await
            .ok
    );
    assert!(approve(&core, &rt, "demo", Action::Revoke).await.confirmed);
    assert!(
        !crate::tools::skill::SkillTool
            .run(
                &ctx(core.clone(), rt.clone()),
                serde_json::json!({"skill":"demo"})
            )
            .await
            .ok
    );
    assert_eq!(
        test_support::request(&core, &rt).await.system_core,
        before.system_core,
        "revocation rewrote already-admitted history/prefix"
    );
    let second = core.get_or_create_session("second", ws.path().into(), None, vec![], None, vec![]);
    approve(&core, &second, "AGENTS.md", Action::Once).await;
    second.running.store(true, Ordering::SeqCst);
    assert!(
        test_support::request(&core, &second)
            .await
            .system_core
            .contains("UNREVIEWED_REPLACEMENT")
    );
    std::fs::remove_file(ws.path().join("AGENTS.md")).unwrap();
    let inspection = review::inspect(&core, Some(&rt.id)).await.unwrap();
    let removed = inspection
        .resources
        .iter()
        .find(|resource| resource.name == "AGENTS.md")
        .unwrap();
    assert!(!removed.available);
    assert_eq!(removed.active_versions.len(), 2);
    assert!(
        removed
            .active_versions
            .iter()
            .any(|usage| usage.version == crate::resources::content_version(b"INSTRUCTION_MARKER"))
    );
    rt.system_frozen.lock().unwrap().take();
    let next = test_support::request(&core, &rt).await;
    assert!(
        !next.system_core.contains("UNREVIEWED_REPLACEMENT")
            && !next.system_core.contains("SKILL_INDEX_MARKER")
    );
    std::fs::write(ws.path().join("AGENTS.md"), "REVIEWED_REPLACEMENT").unwrap();
    approve(&core, &rt, "AGENTS.md", Action::Once).await;
    rt.system_frozen.lock().unwrap().take();
    assert!(
        test_support::request(&core, &rt)
            .await
            .system_core
            .contains("REVIEWED_REPLACEMENT")
    );
}

#[tokio::test]
async fn remembered_versions_restore_but_do_not_cross_projects_and_revoke_blocks_new_admission() {
    let ws = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fixtures(ws.path(), data.path());
    let backend = Arc::new(Backend::default());
    let (a, rt) = core_at(ws.path(), data.path(), backend.clone());
    approve(&a, &rt, "AGENTS.md", Action::Once).await;
    approve(&a, &rt, "demo", Action::Remember).await;
    let (b, brt) = core_at(ws.path(), data.path(), backend.clone());
    let text = test_support::request(&b, &brt).await.system_core;
    assert!(!text.contains("INSTRUCTION_MARKER") && text.contains("SKILL_INDEX_MARKER"));
    let (c, crt) = core_at(other.path(), data.path(), backend);
    assert!(
        !test_support::request(&c, &crt)
            .await
            .system_core
            .contains("SKILL_INDEX_MARKER")
    );
    approve(&a, &rt, "demo", Action::Revoke).await;
    assert!(
        !crate::tools::skill::SkillTool
            .run(&ctx(b, brt), serde_json::json!({"skill":"demo"}))
            .await
            .ok
    );
    let child = SessionRuntime::new_sub(&rt, "child".into());
    let cfg = a.cfg.read().unwrap().clone();
    assert!(
        system(&a, &child, &cfg)
            .await
            .0
            .contains("INSTRUCTION_MARKER")
    );
    let task = SessionRuntime::new_task(
        "task".into(),
        data.path().into(),
        ws.path().into(),
        data.path().into(),
    );
    assert!(
        !system(&a, &task, &cfg)
            .await
            .0
            .contains("INSTRUCTION_MARKER")
    );
}

#[tokio::test]
async fn changed_consumed_and_revoked_tickets_cannot_grant_a_resource() {
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fixtures(ws.path(), data.path());
    let (core, rt) = core_at(ws.path(), data.path(), Arc::new(Backend::default()));
    let resource = collect(&core, &ResourceScope::runtime(&rt))
        .resources
        .into_iter()
        .find(|resource| resource.name == "AGENTS.md")
        .unwrap();
    let ticket = review::prepare(
        &core,
        Some(&rt.id),
        &resource.id,
        &resource.origin.version,
        Action::Once,
    )
    .await
    .unwrap();
    std::fs::write(ws.path().join("AGENTS.md"), "changed").unwrap();
    assert!(
        review::confirm(&core, Some(&rt.id), &ticket.id)
            .await
            .is_err()
    );
    std::fs::write(ws.path().join("AGENTS.md"), "INSTRUCTION_MARKER").unwrap();
    assert!(
        review::confirm(&core, Some(&rt.id), &ticket.id)
            .await
            .is_err()
    );
    let ticket = review::prepare(
        &core,
        Some(&rt.id),
        &resource.id,
        &resource.origin.version,
        Action::Once,
    )
    .await
    .unwrap();
    approve(&core, &rt, "AGENTS.md", Action::Revoke).await;
    assert!(
        review::confirm(&core, Some(&rt.id), &ticket.id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn source_override_and_disabled_changes_cannot_use_the_catalog_ttl_to_bypass_review() {
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fixtures(ws.path(), data.path());
    let (core, rt) = core_at(ws.path(), data.path(), Arc::new(Backend::default()));
    approve(&core, &rt, "demo", Action::Once).await;
    let tool = ctx(core.clone(), rt.clone());
    assert!(
        crate::tools::skill::SkillTool
            .run(&tool, serde_json::json!({"skill":"demo"}))
            .await
            .ok
    );
    std::fs::write(
        data.path().join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: changed\n---\nNEW_BODY",
    )
    .unwrap();
    assert!(
        !crate::tools::skill::SkillTool
            .run(&tool, serde_json::json!({"skill":"demo"}))
            .await
            .ok
    );
    approve(&core, &rt, "demo", Action::Once).await;
    core.cfg
        .write()
        .unwrap()
        .disabled_skills
        .push("demo".into());
    assert_eq!(
        crate::tools::skill::SkillTool
            .run(&tool, serde_json::json!({"skill":"demo"}))
            .await
            .error
            .unwrap()
            .code,
        "E_SKILL_NOT_FOUND"
    );
}

#[tokio::test]
async fn unknown_native_commit_is_inspected_without_rewriting_and_unavailable_never_grants() {
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fixtures(ws.path(), data.path());
    let backend = Arc::new(Backend::default());
    let (core, rt) = core_at(ws.path(), data.path(), backend.clone());
    backend.unknown.store(true, Ordering::SeqCst);
    let unknown = approve(&core, &rt, "demo", Action::Remember).await;
    assert!(!unknown.confirmed && !unknown.resolved);
    assert!(
        !crate::tools::skill::SkillTool
            .run(
                &ctx(core.clone(), rt.clone()),
                serde_json::json!({"skill":"demo"})
            )
            .await
            .ok
    );
    backend.unavailable.store(false, Ordering::SeqCst);
    assert!(
        review::operation(&core, Some(&rt.id), &unknown.operation_id)
            .await
            .unwrap()
            .confirmed
    );
    assert!(
        review::confirm(&core, Some(&rt.id), &unknown.operation_id)
            .await
            .is_err()
    );
    let records: Vec<serde_json::Value> = backend
        .entries
        .lock()
        .unwrap()
        .values()
        .map(|value| serde_json::from_str(value).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["revision"], 1);
}

#[test]
fn project_projection_has_character_limits_and_rejects_oversized_sources_without_a_fake_hash() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("AGENTS.md"), "汉".repeat(25_000)).unwrap();
    let scope = ResourceScope {
        workspace: ws.path().into(),
        data_dir: ws.path().into(),
        project_dir: None,
        project_id: None,
        owner: "scope".into(),
    };
    let snapshot = collect_project(&scope);
    assert_eq!(snapshot.resources[0].text.chars().count(), 24_000);
    assert_eq!(
        snapshot.resources[0].origin.version,
        crate::resources::content_version("汉".repeat(25_000).as_bytes())
    );
    std::fs::write(ws.path().join("AGENTS.md"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let snapshot = collect_project(&scope);
    assert!(snapshot.resources.is_empty());
    assert_eq!(snapshot.issues.len(), 1);
}

#[cfg(unix)]
#[test]
fn non_regular_instruction_sources_are_rejected_before_blocking_open() {
    let ws = tempfile::tempdir().unwrap();
    let path = ws.path().join("AGENTS.md");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let start = std::time::Instant::now();
    assert!(crate::resources::read_bytes(&path).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    let scope = ResourceScope {
        workspace: ws.path().into(),
        data_dir: ws.path().into(),
        project_dir: None,
        project_id: None,
        owner: "scope".into(),
    };
    let snapshot = collect_project(&scope);
    assert!(snapshot.resources.is_empty());
    assert_eq!(snapshot.issues.len(), 1);
}

#[tokio::test]
async fn cancellation_and_snapshot_deadline_do_not_wait_once_per_native_resource() {
    struct Blocking {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        released: Mutex<bool>,
        wake: std::sync::Condvar,
        reads: std::sync::atomic::AtomicUsize,
    }
    impl TrustBackend for Blocking {
        fn read(&self, _: &str) -> Result<Option<String>, TrustError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
            Ok(None)
        }
        fn write(&self, _: &str, _: &str) -> Result<(), TrustError> {
            unreachable!()
        }
    }
    struct Release(Arc<Blocking>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.released.lock().unwrap() = true;
            self.0.wake.notify_all();
        }
    }
    for cancel_now in [true, false] {
        let ws = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        fixtures(ws.path(), data.path());
        let (mut core, rt) = core_at(ws.path(), data.path(), Arc::new(Backend::default()));
        let (entered, started) = tokio::sync::oneshot::channel();
        let backend = Arc::new(Blocking {
            entered: Mutex::new(Some(entered)),
            released: Mutex::new(false),
            wake: std::sync::Condvar::new(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let _release = Release(backend.clone());
        Arc::get_mut(&mut core).unwrap().instructions = InstructionTrust::with_store(Arc::new(
            TrustStore::new(data.path().into(), backend.clone()),
        ));
        let scope = ResourceScope::runtime(&rt);
        let resources: Vec<_> = collect(&core, &scope)
            .resources
            .into_iter()
            .filter(|resource| !resource.builtin())
            .collect();
        assert!(resources.len() >= 5);
        let cancel = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn({
            let core = core.clone();
            let cancel = cancel.clone();
            async move {
                core.instructions
                    .statuses(&scope, &resources, &cancel)
                    .await
            }
        });
        started.await.unwrap();
        if cancel_now {
            cancel.cancel();
        } else {
            tokio::time::pause();
            tokio::time::advance(std::time::Duration::from_secs(6)).await;
        }
        let statuses = tokio::time::timeout(std::time::Duration::from_millis(250), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            statuses
                .iter()
                .all(|status| !status.trusted && status.error.is_some())
        );
        assert_eq!(
            backend.reads.load(Ordering::SeqCst),
            1,
            "native failure triggered another per-resource wait"
        );
        if !cancel_now {
            tokio::time::resume();
        }
    }
}
