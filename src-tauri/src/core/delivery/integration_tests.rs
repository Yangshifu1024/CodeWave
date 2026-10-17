//! #22 集成后验收夹具：A/B 各自通过但合体后失败、锁文件变化使旧检查 stale、
//! 父并发写入不完成、partial 子结果不能自动集成通过、重复集成不重复计费。

use super::*;
use crate::core::delivery::acceptance::Ledger;
use crate::core::delivery::types::{
    CheckExpectation, CheckKind, CheckSpec, CommandSpec, ExecutionOutcome, PathRef, ReceiptIssuer,
    Requirement, ReviewPolicy, RunIdentity, SCHEMA, VerificationScope,
};
use crate::core::delivery::version::{CaptureOptions, ScopeVersion, capture_scope};
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

fn owner() -> RunIdentity {
    RunIdentity::root("sess1", "run1")
}

fn p(rel: &str) -> PathRef {
    PathRef {
        root_id: "ws".into(),
        relative_path: rel.into(),
    }
}

fn scope(rel: &str) -> VerificationScope {
    VerificationScope {
        roots: Vec::new(),
        inputs: vec![p(rel)],
        outputs: Vec::new(),
        dependency_files: Vec::new(),
        environment_keys: Vec::new(),
    }
}

fn contract(check_rel: &str) -> TaskContract {
    TaskContract {
        schema: SCHEMA,
        task_id: "task1".into(),
        revision: 1,
        owner: owner(),
        requirements: vec![Requirement {
            id: "r1".into(),
            title: "整体通过".into(),
            required: true,
            check_ids: vec!["ci".into()],
            artifact_ids: vec![],
        }],
        artifacts: Vec::new(),
        checks: vec![CheckSpec {
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            independent_verifier: false,
            id: "ci".into(),
            kind: CheckKind::Command,
            command: Some(CommandSpec::Requested {
                command_text: "cargo test --workspace".into(),
                shell_hint: None,
            }),
            scope: scope(check_rel),
            required: true,
            expectation: CheckExpectation::Command {
                allowed_exit_codes: vec![0],
            },
        }],
        review_policy: ReviewPolicy::default(),
        scope_changes: Vec::new(),
    }
}

fn target(required: Vec<&str>) -> IntegrationTarget {
    IntegrationTarget {
        contract_revision: 1,
        accepted_delivery_ids: vec!["dlv_a".into(), "dlv_b".into()],
        expected_parent_version: "before".into(),
        result_version: None,
        required_check_ids: required.into_iter().map(String::from).collect(),
        status: IntegrationStatus::Pending,
        shared_resources: Vec::new(),
        reason: None,
        not_required_basis: None,
    }
}

fn receipt_for(
    check: &str,
    exit: i32,
    after: Option<ScopeVersion>,
    owner_actor: &str,
) -> ExecutionReceipt {
    let mut o = owner();
    o.actor_id = owner_actor.into();
    o.actor_run_id = format!("{owner_actor}-run");
    ExecutionReceipt {
        binding: Some(crate::core::delivery::types::ContractBinding {
            task_id: "task1".into(),
            revision: 1,
        }),
        schema: SCHEMA,
        id: format!("execution_{check}_{owner_actor}"),
        owner: o,
        check_id: Some(check.into()),
        requested: CommandSpec::Requested {
            command_text: "cargo test --workspace".into(),
            shell_hint: None,
        },
        resolved: None,
        cwd: Some(PathRef {
            root_id: "ws".into(),
            relative_path: ".".into(),
        }),
        started_at: "0".into(),
        finished_at: "1".into(),
        outcome: ExecutionOutcome::Exited,
        exit_code: Some(exit),
        signal: None,
        output_ref: None,
        output_truncated: false,
        before: after.clone(),
        after,
        issuer: ReceiptIssuer::Host,
        reason: None,
    }
}

fn eval(
    f: &Fx,
    t: &IntegrationTarget,
    c: &TaskContract,
    l: &Ledger,
    before: Option<&ScopeVersion>,
    after: Option<&ScopeVersion>,
) -> IntegrationOutcome {
    evaluate_integration(
        t,
        c,
        l,
        &f.roots,
        &CaptureOptions::default(),
        &c.checks[0].scope,
        before,
        after,
    )
}

#[test]
fn integration_passes_when_target_version_is_clean_and_check_passes() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_ci_root".into(),
        receipt_for("ci", 0, Some(ver.clone()), "root"),
    );
    let out = eval(&f, &target(vec!["ci"]), &c, &l, Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Verified);
    assert!(out.status.allows_completion());
}

#[test]
fn children_passing_individually_does_not_mean_the_composite_passes() {
    // A/B 各自的检查都过，但整体检查失败 → 整体不通过。
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    // 两个子级回执（绑定别的检查）都通过
    for actor in ["sub-a", "sub-b"] {
        let mut r = receipt_for("child", 0, Some(ver.clone()), actor);
        l.receipts.insert(r.id.clone(), r.clone());
        r.check_id = Some("child".into());
    }
    // 整体检查失败
    l.receipts.insert(
        "execution_ci_root".into(),
        receipt_for("ci", 101, Some(ver.clone()), "root"),
    );
    let out = eval(&f, &target(vec!["ci"]), &c, &l, Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Failed);
    assert!(!out.status.allows_completion());
}

