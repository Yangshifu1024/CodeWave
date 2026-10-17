//! subagent 工具（[docs/technical-design](../../../docs/technical-design.md)）：必填 maxSteps 预算与角色；独立 runtime +
//! drive_agent 复用（排除集 + 低预算提示 + 强制汇报）；全局并发 4。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::agent::{DriveParams, SessionRuntime, SubBase};
use crate::core::prefs::ApprovalMode;
use crate::core::wait_targets::{TargetKind, TargetState};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 子代理默认步数预算。
pub const DEFAULT_STEPS: usize = 25;
/// 子代理步数预算上限。
pub const MAX_STEPS: usize = 1000;
/// 全局子代理并发上限。
pub const MAX_CONCURRENT: usize = 4;

/// 子代理收尾形态：稳定字符串，进 `ended` 字段（事件载荷 / 工具出参 / 会话日志）。
///
/// 四象限判据由 [`ended_of`] 单点决定（[docs/subagents](../../../docs/subagents.md)）：
///
/// | 跑满预算 | 带 `<report>` | ended | 语义 |
/// |---|---|---|---|
/// | 否 | 是 | [`Report`](Self::Report) | 主动完成后按约定汇报 |
/// | 是 | 是 | [`Partial`](Self::Partial) | **撞上限才交汇报**：拿到了成果但未必做完 |
/// | 是 | 否 | [`Budget`](Self::Budget) | 撞上限且未交汇报 |
/// | 否 | 否 | [`NoReport`](Self::NoReport) | 提前退出且未交汇报 |
///
/// `Partial` 是本枚举存在的唯一理由：`force_report` 会在撞顶前一步逼出一份汇报
/// （预算耗尽前强制汇报一轮），而旧判据「有没有 `<report>` 标签」把它与真正做完的汇报
/// 混为一谈——主代理因此不知道自己被砍断过，只能读报告正文推断缺口再派一次
/// （探路与实现成本付两遍）。
///
/// 注意：值名是**线协议**（前端 `run.types.ts` / `ipc/types.ts` 两处联合类型 + i18n 文案
/// 依赖），改名即破坏兼容，必须同步前端。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubEnded {
    /// 主动完成后按约定汇报（`ended = "report"`）
    Report,
    /// 跑满步数预算才收尾、但交了汇报：成果在手，未完成清单见报告正文（`ended = "partial"`）
    Partial,
    /// 跑满步数预算且未交汇报（`ended = "budget"`）
    Budget,
    /// 未跑满就结束、且未交汇报（`ended = "no_report"`）
    NoReport,
}

impl SubEnded {
    /// 稳定字符串形态（wire 值）。
    pub fn as_str(self) -> &'static str {
        match self {
            SubEnded::Report => "report",
            SubEnded::Partial => "partial",
            SubEnded::Budget => "budget",
            SubEnded::NoReport => "no_report",
        }
    }

    /// 是否算「按约定交出了成果」。
    ///
    /// 仅 [`Report`](Self::Report) 为真——`Partial` 已交汇报但**未必做完**（撞顶逼出来的），
    /// 不能与真收尾同等对待。此谓词是 `analysis_done` 置位（plan 档 G2 分析闸）
    /// 与文案分档的共同单一事实源。
    pub fn is_delivered(self) -> bool {
        matches!(self, SubEnded::Report)
    }

    /// 等待目标是否取得成果：完整汇报和预算耗尽时的部分汇报都算成功。
    /// 与分析批准门刻意分开，partial 保住成果但不能解锁不完整分析。
    pub fn wait_state(self) -> TargetState {
        match self {
            SubEnded::Report | SubEnded::Partial => TargetState::Succeeded,
            SubEnded::Budget | SubEnded::NoReport => TargetState::Failed,
        }
    }
}

/// 子代理终态：把 `submit_delivery` 收到的草稿落成不可变交付记录。
///
/// 返回 `(delivery_id, 摘要)`；未绑定契约或无草稿时返回 `None`——**不是**错误：
/// 旧任务与问答 / 机械修改本来就没有结构化交付，强行落一份空记录只会制造噪音。
/// 落盘失败同样返回 `None`，摘要里说明原因，让上层显示为未验证而非完成。
fn finalize_delivery(
    ctx: &ToolCtx,
    sub_rt: &std::sync::Arc<crate::core::agent::SessionRuntime>,
    sub_id: &str,
    ended: SubEnded,
) -> Option<(String, String, bool)> {
    let task_id = sub_rt.delivery_task_id.lock().unwrap().clone()?;
    let draft = sub_rt.delivery_draft.lock().unwrap().take()?;
    let svc = ctx.core.delivery.clone();
    let identity = crate::core::delivery::DeliveryService::identity_of(sub_rt);
    let Some(mut contract) = svc.read_contract(&identity.session_id, &task_id) else {
        return Some((
            String::new(),
            format!("legacy_unverified：契约 {task_id} 已不在索引内，结构化交付未落盘"),
            false,
        ));
    };
    let assigned = sub_rt.delivery_requirement_ids.lock().unwrap().clone();
    if !assigned.is_empty() {
        contract.requirements.retain(|r| assigned.contains(&r.id));
        if let Some(checks) = sub_rt.delivery_check_ids.lock().unwrap().as_ref() {
            for req in &mut contract.requirements {
                req.check_ids.retain(|id| checks.contains(id));
            }
        }
        let checks: std::collections::BTreeSet<_> = contract
            .requirements
            .iter()
            .flat_map(|r| r.check_ids.iter().cloned())
            .collect();
        let artifacts: std::collections::BTreeSet<_> = contract
            .requirements
            .iter()
            .flat_map(|r| r.artifact_ids.iter().cloned())
            .collect();
        contract.checks.retain(|c| checks.contains(&c.id));
        contract.artifacts.retain(|a| artifacts.contains(&a.id));
    }
    // 草稿声明的修订必须仍是当前修订：期间修订升级过则旧草稿不适用。
    if draft.contract_revision != contract.revision {
        return Some((
            String::new(),
            format!(
                "stale_draft：草稿基于修订 {}，当前修订 {}，请重新提交",
                draft.contract_revision, contract.revision
            ),
            false,
        ));
    }
    let ended_kind = match ended {
        SubEnded::Report => crate::core::delivery::types::EndedKind::Report,
        SubEnded::Partial => crate::core::delivery::types::EndedKind::Partial,
        SubEnded::Budget => crate::core::delivery::types::EndedKind::Budget,
        SubEnded::NoReport => crate::core::delivery::types::EndedKind::NoReport,
    };
    match svc.submit_delivery(
        &identity,
        &contract,
        ended_kind,
        draft.completed,
        draft.incomplete,
        draft.changes,
        draft.evidence,
        draft.blockers,
        &format!("dlv_{sub_id}"),
    ) {
        Ok((result, _receipt, state)) => {
            // [#18] 落盘后立刻跑一次宿主校验，把结论交给父任务。
            // `SubEnded` 四象限与校验结论**并列**：partial 仍是 partial，只是额外说明
            // "为什么它不能被当作完整交付"。
            let ledger = svc.ledger(&identity.session_id);
            let roots = crate::core::delivery::DeliveryService::roots_from_runtime(sub_rt);
            let validation = crate::core::delivery::validation::validate_delivery(
                &contract,
                &result,
                &ledger,
                &roots,
                &crate::core::delivery::version::CaptureOptions::default(),
            );
            let summary = format!(
                "{state:?}/{ended_kind:?}/{}: completed {} 项, incomplete {} 项, blockers {} 项",
                validation.verdict.as_str(),
                result.completed.len(),
                result.incomplete.len(),
                result.blockers.len()
            );
            let scope = crate::core::delivery::VerificationScope {
                inputs: contract
                    .checks
                    .iter()
                    .flat_map(|c| {
                        c.scope
                            .inputs
                            .iter()
                            .chain(&c.scope.outputs)
                            .chain(&c.scope.dependency_files)
                    })
                    .cloned()
                    .chain(contract.artifacts.iter().map(|a| a.path.clone()))
                    .collect(),
                ..Default::default()
            };
            let target =
                crate::core::delivery::version::capture_scope(&roots, &scope, &Default::default())
                    .ok()
                    .filter(|v| v.is_valid());
            let accepted = validation.accepted()
                && target.is_some()
                && sub_rt.save_clean.load(Ordering::SeqCst);
            let operation = format!("sub_validate_{sub_id}");
            let marker = crate::core::delivery::BuiltinValidationRecord {
                binding: Some(crate::core::delivery::types::ContractBinding::of(&contract)),
                id: crate::core::delivery::store::host_id(&operation, "validation"),
                owner: identity.clone(),
                validator_id: "sub_delivery".into(),
                validator_version: "1".into(),
                check_id: None,
                input_refs: vec![result.id.clone()],
                scope_version: target.map(|v| v.digest),
                started_at: result.submitted_at.clone(),
                finished_at: crate::core::delivery::store::now_iso(),
                outcome: if accepted {
                    crate::core::delivery::ValidationOutcome::Passed
                } else {
                    crate::core::delivery::ValidationOutcome::Failed
                },
                findings: validation
                    .reasons
                    .iter()
                    .map(|r| crate::core::delivery::ValidationFinding {
                        severity: crate::core::delivery::types::FindingSeverity::Error,
                        location: r.item_id.clone(),
                        message: r.message.clone(),
                    })
                    .collect(),
                output_ref: None,
            };
            if let Err(error) = svc.record_validation(&identity, marker, &operation) {
                return Some((
                    result.id,
                    format!("{summary}；宿主校验记录未保存：{error}"),
                    false,
                ));
            }
            Some((result.id, summary, accepted))
        }
        Err(e) => Some((
            String::new(),
            format!("legacy_unverified：交付草稿未落盘（{}）", e.code()),
            false,
        )),
    }
}

/// 子代理收尾形态判定（[docs/subagents](../../../docs/subagents.md)）。
///
/// 抽成纯函数而非内联三元：四象限是**跨模块契约**（事件载荷 + 前端徽标 + 分析闸），
/// 内联写法难以写单测，而本判定此前零 Rust 覆盖——正是「半成品伪装成成功」
/// 能长期存在的原因。
///
/// - `tagged`：报告带 `<report>` 标记（`split_report` 的第二个返回值）
/// - `steps_used` / `max_steps`：已启动步数 / 预算（`step_count` 是「已启动步数」口径，
///   每步开头写 `step + 1`，故真正跑完预算时恰等于 `max_steps`）
pub fn ended_of(steps_used: usize, max_steps: usize, tagged: bool) -> SubEnded {
    // 撞顶判定用 >=：steps_used 是已启动步数，撞顶时恰等于 max_steps（不小于）。
    let exhausted = steps_used >= max_steps;
    match (tagged, exhausted) {
        (true, false) => SubEnded::Report,
        // 撞顶才交汇报 —— 与 Report 的唯一差别就在这里，前端据此显橙警示而非绿勾。
        (true, true) => SubEnded::Partial,
        (false, true) => SubEnded::Budget,
        (false, false) => SubEnded::NoReport,
    }
}

/// 当前活跃子代理计数（并发上限依据）。
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// 并发满员时的有界等待时长（轮询间隔 500ms；cancel 触发立即放弃）。
const BUSY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// 尝试抢占一个子代理并发槽位：CAS 快速路径 + 满员时每 500ms 轮询直至成功、
/// 超过 `max_wait` 或 `cancel` 触发。返回 false = 未获得槽位（调用方报 E_SUBAGENT_BUSY）。
/// 抽成带时长参数的函数便于单测；CAS 循环保证检查与自增原子——并发 spawn 下
/// 普通 load/check/fetch_add 可能瞬时超限。
async fn acquire_slot(
    cancel: &tokio_util::sync::CancellationToken,
    max_wait: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        let mut active = ACTIVE.load(Ordering::SeqCst);
        loop {
            if active >= MAX_CONCURRENT {
                break;
            }
            match ACTIVE.compare_exchange(active, active + 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return true,
                Err(actual) => active = actual,
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            _ = cancel.cancelled() => return false,
        }
    }
}

