//! 修复前失败证据夹具：真实断言失败→通过、依赖缺失不能作 RED、同名不同命令不能配对、
//! 只给 GREEN 保持 unproven、中断后不重跑、非确定性观测不足。

use super::*;
use crate::core::delivery::version::RootResolver;

fn owner() -> RunIdentity {
    RunIdentity::root("sess1", "run1")
}

fn roots() -> RootResolver {
    RootResolver::new()
}

fn receipt(
    check: &str,
    outcome: ExecutionOutcome,
    exit: Option<i32>,
    finished: &str,
) -> ExecutionReceipt {
    ExecutionReceipt {
        platform: Some(std::env::consts::OS.into()),
        binding: Some(crate::core::delivery::types::ContractBinding {
            task_id: "task1".into(),
            revision: 1,
        }),
        schema: SCHEMA_F,
        id: format!("execution_{check}_{finished}"),
        owner: owner(),
        check_id: Some(check.to_string()),
        requested: CommandSpec::Requested {
            command_text: "cargo test".into(),
            shell_hint: None,
        },
        resolved: Some(CommandSpec::Resolved {
            program: "bash".into(),
            argv: vec!["-c".into(), "cargo test".into()],
            shell_family: Some("bash".into()),
            shell_command: Some("cargo test".into()),
            timeout_ms: 1000,
            config_digest: None,
        }),
        cwd: None,
        started_at: "0".into(),
        finished_at: finished.into(),
        outcome,
        exit_code: exit,
        signal: None,
        output_ref: None,
        output_truncated: false,
        before: None,
        after: Some(crate::core::delivery::version::ScopeVersion {
            schema: 1,
            digest: finished.into(),
            entries: vec![],
            toolchain: vec![],
            completeness: crate::core::delivery::version::VersionCompleteness::Complete,
        }),
        issuer: ReceiptIssuer::Host,
        reason: None,
    }
}

const SCHEMA_F: u32 = 1;

use crate::core::delivery::types::{CommandSpec, ReceiptIssuer};

fn pair() -> RegressionPair {
    RegressionPair {
        issue_id: "issue-1".into(),
        check_id: "c1".into(),
        red_receipt_id: Some("r".into()),
        green_receipt_id: Some("g".into()),
        reproduction_status: ReproductionStatus::Unproven,
        reason: None,
        red_version: Some("1".into()),
        green_version: Some("2".into()),
        check_spec_revision: 1,
        nondeterminism: None,
    }
}

#[test]
fn real_assertion_failure_then_pass_is_fixed() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let (status, _) = judge(
        &pair(),
        Some(&red),
        Some(&green),
        &owner(),
        &[101],
        &roots(),
    );
    assert_eq!(status, ReproductionStatus::Fixed);
    assert!(satisfies_required_evidence(status));
}

#[test]
fn missing_dependency_cannot_serve_as_red() {
    // 程序不存在 = 环境问题，不是缺陷复现
    let red = receipt("c1", ExecutionOutcome::SpawnFailed, None, "1");
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let (status, reason) = judge(
        &pair(),
        Some(&red),
        Some(&green),
        &owner(),
        &[101],
        &roots(),
    );
    assert_eq!(status, ReproductionStatus::Unproven);
    assert!(reason.unwrap().contains("环境问题"));
}

#[test]
fn denied_execution_cannot_serve_as_red() {
    let mut red = receipt("c1", ExecutionOutcome::Denied, None, "1");
    red.reason = Some("fence_block:E_FENCE_BLOCKED".into());
    let (status, reason) = judge(
        &pair(),
        Some(&red),
        Some(&receipt("c1", ExecutionOutcome::Exited, Some(0), "2")),
        &owner(),
        &[101],
        &roots(),
    );
    assert_eq!(status, ReproductionStatus::Unproven);
    assert!(reason.unwrap().contains("环境问题"));
}

#[test]
fn test_not_found_exit_code_is_not_a_defect_reproduction() {
    // 退出码不在"预期失败"集合里（如测试二进制找不到），不算复现
    let red = receipt("c1", ExecutionOutcome::Exited, Some(127), "1");
    let (status, _) = judge(
        &pair(),
        Some(&red),
        Some(&receipt("c1", ExecutionOutcome::Exited, Some(0), "2")),
        &owner(),
        &[101],
        &roots(),
    );
    assert_eq!(status, ReproductionStatus::Unproven);
}

#[test]
fn green_only_stays_unproven() {
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let (status, reason) = judge(&pair(), None, Some(&green), &owner(), &[101], &roots());
    assert_eq!(status, ReproductionStatus::Unproven);
    assert!(reason.unwrap().contains("不证明之前坏过"));
}

#[test]
fn interrupted_red_is_not_rerun_and_stays_unproven() {
    // 中断不是"失败过"：不重跑，历史回执留着对账
    let red = receipt("c1", ExecutionOutcome::Interrupted, None, "1");
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let (status, reason) = judge(
        &pair(),
        Some(&red),
        Some(&green),
        &owner(),
        &[101],
        &roots(),
    );
    assert_eq!(status, ReproductionStatus::Unproven);
    assert!(reason.unwrap().contains("不重跑"));
}

