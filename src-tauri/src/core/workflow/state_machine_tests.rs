//! #5 状态机夹具：重复终态幂等、旧 attempt 迟到拒绝、依赖未通过不能 running、
//! 非法边拒绝、预算内修复受限、身份不符拒绝、快照可恢复且不重跑工具。

use super::*;
use crate::core::workflow::config::{Edge, EdgeKind, Stage, WorkflowDefinition};
use std::collections::BTreeSet;

fn owner() -> RunIdentity {
    RunIdentity::root("s1", "run1")
}

fn def() -> WorkflowDefinition {
    WorkflowDefinition {
        schema: 1,
        id: "main".into(),
        revision: 4,
        // 阶段 id 与单元 id 对齐（`a` 是干活的单元，`b` 是其下游校验单元）：
        // `b → a` 是 repair 回边，`a` 失败后据此在预算内复活。
        stages: vec![
            Stage {
                id: "a".into(),
                title: "a".into(),
                next: vec!["b".into()],
                required_checks: vec![],
            },
            Stage {
                id: "b".into(),
                title: "b".into(),
                next: vec![],
                required_checks: vec![],
            },
        ],
        edges: vec![
            Edge {
                from: "a".into(),
                to: "b".into(),
                kind: EdgeKind::Advance,
            },
            Edge {
                from: "b".into(),
                to: "a".into(),
                kind: EdgeKind::Repair,
            },
        ],
        roles: BTreeMap::new(),
        checks: vec![],
        author: None,
    }
}

fn snap_with_deps() -> WorkflowRunSnapshot {
    let mut deps = BTreeMap::new();
    deps.insert("b".to_string(), vec!["a".to_string()]);
    WorkflowRunSnapshot::with_deps(
        owner(),
        "main",
        4,
        &["a".to_string(), "b".to_string()],
        &deps,
    )
}

fn drive(
    s: &mut WorkflowRunSnapshot,
    d: &WorkflowDefinition,
    unit: &str,
    to: NodeState,
    attempt: u32,
    generation: u64,
) -> Result<Node, Rejection> {
    transition(s, d, unit, to, attempt, generation, 2, &owner())
}

#[test]
fn a_pending_unit_can_start_running_when_it_has_no_dependencies() {
    let mut s = snap_with_deps();
    let n = drive(&mut s, &def(), "a", NodeState::Running, 0, 1).unwrap();
    assert_eq!(n.state, NodeState::Running);
    assert_eq!(s.revision, 1);
}

#[test]
fn a_dependent_unit_cannot_run_until_its_dependency_passes() {
    let mut s = snap_with_deps();
    // b 依赖 a，a 还没过 → b 不能 running。
    let err = drive(&mut s, &def(), "b", NodeState::Running, 0, 1).unwrap_err();
    assert_eq!(err.code, "deps_not_passed");
    assert_eq!(s.node("b").unwrap().state, NodeState::Pending);

    drive(&mut s, &def(), "a", NodeState::Running, 0, 1).unwrap();
    // a 失败也不算通过：一次失败**不使依赖自动 ready**。
    drive(&mut s, &def(), "a", NodeState::Failed, 0, 1).unwrap();
    let err = drive(&mut s, &def(), "b", NodeState::Running, 0, 1).unwrap_err();
    assert_eq!(err.code, "deps_not_passed");

    // a 真正通过后 b 才能起。
    let mut s2 = snap_with_deps();
    drive(&mut s2, &def(), "a", NodeState::Running, 0, 1).unwrap();
    drive(&mut s2, &def(), "a", NodeState::Passed, 0, 1).unwrap();
    assert!(drive(&mut s2, &def(), "b", NodeState::Running, 0, 1).is_ok());
}

#[test]
fn repeating_the_same_terminal_is_idempotent_and_does_not_bump_revision() {
    let mut s = snap_with_deps();
    drive(&mut s, &def(), "a", NodeState::Running, 0, 1).unwrap();
    let first = drive(&mut s, &def(), "a", NodeState::Passed, 0, 1).unwrap();
    let rev = s.revision;
    let again = drive(&mut s, &def(), "a", NodeState::Passed, 0, 1).unwrap();
    assert_eq!(first, again, "重复终态幂等");
    assert_eq!(s.revision, rev, "重复上报不进位");
}

#[test]
fn a_late_result_from_an_older_attempt_is_rejected() {
    let mut s = snap_with_deps();
    drive(&mut s, &def(), "a", NodeState::Running, 0, 2).unwrap();
    drive(&mut s, &def(), "a", NodeState::Failed, 0, 2).unwrap();
    drive(&mut s, &def(), "a", NodeState::Running, 1, 2).unwrap();
    drive(&mut s, &def(), "a", NodeState::Failed, 1, 2).unwrap();
    // A 的迟到 attempt 0 不得覆盖 A2。
    let err = drive(&mut s, &def(), "a", NodeState::Passed, 0, 1).unwrap_err();
    assert!(
        matches!(err.code, "stale_attempt" | "terminal_immutable"),
        "迟到结果必须被拒：{}",
        err.code
    );
}

#[test]
fn a_terminal_state_cannot_be_rewritten_as_running() {
    let mut s = snap_with_deps();
    drive(&mut s, &def(), "a", NodeState::Running, 0, 1).unwrap();
    drive(&mut s, &def(), "a", NodeState::Cancelled, 0, 1).unwrap();
    let err = drive(&mut s, &def(), "a", NodeState::Running, 1, 2).unwrap_err();
    assert_eq!(err.code, "terminal_immutable");
}

