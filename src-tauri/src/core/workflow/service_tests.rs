use super::*;
fn fixture() -> (tempfile::TempDir, TaskContract, BTreeMap<String, PathBuf>) {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("api.rs"), b"fn api() {} ").unwrap();
    let c:TaskContract=serde_json::from_value(serde_json::json!({"schema":1,"task_id":"task1","revision":1,"owner":RunIdentity::root("session1","run1"),
        "requirements":[{"id":"r1","title":"api","required":true}],"artifacts":[{"id":"a1","path":{"root_id":"workspace","relative_path":"api.rs"},"kind":"text","required":true}]})).unwrap();
    let roots = BTreeMap::from([("workspace".into(), d.path().to_path_buf())]);
    (d, c, roots)
}
fn publish(d: &Path, c: &TaskContract) {
    crate::core::delivery::DeliveryService::from_data_root(d.to_path_buf())
        .store()
        .save_contract(
            &c.owner.session_id,
            &c.owner,
            &c.task_id,
            if c.revision == 1 {
                None
            } else {
                Some(c.revision - 1)
            },
            c,
            &format!("publish_{}_{}", c.task_id, c.revision),
        )
        .unwrap();
}
fn prepared(d: &Path, c: &mut TaskContract, roots: &BTreeMap<String, PathBuf>) -> PreparedWorkflow {
    let p = prepare(c, roots, d, "project1", None, &BTreeSet::new()).unwrap();
    persist(d, &p).unwrap();
    publish(d, c);
    p
}
fn design(d: &Path, c: &TaskContract) {
    let mut decision = read(d, &c.owner, &c.task_id).unwrap().design;
    decision.alternatives = vec!["existing architecture".into(), "rewrite".into()];
    decision.selected = Some("existing architecture".into());
    record_design(d, &c.owner, &c.task_id, c.revision, decision).unwrap();
}
#[test]
fn production_prepare_adds_real_policy_and_freezes_recoverable_state() {
    let (d, mut c, roots) = fixture();
    let p = prepared(d.path(), &mut c, &roots);
    assert!(c.check("backend_test").is_some());
    assert!(c.check("host_independent_review").is_some());
    assert!(c.review_policy.required && c.review_policy.independent);
    assert!(c.requirements[0].check_ids.contains(&"backend_test".into()));
    assert_eq!(
        read(d.path(), &c.owner, &c.task_id)
            .unwrap()
            .state
            .node("implementation")
            .unwrap()
            .state,
        NodeState::Running
    );
    assert_eq!(
        read(d.path(), &c.owner, &c.task_id)
            .unwrap()
            .snapshot
            .definition_digest,
        p.snapshot.definition_digest
    );
    assert!(design_ready(d.path(), &c.owner, &c.task_id).is_err());
    design(d.path(), &c);
    assert!(design_ready(d.path(), &c.owner, &c.task_id).is_ok());
}
#[test]
fn scope_revision_keeps_old_frozen_record_and_never_reuses_old_terminal() {
    let (d, mut c, roots) = fixture();
    prepared(d.path(), &mut c, &roots);
    let old = c.clone();
    unit_transition(
        d.path(),
        &c.owner,
        &c.task_id,
        "implementation",
        NodeState::Failed,
        0,
    )
    .unwrap();
    c.revision += 1;
    prepared(d.path(), &mut c, &roots);
    assert!(
        d.path()
            .join("sessions/session1.workflow/task1.r1.json")
            .exists()
    );
    let current = read(d.path(), &c.owner, &c.task_id).unwrap();
    assert_eq!(current.snapshot.contract_revision, 2);
    assert_eq!(
        current.state.node("implementation").unwrap().state,
        NodeState::Running
    );
    let wrong = RunIdentity::root("session1", "other-run");
    assert!(read(d.path(), &wrong, &old.task_id).is_err());
}
#[test]
fn real_unit_plan_enforces_owner_dependencies_and_recovers() {
    let (d, mut c, roots) = fixture();
    prepared(d.path(), &mut c, &roots);
    design(d.path(), &c);
    let unit = WorkUnit {
        design_input: Default::default(),
        id: "api".into(),
        requirement_ids: vec!["r1".into()],
        write_scopes: vec![c.artifacts[0].path.clone()],
        read_scopes: vec![],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: None,
    };
    plan_units(d.path(), &c.owner, &c, vec![unit], &roots).unwrap();
    let mut actor = c.owner.clone();
    actor.actor_id = "sub-real".into();
    unit_transition(d.path(), &actor, &c.task_id, "api", NodeState::Running, 7).unwrap();
    let mut forged = actor.clone();
    forged.actor_id = "sub-forged".into();
    assert!(unit_transition(d.path(), &forged, &c.task_id, "api", NodeState::Passed, 7).is_err());
    assert!(unit_transition(d.path(), &actor, &c.task_id, "api", NodeState::Passed, 6).is_err());
    unit_transition(d.path(), &actor, &c.task_id, "api", NodeState::Passed, 7).unwrap();
    assert_eq!(
        read(d.path(), &c.owner, &c.task_id)
            .unwrap()
            .state
            .node("api")
            .unwrap()
            .state,
        NodeState::Passed
    );
}
#[test]
fn external_project_definition_cannot_be_implicitly_trusted() {
    let (d, mut c, roots) = fixture();
    std::fs::create_dir_all(d.path().join(".codewave")).unwrap();
    std::fs::write(d.path().join(".codewave/workflows.json"), b"{}").unwrap();
    assert!(
        prepare(
            &mut c,
            &roots,
            d.path(),
            "project1",
            Some(d.path()),
            &BTreeSet::new()
        )
        .unwrap_err()
        .contains("未经用户确认")
    );
}

