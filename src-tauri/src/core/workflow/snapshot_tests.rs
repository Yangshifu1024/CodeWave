//! #8 冻结快照夹具：运行中改配置旧 run 不变而新 run 用新版、撤销权限不被快照带回、
//! 模型/角色被删阻塞、快照缺失不拿当前定义补齐、跨项目与跨会话拒绝。

use super::*;
use crate::core::workflow::config::{Edge, EdgeKind, Stage, WorkflowDefinition};

fn owner() -> RunIdentity {
    RunIdentity::root("s1", "run1")
}

fn def(rev: u64) -> WorkflowDefinition {
    WorkflowDefinition {
        schema: 1,
        id: "main".into(),
        revision: rev,
        stages: vec![Stage {
            id: "a".into(),
            title: "a".into(),
            next: vec![],
            required_checks: vec![],

            ..Default::default()
        }],
        edges: vec![Edge {
            from: "a".into(),
            to: "a".into(),
            kind: EdgeKind::Advance,
        }],
        roles: BTreeMap::new(),
        checks: vec![],
        author: Some("user-1".into()),

        ..Default::default()
    }
}

fn snap(def_rev: u64) -> FrozenSnapshot {
    let mut b = BTreeMap::new();
    b.insert(
        "a".to_string(),
        UnitBinding {
            role_id: Some("backend-dev".into()),
            model_id: Some("m-a".into()),
        },
    );
    freeze(owner(), "p1", &def(def_rev), b, BTreeMap::new(), "0")
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn editing_the_definition_does_not_change_a_running_snapshot() {
    // 运行中：冻结的是内嵌定义副本，不是 id。
    let running = snap(4);
    let _edited_on_disk = def(9);
    assert_eq!(running.definition.as_ref().unwrap().revision, 4);
}

#[test]
fn a_new_run_picks_up_the_new_revision() {
    let old = snap(4);
    let new_def = def(9);
    let fresh = freeze(
        owner(),
        "p1",
        &new_def,
        BTreeMap::new(),
        BTreeMap::new(),
        "1",
    );
    assert_eq!(old.definition.as_ref().unwrap().revision, 4);
    assert_eq!(fresh.definition.as_ref().unwrap().revision, 9);
}

#[test]
fn restore_does_not_resurrect_a_revoked_permission() {
    let s = snap(4);
    // 快照**不携带**权限：恢复必须走既有档位解析路径。
    assert!(snapshot_permission(&s).is_none());
    assert_eq!(summary(&s)["carries_permission"], false);
    // 序列化出的结构里也不该出现权限字段。
    let json = serde_json::to_string(&s).unwrap();
    for banned in ["approval_mode", "permission", "access_mode"] {
        assert!(!json.contains(banned), "快照不得冻结权限：{banned}");
    }
}

#[test]
fn a_removed_model_blocks_rather_than_silently_substituting() {
    let s = snap(4);
    let err = restore(
        &s,
        "s1",
        "p1",
        &set(&["backend-dev"]),
        &set(&[]), // 模型已删
    )
    .unwrap_err();
    assert_eq!(err.code, "model_removed");
    assert!(err.message.contains("不静默换人"));
}

#[test]
fn a_removed_role_blocks_too() {
    let s = snap(4);
    let err = restore(&s, "s1", "p1", &set(&[]), &set(&["m-a"])).unwrap_err();
    assert_eq!(err.code, "role_removed");
}

#[test]
fn a_missing_definition_is_not_filled_in_from_the_current_one() {
    let mut s = snap(4);
    s.definition = None;
    // 即使当前配置完全可用，也不补齐。
    let err = restore(&s, "s1", "p1", &set(&["backend-dev"]), &set(&["m-a"])).unwrap_err();
    assert_eq!(err.code, "snapshot_missing_definition");
    assert!(err.message.contains("旧 run 跑的是旧规则"));
}

#[test]
fn cross_project_restore_is_refused() {
    let s = snap(4);
    let err = restore(&s, "s1", "p2", &set(&["backend-dev"]), &set(&["m-a"])).unwrap_err();
    assert_eq!(err.code, "cross_project");
    assert!(err.message.contains("跨项目引用拒绝"));
}

#[test]
fn cross_session_restore_is_refused() {
    let s = snap(4);
    let err = restore(&s, "s2", "p1", &set(&["backend-dev"]), &set(&["m-a"])).unwrap_err();
    assert_eq!(err.code, "cross_session");
}

#[test]
fn a_healthy_snapshot_restores_its_bindings() {
    let s = snap(4);
    let out = restore(&s, "s1", "p1", &set(&["backend-dev"]), &set(&["m-a"])).unwrap();
    assert_eq!(out["a"].model_id.as_deref(), Some("m-a"));
    assert_eq!(out["a"].role_id.as_deref(), Some("backend-dev"));
}

#[test]
fn a_snapshot_round_trips_through_json() {
    let s = snap(4);
    let back: FrozenSnapshot = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}

#[test]
fn a_legacy_snapshot_without_bindings_still_reads_back() {
    // 老快照没有绑定字段：读回即空绑定，不补造"当时用的是谁"。
    let legacy: FrozenSnapshot = serde_json::from_str(
        r#"{"owner":{"session_id":"s","root_run_id":"r"},"project_id":"p","at":"0"}"#,
    )
    .unwrap();
    assert!(legacy.unit_bindings.is_empty());
    assert!(legacy.definition.is_none());
    assert!(restore(&legacy, "s", "p", &set(&[]), &set(&[])).is_err());
}

#[test]
fn the_summary_is_machine_readable() {
    let s = summary(&snap(4));
    assert_eq!(s["definition_revision"], 4);
    assert_eq!(s["definition_id"], "main");
    assert_eq!(s["units"][0], "a");
}
