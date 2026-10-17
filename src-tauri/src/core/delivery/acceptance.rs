//! 逐项验收求值（任务 #54 拥有）。
//!
//! 验收**不是**"看模型说了什么"，而是把契约里的每条必需检查 / 产物，拿回宿主已采纳的
//! 不可变记录与真实文件系统去核对（[#54](../../../../docs/tools-and-safety.md)）。
//!
//! 归约规则（优先级由 [`AcceptanceSnapshot::reduce_overall`] 固定）：
//! `cancelled → blocked → failed → stale → pending → passed`。
//!
//! 四条不可让步的判定纪律：
//!
//! 1. **未绑定检查的需求保持 `pending`**：不做自然语言自动证明。模型在交付结果里写
//!    `passed = true` 不产生任何证据，[`Ledger`] 里没有对应记录就是没有。
//! 2. **证据要新鲜**：相关文件在验证后变化 → `stale`（派生状态，**不改写原始执行记录**）。
//!    采集不完整 → `blocked`（不是 stale，也不是 passed）。
//! 3. **任意退出 0 只满足它自己那一条 `CheckSpec`**，不能顶替别的检查。
//! 4. **豁免只能引用真实用户决定**：`waived` 需要一条 `UserDecision{kind: Waived}`，
//!    预算耗尽 / 平台缺失 / 模型声明**不构成**豁免。

use super::artifacts;
use super::types::*;
use super::version::{self, Freshness, RootResolver};
use std::collections::BTreeMap;

/// 宿主已采纳的证据账本。由 [`crate::core::delivery::store::DeliveryStore`] 从**索引内**记录装配——
/// 未入索引的孤儿一律不进来（它们只供诊断）。
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    /// 执行回执：record_id → 回执。
    pub receipts: BTreeMap<String, ExecutionReceipt>,
    /// 内置校验记录。
    pub validations: BTreeMap<String, BuiltinValidationRecord>,
    /// 审查结论。
    pub reviews: BTreeMap<String, ReviewResult>,
    /// 用户决定。
    pub decisions: BTreeMap<String, UserDecision>,
    /// 交付结果。
    pub deliveries: BTreeMap<String, DeliveryResult>,
    /// 宿主签发的审查分派。
    pub review_assignments: BTreeMap<String, super::responsibility::ReviewAssignment>,
}

impl Ledger {
    /// 空账本。
    pub fn new() -> Self {
        Self::default()
    }
    /// 是否为空（没有任何已采纳证据）。
    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty()
            && self.validations.is_empty()
            && self.reviews.is_empty()
            && self.decisions.is_empty()
            && self.deliveries.is_empty()
            && self.review_assignments.is_empty()
    }
    /// 某 id 是否是宿主已采纳的记录（回执 / 校验 / 审查 / 决定 / 交付任一）。
    ///
    /// 用于拒绝模型自述的证据 id：模型能写出任意字符串，只有索引里有的才算证据。
    pub fn has_record(&self, id: &str) -> bool {
        self.receipts.contains_key(id)
            || self.validations.contains_key(id)
            || self.reviews.contains_key(id)
            || self.decisions.contains_key(id)
            || self.deliveries.contains_key(id)
            || self.review_assignments.contains_key(id)
    }
    /// 某检查最近一次**属于该属主**的执行回执。
    ///
    /// 跨会话 / 跨 run 的回执不返回——迟到结果不得给当前契约作证（[#18](../../../../docs/tools-and-safety.md)）。
    pub fn latest_receipt_for_check(
        &self,
        check_id: &str,
        contract: &TaskContract,
    ) -> Option<&ExecutionReceipt> {
        self.receipts
            .values()
            .filter(|r| {
                r.check_id.as_deref() == Some(check_id)
                    && r.owner.session_id == contract.owner.session_id
                    && r.owner.root_run_id == contract.owner.root_run_id
                    && r.binding
                        .as_ref()
                        .is_some_and(|binding| binding.matches(contract))
            })
            .max_by(|a, b| a.finished_at.cmp(&b.finished_at))
    }
    /// 引用某任务所有交付结果（按提交时刻排序）。
    pub fn deliveries_for(&self, task_id: &str) -> Vec<&DeliveryResult> {
        let mut v: Vec<&DeliveryResult> = self
            .deliveries
            .values()
            .filter(|d| d.task_id == task_id)
            .collect();
        v.sort_by(|a, b| a.submitted_at.cmp(&b.submitted_at).then(a.id.cmp(&b.id)));
        v
    }
}

/// 求值上下文。
pub struct EvalContext<'a> {
    /// 契约（修订已固定）。
    pub contract: &'a TaskContract,
    /// 宿主已采纳证据。
    pub ledger: &'a Ledger,
    /// 根解析器（受控解析，绝不信入参绝对路径）。
    pub roots: &'a RootResolver,
    /// 采集参数（有界 + 可取消）。
    pub capture: version::CaptureOptions,
}

/// 求值结果。
pub struct EvalOutcome {
    /// 逐项结果。
    pub items: Vec<AcceptanceItem>,
    /// 整体状态。
    pub overall: AcceptanceStatusKind,
    /// 合法豁免数量。
    pub waived_count: usize,
    /// 求值时的当前范围版本摘要。
    pub evaluated_version: Option<String>,
    /// 整体级原因。
    pub reasons: Vec<Reason>,
}

fn reason(code: &str, message: impl Into<String>, target: Option<&str>) -> Reason {
    Reason {
        code: code.to_string(),
        message: message.into(),
        target_id: target.map(str::to_string),
    }
}

