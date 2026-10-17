//! #18 子交付宿主校验夹具。
//!
//! 覆盖任务文档列出的七类场景：缺字段 / 未知需求 / 虚构回执拒绝、同时 completed 与
//! incomplete 拒绝、发现高危问题却标全部完成拒绝、仅 exit0 不满足产物检查、
//! partial 不能完整通过、重复校验不改变结论、迟到旧 run 拒绝。
//!
//! 断言的是**具体 reason code 与 item_id**，不是"返回了 false"——父任务与 UI 都要靠
//! 这些码定位到具体哪一项出了问题。

use super::*;
use crate::core::delivery::acceptance::Ledger;
use crate::core::delivery::artifacts::{self};
use crate::core::delivery::version::{CaptureOptions, capture_scope};
use std::collections::BTreeMap;

struct Fx {
    _dir: tempfile::TempDir,
    roots: RootResolver,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let mut roots = BTreeMap::new();
    roots.insert("ws".to_string(), dir.path().to_path_buf());
    Fx { _dir: dir, roots }
}

fn root() -> RunIdentity {
    RunIdentity::root("sess1", "run1")
}

fn sub() -> RunIdentity {
    RunIdentity {
        session_id: "sess1".into(),
        root_run_id: "run1".into(),
        actor_id: "sub-a".into(),
        actor_run_id: "sub-a-run".into(),
        sub_id: Some("sub-a".into()),
    }
}

fn p(rel: &str) -> PathRef {
    PathRef {
        root_id: "ws".into(),
        relative_path: rel.into(),
    }
}

fn check(id: &str, rel: &str) -> CheckSpec {
    CheckSpec {
        platforms: vec![],
        prerequisites: vec![],
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

/// 只有一条必需需求、绑定命令检查的最小契约。
fn contract(req_ids: &[&str], checks: Vec<CheckSpec>) -> TaskContract {
    TaskContract {
        delivery_goal: Default::default(),
        schema: SCHEMA,
        task_id: "task1".into(),
        revision: 1,
        owner: root(),
        requirements: req_ids
            .iter()
            .map(|id| Requirement {
                id: (*id).to_string(),
                title: format!("需求 {id}"),
                required: true,
                check_ids: checks.iter().map(|c| c.id.clone()).collect(),
                artifact_ids: Vec::new(),
            })
            .collect(),
        artifacts: Vec::new(),
        checks,
        review_policy: ReviewPolicy::default(),
        scope_changes: Vec::new(),
    }
}

fn delivery(contract: &TaskContract, completed: Vec<&str>, ended: EndedKind) -> DeliveryResult {
    DeliveryResult {
        schema: SCHEMA,
        id: "dlv1".into(),
        contract_revision: contract.revision,
        owner: sub(),
        task_id: contract.task_id.clone(),
        ended,
        completed: completed.into_iter().map(String::from).collect(),
        incomplete: Vec::new(),
        changes: Vec::new(),
        evidence: Vec::new(),
        blockers: Vec::new(),
        submitted_at: "0".into(),
    }
}

fn validate(
    c: &TaskContract,
    d: &DeliveryResult,
    l: &Ledger,
    r: &RootResolver,
) -> DeliveryValidation {
    validate_delivery(c, d, l, r, &CaptureOptions::default())
}

fn codes(v: &DeliveryValidation) -> Vec<&str> {
    v.reasons.iter().map(|r| r.code.as_str()).collect()
}

// ---------- 1. 身份 / revision ----------

#[test]
fn late_result_from_previous_run_is_rejected() {
    let f = fx();
    let c = contract(&[], vec![]);
    let mut d = delivery(&c, vec![], EndedKind::Report);
    d.owner.root_run_id = "run0".into();
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(!v.accepted());
    assert!(codes(&v).contains(&"delivery_run_mismatch"));
}

#[test]
fn result_from_other_session_is_rejected() {
    let f = fx();
    let c = contract(&[], vec![]);
    let mut d = delivery(&c, vec![], EndedKind::Report);
    d.owner.session_id = "sess2".into();
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(codes(&v).contains(&"delivery_session_mismatch"));
}

#[test]
fn stale_contract_revision_is_rejected() {
    let f = fx();
    let mut c = contract(&[], vec![]);
    c.revision = 2;
    let mut d = delivery(&c, vec![], EndedKind::Report);
    d.contract_revision = 1;
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(codes(&v).contains(&"delivery_revision_stale"));
}

// ---------- 2. 字段完整 ----------

#[test]
fn unknown_requirement_ids_are_rejected_with_item_id() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec!["r1", "r999"], EndedKind::Report);
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    let hit = v
        .reasons
        .iter()
        .find(|r| r.code == "delivery_unknown_requirement")
        .unwrap();
    assert_eq!(hit.item_id.as_deref(), Some("r999"));
}

