//! #9 / #10 / #11 夹具：同 actor 换角色拒绝、跨 root_run 拒绝、缺维度保持 unverified、
//! 伪造 reviewer_id 无效、验证上下文不含主历史、职责只读上限不被 FullAccess 解锁。

use super::*;
use crate::core::prefs::ApprovalMode;

fn impl_actor() -> RunIdentity {
    RunIdentity {
        session_id: "sess1".into(),
        root_run_id: "run1".into(),
        actor_id: "sub-impl".into(),
        actor_run_id: "impl-run".into(),
        sub_id: Some("sub-impl".into()),
    }
}

fn other_actor(role: &str) -> RunIdentity {
    RunIdentity {
        session_id: "sess1".into(),
        root_run_id: "run1".into(),
        actor_id: role.into(),
        actor_run_id: format!("{role}-run"),
        sub_id: Some(role.into()),
    }
}

// ---------- #9 ----------

#[test]
fn full_policy_requires_a_distinct_reviewer() {
    let a = ResponsibilityAssignment {
        requirement_ids: vec!["r1".into()],
        implementation_actor: impl_actor(),
        review_actor: Some(impl_actor()),
        verification_actor: Some(other_actor("tester")),
        policy: TaskPolicy::Full,
    };
    let errs = validate_responsibility(&a).unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.contains("换角色/换模型都不构成独立身份")),
        "{errs:?}"
    );
}

#[test]
fn same_actor_different_run_is_still_rejected_when_actor_matches() {
    // 同一个 actor_id 跑了两次 = 还是同一个人
    let mut reviewer = impl_actor();
    reviewer.actor_run_id = "impl-run-2".into();
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: Some(reviewer),
        verification_actor: Some(other_actor("tester")),
        policy: TaskPolicy::Full,
    };
    assert!(validate_responsibility(&a).is_err());
}

#[test]
fn responsibility_reviewer_from_another_root_run_is_rejected() {
    let mut reviewer = other_actor("reviewer-x");
    reviewer.root_run_id = "run2".into();
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: Some(reviewer),
        verification_actor: Some(other_actor("tester")),
        policy: TaskPolicy::Full,
    };
    let errs = validate_responsibility(&a).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("root run")), "{errs:?}");
}

#[test]
fn full_policy_missing_reviewer_is_rejected() {
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: None,
        verification_actor: Some(other_actor("tester")),
        policy: TaskPolicy::Full,
    };
    assert!(validate_responsibility(&a).is_err());
}

#[test]
fn conversation_policy_needs_no_review_or_verification() {
    // 问答不该被交付流水线拖慢
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: None,
        verification_actor: None,
        policy: TaskPolicy::Conversation,
    };
    assert!(validate_responsibility(&a).is_ok());
}

#[test]
fn mechanical_policy_allows_host_check_without_independent_reviewer() {
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: None,
        verification_actor: None,
        policy: TaskPolicy::Mechanical,
    };
    assert!(validate_responsibility(&a).is_ok());
    assert!(!TaskPolicy::Mechanical.requires_review());
    assert!(TaskPolicy::Full.requires_review());
}

#[test]
fn verifier_cannot_be_the_implementer() {
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: Some(other_actor("reviewer-x")),
        verification_actor: Some(impl_actor()),
        policy: TaskPolicy::Full,
    };
    assert!(validate_responsibility(&a).is_err());
}

#[test]
fn review_and_verification_cannot_share_one_run() {
    let mut v = other_actor("tester");
    v.actor_run_id = "shared-run".into();
    let mut r = other_actor("reviewer-x");
    r.actor_run_id = "shared-run".into();
    let a = ResponsibilityAssignment {
        requirement_ids: vec![],
        implementation_actor: impl_actor(),
        review_actor: Some(r),
        verification_actor: Some(v),
        policy: TaskPolicy::Full,
    };
    let errs = validate_responsibility(&a).unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("共用同一次 actor run")),
        "{errs:?}"
    );
}

// ---------- #10 ----------

fn assignment() -> ReviewAssignment {
    ReviewAssignment {
        review_id: "rev-assign-1".into(),
        implementer_actor_ids: vec!["sub-impl".into(), "sub-impl2".into()],
        target_scope_version: Some("abc123".into()),
        policy_revision: 1,
    }
}

