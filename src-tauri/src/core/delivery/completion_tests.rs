//! #16 完成门夹具：假完成不得 passed、换时间戳不重置预算、到限保留 partial、
//! 取消不触发修复、持久恢复剩同预算、无契约沿原收尾、保存失败不得 allow。

use super::*;
use crate::core::delivery::types::{
    CheckKind, CheckSpec, PathRef, Reason, Requirement, RunIdentity, SCHEMA, TaskContract,
};

fn contract() -> TaskContract {
    TaskContract {
        delivery_goal: Default::default(),
        schema: SCHEMA,
        task_id: "task1".into(),
        revision: 1,
        owner: RunIdentity::root("sess1", "run1"),
        requirements: vec![Requirement {
            id: "r1".into(),
            title: "产物就位".into(),
            required: true,
            check_ids: vec!["ci".into()],
            artifact_ids: vec![],
        }],
        artifacts: Vec::new(),
        checks: vec![CheckSpec {
            platforms: vec![],
            prerequisites: vec![],
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            independent_verifier: false,
            id: "ci".into(),
            kind: CheckKind::Command,
            command: None,
            scope: crate::core::delivery::types::VerificationScope::default(),
            required: true,
            expectation: crate::core::delivery::types::CheckExpectation::Command {
                allowed_exit_codes: vec![0],
            },
        }],
        review_policy: crate::core::delivery::types::ReviewPolicy::default(),
        scope_changes: Vec::new(),
    }
}

/// 快照工厂：默认"已知缺口"（模型声称做完、但没通过任何检查）。
fn snap(overall: AcceptanceStatusKind) -> AcceptanceSnapshot {
    AcceptanceSnapshot {
        task_id: "task1".into(),
        contract_revision: 1,
        revision: 1,
        items: vec![crate::core::delivery::types::AcceptanceItem {
            requirement_id: "r1".into(),
            status: match overall {
                AcceptanceStatusKind::Passed => AcceptanceStatus::Passed,
                AcceptanceStatusKind::Pending => AcceptanceStatus::Pending,
                AcceptanceStatusKind::Failed => AcceptanceStatus::Failed,
                AcceptanceStatusKind::Stale => AcceptanceStatus::Stale,
                AcceptanceStatusKind::Blocked => AcceptanceStatus::Blocked,
                AcceptanceStatusKind::Cancelled => AcceptanceStatus::Cancelled,
            },
            evidence_refs: Vec::new(),
            reasons: vec![Reason {
                code: "check_not_satisfied".into(),
                message: "断言失败于 src/a.rs:42".into(),
                target_id: Some("ci".into()),
            }],
            evaluated_version: Some("sha256:abc".into()),
        }],
        overall,
        waived_count: 0,
        evaluated_version: Some("sha256:abc".into()),
        reasons: Vec::new(),
    }
}

fn budget() -> RepairBudget {
    RepairBudget::default()
}

#[test]
fn known_gap_never_becomes_passed() {
    // 假完成夹具：模型说完了、运行终态 Done，但验收没通过 → 绝不能 allow。
    let c = contract();
    let s = snap(AcceptanceStatusKind::Pending);
    let d = gate(Some(&c), Some(&s), &budget(), false, false, true);
    assert!(!d.allows_completion());
    assert_eq!(d.as_str(), "repair");
    assert!(d.gaps().iter().any(|g| g.check_id.as_deref() == Some("ci")));
}

#[test]
fn passed_with_waiver_is_allowed_but_reports_the_waiver() {
    let c = contract();
    let mut s = snap(AcceptanceStatusKind::Passed);
    s.waived_count = 2;
    let d = gate(Some(&c), Some(&s), &budget(), false, false, true);
    assert!(d.allows_completion());
    assert!(matches!(d, CompletionDecision::Allow { ref reason, .. } if reason.contains("2")));
}

#[test]
fn identical_failure_with_new_timestamp_does_not_reset_the_budget() {
    // 换时刻不换预算：指纹不含时刻字段，两轮之间只多出时间戳也必须被判为重复。
    let c = contract();
    let s = snap(AcceptanceStatusKind::Failed);
    let mut g1 = gaps_of(&s);
    refine_gaps_with_contract(&mut g1, &c);
    let mut b = budget();
    b.consume(&g1);
    assert_eq!(b.rounds_used, 1);

    // 同一失败，只是消息里的时刻与耗时变了。
    let mut s2 = s.clone();
    s2.items[0].reasons[0].message =
        "断言失败于 src/a.rs:42（2026-10-17 03:21:44，耗时 1.23s）".into();
    let mut g2 = gaps_of(&s2);
    refine_gaps_with_contract(&mut g2, &c);
    assert_eq!(
        g1[0].fingerprint, g2[0].fingerprint,
        "时刻与耗时不得进入指纹"
    );
    // 第 2 轮仍可（重复上限 1）；第 3 轮按同一指纹拦住。
    b.consume(&g2);
    assert!(!b.can_repair(&g2));
}