#[test]
fn required_requirement_not_declared_at_all_is_blocking() {
    // 静默遗漏必需项不能当默认通过。
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec![], EndedKind::Report);
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    let hit = v
        .reasons
        .iter()
        .find(|r| r.code == "delivery_required_uncovered")
        .unwrap();
    assert_eq!(hit.item_id.as_deref(), Some("r1"));
}

#[test]
fn incomplete_without_reason_is_blocking() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let mut d = delivery(&c, vec![], EndedKind::Report);
    d.incomplete.push(IncompleteItem {
        target_id: "r1".into(),
        reason: "   ".into(),
    });
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(codes(&v).contains(&"delivery_missing_reason"));
}

// ---------- 3. 互斥 ----------

#[test]
fn same_item_completed_and_incomplete_is_rejected() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let mut d = delivery(&c, vec!["r1"], EndedKind::Report);
    d.incomplete.push(IncompleteItem {
        target_id: "r1".into(),
        reason: "其实没做完".into(),
    });
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    let hit = v
        .reasons
        .iter()
        .find(|r| r.code == "delivery_contradictory_state")
        .unwrap();
    assert_eq!(hit.item_id.as_deref(), Some("r1"));
}

// ---------- 4. 引用及版本 ----------

#[test]
fn fabricated_receipt_id_is_rejected() {
    // 模型自己编的回执 id 不构成证据。
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let mut d = delivery(&c, vec!["r1"], EndedKind::Report);
    d.evidence.push("rcpt_made_up".into());
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    let hit = v
        .reasons
        .iter()
        .find(|r| r.code == "delivery_unknown_evidence")
        .unwrap();
    assert_eq!(hit.item_id.as_deref(), Some("rcpt_made_up"));
}

#[test]
fn adopted_receipt_id_is_accepted_as_evidence() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let mut d = delivery(&c, vec!["r1"], EndedKind::Report);
    d.evidence.push("execution_op1".into());
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_op1".into(),
        ExecutionReceipt {
            platform: Some(std::env::consts::OS.into()),
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            schema: SCHEMA,
            id: "execution_op1".into(),
            owner: sub(),
            check_id: None,
            requested: CommandSpec::Requested {
                command_text: "true".into(),
                shell_hint: None,
            },
            resolved: None,
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            started_at: "0".into(),
            finished_at: "0".into(),
            outcome: ExecutionOutcome::Exited,
            exit_code: Some(0),
            signal: None,
            output_ref: None,
            output_truncated: false,
            before: None,
            after: None,
            issuer: ReceiptIssuer::Host,
            reason: None,
        },
    );
    let v = validate(&c, &d, &l, &f.roots);
    assert!(
        !codes(&v).contains(&"delivery_unknown_evidence"),
        "已采纳回执应被认作证据：{:?}",
        codes(&v)
    );
}

#[test]
fn unverified_change_claim_is_rejected() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let mut d = delivery(&c, vec!["r1"], EndedKind::Report);
    // 声明创建了一个并不存在的文件
    d.changes.push(ChangeRef {
        path: p("never_written.rs"),
        before_hash: None,
        after_hash: Some("deadbeef".into()),
        kind: ChangeKind::Created,
    });
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(codes(&v).contains(&"delivery_change_unverified"));
}

