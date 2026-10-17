//! 职责分离、独立审查身份与干净验证上下文（任务 #9 / #10 / #11）。
//!
//! 三个任务共用一个判断：**「谁做的」「谁审的」「谁验的」必须是可核对的身份，而不是角色名**。
//! 角色字符串只是用途——"以 reviewer 角色自居"不构成独立审查，"换个模型跑一遍"也不构成
//! 独立身份（同一 actor 换模型仍是同一个人）。
//!
//! | 任务 | 本文件提供 |
//! |---|---|
//! | [#9](../../../../docs/tools-and-safety.md) | [`TaskPolicy`]、[`ResponsibilityAssignment`] 及其校验 |
//! | [#10](../../../../docs/tools-and-safety.md) | [`ReviewAssignment`]、逐维结论、finding 结构与签发校验 |
//! | [#11](../../../../docs/tools-and-safety.md) | [`VerificationInput`]、[`ContextManifest`]、clean 锁定与职责只读上限 |
//!
//! 最要紧的一条在 [`DUTY_READONLY_EXCLUDES`]：**审查与验证是职责级只读上限，不随审批档位
//! 放宽**。既有只读角色策略在 FullAccess 档会解锁写工具（那是「用户授权我写」的语义），
//! 而"审查者不该改被审的代码"是**职责**约束——让根会话切 FullAccess 就写得了，等于这次
//! 审查当场作废（自己改自己审）。

use super::types::{
    FindingSeverity, PathRef, REVIEW_DIMENSIONS, ReviewPolicy, RootIdentity, RunIdentity,
    ValidationFinding,
};
use serde::{Deserialize, Serialize};

/// 职责级只读排除集（[#10](../../../../docs/tools-and-safety.md)）。
///
/// **与 [`crate::core::agent::WRITE_TOOLS`] 的区别**：那是「本档位不写」的排除集，
/// 可被 FullAccess 解锁；这里是「本职责不写」的上限，**任何档位都解锁不了**。
/// 审查者改代码后，旧结论对被改内容的证明力立刻归零——独立审查失去意义。
pub const DUTY_READONLY_EXCLUDES: &[&str] = &[
    "edit",
    "create",
    "delete",
    "write_document",
    "edit_document",
];

/// 子任务职责（[#9](../../../../docs/tools-and-safety.md) / [#10](../../../../docs/tools-and-safety.md)）。
///
/// 职责决定**工具上限**，且这个上限**不从审批档位派生**——见 [`DUTY_READONLY_EXCLUDES`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyKind {
    /// 实施：按会话档位正常授权。
    Implement,
    /// 审查：职责级只读，不改被审代码。
    Review,
    /// 验证：职责级只读 + 不派活、不改计划基线。
    Verify,
}

impl DutyKind {
    /// 该职责的工具排除集（与档位无关，**任何档位都不解锁**）。
    pub fn excludes(self) -> Vec<String> {
        match self {
            DutyKind::Implement => Vec::new(),
            DutyKind::Review => review_excludes(),
            DutyKind::Verify => verification_excludes(),
        }
    }
}

// ---------- #9 职责分离 ----------

/// 任务策略：决定要不要走完整三职责流水线（[#9](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPolicy {
    /// 问答：无交付流水线，不产生契约与验收。
    Conversation,
    /// 机械修改：可由宿主直接检查定论，不必强制三职责。
    Mechanical,
    /// 完整流程：实现 / 审查 / 验证三职责齐备。
    Full,
}

impl TaskPolicy {
    /// wire 串。
    pub fn as_str(self) -> &'static str {
        match self {
            TaskPolicy::Conversation => "conversation",
            TaskPolicy::Mechanical => "mechanical",
            TaskPolicy::Full => "full",
        }
    }
    /// 是否要求独立审查。
    pub fn requires_review(self) -> bool {
        matches!(self, TaskPolicy::Full)
    }
    /// 是否要求验证职责。
    pub fn requires_verification(self) -> bool {
        matches!(self, TaskPolicy::Full)
    }
}

