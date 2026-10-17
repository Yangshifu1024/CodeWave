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
            &format!("publish{}", c.revision),
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
    let definition:WorkflowDefinition=serde_json::from_value(serde_json::json!({"schema":1,"id":"project","stages":[{"id":"build","title":"build","next":["review"],"required_checks":["unit_test"]},{"id":"review","title":"review"}],"checks":[{"id":"unit_test","command_text":"cargo test"}],"edges":[{"from":"build","to":"review","kind":"advance"},{"from":"review","to":"build","kind":"repair"}]})).unwrap();
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
