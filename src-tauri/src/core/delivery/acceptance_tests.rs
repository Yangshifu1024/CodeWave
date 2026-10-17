//! #54 逐项验收夹具：同名 id / 悬空引用 / 修订升级使旧证据失效 / 模型 JSON 不生成通过 /
//! 取消只影响指定范围 / 未绑定检查保持 pending / 豁免需真实用户决定。

use super::*;
use crate::core::delivery::version::RootResolver;
use std::collections::BTreeMap;

fn ws() -> RunIdentity {
    RunIdentity::root("sess1", "run1")
}

fn sub() -> RunIdentity {
    RunIdentity {
        session_id: "sess1".into(),
        root_run_id: "run1".into(),
        actor_id: "sub-a".into(),
        actor_run_id: "subrun-a".into(),
        sub_id: Some("sub-a".into()),
    }
}

fn roots(dir: &std::path::Path) -> RootResolver {
    let mut m = BTreeMap::new();
    m.insert("ws".to_string(), dir.to_path_buf());
    m
}

fn root_ident(dir: &std::path::Path) -> RootIdentity {
    RootIdentity {
        root_id: "ws".into(),
        kind: RootKind::Workspace,
        absolute: dir.to_string_lossy().into_owned(),
    }
}

fn p(rel: &str) -> PathRef {
    PathRef {
        root_id: "ws".into(),
        relative_path: rel.into(),
    }
}

fn command_check(id: &str, rel: &str) -> CheckSpec {
    CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: id.into(),
        kind: CheckKind::Command,
        command: Some(CommandSpec::Requested {
            command_text: "true".into(),
            shell_hint: None,
        }),
        scope: VerificationScope {
            roots: Vec::new(),
            inputs: vec![p(rel)],
            outputs: Vec::new(),
            dependency_files: Vec::new(),
            environment_keys: Vec::new(),
        },
        required: true,
        expectation: CheckExpectation::Command {
            allowed_exit_codes: vec![0],
        },
    }
}

fn receipt_of(check_id: &str, exit_code: i32, owner: &RunIdentity) -> ExecutionReceipt {
    ExecutionReceipt {
        binding: Some(crate::core::delivery::types::ContractBinding {
            task_id: "task1".into(),
            revision: 1,
        }),
        schema: SCHEMA,
        id: format!("rcpt_{check_id}"),
        owner: owner.clone(),
        check_id: Some(check_id.into()),
        requested: CommandSpec::Requested {
            command_text: "true".into(),
            shell_hint: None,
        },
        resolved: Some(CommandSpec::Resolved {
            program: "/usr/bin/true".into(),
            argv: vec!["true".into()],
            shell_family: None,
            shell_command: None,
            timeout_ms: 1000,
            config_digest: None,
        }),
        cwd: Some(p(".")),
        started_at: "0".into(),
        finished_at: "1".into(),
        outcome: ExecutionOutcome::Exited,
        exit_code: Some(exit_code),
        signal: None,
        output_ref: None,
        output_truncated: false,
        before: None,
        after: None,
        issuer: ReceiptIssuer::Host,
        reason: None,
    }
}

fn contract_with(req: Requirement, checks: Vec<CheckSpec>) -> TaskContract {
    TaskContract {
        schema: SCHEMA,
        task_id: "task1".into(),
        revision: 1,
        owner: ws(),
        requirements: vec![req],
        artifacts: Vec::new(),
        checks,
        review_policy: ReviewPolicy {
            required: true,
            independent: true,
            dimensions: None,
            blocking_severities: None,
        },
        scope_changes: Vec::new(),
    }
}

fn req(id: &str, check_ids: Vec<&str>) -> Requirement {
    Requirement {
        id: id.into(),
        title: "r".into(),
        required: true,
        check_ids: check_ids.into_iter().map(String::from).collect(),
        artifact_ids: Vec::new(),
    }
}

fn eval<'a>(
    contract: &'a TaskContract,
    ledger: &'a Ledger,
    r: &'a RootResolver,
) -> Result<EvalOutcome, Vec<Reason>> {
    evaluate(&EvalContext {
        contract,
        ledger,
        roots: r,
        capture: crate::core::delivery::version::CaptureOptions::default(),
    })
}