/// 职责分派（[#9](../../../../docs/tools-and-safety.md)）。
///
/// `review_actor` / `verification_actor` 为 `None` 时**只在该策略不要求时合法**——
/// 校验函数会拒绝"完整流程但没有审查身份"这种省事写法。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponsibilityAssignment {
    /// 覆盖的需求 id。
    #[serde(default)]
    pub requirement_ids: Vec<String>,
    /// 实现者身份。
    pub implementation_actor: RunIdentity,
    /// 审查者身份（`None` = 由宿主检查承担，见 [`TaskPolicy::Mechanical`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_actor: Option<RunIdentity>,
    /// 验证者身份（专用 tester 或宿主检查；**不伪造 tester 执行**）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_actor: Option<RunIdentity>,
    /// 策略。
    pub policy: TaskPolicy,
}

/// 校验职责分派。
///
/// 三条不可让步：
/// 1. 完整流程要求**实现与审查不同 actor 且不同 actor_run**（同 actor 换角色不算独立）；
/// 2. 三者必须同 `root_run_id`——跨 run 的身份无法对本轮产物负责；
/// 3. 验证者若存在，也不得与实现者同 actor（"自己写自己验"同样不算验证）。
pub fn validate_responsibility(a: &ResponsibilityAssignment) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    let implr = &a.implementation_actor;

    if a.policy.requires_review() {
        match &a.review_actor {
            None => problems.push("完整流程缺少审查身份（不能用『大家自己看看』代替）".into()),
            Some(r) => {
                if r.actor_id == implr.actor_id {
                    problems.push(format!(
                        "审查者与实现者同为 actor `{}`：换角色/换模型都不构成独立身份",
                        implr.actor_id
                    ));
                }
                if r.actor_run_id == implr.actor_run_id {
                    problems.push("审查者与实现者共用同一次 actor run".into());
                }
                if r.root_run_id != implr.root_run_id {
                    problems.push(format!(
                        "审查者属于 root run {}，与实现者的 {} 不同",
                        r.root_run_id, implr.root_run_id
                    ));
                }
            }
        }
    }
    if a.policy.requires_verification() {
        match &a.verification_actor {
            None => problems.push("完整流程缺少验证身份".into()),
            Some(v) => {
                if v.actor_id == implr.actor_id {
                    problems.push("验证者与实现者同 actor：自写自验不算验证".into());
                }
                if v.root_run_id != implr.root_run_id {
                    problems.push("验证者与实现者不在同一 root run".into());
                }
            }
        }
    }
    // 同 actor_run 出现在两个职责位上，永远是分派错误（与是否要求无关）。
    if let (Some(r), Some(v)) = (&a.review_actor, &a.verification_actor)
        && r.actor_run_id == v.actor_run_id
    {
        problems.push("审查与验证共用同一次 actor run".into());
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

// ---------- #10 独立审查 ----------

/// 审查分派（[#10](../../../../docs/tools-and-safety.md)）。
///
/// 由**宿主**在派发审查子代理时签发，存进交付边车。审查者只能凭这张分派提交结论——
/// 入参里自称的 `reviewer_id` 一律无效（模型可以编任何字符串）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewAssignment {
    /// 审查 id（宿主生成）。
    pub review_id: String,
    /// 实现者 actor 集合（一个任务可能多人接力实现）。
    pub implementer_actor_ids: Vec<String>,
    /// 被审目标的范围版本（审查结论只对该版本有效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_version: Option<String>,
    /// 审查策略修订。
    pub policy_revision: u64,
}

/// 单维审查结论状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionState {
    /// 已检查且无阻断项。
    Checked,
    /// 该维度对本任务不适用（必须给理由）。
    NotApplicable,
    /// 未验证（**缺维度即保持此项**，不得当作通过）。
    Unverified,
}

/// 单维结论。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionOutcome {
    /// 维度名。
    pub dimension: String,
    /// 状态。
    pub state: DimensionState,
    /// 理由（`NotApplicable` / `Unverified` 时必填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 该维发现。
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
}

/// 审查发现（[#10](../../../../docs/tools-and-safety.md)）。
///
/// 与 [`ValidationFinding`] 的差别：**有稳定 id**，所以复审可以逐项引用旧 finding 并说明
/// 是否解决；缺可定位证据的 finding **不清零**，但标"需人工核对"——它仍会阻断。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewFinding {
    /// 稳定 id（同一问题跨轮次一致）。
    pub stable_id: String,
    /// 严重度。
    pub severity: FindingSeverity,
    /// 定位路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 行范围（闭区间）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(u32, u32)>,
    /// 说明。
    pub message: String,
    /// 支撑证据（回执 id / 文件引用）；缺证据不降级，只标需人工核对。
    #[serde(default)]
    pub evidence: Vec<String>,
    /// 是否有可定位证据（供 UI 分"需人工核对"）。
    pub needs_human_check: bool,
}