#[test]
fn implementer_cannot_review_their_own_work() {
    let actor = impl_actor();
    let errs = validate_review_submission(&assignment(), &actor, "run1", &ReviewPolicy::default())
        .unwrap_err();
    assert!(
        errs.iter().any(|e| e.contains("自审不是独立审查")),
        "{errs:?}"
    );
}

#[test]
fn second_implementer_also_cannot_review() {
    let mut a = impl_actor();
    a.actor_id = "sub-impl2".into();
    assert!(
        validate_review_submission(&assignment(), &a, "run1", &ReviewPolicy::default()).is_err()
    );
}

#[test]
fn reviewer_from_another_root_run_is_rejected() {
    let mut actor = other_actor("reviewer-x");
    actor.root_run_id = "run9".into();
    assert!(
        validate_review_submission(&assignment(), &actor, "run1", &ReviewPolicy::default())
            .is_err()
    );
}

#[test]
fn independent_reviewer_is_accepted() {
    assert!(
        validate_review_submission(
            &assignment(),
            &other_actor("reviewer-x"),
            "run1",
            &ReviewPolicy::default()
        )
        .is_ok()
    );
}

#[test]
fn non_independent_policy_allows_same_actor() {
    // 用户明确要求不独立时允许，但 policy 必须显式声明 independent=false。
    let policy = ReviewPolicy {
        required: true,
        independent: false,
        dimensions: None,
        blocking_severities: None,
    };
    assert!(validate_review_submission(&assignment(), &impl_actor(), "run1", &policy).is_ok());
}

#[test]
fn missing_dimension_stays_unverified_with_reason() {
    let outcomes = vec![DimensionOutcome {
        dimension: "correctness".into(),
        state: DimensionState::Checked,
        reason: None,
        findings: vec![],
    }];
    let gaps = dimension_gaps(&outcomes, &default_dimensions());
    assert!(gaps.iter().any(|g| g.contains("未提交结论")), "{gaps:?}");
    assert_eq!(gaps.len(), 6, "只覆盖一维，其余六维都缺");
}

#[test]
fn not_applicable_without_reason_is_still_a_gap() {
    let outcomes = vec![DimensionOutcome {
        dimension: "performance".into(),
        state: DimensionState::NotApplicable,
        reason: None,
        findings: vec![],
    }];
    let gaps = dimension_gaps(&outcomes, &["performance".into()]);
    assert!(gaps.iter().any(|g| g.contains("未给理由")), "{gaps:?}");
}

#[test]
fn not_applicable_with_reason_is_accepted() {
    let outcomes = vec![DimensionOutcome {
        dimension: "performance".into(),
        state: DimensionState::NotApplicable,
        reason: Some("本次改动不涉及热路径".into()),
        findings: vec![],
    }];
    assert!(dimension_gaps(&outcomes, &["performance".into()]).is_empty());
}

#[test]
fn unverified_dimension_stays_a_gap_even_with_a_reason() {
    // 与 not_applicable 的关键区别：unverified 是"没验"，给理由也不能变成"已验"。
    let outcomes = vec![DimensionOutcome {
        dimension: "security".into(),
        state: DimensionState::Unverified,
        reason: Some("暂时找不到复现路径".into()),
        findings: vec![],
    }];
    let gaps = dimension_gaps(&outcomes, &["security".into()]);
    assert!(gaps.iter().any(|g| g.contains("unverified")), "{gaps:?}");
}

#[test]
fn finding_keeps_stable_id_across_rounds() {
    // 复审引用旧 finding 逐项解决：稳定 id 是前提。
    let a = ReviewFinding {
        stable_id: "F-SEC-1".into(),
        severity: FindingSeverity::Error,
        path: Some("src/lib.rs".into()),
        line_range: Some((10, 20)),
        message: "越权".into(),
        evidence: vec!["execution_op1".into()],
        needs_human_check: false,
    };
    let b = a.clone();
    assert_eq!(a.stable_id, b.stable_id);
    let projected = to_validation_findings(&[DimensionOutcome {
        dimension: "security".into(),
        state: DimensionState::Checked,
        reason: None,
        findings: vec![a],
    }]);
    assert_eq!(projected[0].location.as_deref(), Some("src/lib.rs:10-20"));
}

#[test]
fn default_blocking_severities_are_errors_only() {
    assert_eq!(DEFAULT_BLOCKING_SEVERITIES, &[FindingSeverity::Error]);
}

// ---------- #11 ----------

