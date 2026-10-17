//! #17 结构化交付夹具：四象限 × 提交/未提交、字段越界拒绝、重复提交幂等、
//! 迟到结果不写入下一 run、恢复后同一 delivery_id、partial 保留成果但不解锁完整结果。

use super::SUBMIT_DELIVERY_TOOL;
use crate::core::delivery::service::DeliveryService;
use crate::core::delivery::types::*;
use std::sync::Arc;

fn svc() -> (tempfile::TempDir, Arc<DeliveryService>) {
    let dir = tempfile::tempdir().unwrap();
    let svc = Arc::new(DeliveryService::new(dir.path().join("sessions")));
    (dir, svc)
}

fn root() -> RunIdentity {
    RunIdentity::root("sess1", "run1")
}

/// 子 runtime 身份：同 root_run，actor 独立。
fn sub(sub_id: &str) -> RunIdentity {
    RunIdentity {
        session_id: "sess1".into(),
        root_run_id: "run1".into(),
        actor_id: sub_id.into(),
        actor_run_id: format!("{sub_id}-run"),
        sub_id: Some(sub_id.into()),
    }
}

fn contract() -> TaskContract {
    TaskContract {
        delivery_goal: Default::default(),
        schema: SCHEMA,
        task_id: "task1".into(),
        revision: 1,
        owner: root(),
        requirements: vec![
            Requirement {
                id: "r1".into(),
                title: "写实现".into(),
                required: true,
                check_ids: vec![],
                artifact_ids: vec![],
            },
            Requirement {
                id: "r2".into(),
                title: "补测试".into(),
                required: true,
                check_ids: vec![],
                artifact_ids: vec![],
            },
        ],
        artifacts: Vec::new(),
        checks: Vec::new(),
        review_policy: ReviewPolicy::default(),
        scope_changes: Vec::new(),
    }
}

fn persist_contract(s: &DeliveryService, c: &TaskContract) {
    s.store()
        .save_contract("sess1", &root(), "task1", None, c, "c_op")
        .unwrap();
}

fn submit(
    s: &DeliveryService,
    who: &RunIdentity,
    ended: EndedKind,
    completed: Vec<&str>,
    incomplete: Vec<IncompleteItem>,
    op: &str,
) -> Result<
    (
        DeliveryResult,
        crate::core::delivery::WriteReceipt,
        SubmissionState,
    ),
    crate::core::delivery::DeliveryError,
> {
    s.submit_delivery(
        who,
        &contract(),
        ended,
        completed.into_iter().map(String::from).collect(),
        incomplete,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        op,
    )
}

#[test]
fn root_runtime_cannot_submit_delivery() {
    // 根身份没有 root_run 可归属，结构化提交对它无意义。
    let (_d, s) = svc();
    let err = submit(&s, &root(), EndedKind::Report, vec!["r1"], vec![], "op1").unwrap_err();
    assert_eq!(err.code(), "E_DELIVERY_OWNER");
}

#[test]
fn late_submission_from_previous_run_is_rejected() {
    // 迟到结果：属于上一 root_run 的子代理不能把草稿写进本 run。
    let (_d, s) = svc();
    let prev = RunIdentity {
        session_id: "sess1".into(),
        root_run_id: "run0".into(),
        actor_id: "sub-a".into(),
        actor_run_id: "sub-a-run".into(),
        sub_id: Some("sub-a".into()),
    };
    let err = submit(&s, &prev, EndedKind::Report, vec!["r1"], vec![], "op1").unwrap_err();
    assert_eq!(err.code(), "E_DELIVERY_OWNER");
}

#[test]
fn cross_session_submission_is_rejected() {
    let (_d, s) = svc();
    let mut other = sub("sub-a");
    other.session_id = "sess2".into();
    let err = submit(&s, &other, EndedKind::Report, vec!["r1"], vec![], "op1").unwrap_err();
    assert_eq!(err.code(), "E_DELIVERY_OWNER");
}

#[test]
fn unknown_ids_are_invalid_submission_not_silent_accept() {
    let (_d, s) = svc();
    let (_, _, state) = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1", "r999"],
        vec![],
        "op1",
    )
    .unwrap();
    assert_eq!(state, SubmissionState::Invalid);
}

#[test]
fn contradictory_item_state_is_invalid() {
    // 同一 id 既完成又未完成 = 自相矛盾，必须被拒。
    let (_d, s) = svc();
    let (_, _, state) = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1"],
        vec![IncompleteItem {
            target_id: "r1".into(),
            reason: "其实没做完".into(),
        }],
        "op1",
    )
    .unwrap();
    assert_eq!(state, SubmissionState::Invalid);
}