/// 阻断级严重度默认集合（[#10](../../../../docs/tools-and-safety.md)）。
pub const DEFAULT_BLOCKING_SEVERITIES: &[FindingSeverity] = &[FindingSeverity::Error];

/// 校验一次审查提交能否被采纳。
///
/// `actor` 是**宿主从真实 runtime 补齐的身份**（不接受工具入参）；`assignment` 来自边车
/// 索引（不接受工具入参）。两条都对了才谈内容。
pub fn validate_review_submission(
    assignment: &ReviewAssignment,
    actor: &RunIdentity,
    expected_root_run: &str,
    policy: &ReviewPolicy,
) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    if actor.root_run_id != expected_root_run {
        problems.push(format!(
            "审查者属于 root run {}，目标属于 {expected_root_run}",
            actor.root_run_id
        ));
    }
    if policy.independent && assignment.implementer_actor_ids.contains(&actor.actor_id) {
        problems.push(format!(
            "actor `{}` 在实现者集合内：自审不是独立审查",
            actor.actor_id
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// 逐维完整性：**缺维度**保持 `Unverified`；`not_applicable` 只要给了**具体理由**就算已处置
/// （[#10](../../../../docs/tools-and-safety.md)：逐维记录 checked/not_applicable/unverified
/// 并附理由——理由就是问责凭据，不是免检牌）。
///
/// 反过来，`not_applicable` 不给理由、或 `unverified` 无论理由如何，都仍是缺口。
pub fn dimension_gaps(outcomes: &[DimensionOutcome], required: &[String]) -> Vec<String> {
    let covered: Vec<&str> = outcomes.iter().map(|o| o.dimension.as_str()).collect();
    let mut gaps = Vec::new();
    for d in required {
        if !covered.contains(&d.as_str()) {
            gaps.push(format!("维度 `{d}` 未提交结论"));
            continue;
        }
        let o = outcomes.iter().find(|o| &o.dimension == d).unwrap();
        match o.state {
            DimensionState::Checked => {}
            DimensionState::NotApplicable => match o.reason.as_deref().map(str::trim) {
                Some(r) if !r.is_empty() => {}
                _ => gaps.push(format!(
                    "维度 `{d}` 标为 not_applicable 但未给理由：不允许无理由豁免"
                )),
            },
            DimensionState::Unverified => {
                gaps.push(format!(
                    "维度 `{d}` 仍为 unverified{}",
                    o.reason
                        .as_deref()
                        .filter(|r| !r.trim().is_empty())
                        .map(|r| format!("（{r}）"))
                        .unwrap_or_default()
                ));
            }
        }
    }
    gaps
}

/// 七维默认清单。
pub fn default_dimensions() -> Vec<String> {
    REVIEW_DIMENSIONS.iter().map(|s| (*s).to_string()).collect()
}

/// 维度结论 → [`ValidationFinding`] 投影（供 #18 统一消费，避免两套 findings 语义）。
pub fn to_validation_findings(outcomes: &[DimensionOutcome]) -> Vec<ValidationFinding> {
    let mut out = Vec::new();
    for o in outcomes {
        for f in &o.findings {
            out.push(ValidationFinding {
                severity: f.severity,
                location: f
                    .path
                    .clone()
                    .map(|p| match f.line_range {
                        Some((a, b)) => format!("{p}:{a}-{b}"),
                        None => p,
                    })
                    .or_else(|| Some(f.message.clone())),
                message: f.message.clone(),
            });
        }
    }
    out
}

// ---------- #11 干净验证上下文 ----------

/// 验证输入（[#11](../../../../docs/tools-and-safety.md)）。
///
/// **刻意不含实现者推理全文**：只有需求、可检查产物、检查规范、环境事实、目标版本与授权根。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationInput {
    /// 契约修订。
    pub contract_revision: u64,
    /// 要验证的检查 id。
    #[serde(default)]
    pub check_ids: Vec<String>,
    /// 可检查产物引用。
    #[serde(default)]
    pub artifact_refs: Vec<PathRef>,
    /// 需要的非秘密环境事实（如 `rustc` 版本键）。
    #[serde(default)]
    pub environment_facts: Vec<String>,
    /// 被验证目标的内容版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_version: Option<String>,
    /// 允许读取的根。
    #[serde(default)]
    pub allowed_roots: Vec<RootIdentity>,
}

/// 单条输入的登记项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// 输入类别（requirement / check_spec / artifact / environment / explicit_note）。
    pub kind: String,
    /// 引用标识。
    pub ref_id: String,
    /// 内容摘要（**只存 hash**，不存原文，尤其不存环境值）。
    pub hash: String,
}

/// 验证上下文清单（[#11](../../../../docs/tools-and-safety.md)）。
///
/// 证明"验证者看到了什么"，而不是"它被允许看到什么"：只登记**引用与摘要**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifest {
    /// 契约修订。
    pub contract_revision: u64,
    /// 登记项。
    #[serde(default)]
    pub entries: Vec<ManifestEntry>,
    /// 整体摘要。
    pub digest: String,
}

