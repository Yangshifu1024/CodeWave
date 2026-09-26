//! 目标模式：持久合同、生命周期与阶段提示。执行权限复用完全访问。
//!
//! 本模块只做纯数据 + 纯函数，不碰 IO、不碰锁：状态本身存在 `SessionRuntime.goal`
//! （内存 + `sessions/<id>.goal.json` 边车持久化），读写入口是 `tools/goal.rs` 的 `goal` 工具，
//! 阶段注入（系统提示块）与档位切换由驱动层消费本模块的 `goal_phase` / `render_goal_block`。
pub use super::goal_delivery::{GoalBudget, GoalDelivery};
use serde::{Deserialize, Serialize};

/// 同一目标的澄清轮次上限（超出由驱动层提示收敛，不再无限追问）。
pub const GOAL_TEXT_TURN_LIMIT: u32 = 8;
/// 停滞软提醒阈值（`stall_streak` 达到此值注入提醒）。
pub const STALL_NUDGE_AT: u32 = 5;
/// 停滞硬停阈值（`stall_streak` 达到此值终止本 run）。
pub const STALL_STOP_AT: u32 = 10;
/// 续跑目标的档位校验文案：host 命令层（`goal_resume_mode_guard`）与 core
/// （`AgentCore::resume_goal`）是同一道门的两处落地，**共用这一份常量**——两套文案必然漂移。
pub const GOAL_RESUME_MODE_REQUIRED: &str =
    "E_GOAL_MODE_REQUIRED: 会话当前不在目标模式，无法续跑目标。请先切回目标模式后再续跑。";

/// 目标生命周期状态（wire 形态 snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// 澄清期：目标/验收标准仍在登记或修订，工作区只读。
    Clarify,
    /// 执行期：完全访问能力下推进批准的交付合同。
    Executing,
    /// 执行期暂停（等用户确认，不再动工作区）。
    Paused,
    /// Cancellation requested; tools and owned processes are still draining.
    Stopping,
    /// Machine checks passed; explicit human acceptance remains outstanding.
    AwaitingAcceptance,
    /// 目标达成（全部验收标准已勾选）。
    Done,
    /// 目标中止（用户放弃或转向）。
    Aborted,
}

impl GoalStatus {
    /// 中文名（提示块、汇总与错误文案共用）。
    pub fn label(self) -> &'static str {
        match self {
            GoalStatus::Clarify => "澄清中",
            GoalStatus::Executing => "执行中",
            GoalStatus::Paused => "已暂停",
            GoalStatus::Stopping => "正在停止",
            GoalStatus::AwaitingAcceptance => "待你验收",
            GoalStatus::Done => "已完成",
            GoalStatus::Aborted => "已中止",
        }
    }
}

/// 单条验收标准：可判定的标题 + 是否已达成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCriterion {
    /// 标题（非空；执行期锁定不可改）。
    pub title: String,
    /// 是否已达成。
    pub done: bool,
    #[serde(default)]
    pub manual: bool,
    #[serde(default)]
    pub verification: Option<super::goal_delivery::GoalCheck>,
}

/// 历史目标的账本元数据，仅为既有存档保留，不再参与权限判定。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalLedger {
    /// 允许写入/修改的路径（前缀匹配，目录或文件）。
    #[serde(default)]
    pub paths: Vec<String>,
    /// 允许运行的程序（程序名或可执行文件路径）。
    #[serde(default)]
    pub programs: Vec<String>,
}

/// 目标状态（会话级；`SessionRuntime.goal` 的唯一内容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    /// 目标陈述（一句话；执行期锁定）。
    pub text: String,
    /// 验收标准（执行期只允许改 `done`）。
    pub criteria: Vec<GoalCriterion>,
    /// 账本（执行期锁定）。
    pub ledger: GoalLedger,
    /// 生命周期状态。
    pub status: GoalStatus,
    /// 已做出的关键决策（执行期只追加）。
    #[serde(default)]
    pub decisions: Vec<String>,
    /// 当前未完成事项（可替换，完成后移除）。
    #[serde(default)]
    pub pending: Vec<String>,
    /// 阻塞项（无法自行解决，需用户介入）。
    #[serde(default)]
    pub blocked: Vec<String>,
    /// 已进行的执行轮数（驱动层累加）。
    #[serde(default)]
    pub rounds: u32,
    /// 连续无进展轮数（驱动层累加，判定见 `stall_verdict`）。
    #[serde(default)]
    pub stall_streak: u32,
    /// 旧账本拒绝计数，仅保留存档数据，不再参与停止判定。
    #[serde(default)]
    pub ledger_denials: u32,
    #[serde(default)]
    pub delivery: GoalDelivery,
}

/// 目标阶段：用于提示与生命周期；写入工具仅在 Executing 状态开放。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalPhase {
    /// 澄清期：只读调研 + 登记/修订目标。
    Clarify,
    /// 执行期：完全访问能力下推进批准的交付合同。
    Execute,
}

