//! #1 路由夹具：五组固定路由、规模不升级、锁文件降级、覆盖不被当后门、路径重叠语义。

use super::*;

fn p(root: &str, rel: &str) -> PathRef {
    PathRef {
        root_id: root.into(),
        relative_path: rel.into(),
    }
}

fn write_input(paths: &[(&str, &str)]) -> RouteInput {
    RouteInput {
        planned_writes: paths.iter().map(|(r, rel)| p(r, rel)).collect(),
        ..Default::default()
    }
}

#[test]
fn pure_question_does_not_produce_write_units() {
    let mut i = write_input(&[]);
    i.suggested = Some(TaskKind::ParallelSurfaces);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::Conversation);
    assert!(!d.profile.kind.writes());
    assert!(to_work_units(&d.profile, &RunIdentity::root("s", "r"), &["r1".into()]).is_empty());
}

#[test]
fn mechanical_rename_is_single_write_with_no_reads() {
    let mut i = write_input(&[("ws", "src/lib.rs")]);
    i.suggested = Some(TaskKind::Mechanical);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::Mechanical);
    assert!(!d.allows_parallel());
}

#[test]
fn same_module_two_files_is_serial_not_parallel() {
    // 路径不相交 ≠ 语义独立：同一模块的两个文件可能互相引用类型、共享私有 helper。
    // 按「证明不了独立默认串行」，同顶层组件内一律串行。
    let mut i = write_input(&[("ws", "src/a.rs"), ("ws", "src/b.rs")]);
    i.planned_reads = vec![p("ws", "src/lib.rs")];
    i.suggested = Some(TaskKind::ParallelSurfaces);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::SerialSurfaces);
    assert_eq!(d.profile.surfaces.len(), 2);
    assert!(!d.allows_parallel());
    assert!(
        d.profile
            .reasons
            .iter()
            .any(|r| r.contains("证明不了语义独立"))
    );
}

#[test]
fn independent_front_and_back_are_parallel_candidates() {
    let mut i = write_input(&[("ws", "src/server.rs"), ("ws", "ui/src/App.tsx")]);
    i.suggested = Some(TaskKind::ParallelSurfaces);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::ParallelSurfaces);
    assert_eq!(d.profile.surfaces.len(), 2);
    assert!(
        d.allows_parallel(),
        "互不重叠的两面可并行（仍须过 #2 准入）"
    );
    assert!(d.profile.reasons.iter().any(|r| r.contains("可并行")));
}

#[test]
fn truly_overlapping_scopes_merge_into_one_surface() {
    // 写目录 `src/a` 与其下的 `src/a/x.rs`：祖先/子孙重叠 = 同一面、同一属主。
    let i = write_input(&[("ws", "src/a"), ("ws", "src/a/x.rs")]);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::SingleSurface);
    assert_eq!(d.profile.surfaces.len(), 1);
    assert_eq!(d.profile.surfaces[0].write_scopes.len(), 2);
}

#[test]
fn file_and_sibling_directory_are_not_overlapping() {
    // `src/a.rs` 是文件、`src/a/` 是目录：组件级比较下二者**不**互为祖先。
    assert!(!path_overlap(&p("ws", "src/a.rs"), &p("ws", "src/a/x.rs")));
}

#[test]
fn model_claiming_parallel_cannot_override_unprovable_independence() {
    // 两面都要动 Cargo.lock → 公共文件多属主，宿主降级串行。
    let mut i = write_input(&[
        ("ws", "src/a.rs"),
        ("ws", "ui/App.tsx"),
        ("ws", "Cargo.lock"),
    ]);
    i.suggested = Some(TaskKind::ParallelSurfaces);
    let d = route(&i);
    assert!(!d.allows_parallel());
    assert_eq!(d.profile.kind, TaskKind::SerialSurfaces);
    assert!(d.downgraded_because.unwrap().contains("公共文件"));
    assert!(d.profile.reasons.iter().any(|r| r.contains("无法证明独立")));
}

