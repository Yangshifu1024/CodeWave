//! #14 执行回执夹具：真实 exit7 / exit0、程序不存在 → spawn_failed 且无退出码、
//! 保存失败保留真实工具结果、同 provider tool_use.id 幂等、模型无法签发。

use super::*;
use crate::core::delivery::types::{ExecutionReceipt, UserDecisionKind};
use crate::core::delivery::{DeliveryService, RunIdentity};
use std::sync::Arc;

struct Fixture {
    /// 保留 tempdir：目录必须在夹具存活期间不被清理。
    _dir: tempfile::TempDir,
    svc: Arc<DeliveryService>,
    identity: RunIdentity,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let svc = Arc::new(DeliveryService::new(dir.path().join("sessions")));
    Fixture {
        _dir: dir,
        svc,
        identity: RunIdentity::root("sess1", "run1"),
    }
}

fn recorder(fx: &Fixture, call_id: &str, command: &str) -> ReceiptRecorder {
    ReceiptRecorder::new(
        fx.svc.clone(),
        fx.identity.clone(),
        call_id,
        command,
        Some("src".into()),
        Some(1000),
        None,
    )
}

fn resolved() -> CommandSpec {
    CommandSpec::Resolved {
        program: "/bin/sh".into(),
        argv: vec!["-c".into(), "exit 7".into()],
        shell_family: Some("sh".into()),
        shell_command: Some("exit 7".into()),
        timeout_ms: 1000,
        config_digest: None,
    }
}

fn receipt(svc: &DeliveryService, id: &str) -> Option<ExecutionReceipt> {
    let raw = svc.store().load_record("sess1", id)?;
    match raw.body {
        crate::core::delivery::store::RecordBody::Execution(r) => Some(*r),
        _ => None,
    }
}

#[test]
fn real_exit_seven_is_recorded_with_its_code() {
    let fx = fixture();
    let mut r = recorder(&fx, "call1", "sh -c 'exit 7'");
    r.begin();
    let rec = r
        .complete(
            Some(resolved()),
            Some(PathRef {
                root_id: "workspace".into(),
                relative_path: "src".into(),
            }),
            ExecutionOutcome::Exited,
            Some(7),
            None,
            Some(b"boom".to_vec()),
            None,
            None,
            None,
        )
        .expect("回执应落盘");
    assert_eq!(rec.outcome, ExecutionOutcome::Exited);
    assert_eq!(rec.exit_code, Some(7));
    // 读回：索引内的记录与内存里那份一致
    let back = receipt(&fx.svc, &rec.id).expect("应可从索引读回");
    assert_eq!(back.exit_code, Some(7));
}

#[test]
fn real_exit_zero_is_recorded_separately() {
    let fx = fixture();
    let mut r = recorder(&fx, "call1", "true");
    r.begin();
    let rec = r
        .complete(
            Some(resolved()),
            None,
            ExecutionOutcome::Exited,
            Some(0),
            None,
            Some(b"".to_vec()),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(rec.exit_code, Some(0));
}

#[test]
fn nonexistent_program_is_spawn_failed_with_no_exit_code() {
    // 程序不存在：spawn 从未成功 → 没有退出码可编。
    let fx = fixture();
    let identity = RunIdentity::root("sess1", "run1");
    let err = fx
        .svc
        .finish_execution(
            &identity,
            "op_spawn",
            CommandSpec::Requested {
                command_text: "./no-such-binary".into(),
                shell_hint: None,
            },
            Some(CommandSpec::Resolved {
                program: "./no-such-binary".into(),
                argv: vec!["./no-such-binary".into()],
                shell_family: None,
                shell_command: None,
                timeout_ms: 1000,
                config_digest: None,
            }),
            Some(PathRef {
                root_id: "workspace".into(),
                relative_path: ".".into(),
            }),
            ExecutionOutcome::SpawnFailed,
            Some(127), // ← 捏造的退出码必须被拒
            None,
            None,
            None,
            None,
            Some("spawn failed".into()),
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), "E_EVIDENCE_INVALID");
}

#[test]
fn exit_code_without_exit_is_rejected() {
    let fx = fixture();
    let identity = RunIdentity::root("sess1", "run1");
    let err = fx
        .svc
        .finish_execution(
            &identity,
            "op_x",
            CommandSpec::Requested {
                command_text: "x".into(),
                shell_hint: None,
            },
            None,
            None,
            ExecutionOutcome::Cancelled,
            Some(0),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), "E_EVIDENCE_INVALID");
}