/// 由状态推导阶段（`None` = 目标模式已开启但尚未登记 → 澄清期）。
/// Done/Aborted 归入澄清期：上一个目标已结束，再动工作区必须重新登记新目标。
pub fn goal_phase(state: Option<&GoalState>) -> Option<GoalPhase> {
    match state {
        None => Some(GoalPhase::Clarify),
        Some(s) => match s.status {
            GoalStatus::Clarify => Some(GoalPhase::Clarify),
            GoalStatus::Executing
            | GoalStatus::Paused
            | GoalStatus::Stopping
            | GoalStatus::AwaitingAcceptance => Some(GoalPhase::Execute),
            GoalStatus::Done | GoalStatus::Aborted => Some(GoalPhase::Clarify),
        },
    }
}

/// 是否已登记目标（`Some` 即已登记，与目标内容无关）。
pub fn is_registered(state: Option<&GoalState>) -> bool {
    state.is_some()
}

/// 停滞判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallVerdict {
    /// < 5：继续。
    Continue,
    /// 5..=9：注入提醒。
    Nudge,
    /// >= 10：终止本 run。
    Stop,
}

/// 连续无进展轮数 → 停滞动作（阈值常量见 `STALL_NUDGE_AT` / `STALL_STOP_AT`）。
pub fn stall_verdict(streak: u32) -> StallVerdict {
    if streak >= STALL_STOP_AT {
        StallVerdict::Stop
    } else if streak >= STALL_NUDGE_AT {
        StallVerdict::Nudge
    } else {
        StallVerdict::Continue
    }
}

/// 阶段化系统提示块（每步注入；块内已含目标摘要，模型无需再调工具确认现状）。
pub fn render_goal_block(state: Option<&GoalState>) -> String {
    let Some(s) = state else {
        return "\n<goal-mode>目标澄清期：只读调查用户指定的需求文档、技术方案和现有项目。登记 text、criteria（机器项必须列verification.command与cwd；需人验收的标manual=true）、sources（文档路径）、baseline（已有失败）。补齐异常处理、安全、数据完整性等必要质量要求；澄清冲突并建立需求→任务→验收对应关系。请用户在目标卡显式选择预算或不设上限，再用 ask 批准完整合同（选项 mode=goal）。批准后执行权限与完全访问一致，无程序/路径账本限制。不要为工作区修改逐项索取授权。</goal-mode>".into();
    };
    let rule = match s.status {
        GoalStatus::Clarify => {
            "只读澄清；完善 text/criteria/sources/baseline。预算由用户在界面选择，模型不能代选。一次批准后连续推进，不按阶段重新请求批准。"
        }
        GoalStatus::Executing => {
            "执行权限与完全访问一致。按需求文档连续推进里程碑，使用 plan 管理任务依赖与检查点。实现细节可自主调整并记 decisions；不得删功能、降低验收标准。新增范围先记 blocked 并暂停相关工作，其余继续。失败先诊断、换方案、有限重试。command 的真实结果及 reviewer/code-reviewer 子代理报告会登记为 verification；用 evidence=[{criterion:0,call_id:真实调用id,summary:验收说明}] 绑定每个机器验收项。文件改变后旧证据失效，必须重新验证。独立审查须读取原始需求查遗漏。pending/blocked 是当前未解决项，解决后更新列表清除。完成机器检查后 status=done；有人工验收项会自动进入 awaiting_acceptance，不能用 Mock/占位替代真实接入。预算耗尽或实际阻塞应暂停并报告剩余工作，不得自称完成。"
        }
        GoalStatus::Stopping => "正在停止：不要启动新工作，等待工具和子代理退出。",
        GoalStatus::Paused => "已暂停：可以记录用户补充，但未经显式继续不能执行工作区操作。",
        GoalStatus::AwaitingAcceptance => {
            "机器验收已完成，等待用户人工验收；反馈后沿原目标修复，不自动重开目标。"
        }
        GoalStatus::Done => "目标约定验收已通过。新需求须重新登记和批准，不意味着软件绝对无缺陷。",
        GoalStatus::Aborted => "目标已中止，保留进度和记录；新目标须重新登记。",
    };
    format!(
        "\n<goal-mode>{rule}\n{}\n</goal-mode>",
        render_goal_summary(s)
    )
}