#[test]
fn approved_stage_graph_controls_real_unit_start_and_finish() {
    let (d, mut contract, roots) = fixture();
    let definition:WorkflowDefinition=serde_json::from_value(serde_json::json!({"schema":1,"id":"project","nodes":[{"id":"build","title":"build","next":["review"],"required_checks":["unit_test"]},{"id":"review","title":"review"}],"checks":[{"id":"unit_test","command_text":"cargo test"}],"repair_rules":[{"id":"retry","from":"review","to":"build","max_rounds":2,"fingerprint_budget":2}],"edges":[{"from":"build","to":"review","kind":"advance"},{"from":"review","to":"build","kind":"repair"}]})).unwrap();
    let config = d.path().join(".codewave/workflows.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec_pretty(&definition).unwrap();
    std::fs::write(&config, &bytes).unwrap();
    crate::core::process_coordination::atomic_write(
        &approval_pin(d.path(), d.path()),
        version::digest_of_bytes(&bytes).as_bytes(),
    )
    .unwrap();
    let p = prepare(
        &mut contract,
        &roots,
        d.path(),
        "project1",
        Some(d.path()),
        &BTreeSet::new(),
    )
    .unwrap();
    persist(d.path(), &p).unwrap();
    publish(d.path(), &contract);
    design(d.path(), &contract);
    let mut a = WorkUnit {
        design_input: Default::default(),
        id: "a".into(),
        requirement_ids: vec!["r1".into()],
        write_scopes: vec![contract.artifacts[0].path.clone()],
        read_scopes: vec![],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: Some("build".into()),
    };
    let mut b = a.clone();
    b.id = "b".into();
    b.write_scopes.clear();
    b.stage_id = Some("review".into());
    assert!(
        plan_units(
            d.path(),
            &contract.owner,
            &contract,
            vec![a.clone(), b.clone()],
            &roots
        )
        .unwrap_err()
        .contains("未绑定工作单元")
    );
    a.checks.push("project_unit_test".into());
    plan_units(
        d.path(),
        &contract.owner,
        &contract,
        vec![a.clone(), b],
        &roots,
    )
    .unwrap();
    assert!(
        unit_transition(
            d.path(),
            &contract.owner,
            &contract.task_id,
            "b",
            NodeState::Running,
            2
        )
        .is_err()
    );
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "a",
        NodeState::Running,
        1,
    )
    .unwrap();
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "a",
        NodeState::Failed,
        1,
    )
    .unwrap();
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "a",
        NodeState::Running,
        2,
    )
    .unwrap();
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "a",
        NodeState::Passed,
        2,
    )
    .unwrap();
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "b",
        NodeState::Running,
        3,
    )
    .unwrap();
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "b",
        NodeState::Passed,
        3,
    )
    .unwrap();
    let recovered = read(d.path(), &contract.owner, &contract.task_id).unwrap();
    assert_eq!(
        recovered.state.node("stage:build").unwrap().state,
        NodeState::Passed
    );
    assert_eq!(
        recovered.state.node("stage:review").unwrap().state,
        NodeState::Passed
    );
    assert_eq!(recovered.state.node("a").unwrap().repair_rounds, 1);
    a.id = "attempt_cannot_replace_frozen_plan".into();
    assert!(plan_units(d.path(), &contract.owner, &contract, vec![a], &roots).is_err());
}