/// 构造验证上下文清单。
///
/// 输入按**白名单选择**逐条登记——不复制主历史再删字段（那会让实现者的推理在内存里
/// 完整存在过一次，"已经删干净"只是自我安慰）。
pub fn build_context_manifest(
    input: &VerificationInput,
    requirement_titles: &[(String, String)],
    check_specs: &[(String, String)],
    explicit_notes: &[(String, String)],
) -> ContextManifest {
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut push = |kind: &str, ref_id: &str, body: &str| {
        entries.push(ManifestEntry {
            kind: kind.to_string(),
            ref_id: ref_id.to_string(),
            hash: super::version::digest_of_bytes(body.as_bytes()),
        });
    };
    for (id, title) in requirement_titles {
        push("requirement", id, title);
    }
    for (id, spec) in check_specs {
        push("check_spec", id, spec);
    }
    for a in &input.artifact_refs {
        push(
            "artifact",
            &a.relative_path,
            &format!("{}::{}", a.root_id, a.relative_path),
        );
    }
    for k in &input.environment_facts {
        // 只登记**键名**：环境值不落盘、不进历史、不进 provider。
        push("environment", k, k);
    }
    // 必要实现说明可作为显式输入，但来源必须记录（谁写的、为什么需要）。
    for (source, note) in explicit_notes {
        push("explicit_note", source, note);
    }
    entries.sort_by(|a, b| {
        (a.kind.as_str(), a.ref_id.as_str()).cmp(&(b.kind.as_str(), b.ref_id.as_str()))
    });
    let digest = super::version::digest_of_bytes(
        entries
            .iter()
            .map(|e| format!("{}|{}|{}", e.kind, e.ref_id, e.hash))
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    );
    ContextManifest {
        contract_revision: input.contract_revision,
        entries,
        digest,
    }
}

/// 验证职责的能力上限（[#11](../../../../docs/tools-and-safety.md)）。
///
/// 首版只给「读 + 跑验证命令」。**声明会写的检查必须单独列 outputs 并走审批**——
/// 不能靠贴一个"我是来验证的"标签就拿到广泛写权。
pub fn verification_capability() -> &'static [&'static str] {
    &["read", "grep", "list_files", "command", "read_document"]
}

/// 是否在验证职责的允许能力内。
pub fn is_verification_capability(tool: &str) -> bool {
    verification_capability().contains(&tool)
}

/// 验证职责应排除的工具（职责级，与档位无关）。
pub fn verification_excludes() -> Vec<String> {
    let mut v: Vec<String> = DUTY_READONLY_EXCLUDES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    // 子代理自身派生与计划工具在验证职责下同样关闭：验证者不该再派活或改计划基线。
    v.push("subagent".into());
    v.push("plan".into());
    v
}

/// 审查职责应排除的工具（职责级，与档位无关）。
pub fn review_excludes() -> Vec<String> {
    DUTY_READONLY_EXCLUDES
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

#[cfg(test)]
#[path = "responsibility_tests.rs"]
mod tests;