#[test]
fn rewording_the_failure_message_does_not_buy_a_new_budget() {
    // 模型换个说法描述同一处失败，指纹不变，预算不重置。
    let a = fingerprint(
        Some("ci"),
        "check_not_satisfied",
        "断言失败于 src/a.rs:42",
        "roots=0;in=1;dep=0",
    );
    let b = fingerprint(
        Some("ci"),
        "check_not_satisfied",
        "expected true, got false (src/a.rs:42)",
        "roots=0;in=1;dep=0",
    );
    assert_eq!(a, b);
}

#[test]
fn a_genuinely_different_location_is_a_different_fingerprint() {
    let a = fingerprint(
        Some("ci"),
        "check_not_satisfied",
        "失败于 src/a.rs:42",
        "roots=0;in=1;dep=0",
    );
    let b = fingerprint(
        Some("ci"),
        "check_not_satisfied",
        "失败于 src/b.rs:42",
        "roots=0;in=1;dep=0",
    );
    assert_ne!(a, b);
}

#[test]
fn rounds_are_capped_and_partial_is_preserved() {
    let c = contract();
    let s = snap(AcceptanceStatusKind::Failed);
    let mut b = budget();
    for _ in 0..MAX_REPAIR_ROUNDS {
        let gaps = gaps_of(&s);
        assert!(b.can_repair(&gaps));
        b.consume(&gaps);
    }
    let d = gate(Some(&c), Some(&s), &b, false, false, true);
    assert_eq!(d.as_str(), "blocked");
    assert!(!d.allows_completion());
    // 到限保留 partial：缺项仍在快照里，界面得以显示"还差什么"。
    assert_eq!(d.gaps().len(), 1);
    assert!(
        matches!(d, CompletionDecision::Blocked { ref reason, .. } if reason.contains("修复轮次已用尽"))
    );
}

#[test]
fn a_stable_fingerprint_may_only_be_repaired_twice_in_total() {
    let s = snap(AcceptanceStatusKind::Failed);
    let gaps = gaps_of(&s);
    let mut b = budget();
    b.consume(&gaps); // 首轮
    assert!(b.can_repair(&gaps), "同一指纹的第一次重复修复仍允许");
    b.consume(&gaps); // 重复修复
    assert!(!b.can_repair(&gaps), "同一指纹不得再开第三轮");
}

#[test]
fn extra_step_budget_is_independent_and_capped() {
    let s = snap(AcceptanceStatusKind::Failed);
    let mut b = budget();
    b.extra_steps_used = MAX_REPAIR_EXTRA_STEPS;
    let c = contract();
    let d = gate(Some(&c), Some(&s), &b, false, false, true);
    assert_eq!(d.as_str(), "blocked");
    assert!(
        matches!(d, CompletionDecision::Blocked { ref reason, .. } if reason.contains("额外 step 已用尽"))
    );
}

#[test]
fn cancellation_wins_over_repair_and_never_dispatches_a_new_run() {
    let c = contract();
    let s = snap(AcceptanceStatusKind::Pending);
    // 预算充足、本可修复——但用户取消优先。
    let d = gate(Some(&c), Some(&s), &budget(), true, false, true);
    assert_eq!(d.as_str(), "cancelled");
    assert!(!d.needs_repair_run());
    assert!(!d.allows_completion());
    // 已发生的事实保留可见，不是丢弃。
    assert!(d.snapshot().is_some());
}

#[test]
fn whole_task_cancellation_also_wins() {
    let c = contract();
    let s = snap(AcceptanceStatusKind::Pending);
    let d = gate(Some(&c), Some(&s), &budget(), false, true, true);
    assert_eq!(d.as_str(), "cancelled");
    assert!(!d.needs_repair_run());
}

#[test]
fn no_contract_keeps_the_original_finish_path() {
    // 旧问答不为每条回答加门。
    let d = gate(None, None, &budget(), false, false, true);
    assert!(d.allows_completion());
    assert_eq!(d.as_str(), "allow");
}

#[test]
fn save_failure_must_never_allow() {
    // 验收快照是 passed，但**事实没能落盘** → 不得 allow：保存失败被伪装成事实。
    let c = contract();
    let s = snap(AcceptanceStatusKind::Passed);
    let d = gate(Some(&c), Some(&s), &budget(), false, false, false);
    assert_eq!(d.as_str(), "blocked");
    assert!(!d.allows_completion());
    assert!(
        matches!(d, CompletionDecision::Blocked { ref reason, .. } if reason.contains("未能落盘"))
    );
}