#[test]
fn child_receipts_cannot_substitute_for_the_required_overall_check() {
    // 只有子级回执、没有绑定 ci 的回执 → 整体仍未验证
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_child_sub-a".into(),
        receipt_for("child", 0, Some(ver.clone()), "sub-a"),
    );
    let out = eval(&f, &target(vec!["ci"]), &c, &l, Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Failed);
    assert!(out.reason.unwrap().contains("尚无真实回执"));
}

#[test]
fn lock_file_change_after_child_merge_makes_the_old_check_stale() {
    let f = fx();
    let ws = f.roots.get("ws").unwrap().clone();
    std::fs::write(ws.join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_ci_root".into(),
        receipt_for("ci", 0, Some(ver.clone()), "root"),
    );
    // 合体后锁文件变了 → 子级检查证据失效
    std::fs::write(ws.join("Cargo.lock"), b"changed").unwrap();
    let mut sc = c.checks[0].scope.clone();
    sc.dependency_files.push(p("Cargo.lock"));
    let mut c2 = c.clone();
    c2.checks[0].scope = sc;
    let ver2 = capture_scope(&f.roots, &c2.checks[0].scope, &CaptureOptions::default()).unwrap();
    let out = eval(&f, &target(vec!["ci"]), &c2, &l, Some(&ver2), Some(&ver2));
    assert_eq!(out.status, IntegrationStatus::Failed);
    assert!(out.reason.unwrap().contains("证据已失效"));
}

#[test]
fn parent_write_during_check_stops_completion() {
    let f = fx();
    let ws = f.roots.get("ws").unwrap().clone();
    std::fs::write(ws.join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let before = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    // 父在检查期间改了文件
    std::fs::write(ws.join("a.txt"), b"y").unwrap();
    let after = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let l = Ledger::new();
    let out = eval(&f, &target(vec!["ci"]), &c, &l, Some(&before), Some(&after));
    assert_eq!(out.status, IntegrationStatus::Drifted);
    assert!(!out.status.allows_completion());
}

#[test]
fn missing_parent_versions_cannot_prove_no_write_happened() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let l = Ledger::new();
    let out = eval(&f, &target(vec!["ci"]), &c, &l, None, None);
    assert_eq!(out.status, IntegrationStatus::Pending);
    assert!(out.reason.unwrap().contains("缺少检查前后"));
}

#[test]
fn check_scope_outside_parent_scope_cannot_prove_no_write_happened() {
    // 前后对比只覆盖 `parent_scope`。若必跑检查看的范围落在父范围之外，
    // 那么"检查期间没被写入"对该区域**无从证明**——不能因为父范围前后相等就放行。
    let f = fx();
    let ws = f.roots.get("ws").unwrap().clone();
    std::fs::write(ws.join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    // 父范围只盯 `other.txt`，而必跑检查看的是 `a.txt`。
    let parent = scope("other.txt");
    let before = capture_scope(&f.roots, &parent, &CaptureOptions::default()).unwrap();
    let after = capture_scope(&f.roots, &parent, &CaptureOptions::default()).unwrap();
    // 即便检查本身有真实通过回执，也不能顶替"覆盖了被检查区域"的证明。
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_ci_root".into(),
        receipt_for(
            "ci",
            0,
            Some(capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap()),
            "root",
        ),
    );
    let out = evaluate_integration(
        &target(vec!["ci"]),
        &c,
        &l,
        &f.roots,
        &CaptureOptions::default(),
        &parent,
        Some(&before),
        Some(&after),
    );
    assert_eq!(out.status, IntegrationStatus::Pending);
    assert!(!out.status.allows_completion());
    assert!(out.reason.unwrap().contains("落在父范围之外"));
}

#[test]
fn stale_contract_revision_stops_integration() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let mut c = contract("a.txt");
    c.revision = 2;
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut t = target(vec!["ci"]);
    t.contract_revision = 1;
    let out = eval(&f, &t, &c, &Ledger::new(), Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Stale);
}

#[test]
fn not_required_without_basis_is_rejected() {
    // 失败之后自动降级成"不需要整体验证"必须被拒
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut t = target(vec!["ci"]);
    t.status = IntegrationStatus::NotRequired;
    t.not_required_basis = None;
    let out = eval(&f, &t, &c, &Ledger::new(), Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Pending);
    assert!(out.reason.unwrap().contains("不得事后自动降级"));
}