// ---------- 5. 必需检查 ----------

#[test]
fn exit_zero_alone_does_not_satisfy_a_missing_artifact() {
    // 「仅 exit0 不满足产物检查」：命令退出码与产物是**两件独立的事**。
    // 第一步证明 exit0 确实满足它自己那条命令检查；第二步证明它顶替不了缺失的产物。
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract(&["r1"], vec![check("c1", "a.txt")]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);

    // 命令检查真实通过（退出 0 + 范围未变）
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_c1".into(),
        ExecutionReceipt {
            platform: Some(std::env::consts::OS.into()),
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            schema: SCHEMA,
            id: "execution_c1".into(),
            owner: root(),
            check_id: Some("c1".into()),
            requested: CommandSpec::Requested {
                command_text: "true".into(),
                shell_hint: None,
            },
            resolved: None,
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            started_at: "0".into(),
            finished_at: "0".into(),
            outcome: ExecutionOutcome::Exited,
            exit_code: Some(0),
            signal: None,
            output_ref: None,
            output_truncated: false,
            before: Some(ver.clone()),
            after: Some(ver),
            issuer: ReceiptIssuer::Host,
            reason: None,
        },
    );
    // 只有命令检查时，exit0 足够——它确实满足了那一条。
    let v = validate(&c, &d, &l, &f.roots);
    assert!(
        v.accepted(),
        "命令检查真实通过时不应被误拒：{:?}",
        v.reasons
    );

    // 加上一个**不存在**的必需产物：同一份 exit0 证据顶替不了它。
    let mut c2 = c.clone();
    c2.artifacts.push(ArtifactRequirement {
        id: "a1".into(),
        path: p("missing.bin"),
        kind: ArtifactKind::Text,
        constraints: Vec::new(),
        required: true,
    });
    c2.requirements[0].artifact_ids = vec!["a1".into()];
    let v2 = validate(&c2, &d, &l, &f.roots);
    assert!(
        !v2.accepted(),
        "产物缺失时 exit0 不得让交付通过：{:?}",
        v2.reasons
    );
    assert!(codes(&v2).contains(&"delivery_check_failed"));
}

#[test]
fn missing_check_evidence_blocks_delivery() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract(&["r1"], vec![check("c1", "a.txt")]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(
        codes(&v).contains(&"delivery_check_pending")
            || codes(&v).contains(&"delivery_check_missing")
    );
}

#[test]
fn stale_receipt_blocks_delivery() {
    let f = fx();
    let target = f.roots.get("ws").unwrap().join("a.txt");
    std::fs::write(&target, b"x").unwrap();
    let c = contract(&["r1"], vec![check("c1", "a.txt")]);
    let mut d = delivery(&c, vec!["r1"], EndedKind::Report);
    d.changes.clear();

    let scope = c.checks[0].scope.clone();
    let ver = capture_scope(&f.roots, &scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_c1".into(),
        ExecutionReceipt {
            platform: Some(std::env::consts::OS.into()),
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            schema: SCHEMA,
            id: "execution_c1".into(),
            owner: root(),
            check_id: Some("c1".into()),
            requested: CommandSpec::Requested {
                command_text: "true".into(),
                shell_hint: None,
            },
            resolved: None,
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            started_at: "0".into(),
            finished_at: "0".into(),
            outcome: ExecutionOutcome::Exited,
            exit_code: Some(0),
            signal: None,
            output_ref: None,
            output_truncated: false,
            before: Some(ver.clone()),
            after: Some(ver),
            issuer: ReceiptIssuer::Host,
            reason: None,
        },
    );
    // 验证之后文件变了
    std::fs::write(&target, b"changed").unwrap();
    let v = validate(&c, &d, &l, &f.roots);
    assert!(!v.accepted());
    assert!(
        codes(&v).contains(&"delivery_check_stale")
            || codes(&v).contains(&"delivery_stale_evidence"),
        "stale 证据必须阻断：{:?}",
        codes(&v)
    );
}

// ---------- 6. 审查 findings ----------