/// subagent 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 自包含的任务描述（子代理看不到主会话上下文，除非 cleanContext=false）。
    task: String,
    /// 角色名（canonical 角色或自由字符串）。
    role: String,
    /// 步数预算，默认 25、上限 1000。alias 兼容 snake_case 键名笔误（max_steps）。
    #[serde(alias = "max_steps")]
    max_steps: Option<usize>,
    /// 前端卡片的短标签（缺省用角色名）。
    #[serde(default)]
    description: Option<String>,
    /// 是否使用干净上下文（默认 true；false 时携带主会话最后 6 条消息作背景）。
    /// alias 兼容 snake_case 键名笔误（clean_context）。
    #[serde(default, alias = "clean_context")]
    clean_context: Option<bool>,
    /// 绑定已有交付契约的任务 id（[#17](../../../../docs/tools-and-safety.md)）。
    ///
    /// 模型只能**指名**，不能新建：宿主会核验该契约存在于本会话且属本 root_run，
    /// 核验不过就当作未绑定（结构化交付降级为文本收尾，不报错打断委派）。
    #[serde(default, alias = "contract_task_id")]
    contract_task_id: Option<String>,
    /// 已登记审查分派的 id（[#10](../../../../docs/tools-and-safety.md)）。
    ///
    /// **只是指名**：宿主从交付边车取真实分派，核验实现者集合与身份后才给审查职责。
    /// 核验不过按普通子代理处理（不报错打断委派），并在日志里留痕——
    /// 模型自称「我是审查者」不产生任何只读上限之外的额外权限。
    #[serde(default, alias = "review_assignment_id")]
    review_assignment_id: Option<String>,
    /// 已登记契约 id（验证职责）：核验通过后本子任务按**干净上下文**构建
    /// （[#11](../../../../docs/tools-and-safety.md)：锁 clean_context=true）。
    #[serde(default, alias = "verification_task_id")]
    verification_task_id: Option<String>,
    /// 声明写范围（[#2](../../../../docs/plan-and-approval.md) 并行准入）：`["ws:src/a.rs"]`。
    ///
    /// 声明了就必须过宿主准入才允许 spawn；**声明本身不授予任何权限**，也不等于
    /// 「只写这些」——写能力不可归因（shell / MCP）的单元一律被准入拒绝并串行。
    /// 不声明则沿用既有行为（不占工作流租约）。
    #[serde(default, alias = "write_scopes")]
    write_scopes: Vec<String>,
    #[serde(default)]
    requirement_ids: Vec<String>,
    #[serde(default)]
    structured_writes_only: Option<bool>,
    #[serde(default)]
    unit_id: Option<String>,
}

/// subagent 工具：把自包含任务委派给带独立步数预算的子代理。
/// 入参为 task / role / maxSteps（必填）+ description / cleanContext；Meta 分级。
/// 独立 runtime + drive_agent 复用：排除交互类工具、步数耗尽强制汇报；全局并发上限 4（超限快速失败）。
/// 权限继承父会话（工具排除集 / MCP 排除 / Plan 档提示合并），杜绝「借子代理绕过 plan 档」；
/// canonical 角色注入完整职业定义，未知角色按通用子代理运行。
pub struct SubagentTool;

/// 从子代理历史尾部提取 run 摘录：最后一个工具名 + 最后一个 text/thinking 块的尾部片段（220 字符）。
/// 供前端子代理卡实时展示内部进度（800ms 采样，非逐字流）。
fn summarize_sub_tail(history: &[crate::core::types::Message]) -> (Option<String>, String) {
    let mut last_tool: Option<String> = None;
    let mut snippet = String::new();
    for m in history.iter().rev() {
        if last_tool.is_some() && !snippet.is_empty() {
            break;
        }
        for c in m.content.iter().rev() {
            match c {
                crate::core::types::Content::ToolUse { name, .. } if last_tool.is_none() => {
                    last_tool = Some(name.clone());
                }
                crate::core::types::Content::Text { text }
                | crate::core::types::Content::Thinking { text }
                    if snippet.is_empty() =>
                {
                    let chars: Vec<char> = text.chars().collect();
                    let start = chars.len().saturating_sub(220);
                    snippet = chars[start..].iter().collect();
                }
                _ => {}
            }
        }
    }
    (last_tool, snippet)
}

/// 子代理基座排除集（B1）：spawn 时冻结的内部 7 项（交互类 / 元工具），与档位无关。
/// 父档派生（Plan 档的写工具 / 后台服务 / 计划任务）与角色派生（只读角色的写工具）
/// 在 `subagent_drive_params` 重建时另加。
pub(crate) const SUB_BASE_EXCLUDES: &[&str] = &[
    "ask",
    "subagent",
    "workflow",
    "plan",
    "skill",
    "scheduled_task",
    "suggest",
    "wait",
];

/// 组装子代理的 system_extra：公共纪律 + 注册表命中时的完整角色定义（<agent-definition>）。
/// 抽成纯函数便于单测；纪律块在前，保证追加角色正文不破坏既有前缀语义。
/// 纪律块展示规范角色名（命中时），避免「PM」之类别名造成展示歧义。
///
/// `mode` 只影响只读角色的两处文案（B3）：FullAccess 档下写工具已解锁，纪律块的提示句改为授权说明，
/// 角色定义正文末尾另追加覆盖声明——否则仍会剩「提示说没有写工具 / 正文说不要写、事实上写得动」的自相矛盾。
pub(crate) fn build_system_extra(
    role: &str,
    max_steps: usize,
    def: Option<&crate::agents::AgentDef>,
    mode: ApprovalMode,
) -> String {
    let display_role = def.map(|d| d.name).unwrap_or(role);
    // 只读角色（explore/reviewer/code-reviewer）补一句可执行约束：写工具已在工具层被排除
    //（见 readonly_extra_excludes），这里让子代理知道自己写不了文件，把发现写进汇报，
    // 免得它反复试探被拒而白烧步数（[docs/subagents]）。
    // FullAccess 档例外（B3）：写工具不再被排除，提示改为授权说明。
    let readonly_notice = if def.is_some_and(|d| d.readonly) {
        if mode == ApprovalMode::FullAccess {
            "角色定位是只读调研，但当前会话为完全访问档，写工具（edit/create/delete）已解锁：确有必要时可以直接写文件。"
        } else {
            "你是只读角色，没有写工具（edit/create/delete 不可用）：不要尝试写文件，把发现写进最终汇报。"
        }
    } else {
        ""
    };
    let mut s = format!(
        "\n<subagent-discipline>你是子代理（角色：{}）。纪律：不得向用户提问（无 ask 工具）；不得派生子代理；不得写全局记忆；步数预算 {} 步，耗尽前必须输出最终汇报（已完成/未完成/结论），且最终汇报必须用 <report>…</report> 包裹——过程旁白（如「接下来我来改 X」）不会被当作汇报；只输出文字而不发起工具调用的回合会被视为未完成并提示你继续（[docs/subagents]）。写操作遇 E_FILE_CLAIMED = 文件已被兄弟任务认领（严格文件隔离）：不得重试或等待，剔除该文件并在汇报「未完成」中列明，由主代理统一处理。{}</subagent-discipline>",
        display_role, max_steps, readonly_notice
    );
    if let Some(d) = def {
        // 只读角色的定义正文自身带只读禁令（explore「只读调研：不修改任何文件」、
        // reviewer「只审查不修改代码」），与上面的授权说明直接打架 → FullAccess 档下在正文末尾
        // 追加覆盖声明显式作废它；其余三档正文逐字不变（回归保护，测试钉死）。
        let override_note = if d.readonly && mode == ApprovalMode::FullAccess {
            "\n\n注意：当前会话为完全访问档，上述角色定义中的只读约束（不修改任何文件、不执行写操作）暂停，你可以直接写文件。"
        } else {
            ""
        };
        s.push_str(&format!(
            "\n<agent-definition name=\"{}\">\n{}{}\n</agent-definition>",
            d.name, d.body, override_note
        ));
    }
    s
}

/// 由宿主核验子任务的职责（[#9](../../../../docs/subagents.md) / [#10](../../../../docs/tools-and-safety.md)）。
///
/// 三条纪律：
/// - 模型给的 id 只是**指名**，真实分派从交付边车索引取；取不到或核验不过 → `Implement`
///   （即"按普通子代理跑"），不静默给审查者只读上限之外的任何优待；
/// - 审查职责要求审查者**不在**该分派的实现者集合内（自审不是独立审查）；
/// - 验证职责要求契约存在且属本 root_run。
pub(crate) fn resolve_duty(
    ctx: &ToolCtx,
    args: &Args,
) -> crate::core::delivery::responsibility::DutyKind {
    use crate::core::delivery::responsibility::DutyKind;
    let owner = crate::core::delivery::DeliveryService::identity_of(&ctx.rt);
    let svc = ctx.core.delivery.clone();

    // 审查与验证互斥：一次派发不能既是审查者又是验证者（分权的基本前提）。
    let want_review = args.review_assignment_id.is_some();
    let want_verify = args.verification_task_id.is_some();
    if want_review && want_verify {
        tracing::info!(
            "subagent 同时声明 reviewAssignmentId 与 verificationTaskId，按普通子代理处理"
        );
        return DutyKind::Implement;
    }
    if want_review {
        // **只有已登记的分派才算审查职责**。模型可以编任意 id；取不到就按普通子代理跑。
        let id = args.review_assignment_id.as_deref().unwrap_or_default();
        if svc.read_review_assignment(&owner.session_id, id).is_some() {
            return DutyKind::Review;
        }
        tracing::info!(
            "subagent 的 reviewAssignmentId `{id}` 不是宿主已登记的分派，按普通子代理处理"
        );
        return DutyKind::Implement;
    }
    if want_verify {
        if let Some(task_id) = &args.verification_task_id
            && svc
                .read_contract(&owner.session_id, task_id)
                .is_some_and(|c| c.owner.root_run_id == owner.root_run_id)
        {
            return DutyKind::Verify;
        }
        tracing::info!("subagent 的 verificationTaskId 未通过核验，按普通子代理处理");
    }
    DutyKind::Implement
}

/// 角色纪律块的按档重建（B1 重建路径消费）：档位只影响只读提示句（B3）。
pub(crate) fn base_extra(role: &str, max_steps: usize, mode: ApprovalMode) -> String {
    build_system_extra(role, max_steps, crate::agents::find(role), mode)
}

/// 子代理档位基座（B1）的构造：spawn 时冻结一次，之后每步按父会话实时档位重建。
/// `root_session_id` 由调用点回填（子 rt 建好后才拿到）。
pub(crate) fn sub_base(
    role: &str,
    max_steps: usize,
    def: Option<&crate::agents::AgentDef>,
    spawn_mode: ApprovalMode,
    duty: crate::core::delivery::responsibility::DutyKind,
) -> SubBase {
    SubBase {
        root_session_id: None,
        role: role.to_string(),
        max_steps,
        spawn_mode,
        base_excludes: SUB_BASE_EXCLUDES
            .iter()
            .map(|s| (*s).to_string())
            .chain(match duty {
                crate::core::delivery::responsibility::DutyKind::Review => {
                    vec![
                        "command".into(),
                        "service".into(),
                        "http_request".into(),
                        "mcp:*".into(),
                    ]
                }
                crate::core::delivery::responsibility::DutyKind::Verify => {
                    vec!["service".into(), "http_request".into(), "mcp:*".into()]
                }
                _ => vec![],
            })
            .collect(),
        base_system_extra: build_system_extra(role, max_steps, def, spawn_mode),
        idle_policy: idle_policy_for(role),
        duty,
    }
}