#[test]
fn not_required_with_contract_basis_is_allowed() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut t = target(vec!["ci"]);
    t.status = IntegrationStatus::NotRequired;
    t.not_required_basis = Some("decision_dec_1".into());
    let mut ledger = Ledger::new();
    ledger.decisions.insert(
        "decision_dec_1".into(),
        crate::core::delivery::types::UserDecision {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            id: "decision_dec_1".into(),
            owner: owner(),
            item_ids: vec!["r1".into()],
            decision: crate::core::delivery::types::UserDecisionKind::Waived,
            scope_version: Some(ver.digest.clone()),
            prompt_id: "real_ask".into(),
            answered_at: "0".into(),
        },
    );
    let out = eval(&f, &t, &c, &ledger, Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::NotRequired);
    assert!(out.status.allows_completion());
}

#[test]
fn shared_external_resource_without_version_is_not_assumed_mergeable() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut t = target(vec!["ci"]);
    t.shared_resources = vec!["postgres-main".into()];
    let out = eval(&f, &t, &c, &Ledger::new(), Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Pending);
    assert!(out.reason.unwrap().contains("不得推断可合并"));
}

#[test]
fn shared_resource_with_registered_version_proceeds() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut t = target(vec!["ci"]);
    t.shared_resources = vec!["postgres-main".into()];
    let mut l = Ledger::new();
    l.validations.insert(
        "validation_res".into(),
        crate::core::delivery::types::BuiltinValidationRecord {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            id: "validation_res".into(),
            owner: owner(),
            validator_id: "resource-probe".into(),
            validator_version: "1".into(),
            check_id: None,
            input_refs: vec!["resource:postgres-main@16".into()],
            scope_version: None,
            started_at: "0".into(),
            finished_at: "1".into(),
            outcome: crate::core::delivery::types::ValidationOutcome::Passed,
            findings: Vec::new(),
            output_ref: None,
        },
    );
    l.receipts.insert(
        "execution_ci_root".into(),
        receipt_for("ci", 0, Some(ver.clone()), "root"),
    );
    let out = eval(&f, &t, &c, &l, Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Verified);
}

#[test]
fn bare_resource_name_without_version_segment_is_not_a_registration() {
    // "知道 postgres-main 存在"不等于"知道它当前是哪个版本"：只有带版本的事实
    // 才能支撑"可以合并"，裸资源名仍按未知处理。
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut t = target(vec!["ci"]);
    t.shared_resources = vec!["postgres-main".into()];
    let mut l = Ledger::new();
    l.validations.insert(
        "validation_res".into(),
        crate::core::delivery::types::BuiltinValidationRecord {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "task1".into(),
                revision: 1,
            }),
            id: "validation_res".into(),
            owner: owner(),
            validator_id: "resource-probe".into(),
            validator_version: "1".into(),
            check_id: None,
            input_refs: vec!["resource:postgres-main".into()],
            scope_version: None,
            started_at: "0".into(),
            finished_at: "1".into(),
            outcome: crate::core::delivery::types::ValidationOutcome::Passed,
            findings: Vec::new(),
            output_ref: None,
        },
    );
    let out = eval(&f, &t, &c, &l, Some(&ver), Some(&ver));
    assert_eq!(out.status, IntegrationStatus::Pending);
    assert!(out.reason.unwrap().contains("不得推断可合并"));
}

#[test]
fn missing_required_check_in_contract_fails_loudly() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let out = eval(
        &f,
        &target(vec!["not_in_contract"]),
        &c,
        &Ledger::new(),
        Some(&ver),
        Some(&ver),
    );
    assert_eq!(out.status, IntegrationStatus::Failed);
    assert!(out.reason.unwrap().contains("不存在的检查"));
}

#[test]
fn repeated_evaluation_is_stable_and_reuses_the_same_receipt() {
    // 重复集成回执不重复检查、不重复计费：同一张回执被复用，结论一致。
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = Ledger::new();
    l.receipts.insert(
        "execution_ci_root".into(),
        receipt_for("ci", 0, Some(ver.clone()), "root"),
    );
    let a = eval(&f, &target(vec!["ci"]), &c, &l, Some(&ver), Some(&ver));
    let b = eval(&f, &target(vec!["ci"]), &c, &l, Some(&ver), Some(&ver));
    assert_eq!(a.status, b.status);
    assert_eq!(a.reason, b.reason);
    assert_eq!(l.receipts.len(), 1, "重复评估不新增回执");
}

#[test]
fn summary_is_machine_readable() {
    let f = fx();
    std::fs::write(f.roots.get("ws").unwrap().join("a.txt"), b"x").unwrap();
    let c = contract("a.txt");
    let ver = capture_scope(&f.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let out = eval(
        &f,
        &target(vec!["ci"]),
        &c,
        &Ledger::new(),
        Some(&ver),
        Some(&ver),
    );
    let s = summary(&out);
    assert!(s["status"].is_string());
    assert!(s["reason"].is_string());
}

#[test]
fn only_verified_or_not_required_allows_completion() {
    assert!(IntegrationStatus::Verified.allows_completion());
    assert!(IntegrationStatus::NotRequired.allows_completion());
    for s in [
        IntegrationStatus::Pending,
        IntegrationStatus::Failed,
        IntegrationStatus::Drifted,
        IntegrationStatus::Stale,
    ] {
        assert!(!s.allows_completion(), "{s:?} 不得放行完成");
    }
}