// ---------- 形状校验（创建即拒） ----------

#[test]
fn duplicate_ids_are_rejected() {
    let c = contract_with(
        req("r1", vec!["c1"]),
        vec![command_check("c1", "a.txt"), command_check("c1", "a.txt")],
    );
    let errs = validate_contract_shape(&c).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "duplicate_id"));
}

#[test]
fn dangling_check_reference_is_rejected() {
    let c = contract_with(req("r1", vec!["nope"]), vec![]);
    let errs = validate_contract_shape(&c).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "dangling_check_ref"));
}

#[test]
fn expectation_kind_mismatch_is_rejected() {
    let mut spec = command_check("c1", "a.txt");
    spec.expectation = CheckExpectation::Manual { prompt: None };
    let c = contract_with(req("r1", vec!["c1"]), vec![spec]);
    let errs = validate_contract_shape(&c).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "expectation_kind_mismatch"));
}

#[test]
fn scope_change_without_user_decision_is_rejected() {
    let mut c = contract_with(req("r1", vec![]), vec![]);
    c.scope_changes.push(ScopeChange {
        id: "sc1".into(),
        target_id: "r1".into(),
        kind: ScopeChangeKind::Cancelled,
        reason: "用户说不用做了".into(),
        user_decision_id: None,
        at: "0".into(),
    });
    let errs = validate_contract_shape(&c).unwrap_err();
    assert!(
        errs.iter()
            .any(|e| e.code == "scope_change_without_user_decision")
    );
}

#[test]
fn scale_over_limit_is_rejected_not_truncated() {
    let mut c = contract_with(req("r1", vec![]), vec![]);
    c.requirements = (0..=MAX_REQUIREMENTS)
        .map(|i| req(&format!("r{i}"), vec![]))
        .collect();
    let errs = validate_contract_shape(&c).unwrap_err();
    assert!(errs.iter().any(|e| e.code == "contract_too_large"));
    assert_eq!(c.requirements.len(), MAX_REQUIREMENTS + 1, "不得截断");
}

// ---------- 逐项判定 ----------

#[test]
fn requirement_without_bound_check_stays_pending() {
    // 无自然语言自动证明：没有绑定检查的需求永远是 pending。
    let dir = tempfile::tempdir().unwrap();
    let c = contract_with(req("r1", vec![]), vec![]);
    let ledger = Ledger::new();
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Pending);
    assert!(
        out.items[0]
            .reasons
            .iter()
            .any(|r| r.code == "no_bound_check")
    );
    assert_eq!(out.overall, AcceptanceStatusKind::Pending);
}

#[test]
fn exit_zero_alone_does_not_satisfy_without_scope_version() {
    // 真实退出 0，但没采集范围版本 → 不能证明"内容未变" → blocked。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let mut ledger = Ledger::new();
    ledger
        .receipts
        .insert("rcpt_c1".into(), receipt_of("c1", 0, &ws()));
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Blocked);
    assert!(
        out.items[0]
            .reasons
            .iter()
            .any(|r| r.code == "evidence_version_missing")
    );
}

#[test]
fn nonzero_exit_fails_the_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let mut ledger = Ledger::new();
    let mut r = receipt_of("c1", 7, &ws());
    r.before = Some(
        version::capture_scope(&roots(dir.path()), &c.checks[0].scope, &Default::default())
            .unwrap(),
    );
    r.after = r.before.clone();
    ledger.receipts.insert("rcpt_c1".into(), r);
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Failed);
    assert!(
        out.items[0]
            .reasons
            .iter()
            .any(|x| x.code == "exit_code_not_allowed")
    );
}

