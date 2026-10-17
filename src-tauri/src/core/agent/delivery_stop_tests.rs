//! #16 统一收尾入口夹具：**逐个驱动生产收尾分支**，断言缺产物时均无 passed。
//!
//! 只测 `text_turn_action` 不足以验收本任务（见 s1-16 文档）——本文件对
//! `StopReason` 的**每一个生产取值**都跑一遍真实的 `gate_delivery_stop`，
//! 并断言下游是否派发修复。

use super::*;
use crate::core::agent::test_support::{make_core, make_runtime};
use crate::core::delivery::types::{ArtifactKind, ArtifactRequirement, ContractDraft, Requirement};
use crate::tools::pathutil::WriteRoots;

struct Fx {
    _dir: tempfile::TempDir,
    core: std::sync::Arc<AgentCore>,
    rt: std::sync::Arc<SessionRuntime>,
    task_id: String,
}

/// 建一个**有契约、且契约要求一项没有任何证据的检查**的会话——即"模型说做完了，
/// 但产物/检查证据不存在"的假完成现场。
fn fx_with_gap() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let roots = WriteRoots {
        data_dir: dir.path().to_path_buf(),
        workspace: dir.path().to_path_buf(),
        extra: Vec::new(),
        project_dir: None,
    };
    let core = make_core(&roots);
    let rt = make_runtime(roots.workspace.clone(), roots.data_dir.clone(), Vec::new());
    let identity = DeliveryService::identity_of(&rt);
    let resolver = DeliveryService::roots_from_runtime(&rt);
    // 契约要求一个**没有任何证据**的产物：模型说做完了，但文件不存在。
    let (root_id, root_abs) = resolver
        .iter()
        .next()
        .map(|(k, v)| (k.clone(), v.to_string_lossy().to_string()))
        .expect("会话快照应有根");
    let draft = ContractDraft {
        delivery_goal: Default::default(),
        requirements: vec![Requirement {
            id: "r1".into(),
            title: "报告已产出".into(),
            required: true,
            check_ids: vec![],
            artifact_ids: vec!["a1".into()],
        }],
        artifacts: vec![ArtifactRequirement {
            id: "a1".into(),
            kind: ArtifactKind::Text,
            path: crate::core::delivery::types::PathRef {
                root_id: root_id.clone(),
                relative_path: "out/report.md".into(),
            },
            constraints: vec![crate::core::delivery::types::ArtifactConstraint::Exists],
            required: true,
        }],
        checks: Vec::new(),
        // 要求审查：无需（也无法在 S1 阶段）伪造"豁免独立审查"的路由依据。
        review_policy: crate::core::delivery::types::ReviewPolicy {
            required: true,
            ..Default::default()
        },
        scope_changes: Vec::new(),
        roots: vec![crate::core::delivery::types::RootIdentity {
            root_id,
            kind: crate::core::delivery::types::RootKind::Workspace,
            absolute: root_abs,
        }],
    };
    let (contract, _) = core
        .delivery
        .create_contract_with(&identity, &draft, &resolver, "op-create-1", |contract| {
            let prepared = crate::core::workflow::service::prepare(
                contract,
                &resolver,
                &rt.data_dir,
                "temporary",
                None,
                &Default::default(),
            )
            .map_err(|message| crate::core::delivery::DeliveryError::Save { message })?;
            crate::core::workflow::service::persist(&rt.data_dir, &prepared)
                .map_err(|message| crate::core::delivery::DeliveryError::Save { message })
        })
        .expect("建契约");
    Fx {
        _dir: dir,
        core,
        rt,
        task_id: contract.task_id,
    }
}

/// 无契约的普通问答现场。
fn fx_without_contract() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let roots = WriteRoots {
        data_dir: dir.path().to_path_buf(),
        workspace: dir.path().to_path_buf(),
        extra: Vec::new(),
        project_dir: None,
    };
    let core = make_core(&roots);
    let rt = make_runtime(roots.workspace.clone(), roots.data_dir.clone(), Vec::new());
    Fx {
        _dir: dir,
        core,
        rt,
        task_id: String::new(),
    }
}

