//! #16 统一收尾入口：把"停止运行"与"申请交付通过"分开。
//!
//! 审计结论（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：
//! drive 的**正常返回点**只有两类会走到 `RunTerminalKind::Done`：
//!
//! | 分支 | 位置 | 收尾动因 |
//! |---|---|---|
//! | 纯文本收尾 | [`TextTurnAction::Finish`](drive::TextTurnAction::Finish) | 模型说完了 |
//! | 批次收尾 | `batch_done` | `suggest_items` 提前返回 / `force_report` 汇报轮 / 计划收口 |
//!
//! 其余返回点（取消、provider 错误、监督终止、空响应、文本轮上限、panic）都以
//! `RunTerminalKind::Cancelled` / `Error` 结束——它们**按原运行终态停止并附未完成验收**，
//! **不因完成门再发起模型调用**。
//!
//! `max_steps` 耗尽（`force_report` 之后）同样不另开修复 run：按规格记 partial/blocked。
//!
//! 所以本模块只需在**这两个**正常分支前插入 [`gate_delivery_stop`]，且它**必须**返回
//! `Stop` 时才真的停。`batch_done` 分支同时覆盖 `suggest_items` 提前返回——
//! 规格特别点名要测的那条路径。

use super::drive::DriveParams;
use super::runtime::{AgentCore, SessionRuntime};
pub(super) use crate::core::delivery::completion::{
    CompletionDecision, RepairBudget, StopAction, gate, repair_feedback, stop_action,
};
use crate::core::delivery::service::DeliveryService;
use std::sync::Arc;

/// 收尾原因（用于日志与反馈文案，不参与判定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 纯文本收尾（模型说完了）。
    Text,
    /// `suggest_items` 提前返回。
    SuggestItems,
    /// `batch_done`（含 force_report 汇报轮收口）。
    BatchDone,
    /// 步数上限耗尽。
    MaxSteps,
    /// 计划任务运行收尾（**不因 runtime 是 FullAccess 而跳过验收**）。
    TaskRun,
    /// 子 run 收尾（完整交付由 #18 校验，本处只看完成门结论）。
    SubRun,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Text => "text",
            StopReason::SuggestItems => "suggest_items",
            StopReason::BatchDone => "batch_done",
            StopReason::MaxSteps => "max_steps",
            StopReason::TaskRun => "task_run",
            StopReason::SubRun => "sub_run",
        }
    }
}