#[test]
fn passing_receipt_with_matching_scope_passes_and_stale_after_change() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let rr = roots(dir.path());
    let mut ledger = Ledger::new();
    let mut r = receipt_of("c1", 0, &ws());
    r.before = Some(version::capture_scope(&rr, &c.checks[0].scope, &Default::default()).unwrap());
    r.after = r.before.clone();
    ledger.receipts.insert("rcpt_c1".into(), r);

    let out = eval(&c, &ledger, &rr).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Passed);
    assert_eq!(out.overall, AcceptanceStatusKind::Passed);
    assert_eq!(out.waived_count, 0, "无豁免时不得报豁免数");

    // 相关文件变了 → stale（派生状态，不改原始记录）。
    std::fs::write(dir.path().join("a.txt"), b"changed").unwrap();
    let out2 = eval(&c, &ledger, &rr).unwrap();
    assert_eq!(out2.items[0].status, AcceptanceStatus::Stale);
    assert_eq!(out2.overall, AcceptanceStatusKind::Stale);
    assert!(
        out2.items[0]
            .reasons
            .iter()
            .any(|x| x.code == "evidence_stale")
    );
}

#[test]
fn spawn_failed_without_exit_code_is_failed_not_passed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let mut ledger = Ledger::new();
    let mut r = receipt_of("c1", 0, &ws());
    r.outcome = ExecutionOutcome::SpawnFailed;
    r.exit_code = None;
    ledger.receipts.insert("rcpt_c1".into(), r);
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Failed);
}

#[test]
fn cross_session_receipt_cannot_satisfies_current_task() {
    // 跨会话证据不得给当前任务作证。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let other = RunIdentity::root("sess2", "run1");
    let mut ledger = Ledger::new();
    ledger
        .receipts
        .insert("rcpt_c1".into(), receipt_of("c1", 0, &other));
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Pending);
}

#[test]
fn stale_receipt_from_previous_run_is_rejected() {
    // 迟到结果：属于上一 root_run 的回执不能给本 run 作证。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let prev = RunIdentity::root("sess1", "run0");
    let mut ledger = Ledger::new();
    ledger
        .receipts
        .insert("rcpt_c1".into(), receipt_of("c1", 0, &prev));
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Pending);
}

#[test]
fn model_shaped_passed_json_creates_no_evidence() {
    // 账本里没有任何记录 → 无论报告怎么写都是 pending。
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let mut ledger = Ledger::new();
    ledger.deliveries.insert(
        "dlv1".into(),
        DeliveryResult {
            schema: SCHEMA,
            id: "dlv1".into(),
            contract_revision: 1,
            owner: sub(),
            task_id: "task1".into(),
            ended: EndedKind::Report,
            completed: vec!["r1".into()],
            incomplete: Vec::new(),
            changes: Vec::new(),
            evidence: vec!["c1".into()], // 模型自称有证据
            blockers: Vec::new(),
            submitted_at: "0".into(),
        },
    );
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(
        out.items[0].status,
        AcceptanceStatus::Pending,
        "自称的证据 id 不构成证据"
    );
}

// ---------- 取消 / 豁免 ----------

#[test]
fn user_cancel_only_affects_targeted_scope() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    c.requirements.push(req("r2", vec![]));
    c.scope_changes.push(ScopeChange {
        id: "sc1".into(),
        target_id: "r1".into(),
        kind: ScopeChangeKind::Cancelled,
        reason: "用户取消".into(),
        user_decision_id: Some("dec1".into()),
        at: "0".into(),
    });
    let mut ledger = Ledger::new();
    let scope = crate::core::delivery::service::target_scope(&c, "r1").unwrap();
    let version = version::capture_scope(&roots(dir.path()), &scope, &Default::default()).unwrap();
    ledger.decisions.insert(
        "dec1".into(),
        UserDecision {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            id: "dec1".into(),
            owner: ws(),
            item_ids: vec!["r1".into()],
            decision: UserDecisionKind::Cancelled,
            scope_version: Some(version.digest),
            prompt_id: "real_ask".into(),
            answered_at: "0".into(),
        },
    );
    c.revision = 2;
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Cancelled);
    assert_eq!(
        out.items[1].status,
        AcceptanceStatus::Pending,
        "未取消的需求不受影响"
    );
    assert_eq!(
        c.requirements.len(),
        2,
        "取消只减少有效集，原始需求记录保留"
    );
}