fn params() -> DriveParams {
    DriveParams {
        max_steps: 12,
        ..DriveParams::default()
    }
}

fn gate_at(
    fx: &Fx,
    step: usize,
    reason: StopReason,
    cancelled: bool,
) -> (StopAction, CompletionDecision) {
    let (a, d, _) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "run-test",
        &params(),
        step,
        reason,
        cancelled,
    );
    (a, d)
}

// ---------- 逐分支：无契约的旧问答一律沿原收尾 ----------

#[test]
fn plain_qa_without_contract_never_gated_on_any_branch() {
    let fx = fx_without_contract();
    for reason in [
        StopReason::Text,
        StopReason::SuggestItems,
        StopReason::BatchDone,
        StopReason::MaxSteps,
    ] {
        let (a, d) = gate_at(&fx, 1, reason, false);
        assert_eq!(a, StopAction::Stop, "{reason:?} 不该为普通问答加门");
        assert!(d.allows_completion(), "{reason:?} 应沿原收尾");
        assert!(d.snapshot().is_none(), "无契约不得编造快照");
    }
}

// ---------- 逐分支：缺产物时均无 passed ----------

#[test]
fn text_stop_with_missing_evidence_repairs_instead_of_completing() {
    let fx = fx_with_gap();
    let (a, d) = gate_at(&fx, 1, StopReason::Text, false);
    assert!(!d.allows_completion(), "缺证据不得 passed");
    assert_eq!(a, StopAction::Repair);
    assert!(!d.gaps().is_empty());
}

#[test]
fn suggest_items_early_return_cannot_bypass_the_gate() {
    // 规格特别点名：suggest 只是建议项，不能绕门。
    let fx = fx_with_gap();
    let (a, d) = gate_at(&fx, 1, StopReason::SuggestItems, false);
    assert!(!d.allows_completion());
    assert_eq!(a, StopAction::Repair);
}

#[test]
fn batch_done_cannot_bypass_the_gate() {
    let fx = fx_with_gap();
    let (a, d) = gate_at(&fx, 1, StopReason::BatchDone, false);
    assert!(!d.allows_completion());
    assert_eq!(a, StopAction::Repair);
}

#[test]
fn budget_close_out_stops_with_blocked_and_keeps_partial() {
    // max_steps / force_report 收口后绝不另开修复 run。
    let fx = fx_with_gap();
    let (a, d) = gate_at(&fx, params().max_steps, StopReason::MaxSteps, false);
    assert_eq!(a, StopAction::Stop);
    assert!(!d.allows_completion());
    assert_eq!(d.as_str(), "blocked");
    // 阻塞理由可定位，且**不是**运行 error。
    assert!(
        matches!(d, CompletionDecision::Blocked { ref reason, .. } if reason.contains("步数上限"))
    );
}

// ---------- 取消优先 ----------

#[test]
fn cancellation_on_every_branch_never_dispatches_repair() {
    let fx = fx_with_gap();
    for reason in [
        StopReason::Text,
        StopReason::SuggestItems,
        StopReason::BatchDone,
    ] {
        let (a, d) = gate_at(&fx, 1, reason, true);
        assert_eq!(a, StopAction::Stop, "{reason:?} 取消后不得派发修复");
        assert!(!d.allows_completion());
        assert_eq!(d.as_str(), "cancelled");
    }
}

#[test]
fn cancellation_preserves_already_happened_facts() {
    let fx = fx_with_gap();
    let (_, d) = gate_at(&fx, 1, StopReason::Text, true);
    assert!(d.snapshot().is_some(), "取消不丢弃已发生的事实");
}

// ---------- 预算真的会被扣，且跨调用保留 ----------