#[test]
fn budget_tail_cannot_claim_completed_items() {
    // 预算耗尽 / 未交报告时声明"已完成"是假完成的首选路径，必须被识别为无效。
    for ended in [EndedKind::Budget, EndedKind::NoReport] {
        let (_d, s) = svc();
        let (_, _, state) = submit(&s, &sub("sub-a"), ended, vec!["r1"], vec![], "op1").unwrap();
        assert_eq!(state, SubmissionState::Invalid, "{ended:?} 不得声明完成项");
    }
}

#[test]
fn partial_keeps_results_without_promoting_to_complete() {
    // partial = 撞顶逼出来的草稿：成果保留，但状态仍是 Partial。
    let (_d, s) = svc();
    let (result, _, state) = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Partial,
        vec!["r1"],
        vec![IncompleteItem {
            target_id: "r2".into(),
            reason: "预算耗尽".into(),
        }],
        "op1",
    )
    .unwrap();
    assert_eq!(state, SubmissionState::Valid);
    assert_eq!(result.ended, EndedKind::Partial);
    assert_eq!(result.completed, vec!["r1"]);
    assert_eq!(result.incomplete.len(), 1);
}

#[test]
fn repeated_submission_is_idempotent() {
    let (_d, s) = svc();
    let first = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1"],
        vec![],
        "op1",
    )
    .unwrap();
    let second = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1"],
        vec![],
        "op1",
    )
    .unwrap();
    assert_eq!(first.0.id, second.0.id);
    assert_eq!(s.ledger("sess1").deliveries.len(), 1);
}

#[test]
fn contract_revision_is_frozen_into_the_delivery() {
    let (_d, s) = svc();
    let (result, _, _) = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1"],
        vec![],
        "op1",
    )
    .unwrap();
    assert_eq!(result.contract_revision, 1);
    assert_eq!(result.task_id, "task1");
    // owner 由宿主赋值：与提交者一致，不是模型自称
    assert_eq!(result.owner.actor_id, "sub-a");
}

#[test]
fn deliveries_are_grouped_per_task_for_restore() {
    // 恢复时主卡 / 抽屉 / 子过程流共用同一 delivery_id：账本按 task 归集即可。
    let (_d, s) = svc();
    persist_contract(&s, &contract());
    submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1"],
        vec![],
        "op1",
    )
    .unwrap();
    submit(
        &s,
        &sub("sub-b"),
        EndedKind::Report,
        vec!["r2"],
        vec![],
        "op2",
    )
    .unwrap();
    let ledger = s.ledger("sess1");
    assert_eq!(ledger.deliveries_for("task1").len(), 2);
    // 两次派发是不同 actor 的独立身份
    let actors: std::collections::BTreeSet<&str> = ledger
        .deliveries_for("task1")
        .iter()
        .map(|d| d.owner.actor_id.as_str())
        .collect();
    assert_eq!(actors.len(), 2, "两个子代理必须是不同 actor");
}

#[test]
fn unsubmitted_task_keeps_legacy_text_report_semantics() {
    // 没有结构化提交时，账本里没有交付记录——旧 `<report>` 路径不受影响也不被改写。
    let (_d, s) = svc();
    assert!(s.ledger("sess1").deliveries_for("task1").is_empty());
}

#[test]
fn tool_is_hidden_from_main_session() {
    let prefs = crate::core::prefs::SessionPrefs::default();
    let params = crate::core::agent::drive::main_drive_params(&prefs);
    assert!(
        params
            .exclude_tools
            .iter()
            .any(|t| t == SUBMIT_DELIVERY_TOOL),
        "主会话不得看到 submit_delivery"
    );
}

#[test]
fn tool_is_available_to_sub_runtime() {
    let reg = crate::tools::registry::ToolRegistry::default_tools();
    assert!(reg.get(SUBMIT_DELIVERY_TOOL).is_some(), "子代理必须能用它");
}

#[test]
fn delivery_result_round_trips_through_disk() {
    // 恢复一致性：落盘记录读回后字段完整（主卡 / 抽屉 / 子过程流共用它）。
    let (_d, s) = svc();
    persist_contract(&s, &contract());
    let (result, receipt, _) = submit(
        &s,
        &sub("sub-a"),
        EndedKind::Report,
        vec!["r1"],
        vec![IncompleteItem {
            target_id: "r2".into(),
            reason: "未开始".into(),
        }],
        "op1",
    )
    .unwrap();
    assert!(receipt.persisted);
    let raw = s
        .store()
        .load_record("sess1", &result.id)
        .expect("应可从索引读回");
    match raw.body {
        crate::core::delivery::store::RecordBody::Delivery(d) => {
            assert_eq!(d.id, result.id);
            assert_eq!(d.completed, vec!["r1"]);
            assert_eq!(d.incomplete[0].target_id, "r2");
        }
        other => panic!("记录种类不符：{}", other.kind_name()),
    }
}