#[test]
fn executed_without_resolved_command_is_rejected() {
    let fx = fixture();
    let identity = RunIdentity::root("sess1", "run1");
    let err = fx
        .svc
        .finish_execution(
            &identity,
            "op_y",
            CommandSpec::Requested {
                command_text: "x".into(),
                shell_hint: None,
            },
            None, // 声称跑过却没有已解析形态
            None,
            ExecutionOutcome::Exited,
            Some(0),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(err.code(), "E_EVIDENCE_INVALID");
}

#[test]
fn denied_receipt_has_no_resolved_command_and_no_cwd() {
    // 被拒 = 从未启动：不能回填假 program/argv，也不能用默认目录补 cwd。
    let fx = fixture();
    let mut r = recorder(&fx, "call_deny", "rm -rf /");
    r.begin();
    r.deny("fence_block:E_FENCE_BLOCKED");
    let id = r.receipt_id().expect("拒绝回执应落盘").to_string();
    let rec = receipt(&fx.svc, &id).unwrap();
    assert_eq!(rec.outcome, ExecutionOutcome::Denied);
    assert!(rec.resolved.is_none(), "被拒不得有已解析命令");
    assert!(rec.cwd.is_none(), "被拒不得补默认 cwd");
    assert!(rec.exit_code.is_none());
    assert!(matches!(rec.requested, CommandSpec::Requested { .. }));
    assert!(rec.reason.unwrap().contains("fence_block"));
}

#[test]
fn same_provider_call_id_does_not_duplicate_the_record() {
    // 同 tool_use.id 重放 → 只有一条记录。
    let fx = fixture();
    let mut r = recorder(&fx, "call_dup", "true");
    r.begin();
    let first = r
        .complete(
            Some(resolved()),
            None,
            ExecutionOutcome::Exited,
            Some(0),
            None,
            Some(b"ok".to_vec()),
            None,
            None,
            None,
        )
        .unwrap();
    // 同 operation_id、同内容再走一遍
    let identity = fx.identity.clone();
    let (_, again) = fx
        .svc
        .finish_execution(
            &identity,
            r.operation_id(),
            CommandSpec::Requested {
                command_text: "true".into(),
                shell_hint: Some("sh".into()),
            },
            Some(CommandSpec::Resolved {
                program: "/bin/sh".into(),
                argv: vec!["-c".into(), "exit 7".into()],
                shell_family: Some("sh".into()),
                shell_command: Some("exit 7".into()),
                timeout_ms: 1000,
                config_digest: None,
            }),
            None,
            ExecutionOutcome::Exited,
            Some(0),
            None,
            Some(&b"ok"[..]),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(first.id, again.record_id, "同键不得产生第二条记录");
    let idx = fx.svc.store().load_index("sess1");
    assert_eq!(
        idx.records
            .values()
            .filter(|p| p.contains("execution"))
            .count(),
        1
    );
}

#[test]
fn model_cannot_reach_the_issuing_interface() {
    // 签发接口不经工具注册表暴露：模型没有可调的"发回执"工具。
    let reg = crate::tools::registry::ToolRegistry::default_tools();
    for forbidden in [
        "record_execution_receipt",
        "set_passed",
        "issue_receipt",
        "mark_acceptance",
    ] {
        assert!(
            reg.get(forbidden).is_none(),
            "{forbidden} 不得作为模型可调用工具出现"
        );
    }
}

#[test]
fn save_failure_keeps_real_result_and_reports_unpersisted() {
    // 交付目录被占成文件 → 落盘必失败。真实执行事实**保留**，只报未保存。
    let fx = fixture();
    let blocker = fx.svc.store().delivery_dir("sess1");
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"not a dir").unwrap();
    let mut r = recorder(&fx, "call_save_fail", "true");
    r.begin();
    let got = r.complete(
        Some(resolved()),
        None,
        ExecutionOutcome::Exited,
        Some(0),
        None,
        Some(b"out".to_vec()),
        None,
        None,
        None,
    );
    assert!(got.is_none(), "保存失败不应返回回执");
    let warnings = r.take_warnings();
    assert!(
        warnings.iter().any(|w| w.contains("未能落盘")),
        "必须明确告知未保存：{warnings:?}"
    );
}

#[test]
fn output_truncation_is_visible_on_the_receipt() {
    let fx = fixture();
    let mut r = recorder(&fx, "call_trunc", "big");
    r.begin();
    let big = vec![b'x'; (crate::core::delivery::types::MAX_OUTPUT_BYTES + 1) as usize];
    let rec = r
        .complete(
            Some(resolved()),
            None,
            ExecutionOutcome::Exited,
            Some(0),
            None,
            Some(big),
            None,
            None,
            None,
        )
        .unwrap();
    assert!(rec.output_truncated, "截断必须显式标记");
    assert!(rec.output_ref.is_some());
    assert!(r.take_warnings().iter().any(|w| w.contains("截断")));
}

#[test]
fn requested_text_is_bounded_before_reaching_the_sidecar() {
    let fx = fixture();
    let long = "x".repeat(5000);
    let mut r = recorder(&fx, "call_bound", &long);
    r.begin();
    let rec = r
        .complete(
            Some(resolved()),
            None,
            ExecutionOutcome::Exited,
            Some(0),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    match rec.requested {
        CommandSpec::Requested { command_text, .. } => {
            assert!(command_text.chars().count() <= 513, "入参须有界")
        }
        _ => panic!("请求形态应保持 Requested"),
    }
}

#[test]
fn recorder_refuses_to_assume_a_check_from_command_text() {
    // 命令文本自称检查身份不被采纳：默认 check_id 为 None。
    let fx = fixture();
    let r = recorder(&fx, "call_chk", "cargo test --check-id c1");
    assert!(r.check_id.is_none());
    // 只有宿主显式登记才关联。
    let r2 = r.with_check("registered-check");
    assert_eq!(r2.check_id.as_deref(), Some("registered-check"));
}

#[test]
fn intent_alone_never_counts_as_execution() {
    // 只有 intent、没有回执 → 账本里没有执行记录，不能满足任何检查。
    let fx = fixture();
    let mut r = recorder(&fx, "call_intent", "true");
    r.begin();
    let ledger = fx.svc.ledger("sess1");
    assert!(ledger.receipts.is_empty(), "意向不得被当成已执行证据");
    let _ = &mut r;
}

#[test]
fn user_decision_kinds_stay_distinct() {
    // 「已人工验证」与「豁免」是两种决定，不能互相顶替。
    assert_ne!(UserDecisionKind::Confirmed, UserDecisionKind::Waived);
}

#[test]
fn labelled_credentials_are_redacted_before_recording() {
    let text =
        super::redact("API_KEY=secret-one token='secret-two' Authorization: Bearer secret-three");
    assert!(!text.contains("secret-one"));
    assert!(!text.contains("secret-two"));
    assert!(!text.contains("secret-three"));
    assert!(text.contains("[REDACTED]"));
}