#[test]
fn timed_out_red_is_incomplete_not_reproduction() {
    let red = receipt("c1", ExecutionOutcome::TimedOut, None, "1");
    let (status, _) = judge(
        &pair(),
        Some(&red),
        Some(&receipt("c1", ExecutionOutcome::Exited, Some(0), "2")),
        &owner(),
        &[101],
        &roots(),
    );
    assert_eq!(status, ReproductionStatus::Unproven);
}

#[test]
fn same_name_different_command_cannot_be_paired() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let green = receipt("c2", ExecutionOutcome::Exited, Some(0), "2");
    assert!(
        pair_usable(&red, &green, &owner(), "c1").is_err(),
        "同名不同命令不能直接配对"
    );
}

#[test]
fn cross_session_receipt_cannot_be_paired() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let mut green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    green.owner.session_id = "sess2".into();
    assert!(pair_usable(&red, &green, &owner(), "c1").is_err());
}

#[test]
fn late_receipt_from_previous_run_cannot_be_paired() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let mut green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    green.owner.root_run_id = "run0".into();
    assert!(pair_usable(&red, &green, &owner(), "c1").is_err());
}

#[test]
fn same_version_red_and_green_is_not_a_fix() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let mut p = pair();
    p.green_version = p.red_version.clone();
    let (status, reason) = judge(&p, Some(&red), Some(&green), &owner(), &[101], &roots());
    assert_eq!(status, ReproductionStatus::Unproven);
    assert!(reason.unwrap().contains("内容没变过"));
}

#[test]
fn nondeterministic_single_pass_is_not_conclusive() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let mut p = pair();
    p.nondeterminism = Some(NondeterminismRecord {
        runs: 1,
        failures: 0,
        environment: vec!["darwin".into()],
    });
    let (status, reason) = judge(&p, Some(&red), Some(&green), &owner(), &[101], &roots());
    assert_eq!(status, ReproductionStatus::Reproduced);
    assert!(reason.unwrap().contains("一次通过不作数"));
}

#[test]
fn nondeterministic_enough_runs_with_zero_failures_is_conclusive() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let green = receipt("c1", ExecutionOutcome::Exited, Some(0), "2");
    let mut p = pair();
    p.nondeterminism = Some(NondeterminismRecord {
        runs: 12,
        failures: 0,
        environment: vec!["darwin".into()],
    });
    let (status, _) = judge(&p, Some(&red), Some(&green), &owner(), &[101], &roots());
    assert_eq!(status, ReproductionStatus::Fixed);
}

#[test]
fn failure_rate_is_reported_faithfully() {
    let n = NondeterminismRecord {
        runs: 10,
        failures: 3,
        environment: vec![],
    };
    assert!((n.failure_rate() - 0.3).abs() < 1e-9);
    assert!(!n.is_conclusive(), "还有失败就不是解决");
}

#[test]
fn zero_runs_does_not_panic_or_claim_success() {
    let n = NondeterminismRecord {
        runs: 0,
        failures: 0,
        environment: vec![],
    };
    assert_eq!(n.failure_rate(), 0.0);
    assert!(!n.is_conclusive(), "没观测过不等于没问题");
}

#[test]
fn reproduction_without_green_is_reproduced_not_fixed() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    let (status, _) = judge(&pair(), Some(&red), None, &owner(), &[101], &roots());
    assert_eq!(status, ReproductionStatus::Reproduced);
    assert!(!satisfies_required_evidence(status));
}

#[test]
fn not_reproducible_never_satisfies_required_evidence() {
    assert!(!satisfies_required_evidence(
        ReproductionStatus::NotReproducible
    ));
    assert!(!satisfies_required_evidence(ReproductionStatus::Unproven));
    assert!(!satisfies_required_evidence(ReproductionStatus::Reproduced));
    assert!(satisfies_required_evidence(ReproductionStatus::Fixed));
}

#[test]
fn green_failure_or_missing_version_never_proves_fixed() {
    let red = receipt("c1", ExecutionOutcome::Exited, Some(101), "1");
    for mut green in [
        receipt("c1", ExecutionOutcome::Exited, Some(1), "2"),
        receipt("c1", ExecutionOutcome::SpawnFailed, None, "2"),
    ] {
        assert_ne!(
            judge(
                &pair(),
                Some(&red),
                Some(&green),
                &owner(),
                &[101],
                &roots()
            )
            .0,
            ReproductionStatus::Fixed
        );
        green.outcome = ExecutionOutcome::Exited;
        green.exit_code = Some(0);
        green.after = None;
        assert_ne!(
            judge(
                &pair(),
                Some(&red),
                Some(&green),
                &owner(),
                &[101],
                &roots()
            )
            .0,
            ReproductionStatus::Fixed
        );
    }
}
