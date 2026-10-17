//! #55 可重复评测夹具。
//!
//! 目标很窄但很硬：**同一份输入重复跑，结论必须稳定**。
//! 评测一旦不可重复，"改了之后变好了"就是无法判定的说法。
//!
//! 判据（消费 S1 已落地的证据语义，不另起一套）：
//!
//! - **证据 / 完成 / 权限三者分离**：`completed`、`report`、退出 0、wait 命中
//!   **都不等于**验收通过；只有 `acceptance::evaluate` 推出的 `passed` 才算。
//! - **重复运行结论一致**：同输入 N 次，同一份快照摘要必须逐字节相同。
//! - **注入扰动能被发现**：改动被验证范围里的一个字节，结论必须从 passed 变 stale/failed。
//!   一条"怎么改都过"的评测等于没有评测。
//! - **隔离**：数据根用临时目录，不读真实配置、不读真实密钥。

use crate::core::delivery::acceptance::{EvalContext, Ledger, evaluate, validate_contract_shape};
use crate::core::delivery::types::*;
use crate::core::delivery::version::{CaptureOptions, RootResolver, ScopeVersion, capture_scope};

/// 一次评测的输入与输出。
pub struct EvalCase {
    /// 用例名。
    pub name: String,
    /// 判定摘要（稳定字符串，可逐字节比对）。
    pub verdict: String,
    /// 是否算通过。
    pub passed: bool,
}

/// 隔离工作区：一个临时数据根 + 一个根解析器。
pub struct Sandbox {
    _dir: tempfile::TempDir,
    pub roots: RootResolver,
}

impl Sandbox {
    /// 新建隔离沙箱（**不读真实配置与密钥**）。
    pub fn new() -> Sandbox {
        let dir = tempfile::tempdir().expect("临时数据根");
        let mut roots = RootResolver::new();
        roots.insert("ws".to_string(), dir.path().to_path_buf());
        Sandbox { _dir: dir, roots }
    }

    pub fn ws(&self) -> std::path::PathBuf {
        self.roots.get("ws").cloned().unwrap()
    }

    pub fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.ws().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, bytes).unwrap();
    }
}

fn owner() -> RunIdentity {
    RunIdentity::root("eval-sess", "eval-run")
}

fn p(rel: &str) -> PathRef {
    PathRef {
        root_id: "ws".into(),
        relative_path: rel.into(),
    }
}

fn contract() -> TaskContract {
    TaskContract {
        delivery_goal: Default::default(),
        schema: SCHEMA,
        task_id: "eval-task".into(),
        revision: 1,
        owner: owner(),
        requirements: vec![Requirement {
            id: "r1".into(),
            title: "报告已产出".into(),
            required: true,
            check_ids: vec!["ci".into()],
            artifact_ids: vec!["a1".into()],
        }],
        artifacts: vec![ArtifactRequirement {
            id: "a1".into(),
            path: p("out/report.md"),
            kind: ArtifactKind::Text,
            constraints: vec![ArtifactConstraint::Exists],
            required: true,
        }],
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
            command: Some(CommandSpec::Requested {
                command_text: "cargo test --workspace".into(),
                shell_hint: Some("src-tauri".into()),
            }),
            scope: VerificationScope {
                roots: Vec::new(),
                inputs: vec![p("out/report.md")],
                outputs: Vec::new(),
                dependency_files: Vec::new(),
                environment_keys: Vec::new(),
            },
            required: true,
            expectation: CheckExpectation::Command {
                allowed_exit_codes: vec![0],
            },
        }],
        review_policy: ReviewPolicy {
            required: false,
            ..Default::default()
        },
        scope_changes: Vec::new(),
    }
}

/// 一份"全部齐活"的账本：产物存在 + 有真实通过回执。
fn satisfied_ledger(c: &TaskContract, after: ScopeVersion) -> Ledger {
    let mut l = Ledger::new();
    let mut r = owner();
    r.actor_id = "root".into();
    r.actor_run_id = "eval-run".into();
    l.receipts.insert(
        "execution_ci".into(),
        ExecutionReceipt {
            platform: Some(std::env::consts::OS.into()),
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "eval-task".into(),
                revision: 1,
            }),
            schema: SCHEMA,
            id: "execution_ci".into(),
            owner: r,
            check_id: Some("ci".into()),
            requested: CommandSpec::Requested {
                command_text: "cargo test --workspace".into(),
                shell_hint: Some("src-tauri".into()),
            },
            resolved: None,
            cwd: Some(PathRef {
                root_id: "ws".into(),
                relative_path: ".".into(),
            }),
            started_at: "0".into(),
            finished_at: "1".into(),
            outcome: ExecutionOutcome::Exited,
            exit_code: Some(0),
            signal: None,
            output_ref: None,
            output_truncated: false,
            before: Some(after.clone()),
            after: Some(after),
            issuer: ReceiptIssuer::Host,
            reason: None,
        },
    );
    let _ = c;
    l
}

fn verdict_of(snap: &AcceptanceSnapshot) -> String {
    // 稳定摘要：不含时刻、不含随机 id，逐字节可比。
    let items: Vec<String> = snap
        .items
        .iter()
        .map(|i| {
            let codes: Vec<&str> = i.reasons.iter().map(|r| r.code.as_str()).collect();
            format!("{}:{:?}:{}", i.requirement_id, i.status, codes.join(","))
        })
        .collect();
    format!(
        "{}|waived={}|{}",
        format!("{:?}", snap.overall).to_ascii_lowercase(),
        snap.waived_count,
        items.join(";")
    )
}

