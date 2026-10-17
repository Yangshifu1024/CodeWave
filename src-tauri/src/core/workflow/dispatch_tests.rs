//! #2 并行准入夹具：同路径 / 祖先 / symlink 别名 / 锁文件冲突、独立包同时准入、
//! shell 改公共文件串行、MCP 未知写串行、父与子公共写拒绝、迟到释放不影响后继、
//! 依赖 partial 不解锁。

use super::*;
use std::collections::BTreeMap;

struct Fx {
    _dir: tempfile::TempDir,
    roots: BTreeMap<String, PathBuf>,
}

/// 每个用例一个独立 root run id：租约表是进程级全局的，并行执行时
/// 若共用 run id 会互相把请求判成"过期"，也会让路径冲突用例看到别的用例的租约。
fn fx() -> Fx {
    let d = tempfile::tempdir().unwrap();
    let mut roots = BTreeMap::new();
    roots.insert("ws".to_string(), d.path().to_path_buf());
    Fx { _dir: d, roots }
}

fn unit(id: &str, rels: &[&str]) -> WorkUnit {
    WorkUnit {
        design_input: Default::default(),
        id: id.into(),
        requirement_ids: vec![],
        read_scopes: vec![],
        write_scopes: rels
            .iter()
            .map(|r| PathRef {
                root_id: "ws".into(),
                relative_path: (*r).into(),
            })
            .collect(),
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: None,
    }
}

fn owner() -> RunIdentity {
    RunIdentity::root("s1", "run1")
}

/// 读本 run 的当前 generation（`admission` 的 expected_generation 入参）。
fn next_gen(run: &str) -> u64 {
    table()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .generation
        .get(run)
        .copied()
        .unwrap_or(0)
}

/// 准入一批（单元 → 工具表）。
fn admit(
    f: &Fx,
    run: &str,
    units: Vec<WorkUnit>,
    deps: BTreeMap<String, bool>,
    tools: BTreeMap<String, Vec<String>>,
) -> Result<Vec<DispatchLease>, Vec<Denial>> {
    let cands: Vec<(WorkUnit, Vec<String>)> = units.into_iter().map(|u| (u, vec![])).collect();
    let mut identity = owner();
    identity.root_run_id = run.into();
    admission(
        &identity,
        run,
        next_gen(run),
        &cands,
        &f.roots,
        &deps,
        &tools,
    )
}

