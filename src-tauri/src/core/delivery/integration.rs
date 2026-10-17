//! 集成后验收（任务 #22，[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）。
//!
//! 多个子任务各自通过，**不等于**它们合在一起也通过。首版模型是 [`IntegrationTarget`]：
//! 合体后的真实内容版本 + 一组必须重跑的整体检查。
//!
//! 四条不可让步的判定纪律：
//!
//! 1. **目标 = 所有子写入结束后的当前内容**。不是 A 的测试结果加 B 的审查结论拼起来——
//!    那是把两份互不相干的证明凑成一份。
//! 2. **只跑契约里的 integration checks**，仍走真实 `command` / fence / 回执；
//!    子级检查可以保留作说明，但**替代不了**必需的整体验证。
//! 3. **父在检查前后写入 → `drifted` / `stale`，停止完成**。否则"整体通过"证明的是一个
//!    已经被改掉的内容。
//! 4. **`not_required` 必须事先决定或由用户明确豁免**，不能在失败之后自动降级成"不需要"。

use super::types::{ExecutionReceipt, TaskContract, VerificationScope};
use super::version::{self, CaptureOptions, RootResolver};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 集成状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    /// 尚待验证。
    Pending,
    /// 整体验证通过。
    Verified,
    /// 整体验证失败。
    Failed,
    /// 检查期间目标漂移（父并发写入）——停止完成。
    Drifted,
    /// 相关文件在验证后变化——证据失效。
    Stale,
    /// 无需整体验证（纯问答汇总）。**必须契约事先决定或用户豁免**。
    NotRequired,
}

impl IntegrationStatus {
    /// 是否可让任务整体完成。
    ///
    /// 只有 [`Verified`](Self::Verified) 和 [`NotRequired`](Self::NotRequired) 可放行；
    /// 其余一律阻断完成。
    pub fn allows_completion(self) -> bool {
        matches!(
            self,
            IntegrationStatus::Verified | IntegrationStatus::NotRequired
        )
    }
    /// wire 串。
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrationStatus::Pending => "pending",
            IntegrationStatus::Verified => "verified",
            IntegrationStatus::Failed => "failed",
            IntegrationStatus::Drifted => "drifted",
            IntegrationStatus::Stale => "stale",
            IntegrationStatus::NotRequired => "not_required",
        }
    }
}

/// 集成目标（[#22](../../../../docs/plan-and-approval.md)）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationTarget {
    /// 契约修订。
    pub contract_revision: u64,
    /// 已接受的子交付记录 id。
    pub accepted_delivery_ids: Vec<String>,
    /// 冻结时父目录内容版本（合体**前**，用于检测检查期间的写入）。
    pub expected_parent_version: String,
    /// 实际整体结果版本（验证时重新采集）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_version: Option<String>,
    /// 必须重跑的整体检查 id（契约里的 integration checks）。
    pub required_check_ids: Vec<String>,
    /// 整体状态。
    pub status: IntegrationStatus,
    /// 共享外部资源（数据库 / 服务）标识；未知时不得推断可并行。
    #[serde(default)]
    pub shared_resources: Vec<String>,
    /// 状态理由。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `not_required` 的依据：契约事先决定或用户豁免的记录 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_required_basis: Option<String>,
}

/// 集成判定结果。
#[derive(Debug, Clone, PartialEq)]
pub struct IntegrationOutcome {
    /// 状态。
    pub status: IntegrationStatus,
    /// 理由（可定位）。
    pub reason: Option<String>,
    /// 实际结果版本。
    pub result_version: Option<String>,
}

impl IntegrationOutcome {
    fn new(status: IntegrationStatus, reason: impl Into<String>, version: Option<String>) -> Self {
        IntegrationOutcome {
            status,
            reason: Some(reason.into()),
            result_version: version,
        }
    }
}

