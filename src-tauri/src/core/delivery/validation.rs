//! 子交付宿主校验（任务 #18，[docs/subagents](../../../../docs/subagents.md)）。
//!
//! 子代理说"做完了"，父任务**凭什么采信**？这里给出唯一答案：把那份结构化交付结果
//! 按固定顺序逐层校验，每层都产出稳定 reason code 与 `item_id`——不是一个布尔值。
//!
//! 校验顺序是**契约的一部分**，不得调整（[任务文档](../../../../docs/tools-and-safety.md)）：
//!
//! 1. 身份 / 契约 revision
//! 2. 字段完整
//! 3. 完成与未完成互斥
//! 4. 引用及版本
//! 5. 必需检查
//! 6. 审查 findings
//! 7. 终态一致性
//!
//! 为什么单列第 7 步：`report` + 字段合法**仍然不等于**完整交付。`partial`（撞顶逼出来的
//! 收尾）、`budget`、`no_report` 都保留成果，但都不能判为完整——把它们和字段校验混在一起，
//! "看起来很完整的一份 partial" 就会通过。
//!
//! 另两条不可让步的纪律：
//!
//! - **父不得靠拼接文本把 invalid 变 passed**：本模块只认 [`Ledger`] 里宿主已采纳的记录，
//!   报告正文一字不看。
//! - **可重算、不可编辑**：校验是纯函数（重算即得同一结论），交付结果是不可变记录。
//!   不存在"修一下状态字段让它通过"的路径。

use super::acceptance::{self, EvalContext, Ledger};
use super::artifacts;
use super::types::*;
use super::version::{CaptureOptions, RootResolver};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 校验结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryVerdict {
    /// 父任务可依赖这份交付（仍不代表契约整体通过——那由 #54 求值决定）。
    Accepted,
    /// 不可依赖。`reasons` 逐条说明阻断点。
    Rejected,
}

impl DeliveryVerdict {
    /// wire 串。
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryVerdict::Accepted => "accepted",
            DeliveryVerdict::Rejected => "rejected",
        }
    }
}

/// 一条阻断原因（code 与 `item_id` 稳定，UI 与模型都据此定位）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockingReason {
    /// 稳定原因码。
    pub code: String,
    /// 指向的需求 / 产物 / 检查 id；身份级问题为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// 可定位说明。
    pub message: String,
    /// 是否为阻断级（false = 建议性，不自动失败）。
    pub blocking: bool,
}

impl BlockingReason {
    fn blocking(code: &str, item: Option<&str>, message: impl Into<String>) -> Self {
        BlockingReason {
            code: code.into(),
            item_id: item.map(str::to_string),
            message: message.into(),
            blocking: true,
        }
    }
    fn advisory(code: &str, item: Option<&str>, message: impl Into<String>) -> Self {
        BlockingReason {
            code: code.into(),
            item_id: item.map(str::to_string),
            message: message.into(),
            blocking: false,
        }
    }
}

/// 校验结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryValidation {
    /// 结论。
    pub verdict: DeliveryVerdict,
    /// 阻断原因（按校验顺序）。
    #[serde(default)]
    pub reasons: Vec<BlockingReason>,
    /// 建议性原因（不自动失败）。
    #[serde(default)]
    pub advisories: Vec<BlockingReason>,
    /// 交付结果 id（便于事件与 UI 直接引用）。
    pub delivery_id: String,
    /// 收尾形态（终态一致性检查的输入）。
    pub ended: EndedKind,
}

impl DeliveryValidation {
    /// 是否可被父任务依赖。
    pub fn accepted(&self) -> bool {
        self.verdict == DeliveryVerdict::Accepted
    }
    /// 阻断原因条数。
    pub fn blocking_count(&self) -> usize {
        self.reasons.iter().filter(|r| r.blocking).count()
    }
    /// 供 `sub:done` / `wait` 出参使用的紧凑摘要。
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "verdict": self.verdict.as_str(),
            "ended": ended_str(self.ended),
            "blocking": self.blocking_count(),
            "advisory": self.advisories.len(),
            "reasons": self.reasons.iter().map(|r| serde_json::json!({
                "code": r.code, "item_id": r.item_id, "message": r.message,
            })).collect::<Vec<_>>(),
        })
    }
}

fn ended_str(e: EndedKind) -> &'static str {
    match e {
        EndedKind::Report => "report",
        EndedKind::Partial => "partial",
        EndedKind::Budget => "budget",
        EndedKind::NoReport => "no_report",
    }
}