/// 引用闭包校验：同名 id、悬空 check/artifact 引用、类别与期望错配、规模超限。
///
/// 这些都是**创建契约时**就该拒的；求值前再校验一次，保证手改 / 旧版本契约不会被静默采信。
pub fn validate_contract_shape(contract: &TaskContract) -> Result<(), Vec<Reason>> {
    let mut reasons = Vec::new();
    if contract.requirements.len() > MAX_REQUIREMENTS {
        reasons.push(reason(
            "contract_too_large",
            format!(
                "需求数 {} 超过上限 {MAX_REQUIREMENTS}",
                contract.requirements.len()
            ),
            None,
        ));
    }
    if contract.checks.len() > MAX_CHECKS {
        reasons.push(reason(
            "contract_too_large",
            format!("检查数 {} 超过上限 {MAX_CHECKS}", contract.checks.len()),
            None,
        ));
    }
    if contract.artifacts.len() > MAX_ARTIFACTS {
        reasons.push(reason(
            "contract_too_large",
            format!(
                "产物数 {} 超过上限 {MAX_ARTIFACTS}",
                contract.artifacts.len()
            ),
            None,
        ));
    }
    let mut req_ids: Vec<&str> = Vec::new();
    for r in &contract.requirements {
        if req_ids.contains(&r.id.as_str()) {
            reasons.push(reason(
                "duplicate_id",
                format!("需求 id `{}` 重复", r.id),
                Some(&r.id),
            ));
        }
        req_ids.push(&r.id);
    }
    let mut check_ids: Vec<&str> = Vec::new();
    for c in &contract.checks {
        if check_ids.contains(&c.id.as_str()) {
            reasons.push(reason(
                "duplicate_id",
                format!("检查 id `{}` 重复", c.id),
                Some(&c.id),
            ));
        }
        check_ids.push(&c.id);
        if !c.expectation.matches_kind(c.kind) {
            reasons.push(reason(
                "expectation_kind_mismatch",
                format!(
                    "检查 `{}` 声明种类 {:?}，但期望为 {}",
                    c.id,
                    c.kind,
                    c.expectation.kind_str()
                ),
                Some(&c.id),
            ));
        }
        if c.kind == CheckKind::Command && c.command.is_none() {
            reasons.push(reason(
                "command_missing",
                format!("命令检查 `{}` 缺少 command", c.id),
                Some(&c.id),
            ));
        }
    }
    let mut art_ids: Vec<&str> = Vec::new();
    for a in &contract.artifacts {
        if art_ids.contains(&a.id.as_str()) {
            reasons.push(reason(
                "duplicate_id",
                format!("产物 id `{}` 重复", a.id),
                Some(&a.id),
            ));
        }
        art_ids.push(&a.id);
    }
    for check in &contract.checks {
        if let CheckExpectation::Artifact { artifact_ids } = &check.expectation {
            if artifact_ids.is_empty() {
                reasons.push(reason(
                    "artifact_check_empty",
                    "产物检查必须引用至少一个真实产物",
                    Some(&check.id),
                ));
            }
            for artifact_id in artifact_ids {
                if !art_ids.contains(&artifact_id.as_str()) {
                    reasons.push(reason(
                        "dangling_artifact_ref",
                        format!("产物检查 `{}` 引用不存在的产物 `{artifact_id}`", check.id),
                        Some(&check.id),
                    ));
                }
            }
        }
    }
    for r in &contract.requirements {
        for cid in &r.check_ids {
            if !check_ids.contains(&cid.as_str()) {
                reasons.push(reason(
                    "dangling_check_ref",
                    format!("需求 `{}` 引用不存在的检查 `{cid}`", r.id),
                    Some(&r.id),
                ));
            }
        }
        for aid in &r.artifact_ids {
            if !art_ids.contains(&aid.as_str()) {
                reasons.push(reason(
                    "dangling_artifact_ref",
                    format!("需求 `{}` 引用不存在的产物 `{aid}`", r.id),
                    Some(&r.id),
                ));
            }
        }
    }
    for sc in &contract.scope_changes {
        if matches!(
            sc.kind,
            ScopeChangeKind::Cancelled | ScopeChangeKind::Waived
        ) && sc.user_decision_id.is_none()
        {
            reasons.push(reason(
                "scope_change_without_user_decision",
                format!("范围变更 `{}` 缺少真实用户决定依据", sc.id),
                Some(&sc.target_id),
            ));
        }
        let known = req_ids.contains(&sc.target_id.as_str())
            || check_ids.contains(&sc.target_id.as_str())
            || art_ids.contains(&sc.target_id.as_str());
        if !known {
            reasons.push(reason(
                "scope_change_unknown_target",
                format!("范围变更 `{}` 指向未知目标 `{}`", sc.id, sc.target_id),
                Some(&sc.target_id),
            ));
        }
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons)
    }
}