fn review_with(sev: FindingSeverity) -> ReviewResult {
    ReviewResult {
        schema: SCHEMA,
        id: "rev1".into(),
        owner: sub(),
        reviewer: RunIdentity {
            session_id: "sess1".into(),
            root_run_id: "run1".into(),
            actor_id: "reviewer-x".into(),
            actor_run_id: "rev-run".into(),
            sub_id: Some("reviewer-x".into()),
        },
        task_id: "task1".into(),
        check_id: Some("review".into()),
        contract_revision: 1,
        dimensions: vec![ReviewDimensionResult {
            state: Some(crate::core::delivery::responsibility::DimensionState::Checked),
            reason: None,
            review_findings: vec![],
            dimension: "security".into(),
            findings: vec![ValidationFinding {
                severity: sev,
                location: Some("src/lib.rs:10".into()),
                message: "存在越权路径".into(),
            }],
        }],
        verdict: ReviewVerdict {
            approved: false,
            blocking_count: 1,
        },
        scope_version: None,
        reviewed_at: "0".into(),
    }
}

#[test]
fn high_severity_finding_blocks_even_when_marked_all_complete() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let mut l = Ledger::new();
    l.reviews
        .insert("rev1".into(), review_with(FindingSeverity::Error));
    let v = validate(&c, &d, &l, &f.roots);
    assert!(!v.accepted(), "高危发现存在时标全部完成也必须拒");
    assert!(codes(&v).contains(&"review_error"));
}

#[test]
fn advisory_finding_does_not_fail_delivery() {
    // 建议性 findings 不自动失败，只进 advisories。
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let mut l = Ledger::new();
    l.reviews
        .insert("rev1".into(), review_with(FindingSeverity::Warning));
    let v = validate(&c, &d, &l, &f.roots);
    assert!(!v.advisories.is_empty(), "建议级发现应作为提示保留");
    assert!(
        v.reasons.iter().all(|r| r.code != "review_warning"),
        "建议级发现不得进阻断列表"
    );
}

#[test]
fn review_by_same_actor_blocks_independence_claim() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let mut l = Ledger::new();
    let mut rv = review_with(FindingSeverity::Info);
    rv.reviewer = root();
    l.reviews.insert("rev1".into(), rv);
    let v = validate(&c, &d, &l, &f.roots);
    assert!(codes(&v).contains(&"review_not_independent"));
}

// ---------- 7. 终态一致性 ----------

#[test]
fn partial_cannot_be_accepted_as_complete() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec!["r1"], EndedKind::Partial);
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    assert!(codes(&v).contains(&"delivery_partial_not_complete"));
}

#[test]
fn budget_and_no_report_cannot_be_accepted() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    for (ended, code) in [
        (EndedKind::Budget, "delivery_budget_exhausted"),
        (EndedKind::NoReport, "delivery_no_report"),
    ] {
        let d = delivery(&c, vec!["r1"], ended);
        let v = validate(&c, &d, &Ledger::new(), &f.roots);
        assert!(codes(&v).contains(&code), "{ended:?} 应阻断");
    }
}

#[test]
fn report_with_full_evidence_is_accepted() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract(&["r1"], vec![check("c1", "a.txt")]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);

    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_c1".into(),
        ExecutionReceipt {
            platform: Some(std::env::consts::OS.into()),
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            schema: SCHEMA,
            id: "execution_c1".into(),
            owner: root(),
            check_id: Some("c1".into()),
            requested: CommandSpec::Requested {
                command_text: "true".into(),
                shell_hint: None,
            },
            resolved: None,
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            started_at: "0".into(),
            finished_at: "0".into(),
            outcome: ExecutionOutcome::Exited,
            exit_code: Some(0),
            signal: None,
            output_ref: None,
            output_truncated: false,
            before: Some(ver.clone()),
            after: Some(ver),
            issuer: ReceiptIssuer::Host,
            reason: None,
        },
    );
    let v = validate(&c, &d, &l, &f.roots);
    assert!(v.accepted(), "证据齐全且 report 应被接受：{:?}", v.reasons);
}