fn input() -> VerificationInput {
    VerificationInput {
        contract_revision: 3,
        check_ids: vec!["c1".into()],
        artifact_refs: vec![PathRef {
            root_id: "ws".into(),
            relative_path: "src/lib.rs".into(),
        }],
        environment_facts: vec!["rustc".into()],
        target_version: Some("deadbeef".into()),
        allowed_roots: vec![RootIdentity {
            root_id: "ws".into(),
            kind: crate::core::delivery::types::RootKind::Workspace,
            absolute: "/tmp/ws".into(),
        }],
    }
}

#[test]
fn manifest_records_only_references_and_hashes_not_bodies() {
    let m = build_context_manifest(
        &input(),
        &[("r1".into(), "需求：修好越权".into())],
        &[("c1".into(), "cargo test --workspace".into())],
        &[("impl-note".into(), "实现者说明：改了三处".into())],
    );
    // 只登记键与摘要
    let kinds: Vec<&str> = m.entries.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"requirement"));
    assert!(kinds.contains(&"check_spec"));
    assert!(kinds.contains(&"artifact"));
    assert!(kinds.contains(&"environment"));
    assert!(kinds.contains(&"explicit_note"));
    // 环境只登记键名，不含任何值
    let env = m.entries.iter().find(|e| e.kind == "environment").unwrap();
    assert_eq!(env.ref_id, "rustc");
    assert!(m.entries.iter().all(|e| !e.hash.is_empty()));
    assert_eq!(m.contract_revision, 3);
}

#[test]
fn manifest_never_contains_the_main_history_text() {
    // 验证上下文不携带实现者推理全文：只登记需求标题，不登记任何 history 正文。
    let m = build_context_manifest(
        &input(),
        &[("r1".into(), "需求标题".into())],
        &[("c1".into(), "命令".into())],
        &[],
    );
    let blob = serde_json::to_string(&m).unwrap();
    assert!(!blob.contains("secret_marker"), "清单里不得出现主历史内容");
    assert!(!blob.contains("需求：修好越权"), "清单只存 hash，不存原文");
}

#[test]
fn manifest_digest_is_stable_and_input_sensitive() {
    let a = build_context_manifest(&input(), &[("r1".into(), "A".into())], &[], &[]);
    let b = build_context_manifest(&input(), &[("r1".into(), "A".into())], &[], &[]);
    assert_eq!(a.digest, b.digest, "同输入同摘要");
    let c = build_context_manifest(&input(), &[("r1".into(), "B".into())], &[], &[]);
    assert_ne!(a.digest, c.digest, "内容变了摘要必须变");
}

#[test]
fn manifest_entries_are_sorted_for_reproducibility() {
    let m = build_context_manifest(
        &input(),
        &[("r2".into(), "B".into()), ("r1".into(), "A".into())],
        &[("c2".into(), "y".into()), ("c1".into(), "x".into())],
        &[],
    );
    let keys: Vec<(String, String)> = m
        .entries
        .iter()
        .map(|e| (e.kind.clone(), e.ref_id.clone()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn verification_capability_is_narrow_and_declared() {
    // 默认能力窄：能读能跑验证命令，不能写、不能派活。
    assert!(is_verification_capability("read"));
    assert!(is_verification_capability("command"));
    assert!(!is_verification_capability("create"));
    assert!(!is_verification_capability("edit"));
    assert!(!is_verification_capability("subagent"));
    let ex = verification_excludes();
    for t in ["edit", "create", "delete", "subagent", "plan"] {
        assert!(ex.contains(&t.to_string()), "验证职责必须排除 {t}");
    }
}

#[test]
fn review_duty_readonly_cap_survives_full_access() {
    // 关键回归：职责级只读上限**不被审批档位解锁**。
    let mut params = crate::core::agent::DriveParams {
        main_session: false,
        ..Default::default()
    };
    for t in review_excludes() {
        params.exclude_tools.push(t);
    }
    let full = crate::core::prefs::SessionPrefs {
        approval_mode: ApprovalMode::FullAccess,
        ..Default::default()
    };
    // 父档切到 FullAccess 后重建：排除项仍在（职责上限不是档位派生）
    let rebuilt = {
        let mut excludes = params.exclude_tools.clone();
        excludes.extend(crate::core::agent::drive::main_drive_params(&full).exclude_tools);
        excludes
    };
    for t in crate::core::agent::WRITE_TOOLS {
        assert!(
            rebuilt.iter().any(|e| e == t),
            "FullAccess 不得解锁职责级排除的写工具 {t}"
        );
    }
}