/// 只读角色判定：注册表 `readonly` 标记的单一事实源（explore/reviewer/code-reviewer = true）。
/// 与 `is_analysis_role` 同风格：走 `crate::agents::find`，别名与大小写归一同源——
/// 谓词只此一份，避免「改一处漏一处」。
fn is_readonly_role(role: &str) -> bool {
    crate::agents::find(role).is_some_and(|d| d.readonly)
}

/// 空转看门狗策略（[docs/subagents]）：只读角色 → `NudgeOnly`
///（空转层 16 步纠偏一次、不硬终止——只读调研天然是「大段只读步骤 + 偶尔产出」；
/// 失败重复层与步数/汇报门不受影响，照常终止）；
/// 其余（含未命中角色、空串、归一后未命中者）→ `Stop`（8 步纠偏 / 14 步终止，语义不变）。
fn idle_policy_for(role: &str) -> crate::core::agent::IdlePolicy {
    if is_readonly_role(role) {
        crate::core::agent::IdlePolicy::NudgeOnly
    } else {
        crate::core::agent::IdlePolicy::Stop
    }
}

/// 只读角色的额外工具排除集（写工具三件套 = 与 plan 档排除共用的
/// `crate::core::agent::WRITE_TOOLS`）；非只读角色为空。
/// 消费既有排除通路：暴露前过滤（stream.rs 按名过滤）+ 调用时硬拒（E_TOOL_BLOCKED）。
/// **FullAccess 档例外（B3）**：该档下只读角色不再排除写工具（父档已完全放行写入，
/// 再锁子代理会与「完全访问」的语义矛盾）；`idle_policy` 不随之变（与档位解耦）。
fn readonly_extra_excludes(role: &str, mode: ApprovalMode) -> Vec<&'static str> {
    if is_readonly_role(role) && mode != ApprovalMode::FullAccess {
        crate::core::agent::WRITE_TOOLS.to_vec()
    } else {
        Vec::new()
    }
}

/// 子代理角色策略装配（[docs/subagents]）：一步到位置 `idle_policy`
/// 并追加只读写工具排除（按档位：FullAccess 下不排除，B3）——独立成函数以让调用点可被单测断言
///（后人重排参数构造时不致静默回归）。
/// 对已有排除项去重，重复调用幂等（与父档位合并集同存一份，重复项本就无害）。
pub(crate) fn apply_role_policy(params: &mut DriveParams, role: &str, mode: ApprovalMode) {
    params.idle_policy = idle_policy_for(role);
    for t in readonly_extra_excludes(role, mode) {
        if !params.exclude_tools.iter().any(|e| e == t) {
            params.exclude_tools.push(t.to_string());
        }
    }
}

/// G2/arch 批准闸的分析角色检查：按注册表规范名校验（别名 test→tester、pm→product-manager
/// 与角色注入共用 `find()` 单一事实源；评审 Y1：防止 role="test" 注入 tester 正文却不置分析标记）。
fn is_analysis_role(role: &str) -> bool {
    matches!(
        crate::agents::find(role).map(|d| d.name),
        Some("product-manager") | Some("tester")
    )
}