/// 执行一次完整验收。
///
/// **每次求值都重新采集当前范围版本**，不缓存哈希跨调用（先保证正确，性能另测）；
/// watcher 只作刷新提示，不参与判定——它丢事件不该让 stale 证据复活。
pub fn evaluate(ctx: &EvalContext<'_>) -> Result<EvalOutcome, Vec<Reason>> {
    validate_contract_shape(ctx.contract)?;
    let contract = ctx.contract;
    let mut items = Vec::with_capacity(contract.requirements.len());
    let mut waived_count = 0usize;
    let mut overall_reasons: Vec<Reason> = Vec::new();
    let mut version_digests: Vec<String> = Vec::new();

    for req in &contract.requirements {
        if contract.is_cancelled(&req.id) || contract.is_waived(&req.id) {
            let kind = if contract.is_cancelled(&req.id) {
                UserDecisionKind::Cancelled
            } else {
                UserDecisionKind::Waived
            };
            if let Some((id, decision)) = authorized_scope_decision(ctx, &req.id, kind) {
                let status = if kind == UserDecisionKind::Cancelled {
                    AcceptanceStatus::Cancelled
                } else {
                    waived_count += 1;
                    AcceptanceStatus::Waived
                };
                items.push(AcceptanceItem {
                    requirement_id: req.id.clone(),
                    status,
                    evidence_refs: vec![EvidenceRef {
                        kind: EvidenceKind::UserDecision,
                        record_id: id,
                        owner: decision.owner.clone(),
                        scope_version: decision.scope_version.clone(),
                    }],
                    reasons: vec![reason(
                        "user_scope_decision",
                        "用户明确范围决定",
                        Some(&req.id),
                    )],
                    evaluated_version: decision.scope_version.clone(),
                });
            } else {
                let why = reason(
                    "waiver_without_user_decision",
                    "取消/豁免缺少当前版本、同 run 的明确用户决定",
                    Some(&req.id),
                );
                overall_reasons.push(why.clone());
                items.push(AcceptanceItem {
                    requirement_id: req.id.clone(),
                    status: AcceptanceStatus::Blocked,
                    evidence_refs: Vec::new(),
                    reasons: vec![why],
                    evaluated_version: None,
                });
            }
            continue;
        }

        let mut item = evaluate_requirement(req, ctx, &mut version_digests);
        // 非必需需求：不阻断整体，但状态照样如实展示。
        if !req.required && item.status == AcceptanceStatus::Failed {
            item.reasons.push(reason(
                "non_requirement",
                "非必需需求的失败不阻断整体验收",
                Some(&req.id),
            ));
        }
        items.push(item);
    }

    let participating: Vec<_> = items
        .iter()
        .filter(|item| {
            contract
                .requirement(&item.requirement_id)
                .is_some_and(|requirement| requirement.required)
        })
        .cloned()
        .collect();
    let overall = if participating.is_empty() && !items.is_empty() {
        AcceptanceStatusKind::Passed
    } else {
        AcceptanceSnapshot::reduce_overall(&participating, false)
    };
    let evaluated_version = if version_digests.is_empty() {
        None
    } else {
        version_digests.sort();
        version_digests.dedup();
        Some(version_digests.join("+"))
    };
    Ok(EvalOutcome {
        items,
        overall,
        waived_count,
        evaluated_version,
        reasons: overall_reasons,
    })
}

/// 单条需求的求值：全部必需检查与产物成立才 `passed`。
fn evaluate_requirement(
    req: &Requirement,
    ctx: &EvalContext<'_>,
    version_digests: &mut Vec<String>,
) -> AcceptanceItem {
    let contract = ctx.contract;
    let mut statuses: Vec<(AcceptanceStatus, Reason)> = Vec::new();
    let mut evidence: Vec<EvidenceRef> = Vec::new();
    let mut evaluated_version: Option<String> = None;

    for cid in &req.check_ids {
        let Some(spec) = contract.check(cid) else {
            // 悬空引用已在 validate_contract_shape 拒绝；这里按失败兜底不静默通过。
            statuses.push((
                AcceptanceStatus::Failed,
                reason(
                    "dangling_check_ref",
                    format!("检查 `{cid}` 不存在"),
                    Some(&req.id),
                ),
            ));
            continue;
        };
        let (status, refs, ver, detail) = evaluate_check(spec, ctx, version_digests);
        evidence.extend(refs);
        if let Some(v) = ver {
            evaluated_version = Some(v);
        }
        // stale / blocked 的具体位置（变了哪些路径、为什么不能作结论）必须进 reasons——
        // 只留一个状态码等于让 UI 和模型都猜不出该重验什么。
        match detail {
            Some(d) => statuses.push((status, d)),
            None => statuses.push((status, reason_for(status, cid, spec))),
        }
    }

    for aid in &req.artifact_ids {
        let Some(spec) = contract.artifact(aid) else {
            statuses.push((
                AcceptanceStatus::Failed,
                reason(
                    "dangling_artifact_ref",
                    format!("产物 `{aid}` 不存在"),
                    Some(&req.id),
                ),
            ));
            continue;
        };
        let (status, refs, detail) = evaluate_artifact(spec, ctx, version_digests);
        evidence.extend(refs);
        if status != AcceptanceStatus::Passed {
            statuses.push((
                status,
                detail.unwrap_or_else(|| {
                    reason("artifact_not_satisfied", "产物未满足约束", Some(aid))
                }),
            ));
        }
    }

    // A physical artifact alone cannot prove the requirement: an explicit check remains mandatory.
    if statuses.is_empty() {
        return AcceptanceItem {
            requirement_id: req.id.clone(),
            status: AcceptanceStatus::Pending,
            evidence_refs: Vec::new(),
            reasons: vec![reason(
                "no_bound_check",
                "该需求尚无显式检查证明，产物存在本身不代表需求已验证",
                Some(&req.id),
            )],
            evaluated_version: None,
        };
    }

    // 归约：failed > blocked > stale > pending。
    let status = if statuses.iter().any(|(s, _)| *s == AcceptanceStatus::Failed) {
        AcceptanceStatus::Failed
    } else if statuses
        .iter()
        .any(|(s, _)| *s == AcceptanceStatus::Blocked)
    {
        AcceptanceStatus::Blocked
    } else if statuses.iter().any(|(s, _)| *s == AcceptanceStatus::Stale) {
        AcceptanceStatus::Stale
    } else if statuses
        .iter()
        .any(|(s, _)| *s == AcceptanceStatus::Pending)
    {
        AcceptanceStatus::Pending
    } else {
        AcceptanceStatus::Passed
    };

    AcceptanceItem {
        requirement_id: req.id.clone(),
        status,
        evidence_refs: evidence,
        reasons: statuses.into_iter().map(|(_, r)| r).collect(),
        evaluated_version,
    }
}