#[test]
fn waiver_without_real_user_decision_does_not_count() {
    // 模型自称豁免无效：没有 UserDecision 记录就不算 waived。
    let dir = tempfile::tempdir().unwrap();
    let mut c = contract_with(req("r1", vec![]), vec![]);
    c.scope_changes.push(ScopeChange {
        id: "sc1".into(),
        target_id: "r1".into(),
        kind: ScopeChangeKind::Waived,
        reason: "模型自称豁免".into(),
        user_decision_id: Some("made-up".into()),
        at: "0".into(),
    });
    let out = eval(&c, &Ledger::new(), &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Blocked);
    assert_eq!(out.waived_count, 0);
    assert!(
        out.reasons
            .iter()
            .any(|r| r.code == "waiver_without_user_decision")
    );
}

#[test]
fn real_user_waiver_is_counted_and_visible() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = contract_with(req("r1", vec![]), vec![]);
    c.scope_changes.push(ScopeChange {
        id: "sc1".into(),
        target_id: "r1".into(),
        kind: ScopeChangeKind::Waived,
        reason: "用户明确豁免".into(),
        user_decision_id: Some("dec1".into()),
        at: "0".into(),
    });
    let mut ledger = Ledger::new();
    ledger.decisions.insert(
        "dec1".into(),
        UserDecision {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            id: "dec1".into(),
            owner: ws(),
            item_ids: vec!["r1".into()],
            decision: UserDecisionKind::Waived,
            scope_version: Some(
                version::capture_scope(
                    &RootResolver::new(),
                    &VerificationScope::default(),
                    &Default::default(),
                )
                .unwrap()
                .digest,
            ),
            prompt_id: "ask1".into(),
            answered_at: "0".into(),
        },
    );
    c.revision = 2;
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Waived);
    assert_eq!(out.waived_count, 1);
    // 有合法豁免时整体可以 passed，但必须带上豁免计数，摘要不得写"全部验证通过"。
    assert_eq!(out.overall, AcceptanceStatusKind::Passed);
    assert_eq!(out.waived_count, 1);
}

#[test]
fn manual_check_without_user_decision_is_blocked_not_passed() {
    let dir = tempfile::tempdir().unwrap();
    let spec = CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: "m1".into(),
        kind: CheckKind::Manual,
        command: None,
        scope: VerificationScope::default(),
        required: true,
        expectation: CheckExpectation::Manual { prompt: None },
    };
    let c = contract_with(req("r1", vec!["m1"]), vec![spec]);
    let out = eval(&c, &Ledger::new(), &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Blocked);
    assert_eq!(out.overall, AcceptanceStatusKind::Blocked);
}

#[test]
fn manual_check_with_real_confirmation_passes() {
    let dir = tempfile::tempdir().unwrap();
    let spec = CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: "m1".into(),
        kind: CheckKind::Manual,
        command: None,
        scope: VerificationScope::default(),
        required: true,
        expectation: CheckExpectation::Manual { prompt: None },
    };
    let c = contract_with(req("r1", vec!["m1"]), vec![spec]);
    let mut ledger = Ledger::new();
    ledger.decisions.insert(
        "dec1".into(),
        UserDecision {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            id: "dec1".into(),
            owner: ws(),
            item_ids: vec!["m1".into()],
            decision: UserDecisionKind::Confirmed,
            scope_version: Some(
                version::capture_scope(
                    &RootResolver::new(),
                    &VerificationScope::default(),
                    &Default::default(),
                )
                .unwrap()
                .digest,
            ),
            prompt_id: "ask1".into(),
            answered_at: "0".into(),
        },
    );
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Passed);
}

// ---------- 独立审查（#10 的判定落点） ----------