#[async_trait::async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &'static str {
        "subagent"
    }
    fn description(&self) -> &'static str {
        "把自包含任务委派给拥有独立步数预算的子代理。必填：task（完全自包含的任务描述）、role、maxSteps（默认 25，上限 1000）。优先使用规范角色：explore（只读代码调研）、backend-dev（后端实现：API/数据层/并发）、frontend-dev（前端实现：组件/状态/类型/样式）、app-dev（App 实现：移动 iOS/Android + 桌面 Electron/Tauri）、reviewer（方案对齐审查）、code-reviewer（代码质量审查）、product-manager（需求分析）、tester（测试执行/报告）——规范角色会注入完整的职业定义到子代理；未知角色按通用子代理运行。子代理不能向你或用户提问，也不能派生子代理；它会返回最终汇报。并行子代理实行严格文件隔离（写认领制）：并行任务包的文件范围必须互斥，跨包共享的文件由你亲自修改；冲突报 E_FILE_CLAIMED。子代理被用户手动停止时返回 E_SUBAGENT_STOPPED：用 ask 向用户确认是否重派，不要自动重启。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["task", "role", "maxSteps"],
  "properties": {
    "task": {"type": "string", "description": "自包含的任务描述"},
    "role": {"type": "string", "description": "规范角色：explore | backend-dev | frontend-dev | app-dev | reviewer | code-reviewer | product-manager | tester"},
    "maxSteps": {"type": "integer", "minimum": 1, "maximum": 1000},
    "description": {"type": "string", "description": "卡片短标签"},
    "cleanContext": {"type": "boolean", "description": "默认 true"},
    "contractTaskId": {"type":"string"},
    "reviewAssignmentId": {"type":"string"},
    "verificationTaskId": {"type":"string"},
    "writeScopes": {"type":"array","items":{"type":"string"},"description":"工作单元声明写范围，格式 workspace:relative/path；未声明写范围时按独占串行运行"},
    "unitId":{"type":"string","description":"workflow plan_units 已冻结的工作单元id；宿主强制其依赖/需求/范围"},
    "structuredWritesOnly": {"type":"boolean","description":"声明writeScopes时默认true，宿主实际禁用command/service/MCP以安全并行；false时独占串行"},
    "requirementIds": {"type":"array","items":{"type":"string"},"description":"绑定契约内分配给此子任务的需求；缺省为全部"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        // 在等待并发槽位之前固化所属主 run，迟到轮询不能误归到下一轮同名子代理。
        let parent_run_id = ctx.rt.run_snapshot().run_id;
        let mut args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!(
                        "参数解析失败：{e}。入参要求：task/role 非空字符串、maxSteps 1–1000 整数、键名 camelCase（maxSteps/cleanContext）。此错误不计失败：请修正参数后立即重发。"
                    ),
                );
            }
        };
        let max_steps = args.max_steps.unwrap_or(DEFAULT_STEPS).clamp(1, MAX_STEPS);
        if args.task.trim().is_empty() || args.role.trim().is_empty() {
            return ToolOutcome::err(
                "E_ARGS",
                "task 与 role 均不能为空。此错误不计失败：请修正参数后立即重发。",
            );
        }
        // title 是内部自动命名角色（[docs/sessions-and-projects](../../../docs/sessions-and-projects.md)）：注册表中存在但不可被主代理委派，
        // 把 agents/mod.rs 头注释中的软约定变成硬约束
        if crate::agents::find(&args.role).map(|d| d.name) == Some("title") {
            return ToolOutcome::err(
                "E_ARGS",
                "title 为内部角色（会话自动命名），不可经 subagent 委派",
            );
        }
        // 注册表命中 → 注入完整角色定义（[docs/plan-and-approval](../../../docs/plan-and-approval.md)）；未命中保持自由字符串旧行为
        let agent_def = crate::agents::find(&args.role);
        // 父档位快照（spawn 时）：基座按它构造，之后每步由 drive 层按**实时**父档重建（B1）
        let parent_prefs = ctx.rt.prefs();

        // 并发上限 4：先快速 CAS 抢位；满员则有界等待（≈30s×500ms 轮询，cancel 可中断）
        // 再快速失败——替代旧「立即 E_SUBAGENT_BUSY」白烧一步的行为（重试硬化批次）
        if !acquire_slot(&ctx.cancel, BUSY_WAIT).await {
            return ToolOutcome::err(
                "E_SUBAGENT_BUSY",
                format!(
                    "子代理并发已达上限（{MAX_CONCURRENT}）且等待 {}s 仍满。此失败不计失败：可用 wait 稍候后原样重发本调用。",
                    BUSY_WAIT.as_secs()
                ),
            );
        }
        let _guard = GuardGuard;

        let sub_id = format!("sub_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        // [#10/#11] 职责由**宿主核验过的分派**决定，不由模型自称；必须早于 clean 上下文
        // 与基座构造（两者都消费它）。
        let duty = resolve_duty(ctx, &args);
        let description = args
            .description
            .clone()
            .unwrap_or_else(|| args.role.clone());
        // 独立 runtime（继承 workspace/data_dir/extra_roots）+ 注册（支持 StopSubagent）
        let mut sub_rt = SessionRuntime::new_sub(&ctx.rt, sub_id.clone());
        let _workspace_guard = sub_rt.workspace_guard.lock().unwrap().clone();
        if let Some(sub) = std::sync::Arc::get_mut(&mut sub_rt) {
            // 工具入口已固化身份，等待并发槽位后也沿用同一轮。
            sub.root_run_id = parent_run_id.clone();
        }
        sub_rt.prepare_actor_run(&format!("sub_{sub_id}"));
        // [#17] 绑定交付契约：模型只能指名，宿主核验存在性与归属后才生效。
        // 核验不过就当未绑定——结构化交付降级为文本收尾，不打断委派。
        let candidate_task = args
            .contract_task_id
            .clone()
            .or(args.verification_task_id.clone())
            .or_else(|| ctx.rt.delivery_task_id.lock().unwrap().clone());
        let bound_contract = candidate_task.as_ref().filter(|task_id| {
            let owner = crate::core::delivery::DeliveryService::identity_of(&ctx.rt);
            match ctx.core.delivery.read_contract(&owner.session_id, task_id) {
                Some(c) => c.owner.root_run_id == owner.root_run_id,
                None => false,
            }
        });
        if args.contract_task_id.is_some() && bound_contract.is_none() {
            tracing::info!(
                "subagent {} 的 contractTaskId 未通过核验（不存在或不属于本 root_run），按未绑定处理",
                sub_id
            );
        }
        *sub_rt.delivery_task_id.lock().unwrap() = bound_contract.cloned();

        if candidate_task.is_some() && bound_contract.is_none() {
            return ToolOutcome::err(
                "E_CONTRACT_BINDING",
                "contractTaskId 不存在或不属于当前运行，拒绝降级为无契约任务",
            );
        }
        if args.review_assignment_id.is_some()
            && duty != crate::core::delivery::responsibility::DutyKind::Review
            || args.verification_task_id.is_some()
                && duty != crate::core::delivery::responsibility::DutyKind::Verify
        {
            return ToolOutcome::err(
                "E_DUTY_BINDING",
                "审查/验证职责绑定无效，不允许降级为实现职责",
            );
        }
        if let Some(task) = bound_contract {
            let c = ctx
                .core
                .delivery
                .read_contract(
                    &crate::core::delivery::DeliveryService::identity_of(&ctx.rt).session_id,
                    task,
                )
                .unwrap();
            if args
                .requirement_ids
                .iter()
                .any(|id| c.requirement(id).is_none())
            {
                return ToolOutcome::err(
                    "E_REQUIREMENT_BINDING",
                    "requirementIds 引用了契约外需求",
                );
            }
        } else if !args.requirement_ids.is_empty() {
            return ToolOutcome::err("E_REQUIREMENT_BINDING", "requirementIds 必须绑定契约");
        }
        let mut unit_dependencies = std::collections::BTreeMap::new();
        let mut frozen_unit = None;
        if let Some(unit_id) = &args.unit_id {
            let Some(task) = bound_contract else {
                return ToolOutcome::err("E_WORKFLOW_UNIT", "工作单元必须绑定契约");
            };
            if let Err(e) = crate::core::workflow::service::design_ready(
                &ctx.rt.data_dir,
                &crate::core::delivery::DeliveryService::identity_of(&ctx.rt),
                task,
            ) {
                return ToolOutcome::err("E_WORKFLOW_DESIGN", e);
            }
            let p = match crate::core::workflow::service::read(
                &ctx.rt.data_dir,
                &crate::core::delivery::DeliveryService::identity_of(&ctx.rt),
                task,
            ) {
                Ok(p) => p,
                Err(e) => return ToolOutcome::err("E_WORKFLOW_UNIT", e),
            };
            let Some(unit) = p.units.iter().find(|u| &u.id == unit_id).cloned() else {
                return ToolOutcome::err("E_WORKFLOW_UNIT", "工作单元未在冻结计划内");
            };
            if p.state.node(unit_id).is_none() {
                return ToolOutcome::err(
                    "E_WORKFLOW_UNIT",
                    "先使用 workflow plan_units 冻结派发计划",
                );
            }
            unit_dependencies = p
                .state
                .nodes
                .iter()
                .map(|(id, n)| {
                    (
                        id.clone(),
                        n.state == crate::core::workflow::NodeState::Passed,
                    )
                })
                .collect();
            if unit.role_id.as_ref().is_some_and(|role| {
                crate::agents::normalize_role(role) != crate::agents::normalize_role(&args.role)
            }) {
                return ToolOutcome::err("E_WORKFLOW_ROLE", "请求角色与冻结工作单元不符");
            }
            if unit
                .model_id
                .as_ref()
                .is_some_and(|model| ctx.core.cfg.read().unwrap().find_model(model).is_none())
            {
                return ToolOutcome::err("E_WORKFLOW_MODEL", "工作单元冻结模型已删除");
            }
            let ledger = ctx
                .core
                .delivery
                .ledger(&crate::core::delivery::DeliveryService::identity_of(&ctx.rt).session_id);
            if let Some(contract) = ctx.core.delivery.read_contract(
                &crate::core::delivery::DeliveryService::identity_of(&ctx.rt).session_id,
                task,
            ) {
                let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&ctx.rt);
                for dep in &unit.depends_on {
                    let valid = p.units.iter().find(|u| &u.id == dep).is_some_and(|u| {
                        let mut c = contract.clone();
                        c.requirements.retain(|r| u.requirement_ids.contains(&r.id));
                        for req in &mut c.requirements {
                            req.check_ids.retain(|id| u.checks.contains(id));
                        }
                        crate::core::delivery::acceptance::evaluate(
                            &crate::core::delivery::acceptance::EvalContext {
                                contract: &c,
                                ledger: &ledger,
                                roots: &roots,
                                capture: Default::default(),
                            },
                        )
                        .is_ok_and(|e| {
                            e.overall == crate::core::delivery::AcceptanceStatusKind::Passed
                        })
                    });
                    if !valid {
                        unit_dependencies.insert(dep.clone(), false);
                    }
                }
            }
            args.write_scopes = unit
                .write_scopes
                .iter()
                .map(|p| format!("{}:{}", p.root_id, p.relative_path))
                .collect();
            args.requirement_ids = unit.requirement_ids.clone();
            *sub_rt.delivery_check_ids.lock().unwrap() = Some(unit.checks.clone());
            frozen_unit = Some(unit);
        }
        *sub_rt.delivery_requirement_ids.lock().unwrap() = args.requirement_ids.clone();
        *sub_rt.delivery_review_assignment_id.lock().unwrap() = args.review_assignment_id.clone();
        *sub_rt.delivery_verification_task_id.lock().unwrap() =
            if duty == crate::core::delivery::responsibility::DutyKind::Verify {
                args.verification_task_id.clone()
            } else {
                None
            };
        let verification_message = if duty
            == crate::core::delivery::responsibility::DutyKind::Verify
        {
            let Some(task) = bound_contract else {
                return ToolOutcome::err("E_VERIFY_INPUT", "验证职责缺少契约绑定");
            };
            let c = ctx
                .core
                .delivery
                .read_contract(
                    &crate::core::delivery::DeliveryService::identity_of(&ctx.rt).session_id,
                    task,
                )
                .unwrap();
            let resolver = crate::core::delivery::DeliveryService::roots_from_runtime(&ctx.rt);
            let scope = crate::core::delivery::VerificationScope {
                inputs: c
                    .checks
                    .iter()
                    .flat_map(|c| {
                        c.scope
                            .inputs
                            .iter()
                            .chain(&c.scope.outputs)
                            .chain(&c.scope.dependency_files)
                    })
                    .cloned()
                    .chain(c.artifacts.iter().map(|a| a.path.clone()))
                    .collect(),
                ..Default::default()
            };
            let target = match crate::core::delivery::version::capture_scope(
                &resolver,
                &scope,
                &Default::default(),
            ) {
                Ok(v) if v.is_valid() => v,
                _ => {
                    return ToolOutcome::err(
                        "E_VERIFY_INPUT",
                        "验证目标范围未完整采集，拒绝派发未知版本",
                    );
                }
            };
            let input = crate::core::delivery::responsibility::VerificationInput {
                contract_revision: c.revision,
                check_ids: c.checks.iter().map(|c| c.id.clone()).collect(),
                artifact_refs: c.artifacts.iter().map(|a| a.path.clone()).collect(),
                environment_facts: vec![],
                target_version: Some(target.digest.clone()),
                allowed_roots: resolver
                    .iter()
                    .map(|(id, path)| crate::core::delivery::types::RootIdentity {
                        root_id: id.clone(),
                        absolute: path.to_string_lossy().into_owned(),
                        kind: match id.as_str() {
                            "workspace" => crate::core::delivery::types::RootKind::Workspace,
                            "data_dir" => crate::core::delivery::types::RootKind::DataDir,
                            "project_dir" => crate::core::delivery::types::RootKind::ProjectDir,
                            _ => crate::core::delivery::types::RootKind::Extra,
                        },
                    })
                    .collect(),
            };
            let requirements: Vec<_> = c
                .requirements
                .iter()
                .map(|r| (r.id.clone(), r.title.clone()))
                .collect();
            let specs: Vec<_> = c
                .checks
                .iter()
                .map(|c| (c.id.clone(), serde_json::to_string(c).unwrap_or_default()))
                .collect();
            let notes = vec![
                ("parent_explicit_task".into(), args.task.clone()),
                ("host_target_version".into(), target.digest.clone()),
            ];
            let manifest = crate::core::delivery::responsibility::build_context_manifest(
                &input,
                &requirements,
                &specs,
                &notes,
            );
            let manifest_path = ctx
                .rt
                .data_dir
                .join("sessions")
                .join(format!("{}.workflow", c.owner.session_id))
                .join(format!("{sub_id}.manifest.json"));
            if let Err(e) = crate::core::process_coordination::atomic_write(
                &manifest_path,
                &serde_json::to_vec(&manifest).unwrap_or_default(),
            ) {
                return ToolOutcome::err("E_VERIFY_MANIFEST", format!("验证输入清单未保存: {e}"));
            }
            Some(json!({"verification_input":input,"requirements":requirements,"checks":c.checks,"explicit_notes":notes,"context_manifest":manifest}).to_string())
        } else {
            None
        };
        let workflow_policy = if let Some(task) = bound_contract {
            match crate::core::workflow::service::read(
                &ctx.rt.data_dir,
                &crate::core::delivery::DeliveryService::identity_of(&ctx.rt),
                task,
            ) {
                Ok(p) => p
                    .snapshot
                    .definition
                    .as_ref()
                    .and_then(|d| d.roles.get(&crate::agents::normalize_role(&args.role)))
                    .cloned(),
                Err(e) => return ToolOutcome::err("E_WORKFLOW_SNAPSHOT", e),
            }
        } else {
            None
        };
        let mut workflow_excludes = Vec::<String>::new();
        if let Some(policy) = &workflow_policy {
            if let Some(model) = &policy.model_id
                && ctx.core.cfg.read().unwrap().find_model(model).is_none()
            {
                return ToolOutcome::err("E_WORKFLOW_MODEL", "冻结模型已被删除，不能静默换人");
            }
            if !policy.tools.iter().any(|t| t == "*") {
                workflow_excludes.extend(
                    crate::tools::registry::ToolRegistry::all_tool_names()
                        .into_iter()
                        .filter(|t| !policy.tools.contains(t)),
                );
                workflow_excludes.push("mcp:*".into());
            }
        }
        if let Some(task) = bound_contract {
            let owner = crate::core::delivery::DeliveryService::identity_of(&sub_rt);
            let Some(contract) = ctx.core.delivery.read_contract(&owner.session_id, task) else {
                return ToolOutcome::err("E_DELIVERY_BASELINE", "派发契约丢失");
            };
            let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&sub_rt);
            let writes: Vec<_> = args
                .write_scopes
                .iter()
                .filter_map(|raw| raw.split_once(':'))
                .map(|(root, path)| crate::core::delivery::PathRef {
                    root_id: if root == "ws" {
                        "workspace".into()
                    } else {
                        root.into()
                    },
                    relative_path: path.into(),
                })
                .collect();
            if let Err(error) = ctx
                .core
                .delivery
                .capture_actor_baseline(&owner, &contract, &roots, &writes)
            {
                return ToolOutcome::err("E_DELIVERY_BASELINE", error.to_string());
            }
        }
        // Derive admission from the effective child capability set, including MCP.
        // Unknown capabilities use one exclusive lease; omission never bypasses admission.
        let mut probe_base = sub_base(
            &args.role,
            max_steps,
            agent_def,
            parent_prefs.approval_mode,
            duty,
        );
        let structured_only = args
            .structured_writes_only
            .unwrap_or(!args.write_scopes.is_empty());
        probe_base.base_excludes.extend(workflow_excludes.clone());
        if structured_only {
            probe_base.base_excludes.extend([
                "command".into(),
                "service".into(),
                "http_request".into(),
                "mcp:*".into(),
            ]);
        }
        let probe = crate::core::agent::subagent_drive_params(&probe_base, &parent_prefs);
        let mut tools: Vec<String> = ctx
            .core
            .tools
            .tool_defs()
            .into_iter()
            .filter(|t| !probe.exclude_tools.contains(&t.name))
            .map(|t| t.name)
            .collect();
        if !probe.exclude_mcp {
            tools.extend(
                ctx.core
                    .mcp
                    .tool_snapshot_for(&ctx.rt.id)
                    .await
                    .into_iter()
                    .map(|t| t.function_name),
            );
        }
        let scopes: Vec<_> = args
            .write_scopes
            .iter()
            .map(|s| {
                s.strip_prefix("ws:")
                    .map(|p| format!("workspace:{p}"))
                    .unwrap_or_else(|| s.clone())
            })
            .collect();
        let mut unit = crate::core::workflow::dispatch::spawn_unit(&sub_id, &scopes);
        if args.write_scopes.is_empty() {
            tools.push("command".into());
        }
        unit.requirement_ids = args.requirement_ids.clone();
        let frozen_unit_model = frozen_unit.as_ref().and_then(|u| u.model_id.clone());
        if frozen_unit.is_none() {
            unit.mutable_resources
                .push("host:serial_without_frozen_plan".into());
        }
        if let Some(frozen) = frozen_unit {
            unit.depends_on = frozen.depends_on;
            unit.mutable_resources = frozen.mutable_resources;
        }
        let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&ctx.rt);
        let owner = crate::core::delivery::DeliveryService::identity_of(&ctx.rt);
        let run = parent_run_id
            .clone()
            .unwrap_or_else(|| owner.root_run_id.clone());
        let expected = crate::core::workflow::dispatch::current_generation(&run);
        let mut tool_map = std::collections::BTreeMap::new();
        tool_map.insert(unit.id.clone(), tools);
        let leases = match crate::core::workflow::dispatch::admission(
            &owner,
            &run,
            expected,
            &[(unit, Vec::new())],
            &roots,
            &unit_dependencies,
            &tool_map,
        ) {
            Ok(v) => v,
            Err(denials) => {
                return ToolOutcome::error_with_data(
                    "工作单元准入被拒，等待现有单元清理或调整范围",
                    json!({"admission":"denied", "denials":denials.iter().map(|d|json!({"code":d.code,"message":d.message})).collect::<Vec<_>>()}),
                );
            }
        };
        let workflow_generation = leases.first().map(|l| l.generation).unwrap_or(0);
        let _workflow_leases: Vec<_> = leases
            .into_iter()
            .map(crate::core::workflow::dispatch::LeaseGuard)
            .collect();
        let workflow_node =
            if let (Some(task), Some(unit)) = (bound_contract, args.unit_id.as_ref()) {
                let actor = crate::core::delivery::DeliveryService::identity_of(&sub_rt);
                if let Err(e) = crate::core::workflow::service::unit_transition(
                    &ctx.rt.data_dir,
                    &actor,
                    task,
                    unit,
                    crate::core::workflow::NodeState::Running,
                    workflow_generation,
                ) {
                    return ToolOutcome::err("E_WORKFLOW_STATE", e);
                }
                Some(WorkflowNodeGuard {
                    data_dir: ctx.rt.data_dir.clone(),
                    owner: actor,
                    task: task.clone(),
                    unit: unit.clone(),
                    generation: workflow_generation,
                    finished: false,
                })
            } else {
                None
            };
        let mut workflow_node = workflow_node;

        // task 全量下发：抽屉「分配的任务」要能看到完整任务。此前在此 trunc 2000 字符，实时面板只能
        // 显示「…(truncated)」，而落盘历史存的是全文——恢复会话后反而完整，实时与归档两路不一致。
        // 下方日志行仍按 300 字符截断（那是日志文件，不是 UI 载荷）。
        ctx.core.sink.emit(
            &ctx.rt.id,
            "sub:spawn",
            json!({ "session": ctx.rt.id, "run_id": parent_run_id, "sub_id": sub_id, "role": args.role,
                    "name": agent_def.map(|d| d.name), "task": args.task.clone(),
                    "description": description, "max_steps": max_steps }),
        );
        // [docs/subagents](../../../docs/subagents.md)：子代理绑定父会话通道——子代理流式帧经 TauriSink 包装为
        // Frame::Sub 信封借父通道下发；前端按 sub_id 路由到过程抽屉自己的消息流
        ctx.core.sink.bind_sub_channel(&ctx.rt.id, &sub_id);
        // [docs/sessions-and-projects](../../../docs/sessions-and-projects.md) 会话日志：spawn 记入父会话日志；子代理自身的 LLM/工具轨迹落子日志文件
        crate::core::session_log::info(
            &ctx.rt,
            &format!(
                "子代理 spawn [{sub_id}] role={} steps={max_steps} task={}",
                args.role,
                crate::core::session_log::trunc(&args.task, 300)
            ),
        );

        ctx.core.subs.insert(sub_id.clone(), sub_rt.clone());
        ctx.core
            .activity
            .register_sub(&ctx.rt, &sub_rt, parent_run_id.as_deref());
        // 登记为可等待目标（`wait` 的 until 可等它收尾）：放在 spawn 真正成功之后——
        // 抢占并发槽位失败等早退路径都不会走到这里，不会留下永远等不到的 running 条目。
        ctx.core
            .wait_targets
            .register(sub_id.clone(), TargetKind::Subagent, None);
        // [#11] 验证职责下 clean_context **锁 true**：验证者拿到的必须是显式输入，
        // 不是实现者推理全文。调用方传 false 一律忽略并留痕，不静默照办。
        let clean_context = match duty {
            crate::core::delivery::responsibility::DutyKind::Verify => {
                if args.clean_context == Some(false) {
                    tracing::info!(
                        "subagent [{}] 处于验证职责，cleanContext=false 已被忽略（强制干净上下文）",
                        sub_id
                    );
                }
                true
            }
            _ => args.clean_context != Some(false),
        };
        // clean_context=false 时携带主会话最后 6 条消息作背景
        if !clean_context {
            let tail: Vec<_> = ctx
                .rt
                .history
                .lock()
                .unwrap()
                .iter()
                .rev()
                .take(6)
                .rev()
                .cloned()
                .collect();
            *sub_rt.history.lock().unwrap() = tail;
        }
        sub_rt
            .history
            .lock()
            .unwrap()
            .push(crate::core::types::Message::user_text(format!(
                "<subagent-task role=\"{}\">\n{}\n</subagent-task>",
                args.role,
                verification_message.as_deref().unwrap_or(&args.task)
            )));
        // 目标模式已删除：reviewer/code-reviewer 子代理不再读取目标合同；保留通用评审任务消息。

        // 角色级模型覆盖：父 prefs 已复制入 sub_rt（runtime.rs:377），此处用 effective_subagent_model 重新解析并覆写 model_id。
        // approval_mode / reasoning_effort 保持父继承（沿用现有档位同步语义，不动）。
        // args.role 未统一归一，但以 normalize_role 跳一次保证与 subagent_models 键一致：
        // find() 内部走同一归一，HashMap 键与 DELEGABLE_ROLES 都是 kebab-case，双重归一是安全的。
        {
            let role_key = crate::agents::normalize_role(&args.role);
            // Missing roles inherit naturally; runtime reads never mutate persisted configuration.
            // 读锁读出 effective_subagent_model（纯函数，不嵌套持锁）。
            let resolved = {
                let cfg = ctx.core.cfg.read().unwrap();
                crate::core::prefs::effective_subagent_model(&cfg, &role_key, &parent_prefs)
                    .map(|m| m.id.clone())
            };
            if let Some(id) = resolved
                && let Ok(mut p) = sub_rt.prefs.lock()
            {
                p.model_id = Some(id);
            }
        }

        // 档位基座（B1）：内部排除集 + 角色纪律块 + idle 策略在 spawn 冻结一次；
        // 此后每步由 drive 层按父会话**实时**档位从基座重建（`subagent_drive_params`）——
        // 子代理不再冻结在 spawn 时的档位上。
        let mut base = sub_base(
            &args.role,
            max_steps,
            agent_def,
            parent_prefs.approval_mode,
            duty,
        );
        if let Some(model) = frozen_unit_model
            .as_ref()
            .or_else(|| workflow_policy.as_ref().and_then(|p| p.model_id.as_ref()))
        {
            sub_rt.prefs.lock().unwrap().model_id = Some(model.clone());
        }
        base.base_excludes.extend(workflow_excludes);
        base.root_session_id = sub_rt.root_session_id.clone();
        if structured_only {
            base.base_excludes.extend([
                "command".into(),
                "service".into(),
                "http_request".into(),
                "mcp:*".into(),
            ]);
        }
        // 权限继承（[docs/chat-and-interface](../../../docs/chat-and-interface.md) 收紧）：子代理权限 ⊆ 父会话权限。
        // 父 Plan 档的写排除 / MCP 排除 / plan 档提示由 subagent_drive_params 从基座合并进子参数，
        // 堵住「借子代理绕过 plan 档」的洞；只读角色的策略（含 FullAccess 下解锁写工具，B3）
        // 同样在其中按档位置位。spawn 与每步重算共用这一条装配路径，两处不会漂移。
        let mut params = crate::core::agent::subagent_drive_params(&base, &parent_prefs);
        params.max_steps = max_steps;
        params.budget_notice = true;
        params.emit_events = false;
        params.force_report = true;
        // 子代理：纯文本回合不等于完成（防止过程旁白被当成最终汇报提前退出，
        // [docs/subagents]）
        params.finish_on_text = false;
        // 取消级联：主会话停止（cancel_run）→ 父令牌取消 → 本子代理 child_token 取消，
        // LLM 流与审批等待随之中止（[docs/subagents]）
        params.parent_cancel = Some(ctx.cancel.clone());
        // 档位重算基座：drive 层每步据此按父档重建（None = 不重算）
        params.sub_base = Some(base);

        let core = ctx.core.clone();
        let sink = ctx.core.sink.clone();
        let session = ctx.rt.id.clone();
        let sub2 = sub_id.clone();
        let step_rt = sub_rt.clone();
        let progress_run_id = parent_run_id.clone();
        // 步数进度事件：轮询 history（每 800ms）携带内部 run 摘录（前端子代理卡展开可见）
        let progress = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                let (steps, last_tool, detail) = {
                    let h = step_rt.history.lock().unwrap();
                    let (last_tool, detail) = summarize_sub_tail(&h);
                    // 真实步数（drive 每步 store），不是 history.len()——后者每步增约 2 条消息，
                    // 60 步预算会显示成 120+（前端 120/60 失真缺陷）
                    (
                        step_rt.step_count.load(std::sync::atomic::Ordering::SeqCst),
                        last_tool,
                        detail,
                    )
                };
                // 当前档位（B2）：子 rt 的 prefs 由 drive 层每步按父档同步，故此处即实时值。
                // 载荷只**新增**字段，事件键名与 29 键契约不变（前端据它显示子代理卡档位）。
                let approval_mode = step_rt.prefs().approval_mode;
                sink.emit(&session, "sub:step", json!({ "session": session, "run_id": progress_run_id, "sub_id": sub2, "step": steps, "tool": last_tool, "detail": detail, "approval_mode": approval_mode }));
            }
        });

        // panic unwind 兜底：停掉进度轮询、注销、通知前端收尾子代理卡
        //（此前 panic 会让轮询器永远发 sub:step、前端卡卡在「运行中」）。
        // 正常路径在返回前解除武装——sub:done / sub:error 已由下方分支发出。
        let mut cleanup = SubCleanupGuard {
            core: core.clone(),
            session: ctx.rt.id.clone(),
            parent_run_id: parent_run_id.clone(),
            sub_id: sub_id.clone(),
            sub_rt: sub_rt.clone(),
            progress,
            armed: true,
            // 默认失败：以下三个失败分支（取消 / provider 错误）不覆写就是 Failed，
            // panic 路（armed 仍为 true）也落到 Failed。
            final_state: TargetState::Failed,
            final_text: None,
        };

        let run_id = format!("sub_{}", sub_id);
        let actual_model_id = sub_rt.prefs().model_id.clone().unwrap_or_default();
        let actual_format = core
            .cfg
            .read()
            .unwrap()
            .find_model(&actual_model_id)
            .map(|m| m.api_format);
        let (result, usage, _) =
            crate::core::agent::drive_agent(&core, &sub_rt, params, &run_id).await;

        // [docs/subagents](../../../docs/subagents.md)：子代理完整执行历史落盘（ok/err 两路在此覆盖；panic 路由 SubCleanupGuard 兜底），
        // 过程抽屉在会话恢复后可重建完整消息流
        {
            let h = sub_rt.history.lock().unwrap();
            match ctx.core.store.save_sub_history(&ctx.rt.id, &sub_id, &h) {
                Ok(report) => {
                    // 子代理不是会话、没有 UI 载体：越限/失败只走日志，不写索引也不发事件
                    if !report.is_clean() {
                        sub_rt.save_clean.store(false, Ordering::SeqCst);
                        ctx.rt.save_clean.store(false, Ordering::SeqCst);
                        let line = format!(
                            "子代理 [{sub_id}] 过程历史有损保存：剥离图片 {} 张 / 丢弃轮次 {} 轮",
                            report.stripped_images, report.dropped_rounds
                        );
                        crate::core::session_log::warn(&ctx.rt, &line);
                    }
                }
                Err(e) => {
                    sub_rt.save_clean.store(false, Ordering::SeqCst);
                    ctx.rt.save_clean.store(false, Ordering::SeqCst);
                    crate::core::session_log::warn(
                        &ctx.rt,
                        &format!("子代理 [{sub_id}] 过程历史落盘失败：{e}"),
                    );
                }
            }
        }

        // L10: Attribute all consumed usage to the frozen child model, including failures.
        if usage.input + usage.output + usage.cache_read + usage.cache_write > 0 {
            let model_id = actual_model_id.clone();
            core.stats.record(crate::core::stats::UsageRecord {
                session: ctx.rt.id.clone(),
                model_id,
                workspace: sub_rt.workspace.to_string_lossy().into_owned(),
                input: usage.input,
                output: usage.output,
                cache_read: usage.cache_read,
                cache_write: usage.cache_write,
                runs: 1,
                kind: crate::core::stats::KIND_SUB.into(),
            });
        }
        let input_total = actual_format
            .as_ref()
            .map(|format| usage.prompt_total(format));
        let usage_owner = crate::core::delivery::DeliveryService::identity_of(&sub_rt);
        ctx.core.sink.emit(&ctx.rt.id,"sub:usage",json!({"session":ctx.rt.id,"run_id":parent_run_id,"sub_id":sub_id,"actor_run_id":usage_owner.actor_run_id,"owner":usage_owner,
            "model_id":actual_model_id,"input_total":input_total,"usage":{"input":usage.input,"output":usage.output,"cache_read":usage.cache_read,"cache_write":usage.cache_write,"input_total":input_total}}));
        let actor_cancelled =
            matches!(&result, Err(crate::provider::dto::ProviderError::Cancelled));
        let mut structured_accepted = false;
        let outcome = match result {
            Ok(report) => {
                // <report> 标记只用于收尾判定（drive 层 text_turn_action），不进入汇报正文
                //（[docs/subagents]）
                let (clean_report, tagged) = crate::core::agent::split_report(&report);
                // 收尾形态：四象限单点判定（[docs/subagents](../../../docs/subagents.md)）。
                // 关键修复——`Partial`（撞顶才交汇报）原先与 `Report` 同判 `report`，
                // 让主代理无从判断子代理是否被砍断，只能读报告正文推断缺口再派（成本付两遍）。
                let steps_used = sub_rt.step_count.load(std::sync::atomic::Ordering::SeqCst);
                let ended = ended_of(steps_used, max_steps, tagged);
                let ended_str = ended.as_str();
                crate::core::session_log::info(
                    &ctx.rt,
                    &format!(
                        "子代理 [{sub_id}] 返回（input {}/output {} tokens，步数 {steps_used}/{max_steps}，收尾 {ended_str}）：{}",
                        usage.input,
                        usage.output,
                        crate::core::session_log::trunc(&clean_report, 400)
                    ),
                );
                // G2（[docs/plan-and-approval](../../../docs/plan-and-approval.md)）+ arch 批准闸（[docs/plan-and-approval](../../../docs/plan-and-approval.md)）：pm/tester 分析子代理成功返回 → 置分析产物标记。
                // 跨档置位：标记只在批准执行点（ask 批准闸）消费，其余档位置位无副作用；
                // arch 流程在 ConfirmEach 档发起批准同样依赖该标记。
                //
                // **必须按 `is_delivered()` 过滤**（[docs/subagents](../../../docs/subagents.md)）：
                // `Partial` 是撞顶逼出来的半成品分析，`Budget`/`NoReport` 更是压根没交分析。
                // 旧判据只看「结果是否 Ok」，半成品 pm/tester 报告照样置位 → plan 档在分析
                // 不完整的情况下放行批准闸（跨机制静默提权路径，且此前无任何测试覆盖）。
                if ended.is_delivered() && is_analysis_role(&args.role) {
                    ctx.rt
                        .analysis_done
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                // [#17] 结构化交付落盘：`ended` 在**这一刻**由宿主按真实步数 / 报告标记判定
                // （模型无权自称）。落盘失败如实降级为 `legacy_unverified`，不静默当完成。
                let delivery = finalize_delivery(ctx, &sub_rt, &sub_id, ended);
                structured_accepted = delivery.as_ref().is_some_and(|d| d.2);
                // 最终汇报先进卡 → usage → done（前端在关闭进度条前展示汇报）
                // 载荷只**增**可选字段：事件键名与既有字段一律不动（29 键契约）。
                let mut report_payload = json!({ "session": ctx.rt.id, "run_id": parent_run_id, "sub_id": sub_id, "report": clean_report.clone() });
                if let Some(d) = delivery.as_ref() {
                    report_payload["delivery_id"] = json!(d.0);
                    report_payload["delivery_summary"] = json!(d.1);
                }
                ctx.core.sink.emit(&ctx.rt.id, "sub:report", report_payload);
                let mut done_payload = json!({ "session": ctx.rt.id, "run_id": parent_run_id, "sub_id": sub_id, "usage": usage,
                            "steps_used": steps_used, "ended": ended_str });
                // `delivery_id` / `delivery_summary` 是**可选新增字段**：没绑定契约的旧任务
                // 完全不带这两个键，前端据此继续走旧的纯文本展示。
                if let Some(d) = delivery.as_ref() {
                    done_payload["delivery_id"] = json!(d.0);
                    done_payload["delivery_summary"] = json!(d.1);
                }
                ctx.core.sink.emit(&ctx.rt.id, "sub:done", done_payload);
                // 部分汇报也取得成果，wait 成功与工具成功一致；完整分析批准仍由
                // is_delivered 单独守护。Budget/NoReport 没交汇报，保持失败。
                // report 全文按 2000 字符截断作终态摘要（避免大报告长期占内存）。
                cleanup.final_state = ended.wait_state();
                cleanup.final_text = Some(crate::core::session_log::trunc(&clean_report, 2000));
                ToolOutcome::ok(
                    // sub_id 放首位：它能在 tool_result 头部截断后幸存，
                    // 供会话恢复关联过程历史文件（[docs/subagents](../../../docs/subagents.md)）
                    json!({ "sub_id": sub_id, "role": args.role, "role_resource": agent_def.map(|definition| definition.resource()), "steps_budget": max_steps,
                            "steps_used": steps_used, "ended": ended_str, "report": clean_report,
                            // 结构化交付：delivery_id 与完整性状态（无契约则不带这两个键）
                            "delivery_id": delivery.as_ref().map(|d| d.0.clone()),
                            "delivery_summary": delivery.as_ref().map(|d| d.1.clone()),
                            // tokens 同层输出：恢复路径回填卡片 token 数的唯一来源
                            //（[docs/subagents](../../../docs/subagents.md)）
                            "tokens": usage.input + usage.output }),
                )
            }
            Err(crate::provider::dto::ProviderError::Cancelled) => {
                // 两种取消必须区分：父令牌未取消 = 用户在子代理卡单独停止——主代理要即时察觉
                // 并询问是否重派；父令牌已取消 = 随主会话停止级联——主 run 即将终止，不诱导询问
                let user_stopped = !ctx.cancel.is_cancelled();
                let (ui_err, tool_msg) = if user_stopped {
                    (
                        "子代理被用户手动停止".to_string(),
                        "子代理被用户手动停止（E_SUBAGENT_STOPPED）。请先用 ask 询问用户是否重新派发该任务（可附调整后的任务描述）；获得确认前不要自行重启。".to_string(),
                    )
                } else {
                    (
                        "子代理随主会话停止而取消".to_string(),
                        "子代理随主会话停止而取消。".to_string(),
                    )
                };
                crate::core::session_log::warn(
                    &ctx.rt,
                    &format!("子代理 [{sub_id}] 取消：{ui_err}"),
                );
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:error",
                    json!({ "session": ctx.rt.id, "run_id": parent_run_id, "sub_id": sub_id, "error": ui_err, "cancelled": true, "steps_used": sub_rt.step_count.load(Ordering::SeqCst) }),
                );
                ToolOutcome::err("E_SUBAGENT_STOPPED", tool_msg)
            }
            Err(e) => {
                crate::core::session_log::error(&ctx.rt, &format!("子代理 [{sub_id}] 失败：{e}"));
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:error",
                    json!({ "session": ctx.rt.id, "run_id": parent_run_id, "sub_id": sub_id, "error": e.to_string(), "steps_used": sub_rt.step_count.load(Ordering::SeqCst) }),
                );
                ToolOutcome::err("E_SUBAGENT", format!("子代理执行失败：{e}"))
            }
        };
        if let Some(node) = workflow_node.as_mut() {
            let mut passed = false;
            if structured_accepted
                && outcome.ok
                && cleanup.final_state == TargetState::Succeeded
                && let Some(task) = bound_contract
            {
                let owner = crate::core::delivery::DeliveryService::identity_of(&sub_rt);
                if let Some(mut contract) = ctx.core.delivery.read_contract(&owner.session_id, task)
                {
                    contract
                        .requirements
                        .retain(|r| args.requirement_ids.contains(&r.id));
                    if let Some(checks) = sub_rt.delivery_check_ids.lock().unwrap().as_ref() {
                        for req in &mut contract.requirements {
                            req.check_ids.retain(|id| checks.contains(id));
                        }
                    }
                    let ledger = ctx.core.delivery.ledger(&owner.session_id);
                    let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&sub_rt);
                    passed = crate::core::delivery::acceptance::evaluate(
                        &crate::core::delivery::acceptance::EvalContext {
                            contract: &contract,
                            ledger: &ledger,
                            roots: &roots,
                            capture: Default::default(),
                        },
                    )
                    .is_ok_and(|r| {
                        r.overall == crate::core::delivery::AcceptanceStatusKind::Passed
                    });
                }
            }
            if let Err(e) = node.finish(if passed {
                crate::core::workflow::NodeState::Passed
            } else {
                crate::core::workflow::NodeState::Failed
            }) {
                cleanup.armed = false;
                drop(cleanup);
                sub_rt.finish_actor_run(&run_id, true, actor_cancelled);
                return ToolOutcome::err("E_WORKFLOW_STATE", e);
            }
        }
        cleanup.armed = false;
        drop(cleanup); // 立即执行清理（中止已结束的轮询 / 注销均为幂等）
        sub_rt.finish_actor_run(&run_id, !outcome.ok, actor_cancelled);
        outcome
    }
}