/// Resolve structured artifact constraints only through a real, current, task-bound check.
/// The structured-check branch below verifies physical constraints only, so this does not recurse.
fn evaluate_artifact(
    artifact: &ArtifactRequirement,
    ctx: &EvalContext<'_>,
    versions: &mut Vec<String>,
) -> (AcceptanceStatus, Vec<EvidenceRef>, Option<Reason>) {
    if let Some(reason) = artifacts::validate_artifact_physical(ctx.roots, artifact).reason {
        return (AcceptanceStatus::Failed, vec![], Some(reason));
    }
    let mut evidence = vec![EvidenceRef {
        kind: EvidenceKind::Artifact,
        record_id: artifact.id.clone(),
        owner: ctx.contract.owner.clone(),
        scope_version: None,
    }];
    for constraint in &artifact.constraints {
        let ArtifactConstraint::StructuredCheck {
            validator_id,
            validator_version,
        } = constraint
        else {
            continue;
        };
        let mut failure = (
            AcceptanceStatus::Pending,
            Some(reason(
                "artifact_check_pending",
                format!("结构化校验 `{validator_id}` 尚无本产物当前版本的有效证据"),
                Some(&artifact.id),
            )),
        );
        let mut satisfied = false;
        for check in &ctx.contract.checks {
            let CheckExpectation::Structured {
                validator_id: check_validator,
                validator_version: check_version,
            } = &check.expectation
            else {
                continue;
            };
            if check.kind != CheckKind::Structured
                || check_validator != validator_id
                || matches!((validator_version, check_version), (Some(required), Some(declared)) if required != declared)
            {
                continue;
            }
            let relative = artifact.path.relative_path.replace('\\', "/");
            let relative = relative.trim_start_matches("./");
            // A validation of unrelated inputs is not evidence about this artifact.
            if !version::capture_scope(ctx.roots, &check.scope, &ctx.capture)
                .ok()
                .is_some_and(|scope| {
                    scope.is_valid() && scope.entry(&artifact.path.root_id, relative).is_some()
                })
            {
                continue;
            }
            let mut bound_check = check.clone();
            if let Some(version) = validator_version {
                bound_check.expectation = CheckExpectation::Structured {
                    validator_id: validator_id.clone(),
                    validator_version: Some(version.clone()),
                };
            }
            let (status, refs, _, detail) = evaluate_check(&bound_check, ctx, versions);
            if status == AcceptanceStatus::Passed {
                evidence.extend(refs);
                satisfied = true;
                break;
            }
            failure = (status, detail);
        }
        if !satisfied {
            return (failure.0, evidence, failure.1);
        }
    }
    (AcceptanceStatus::Passed, evidence, None)
}

/// 状态 → 面向用户的稳定说明（原因码保留在 `reasons`，不靠错误码文本当真值）。
fn reason_for(status: AcceptanceStatus, check_id: &str, spec: &CheckSpec) -> Reason {
    let message = match status {
        AcceptanceStatus::Passed => "检查成立".to_string(),
        AcceptanceStatus::Failed => "检查未成立（无有效通过证据）".to_string(),
        AcceptanceStatus::Blocked => "证据不足以支撑通过（采集不完整或依赖缺失）".to_string(),
        AcceptanceStatus::Stale => "相关文件在验证后已变化，证据失效".to_string(),
        AcceptanceStatus::Pending => "尚无该检查的已采纳证据".to_string(),
        AcceptanceStatus::Waived => "用户明确豁免".to_string(),
        AcceptanceStatus::Cancelled => "用户明确取消".to_string(),
    };
    Reason {
        code: match status {
            AcceptanceStatus::Passed => "check_satisfied",
            AcceptanceStatus::Failed => "check_not_satisfied",
            AcceptanceStatus::Blocked => "check_blocked",
            AcceptanceStatus::Stale => "evidence_stale",
            AcceptanceStatus::Pending => "no_evidence",
            AcceptanceStatus::Waived => "user_waived",
            AcceptanceStatus::Cancelled => "user_cancelled",
        }
        .to_string(),
        message,
        target_id: Some(format!("{check_id}:{}", spec.kind_str_hint())),
    }
}

impl CheckSpec {
    /// 人类可读种类（用于说明文本，不参与判定）。
    fn kind_str_hint(&self) -> &'static str {
        match self.kind {
            CheckKind::Command => "command",
            CheckKind::Artifact => "artifact",
            CheckKind::Structured => "structured",
            CheckKind::Review => "review",
            CheckKind::Manual => "manual",
        }
    }
}