// ---------- 可重算 ----------

#[test]
fn repeated_validation_does_not_change_verdict() {
    // 校验是纯函数：重算得到同一结论（wait 重复调用不得改变验收）。
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec!["r1"], EndedKind::Partial);
    let l = Ledger::new();
    let a = validate(&c, &d, &l, &f.roots);
    let b = validate(&c, &d, &l, &f.roots);
    assert_eq!(a.verdict, b.verdict);
    assert_eq!(a.reasons, b.reasons);
}

#[test]
fn validation_summary_is_machine_readable() {
    let f = fx();
    let c = contract(&["r1"], vec![]);
    let d = delivery(&c, vec![], EndedKind::Report);
    let v = validate(&c, &d, &Ledger::new(), &f.roots);
    let s = v.summary();
    assert_eq!(s["verdict"], "rejected");
    assert!(!s["reasons"].as_array().unwrap().is_empty());
}

// ---------- 变更核对复用同一实现 ----------

#[test]
fn validation_and_claim_validation_agree_on_a_fake_change() {
    // 两处不得对"改动是否成立"有两套判断。
    let f = fx();
    let ch = ChangeRef {
        path: p("nope.rs"),
        before_hash: None,
        after_hash: Some("x".into()),
        kind: ChangeKind::Created,
    };
    assert!(artifacts::validate_change(&f.roots, &ch).reason.is_some());
}

#[test]
fn stable_review_findings_obey_blocking_policy_in_sub_delivery() {
    let f = fx();
    let mut c = contract(&["r1"], vec![]);
    c.review_policy.blocking_severities = Some(vec![FindingSeverity::Warning]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let mut review = review_with(FindingSeverity::Warning);
    review.dimensions[0].findings.clear();
    review.dimensions[0].review_findings.push(
        crate::core::delivery::responsibility::ReviewFinding {
            stable_id: "warning-1".into(),
            severity: FindingSeverity::Warning,
            path: None,
            line_range: None,
            message: "must block".into(),
            evidence: vec![],
            needs_human_check: true,
        },
    );
    let mut ledger = Ledger::new();
    ledger.reviews.insert(review.id.clone(), review);
    let result = validate(&c, &d, &ledger, &f.roots);
    assert!(codes(&result).contains(&"review_warning"));
}

#[test]
fn latest_clean_review_supersedes_old_finding_without_deleting_history() {
    let f = fx();
    let mut c = contract(&["r1"], vec![]);
    c.review_policy.blocking_severities = Some(vec![FindingSeverity::Warning]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let old = review_with(FindingSeverity::Warning);
    let mut clean = old.clone();
    clean.id = "clean".into();
    clean.reviewed_at = "1".into();
    clean.dimensions[0].findings.clear();
    clean.verdict.approved = true;
    clean.verdict.blocking_count = 0;
    let mut ledger = Ledger::new();
    ledger.reviews.insert(old.id.clone(), old);
    ledger.reviews.insert(clean.id.clone(), clean);
    let result = validate(&c, &d, &ledger, &f.roots);
    assert!(!codes(&result).contains(&"review_warning"));
    assert_eq!(ledger.reviews.len(), 2);
}

#[test]
fn clean_review_of_another_check_cannot_hide_a_blocking_finding() {
    let f = fx();
    let mut c = contract(&["r1"], vec![]);
    c.review_policy.blocking_severities = Some(vec![FindingSeverity::Warning]);
    let d = delivery(&c, vec!["r1"], EndedKind::Report);
    let old = review_with(FindingSeverity::Warning);
    let mut other = old.clone();
    other.id = "other".into();
    other.check_id = Some("another-check".into());
    other.reviewed_at = "1".into();
    other.dimensions[0].findings.clear();
    other.verdict.approved = true;
    other.verdict.blocking_count = 0;
    let mut ledger = Ledger::new();
    ledger.reviews.insert(old.id.clone(), old);
    ledger.reviews.insert(other.id.clone(), other);
    assert!(codes(&validate(&c, &d, &ledger, &f.roots)).contains(&"review_warning"));
}
