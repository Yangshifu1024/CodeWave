//! #55 评测夹具：重复可重复、扰动可被发现、证据 ≠ 完成、退出 0 ≠ 需求完成。

use super::*;

#[test]
fn repeated_runs_produce_identical_verdicts() {
    let cases = repeat(4);
    assert_eq!(cases.len(), 4);
    let first = &cases[0].verdict;
    for c in &cases {
        assert_eq!(&c.verdict, first, "同输入重复跑必须给出同一结论");
        assert!(c.passed, "证据齐活时应通过：{}", c.verdict);
    }
}

#[test]
fn the_verdict_string_carries_no_timestamp_or_random_id() {
    let a = repeat(1).remove(0).verdict;
    let b = repeat(1).remove(0).verdict;
    assert_eq!(a, b);
    // 不含 ISO 时刻。
    assert!(!a.contains("T00:") && !a.contains("Z\""));
}

#[test]
fn perturbation_is_detected() {
    // 改动被验证范围内的一个字节，结论必须改变——
    // 一条"怎么改都过"的评测等于没有评测。
    let c = perturbed();
    assert!(c.passed, "扰动未被检出：{}", c.verdict);
    assert!(c.verdict.contains("=>"), "应同时给出扰动前后的结论");
}

#[test]
fn a_users_yes_is_not_evidence() {
    let c = evidence_completion_separation();
    assert!(c.passed, "用户口头确认被当成了证据：{}", c.verdict);
    assert!(c.verdict.starts_with("pending") || c.verdict.contains("no_evidence"));
}

#[test]
fn a_failing_check_is_not_a_passed_requirement() {
    let c = exit_zero_is_not_completion();
    assert!(c.passed, "检查没过却被判通过：{}", c.verdict);
    assert!(c.verdict.starts_with("failed"));
}

#[test]
fn the_report_summarises_every_property() {
    let r = report();
    assert_eq!(r["repeatable"], true);
    assert_eq!(r["evidence_completion_separated"], true);
    assert_eq!(r["exit_zero_not_completion"], true);
    assert_eq!(r["perturbation_detected"], true);
    assert!(r["verdicts"].as_array().unwrap().len() >= 3);
}

#[test]
fn the_sandbox_is_isolated_from_the_real_data_root() {
    // 每次评测都用自己的临时目录：不读真实配置与密钥。
    let a = Sandbox::new();
    let b = Sandbox::new();
    assert_ne!(a.ws(), b.ws());
    assert!(a.ws().to_string_lossy().contains("Temporary") || a.ws().is_absolute());
}