#[test]
fn missing_snapshot_with_a_contract_is_unknown_not_allowed() {
    let c = contract();
    let d = gate(Some(&c), None, &budget(), false, false, true);
    assert!(!d.allows_completion());
    assert_eq!(d.as_str(), "blocked");
    assert!(
        matches!(d, CompletionDecision::Blocked { snapshot, .. } if snapshot.overall == AcceptanceStatusKind::Blocked)
    );
}

#[test]
fn needing_user_or_environment_blocks_instead_of_looping_the_same_question() {
    let c = contract();
    let mut s = snap(AcceptanceStatusKind::Blocked);
    s.items[0].reasons[0].code = "check_blocked".into();
    s.items[0].reasons[0].message = "缺少数据库连接串，无法运行检查".into();
    // 预算全新也无济于事——问同一问题不会有答案。
    let d = gate(Some(&c), Some(&s), &budget(), false, false, true);
    assert_eq!(d.as_str(), "blocked");
    assert!(!d.needs_repair_run());
    assert!(
        matches!(d, CompletionDecision::Blocked { ref reason, .. } if reason.contains("不自动循环问同一问题"))
    );
}

#[test]
fn waived_and_cancelled_items_are_not_gaps() {
    // 用户明确豁免 / 取消不是"待修复缺项"，否则会拿预算去修一件用户已经决定的事。
    let mut s = snap(AcceptanceStatusKind::Passed);
    s.items[0].status = AcceptanceStatus::Waived;
    s.items[0].reasons[0].code = "user_waived".into();
    assert!(gaps_of(&s).is_empty());

    s.items[0].status = AcceptanceStatus::Cancelled;
    s.items[0].reasons[0].code = "user_cancelled".into();
    assert!(gaps_of(&s).is_empty());
}

#[test]
fn persisted_budget_survives_restart_with_the_same_remaining_room() {
    // 持久化恢复：预算从边车读回后仍剩同样的额度，不因重启归零。
    let s = snap(AcceptanceStatusKind::Failed);
    let gaps = gaps_of(&s);
    let mut b = budget();
    b.consume(&gaps);
    b.consume_steps(5);

    let json = serde_json::to_string(&b).unwrap();
    let restored: RepairBudget = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, b);
    assert_eq!(restored.rounds_used, 1);
    assert_eq!(restored.extra_steps_used, 5);
    // 恢复后仍只够再修一轮。
    assert!(restored.can_repair(&gaps));
    let mut restored = restored;
    restored.consume(&gaps);
    assert!(!restored.can_repair(&gaps));
}

#[test]
fn budget_defaults_are_zero_for_a_legacy_session_without_the_field() {
    // 老会话没有本能力：读回缺字段时按 0 计（显示未验证），不补造已用预算。
    let restored: RepairBudget = serde_json::from_str("{}").unwrap();
    assert_eq!(restored.rounds_used, 0);
    assert_eq!(restored.extra_steps_used, 0);
    assert!(restored.fingerprints.is_empty());
}

#[test]
fn repeated_gate_calls_are_stable_and_consume_nothing() {
    // 同一个请求重复求值：裁决稳定，且**不消耗预算**（消费只发生在真正开修复轮时）。
    let c = contract();
    let s = snap(AcceptanceStatusKind::Failed);
    let b = budget();
    let a = gate(Some(&c), Some(&s), &b, false, false, true);
    let d = gate(Some(&c), Some(&s), &b, false, false, true);
    assert_eq!(a.as_str(), d.as_str());
    assert_eq!(a.gaps().len(), d.gaps().len());
    assert_eq!(b.rounds_used, 0);
}

#[test]
fn repair_feedback_lands_in_data_not_warnings() {
    // 写入后检查的结论必须在 data：写进 warnings 模型一个字都收不到。
    let s = snap(AcceptanceStatusKind::Failed);
    let gaps = gaps_of(&s);
    let b = budget();
    let fb = repair_feedback(&gaps, &b);
    assert_eq!(fb["acceptance"], "blocked");
    assert_eq!(fb["gaps"][0]["check_id"], "ci");
    assert_eq!(fb["repair_rounds_max"], MAX_REPAIR_ROUNDS);
    assert!(fb.get("warnings").is_none());
}

#[test]
fn scope_class_participates_in_the_fingerprint() {
    let c = contract();
    let s = snap(AcceptanceStatusKind::Failed);
    let mut g1 = gaps_of(&s);
    refine_gaps_with_contract(&mut g1, &c);
    // 契约换了检查范围 → 目标范围类别变了 → 指纹随之改变。
    let mut c2 = contract();
    c2.checks[0]
        .scope
        .inputs
        .push(crate::core::delivery::types::PathRef {
            root_id: "ws".into(),
            relative_path: "extra.txt".into(),
        });
    let mut g2 = gaps_of(&s);
    refine_gaps_with_contract(&mut g2, &c2);
    assert_ne!(g1[0].fingerprint, g2[0].fingerprint);
}
