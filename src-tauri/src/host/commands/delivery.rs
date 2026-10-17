//! 交付与验收的只读 IPC（[#54](../../../../docs/tools-and-safety.md) / #16）。
//!
//! 命令层只做参数校验 + 转调 core，**不放业务逻辑**。
//!
//! 跨层纪律：
//!
//! - **只读**：这里没有任何写接口，也没有 `set_passed`。通过只能由求值推导出来，
//!   模型同形 JSON 无效。
//! - **不加载历史、不执行工具**：恢复出来的只是快照与中断状态。
//! - **没有快照就返回 `None`**：老会话缺少本能力时界面显示"未验证"，
//!   绝不补造一个"未通过"以外的结论。

use super::util::Core;
use crate::core::agent::AgentCore;
use crate::core::delivery::completion::{self, CompletionDecision};
use crate::core::delivery::service::DeliveryService;
use crate::core::delivery::types::AcceptanceSnapshot;
use std::sync::Arc;

/// 会话的验收投影：快照 + 完成门裁决摘要 + 修复预算。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeliveryProjection {
    /// 任务 id（无契约时为空串，界面据此显示"本会话没有交付契约"）。
    pub task_id: String,
    /// 契约修订。
    pub contract_revision: u64,
    /// 验收快照（无契约 / 老会话时为 `None`）。
    pub snapshot: Option<AcceptanceSnapshot>,
    /// 完成门裁决（`allow` / `repair` / `blocked` / `cancelled`）。
    pub decision: &'static str,
    /// **是否放行交付通过**：只有验收器推出的 `passed`（或无契约旧问答）才为真。
    pub allows_completion: bool,
    /// 缺项（供界面显示"还差什么"）。
    pub gaps: Vec<serde_json::Value>,
    /// 已用修复轮 / 上限。
    pub repair_rounds_used: u32,
    pub repair_rounds_max: u32,
}

#[tauri::command]
pub fn get_delivery_projection(
    core: Core<'_>,
    session_id: String,
) -> Result<DeliveryProjection, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    Ok(project_for(&core, &rt))
}

/// 复用 drive 已算好的裁决；没有就**按只读重算**（不派发任何东西）。
fn project_for(
    core: &Arc<AgentCore>,
    rt: &Arc<crate::core::agent::SessionRuntime>,
) -> DeliveryProjection {
    let svc = &core.delivery;
    if svc.store().ensure_readable(&rt.id).is_err() {
        return DeliveryProjection {
            task_id: "unreadable_delivery".into(),
            contract_revision: 0,
            snapshot: None,
            decision: "blocked",
            allows_completion: false,
            gaps: vec![
                serde_json::json!({"reason_code":"delivery_unreadable","message":"交付状态不可读，不能当作无契约"}),
            ],
            repair_rounds_used: 0,
            repair_rounds_max: completion::MAX_REPAIR_ROUNDS,
        };
    }
    let selected = rt
        .delivery_task_id
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|task| svc.read_contract(&rt.id, task))
        .or_else(|| svc.active_contract(&rt.id));
    let Some(contract) = selected else {
        return DeliveryProjection {
            task_id: String::new(),
            contract_revision: 0,
            snapshot: None,
            decision: "allow",
            allows_completion: true,
            gaps: Vec::new(),
            repair_rounds_used: 0,
            repair_rounds_max: completion::MAX_REPAIR_ROUNDS,
        };
    };
    let budget = svc.load_budget(&rt.id, &contract.task_id);
    let whole_cancelled = svc.store().whole_task_cancelled(&rt.id, &contract.task_id);

    // External edits invalidate evidence even if no new run has started. Never reuse a cached passed decision.
    let roots = DeliveryService::roots_from_runtime(rt);
    let decision: CompletionDecision = match svc.evaluate_current(&rt.id, &contract.task_id, &roots)
    {
        Ok(snapshot) => completion::gate(
            Some(&contract),
            Some(&snapshot),
            &budget,
            false,
            whole_cancelled,
            true,
        ),
        Err(_) => completion::gate(
            Some(&contract),
            None,
            &budget,
            false,
            whole_cancelled,
            false,
        ),
    };
    DeliveryProjection {
        task_id: contract.task_id.clone(),
        contract_revision: contract.revision,
        snapshot: decision.snapshot().cloned(),
        decision: decision.as_str(),
        allows_completion: decision.allows_completion(),
        gaps: decision
            .gaps()
            .iter()
            .map(|g| {
                serde_json::json!({
                    "requirement_id": g.requirement_id,
                    "check_id": g.check_id,
                    "reason_code": g.reason_code,
                    "message": g.message,
                })
            })
            .collect(),
        repair_rounds_used: budget.rounds_used,
        repair_rounds_max: completion::MAX_REPAIR_ROUNDS,
    }
}