/// 校验集成结果并给出状态。
///
/// `parent_version_before` / `parent_version_after` 是检查前后各采集一次的父目录范围版本：
/// 两者不等即**检查期间发生写入**（[#22](../../../../docs/plan-and-approval.md) 明确要求停止完成）。
#[allow(clippy::too_many_arguments)]
pub fn evaluate_integration(
    target: &IntegrationTarget,
    contract: &TaskContract,
    ledger: &super::acceptance::Ledger,
    roots: &RootResolver,
    capture: &CaptureOptions,
    parent_scope: &VerificationScope,
    parent_version_before: Option<&version::ScopeVersion>,
    parent_version_after: Option<&version::ScopeVersion>,
) -> IntegrationOutcome {
    // 契约修订漂移：按旧修订的集成结论不适用当前契约。
    if target.contract_revision != contract.revision {
        return IntegrationOutcome::new(
            IntegrationStatus::Stale,
            format!(
                "集成目标基于契约修订 {}，当前修订 {}",
                target.contract_revision, contract.revision
            ),
            target.result_version.clone(),
        );
    }
    // 纯问答汇总可不验整体，但**必须事先有依据**：事后自动降级等于把失败变成"不需要"。
    if target.status == IntegrationStatus::NotRequired {
        let Some(basis) = target
            .not_required_basis
            .as_deref()
            .filter(|b| !b.is_empty())
        else {
            return IntegrationOutcome::new(
                IntegrationStatus::Pending,
                "声明 not_required 但没有契约事先决定或用户豁免的依据：不得事后自动降级",
                target.result_version.clone(),
            );
        };
        let authorized = contract
            .scope_changes
            .iter()
            .any(|change| change.id == basis && !change.reason.trim().is_empty())
            || ledger.decisions.get(basis).is_some_and(|d| {
                d.binding
                    .as_ref()
                    .is_some_and(|binding| binding.matches(contract))
                    && d.owner.session_id == contract.owner.session_id
                    && d.owner.root_run_id == contract.owner.root_run_id
                    && d.decision == super::types::UserDecisionKind::Waived
            });
        if !authorized {
            return IntegrationOutcome::new(
                IntegrationStatus::Pending,
                "not_required依据不是已采纳的契约决定或真实用户豁免",
                None,
            );
        }
        return IntegrationOutcome::new(
            IntegrationStatus::NotRequired,
            format!("无需整体验证（依据 `{basis}`）"),
            target.result_version.clone(),
        );
    }
    // 共享外部资源未登记版本时，不得推断"可以并行/可以合并"。
    if !target.shared_resources.is_empty() && !has_resource_versions(target, ledger, contract) {
        return IntegrationOutcome::new(
            IntegrationStatus::Pending,
            format!(
                "涉及共享外部资源 {:?} 但缺少验证环境版本：未知状态不得推断可合并",
                target.shared_resources
            ),
            target.result_version.clone(),
        );
    }
    // 子交付未齐 → 整体验证还谈不上。
    if target.accepted_delivery_ids.is_empty() && !target.required_check_ids.is_empty() {
        return IntegrationOutcome::new(
            IntegrationStatus::Pending,
            "尚无已接受的子交付结果，整体验证不能开始",
            None,
        );
    }
    // 必须在检查前后各采一次父目录范围；缺任何一次都无法证明"期间没变"。
    let (Some(before), Some(after)) = (parent_version_before, parent_version_after) else {
        return IntegrationOutcome::new(
            IntegrationStatus::Pending,
            "缺少检查前后的父目录范围版本：无法证明检查期间没有写入",
            None,
        );
    };
    match version::compare_scope(before, after) {
        version::Freshness::Stale { changed } => {
            let paths: Vec<String> = changed
                .iter()
                .take(5)
                .map(|c| format!("{}/{}", c.root_id, c.relative_path))
                .collect();
            return IntegrationOutcome::new(
                IntegrationStatus::Drifted,
                format!(
                    "整体验证期间父目录被写入（{}）：停止完成，先合流再验",
                    paths.join("、")
                ),
                None,
            );
        }
        version::Freshness::Unknown { reason } => {
            return IntegrationOutcome::new(
                IntegrationStatus::Pending,
                format!("父目录范围采集不可信（{reason}）：不推断为通过"),
                None,
            );
        }
        version::Freshness::Fresh => {}
    }
    // 重跑契约声明的 integration checks；**子级回执不能顶替**。
    let required: Vec<String> = target.required_check_ids.clone();
    let missing: Vec<String> = required
        .iter()
        .filter(|id| contract.check(id).is_none())
        .cloned()
        .collect();
    if !missing.is_empty() {
        return IntegrationOutcome::new(
            IntegrationStatus::Failed,
            format!("整体验证引用了契约里不存在的检查：{missing:?}"),
            None,
        );
    }
    // 必跑检查读到的每一处，都必须落在漂移检测窗口内，否则"检查期间没被写入"无从证明。
    let uncovered: Vec<String> = required
        .iter()
        .filter_map(|id| contract.check(id))
        .flat_map(|spec| uncovered_inputs(parent_scope, &spec.scope))
        .take(5)
        .collect();
    if !uncovered.is_empty() {
        return IntegrationOutcome::new(
            IntegrationStatus::Pending,
            format!(
                "必跑检查的输入落在父范围之外（{uncovered:?}）：前后对比覆盖不到被检查区域，无从证明检查期间未被写入"
            ),
            None,
        );
    }
    let mut failed: Vec<String> = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for id in &required {
        // 只认**绑定该整体检查**且属主的回执；子级检查（绑定别的 check_id）在此不生效。
        let Some(receipt) = ledger
            .receipts
            .values()
            .filter(|r| {
                r.binding
                    .as_ref()
                    .is_some_and(|binding| binding.matches(contract))
                    && r.check_id.as_deref() == Some(id.as_str())
                    && r.owner.root_run_id == contract.owner.root_run_id
                    && r.owner.session_id == contract.owner.session_id
                    && r.owner.actor_id == super::types::ROOT_ACTOR
            })
            .max_by(|a, b| a.finished_at.cmp(&b.finished_at))
        else {
            failed.push(format!("整体检查 `{id}` 尚无真实回执"));
            continue;
        };
        used.insert(receipt.id.clone());
        if let Some(why) = verify_receipt(receipt, contract, roots, capture) {
            failed.push(format!("整体检查 `{id}` 未通过：{why}"));
        }
    }
    let result_version = parent_version_after.map(|v| v.digest.clone());
    let result_version = result_version.unwrap_or_else(|| "unknown".to_string());
    if failed.is_empty() {
        IntegrationOutcome::new(
            IntegrationStatus::Verified,
            format!(
                "整体验证通过（{} 项检查，目标版本 {result_version}）",
                used.len()
            ),
            Some(result_version),
        )
    } else {
        IntegrationOutcome::new(
            IntegrationStatus::Failed,
            format!("整体验证失败：{}", failed.join("；")),
            Some(result_version),
        )
    }
}