#[test]
fn repair_rounds_are_bounded_and_then_block() {
    let mut s = snap_with_deps();
    let d = def();
    // 第一轮、第二轮（repair 边指向 build）
    for round in 0..2u64 {
        drive(&mut s, &d, "a", NodeState::Running, round as u32, round + 1).unwrap();
        drive(&mut s, &d, "a", NodeState::Failed, round as u32, round + 1).unwrap();
        let n = drive(
            &mut s,
            &d,
            "a",
            NodeState::Running,
            (round + 1) as u32,
            round + 2,
        )
        .unwrap();
        assert_eq!(n.repair_rounds as u64, round + 1);
    }
    drive(&mut s, &d, "a", NodeState::Failed, 2, 3).unwrap();
    // 预算用尽：不再回边。
    let err = drive(&mut s, &d, "a", NodeState::Running, 3, 4).unwrap_err();
    assert!(matches!(
        err.code,
        "repair_budget_exhausted" | "terminal_immutable"
    ));
}

#[test]
fn an_illegal_edge_in_the_definition_is_not_taken_as_permission() {
    // 定义里没有 x → a 这条边，状态机也不该凭空允许它存在。
    let d = def();
    assert!(!edge_allowed(&d, "x", "a"));
    assert!(edge_allowed(&d, "a", "b"));
    assert!(edge_allowed(&d, "b", "a"), "repair 回边也是定义里声明的边");
}

#[test]
fn another_session_cannot_advance_this_snapshot() {
    let mut s = snap_with_deps();
    let mut other = owner();
    other.session_id = "s2".into();
    let err = transition(&mut s, &def(), "a", NodeState::Running, 0, 1, 2, &other).unwrap_err();
    assert_eq!(err.code, "identity_mismatch");

    let mut other_run = owner();
    other_run.root_run_id = "run9".into();
    let err = transition(&mut s, &def(), "a", NodeState::Running, 0, 1, 2, &other_run).unwrap_err();
    assert_eq!(err.code, "identity_mismatch");
}

#[test]
fn unknown_unit_is_rejected() {
    let mut s = snap_with_deps();
    let err = drive(&mut s, &def(), "nope", NodeState::Running, 0, 1).unwrap_err();
    assert_eq!(err.code, "unknown_unit");
}

#[test]
fn restoring_from_a_snapshot_does_not_re_run_anything() {
    // 恢复只是把状态读回来：节点仍在终态、revision 不动，不会重放工具。
    let mut s = snap_with_deps();
    drive(&mut s, &def(), "a", NodeState::Running, 0, 1).unwrap();
    drive(&mut s, &def(), "a", NodeState::Passed, 0, 1).unwrap();
    let json = serde_json::to_string(&s).unwrap();
    let restored: WorkflowRunSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, s);
    assert_eq!(restored.node("a").unwrap().state, NodeState::Passed);
    assert_eq!(restored.dependents_of("a"), vec!["b".to_string()]);
}

#[test]
fn the_definition_revision_is_frozen_into_the_snapshot() {
    let s = snap_with_deps();
    assert_eq!(s.definition_revision, 4);
    // 老会话缺该字段时按 0 读回（显示未验证），不补造。
    // 老快照只有最小字段：缺的定义修订按 0 读回（显示未验证），缺的身份按根 run 补齐。
    let legacy: WorkflowRunSnapshot = serde_json::from_str(
        r#"{"owner":{"session_id":"s","root_run_id":"r"},"definition_id":"main"}"#,
    )
    .expect("老快照必须读得回来");
    assert_eq!(legacy.definition_revision, 0);
    assert!(legacy.nodes.is_empty());
    assert_eq!(legacy.owner.actor_id, "root");
}

#[test]
fn cancelled_run_does_not_dispatch_anything_new() {
    let mut s = snap_with_deps();
    drive(&mut s, &def(), "a", NodeState::Running, 0, 1).unwrap();
    drive(&mut s, &def(), "a", NodeState::Cancelled, 0, 1).unwrap();
    // run 取消后 b 不得被推进（依赖既没通过，a 也已是终态）。
    assert!(drive(&mut s, &def(), "b", NodeState::Running, 0, 1).is_err());
}

#[test]
fn concurrent_cas_only_one_commit_succeeds() {
    // 同一 expected_revision 的两次提交：第二次冲突。
    let d = tempfile::tempdir().unwrap();
    let mut defs = def();
    defs.revision = 1;
    std::fs::create_dir_all(d.path().join(".codewave")).unwrap();
    std::fs::write(
        d.path().join(".codewave/workflows.json"),
        serde_json::to_vec(&defs).unwrap(),
    )
    .unwrap();
    let first = super::super::config::commit(
        &d.path().join(".codewave/workflows.json"),
        &defs,
        1,
        d.path(),
        &BTreeSet::new(),
        &owner(),
    );
    assert!(matches!(
        first,
        super::super::config::SaveOutcome::Committed { .. }
    ));
    let second = super::super::config::commit(
        &d.path().join(".codewave/workflows.json"),
        &defs,
        1,
        d.path(),
        &BTreeSet::new(),
        &owner(),
    );
    assert!(matches!(
        second,
        super::super::config::SaveOutcome::Conflict { .. }
    ));
}

#[test]
fn pending_cannot_skip_execution_and_report_passed() {
    let mut s = snap_with_deps();
    assert_eq!(
        drive(&mut s, &def(), "a", NodeState::Passed, 0, 0)
            .unwrap_err()
            .code,
        "illegal_transition"
    );
}
#[test]
fn terminal_from_wrong_lease_generation_is_rejected() {
    let mut s = snap_with_deps();
    drive(&mut s, &def(), "a", NodeState::Running, 0, 2).unwrap();
    assert_eq!(
        drive(&mut s, &def(), "a", NodeState::Passed, 0, 1)
            .unwrap_err()
            .code,
        "lease_generation_mismatch"
    );
}