fn evaluate_check(
    spec: &CheckSpec,
    ctx: &EvalContext<'_>,
    version_digests: &mut Vec<String>,
) -> (
    AcceptanceStatus,
    Vec<EvidenceRef>,
    Option<String>,
    Option<Reason>,
) {
    let contract = ctx.contract;
    let ledger = ctx.ledger;
    // 每条证据都只认「同会话 + 同 root_run」的已采纳记录：跨会话 / 跨 run 的
    // 回执不能给当前任务作证（迟到结果同理——它们带的是别的 run 的 root_run_id）。
    let owned = |o: &RunIdentity| {
        o.session_id == contract.owner.session_id && o.root_run_id == contract.owner.root_run_id
    };
    let pending = || {
        (
            AcceptanceStatus::Pending,
            Vec::new(),
            None,
            Some(reason(
                "no_evidence",
                "尚无该检查的已采纳证据",
                Some(&spec.id),
            )),
        )
    };
    let failed = |r: Reason| (AcceptanceStatus::Failed, Vec::new(), None, Some(r));

    let evidence_current = |digest: Option<&str>| {
        super::version::capture_scope(ctx.roots, &spec.scope, &ctx.capture)
            .ok()
            .is_some_and(|current| current.is_valid() && digest == Some(current.digest.as_str()))
    };
    if contract.is_waived(&spec.id)
        && let Some((id, decision)) =
            authorized_scope_decision(ctx, &spec.id, UserDecisionKind::Waived)
    {
        return (
            AcceptanceStatus::Waived,
            vec![EvidenceRef {
                kind: EvidenceKind::UserDecision,
                record_id: id.clone(),
                owner: decision.owner.clone(),
                scope_version: decision.scope_version.clone(),
            }],
            decision.scope_version.clone(),
            Some(reason(
                "user_waived",
                "用户明确豁免此项检查",
                Some(&spec.id),
            )),
        );
    }
    match spec.kind {
        CheckKind::Command => {
            let Some((receipt_id, receipt)) = ledger
                .receipts
                .iter()
                .filter(|(_, r)| {
                    r.check_id.as_deref() == Some(spec.id.as_str())
                        && owned(&r.owner)
                        && r.binding
                            .as_ref()
                            .is_some_and(|binding| binding.matches(contract))
                })
                .max_by(|a, b| a.1.finished_at.cmp(&b.1.finished_at).then(a.0.cmp(b.0)))
            else {
                return pending();
            };
            let refs = vec![EvidenceRef {
                kind: EvidenceKind::Execution,
                record_id: receipt_id.clone(),
                owner: receipt.owner.clone(),
                scope_version: receipt.after.as_ref().map(|a| a.digest.clone()),
            }];
            let ver = receipt.after.as_ref().map(|a| a.digest.clone());
            // 退出码核对：只认真实退出，且必须落在允许集合内。
            let allowed: Vec<i32> = match &spec.expectation {
                CheckExpectation::Command { allowed_exit_codes } => allowed_exit_codes.clone(),
                _ => vec![0],
            };
            if !receipt.outcome.has_exit_code() {
                // 没有退出码就没有"通过"：拒绝 / 未启动 / 取消 / 中断各有归属。
                let (status, why) = match receipt.outcome {
                    ExecutionOutcome::Denied => (
                        AcceptanceStatus::Failed,
                        format!(
                            "命令在准入阶段被拒，未执行：{}",
                            receipt.reason.as_deref().unwrap_or("未记录原因")
                        ),
                    ),
                    ExecutionOutcome::Cancelled | ExecutionOutcome::Interrupted => (
                        AcceptanceStatus::Blocked,
                        format!("命令{}，尚无可用结果", receipt.outcome.as_str()),
                    ),
                    other => (
                        AcceptanceStatus::Failed,
                        format!("命令未真实退出（{}）", other.as_str()),
                    ),
                };
                return (
                    status,
                    refs,
                    ver,
                    Some(reason("check_not_satisfied", why, Some(&spec.id))),
                );
            }
            let Some(code) = receipt.exit_code else {
                return (
                    AcceptanceStatus::Failed,
                    refs,
                    ver,
                    Some(reason(
                        "check_not_satisfied",
                        "终态标为已退出但没有退出码（记录不自洽）",
                        Some(&spec.id),
                    )),
                );
            };
            if !allowed.contains(&code) {
                return (
                    AcceptanceStatus::Failed,
                    refs,
                    ver,
                    Some(reason(
                        "exit_code_not_allowed",
                        format!("退出码 {code} 不在允许集合 {allowed:?}"),
                        Some(&spec.id),
                    )),
                );
            }
            let expected_cwd = spec.cwd.clone().unwrap_or(PathRef {
                root_id: "workspace".into(),
                relative_path: ".".into(),
            });
            let resolve_cwd = |path: &PathRef| {
                ctx.roots
                    .get(&path.root_id)
                    .and_then(|root| {
                        crate::tools::pathutil::safe_join(root, &path.relative_path).ok()
                    })
                    .map(|path| crate::tools::pathutil::canonical_best_effort(&path))
            };
            let expected = resolve_cwd(&expected_cwd);
            if expected.is_none() || receipt.cwd.as_ref().and_then(resolve_cwd) != expected {
                return (
                    AcceptanceStatus::Blocked,
                    refs,
                    ver,
                    Some(reason(
                        "check_cwd_mismatch",
                        "执行 cwd 未匹配预登记检查目录",
                        Some(&spec.id),
                    )),
                );
            }
            if spec.independent_verifier
                && !ledger.validations.values().any(|marker| {
                    marker.validator_id == "independent_verification"
                        && marker.check_id.as_deref() == Some(spec.id.as_str())
                        && marker
                            .binding
                            .as_ref()
                            .is_some_and(|binding| binding.matches(contract))
                        && owned(&marker.owner)
                        && marker.owner.actor_id != contract.owner.actor_id
                        && marker.outcome == ValidationOutcome::Passed
                        && evidence_current(marker.scope_version.as_deref())
                        && marker.input_refs.iter().any(|id| {
                            ledger.receipts.get(id).is_some_and(|proof| {
                                proof
                                    .binding
                                    .as_ref()
                                    .is_some_and(|binding| binding.matches(contract))
                                    && proof.owner == marker.owner
                                    && proof.check_id == marker.check_id
                                    && proof.outcome == ExecutionOutcome::Exited
                                    && proof.exit_code == Some(0)
                                    && proof.signal.is_none()
                                    && proof.cwd.as_ref().and_then(resolve_cwd) == expected
                            })
                        })
                })
            {
                return (
                    AcceptanceStatus::Blocked,
                    refs,
                    ver,
                    Some(reason(
                        "independent_verification_missing",
                        "缺少宿主绑定独立验证者的真实执行证据",
                        Some(&spec.id),
                    )),
                );
            }
            freshness_of(receipt, spec, ctx, version_digests, refs, ver)
        }
        CheckKind::Artifact => {
            let CheckExpectation::Artifact { artifact_ids } = &spec.expectation else {
                return failed(reason(
                    "expectation_kind_mismatch",
                    "artifact 检查缺少产物期望",
                    Some(&spec.id),
                ));
            };
            let mut refs = Vec::new();
            for aid in artifact_ids {
                let Some(artifact) = contract.artifact(aid) else {
                    return failed(reason(
                        "dangling_artifact_ref",
                        format!("产物 `{aid}` 不在契约内"),
                        Some(&spec.id),
                    ));
                };
                let (status, evidence, detail) = evaluate_artifact(artifact, ctx, version_digests);
                refs.extend(evidence);
                if status != AcceptanceStatus::Passed {
                    return (status, refs, None, detail);
                }
            }
            (AcceptanceStatus::Passed, refs, None, None)
        }
        CheckKind::Structured => {
            let CheckExpectation::Structured {
                validator_id,
                validator_version,
            } = &spec.expectation
            else {
                return failed(reason(
                    "expectation_kind_mismatch",
                    "structured 检查缺少校验器期望",
                    Some(&spec.id),
                ));
            };
            let Some((vid, v)) = ledger
                .validations
                .iter()
                .filter(|(_, v)| {
                    v.check_id.as_deref() == Some(spec.id.as_str())
                        && v.validator_id == *validator_id
                        && validator_version
                            .as_ref()
                            .is_none_or(|ver| v.validator_version == *ver)
                        && owned(&v.owner)
                        && v.binding
                            .as_ref()
                            .is_some_and(|binding| binding.matches(contract))
                })
                .max_by(|a, b| a.1.finished_at.cmp(&b.1.finished_at).then(a.0.cmp(b.0)))
            else {
                return pending();
            };
            let refs = vec![EvidenceRef {
                kind: EvidenceKind::Validation,
                record_id: vid.clone(),
                owner: v.owner.clone(),
                scope_version: v.scope_version.clone(),
            }];
            if !evidence_current(v.scope_version.as_deref()) {
                return (
                    AcceptanceStatus::Stale,
                    refs,
                    v.scope_version.clone(),
                    Some(reason(
                        "evidence_stale",
                        "结构化校验未绑定当前完整范围版本",
                        Some(&spec.id),
                    )),
                );
            }
            // 只对**声明了同一校验器**约束的产物提要求；无关联产物时按检查自身范围判定。
            let related = artifact_ids_of(validator_id, contract);
            let mut missing = Vec::new();
            for aid in &related {
                if let Some(a) = contract.artifact(aid)
                    && let Some(r) = artifacts::validate_artifact_physical(ctx.roots, a).reason
                {
                    missing.push(r);
                }
            }
            let status = match v.outcome {
                ValidationOutcome::Passed if missing.is_empty() => AcceptanceStatus::Passed,
                ValidationOutcome::Failed => AcceptanceStatus::Failed,
                ValidationOutcome::Cancelled | ValidationOutcome::Interrupted => {
                    AcceptanceStatus::Blocked
                }
                ValidationOutcome::Passed => AcceptanceStatus::Failed,
            };
            let detail = match status {
                AcceptanceStatus::Passed => None,
                AcceptanceStatus::Failed => missing.first().cloned().or(Some(reason(
                    "validation_failed",
                    format!(
                        "校验器 `{}` 判定未通过，共 {} 条发现",
                        validator_id,
                        v.findings.len()
                    ),
                    Some(&spec.id),
                ))),
                _ => Some(reason(
                    "validation_not_completed",
                    format!("校验器 `{validator_id}` 未完成（{:?}）", v.outcome),
                    Some(&spec.id),
                )),
            };
            (status, refs, v.scope_version.clone(), detail)
        }
        CheckKind::Review => {
            let policy = &contract.review_policy;
            let current_scope = version::capture_scope(ctx.roots, &spec.scope, &ctx.capture)
                .ok()
                .filter(|scope| scope.is_valid());
            let Some((rid, r)) = ledger
                .reviews
                .iter()
                .filter(|(_, r)| {
                    r.task_id == contract.task_id
                        && r.check_id.as_deref() == Some(spec.id.as_str())
                        && r.contract_revision == contract.revision
                        && owned(&r.owner)
                })
                .max_by(|a, b| {
                    let current = |review: &ReviewResult| {
                        current_scope.as_ref().is_some_and(|scope| {
                            review.scope_version.as_deref() == Some(scope.digest.as_str())
                        })
                    };
                    current(a.1)
                        .cmp(&current(b.1))
                        .then(a.1.reviewed_at.cmp(&b.1.reviewed_at))
                        .then(a.0.cmp(b.0))
                })
            else {
                return pending();
            };
            let refs = vec![EvidenceRef {
                kind: EvidenceKind::Review,
                record_id: rid.clone(),
                owner: r.owner.clone(),
                scope_version: r.scope_version.clone(),
            }];
            if !evidence_current(r.scope_version.as_deref()) {
                return (
                    AcceptanceStatus::Stale,
                    refs,
                    r.scope_version.clone(),
                    Some(reason(
                        "evidence_stale",
                        "审查未绑定当前完整范围版本",
                        Some(&spec.id),
                    )),
                );
            }
            // **独立性**（#10）：要求独立时，同 actor 冒充审查者的记录不采纳。
            if policy.independent && r.reviewer.same_actor(&contract.owner) {
                return (
                    AcceptanceStatus::Failed,
                    refs,
                    r.scope_version.clone(),
                    Some(reason(
                        "review_not_independent",
                        format!(
                            "审查者身份 `{}` 与执行者相同，独立审查不成立",
                            r.reviewer.actor_id
                        ),
                        Some(&spec.id),
                    )),
                );
            }
            // 维度覆盖：缺维度不算审过（缺哪维说哪维，不笼统判失败）。
            let check_dimensions = match &spec.expectation {
                CheckExpectation::Review { dimensions } => dimensions.as_deref(),
                _ => None,
            };
            let required = policy.required_dimensions(check_dimensions);
            let covered: Vec<&str> = r
                .dimensions
                .iter()
                .filter(|d| match d.state {
                    Some(super::responsibility::DimensionState::Checked) => true,
                    Some(super::responsibility::DimensionState::NotApplicable) => d
                        .reason
                        .as_deref()
                        .is_some_and(|reason| !reason.trim().is_empty()),
                    _ => false,
                })
                .map(|d| d.dimension.as_str())
                .collect();
            let missing: Vec<String> = required
                .iter()
                .filter(|d| !covered.contains(&d.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return (
                    AcceptanceStatus::Failed,
                    refs,
                    r.scope_version.clone(),
                    Some(reason(
                        "review_incomplete_dimensions",
                        format!("审查未覆盖维度：{}", missing.join("、")),
                        Some(&spec.id),
                    )),
                );
            }
            // 布尔判定必须与真实阻断数一致；只信 `approved` 会放过自相矛盾的记录。
            let blocking = r
                .dimensions
                .iter()
                .flat_map(|d| d.findings.iter())
                .filter(|f| policy.is_blocking(f.severity))
                .count();
            let stable_blocking = r
                .dimensions
                .iter()
                .flat_map(|d| &d.review_findings)
                .filter(|f| policy.is_blocking(f.severity))
                .count();
            if r.verdict.approved
                && blocking == 0
                && stable_blocking == 0
                && r.verdict.blocking_count == 0
            {
                return (
                    AcceptanceStatus::Passed,
                    refs,
                    r.scope_version.clone(),
                    None,
                );
            }
            (
                AcceptanceStatus::Failed,
                refs,
                r.scope_version.clone(),
                Some(reason(
                    "review_has_blocking_findings",
                    format!(
                        "审查存在 {blocking} 条阻断级发现（记录声明 {}）",
                        r.verdict.blocking_count
                    ),
                    Some(&spec.id),
                )),
            )
        }
        CheckKind::Manual => {
            // 缺真实交互 → blocked（确实需要人，只是没人答），不是 pending。
            let Some((did, d)) = ledger
                .decisions
                .iter()
                .filter(|(_, d)| {
                    d.binding
                        .as_ref()
                        .is_some_and(|binding| binding.matches(contract))
                        && d.item_ids.iter().any(|i| i == &spec.id)
                        && d.owner.session_id == contract.owner.session_id
                })
                .max_by(|a, b| a.1.answered_at.cmp(&b.1.answered_at).then(a.0.cmp(b.0)))
            else {
                return (
                    AcceptanceStatus::Blocked,
                    Vec::new(),
                    None,
                    Some(reason(
                        "awaiting_user_decision",
                        "该检查需要真实用户确认，尚无已采纳的人工决定",
                        Some(&spec.id),
                    )),
                );
            };
            let refs = vec![EvidenceRef {
                kind: EvidenceKind::UserDecision,
                record_id: did.clone(),
                owner: d.owner.clone(),
                scope_version: d.scope_version.clone(),
            }];
            if !owned(&d.owner) || !evidence_current(d.scope_version.as_deref()) {
                return (
                    AcceptanceStatus::Stale,
                    refs,
                    d.scope_version.clone(),
                    Some(reason(
                        "evidence_stale",
                        "人工决定未绑定当前 run 与完整范围版本",
                        Some(&spec.id),
                    )),
                );
            }
            // 「已人工验证」与「豁免」是两种决定：豁免不是 manual 检查的通过证据。
            match d.decision {
                UserDecisionKind::Cancelled => (
                    AcceptanceStatus::Cancelled,
                    refs,
                    d.scope_version.clone(),
                    Some(reason(
                        "user_cancelled",
                        "用户明确取消此项检查",
                        Some(&spec.id),
                    )),
                ),
                UserDecisionKind::Confirmed => (
                    AcceptanceStatus::Passed,
                    refs,
                    d.scope_version.clone(),
                    None,
                ),
                UserDecisionKind::Waived => (
                    AcceptanceStatus::Waived,
                    refs,
                    d.scope_version.clone(),
                    Some(reason(
                        "user_waived",
                        "用户豁免该项（不等于人工验证通过）",
                        Some(&spec.id),
                    )),
                ),
            }
        }
    }
}

/// 该检查涉及的产物 id：只取**声明了同一校验器**结构化约束的那些，
/// 否则一次校验会被摊到无关产物上（看着更"严格"，实际是拿无关检查冒充本项证据）。
fn artifact_ids_of(validator_id: &str, contract: &TaskContract) -> Vec<String> {
    contract
        .artifacts
        .iter()
        .filter(|a| {
            a.constraints.iter().any(|c| {
                matches!(c, ArtifactConstraint::StructuredCheck { validator_id: v, .. } if v == validator_id)
            })
        })
        .map(|a| a.id.clone())
        .collect()
}

/// 证据新鲜度：重新采集当前范围，与回执 `after` 比对。
///
/// 回执侧采集不完整 → `blocked`（该证据本来就没资格作结论）；当前采集不完整 → `blocked`。
fn freshness_of(
    receipt: &ExecutionReceipt,
    spec: &CheckSpec,
    ctx: &EvalContext<'_>,
    version_digests: &mut Vec<String>,
    refs: Vec<EvidenceRef>,
    ver: Option<String>,
) -> (
    AcceptanceStatus,
    Vec<EvidenceRef>,
    Option<String>,
    Option<Reason>,
) {
    // 回执侧没采到版本：不能判"没变"。该证据本来就没资格作结论 → blocked。
    let Some(after) = receipt.after.as_ref() else {
        return (
            AcceptanceStatus::Blocked,
            refs,
            ver,
            Some(reason(
                "evidence_version_missing",
                "该执行未采集范围版本，无法证明内容未变",
                Some(&spec.id),
            )),
        );
    };
    if receipt.before.as_ref().is_none_or(|before| {
        !version::compare_execution_inputs(before, after, &spec.scope).is_fresh()
    }) {
        return (
            AcceptanceStatus::Stale,
            refs,
            ver,
            Some(reason(
                "evidence_drifted",
                "检查执行前后范围不一致或缺少前版本",
                Some(&spec.id),
            )),
        );
    }
    match version::capture_scope(ctx.roots, &spec.scope, &ctx.capture) {
        Ok(current) => {
            version_digests.push(current.digest.clone());
            match version::compare_scope(after, &current) {
                Freshness::Fresh => (AcceptanceStatus::Passed, refs, Some(current.digest), None),
                Freshness::Stale { changed } => {
                    // stale 是**派生**验收状态：原始执行记录保持原样，只在此处派生。
                    let shown: Vec<String> = changed
                        .iter()
                        .take(5)
                        .map(|c| format!("{}/{}", c.root_id, c.relative_path))
                        .collect();
                    (
                        AcceptanceStatus::Stale,
                        refs,
                        Some(current.digest),
                        Some(reason(
                            "evidence_stale",
                            format!(
                                "验证后相关内容已变化（共 {} 条，如 {}），证据失效需重验",
                                changed.len(),
                                shown.join("、")
                            ),
                            Some(&spec.id),
                        )),
                    )
                }
                Freshness::Unknown { reason: why } => (
                    AcceptanceStatus::Blocked,
                    refs,
                    Some(current.digest),
                    Some(reason("evidence_unverifiable", why, Some(&spec.id))),
                ),
            }
        }
        Err(e) => (
            AcceptanceStatus::Blocked,
            refs,
            ver,
            Some(reason(
                "evidence_unverifiable",
                format!("当前范围采集失败（{}: {}）", e.code, e.message),
                Some(&spec.id),
            )),
        ),
    }
}

/// A scope decision is effective only through its exact committed change reference.
fn authorized_scope_decision<'a>(
    ctx: &'a EvalContext<'_>,
    target: &str,
    kind: UserDecisionKind,
) -> Option<(String, &'a UserDecision)> {
    let change = ctx
        .contract
        .scope_changes
        .iter()
        .rev()
        .find(|change| change.target_id == target)?;
    let id = change.user_decision_id.as_ref()?;
    let decision = ctx.ledger.decisions.get(id)?;
    if !decision.binding.as_ref().is_some_and(|binding| {
        binding.task_id == ctx.contract.task_id && binding.revision < ctx.contract.revision
    }) || decision.decision != kind
        || decision.owner.session_id != ctx.contract.owner.session_id
        || decision.owner.root_run_id != ctx.contract.owner.root_run_id
        || !decision.item_ids.iter().any(|item| item == target)
    {
        return None;
    }
    let scope = super::service::target_scope(ctx.contract, target)?;
    let current = version::capture_scope(ctx.roots, &scope, &ctx.capture).ok()?;
    if !current.is_valid() || decision.scope_version.as_deref() != Some(current.digest.as_str()) {
        return None;
    }
    Some((id.clone(), decision))
}

#[cfg(test)]
#[path = "acceptance_tests.rs"]
mod tests;