fn review_of(reviewer: &RunIdentity, revision: u64, approved: bool) -> ReviewResult {
    ReviewResult {
        schema: SCHEMA,
        id: format!("rev_{}", reviewer.actor_id),
        owner: reviewer.clone(),
        reviewer: reviewer.clone(),
        task_id: "task1".into(),
        check_id: Some("rv".into()),
        contract_revision: revision,
        dimensions: REVIEW_DIMENSIONS
            .iter()
            .map(|d| ReviewDimensionResult {
                state: Some(crate::core::delivery::responsibility::DimensionState::Checked),
                reason: None,
                review_findings: vec![],
                dimension: (*d).into(),
                findings: Vec::new(),
            })
            .collect(),
        verdict: ReviewVerdict {
            approved,
            blocking_count: if approved { 0 } else { 1 },
        },
        scope_version: Some(
            version::capture_scope(
                &RootResolver::new(),
                &VerificationScope::default(),
                &Default::default(),
            )
            .unwrap()
            .digest,
        ),
        reviewed_at: "0".into(),
    }
}

#[test]
fn review_by_same_actor_is_not_accepted_as_independent() {
    let dir = tempfile::tempdir().unwrap();
    let spec = CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: "rv".into(),
        kind: CheckKind::Review,
        command: None,
        scope: VerificationScope::default(),
        required: true,
        expectation: CheckExpectation::Review { dimensions: None },
    };
    let c = contract_with(req("r1", vec!["rv"]), vec![spec]);
    let mut ledger = Ledger::new();
    // 同 actor 自审：宿主拒绝采纳。
    ledger
        .reviews
        .insert("rev_root".into(), review_of(&ws(), 1, true));
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Failed);
    assert!(
        out.items[0]
            .reasons
            .iter()
            .any(|x| x.code == "review_not_independent")
    );
}

#[test]
fn review_by_independent_actor_with_all_dimensions_passes() {
    let dir = tempfile::tempdir().unwrap();
    let spec = CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: "rv".into(),
        kind: CheckKind::Review,
        command: None,
        scope: VerificationScope::default(),
        required: true,
        expectation: CheckExpectation::Review { dimensions: None },
    };
    let c = contract_with(req("r1", vec!["rv"]), vec![spec]);
    let mut ledger = Ledger::new();
    ledger
        .reviews
        .insert("rev_sub".into(), review_of(&sub(), 1, true));
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Passed);
}

#[test]
fn review_against_old_contract_revision_is_not_accepted() {
    // 契约修订升级后旧审查不再适用。
    let dir = tempfile::tempdir().unwrap();
    let spec = CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: "rv".into(),
        kind: CheckKind::Review,
        command: None,
        scope: VerificationScope::default(),
        required: true,
        expectation: CheckExpectation::Review { dimensions: None },
    };
    let mut c = contract_with(req("r1", vec!["rv"]), vec![spec]);
    c.revision = 2;
    let mut ledger = Ledger::new();
    ledger
        .reviews
        .insert("rev_sub".into(), review_of(&sub(), 1, true));
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Pending);
}

#[test]
fn review_missing_a_dimension_fails_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let spec = CheckSpec {
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        independent_verifier: false,
        id: "rv".into(),
        kind: CheckKind::Review,
        command: None,
        scope: VerificationScope::default(),
        required: true,
        expectation: CheckExpectation::Review {
            dimensions: Some(vec!["correctness".into(), "security".into()]),
        },
    };
    let c = contract_with(req("r1", vec!["rv"]), vec![spec]);
    let mut ledger = Ledger::new();
    let mut rv = review_of(&sub(), 1, true);
    rv.dimensions.retain(|d| d.dimension != "security");
    ledger.reviews.insert("rev_sub".into(), rv);
    let out = eval(&c, &ledger, &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Failed);
    let r = out.items[0]
        .reasons
        .iter()
        .find(|r| r.code == "review_incomplete_dimensions")
        .unwrap();
    assert!(r.message.contains("security"));
}

// ---------- 产物 ----------

#[test]
fn directory_masquerading_as_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("out.txt")).unwrap();
    let c = contract_with(req("r1", vec![]), vec![]);
    let mut c = c;
    c.artifacts.push(ArtifactRequirement {
        id: "a1".into(),
        path: p("out.txt"),
        kind: ArtifactKind::Text,
        constraints: Vec::new(),
        required: true,
    });
    c.requirements[0].artifact_ids = vec!["a1".into()];
    let out = eval(&c, &Ledger::new(), &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Failed);
    assert!(
        out.items[0]
            .reasons
            .iter()
            .any(|x| x.code == "artifact_kind_mismatch")
    );
}