#[test]
fn real_dependency_forces_serial() {
    let mut i = write_input(&[("ws", "src/a.rs"), ("ws", "ui/App.tsx")]);
    i.unit_depends_on.insert("s2".into(), vec!["s1".into()]);
    i.suggested = Some(TaskKind::ParallelSurfaces);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::SerialSurfaces);
    assert!(d.downgraded_because.unwrap().contains("真实依赖"));
}

#[test]
fn a_big_repo_is_not_grounds_to_upgrade_to_full_workflow() {
    // 规模不是升级条件：一次写、零读依赖的改动在大仓库里仍是 mechanical。
    let mut i = write_input(&[("ws", "docs/very/deep/nested/path/file.md")]);
    i.user_text = "帮我把这个文档里的版本号改一下".into();
    i.suggested = Some(TaskKind::Mechanical);
    let d = route(&i);
    assert_eq!(d.profile.kind, TaskKind::Mechanical);
}

#[test]
fn shared_hidden_lock_file_downgrades_to_serial() {
    // 隐藏的共享锁文件（模型常常不主动列）由宿主按公共文件规则自己发现。
    let i = write_input(&[("ws", "src/a.rs"), ("ws", "pnpm-lock.yaml")]);
    let d = route(&i);
    assert_ne!(d.profile.kind, TaskKind::ParallelSurfaces);
    assert!(!d.allows_parallel());
}

#[test]
fn path_overlap_is_component_wise_not_string_prefix() {
    // `src/ab.rs` 与 `src/a.rs` 不是祖先关系（字符串前缀会误判）。
    assert!(!path_overlap(&p("ws", "src/ab.rs"), &p("ws", "src/a.rs")));
    assert!(path_overlap(&p("ws", "src/a"), &p("ws", "src/a/b/c.rs")));
    assert!(!path_overlap(&p("ws", "src/a.rs"), &p("other", "src/a.rs")));
}

#[test]
fn user_override_is_recorded_but_does_not_unlock_a_real_conflict() {
    // 覆盖面改成并行，但写范围确实重叠 → 覆盖不是后门。
    let i = write_input(&[("ws", "src/a.rs"), ("ws", "src/a/x.rs")]);
    let mut d = route(&i);
    assert!(!d.allows_parallel());
    let owner = RunIdentity::root("s1", "run1");
    let c = contract_at_rev(1);
    apply_override(
        &mut d,
        ProfileOverride {
            kind: TaskKind::ParallelSurfaces,
            actor_id: "user-1".into(),
            contract_revision: 1,
            reason: "我确认这两处互不影响".into(),
            at: "0".into(),
        },
        &c,
        &owner,
    )
    .expect("覆盖应被记录");
    assert_eq!(d.profile.kind, TaskKind::ParallelSurfaces, "分类已被覆盖");
    assert!(d.profile.override_record.is_some(), "覆盖必须留痕");
    assert!(!d.allows_parallel(), "但重叠判定不因覆盖解除");
    assert!(
        d.downgraded_because
            .unwrap()
            .contains("冲突判定不因覆盖解除")
    );
}

#[test]
fn override_on_a_stale_contract_revision_is_rejected() {
    let i = write_input(&[("ws", "src/a.rs")]);
    let mut d = route(&i);
    let c = contract_at_rev(3);
    let err = apply_override(
        &mut d,
        ProfileOverride {
            kind: TaskKind::SerialSurfaces,
            actor_id: "user-1".into(),
            contract_revision: 2,
            reason: "改一下".into(),
            at: "0".into(),
        },
        &c,
        &RunIdentity::root("s1", "run1"),
    )
    .unwrap_err();
    assert!(err.contains("过期修订"));
}