#[test]
fn orphan_prepared_revision_never_supersedes_published_contract() {
    let (d, mut c, roots) = fixture();
    prepared(d.path(), &mut c, &roots);
    let mut orphan = c.clone();
    orphan.revision = 2;
    let p = prepare(
        &mut orphan,
        &roots,
        d.path(),
        "project1",
        None,
        &BTreeSet::new(),
    )
    .unwrap();
    persist(d.path(), &p).unwrap();
    assert_eq!(
        read(d.path(), &c.owner, &c.task_id)
            .unwrap()
            .snapshot
            .contract_revision,
        1
    );
}
#[test]
fn trusted_mechanical_override_recomputes_design_but_keeps_risky_gate() {
    for (path, required) in [("notes.txt", false), ("secrets.rs", true)] {
        let (d, mut c, roots) = fixture();
        c.artifacts[0].path.relative_path = path.into();
        std::fs::write(d.path().join(path), "original").unwrap();
        prepared(d.path(), &mut c, &roots);
        assert!(design_ready(d.path(), &c.owner, &c.task_id).is_err());
        record_override(
            d.path(),
            &c.owner,
            &c.task_id,
            1,
            TaskKind::Mechanical,
            "用户确认单包机械调整",
        )
        .unwrap();
        let p = read(d.path(), &c.owner, &c.task_id).unwrap();
        assert_eq!(p.design.required, required, "{path}");
        assert!(p.design.user_override.is_some());
        assert_eq!(
            design_ready(d.path(), &c.owner, &c.task_id).is_err(),
            required
        );
        assert!(c.review_policy.required);
    }
}