#[test]
fn repair_budget_is_consumed_and_survives_across_gate_calls() {
    let fx = fx_with_gap();
    let mut last = None;
    for step in 1..=4 {
        let (a, _, b) = gate_delivery_stop(
            &fx.core,
            &fx.rt,
            "run-test",
            &params(),
            step,
            StopReason::Text,
            false,
        );
        last = Some((a, b));
    }
    let (a, b) = last.unwrap();
    // 到第 4 轮早已超上限：必须停下并阻塞。
    assert_eq!(a, StopAction::Stop);
    assert!(b.rounds_used >= crate::core::delivery::completion::MAX_REPAIR_ROUNDS);
    // 预算在**新一轮 gate 调用**里仍在（未归零）。
    let (a2, d2, b2) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "run-test-2",
        &params(),
        1,
        StopReason::Text,
        false,
    );
    assert_eq!(b2.rounds_used, b.rounds_used, "预算不因新 run 归零");
    assert_eq!(a2, StopAction::Stop);
    assert_eq!(d2.as_str(), "blocked");
}

#[test]
fn repair_feedback_is_written_into_history_as_a_user_message() {
    // 缺项必须回到模型**看得见**的地方（history），否则模型无从知道要修什么。
    let fx = fx_with_gap();
    let (a, d, b) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "run-test",
        &params(),
        1,
        StopReason::Text,
        false,
    );
    assert_eq!(a, StopAction::Repair);
    inject_repair_feedback(&fx.rt, &d, &b);
    let h = fx.rt.history.lock().unwrap();
    let last = h.last().expect("应注入一条反馈");
    let text = last
        .content
        .iter()
        .find_map(|c| match c {
            crate::core::types::Content::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("反馈应是文本");
    assert!(text.contains("acceptance-gap"));
    assert!(text.contains("acceptance"));
    assert!(text.contains("repair_rounds_max"));
}

// ---------- 幂等与跨会话 ----------

#[test]
fn repeated_gate_calls_do_not_double_count_budget() {
    // 同一停止点重复求值（重复事件 / 重放）不得凭空吃掉预算。
    let fx = fx_with_gap();
    let (_, _, b1) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "run-test",
        &params(),
        1,
        StopReason::Text,
        false,
    );
    let (_, _, b2) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "run-test",
        &params(),
        1,
        StopReason::Text,
        false,
    );
    assert_eq!(b1.rounds_used, b2.rounds_used);
}

#[test]
fn budget_of_another_session_is_never_borrowed() {
    let fx = fx_with_gap();
    // 先把本会话预算用掉。
    let (_, _, used) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "run-test",
        &params(),
        1,
        StopReason::Text,
        false,
    );
    assert_eq!(used.rounds_used, 1);

    // 另一个会话（**同 data_dir、不同 session id**）预算独立——
    // 注意不能用 make_runtime：它固定生成 `rt-test`，那其实是同一个会话。
    let other = {
        let mut o = SessionRuntime::new(
            "rt-other".to_string(),
            fx.rt.workspace.clone(),
            fx.core.data_dir.clone(),
        );
        if let Some(r) = std::sync::Arc::get_mut(&mut o) {
            r.is_main_session = true;
        }
        std::sync::Arc::new(o)
    };
    // 该会话自己没有契约 → 沿原收尾，且不得借用到别会话的预算。
    other
        .delivery_task_id
        .lock()
        .unwrap()
        .replace(fx.task_id.clone());
    let (_, _, fresh) = gate_delivery_stop(
        &fx.core,
        &other,
        "run-other",
        &params(),
        1,
        StopReason::Text,
        false,
    );
    // 本会话的契约不会出现在别的会话里 → 该会话无契约 → 不借用预算。
    assert_eq!(fresh.rounds_used, 0, "预算不得跨会话串用");
}

#[test]
fn previous_contract_does_not_block_a_new_plain_conversation() {
    let fx = fx_with_gap();
    assert!(fx.rt.begin_run("second-run"));
    let p = params();
    let (action, decision, _) = gate_delivery_stop(
        &fx.core,
        &fx.rt,
        "second-run",
        &p,
        1,
        StopReason::Text,
        false,
    );
    assert_eq!(action, StopAction::Stop);
    assert!(
        decision.allows_completion(),
        "previous run contract must not be active: {decision:?}"
    );
    assert!(
        fx.core
            .delivery
            .read_contract(&fx.rt.id, &fx.task_id)
            .is_some(),
        "old contract history stays readable"
    );
    assert!(
        fx.core
            .delivery
            .active_contract_for_run(&fx.rt.id, "second-run")
            .is_none()
    );
}