/// 子代理异常退出（panic unwind）兜底：停进度轮询、注销；armed 时补发 sub:error 让前端收尾
///（正常路径已发 sub:done/sub:error，先解除武装防重复）；并尽力落盘过程历史。
/// Drop 覆盖三条路径（ok/err/panic）：panic 路在 unwind 中兑现；正常路径显式 drop（L353）提前清理。
struct SubCleanupGuard {
    core: std::sync::Arc<crate::core::agent::AgentCore>,
    session: String,
    parent_run_id: Option<String>,
    sub_id: String,
    sub_rt: std::sync::Arc<crate::core::agent::SessionRuntime>,
    progress: tokio::task::JoinHandle<()>,
    armed: bool,
    /// 可等待目标登记表（`wait` 的 until）置终态：正常完成时由 Ok 分支改写为 Succeeded；
    /// 默认 Failed 覆盖用户停止 / provider 错误 / panic 三类非正常收尾。
    /// 无报告但正常返回（ended = budget / no_report）算失败——它没按约定交汇报。
    final_state: TargetState,
    /// 终态摘要（observed.text）：正常路径传 report，其余传 None。
    /// 按 2000 字符截断（与 session_log 同量级；wait 工具侧还会再限长）。
    final_text: Option<String>,
}
impl Drop for SubCleanupGuard {
    fn drop(&mut self) {
        self.progress.abort();
        // 登记表置终态**必须早于 subs.remove**：成功 / 失败 / 用户停止 / panic 共用本路径，
        // 而 wait 的轮询间隙反查 subs 会分不清「已完成」与「从不存在」。早一步置位，
        // 哪怕 wait 反查时空（条目仍在登记表里且已终态）也不误报。
        let report = std::mem::take(&mut self.final_text);
        self.core
            .wait_targets
            .finish(&self.sub_id, self.final_state, report, None);
        self.core.subs.remove(&self.sub_id);
        // [docs/subagents](../../../docs/subagents.md) 配对注销：防止 subs 注册表无限增长（内存泄漏 + 向陈旧 sub_id 误投递一段窗口）。
        // NoopSink（测试）默认无操作；无通道时零副作用。
        self.core.sink.unbind_sub_channel(&self.sub_id);
        if self.armed {
            // 先收口观察摘要再做历史 IO；panic 路不能因落盘失败留住运行中条目。
            self.core.sink.emit(
                &self.session,
                "sub:error",
                json!({ "session": self.session, "run_id": self.parent_run_id, "sub_id": self.sub_id, "error": "子代理内部异常终止", "steps_used": self.sub_rt.step_count.load(Ordering::SeqCst) }),
            );
            // panic 路同样尽力落盘已产生的过程历史（[docs/subagents](../../../docs/subagents.md)）
            let h = crate::core::agent::lock_ok(&self.sub_rt.history);
            match self
                .core
                .store
                .save_sub_history(&self.session, &self.sub_id, &h)
            {
                Ok(report) if !report.is_clean() => {
                    // 子代理无 UI 载体：有损保存只走日志（不写索引、不发事件）
                    crate::core::session_log::warn(
                        &self.sub_rt,
                        &format!(
                            "子代理 [{}] 过程历史有损保存：剥离图片 {} 张 / 丢弃轮次 {} 轮",
                            self.sub_id, report.stripped_images, report.dropped_rounds
                        ),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    crate::core::session_log::warn(
                        &self.sub_rt,
                        &format!("子代理 [{}] panic 路过程历史落盘失败：{e}", self.sub_id),
                    );
                }
            }
            drop(h);
            if let Some(run_id) = self.sub_rt.run_snapshot().run_id {
                self.sub_rt.finish_actor_run(&run_id, true, false);
            }
        }
    }
}

/// 进程级护栏：所有退出路径都会递减计数。
struct GuardGuard;
impl Drop for GuardGuard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

struct WorkflowNodeGuard {
    data_dir: std::path::PathBuf,
    owner: crate::core::delivery::RunIdentity,
    task: String,
    unit: String,
    generation: u64,
    finished: bool,
}
impl WorkflowNodeGuard {
    fn finish(&mut self, state: crate::core::workflow::NodeState) -> Result<(), String> {
        crate::core::workflow::service::unit_transition(
            &self.data_dir,
            &self.owner,
            &self.task,
            &self.unit,
            state,
            self.generation,
        )?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for WorkflowNodeGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish(crate::core::workflow::NodeState::Blocked);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ConfigState;
    use crate::core::prefs::{SessionPrefs, effective_subagent_model};

    #[test]
    fn budget_constants() {
        assert_eq!(DEFAULT_STEPS, 25);
        assert_eq!(MAX_STEPS, 1000);
        assert_eq!(MAX_CONCURRENT, 4);
    }

    /// 四象限判据的完整矩阵（[docs/subagents](../../../docs/subagents.md)）。
    ///
    /// `Partial` 是本轮新增分支，也是旧判据的漏网之鱼：撞顶 + 交汇报原先被判 `report`
    /// （= 真做完），前端显绿勾、主代理以为拿到了成果。
    #[test]
    fn ended_of_four_quadrants() {
        // 未跑满 + 有汇报 = 真正做完
        assert_eq!(ended_of(30, 60, true), SubEnded::Report);
        assert_eq!(ended_of(0, 60, true), SubEnded::Report);
        // 撞顶 + 有汇报 = 半成品（本轮修复的核心：原先与 Report 同判）
        assert_eq!(ended_of(60, 60, true), SubEnded::Partial);
        // 撞顶 + 无汇报 = 预算耗尽且没交
        assert_eq!(ended_of(60, 60, false), SubEnded::Budget);
        // 未跑满 + 无汇报 = 提前退出
        assert_eq!(ended_of(5, 60, false), SubEnded::NoReport);
    }

    /// `step_count` 是「已启动步数」口径（每步开头写 `step + 1`），故真正跑完预算时
    /// 恰等于 `max_steps` 而非大于它——`>=` 与 `>` 在撞顶边界上行为不同，此处钉死 `>=`。
    #[test]
    fn ended_of_exhaustion_boundary_is_inclusive() {
        // 恰好等于预算 = 撞顶
        assert_eq!(ended_of(60, 60, true), SubEnded::Partial);
        assert_eq!(ended_of(60, 60, false), SubEnded::Budget);
        // 差一步 = 尚未撞顶
        assert_eq!(ended_of(59, 60, true), SubEnded::Report);
        assert_eq!(ended_of(59, 60, false), SubEnded::NoReport);
    }

    /// `is_delivered` 是 `analysis_done` 置位（plan 档 G2 分析闸）的唯一判据：
    /// 只有真做完的 `Report` 才算交出了成果。`Partial` 虽是 Ok 结果，但它是撞顶逼出来的
    /// 半成品分析，放行批准闸 = 静默提权。
    #[test]
    fn only_report_counts_as_delivered() {
        assert!(SubEnded::Report.is_delivered());
        for e in [SubEnded::Partial, SubEnded::Budget, SubEnded::NoReport] {
            assert!(!e.is_delivered(), "{e:?} 不得计为已交付成果");
        }
    }

    /// 真实工具调用跨 provider/drive/清理边界：partial 保住成果，但不解锁分析批准门。
    #[tokio::test]
    async fn partial_report_is_successful_wait_target_without_approving_analysis() {
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
        let port = listener.local_addr().unwrap().port();
        let server_listener = listener.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = server_listener.accept().await.unwrap();
            // 读干请求头和正文，避免 Windows 带未读请求关闭发 RST。
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "请求不能提前断开");
                request.extend_from_slice(&buf[..n]);
                if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..header_end]);
                    let len = header
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + len {
                        break;
                    }
                }
            }
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"<report>已完成 A，B 待补</report>\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            project_dir: None,
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        {
            let mut cfg = core.cfg.write().unwrap();
            cfg.providers[0].base_url = format!("http://127.0.0.1:{port}/v1");
            cfg.providers[0].keys = vec!["test-key".into()];
        }
        let rt = core.get_or_create_session(
            "partial-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        let ctx = ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            provider_call_id: String::new(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            SubagentTool.run(
                &ctx,
                json!({
                    "task": "分析需求并返回已有成果", "role": "product-manager", "maxSteps": 1
                }),
            ),
        )
        .await
        .expect("工具应完成");
        server.await.unwrap();
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["ended"], "partial");
        assert_eq!(out.data["report"], "已完成 A，B 待补");
        assert!(
            !ctx.rt.analysis_done.load(Ordering::SeqCst),
            "partial 不能批准不完整分析"
        );
        let id = out.data["sub_id"].as_str().unwrap();
        let wait = super::super::wait::WaitTool
            .run(
                &ctx,
                json!({"seconds": 1, "until": {"target": id, "state": "exited"}}),
            )
            .await;
        assert_eq!(
            wait.data["outcome"], "condition_met",
            "工具成功与 wait 成功应一致：{wait:?}"
        );
        assert_eq!(wait.data["observed"]["status"], "succeeded");
        let failed = super::super::wait::WaitTool
            .run(
                &ctx,
                json!({"seconds": 1, "until": {"target": id, "state": "failed"}}),
            )
            .await;
        assert_eq!(failed.data["outcome"], "timeout", "partial 不等于失败");
    }

    /// wire 值稳定性：前端 `run.types.ts` / `ipc/types.ts` 两处联合类型与 i18n 文案
    /// 按字符串依赖这些值，改名即破坏兼容。
    #[test]
    fn ended_wire_values_are_stable() {
        assert_eq!(SubEnded::Report.as_str(), "report");
        assert_eq!(SubEnded::Partial.as_str(), "partial");
        assert_eq!(SubEnded::Budget.as_str(), "budget");
        assert_eq!(SubEnded::NoReport.as_str(), "no_report");
    }

    #[tokio::test]
    async fn acquire_slot_paths() {
        // 空闲：快速路径立即成功
        assert!(
            acquire_slot(
                &tokio_util::sync::CancellationToken::new(),
                std::time::Duration::from_millis(50)
            )
            .await
        );
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
        // 满员：有界等待后失败
        for _ in 0..MAX_CONCURRENT {
            ACTIVE.fetch_add(1, Ordering::SeqCst);
        }
        assert!(
            !acquire_slot(
                &tokio_util::sync::CancellationToken::new(),
                std::time::Duration::from_millis(100)
            )
            .await
        );
        // 满员 + 取消：立即中断等待（不耗满 max_wait）
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        let started = std::time::Instant::now();
        assert!(!acquire_slot(&cancelled, std::time::Duration::from_secs(30)).await);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        for _ in 0..MAX_CONCURRENT {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn summarize_tail_extracts_tool_and_snippet() {
        let h = vec![
            crate::core::types::Message::user_text("task"),
            crate::core::types::Message {
                role: crate::core::types::Role::Assistant,
                created_at: None,
                content: vec![
                    crate::core::types::Content::Thinking {
                        text: "想一想".into(),
                    },
                    crate::core::types::Content::ToolUse {
                        id: "t".into(),
                        name: "read".into(),
                        args: serde_json::json!({}),
                    },
                ],
            },
        ];
        let (tool, snippet) = summarize_sub_tail(&h);
        assert_eq!(tool.as_deref(), Some("read"));
        assert_eq!(snippet, "想一想");
    }

    #[test]
    fn system_extra_injects_definition_for_known_role() {
        let def = crate::agents::find("backend-dev").unwrap();
        let s = build_system_extra("backend-dev", 60, Some(def), ApprovalMode::AutoEdit);
        assert!(s.contains("<subagent-discipline>"));
        assert!(s.contains("<agent-definition name=\"backend-dev\">"));
        assert!(s.contains("资深后端开发工程师"));
        assert!(s.contains("步数预算 60 步"));
        // <report> 包裹要求（防过程旁白被当作最终汇报）
        assert!(s.contains("<report>"));
        // 纪律块必须在最前（稳定前缀语义）
        let disc = s.find("<subagent-discipline>").unwrap();
        let body = s.find("<agent-definition name=\"backend-dev\">").unwrap();
        assert!(disc < body);
    }

    #[test]
    fn system_extra_unknown_role_keeps_legacy_shape() {
        let s = build_system_extra("my-custom-role", 25, None, ApprovalMode::AutoEdit);
        assert!(s.contains("<subagent-discipline>"));
        assert!(s.contains("角色：my-custom-role"));
        assert!(!s.contains("<agent-definition"));
    }

    #[test]
    fn system_extra_alias_shows_canonical_name() {
        let s = build_system_extra("PM", 30, crate::agents::find("PM"), ApprovalMode::AutoEdit);
        assert!(s.contains("角色：product-manager"));
        assert!(s.contains("<agent-definition name=\"product-manager\">"));
    }

    #[test]
    fn analysis_role_consistent_with_registry_aliases() {
        // 评审 Y1：别名与注册表共用 find() 单一事实源
        assert!(is_analysis_role("tester"));
        assert!(
            is_analysis_role("test"),
            "别名 test 注入 tester 全文，必须同样置分析标记"
        );
        assert!(is_analysis_role("product-manager"));
        assert!(is_analysis_role("PM"));
        assert!(is_analysis_role("product_manager"));
        assert!(!is_analysis_role("backend-dev"));
        assert!(!is_analysis_role("frontend-dev"));
        assert!(!is_analysis_role("app-dev"));
        assert!(!is_analysis_role("reviewer"));
        assert!(!is_analysis_role("protester"));
        assert!(!is_analysis_role(""));
    }

    #[test]
    fn idle_policy_for_role_matrix() {
        use crate::core::agent::IdlePolicy;
        // 只读角色（含大小写 / 下划线 / 空格归一）→ 只纠偏不硬终止
        for role in [
            "explore",
            "reviewer",
            "code-reviewer",
            "Explore",
            " code reviewer ",
            "CODE_REVIEWER",
        ] {
            assert_eq!(
                idle_policy_for(role),
                IdlePolicy::NudgeOnly,
                "{role} 应走只读策略"
            );
        }
        // 可写角色 / 内部 title / 未命中 / 空串：保持默认终止语义（不得放宽）
        for role in [
            "backend-dev",
            "frontend-dev",
            "app-dev",
            "tester",
            "product-manager",
            "testing",
            "title",
            "unknown-role",
            "",
        ] {
            assert_eq!(
                idle_policy_for(role),
                IdlePolicy::Stop,
                "{role} 应保持默认策略"
            );
        }
    }

    #[test]
    fn readonly_roles_exclude_write_tools_only() {
        let readonly_roles = [
            "explore",
            "reviewer",
            "code-reviewer",
            "Explore",
            " code reviewer ",
        ];
        for role in readonly_roles {
            let ex = readonly_extra_excludes(role, ApprovalMode::AutoEdit);
            for tool in ["edit", "create", "delete"] {
                assert!(ex.contains(&tool), "{role} 应排除写工具 {tool}");
            }
            // 只读调研必需的只读工具不得被排除（尤其 command：git status 等）
            for tool in ["command", "read", "grep"] {
                assert!(!ex.contains(&tool), "{role} 不应排除只读工具 {tool}");
            }
        }
        // 非只读角色不得被额外排除（写能力与 command 均保留；权限仍受父档位集合约束）
        for role in [
            "backend-dev",
            "frontend-dev",
            "app-dev",
            "tester",
            "product-manager",
            "title",
            "unknown-role",
            "",
        ] {
            assert!(
                readonly_extra_excludes(role, ApprovalMode::AutoEdit).is_empty(),
                "{role} 不应有额外排除"
            );
        }
        // B3：FullAccess 档下只读角色不再排除写工具（父档已完全放行写入，再锁子代理自相矛盾）
        for role in readonly_roles {
            assert!(
                readonly_extra_excludes(role, ApprovalMode::FullAccess).is_empty(),
                "{role} 在完全访问档下不应再排除写工具"
            );
        }
    }

    /// 策略装配纯函数直接断言调用点（🟡2）：只读角色一步到位置策略 + 追加写工具排除。
    #[test]
    fn apply_role_policy_marks_readonly_roles() {
        use crate::core::agent::IdlePolicy;
        for role in [
            "explore",
            "reviewer",
            "code-reviewer",
            "Explore",
            " code reviewer ",
            "CODE_REVIEWER",
        ] {
            let mut p = DriveParams::default();
            apply_role_policy(&mut p, role, ApprovalMode::AutoEdit);
            assert_eq!(p.idle_policy, IdlePolicy::NudgeOnly, "{role} 应置只读策略");
            for t in crate::core::agent::WRITE_TOOLS.iter().copied() {
                assert!(
                    p.exclude_tools.iter().any(|e| e == t),
                    "{role} 应排除写工具 {t}：{:?}",
                    p.exclude_tools
                );
            }
            // 只读调研必需的只读工具不得被排除（尤其 command：git status 等）
            for t in ["command", "read", "grep"] {
                assert!(
                    !p.exclude_tools.iter().any(|e| e == t),
                    "{role} 不得排除只读工具 {t}：{:?}",
                    p.exclude_tools
                );
            }
        }
    }

    /// 可写 / 内部 / 未命中 / 空串角色：策略保持 `Stop` 且不追加任何排除项。
    #[test]
    fn apply_role_policy_leaves_writable_roles_untouched() {
        use crate::core::agent::IdlePolicy;
        for role in [
            "backend-dev",
            "frontend-dev",
            "app-dev",
            "tester",
            "product-manager",
            "title",
            "unknown-role",
            "",
        ] {
            let mut p = DriveParams::default();
            apply_role_policy(&mut p, role, ApprovalMode::AutoEdit);
            assert_eq!(p.idle_policy, IdlePolicy::Stop, "{role} 应保持默认策略");
            assert!(
                p.exclude_tools.is_empty(),
                "{role} 不应追加排除项：{:?}",
                p.exclude_tools
            );
        }
    }

    /// 幂等：重复调用不产生重复项，也不覆盖父档位已合并的排除集。
    #[test]
    fn apply_role_policy_is_idempotent_over_parent_excludes() {
        // 模拟父档位已合并集（其中 edit 与只读写排除重叠）
        let mut p = DriveParams {
            exclude_tools: vec!["ask".into(), "edit".into()],
            ..Default::default()
        };
        apply_role_policy(&mut p, "explore", ApprovalMode::AutoEdit);
        let once = p.exclude_tools.clone();
        apply_role_policy(&mut p, "explore", ApprovalMode::AutoEdit);
        assert_eq!(p.exclude_tools, once, "重复调用不得追加重复项");
        assert_eq!(
            once.iter().filter(|e| e.as_str() == "edit").count(),
            1,
            "已存在的写工具不得重复 push"
        );
        for t in ["ask", "edit", "create", "delete"] {
            assert!(p.exclude_tools.iter().any(|e| e == t), "缺排除项 {t}");
        }
        assert_eq!(p.idle_policy, crate::core::agent::IdlePolicy::NudgeOnly);
    }

    /// 子代理角色覆盖命中：effective_subagent_model 返回角色级模型。
    #[test]
    fn subagent_model_override_takes_effect() {
        let mut cfg = ConfigState::default();
        cfg.providers.push(crate::core::config::ProviderConfig {
            id: "p1".into(),
            models: vec![
                crate::core::config::ProviderModel {
                    id: "m-active".into(),
                    ..Default::default()
                },
                crate::core::config::ProviderModel {
                    id: "m-tester-role".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        cfg.active_model_id = Some("m-active".into());
        cfg.subagent_models
            .insert("tester".into(), Some("m-tester-role".into()));
        // 父 prefs model_id 设为另一个——仍应被角色覆盖优先。
        let parent_prefs = SessionPrefs {
            model_id: Some("m-active".into()),
            ..Default::default()
        };
        let m = effective_subagent_model(&cfg, "tester", &parent_prefs).unwrap();
        assert_eq!(m.id, "m-tester-role");
    }

    /// 角色覆盖未设值：回落父 prefs（父未设 → 回落全局 active）。
    #[test]
    fn subagent_model_override_unset_falls_back() {
        let mut cfg = ConfigState::default();
        cfg.providers.push(crate::core::config::ProviderConfig {
            id: "p1".into(),
            models: vec![
                crate::core::config::ProviderModel {
                    id: "m-active".into(),
                    ..Default::default()
                },
                crate::core::config::ProviderModel {
                    id: "m-parent".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        cfg.active_model_id = Some("m-active".into());
        cfg.subagent_models.insert("tester".into(), None);
        // 父 prefs 未设 → active 胜出
        assert_eq!(
            effective_subagent_model(&cfg, "tester", &SessionPrefs::default())
                .unwrap()
                .id,
            "m-active"
        );
        // 父 prefs 设了 → 父 prefs 胜出（模拟「跟随父会话」语义）
        let parent_prefs = SessionPrefs {
            model_id: Some("m-parent".into()),
            ..Default::default()
        };
        assert_eq!(
            effective_subagent_model(&cfg, "tester", &parent_prefs)
                .unwrap()
                .id,
            "m-parent"
        );
    }

    #[test]
    fn system_extra_marks_readonly_roles() {
        for role in ["explore", "reviewer", "code-reviewer"] {
            let s = build_system_extra(role, 40, crate::agents::find(role), ApprovalMode::AutoEdit);
            assert!(
                s.contains("你是只读角色，没有写工具"),
                "{role} 缺只读提示句"
            );
            assert!(s.contains("<subagent-discipline>"), "{role} 缺纪律块");
        }
        for role in ["backend-dev", "tester", "product-manager", "unknown-role"] {
            let s = build_system_extra(role, 40, crate::agents::find(role), ApprovalMode::AutoEdit);
            assert!(!s.contains("你是只读角色"), "{role} 不应带只读提示句");
        }
    }

    /// B3：只读提示句随档位切换——FullAccess 下改为授权说明，
    /// 否则会出现「提示说没有写工具、事实上写得动」的自相矛盾。
    #[test]
    fn system_extra_readonly_notice_follows_mode() {
        for role in ["explore", "reviewer", "code-reviewer"] {
            let unlocked = build_system_extra(
                role,
                40,
                crate::agents::find(role),
                ApprovalMode::FullAccess,
            );
            assert!(unlocked.contains("完全访问档"), "{role} 缺授权说明");
            assert!(
                !unlocked.contains("你是只读角色，没有写工具"),
                "{role} 在完全访问档下不得再声称没有写工具"
            );
            // 🟡5 返工：角色定义正文自带只读禁令（explore「只读调研：不修改任何文件」等），
            // 与授权说明打架 → 完全访问档下正文末尾必须带覆盖声明显式作废
            assert!(
                unlocked.contains("上述角色定义中的只读约束"),
                "{role} 在完全访问档下缺正文覆盖声明（正文仍宣称只读）"
            );
            // 其余三档仍为只读约束
            for mode in [
                ApprovalMode::ConfirmEach,
                ApprovalMode::AutoEdit,
                ApprovalMode::Plan,
            ] {
                let s = build_system_extra(role, 40, crate::agents::find(role), mode);
                assert!(
                    s.contains("你是只读角色，没有写工具"),
                    "{role} 在 {mode:?} 档应保留只读提示句"
                );
                assert!(
                    !s.contains("上述角色定义中的只读约束"),
                    "{role} 在 {mode:?} 档正文不得出现覆盖声明（文案须与改造前逐字一致）"
                );
            }
        }
        // 可写角色任何档位都不带授权/只读提示句
        for mode in [ApprovalMode::AutoEdit, ApprovalMode::FullAccess] {
            for role in ["backend-dev", "tester", "unknown-role"] {
                let s = build_system_extra(role, 40, crate::agents::find(role), mode);
                assert!(!s.contains("你是只读角色"), "{role} 不应带只读提示句");
                assert!(!s.contains("完全访问档"), "{role} 不应带授权说明");
                assert!(
                    !s.contains("上述角色定义中的只读约束"),
                    "{role} 不应带正文覆盖声明"
                );
            }
        }
    }

    /// B3：完全访问档下只读角色解锁写能力（idle_policy 与档位解耦，保持 NudgeOnly）。
    #[test]
    fn full_access_unlocks_readonly_write_tools() {
        let mut p = DriveParams::default();
        apply_role_policy(&mut p, "explore", ApprovalMode::FullAccess);
        for t in crate::core::agent::WRITE_TOOLS.iter().copied() {
            assert!(
                !p.exclude_tools.iter().any(|e| e == t),
                "完全访问档不应排除 {t}：{:?}",
                p.exclude_tools
            );
        }
        assert_eq!(p.idle_policy, crate::core::agent::IdlePolicy::NudgeOnly);
        // 其余三档保持只读
        for mode in [
            ApprovalMode::ConfirmEach,
            ApprovalMode::AutoEdit,
            ApprovalMode::Plan,
        ] {
            let mut p = DriveParams::default();
            apply_role_policy(&mut p, "explore", mode);
            for t in crate::core::agent::WRITE_TOOLS.iter().copied() {
                assert!(
                    p.exclude_tools.iter().any(|e| e == t),
                    "{mode:?} 档应排除 {t}"
                );
            }
        }
    }

    /// B1 基座：内部 7 项与档位无关（冻结），root_session_id 由调用点回填，
    /// 基座纪律块本身不含 `<plan-mode>`（父档块由重建时拼接）。
    #[test]
    fn sub_base_freezes_internal_excludes() {
        let base = sub_base(
            "explore",
            25,
            crate::agents::find("explore"),
            ApprovalMode::Plan,
            crate::core::delivery::responsibility::DutyKind::Implement,
        );
        assert_eq!(base.base_excludes.len(), SUB_BASE_EXCLUDES.len());
        for t in SUB_BASE_EXCLUDES {
            assert!(base.base_excludes.iter().any(|e| e == t), "基座缺 {t}");
        }
        assert!(base.root_session_id.is_none());
        assert_eq!(base.idle_policy, crate::core::agent::IdlePolicy::NudgeOnly);
        assert!(base.base_system_extra.contains("<subagent-discipline>"));
        assert!(!base.base_system_extra.contains("<plan-mode>"));
    }
}