/// 宿主校验一份子交付。
///
/// 纯函数：给定同样的（契约、结果、账本、根）必得同一结论——校验**可重算**，
/// 恢复后重算 staleness 不需要也不允许发起任何工具。
#[allow(clippy::too_many_arguments)]
pub fn validate_delivery(
    contract: &TaskContract,
    result: &DeliveryResult,
    ledger: &Ledger,
    roots: &RootResolver,
    capture: &CaptureOptions,
) -> DeliveryValidation {
    let mut reasons: Vec<BlockingReason> = Vec::new();
    let mut advisories: Vec<BlockingReason> = Vec::new();

    // ---- 1. 身份 / 契约 revision ----
    // 顺序上最先判：身份不对时，后面的引用核对全是在替别人看契约。
    if result.task_id != contract.task_id {
        reasons.push(BlockingReason::blocking(
            "delivery_task_mismatch",
            None,
            format!(
                "交付结果属于任务 {}，当前契约是 {}",
                result.task_id, contract.task_id
            ),
        ));
    }
    if result.owner.session_id != contract.owner.session_id {
        reasons.push(BlockingReason::blocking(
            "delivery_session_mismatch",
            None,
            "交付结果与契约不属于同一会话",
        ));
    }
    if result.owner.root_run_id != contract.owner.root_run_id {
        reasons.push(BlockingReason::blocking(
            "delivery_run_mismatch",
            None,
            "交付结果来自其他 root run（迟到结果不参与本次验收）",
        ));
    }
    if result.contract_revision != contract.revision {
        reasons.push(BlockingReason::blocking(
            "delivery_revision_stale",
            None,
            format!(
                "交付基于契约修订 {}，当前修订 {}（修订升级后旧结果不再适用）",
                result.contract_revision, contract.revision
            ),
        ));
    }

    // ---- 2. 字段完整 ----
    let known = |id: &str| contract.requirement(id).is_some() || contract.artifact(id).is_some();
    for id in &result.completed {
        if !known(id) {
            reasons.push(BlockingReason::blocking(
                "delivery_unknown_requirement",
                Some(id),
                format!("完成项 `{id}` 不在契约内"),
            ));
        }
    }
    for item in &result.incomplete {
        if !known(&item.target_id) {
            reasons.push(BlockingReason::blocking(
                "delivery_unknown_requirement",
                Some(&item.target_id),
                format!("未完成项 `{}` 不在契约内", item.target_id),
            ));
        }
        if item.reason.trim().is_empty() {
            reasons.push(BlockingReason::blocking(
                "delivery_missing_reason",
                Some(&item.target_id),
                format!("未完成项 `{}` 缺少原因", item.target_id),
            ));
        }
    }
    // 必需需求既没声明完成也没声明未完成 = 静默遗漏：按未覆盖阻断，不当默认通过。
    for r in contract.requirements.iter().filter(|r| r.required) {
        let covered = result.completed.contains(&r.id)
            || result.incomplete.iter().any(|i| i.target_id == r.id);
        if !covered {
            reasons.push(BlockingReason::blocking(
                "delivery_required_uncovered",
                Some(&r.id),
                format!("必需需求 `{}` 既未声明完成也未声明未完成", r.id),
            ));
        }
    }

    // ---- 3. 完成与未完成互斥 ----
    let completed: BTreeSet<&String> = result.completed.iter().collect();
    for item in &result.incomplete {
        if completed.contains(&item.target_id) {
            reasons.push(BlockingReason::blocking(
                "delivery_contradictory_state",
                Some(&item.target_id),
                format!("`{}` 同时声明完成与未完成", item.target_id),
            ));
        }
    }

    // ---- 4. 引用及版本 ----
    // 4a) 自述证据必须是宿主已采纳的记录：虚构回执 id 一律拒。
    for eid in &result.evidence {
        let bound = ledger
            .receipts
            .get(eid)
            .is_some_and(|r| r.binding.as_ref().is_some_and(|b| b.matches(contract)))
            || ledger
                .validations
                .get(eid)
                .is_some_and(|r| r.binding.as_ref().is_some_and(|b| b.matches(contract)))
            || ledger
                .decisions
                .get(eid)
                .is_some_and(|r| r.binding.as_ref().is_some_and(|b| b.matches(contract)))
            || ledger.reviews.get(eid).is_some_and(|r| {
                r.task_id == contract.task_id && r.contract_revision == contract.revision
            })
            || ledger.deliveries.get(eid).is_some_and(|r| {
                r.task_id == contract.task_id && r.contract_revision == contract.revision
            });
        if !bound {
            reasons.push(BlockingReason::blocking(
                "delivery_unknown_evidence",
                Some(eid),
                format!("证据 `{eid}` 不是宿主已采纳的记录（模型自述的证据 id 无效）"),
            ));
        }
    }
    // 4b) 声明的改动拿回真实文件系统核对（[#12](../../../../docs/tools-and-safety.md)）。
    for ch in &result.changes {
        if let Some(r) = artifacts::validate_change(roots, ch).reason {
            reasons.push(BlockingReason::blocking(
                "delivery_change_unverified",
                Some(&ch.path.relative_path),
                r.message,
            ));
        }
    }
    // 4c) stale 回执：证据绑定的是旧版本，不能支撑当前产物。
    let stale = stale_receipts(ledger, contract, roots, capture);
    for (check_id, paths) in stale {
        reasons.push(BlockingReason::blocking(
            "delivery_stale_evidence",
            Some(&check_id),
            format!(
                "检查 `{check_id}` 的证据在验证后已失效（{}）",
                paths.join("、")
            ),
        ));
    }

    // ---- 5. 必需检查 ----
    // 复用 #54 的逐项求值作为**唯一**判定来源，不另写第二套"什么算通过"。
    let ctx = EvalContext {
        contract,
        ledger,
        roots,
        capture: capture.clone(),
    };
    let eval = acceptance::evaluate(&ctx);
    match eval {
        Ok(out) => {
            for item in &out.items {
                // 非必需需求不参与阻断，但仍如实展示。
                let required = contract
                    .requirement(&item.requirement_id)
                    .is_some_and(|r| r.required);
                if !required {
                    if item.status != AcceptanceStatus::Passed
                        && item.status != AcceptanceStatus::Pending
                    {
                        advisories.push(BlockingReason::advisory(
                            "delivery_optional_item_unsatisfied",
                            Some(&item.requirement_id),
                            format!(
                                "非必需需求 `{}` 状态 {:?}",
                                item.requirement_id, item.status
                            ),
                        ));
                    }
                    continue;
                }
                match item.status {
                    AcceptanceStatus::Passed => {}
                    AcceptanceStatus::Stale => reasons.push(BlockingReason::blocking(
                        "delivery_check_stale",
                        Some(&item.requirement_id),
                        format!("必需需求 `{}` 的证据已失效", item.requirement_id),
                    )),
                    AcceptanceStatus::Pending => reasons.push(BlockingReason::blocking(
                        "delivery_check_missing",
                        Some(&item.requirement_id),
                        format!(
                            "必需需求 `{}` 尚无已采纳证据（报告正文不算证据）",
                            item.requirement_id
                        ),
                    )),
                    other => {
                        let detail = item
                            .reasons
                            .iter()
                            .map(|r| r.message.clone())
                            .collect::<Vec<_>>()
                            .join("；");
                        reasons.push(BlockingReason::blocking(
                            "delivery_check_failed",
                            Some(&item.requirement_id),
                            format!("必需需求 `{}` 未通过（{detail}）", item.requirement_id),
                        ));
                        let _ = other;
                    }
                }
            }
            // 整体未通过也是阻断：逐项看着都好但整体 blocked（例如缺人工确认）。
            if out.overall != AcceptanceStatusKind::Passed
                && out.overall != AcceptanceStatusKind::Cancelled
            {
                reasons.push(BlockingReason::blocking(
                    "delivery_acceptance_not_passed",
                    None,
                    format!("契约整体验收状态为 {:?}", out.overall),
                ));
            }
        }
        Err(shape) => {
            // 契约本身非法（悬空引用等）：交付无法基于它成立。
            for r in shape {
                reasons.push(BlockingReason::blocking(
                    "delivery_contract_invalid",
                    r.target_id.as_deref(),
                    r.message,
                ));
            }
        }
    }

    // ---- 6. 审查 findings ----
    // 严重级别按 ReviewPolicy 声明；**建议级不自动失败**，阻断级才拒。
    let mut latest_reviews = std::collections::BTreeMap::new();
    for (rid, review) in &ledger.reviews {
        if review.task_id != contract.task_id
            || review.contract_revision != contract.revision
            || review.check_id.is_none()
            || review.owner.session_id != contract.owner.session_id
            || review.owner.root_run_id != contract.owner.root_run_id
        {
            continue;
        }
        let review_checks: Vec<_> = contract
            .checks
            .iter()
            .filter(|check| {
                check.kind == CheckKind::Review
                    && review.check_id.as_deref() == Some(check.id.as_str())
            })
            .collect();
        if let Some(scope_version) = &review.scope_version
            && !review_checks.is_empty()
            && !review_checks.iter().any(|check| {
                super::version::capture_scope(roots, &check.scope, capture)
                    .ok()
                    .is_some_and(|current| current.is_valid() && &current.digest == scope_version)
            })
        {
            continue;
        }
        let entry = latest_reviews
            .entry((review.scope_version.clone(), review.check_id.clone()))
            .or_insert((rid, review));
        if (&review.reviewed_at, rid) > (&entry.1.reviewed_at, entry.0) {
            *entry = (rid, review);
        }
    }
    for (rid, review) in latest_reviews.into_values() {
        for d in &review.dimensions {
            let findings = d
                .findings
                .iter()
                .map(|f| (f.severity, f.message.as_str()))
                .chain(
                    d.review_findings
                        .iter()
                        .map(|f| (f.severity, f.message.as_str())),
                );
            for (severity, message) in findings {
                let reason = BlockingReason {
                    code: format!("review_{severity:?}").to_lowercase(),
                    item_id: Some(rid.clone()),
                    message: format!("[{}] {message}", d.dimension),
                    blocking: contract.review_policy.is_blocking(severity),
                };
                if reason.blocking {
                    reasons.push(reason);
                } else {
                    advisories.push(reason);
                }
            }
        }
        // 独立审查不成立时，审查结论整体不可用。
        if contract.review_policy.independent && review.reviewer.same_actor(&contract.owner) {
            reasons.push(BlockingReason::blocking(
                "review_not_independent",
                Some(rid),
                "审查者与执行者同一身份，独立审查结论不成立",
            ));
        }
    }

    // ---- 7. 终态一致性 ----
    // 最后判：前面任何一层没过就已经拒了，这里负责"全部合法但形态不是完整收尾"的情况。
    match result.ended {
        EndedKind::Report => {}
        EndedKind::Partial => reasons.push(BlockingReason::blocking(
            "delivery_partial_not_complete",
            None,
            "收尾形态为 partial（撞步数上限才收尾）：成果保留，但不算完整交付",
        )),
        EndedKind::Budget => reasons.push(BlockingReason::blocking(
            "delivery_budget_exhausted",
            None,
            "收尾形态为 budget（撞上限且未交报告）：不计完整交付",
        )),
        EndedKind::NoReport => reasons.push(BlockingReason::blocking(
            "delivery_no_report",
            None,
            "收尾形态为 no_report（提前退出未交报告）：不计完整交付",
        )),
    }

    DeliveryValidation {
        verdict: if reasons.is_empty() {
            DeliveryVerdict::Accepted
        } else {
            DeliveryVerdict::Rejected
        },
        reasons,
        advisories,
        delivery_id: result.id.clone(),
        ended: result.ended,
    }
}

/// 找出绑定范围已变化的检查（stale 回执），返回 `(check_id, 变化路径)`。
fn stale_receipts(
    ledger: &Ledger,
    contract: &TaskContract,
    roots: &RootResolver,
    capture: &CaptureOptions,
) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for spec in &contract.checks {
        let Some(rec) = ledger.latest_receipt_for_check(&spec.id, contract) else {
            continue;
        };
        let Some(after) = rec.after.as_ref() else {
            continue;
        };
        let Ok(current) = super::version::capture_scope(roots, &spec.scope, capture) else {
            continue;
        };
        match super::version::compare_scope(after, &current) {
            super::version::Freshness::Stale { changed } => {
                let paths: Vec<String> = changed
                    .iter()
                    .take(3)
                    .map(|c| format!("{}/{}", c.root_id, c.relative_path))
                    .collect();
                out.push((spec.id.clone(), paths));
            }
            // 采集不可信不是"没变"：按不可采信报阻断，避免把它静默当新鲜。
            super::version::Freshness::Unknown { reason } => {
                out.push((spec.id.clone(), vec![reason]));
            }
            super::version::Freshness::Fresh => {}
        }
    }
    out
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
