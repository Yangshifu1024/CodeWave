//! #16 完成门与有界修复循环。
//!
//! 这一层只回答一个问题：**模型说完了，能不能算交付通过**。
//!
//! 关键纪律（与 [共同契约](../../../../docs/plan-and-approval.md) §完成门一致）：
//!
//! 1. **运行终态不是验收结论**。`RunTerminal::Done` 只表示模型这一轮停了；只有
//!    [`CompletionDecision::Allow`] 才意味着验收器在已采纳证据上推出了 `passed`。
//!    收尾时缺产物 → 一律不是 `Allow`，界面不得显示"全部完成"。
//! 2. **只有验收器能写 passed**。本模块不提供任何 `set_passed`；它消费
//!    [`AcceptanceSnapshot`]，不生产它。
//! 3. **没有契约的旧问答不额外阻塞**（`contract = None` → 直接 `Allow`），不为每条
//!    普通回答加门。
//! 4. **用户取消优先于一切**：取消时既不派发修复也不开新检查，已发生的副作用如实保留。
//! 5. **预算耗尽即停止**：`max_steps` / `force_report` 收口后绝不再开一个"修复 run"，
//!    记 `Blocked` 并保留 partial。

use super::types::{AcceptanceSnapshot, AcceptanceStatus, AcceptanceStatusKind, Reason};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 每任务修复轮上限（首版规格值）。
pub const MAX_REPAIR_ROUNDS: u32 = 2;
/// 同一稳定失败指纹允许的**重复**修复次数（首轮修复不受此限）。
pub const MAX_REPEAT_PER_FINGERPRINT: u32 = 1;
/// 修复额外 LLM step 总上限；总调用 / 耗时仍受原 run 上限约束。
pub const MAX_REPAIR_EXTRA_STEPS: u32 = 12;

/// 一个待修复缺项。
///
/// 缺项**只从验收快照推导**，不由模型自述：模型说"我改好了"不构成缺口消失。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    /// 所属需求。
    pub requirement_id: String,
    /// 关联检查（无关联时为 `None`，如产物缺失）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    /// 稳定原因码（来自验收器，不来自模型文本）。
    pub reason_code: String,
    /// 面向用户的说明。
    pub message: String,
    /// 稳定失败指纹（**不含时刻与随机输出**，换一句话改不动它）。
    pub fingerprint: String,
    /// 需要用户提供信息或环境资源——这类不自动循环修，直接 blocked。
    #[serde(default)]
    pub needs_user_or_env: bool,
}

impl Gap {
    /// 判定是否属于"等用户 / 等环境"：这类缺口重复尝试没有意义。
    ///
    /// 判据是**原因码**，不是文案：文案可以被模型改写，原因码由验收器写死。
    pub fn needs_user_or_env(reason_code: &str) -> bool {
        matches!(
            reason_code,
            "check_blocked" | "evidence_unverifiable" | "evidence_incomplete"
        )
    }
}

/// 稳定失败指纹。
///
/// 组成 = `check_id + reason_code + 规范化失败位置 + 目标范围类别`，**不含时刻与随机输出**。
///
/// 为什么只取"规范化失败位置"而不取整句消息：模型换个说法（"断言失败" → "expected true"）
/// 不应该换到一个新预算；反过来，**位置真的变了**（换了个文件修）才允许再修一次。
pub fn fingerprint(
    check_id: Option<&str>,
    reason_code: &str,
    message: &str,
    scope_class: &str,
) -> String {
    format!(
        "{}|{}|{}|{}",
        check_id.unwrap_or("-"),
        reason_code,
        scope_class,
        normalize_locations(message)
    )
}