/// 纯文本汇总（计划卡 / 收尾报告 / tool_result 共用；空的分节直接省略）。
pub fn render_goal_summary(state: &GoalState) -> String {
    let mut out = String::new();
    out.push_str(&format!("目标：{}\n", state.text));
    out.push_str(&format!(
        "状态：{}（第 {} 轮）\n",
        state.status.label(),
        state.rounds
    ));
    let done = state.criteria.iter().filter(|c| c.done).count();
    out.push_str(&format!("验收标准（{done}/{}）：\n", state.criteria.len()));
    if state.criteria.is_empty() {
        out.push_str("- （尚未定义验收标准）\n");
    } else {
        for (i, c) in state.criteria.iter().enumerate() {
            let mark = if c.done { "x" } else { " " };
            if let Some(check) = &c.verification {
                out.push_str(&format!(
                    "  验证命令：{}；cwd={}\n",
                    check.command,
                    check.cwd.as_deref().unwrap_or("项目主目录")
                ));
            }
            out.push_str(&format!("- [{mark}] {}. {}\n", i + 1, c.title));
        }
    }
    out.push_str(&format!(
        "执行权限：完全访问；累计用量 {} tokens / {} 秒\n",
        state.delivery.used_tokens,
        state.delivery.elapsed_ms / 1000
    ));
    out.push_str(&format!(
        "预算：{}\n",
        serde_json::to_string(&state.delivery.budget).unwrap_or_default()
    ));
    push_section(&mut out, "需求文档", &state.delivery.sources);
    push_section(&mut out, "已有问题基线", &state.delivery.baseline);
    for v in state.delivery.verifications.iter().rev().take(12) {
        out.push_str(&format!(
            "验证记录 {} [{}] {}\n",
            v.call_id,
            if v.passed { "成功" } else { "失败" },
            v.summary
        ));
    }
    push_section(&mut out, "决策", &state.decisions);
    push_section(&mut out, "待办", &state.pending);
    push_section(&mut out, "阻塞", &state.blocked);
    out.trim_end().to_string()
}

/// 追加一个分节（空列表不输出）。
fn push_section(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("{title}：\n"));
    for it in items {
        out.push_str(&format!("- {it}\n"));
    }
}

pub fn goal_execute_phase(rt: &super::SessionRuntime) -> bool {
    if rt.prefs().approval_mode != crate::core::prefs::ApprovalMode::Goal {
        return false;
    }
    rt.goal_snapshot()
        .is_some_and(|s| s.status == GoalStatus::Executing)
}

/// 子代理共享根目标的授权、预算与证据，独立任务不继承目标。
pub fn goal_gate_rt(
    core: &super::AgentCore,
    rt: &std::sync::Arc<super::SessionRuntime>,
) -> std::sync::Arc<super::SessionRuntime> {
    if rt.is_main_session {
        return rt.clone();
    }
    rt.root_session_id
        .as_deref()
        .and_then(|root| core.session(root))
        .unwrap_or_else(|| rt.clone())
}

pub fn prev_mode_to_record(
    mode_at_open: crate::core::prefs::ApprovalMode,
    switch_target: crate::core::prefs::ApprovalMode,
    prev: Option<crate::core::prefs::ApprovalMode>,
) -> Option<crate::core::prefs::ApprovalMode> {
    use crate::core::prefs::ApprovalMode;
    if switch_target != ApprovalMode::Goal || mode_at_open == ApprovalMode::Goal || prev.is_some() {
        return None;
    }
    Some(mode_at_open)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_delivery_is_not_implicit_unlimited_budget() {
        let state: GoalState =
            serde_json::from_str(r#"{"text":"t","criteria":[],"ledger":{},"status":"clarify"}"#)
                .unwrap();
        assert!(state.delivery.budget_exhausted().is_some());
        assert!(state.delivery.verifications.is_empty());
    }
    #[test]
    fn statuses_round_trip_and_waiting_states_do_not_claim_completion() {
        for status in [
            GoalStatus::Clarify,
            GoalStatus::Executing,
            GoalStatus::Stopping,
            GoalStatus::Paused,
            GoalStatus::AwaitingAcceptance,
            GoalStatus::Done,
            GoalStatus::Aborted,
        ] {
            let text = serde_json::to_string(&status).unwrap();
            assert_eq!(serde_json::from_str::<GoalStatus>(&text).unwrap(), status);
        }
    }
    #[test]
    fn prompt_uses_full_access_and_requires_real_evidence() {
        let mut state: GoalState =
            serde_json::from_str(r#"{"text":"t","criteria":[],"ledger":{},"status":"executing"}"#)
                .unwrap();
        let prompt = render_goal_block(Some(&state));
        assert!(prompt.contains("完全访问"));
        assert!(prompt.contains("真实调用id"));
        assert!(!prompt.contains("只跑账本"));
        state.status = GoalStatus::Paused;
        assert!(render_goal_block(Some(&state)).contains("未经显式继续"));
    }
    #[test]
    fn previous_mode_is_only_recorded_on_entry() {
        use crate::core::prefs::ApprovalMode::*;
        assert_eq!(prev_mode_to_record(Plan, Goal, None), Some(Plan));
        assert_eq!(prev_mode_to_record(Goal, Goal, None), None);
        assert_eq!(prev_mode_to_record(Plan, Goal, Some(FullAccess)), None);
    }
}