#[test]
fn overall_reduction_priority_is_cancelled_blocked_failed_stale_pending_passed() {
    let mk = |s: AcceptanceStatus| AcceptanceItem {
        requirement_id: "r".into(),
        status: s,
        evidence_refs: Vec::new(),
        reasons: Vec::new(),
        evaluated_version: None,
    };
    assert_eq!(
        AcceptanceSnapshot::reduce_overall(
            &[mk(AcceptanceStatus::Passed), mk(AcceptanceStatus::Failed)],
            false
        ),
        AcceptanceStatusKind::Failed
    );
    assert_eq!(
        AcceptanceSnapshot::reduce_overall(
            &[mk(AcceptanceStatus::Stale), mk(AcceptanceStatus::Pending)],
            false
        ),
        AcceptanceStatusKind::Stale
    );
    assert_eq!(
        AcceptanceSnapshot::reduce_overall(
            &[mk(AcceptanceStatus::Blocked), mk(AcceptanceStatus::Failed)],
            false
        ),
        AcceptanceStatusKind::Blocked
    );
    assert_eq!(
        AcceptanceSnapshot::reduce_overall(
            &[
                mk(AcceptanceStatus::Cancelled),
                mk(AcceptanceStatus::Failed)
            ],
            true
        ),
        AcceptanceStatusKind::Cancelled
    );
    // 已取消项不参与归约。
    assert_eq!(
        AcceptanceSnapshot::reduce_overall(
            &[
                mk(AcceptanceStatus::Cancelled),
                mk(AcceptanceStatus::Passed)
            ],
            false
        ),
        AcceptanceStatusKind::Passed
    );
}

#[test]
fn missing_or_old_binding_never_satisfies_identical_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"same").unwrap();
    let mut c = contract_with(req("r1", vec!["c1"]), vec![command_check("c1", "a.txt")]);
    let version =
        version::capture_scope(&roots(dir.path()), &c.checks[0].scope, &Default::default())
            .unwrap();
    let mut r = receipt_of("c1", 0, &ws());
    r.before = Some(version.clone());
    r.after = Some(version);
    let mut ledger = Ledger::new();
    ledger.receipts.insert(r.id.clone(), r.clone());
    assert_eq!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    c.revision = 2;
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    c.revision = 1;
    c.task_id = "different-task".into();
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    c.task_id = "task1".into();
    r.binding = None;
    ledger.receipts.insert(r.id.clone(), r);
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
}

#[test]
fn structured_evidence_requires_current_task_and_revision() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"unchanged").unwrap();
    let mut check = command_check("c1", "a.txt");
    check.kind = CheckKind::Structured;
    check.command = None;
    check.expectation = CheckExpectation::Structured {
        validator_id: "host_probe".into(),
        validator_version: Some("1".into()),
    };
    let mut c = contract_with(req("r1", vec!["c1"]), vec![check]);
    let version =
        version::capture_scope(&roots(dir.path()), &c.checks[0].scope, &Default::default())
            .unwrap();
    let mut ledger = Ledger::new();
    ledger.validations.insert(
        "validation".into(),
        BuiltinValidationRecord {
            binding: Some(crate::core::delivery::types::ContractBinding::of(&c)),
            id: "validation".into(),
            owner: ws(),
            validator_id: "host_probe".into(),
            validator_version: "1".into(),
            check_id: Some("c1".into()),
            input_refs: vec![],
            scope_version: Some(version.digest),
            started_at: "0".into(),
            finished_at: "1".into(),
            outcome: ValidationOutcome::Passed,
            findings: vec![],
            output_ref: None,
        },
    );
    assert_eq!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    c.task_id = "second".into();
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    c.task_id = "task1".into();
    c.revision = 2;
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    c.revision = 1;
    ledger.validations.get_mut("validation").unwrap().binding = None;
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
}