#[test]
fn override_without_actor_or_reason_is_rejected() {
    let i = write_input(&[("ws", "src/a.rs")]);
    let c = contract_at_rev(1);
    for bad in [
        ProfileOverride {
            kind: TaskKind::SerialSurfaces,
            actor_id: "  ".into(),
            contract_revision: 1,
            reason: "x".into(),
            at: "0".into(),
        },
        ProfileOverride {
            kind: TaskKind::SerialSurfaces,
            actor_id: "u".into(),
            contract_revision: 1,
            reason: " ".into(),
            at: "0".into(),
        },
    ] {
        let mut d = route(&i);
        assert!(apply_override(&mut d, bad, &c, &RunIdentity::root("s", "r")).is_err());
    }
}

#[test]
fn conversation_profile_carries_no_owners_and_no_units() {
    let d = route(&RouteInput::default());
    assert!(d.profile.owners.is_empty());
    assert!(to_work_units(&d.profile, &RunIdentity::root("s", "r"), &[]).is_empty());
}

#[test]
fn work_units_carry_requirement_and_shared_resource_context() {
    let mut i = write_input(&[("ws", "src/a.rs"), ("ws", "ui/App.tsx")]);
    i.shared_resources = vec!["postgres-main@16".into()];
    i.owner_hints.insert("s1".into(), "backend-dev".into());
    let d = route(&i);
    let units = to_work_units(
        &d.profile,
        &RunIdentity::root("s", "r"),
        &["r1".into(), "r2".into()],
    );
    assert_eq!(units.len(), 2);
    assert!(units.iter().all(|u| u.requirement_ids.len() == 2));
    assert!(
        units
            .iter()
            .all(|u| u.mutable_resources.contains(&"postgres-main@16".into()))
    );
    assert_eq!(
        units
            .iter()
            .find(|u| u.id == "unit_s1")
            .unwrap()
            .role_id
            .as_deref(),
        Some("backend-dev")
    );
}

#[test]
fn unattributable_write_capability_is_detected() {
    let mut u = WorkUnit {
        design_input: Default::default(),
        id: "u".into(),
        requirement_ids: vec![],
        read_scopes: vec![],
        write_scopes: vec![],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: None,
    };
    assert!(!u.has_unattributable_writes(&["edit".into()]));
    assert!(u.has_unattributable_writes(&["bash".into()]));
    assert!(u.has_unattributable_writes(&["mcp:server_x".into()]));
    u.write_scopes.push(p("ws", "Cargo.lock"));
    assert!(u.touches_public_file());
}

// ---------- 辅助 ----------

fn contract_at_rev(rev: u64) -> TaskContract {
    TaskContract {
        delivery_goal: Default::default(),
        schema: crate::core::delivery::types::SCHEMA,
        task_id: "t1".into(),
        revision: rev,
        owner: RunIdentity::root("s1", "run1"),
        requirements: vec![crate::core::delivery::types::Requirement {
            id: "r1".into(),
            title: "t".into(),
            required: true,
            check_ids: vec![],
            artifact_ids: vec![],
        }],
        artifacts: vec![],
        checks: vec![],
        review_policy: Default::default(),
        scope_changes: vec![],
    }
}

#[test]
fn one_file_without_explicit_mechanical_intent_is_not_mechanical() {
    assert_eq!(
        route(&write_input(&[("ws", "src/security.rs")]))
            .profile
            .kind,
        TaskKind::SingleSurface
    );
}
#[test]
fn shared_resource_defeats_disjoint_top_level_paths() {
    let mut i = write_input(&[("ws", "backend/api.rs"), ("ws", "frontend/view.ts")]);
    i.shared_resources.push("database:main".into());
    assert!(!route(&i).allows_parallel());
}

#[test]
fn cross_surface_reads_of_another_write_force_serial() {
    let mut input = write_input(&[("ws", "backend/api.rs"), ("ws", "frontend/view.ts")]);
    input.planned_reads.push(p("ws", "backend/api.rs"));
    assert!(!route(&input).allows_parallel());
}