/// 单张回执是否满足其 CheckSpec（退出码 + 版本新鲜度）。
fn verify_receipt(
    receipt: &ExecutionReceipt,
    contract: &TaskContract,
    roots: &RootResolver,
    capture: &CaptureOptions,
) -> Option<String> {
    let check_id = receipt.check_id.as_deref()?;
    let spec = contract.check(check_id)?;
    let allowed: Vec<i32> = match &spec.expectation {
        super::types::CheckExpectation::Command { allowed_exit_codes } => {
            allowed_exit_codes.clone()
        }
        _ => vec![0],
    };
    if !receipt.outcome.has_exit_code() {
        return Some(format!("未真实退出（{}）", receipt.outcome.as_str()));
    }
    match receipt.exit_code {
        Some(code) if allowed.contains(&code) => {}
        Some(code) => return Some(format!("退出码 {code} 不在允许集合 {allowed:?}")),
        None => return Some("终态与退出码不自洽".into()),
    }
    // 版本绑定：相关文件变了则证据失效（锁文件变化同样使旧检查 stale）。
    let Some(after) = receipt.after.as_ref() else {
        return Some("未采集范围版本，无法证明内容未变".into());
    };
    if receipt.before.as_ref().is_none_or(|before| {
        !version::compare_execution_inputs(before, after, &spec.scope).is_fresh()
    }) {
        return Some("整体检查前后范围漂移或缺少前版本".into());
    }
    let Ok(current) = version::capture_scope(roots, &spec.scope, capture) else {
        return Some("当前范围采集失败".into());
    };
    match version::compare_scope(after, &current) {
        version::Freshness::Fresh => None,
        version::Freshness::Stale { changed } => Some(format!(
            "证据已失效（{} 变化）",
            changed
                .iter()
                .take(3)
                .map(|c| c.relative_path.clone())
                .collect::<Vec<_>>()
                .join("、")
        )),
        version::Freshness::Unknown { reason } => Some(format!("采集不可信：{reason}")),
    }
}