#[test]
fn structured_artifact_requires_real_matching_current_validation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"validated").unwrap();
    let mut check = command_check("c1", "a.txt");
    check.kind = CheckKind::Structured;
    check.command = None;
    check.expectation = CheckExpectation::Structured {
        validator_id: "host_probe".into(),
        validator_version: Some("1".into()),
    };
    let mut c = contract_with(req("r1", vec!["c1"]), vec![check]);
    c.artifacts.push(ArtifactRequirement {
        id: "a1".into(),
        path: p("a.txt"),
        kind: ArtifactKind::Text,
        constraints: vec![ArtifactConstraint::StructuredCheck {
            validator_id: "host_probe".into(),
            validator_version: Some("1".into()),
        }],
        required: true,
    });
    c.requirements[0].artifact_ids.push("a1".into());
    let rs = roots(dir.path());
    let current = version::capture_scope(&rs, &c.checks[0].scope, &Default::default()).unwrap();
    let mut ledger = Ledger::new();
    assert_ne!(
        eval(&c, &ledger, &rs).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    ledger.validations.insert(
        "v1".into(),
        BuiltinValidationRecord {
            binding: Some(ContractBinding::of(&c)),
            id: "v1".into(),
            owner: ws(),
            validator_id: "host_probe".into(),
            validator_version: "1".into(),
            check_id: Some("c1".into()),
            input_refs: vec![],
            scope_version: Some(current.digest),
            started_at: "0".into(),
            finished_at: "1".into(),
            outcome: ValidationOutcome::Passed,
            findings: vec![],
            output_ref: None,
        },
    );
    assert_eq!(
        eval(&c, &ledger, &rs).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    ledger.validations.get_mut("v1").unwrap().validator_version = "2".into();
    assert_ne!(
        eval(&c, &ledger, &rs).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    ledger.validations.get_mut("v1").unwrap().validator_version = "1".into();
    std::fs::write(dir.path().join("a.txt"), b"changed").unwrap();
    assert_ne!(
        eval(&c, &ledger, &rs).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
}

#[test]
fn artifact_only_keeps_pending_until_explicit_check_exists() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"exists").unwrap();
    let mut c = contract_with(req("r1", vec![]), vec![]);
    c.artifacts.push(ArtifactRequirement {
        id: "a1".into(),
        path: p("a.txt"),
        kind: ArtifactKind::Text,
        constraints: vec![ArtifactConstraint::Exists],
        required: true,
    });
    c.requirements[0].artifact_ids.push("a1".into());
    let out = eval(&c, &Ledger::new(), &roots(dir.path())).unwrap();
    assert_eq!(out.items[0].status, AcceptanceStatus::Pending);
    assert!(out.items[0].reasons[0].message.contains("显式检查"));
    std::fs::remove_file(dir.path().join("a.txt")).unwrap();
    assert_eq!(
        eval(&c, &Ledger::new(), &roots(dir.path())).unwrap().items[0].status,
        AcceptanceStatus::Failed
    );
}

#[test]
fn review_policy_dimensions_and_severities_are_enforced_during_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let mut check = command_check("rv", "a.txt");
    check.kind = CheckKind::Review;
    check.command = None;
    check.scope = VerificationScope::default();
    check.expectation = CheckExpectation::Review { dimensions: None };
    let mut c = contract_with(req("r1", vec!["rv"]), vec![check]);
    c.review_policy.dimensions = Some(vec!["security".into(), "licensing".into()]);
    c.review_policy.blocking_severities = Some(vec![FindingSeverity::Warning]);
    let mut review = review_of(&sub(), 1, true);
    let mut ledger = Ledger::new();
    ledger.reviews.insert(review.id.clone(), review.clone());
    assert_eq!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().items[0].status,
        AcceptanceStatus::Failed,
        "policy extra dimension must be reviewed"
    );
    review.dimensions.push(ReviewDimensionResult {
        dimension: "licensing".into(),
        state: Some(super::super::responsibility::DimensionState::Checked),
        reason: None,
        findings: vec![],
        review_findings: vec![],
    });
    review.dimensions[0]
        .review_findings
        .push(super::super::responsibility::ReviewFinding {
            stable_id: "warning-1".into(),
            severity: FindingSeverity::Warning,
            path: None,
            line_range: None,
            message: "policy blocking warning".into(),
            evidence: vec![],
            needs_human_check: true,
        });
    ledger.reviews.insert(review.id.clone(), review.clone());
    assert_eq!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().items[0].status,
        AcceptanceStatus::Failed,
        "warning is blocking under policy"
    );
    c.review_policy.blocking_severities = None;
    ledger.reviews.insert(review.id.clone(), review);
    assert_eq!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().items[0].status,
        AcceptanceStatus::Passed
    );
}