#[test]
fn concurrent_same_root_unit_transitions_keep_every_node_and_revision() {
    let (d, mut contract, roots) = fixture();
    let count = 12;
    let prototype = contract.artifacts[0].clone();
    contract.artifacts.clear();
    let mut units = Vec::new();
    for i in 0..count {
        let mut artifact = prototype.clone();
        artifact.id = format!("artifact{i}");
        artifact.path.relative_path = format!("component{i}/result.txt");
        units.push(WorkUnit {
            design_input: Default::default(),
            id: format!("unit{i}"),
            requirement_ids: vec!["r1".into()],
            read_scopes: vec![],
            write_scopes: vec![artifact.path.clone()],
            depends_on: vec![],
            mutable_resources: vec![],
            role_id: None,
            model_id: None,
            checks: vec![],
            stage_id: None,
        });
        contract.artifacts.push(artifact);
    }
    prepared(d.path(), &mut contract, &roots);
    design(d.path(), &contract);
    plan_units(d.path(), &contract.owner, &contract, units, &roots).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(count));
    std::thread::scope(|scope| {
        let mut threads = Vec::new();
        for i in 0..count {
            let barrier = barrier.clone();
            let path = d.path();
            let task = &contract.task_id;
            let mut actor = contract.owner.clone();
            actor.actor_id = format!("actor{i}");
            threads.push(scope.spawn(move || {
                barrier.wait();
                unit_transition(
                    path,
                    &actor,
                    task,
                    &format!("unit{i}"),
                    NodeState::Running,
                    i as u64,
                )
                .unwrap();
                unit_transition(
                    path,
                    &actor,
                    task,
                    &format!("unit{i}"),
                    NodeState::Passed,
                    i as u64,
                )
                .unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
    });
    let state = read(d.path(), &contract.owner, &contract.task_id)
        .unwrap()
        .state;
    for i in 0..count {
        assert_eq!(
            state.node(&format!("unit{i}")).unwrap().state,
            NodeState::Passed
        );
    }
    assert_eq!(state.revision, 1 + 2 * count as u64);
}

#[test]
fn transition_journal_repairs_a_missing_mutable_projection() {
    let (d, mut contract, roots) = fixture();
    prepared(d.path(), &mut contract, &roots);
    unit_transition(
        d.path(),
        &contract.owner,
        &contract.task_id,
        "implementation",
        NodeState::Failed,
        0,
    )
    .unwrap();
    std::fs::remove_file(
        d.path()
            .join("sessions/session1.workflow/task1.state-r1.json"),
    )
    .unwrap();
    let recovered = read(d.path(), &contract.owner, &contract.task_id).unwrap();
    assert_eq!(
        recovered.state.node("implementation").unwrap().state,
        NodeState::Failed
    );
    assert!(
        unit_transition(
            d.path(),
            &RunIdentity::root("session1", "wrong"),
            &contract.task_id,
            "implementation",
            NodeState::Passed,
            0
        )
        .is_err()
    );
}
#[test]
fn explicit_rebind_preserves_definition_and_advances_attempts_without_old_actors() {
    let (d, mut contract, roots) = fixture();
    let original = prepared(d.path(), &mut contract, &roots);
    let mut next = contract.clone();
    next.task_id = "new_task".into();
    next.owner = RunIdentity::root("session1", "next-run");
    let rebound = original.rebind(&next);
    assert_eq!(
        rebound.snapshot.definition_digest,
        original.snapshot.definition_digest
    );
    assert_eq!(rebound.state.node("implementation").unwrap().attempt, 1);
    assert_eq!(original.state.node("implementation").unwrap().attempt, 0);
    assert_eq!(rebound.snapshot.owner.root_run_id, "next-run");
}
#[test]
fn shared_budget_is_atomic_and_retry_idempotent() {
    let (d, mut contract, roots) = fixture();
    prepared(d.path(), &mut contract, &roots);
    let service = crate::core::delivery::DeliveryService::from_data_root(d.path().to_path_buf());
    let gap = crate::core::delivery::completion::Gap {
        requirement_id: "r1".into(),
        check_id: None,
        reason_code: "node_failed".into(),
        message: "failure".into(),
        fingerprint: "stable".into(),
        needs_user_or_env: false,
    };
    let first = service
        .reserve_repair(
            &contract.owner,
            &contract.task_id,
            "workflow_rule_u1_1",
            std::slice::from_ref(&gap),
            Some(("rule", 1, 1)),
        )
        .unwrap();
    let replay = service
        .reserve_repair(
            &contract.owner,
            &contract.task_id,
            "workflow_rule_u1_1",
            std::slice::from_ref(&gap),
            Some(("rule", 1, 1)),
        )
        .unwrap();
    assert_eq!(first, replay);
    assert_eq!(first.rounds_used, 1);
    assert!(
        service
            .reserve_repair(
                &contract.owner,
                &contract.task_id,
                "workflow_rule_u1_2",
                &[gap],
                Some(("rule", 1, 1))
            )
            .is_err()
    );
    assert_eq!(service.load_budget("session1", "task1").rounds_used, 1);
}
#[test]
fn bound_definition_rejects_dangling_requirements_at_exact_pointer() {
    let (_, contract, _) = fixture();
    let def:WorkflowDefinition=serde_json::from_value(serde_json::json!({"schema":1,"id":"test","nodes":[{"id":"build","kind":"implement","requirement_ids":["missing"],"tool_allowlist":[]}]})).unwrap();
    let errors = super::super::definition::validate_bound_definition(&def, &contract).unwrap_err();
    assert_eq!(errors[0].pointer, "/nodes/0/requirement_ids/0");
}

#[test]
fn project_review_node_injects_real_evidence_even_for_a_small_task() {
    let (d, mut c, roots) = fixture();
    c.artifacts[0].path.relative_path = "README.md".into();
    std::fs::write(d.path().join("README.md"), "docs").unwrap();
    let cfg = crate::core::config::ConfigState::default();
    let view = super::super::project::read(d.path(), d.path(), &cfg).unwrap();
    let text = r#"{"schema":1,"id":"project","nodes":[{"id":"build","kind":"implement","tool_allowlist":["read"]},{"id":"review","kind":"review","required_checks":["host_independent_review"],"tool_allowlist":["read","delivery_contract"]},{"id":"finish","kind":"complete","tool_allowlist":[]}],"edges":[{"from":"build","to":"review"},{"from":"review","to":"finish"}]}"#;
    super::super::project::save(
        d.path(),
        d.path(),
        &cfg,
        text,
        0,
        &view.digest,
        "save",
        &c.owner,
    )
    .unwrap();
    let p = prepare(
        &mut c,
        &roots,
        d.path(),
        "project1",
        Some(d.path()),
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(c.check("host_independent_review").is_some());
    assert!(c.review_policy.independent);
    assert_eq!(
        p.snapshot.resource_hashes["project_workflow"],
        super::super::project::read(d.path(), d.path(), &cfg)
            .unwrap()
            .digest
    );
}
#[test]
fn pr_goal_adds_the_complete_gate_while_development_keeps_local_checks() {
    let (d, mut c, roots) = fixture();
    c.delivery_goal = DeliveryGoal::PullRequest;
    prepare(&mut c, &roots, d.path(), "project1", None, &BTreeSet::new()).unwrap();
    assert!(c.check("prepr_full_gate").is_some());
    assert!(c.check("backend_test").is_some());
}
#[test]
fn design_confirmation_unblocks_design_dependency_and_does_not_force_docs_design() {
    let (d, mut c, roots) = fixture();
    std::fs::write(d.path().join("README.md"), "docs").unwrap();
    c.artifacts.push(ArtifactRequirement {
        id: "docs".into(),
        path: PathRef {
            root_id: "workspace".into(),
            relative_path: "README.md".into(),
        },
        kind: ArtifactKind::Text,
        required: true,
        constraints: vec![],
    });
    let cfg = crate::core::config::ConfigState::default();
    let view = super::super::project::read(d.path(), d.path(), &cfg).unwrap();
    let text = r#"{"schema":1,"id":"project","nodes":[{"id":"design","kind":"design","tool_allowlist":[]},{"id":"build","kind":"implement","tool_allowlist":["read","edit"]}],"edges":[{"from":"design","to":"build"}]}"#;
    super::super::project::save(
        d.path(),
        d.path(),
        &cfg,
        text,
        0,
        &view.digest,
        "save",
        &c.owner,
    )
    .unwrap();
    let p = prepare(
        &mut c,
        &roots,
        d.path(),
        "project1",
        Some(d.path()),
        &BTreeSet::new(),
    )
    .unwrap();
    persist(d.path(), &p).unwrap();
    publish(d.path(), &c);
    let mut unit = WorkUnit {
        design_input: Default::default(),
        id: "code".into(),
        requirement_ids: vec!["r1".into()],
        read_scopes: vec![],
        write_scopes: vec![PathRef {
            root_id: "workspace".into(),
            relative_path: "api.rs".into(),
        }],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: Some("build".into()),
    };
    let mut doc = unit.clone();
    doc.id = "docs".into();
    doc.write_scopes[0].relative_path = "README.md".into();
    doc.design_input.documentation_only = true;
    let prepared = plan_units(d.path(), &c.owner, &c, vec![unit.clone(), doc], &roots).unwrap();
    assert!(unit_design_ready(d.path(), &c.owner, &c.task_id, "docs").is_ok());
    assert!(unit_design_ready(d.path(), &c.owner, &c.task_id, "code").is_err());
    let mut decision = prepared.unit_designs["code"].clone();
    decision.alternatives = vec!["keep".into()];
    decision.selected = Some("keep".into());
    record_unit_design(d.path(), &c.owner, &c.task_id, c.revision, "code", decision).unwrap();
    assert_eq!(
        read(d.path(), &c.owner, &c.task_id)
            .unwrap()
            .state
            .node("stage:design")
            .unwrap()
            .state,
        NodeState::Passed
    );
    unit_transition(
        d.path(),
        &c.owner,
        &c.task_id,
        "code",
        NodeState::Running,
        1,
    )
    .unwrap();
    let frozen = read(d.path(), &c.owner, &c.task_id).unwrap();
    let mut next_contract = c.clone();
    next_contract.task_id = "continued-task".into();
    next_contract.owner = RunIdentity::root(&c.owner.session_id, "continued-root");
    let next = frozen.rebind(&next_contract);
    persist(d.path(), &next).unwrap();
    publish(d.path(), &next_contract);
    advance_automatic_nodes(
        d.path(),
        &next_contract.owner,
        &next_contract.task_id,
        false,
    )
    .unwrap();
    assert_eq!(
        read(d.path(), &next_contract.owner, &next_contract.task_id)
            .unwrap()
            .state
            .node("stage:design")
            .unwrap()
            .state,
        NodeState::Passed
    );
    assert_eq!(
        read(d.path(), &next_contract.owner, &next_contract.task_id)
            .unwrap()
            .state
            .node("code")
            .unwrap()
            .state,
        NodeState::Pending
    );
    unit.stage_id = Some("design".into());
    assert!(plan_units(d.path(), &c.owner, &c, vec![unit], &roots).is_err());
}
#[tokio::test]
async fn failed_resume_clears_pending_and_open_only_marks_interrupted() {
    let d = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots::new(
        d.path().join("workspace"),
        d.path().join("profile"),
    );
    std::fs::create_dir_all(&roots.workspace).unwrap();
    std::fs::create_dir_all(&roots.data_dir).unwrap();
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "session1",
        roots.workspace.clone(),
        Some("project1".into()),
        vec![],
        None,
        vec![],
    );
    std::fs::write(roots.workspace.join("api.rs"), "api").unwrap();
    let (_, mut c, _) = fixture();
    c.owner = RunIdentity::root(&rt.id, "old-run");
    let resolver = crate::core::delivery::DeliveryService::roots_from_runtime(&rt);
    prepared(&rt.data_dir, &mut c, &resolver);
    super::super::lifecycle::interrupt(&core, &rt).unwrap();
    assert_eq!(
        read(&rt.data_dir, &c.owner, &c.task_id)
            .unwrap()
            .state
            .node("implementation")
            .unwrap()
            .state,
        NodeState::Interrupted
    );
    assert!(!rt.running.load(std::sync::atomic::Ordering::Acquire));
    core.cfg.write().unwrap().providers.clear();
    let result = super::super::lifecycle::continue_task(&core, rt.clone(), &c.task_id, c.revision);
    assert!(result.is_err());
    assert!(rt.pending_contract.lock().unwrap().is_none());
    assert!(rt.pending_workflow.lock().unwrap().is_none());
    assert_eq!(
        rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    );
}

#[test]
fn review_failure_reopens_implementation_as_a_new_budgeted_attempt() {
    let (d, mut c, roots) = fixture();
    c.owner = RunIdentity::root("repair-lease-session", uuid::Uuid::new_v4().to_string());
    let cfg = crate::core::config::ConfigState::default();
    let view = super::super::project::read(d.path(), d.path(), &cfg).unwrap();
    let text = r#"{"schema":1,"id":"project","nodes":[{"id":"build","kind":"implement","tool_allowlist":["read","edit"]},{"id":"review","kind":"review","tool_allowlist":["read","delivery_contract"],"required_checks":["host_independent_review"]}],"edges":[{"from":"build","to":"review"}],"repair_rules":[{"id":"fix","from":"review","to":"build","max_rounds":1,"fingerprint_budget":1}]}"#;
    super::super::project::save(
        d.path(),
        d.path(),
        &cfg,
        text,
        0,
        &view.digest,
        "save",
        &c.owner,
    )
    .unwrap();
    let p = prepare(
        &mut c,
        &roots,
        d.path(),
        "project1",
        Some(d.path()),
        &BTreeSet::new(),
    )
    .unwrap();
    persist(d.path(), &p).unwrap();
    publish(d.path(), &c);
    let a = WorkUnit {
        design_input: Default::default(),
        id: "a".into(),
        requirement_ids: vec!["r1".into()],
        read_scopes: vec![],
        write_scopes: vec![PathRef {
            root_id: "workspace".into(),
            relative_path: "api.rs".into(),
        }],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: Some("build".into()),
    };
    let mut b = a.clone();
    b.id = "b".into();
    b.write_scopes.clear();
    b.stage_id = Some("review".into());
    b.checks = vec!["host_independent_review".into()];
    plan_units(d.path(), &c.owner, &c, vec![a, b], &roots).unwrap();
    unit_transition(d.path(), &c.owner, &c.task_id, "a", NodeState::Running, 1).unwrap();
    unit_transition(d.path(), &c.owner, &c.task_id, "a", NodeState::Passed, 1).unwrap();
    unit_transition(d.path(), &c.owner, &c.task_id, "b", NodeState::Running, 2).unwrap();
    unit_transition(d.path(), &c.owner, &c.task_id, "b", NodeState::Failed, 2).unwrap();
    let mut state = read(d.path(), &c.owner, &c.task_id).unwrap().state;
    state.nodes.get_mut("b").unwrap().actor_id = Some("actual-sub-id".into());
    state.revision += 1;
    super::super::lifecycle::save_state(
        d.path(),
        &c.task_id,
        c.revision,
        &state,
        "test-actor-binding",
    )
    .unwrap();
    let lease = super::super::dispatch::DispatchLease {
        root_run_id: c.owner.root_run_id.clone(),
        unit_id: "actual-sub-id".into(),
        attempt: 0,
        generation: 2,
        held_paths: vec![],
        exclusive: false,
        mutable_resources: vec![],
    };
    super::super::dispatch::remember_lease(&lease);
    assert!(
        unit_transition(d.path(), &c.owner, &c.task_id, "a", NodeState::Running, 3)
            .unwrap_err()
            .contains("cleanup")
    );
    assert_eq!(
        crate::core::delivery::DeliveryService::from_data_root(d.path().to_path_buf())
            .load_budget(&c.owner.session_id, &c.task_id)
            .rounds_used,
        0
    );
    super::super::dispatch::release(&lease.root_run_id, &lease.unit_id, lease.generation).unwrap();
    unit_transition(d.path(), &c.owner, &c.task_id, "a", NodeState::Running, 3).unwrap();
    let next = read(d.path(), &c.owner, &c.task_id).unwrap();
    assert_eq!(next.state.node("a").unwrap().attempt, 1);
    assert_eq!(next.state.node("b").unwrap().state, NodeState::Pending);
    assert_eq!(
        next.state.node("stage:review").unwrap().state,
        NodeState::Pending
    );
    assert_eq!(
        crate::core::delivery::DeliveryService::from_data_root(d.path().to_path_buf())
            .load_budget(&c.owner.session_id, &c.task_id)
            .rounds_used,
        1
    );
    unit_transition(d.path(), &c.owner, &c.task_id, "a", NodeState::Failed, 3).unwrap();
    assert!(unit_transition(d.path(), &c.owner, &c.task_id, "a", NodeState::Running, 4).is_err());
}

#[test]
fn mechanical_resume_uses_new_host_routing_provenance_without_old_waivers() {
    let (d, mut c, roots) = fixture();
    c.artifacts.clear();
    let mut p = prepared(d.path(), &mut c, &roots);
    p.resumed_from = Some((c.task_id.clone(), c.owner.root_run_id.clone()));
    assert!(!c.review_policy.required);
    assert!(c.checks.iter().all(|check| check.kind != CheckKind::Review));
    let mut draft: ContractDraft =
        serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
    draft.roots.clear();
    draft.scope_changes.clear();
    let delivery = crate::core::delivery::DeliveryService::from_data_root(d.path().to_path_buf());
    let next = RunIdentity::root(&c.owner.session_id, "resumed-mechanical-root");
    let (contract, _) = delivery
        .create_contract_with(&next, &draft, &roots, "resume-mechanical", |new| {
            super::super::lifecycle::record_resume_provenance(&p, new)
                .map_err(|message| crate::core::delivery::DeliveryError::Save { message })?;
            persist(d.path(), &p.rebind(new))
                .map_err(|message| crate::core::delivery::DeliveryError::Save { message })
        })
        .unwrap();
    assert_eq!(contract.scope_changes.len(), 1);
    assert_eq!(contract.scope_changes[0].kind, ScopeChangeKind::Added);
    assert!(contract.scope_changes[0].user_decision_id.is_none());
    assert!(contract.scope_changes[0].reason.contains(&c.task_id));
    assert_eq!(contract.owner, next);
}