/// 规范化失败位置：只保留"路径 + 行号"形态的定位信息，其余一律抹掉。
///
/// 时刻、耗时、十六进制摘要、临时目录、随机 id、free prose 全部不参与——它们每轮都在变，
/// 参与进来就等于每次失败都是"新失败"，预算形同虚设。
///
/// 判定刻意很窄：只有**带目录分隔符**或**带字母文件扩展名**的 token 才算定位，
/// 否则 `true` / `1.23s` / `03:21:44` 都会被误当成位置参与指纹。
pub fn normalize_locations(message: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for token in message.split_whitespace() {
        let Some(head) = location_head(token) else {
            continue;
        };
        out.push(shape_of(token, head));
    }
    out.sort();
    out.dedup();
    out.join(",")
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | ':' | '-' | '_')
}

/// 取 token 的"定位头部"：跳过前导括号等修饰，再吃掉一段词字符。
fn location_head(token: &str) -> Option<&str> {
    let body = token.trim_start_matches(|c: char| !is_word(c));
    let end = body.find(|c: char| !is_word(c)).unwrap_or(body.len());
    let head = &body[..end];
    looks_like_location(head).then_some(head)
}

/// 是否像一处定位：带目录分隔符，或末段是**字母**文件扩展名（`1.23s` 的 `23s` 不算）。
fn looks_like_location(head: &str) -> bool {
    if head.contains('/') {
        return true;
    }
    head.rsplit_once('.').is_some_and(|(base, ext)| {
        !base.is_empty()
            && !ext.is_empty()
            && ext.len() <= 5
            && ext.chars().all(|c| c.is_alphabetic())
    })
}

/// 归一后的定位形态：只留末两段路径 + 行号位（数字全替换，避免行号漂移重置预算）。
fn shape_of(token: &str, head: &str) -> String {
    let (path_part, line_part) = match head.split_once(':') {
        Some((p, rest)) => (p, Some(rest)),
        None => (head, None),
    };
    let tail: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
    let path = if tail.len() > 2 {
        tail[tail.len() - 2..].join("/")
    } else {
        tail.join("/")
    };
    let line = line_part.map(|_| ":##").unwrap_or_default();
    let _ = token;
    format!("{path}{line}")
}

/// 修复预算（**持久化**，重启不归零）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepairBudget {
    #[serde(default)]
    pub operations: BTreeMap<String, String>,
    /// 已用修复轮数。
    #[serde(default)]
    pub rounds_used: u32,
    /// 已用额外 step。
    #[serde(default)]
    pub extra_steps_used: u32,
    /// 指纹 → 已为它开过几轮修复。
    #[serde(default)]
    pub fingerprints: BTreeMap<String, u32>,
}

impl RepairBudget {
    /// 某组缺项当前是否还可进入修复。
    ///
    /// 三个闸门任一不满足即不可修复：轮次用尽、额外 step 用尽、**或本组里有任一指纹
    /// 重复次数已到上限**（换一句话改不动指纹，所以这不是可绕过的）。
    pub fn can_repair(&self, gaps: &[Gap]) -> bool {
        !gaps.is_empty()
            && self.rounds_used < MAX_REPAIR_ROUNDS
            && self.extra_steps_used < MAX_REPAIR_EXTRA_STEPS
            && !self.blocking_fingerprint(gaps).is_some()
    }

    /// 返回阻止本组修复的指纹（若有）。
    pub fn blocking_fingerprint(&self, gaps: &[Gap]) -> Option<String> {
        gaps.iter()
            .map(|g| &g.fingerprint)
            .find(|fp| {
                self.fingerprints
                    .get(*fp)
                    .is_some_and(|n| *n > MAX_REPEAT_PER_FINGERPRINT)
            })
            .cloned()
    }

    /// 记一次修复轮消耗（**先判后记**，不满足时不得改动预算）。
    pub fn consume(&mut self, gaps: &[Gap]) {
        self.rounds_used += 1;
        for g in gaps {
            *self.fingerprints.entry(g.fingerprint.clone()).or_insert(0) += 1;
        }
    }

    /// 记额外 step 消耗。
    pub fn consume_steps(&mut self, steps: u32) {
        self.extra_steps_used = self.extra_steps_used.saturating_add(steps);
    }
}