#[test]
fn independent_packages_are_admitted_together() {
    let f = fx();
    let a = unit("a", &["src-tauri/src/core/x.rs"]);
    let b = unit("b", &["ui/src/App.tsx"]);
    let leases = admit(
        &f,
        "r_independent_packages_are_admitted_together",
        vec![a, b],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(leases.len(), 2);
    assert_eq!(leases[0].generation, leases[1].generation);
}

#[test]
fn the_same_path_conflicts() {
    let f = fx();
    admit(
        &f,
        "r_the_same_path_conflicts",
        vec![unit("a", &["src/x.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let err = admit(
        &f,
        "r2_the_same_path_conflicts",
        vec![unit("b", &["src/x.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "path_claimed"));
}

#[test]
fn an_ancestor_path_conflicts() {
    let f = fx();
    admit(
        &f,
        "r_an_ancestor_path_conflicts",
        vec![unit("a", &["src"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let err = admit(
        &f,
        "r2_an_ancestor_path_conflicts",
        vec![unit("b", &["src/deep/x.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "path_claimed"));
}

#[cfg(unix)]
#[test]
fn a_symlink_alias_resolves_to_the_same_lease() {
    // 符号链接别名必须落到同一条目，否则两个名字就能绕过认领各写一次。
    let f = fx();
    let real = f.roots["ws"].join("real.txt");
    std::fs::write(&real, b"x").unwrap();
    let alias = f.roots["ws"].join("alias.txt");
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    admit(
        &f,
        "r_a_symlink_alias_resolves_to_the_same_lease",
        vec![unit("a", &["real.txt"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let err = admit(
        &f,
        "r2_a_symlink_alias_resolves_to_the_same_lease",
        vec![unit("b", &["alias.txt"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(
        err.iter().any(|d| d.code == "path_claimed"),
        "symlink 别名必须同租约"
    );
}

#[cfg(windows)]
#[test]
fn a_junction_alias_resolves_to_the_same_lease() {
    let f = fx();
    let real = f.roots["ws"].join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("file.txt"), b"x").unwrap();
    let alias = f.roots["ws"].join("alias");
    // Directory junctions exercise real Windows alias resolution without
    // requiring the symlink privilege or Developer Mode on the test machine.
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&alias)
        .arg(&real)
        .output()
        .expect("mklink /J");
    assert!(
        output.status.success(),
        "junction creation failed: {output:?}"
    );
    assert_eq!(
        alias.join("file.txt").canonicalize().unwrap(),
        real.join("file.txt").canonicalize().unwrap()
    );
    admit(
        &f,
        "r_a_junction_alias_resolves_to_the_same_lease",
        vec![unit("a", &["real/file.txt"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let err = admit(
        &f,
        "r2_a_junction_alias_resolves_to_the_same_lease",
        vec![unit("b", &["alias/file.txt"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(
        err.iter().any(|d| d.code == "path_claimed"),
        "junction aliases must share the same lease"
    );
}

#[test]
fn a_lock_file_write_is_a_single_owner_resource() {
    let f = fx();
    admit(
        &f,
        "r_a_lock_file_write_is_a_single_owner_resource",
        vec![unit("a", &["src/a.rs", "Cargo.lock"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let err = admit(
        &f,
        "r_a_lock_file_write_is_a_single_owner_resource",
        vec![unit("b", &["Cargo.lock"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "public_file_single_owner"));
}

#[test]
fn a_shell_unit_with_unattributable_writes_is_serialised() {
    let f = fx();
    let mut tools = BTreeMap::new();
    tools.insert("a".to_string(), vec!["bash".to_string()]);
    let err = admit(
        &f,
        "r_a_shell_unit_with_unattributable_writes_is_serialised",
        vec![unit("a", &["src/a.rs"]), unit("b", &["ui/app.ts"])],
        BTreeMap::new(),
        tools,
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "side_effect_scope_unknown"));
    assert!(err[0].message.contains("不采信模型对写入范围的承诺"));
}

#[test]
fn an_mcp_unit_is_serialised_too() {
    let f = fx();
    let mut tools = BTreeMap::new();
    tools.insert("a".to_string(), vec!["mcp:github".to_string()]);
    let err = admit(
        &f,
        "r_an_mcp_unit_is_serialised_too",
        vec![unit("a", &["src/a.rs"]), unit("b", &["ui/app.ts"])],
        BTreeMap::new(),
        tools,
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "side_effect_scope_unknown"));
}

#[test]
fn a_structured_writer_is_attributable() {
    let f = fx();
    let mut tools = BTreeMap::new();
    tools.insert(
        "a".to_string(),
        vec!["edit".to_string(), "create".to_string()],
    );
    assert!(
        admit(
            &f,
            "r_a_structured_writer_is_attributable",
            vec![unit("a", &["src/a.rs"])],
            BTreeMap::new(),
            tools
        )
        .is_ok()
    );
}

#[test]
fn a_partial_dependency_does_not_unlock() {
    let f = fx();
    let mut u = unit("b", &["ui/App.tsx"]);
    u.depends_on.push("a".into());
    // partial / failed 都表示"没通过"。
    let mut deps = BTreeMap::new();
    deps.insert("a".to_string(), false);
    let err = admit(
        &f,
        "r_a_partial_dependency_does_not_unlock",
        vec![u.clone()],
        deps,
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "dependency_not_accepted"));
    // 通过后才解锁。
    let mut deps = BTreeMap::new();
    deps.insert("a".to_string(), true);
    assert!(
        admit(
            &f,
            "r_a_partial_dependency_does_not_unlock",
            vec![u],
            deps,
            BTreeMap::new()
        )
        .is_ok()
    );
}

#[test]
fn shared_mutable_resources_conflict_within_one_batch() {
    let f = fx();
    let mut a = unit("a", &["src-tauri/src/x.rs"]);
    let mut b = unit("b", &["ui/src/y.tsx"]);
    a.mutable_resources.push("postgres-main@16".into());
    b.mutable_resources.push("postgres-main@16".into());
    let err = admit(
        &f,
        "r_shared_mutable_resources_conflict_within_one_batch",
        vec![a, b],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "shared_resource_conflict"));
}

#[test]
fn one_denied_unit_blocks_the_whole_batch() {
    // 全查后全插：不允许"半准入"，否则调度看起来并行、实际半串行。
    let f = fx();
    admit(
        &f,
        "r_one_denied_unit_blocks_the_whole_batch",
        vec![unit("held", &["src/x.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let batch = vec![unit("ok", &["ui/a.tsx"]), unit("bad", &["src/x.rs"])];
    assert!(
        admit(
            &f,
            "r2_one_denied_unit_blocks_the_whole_batch",
            batch,
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    // 被拒批次里的 "ok" 并没有拿到租约。
    let err = release("r2_one_denied_unit_blocks_the_whole_batch", "ok", 0);
    assert!(err.is_err(), "整批拒绝不得留下部分租约");
}

#[test]
fn a_late_release_does_not_free_the_successor_attempt() {
    let f = fx();
    let leases = admit(
        &f,
        "r_a_late_release_does_not_free_the_successor_attempt",
        vec![unit("a", &["src/a.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let g0 = leases[0].generation;
    // A 正常释放。
    release(
        "r_a_late_release_does_not_free_the_successor_attempt",
        "a",
        g0,
    )
    .unwrap();
    // A2 重新准入，拿到新 generation。
    let leases2 = admit(
        &f,
        "r_a_late_release_does_not_free_the_successor_attempt",
        vec![unit("a", &["src/a.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let g1 = leases2[0].generation;
    assert_ne!(g0, g1);
    // A 的迟到完成拿着旧 generation 来释放 → 必须被拒。
    let err = release(
        "r_a_late_release_does_not_free_the_successor_attempt",
        "a",
        g0,
    )
    .unwrap_err();
    assert_eq!(err.code, "generation_mismatch");
    // 后继租约仍在。
    assert!(active_leases().iter().any(|l| l.generation == g1));
}

#[test]
fn a_stale_admission_request_does_not_take_a_slot() {
    let f = fx();
    let cands: Vec<(WorkUnit, Vec<String>)> = vec![(unit("a", &["src/a.rs"]), vec![])];
    // 故意用过期 generation。
    let err = admission(
        &RunIdentity::root("s1", "r_a_stale_admission_request_does_not_take_a_slot"),
        "r_a_stale_admission_request_does_not_take_a_slot",
        next_gen("r_a_stale_admission_request_does_not_take_a_slot") + 99,
        &cands,
        &f.roots,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "stale_generation"));
    assert!(
        !active_leases()
            .iter()
            .any(|l| l.root_run_id == "run1" && l.unit_id == "a"),
        "过期请求不得占用槽位"
    );
}

#[test]
fn the_concurrency_limit_is_enforced() {
    let f = fx();
    for i in 0..MAX_CONCURRENT_UNITS {
        assert!(
            admit(
                &f,
                "r_the_concurrency_limit_is_enforced",
                vec![unit(&format!("u{i}"), &[&format!("src/{i}.rs")])],
                BTreeMap::new(),
                BTreeMap::new()
            )
            .is_ok()
        );
    }
    let err = admit(
        &f,
        "r_the_concurrency_limit_is_enforced",
        vec![unit("overflow", &["src/overflow.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "concurrency_limit"));
}

#[test]
fn a_root_outside_the_session_snapshot_is_refused() {
    let f = fx();
    let mut u = unit("a", &["src/a.rs"]);
    u.write_scopes[0].root_id = "不存在的根".into();
    let err = admit(
        &f,
        "r_a_root_outside_the_session_snapshot_is_refused",
        vec![u],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(err.iter().any(|d| d.code == "root_not_in_snapshot"));
}

#[test]
fn separately_spawned_siblings_conflict_and_raii_releases_slot() {
    let f = fx();
    let run = "siblings_raii_regression";
    let first = admit(
        &f,
        run,
        vec![unit("a", &["src"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap()
    .remove(0);
    let denied = admit(
        &f,
        run,
        vec![unit("b", &["src/a.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap_err();
    assert!(denied.iter().any(|d| d.code == "path_claimed"));
    assert!(check_write(run, "parent", &f.roots["ws"].join("src/a.rs")).is_err());
    assert!(check_write(run, "a", &f.roots["ws"].join("elsewhere.rs")).is_err());
    drop(LeaseGuard(first));
    assert!(
        admit(
            &f,
            run,
            vec![unit("b", &["src/a.rs"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_ok()
    );
}
#[test]
fn unknown_command_writes_serialize_without_bypass() {
    let f = fx();
    let run = "unknown_serial_regression";
    let tools = BTreeMap::from([("a".into(), vec!["command".into()])]);
    let first = admit(&f, run, vec![unit("a", &[])], BTreeMap::new(), tools)
        .unwrap()
        .remove(0);
    assert!(first.exclusive);
    assert!(
        admit(
            &f,
            run,
            vec![unit("b", &["ui"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(check_unattributed(run, "parent").is_err());
    assert!(check_unattributed(run, "a").is_ok());
    assert!(check_unattributed("unrelated_root_run", "parent").is_ok());
    drop(LeaseGuard(first));
}
#[test]
fn shared_mutable_resource_conflicts_across_separate_admissions() {
    let f = fx();
    let run = "resource_serial_regression";
    let mut a = unit("a", &["backend"]);
    a.mutable_resources = vec!["database:test".into()];
    let mut b = unit("b", &["frontend"]);
    b.mutable_resources = a.mutable_resources.clone();
    let first = admit(&f, run, vec![a], BTreeMap::new(), BTreeMap::new())
        .unwrap()
        .remove(0);
    assert!(admit(&f, run, vec![b], BTreeMap::new(), BTreeMap::new()).is_err());
    drop(LeaseGuard(first));
}

#[test]
fn parent_opaque_effect_blocks_later_child_admission_until_drop() {
    let f = fx();
    let run = "parent_opaque_effect_race";
    let guard = acquire_effect(run, "parent", &[], true, &f.roots).unwrap();
    assert!(
        admit(
            &f,
            run,
            vec![unit("child", &["a.rs"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    // Same actor may run normal commands while its own background service exists.
    let same_actor = acquire_effect(run, "parent", &[], true, &f.roots).unwrap();
    drop(same_actor);
    drop(guard);
    let lease = admit(
        &f,
        run,
        vec![unit("child", &["a.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap()
    .remove(0);
    assert!(acquire_effect(run, "parent", &[], true, &f.roots).is_err());
    drop(LeaseGuard(lease));
}

#[test]
fn parent_file_effect_serializes_overlap_but_preserves_disjoint_child_parallelism() {
    let f = fx();
    let run = "parent_file_effect_race";
    let path = f.roots["ws"].join("a.rs");
    let guard = acquire_effect(run, "parent", &[path], false, &f.roots).unwrap();
    assert!(
        admit(
            &f,
            run,
            vec![unit("child", &["a.rs"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    let lease = admit(
        &f,
        run,
        vec![unit("child", &["b.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap()
    .remove(0);
    let child_effect =
        acquire_effect(run, "child", &[f.roots["ws"].join("b.rs")], false, &f.roots).unwrap();
    drop(child_effect);
    drop(LeaseGuard(lease));
    drop(guard);
}

#[test]
fn background_effect_blocks_same_physical_workspace_in_next_run() {
    let f = fx();
    let guard = acquire_effect("previous_run", "parent", &[], true, &f.roots).unwrap();
    assert!(
        admit(
            &f,
            "next_run",
            vec![unit("child", &["a.rs"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    drop(guard);
}

#[test]
fn effect_guard_covers_failed_admission_and_unwind_without_releasing_others() {
    let f = fx();
    let run = "effect_panic_cleanup";
    let surviving = acquire_effect(
        run,
        "parent",
        &[f.roots["ws"].join("a.rs")],
        false,
        &f.roots,
    )
    .unwrap();
    let _ = std::panic::catch_unwind(|| {
        let _temporary = acquire_effect(
            run,
            "parent",
            &[f.roots["ws"].join("b.rs")],
            false,
            &f.roots,
        )
        .unwrap();
        panic!("fixture unwind");
    });
    assert!(
        admit(
            &f,
            run,
            vec![unit("a_child", &["a.rs"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    let lease = admit(
        &f,
        run,
        vec![unit("b_child", &["b.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap()
    .remove(0);
    drop(LeaseGuard(lease));
    drop(surviving);
}

#[test]
fn identical_actor_labels_in_different_runs_are_not_reentrant() {
    let f = fx();
    let original = acquire_effect("first_root", "root", &[], true, &f.roots).unwrap();
    assert!(acquire_effect("second_root", "root", &[], true, &f.roots).is_err());
    drop(original);
    let other = acquire_effect("second_root", "root", &[], true, &f.roots).unwrap();
    drop(other);
}

#[test]
fn generic_http_api_capability_cannot_claim_structured_parallel_ownership() {
    let writer = unit("http-writer", &["api.rs"]);
    assert_eq!(
        side_effect_scope(&writer, &["http_request".into()]),
        SideEffectScope::Unknown
    );
    assert_eq!(
        side_effect_scope(&writer, &["web_fetch".into()]),
        SideEffectScope::Attributable
    );
}

#[test]
fn root_structured_edits_keep_existing_file_lock_queue_and_both_block_children() {
    let f = fx();
    let paths = [f.roots["ws"].join("a.rs")];
    let first = acquire_effect("queued_root_one", "root", &paths, false, &f.roots).unwrap();
    let second = acquire_effect("queued_root_two", "root", &paths, false, &f.roots).unwrap();
    drop(first);
    assert!(
        admit(
            &f,
            "queued_child",
            vec![unit("child", &["a.rs"])],
            BTreeMap::new(),
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(acquire_effect("opaque_other", "root", &[], true, &f.roots).is_err());
    drop(second);
    let lease = admit(
        &f,
        "queued_child",
        vec![unit("child", &["a.rs"])],
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .unwrap()
    .remove(0);
    drop(LeaseGuard(lease));
}