#[test]
fn empty_or_dangling_artifact_expectation_is_rejected_before_evaluation() {
    let mut check = command_check("artifact", "a.txt");
    check.kind = CheckKind::Artifact;
    check.command = None;
    check.expectation = CheckExpectation::Artifact {
        artifact_ids: vec![],
    };
    let mut c = contract_with(req("r1", vec!["artifact"]), vec![check]);
    assert!(
        validate_contract_shape(&c)
            .unwrap_err()
            .iter()
            .any(|r| r.code == "artifact_check_empty")
    );
    c.checks[0].expectation = CheckExpectation::Artifact {
        artifact_ids: vec!["unknown".into()],
    };
    assert!(
        validate_contract_shape(&c)
            .unwrap_err()
            .iter()
            .any(|r| r.code == "dangling_artifact_ref")
    );
}

#[test]
fn independent_review_checks_select_their_own_current_scope() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let mut a = command_check("review-a", "a.txt");
    a.kind = CheckKind::Review;
    a.command = None;
    a.expectation = CheckExpectation::Review { dimensions: None };
    let mut b = a.clone();
    b.id = "review-b".into();
    b.scope.inputs = vec![p("b.txt")];
    let c = contract_with(req("r1", vec!["review-a", "review-b"]), vec![a, b]);
    let rs = roots(dir.path());
    let mut ledger = Ledger::new();
    for (index, check) in c.checks.iter().enumerate() {
        let mut review = review_of(&sub(), 1, true);
        review.id = check.id.clone();
        review.check_id = Some(check.id.clone());
        review.reviewed_at = index.to_string();
        review.scope_version = Some(
            version::capture_scope(&rs, &check.scope, &Default::default())
                .unwrap()
                .digest,
        );
        ledger.reviews.insert(review.id.clone(), review);
    }
    assert_eq!(
        eval(&c, &ledger, &rs).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    std::fs::write(dir.path().join("a.txt"), b"changed").unwrap();
    assert_ne!(
        eval(&c, &ledger, &rs).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
}

#[test]
fn review_checks_with_identical_scope_keep_distinct_check_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = command_check("review-a", "a.txt");
    a.kind = CheckKind::Review;
    a.command = None;
    a.scope = VerificationScope::default();
    a.expectation = CheckExpectation::Review {
        dimensions: Some(vec!["security".into()]),
    };
    let mut b = a.clone();
    b.id = "review-b".into();
    b.expectation = CheckExpectation::Review {
        dimensions: Some(vec!["performance".into()]),
    };
    let c = contract_with(req("r1", vec!["review-a", "review-b"]), vec![a, b]);
    let mut ledger = Ledger::new();
    for (index, check) in c.checks.iter().enumerate() {
        let mut review = review_of(&sub(), 1, true);
        review.id = check.id.clone();
        review.check_id = Some(check.id.clone());
        review.reviewed_at = index.to_string();
        let CheckExpectation::Review {
            dimensions: Some(dimensions),
        } = &check.expectation
        else {
            unreachable!()
        };
        review
            .dimensions
            .retain(|dimension| dimensions.contains(&dimension.dimension));
        ledger.reviews.insert(review.id.clone(), review);
    }
    assert_eq!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed
    );
    ledger.reviews.get_mut("review-a").unwrap().check_id = None;
    assert_ne!(
        eval(&c, &ledger, &roots(dir.path())).unwrap().overall,
        AcceptanceStatusKind::Passed,
        "legacy unbound review cannot satisfy a check"
    );
}