/// 完成门裁决。
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionDecision {
    /// 允许交付通过。
    ///
    /// `snapshot = None` 只在**无契约**时出现（旧问答沿原收尾）；有契约而通过时
    /// 快照必为 `overall = Passed`。
    Allow {
        snapshot: Option<AcceptanceSnapshot>,
        reason: String,
    },
    /// 缺可修复项且预算允许：进入一轮有界修复。
    Repair {
        snapshot: AcceptanceSnapshot,
        gaps: Vec<Gap>,
        reason: String,
    },
    /// 缺项但不可自动修复：保留成果，明确阻塞。
    Blocked {
        snapshot: AcceptanceSnapshot,
        gaps: Vec<Gap>,
        reason: String,
    },
    /// 用户取消：优先于修复与检查。
    Cancelled {
        snapshot: Option<AcceptanceSnapshot>,
        reason: String,
    },
}

impl CompletionDecision {
    /// 是否放行交付通过。**只有验收器推出的 `passed`（或无契约旧问答）才为真。**
    pub fn allows_completion(&self) -> bool {
        match self {
            CompletionDecision::Allow { snapshot, .. } => match snapshot {
                None => true,
                Some(s) => s.overall == AcceptanceStatusKind::Passed,
            },
            _ => false,
        }
    }

    /// 是否需要下游派发一轮修复（供 drive 决定要不要继续跑模型）。
    pub fn needs_repair_run(&self) -> bool {
        matches!(self, CompletionDecision::Repair { .. })
    }

    /// 供界面 / 事件使用的紧凑摘要。
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "decision": self.as_str(),
            "allows_completion": self.allows_completion(),
            "snapshot": self.snapshot().map(snapshot_summary),
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CompletionDecision::Allow { .. } => "allow",
            CompletionDecision::Repair { .. } => "repair",
            CompletionDecision::Blocked { .. } => "blocked",
            CompletionDecision::Cancelled { .. } => "cancelled",
        }
    }

    pub fn snapshot(&self) -> Option<&AcceptanceSnapshot> {
        match self {
            CompletionDecision::Allow { snapshot, .. }
            | CompletionDecision::Cancelled { snapshot, .. } => snapshot.as_ref(),
            CompletionDecision::Repair { snapshot, .. }
            | CompletionDecision::Blocked { snapshot, .. } => Some(snapshot),
        }
    }

    /// 缺口列表（界面显示"还差什么"）。
    pub fn gaps(&self) -> &[Gap] {
        match self {
            CompletionDecision::Repair { gaps, .. } | CompletionDecision::Blocked { gaps, .. } => {
                gaps
            }
            _ => &[],
        }
    }
}

/// 验收未通过却没给出原因时的兜底原因（稳定码，不让"没原因"变成缺口消失）。
fn unspecified_gap() -> Reason {
    Reason {
        code: "unspecified_gap".into(),
        message: "验收未通过且未给出原因".into(),
        target_id: None,
    }
}

/// 验收快照 → 缺口列表。**唯一事实源**，模型不能自称已解决。
pub fn gaps_of(snapshot: &AcceptanceSnapshot) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for item in &snapshot.items {
        // waived 与 cancelled 都不是缺口：前者是用户明确豁免，后者是用户明确取消。
        if matches!(
            item.status,
            AcceptanceStatus::Passed | AcceptanceStatus::Waived | AcceptanceStatus::Cancelled
        ) {
            continue;
        }
        let reasons: Vec<Reason> = if item.reasons.is_empty() {
            vec![unspecified_gap()]
        } else {
            item.reasons.clone()
        };
        for r in &reasons {
            gaps.push(Gap {
                requirement_id: item.requirement_id.clone(),
                check_id: r.target_id.clone(),
                reason_code: r.code.clone(),
                message: r.message.clone(),
                fingerprint: fingerprint(
                    r.target_id.as_deref(),
                    &r.code,
                    &r.message,
                    // 快照本身不携带检查范围，退化为需求级类别；范围更细的类别由
                    // 调用方在有契约时用 `refine_gaps_with_contract` 覆盖。
                    &format!("req:{}", item.requirement_id),
                ),
                needs_user_or_env: Gap::needs_user_or_env(&r.code),
            });
        }
    }
    gaps
}