fn evaluate_once(sb: &Sandbox, ledger: &Ledger) -> AcceptanceSnapshot {
    let c = contract();
    validate_contract_shape(&c).expect("契约形状应合法");
    let ctx = EvalContext {
        contract: &c,
        ledger,
        roots: &sb.roots,
        capture: CaptureOptions::default(),
    };
    evaluate(&ctx).expect("求值应成功").into_snapshot()
}

impl crate::core::delivery::acceptance::EvalOutcome {
    fn into_snapshot(self) -> AcceptanceSnapshot {
        AcceptanceSnapshot {
            task_id: "eval-task".into(),
            contract_revision: 1,
            revision: 1,
            items: self.items,
            overall: self.overall,
            waived_count: self.waived_count,
            evaluated_version: self.evaluated_version,
            reasons: self.reasons,
        }
    }
}

/// 重复运行评测，返回 N 次结论（用于判定可重复性）。
pub fn repeat(cases: usize) -> Vec<EvalCase> {
    let sb = Sandbox::new();
    sb.write("out/report.md", "# 报告\n".as_bytes());
    let c = contract();
    let after = capture_scope(&sb.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let ledger = satisfied_ledger(&c, after);
    (0..cases)
        .map(|i| {
            let snap = evaluate_once(&sb, &ledger);
            EvalCase {
                name: format!("repeat-{i}"),
                verdict: verdict_of(&snap),
                passed: snap.overall == AcceptanceStatusKind::Passed,
            }
        })
        .collect()
}

/// 扰动评测：改动一个字节后结论必须改变。
pub fn perturbed() -> EvalCase {
    let sb = Sandbox::new();
    sb.write("out/report.md", "# 报告\n".as_bytes());
    let c = contract();
    let after = capture_scope(&sb.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let ledger = satisfied_ledger(&c, after);
    let before_verdict = verdict_of(&evaluate_once(&sb, &ledger));

    // 扰动：改动被验证范围内的内容。
    sb.write("out/report.md", "# 报告（改了一个字节）\n".as_bytes());
    let after_verdict = verdict_of(&evaluate_once(&sb, &ledger));
    EvalCase {
        name: "perturbed".into(),
        verdict: format!("{before_verdict} => {after_verdict}"),
        passed: before_verdict != after_verdict,
    }
}

/// 证据 / 完成 / 权限分离：空账本 + 产物齐全也**不得**通过。
///
/// 这是评测最容易被绕过的地方——「产物在」不等于「检查过了」。
pub fn evidence_completion_separation() -> EvalCase {
    let sb = Sandbox::new();
    sb.write("out/report.md", "# 报告\n".as_bytes());
    // 账本里只有用户决定（一种"完成"的表述），没有任何执行回执。
    let mut l = Ledger::new();
    let mut r = owner();
    r.actor_id = "root".into();
    l.decisions.insert(
        "decision_1".into(),
        UserDecision {
            binding: Some(crate::core::delivery::types::ContractBinding {
                task_id: "eval-task".into(),
                revision: 1,
            }),
            id: "decision_1".into(),
            owner: r,
            item_ids: vec!["r1".into()],
            decision: UserDecisionKind::Confirmed,
            scope_version: None,
            prompt_id: "ask_1".into(),
            answered_at: "0".into(),
        },
    );
    let snap = evaluate_once(&sb, &l);
    EvalCase {
        name: "user_says_done_is_not_evidence".into(),
        verdict: verdict_of(&snap),
        passed: snap.overall != AcceptanceStatusKind::Passed,
    }
}

/// 退出 0 不等于需求完成：一张"跑完了但检查没过"的回执。
pub fn exit_zero_is_not_completion() -> EvalCase {
    let sb = Sandbox::new();
    sb.write("out/report.md", "# 报告\n".as_bytes());
    let c = contract();
    let after = capture_scope(&sb.roots, &c.checks[0].scope, &CaptureOptions::default()).unwrap();
    let mut l = satisfied_ledger(&c, after);
    // 把回执改成"命令本身失败但进程正常退出"——形态上仍 exited，
    // 但退出码不在允许集合内。
    for r in l.receipts.values_mut() {
        r.exit_code = Some(101);
    }
    let snap = evaluate_once(&sb, &l);
    EvalCase {
        name: "failing_check_is_not_passed".into(),
        verdict: verdict_of(&snap),
        passed: snap.overall != AcceptanceStatusKind::Passed,
    }
}

/// 导出评测结果（供 `eval` 子命令消费；本函数是唯一的对外读口）。
pub fn report() -> serde_json::Value {
    let repeats = repeat(3);
    let stable = repeats.windows(2).all(|w| w[0].verdict == w[1].verdict);
    let sep = evidence_completion_separation();
    let exit = exit_zero_is_not_completion();
    let pert = perturbed();
    serde_json::json!({
        "repeatable": stable && repeats.iter().all(|c| c.passed),
        "verdicts": repeats.iter().map(|c| c.verdict.clone()).collect::<Vec<_>>(),
        "evidence_completion_separated": sep.passed,
        "evidence_completion_verdict": sep.verdict,
        "exit_zero_not_completion": exit.passed,
        "exit_zero_verdict": exit.verdict,
        "perturbation_detected": pert.passed,
        "perturbation": pert.verdict,
    })
}

#[cfg(test)]
#[path = "evaluation_tests.rs"]
mod tests;