/// 必跑检查的只读输入中，落在父范围之外的路径描述。
///
/// 只看**输入与依赖文件**：检查自己的产物由检查自己写出，不该因为落在窗口内就算漂移；
/// 而检查读到的输入如果在窗口外，期间被改了就完全无法察觉。
fn uncovered_inputs(
    parent: &super::types::VerificationScope,
    check: &super::types::VerificationScope,
) -> Vec<String> {
    let covered: Vec<&super::types::PathRef> = parent
        .inputs
        .iter()
        .chain(parent.outputs.iter())
        .chain(parent.dependency_files.iter())
        .collect();
    check
        .inputs
        .iter()
        .chain(check.dependency_files.iter())
        .filter(|c| !covered.iter().any(|p| covers(p, c)))
        .map(|c| format!("{}:{}", c.root_id, c.relative_path))
        .collect()
}

/// `parent` 是否覆盖 `child` 指向的路径：同根，且父为空（整根）、完全相等，或为前缀目录。
fn covers(parent: &super::types::PathRef, child: &super::types::PathRef) -> bool {
    if parent.root_id != child.root_id {
        return false;
    }
    let p = parent.relative_path.trim_matches('/');
    let c = child.relative_path.trim_matches('/');
    p.is_empty() || p == c || c.strip_prefix(p).is_some_and(|rest| rest.starts_with('/'))
}

/// 共享资源是否登记了验证环境版本。
///
/// 版本事实的写法是 `resource:<资源名>@<版本>`（如 `resource:postgres-main@16`），
/// 因为"知道这个资源存在"和"知道它当前是哪个版本"是两回事：只有带版本的事实才
/// 能支撑"可以合并"，裸 `resource:postgres-main` 等于没登记。
fn has_resource_versions(
    target: &IntegrationTarget,
    ledger: &super::acceptance::Ledger,
    contract: &TaskContract,
) -> bool {
    // 只收集**带非空版本**的资源名；版本段缺失的事实不构成登记。
    let registered: BTreeSet<&str> = ledger
        .validations
        .values()
        .filter(|v| {
            v.binding
                .as_ref()
                .is_some_and(|binding| binding.matches(contract))
        })
        .flat_map(|v| v.input_refs.iter().map(String::as_str))
        .filter_map(|r| r.strip_prefix("resource:"))
        .filter_map(|r| {
            let (name, version) = r.rsplit_once('@')?;
            (!name.is_empty() && !version.is_empty()).then_some(name)
        })
        .collect();
    target
        .shared_resources
        .iter()
        .all(|r| registered.contains(r.as_str()))
}

/// 供 UI / 事件使用的紧凑摘要。
pub fn summary(outcome: &IntegrationOutcome) -> serde_json::Value {
    serde_json::json!({
        "status": outcome.status.as_str(),
        "reason": outcome.reason,
        "result_version": outcome.result_version,
    })
}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;