/// 用契约里的检查范围细化指纹（目标范围类别）。
///
/// 快照只到需求粒度，预算指纹必须落到"哪个检查、什么范围"——否则同一需求下换一个
/// 检查失败会被当成同一指纹，白白浪费一次允许的重复修复。
pub fn refine_gaps_with_contract(gaps: &mut [Gap], contract: &super::types::TaskContract) {
    for g in gaps.iter_mut() {
        let Some(check_id) = g.check_id.as_deref() else {
            continue;
        };
        let Some(spec) = contract.check(check_id) else {
            continue;
        };
        g.fingerprint = fingerprint(
            Some(check_id),
            &g.reason_code,
            &g.message,
            &scope_class(&spec.scope),
        );
    }
}

/// 目标范围类别（参与指纹；不参与验收判定）。
fn scope_class(scope: &super::types::VerificationScope) -> String {
    format!(
        "roots={};in={};dep={}",
        scope.roots.len(),
        scope.inputs.len(),
        scope.dependency_files.len()
    )
}

/// 快照 → 紧凑 JSON（事件与 UI 投影共用）。
pub fn snapshot_summary(snapshot: &AcceptanceSnapshot) -> serde_json::Value {
    serde_json::json!({
        "task_id": snapshot.task_id,
        "revision": snapshot.revision,
        "overall": snapshot.overall,
        "waived_count": snapshot.waived_count,
        "evaluated_version": snapshot.evaluated_version,
        "items": snapshot.items.iter().map(|i| serde_json::json!({
            "requirement_id": i.requirement_id,
            "status": i.status,
            "reasons": i.reasons,
        })).collect::<Vec<_>>(),
        "reasons": snapshot.reasons,
    })
}