/// 统一收尾入口（**drive 内唯一**判定"可以交付通过"的地方）。
///
/// 返回 [`StopAction`]：
/// - `Repair` → 调用方**必须**把缺项写回历史并继续跑一轮，不得直接收尾；
/// - `Stop` → 调用方按原路径收尾，成果照常保留。
///
/// 无契约（普通问答）时返回 `Stop` 且不产生任何快照：**不为每条回答加门**。
#[allow(clippy::too_many_arguments)]
fn gate_delivery_stop_inner(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    run_id: &str,
    params: &DriveParams,
    next_step: usize,
    reason: StopReason,
    cancelled: bool,
) -> (StopAction, CompletionDecision, RepairBudget) {
    let svc: &DeliveryService = &core.delivery;
    let identity = DeliveryService::identity_of(rt);
    let owner_id = rt.root_session_id.as_deref().unwrap_or(&rt.id);
    if let Err(error) = svc.store().ensure_readable(owner_id) {
        let snapshot = crate::core::delivery::AcceptanceSnapshot {
            task_id: "unreadable_delivery".into(),
            contract_revision: 0,
            revision: 0,
            items: Vec::new(),
            overall: crate::core::delivery::AcceptanceStatusKind::Blocked,
            waived_count: 0,
            evaluated_version: None,
            reasons: vec![crate::core::delivery::Reason {
                code: "delivery_unreadable".into(),
                message: error.to_string(),
                target_id: None,
            }],
        };
        return (
            StopAction::Stop,
            CompletionDecision::Blocked {
                snapshot,
                gaps: Vec::new(),
                reason: "交付状态不可读".into(),
            },
            RepairBudget::default(),
        );
    }
    let contract = rt
        .delivery_task_id
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|id| svc.read_contract(owner_id, id))
        .filter(|contract| contract.owner.root_run_id == identity.root_run_id)
        .or_else(|| svc.active_contract_for_run(owner_id, &identity.root_run_id));
    let Some(contract) = contract else {
        // 无交付契约：沿原收尾路径。
        return (
            StopAction::Stop,
            CompletionDecision::Allow {
                snapshot: None,
                reason: "无交付契约：沿原收尾路径".into(),
            },
            RepairBudget::default(),
        );
    };
    let owner = DeliveryService::identity_of(rt);
    let roots = DeliveryService::roots_from_runtime(rt);
    let budget = svc.load_budget(owner_id, &contract.task_id);

    // 步数上限已耗尽：绝不另开修复 run，直接按阻塞收尾（保留 partial）。
    if next_step >= params.max_steps {
        let decision = CompletionDecision::Blocked {
            snapshot: crate::core::delivery::AcceptanceSnapshot {
                task_id: contract.task_id.clone(),
                contract_revision: contract.revision,
                revision: 0,
                items: Vec::new(),
                overall: crate::core::delivery::AcceptanceStatusKind::Blocked,
                waived_count: 0,
                evaluated_version: None,
                reasons: vec![crate::core::delivery::Reason {
                    code: "run_budget_exhausted".into(),
                    message: format!(
                        "步数上限已用尽（{}/{}），不再另开修复 run：保留已交付部分并标记未通过",
                        next_step, params.max_steps
                    ),
                    target_id: None,
                }],
            },
            gaps: Vec::new(),
            reason: format!("步数上限已用尽（{}/{}）", next_step, params.max_steps),
        };
        return (StopAction::Stop, decision, budget);
    }

    // 求值 + 落盘；落盘失败时 save_ok = false，完成门据此阻止通过。
    let (snapshot, save_ok) = match svc.evaluate(owner_id, &contract.task_id, &roots) {
        Ok(v) => v,
        Err(_) => {
            // 求值本身失败（如契约形状非法）：完成状态未知，按阻塞处理。
            let decision = gate(
                Some(&contract),
                None,
                &budget,
                cancelled,
                svc.store()
                    .whole_task_cancelled(owner_id, &contract.task_id),
                true,
            );
            return (StopAction::Stop, decision, budget);
        }
    };
    let whole_cancelled = svc
        .store()
        .whole_task_cancelled(owner_id, &contract.task_id);
    let decision = gate(
        Some(&contract),
        Some(&snapshot),
        &budget,
        cancelled,
        whole_cancelled,
        save_ok,
    );

    let action = stop_action(&decision, next_step, params.max_steps);
    if action == StopAction::Repair {
        // 先落盘再继续：预算写丢了就等于悄悄放开上限。
        let mut next = budget.clone();
        if let CompletionDecision::Repair { gaps, .. } = &decision {
            next.consume(gaps);
        }
        next.consume_steps(1);
        if svc
            .save_budget(
                owner_id,
                &owner,
                &contract.task_id,
                &next,
                &format!("budget_{run_id}_{next_step}"),
            )
            .is_ok()
        {
            crate::core::session_log::info(
                rt,
                &format!(
                    "完成门（#16）收尾于 {}：{} 项缺项可修，消耗第 {}/{} 轮",
                    reason.as_str(),
                    decision.gaps().len(),
                    next.rounds_used,
                    crate::core::delivery::completion::MAX_REPAIR_ROUNDS,
                ),
            );
        } else {
            // 预算没能落盘就不许继续：不能一边记不住、一边继续烧预算。
            return (
                StopAction::Stop,
                CompletionDecision::Blocked {
                    snapshot: snapshot.clone(),
                    gaps: decision.gaps().to_vec(),
                    reason: "修复预算未能落盘：停止而不继续，避免预算被悄悄放开".into(),
                },
                budget,
            );
        }
        return (action, decision, next);
    }
    crate::core::session_log::info(
        rt,
        &format!(
            "完成门（#16）收尾于 {}：{}",
            reason.as_str(),
            decision.as_str()
        ),
    );
    (action, decision, budget)
}

/// Settle every root stop through the frozen workflow; missing/corrupt state is a real blocker.
#[allow(clippy::too_many_arguments)]
pub fn gate_delivery_stop(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    run_id: &str,
    params: &DriveParams,
    next_step: usize,
    reason: StopReason,
    cancelled: bool,
) -> (StopAction, CompletionDecision, RepairBudget) {
    let (action, decision, budget) =
        gate_delivery_stop_inner(core, rt, run_id, params, next_step, reason, cancelled);
    if action == StopAction::Stop
        && rt.root_session_id.is_none()
        && let Some(snapshot) = decision.snapshot()
        && let Err(message) = crate::core::workflow::service::settle(
            &rt.data_dir,
            &DeliveryService::identity_of(rt),
            snapshot,
        )
    {
        rt.save_clean
            .store(false, std::sync::atomic::Ordering::SeqCst);
        return (
            StopAction::Stop,
            CompletionDecision::Blocked {
                snapshot: snapshot.clone(),
                gaps: decision.gaps().to_vec(),
                reason: format!("工作流终态未能保存：{message}"),
            },
            budget,
        );
    }
    (action, decision, budget)
}

/// 把缺项写回模型可见的 `data`（**不是 warnings**：后者模型一个字都收不到）。
pub fn inject_repair_feedback(
    rt: &Arc<SessionRuntime>,
    decision: &CompletionDecision,
    budget: &RepairBudget,
) {
    let gaps = decision.gaps();
    if gaps.is_empty() {
        return;
    }
    let text = serde_json::to_string(&repair_feedback(gaps, budget)).unwrap_or_default();
    rt.history.lock().unwrap().push(
        crate::core::types::Message::user_text(format!("<acceptance-gap>{text}</acceptance-gap>"))
            .stamped(),
    );
}

#[cfg(test)]
#[path = "delivery_stop_tests.rs"]
mod tests;