/// 完成门裁决。**drive 的每个正常收尾点都必须经过这里**，没有旁路。
///
/// - `contract = None` → 无契约的旧问答沿原收尾，不额外阻塞。
/// - `cancelled = true` → 用户取消优先，不派发修复也不开检查。
/// - `whole_cancelled`（整个任务被取消）→ 同上。
/// - `save_ok = false` → 保存失败明确标未知并阻止通过（**不重试副作用**）。
#[allow(clippy::too_many_arguments)]
pub fn gate(
    contract: Option<&super::types::TaskContract>,
    snapshot: Option<&AcceptanceSnapshot>,
    budget: &RepairBudget,
    cancelled: bool,
    whole_task_cancelled: bool,
    save_ok: bool,
) -> CompletionDecision {
    // 无契约：旧问答沿原收尾。
    let Some(contract) = contract else {
        return CompletionDecision::Allow {
            snapshot: snapshot.cloned(),
            reason: "无交付契约：沿原收尾路径，不额外阻塞".into(),
        };
    };
    if cancelled || whole_task_cancelled {
        return CompletionDecision::Cancelled {
            snapshot: snapshot.cloned(),
            reason: "用户取消优先：保留已发生副作用，不派发修复或新检查".into(),
        };
    }
    let Some(snapshot) = snapshot else {
        // 有契约却求不出快照 = 事实不可读，按 blocked 处理，绝不按"没有要求"放行。
        return CompletionDecision::Blocked {
            snapshot: AcceptanceSnapshot {
                task_id: contract.task_id.clone(),
                contract_revision: contract.revision,
                revision: 0,
                items: Vec::new(),
                overall: AcceptanceStatusKind::Blocked,
                waived_count: 0,
                evaluated_version: None,
                reasons: vec![Reason {
                    code: "acceptance_unavailable".into(),
                    message: "无法取得验收快照：完成状态未知，不得推断为通过".into(),
                    target_id: None,
                }],
            },
            gaps: Vec::new(),
            reason: "验收快照不可用：完成状态未知".into(),
        };
    };
    if !save_ok {
        return CompletionDecision::Blocked {
            snapshot: snapshot.clone(),
            gaps: Vec::new(),
            reason: "验收事实未能落盘（保存失败）：标未知并阻止通过，不盲目重试副作用".into(),
        };
    }
    if snapshot.overall == AcceptanceStatusKind::Passed {
        let waived = snapshot.waived_count;
        return CompletionDecision::Allow {
            snapshot: Some(snapshot.clone()),
            reason: if waived > 0 {
                format!("验收通过（含 {waived} 项用户豁免）")
            } else {
                "验收通过".into()
            },
        };
    }
    let mut gaps = gaps_of(snapshot);
    refine_gaps_with_contract(&mut gaps, contract);
    let waiting_on_user = gaps.iter().any(|g| g.needs_user_or_env);
    if waiting_on_user {
        return CompletionDecision::Blocked {
            snapshot: snapshot.clone(),
            gaps,
            reason: "需要用户提供信息或环境资源：不自动循环问同一问题，保留成果待确认".into(),
        };
    }
    if budget.can_repair(&gaps) {
        return CompletionDecision::Repair {
            snapshot: snapshot.clone(),
            reason: format!(
                "缺 {} 项且预算允许（第 {}/{MAX_REPAIR_ROUNDS} 轮）",
                gaps.len(),
                budget.rounds_used + 1
            ),
            gaps,
        };
    }
    let reason = match budget.blocking_fingerprint(&gaps) {
        Some(fp) => format!("同一失败指纹已修复过（{fp}）：不重复修复，保留成果并标阻塞"),
        None if budget.rounds_used >= MAX_REPAIR_ROUNDS => {
            format!(
                "修复轮次已用尽（{}/{MAX_REPAIR_ROUNDS}）",
                budget.rounds_used
            )
        }
        None if budget.extra_steps_used >= MAX_REPAIR_EXTRA_STEPS => {
            format!(
                "修复额外 step 已用尽（{}/{MAX_REPAIR_EXTRA_STEPS}）",
                budget.extra_steps_used
            )
        }
        _ => "缺项不可自动修复".to_string(),
    };
    CompletionDecision::Blocked {
        snapshot: snapshot.clone(),
        gaps,
        reason,
    }
}

/// 收尾动作：这个停止点上还要不要再跑一轮有界修复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopAction {
    /// 停止运行（通过、阻塞或取消收尾——三者都保留成果）。
    Stop,
    /// 继续跑一轮修复（**已按预算扣账**）。
    Repair,
}

/// 把裁决落到本次停止点的动作。
///
/// `next_step >= max_steps` 时**绝不**再开一轮：`force_report` 与 `max_steps` 已经收口，
/// 此时另起一轮就是无限续跑的入口——按规格记 partial/blocked 并停止。
pub fn stop_action(
    decision: &CompletionDecision,
    next_step: usize,
    max_steps: usize,
) -> StopAction {
    match decision {
        CompletionDecision::Repair { .. } if next_step < max_steps => StopAction::Repair,
        _ => StopAction::Stop,
    }
}

/// 把缺口写回模型可见的反馈（**data**，不是 warnings——后者模型一个字都收不到）。
pub fn repair_feedback(gaps: &[Gap], budget: &RepairBudget) -> serde_json::Value {
    serde_json::json!({
        "acceptance": "blocked",
        "gaps": gaps.iter().map(|g| serde_json::json!({
            "requirement_id": g.requirement_id,
            "check_id": g.check_id,
            "reason_code": g.reason_code,
            "message": g.message,
        })).collect::<Vec<_>>(),
        "repair_rounds_used": budget.rounds_used,
        "repair_rounds_max": MAX_REPAIR_ROUNDS,
        "repair_steps_used": budget.extra_steps_used,
        "repair_steps_max": MAX_REPAIR_EXTRA_STEPS,
        "next": "只修复上述缺项；不要重述已完成的工作，也不要更换失败表述以重置预算。",
    })
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
