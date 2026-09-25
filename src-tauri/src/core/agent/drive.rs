use super::goal::{
    self, GOAL_TEXT_TURN_LIMIT, GoalPhase, GoalState, GoalStatus, LEDGER_DENIAL_RETRY_LIMIT,
    StallVerdict,
};
use super::guards::{CompactingGuard, DriveUnwindGuard, lock_ok};
use super::runtime::{
    AgentCore, CHECKPOINT_EVERY_STEPS, EventSink, Frame, INJECT_BUFFER, MAX_STEPS,
    STREAM_THROTTLE_MS, SessionRuntime,
};
use super::stream::{ERROR_CAP, VERBOSE_BODY_CAP};
use super::stream::{
    build_assistant_message, build_stream_request, collect_deltas, flush_segments,
    refresh_request_messages, stream_flush_loop,
};
use super::supervise::{BatchDigest, CallSig, IdlePolicy, SupervisionState, Verdict};
use super::text_ask;
use crate::core::context::{self};
use crate::core::session_log;
use crate::core::sessions::SaveReport;
use crate::core::sessions::repair;
use crate::core::types::{Content, Message, Role};
use crate::provider::dto::{AsmBlock, Assembled, AssembledToolCall, ProviderError};
use crate::provider::retry;
use crate::tools::batch::execute_batch;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 文本形态 ask 兜底是否可用：仅主会话，且 ask 确实在本次工具集里。
/// 排除项统一由档位/角色策略写入 `exclude_tools`（目标档执行期「零提问」、子代理工具集
/// 不含 ask），故此处只看名单——将来新增排除方无需再改这里。
fn ask_available(params: &DriveParams) -> bool {
    params.main_session && !params.exclude_tools.iter().any(|t| t == "ask")
}

/// 从装配块里剥掉被恢复的块文本；返回是否真剥掉了。
/// 找不到就不兜底——「正文留着协议原文却多出一个调用」比不兜底更糟。
fn strip_text_block(asm: &mut Assembled, block: &str) -> bool {
    for b in asm.blocks.iter_mut() {
        if let AsmBlock::Text(t) = b {
            if t.contains(block) {
                *t = text_ask::strip_block(t, block);
                return true;
            }
        }
    }
    false
}

/// 本回合正文总字数（诊断日志用；避免只为了计数克隆整段文本）。
fn assembled_text_chars(asm: &Assembled) -> usize {
    asm.blocks
        .iter()
        .filter_map(|b| match b {
            AsmBlock::Text(t) => Some(t.chars().count()),
            _ => None,
        })
        .sum()
}

/// 归一化后的工具调用（参数已修复为合法 JSON object；供批次执行层消费）。
#[derive(Debug, Clone)]
pub struct NormalizedCall {
    /// 调用 id（与 tool_use 对应）
    pub id: String,
    /// 工具名
    pub name: String,
    /// 归一化后的参数（非 object 值包成 {"value": ...}）
    pub args: serde_json::Value,
    /// 批内位置（`execute_batch` 入口重编号；前端 `call_key` 与进度帧 index 的唯一真相）。
    /// 上游 provider 侧下标（anthropic 内容块下标，会被 thinking / text 块顶偏）不得泄漏到前端 key。
    pub index: usize,
}

/// 驱动主循环的参数集：主会话 / 子代理 / 任务运行三类调用方共用，差异全在此表达。
pub struct DriveParams {
    /// 步数预算（主会话 MAX_STEPS；子代理/任务各有限额）
    pub max_steps: usize,
    /// 工具排除集（按名；子代理排除 ask/subagent/plan/skill/scheduled_task；
    /// 派发层对被排除调用硬拒绝（batch::run_tool），与模型工具列表同源）
    pub exclude_tools: Vec<String>,
    /// 是否排除 MCP 工具（任务隔离）
    pub exclude_mcp: bool,
    /// 追加到 system prompt 末尾（角色注入 / 任务指令）
    pub system_extra: String,
    /// 步数低预算提醒（子代理）
    pub budget_notice: bool,
    /// 是否发 run:* / tool:* 事件（子代理与任务运行走各自的事件通道）
    pub emit_events: bool,
    /// 预算耗尽前强制汇报一轮
    pub force_report: bool,
    /// 无工具调用的回合即视为本 run 完成（主会话语义）。
    /// `false` = 子代理 / 任务运行：纯文本回合不再无条件收尾——须带 `<report>` 标记，
    /// 否则注入提示后续跑（上限 `MAX_TEXT_TURNS`），绝不把过程旁白当成最终汇报
    ///（[docs/subagent-text-turn-premature-exit]）。
    pub finish_on_text: bool,
    /// 主会话 run：每步按当前偏好重算计划限制（ask 批准切档后立即生效）
    pub main_session: bool,
    /// 父 run 取消令牌（子代理派发时传入）：提供时本 run 的取消令牌由其 child_token 派生，
    /// 主会话停止即级联中止子代理的 LLM 流与审批等待（[docs/subagent-file-isolation]）
    pub parent_cancel: Option<CancellationToken>,
    /// 空转看门狗策略（[docs/subagent-idle-watchdog-misfire]）：默认 `Stop`（8 步纠偏 /
    /// 14 步终止）；只读角色子代理由 `subagent` 工具置 `NudgeOnly`（空转层 16 步纠偏一次、
    /// 不硬终止——失败重复层与步数/汇报门不受本字段影响，照常终止）。
    pub idle_policy: IdlePolicy,
    /// 子代理档位基座（B1）：`Some` = 本 run 是子代理，每步按父会话**实时**档位从基座重建
    /// 排除集 / system_extra / idle_policy（`refresh_subagent_mode`）；`None` = 主会话或
    /// 任务运行（任务运行的档位在 `core::scheduler` 侧显式置 FullAccess，参数保持冻结）。
    pub sub_base: Option<SubBase>,
    /// 目标模式推进指令（**瞬态**）：只附在**下一次请求**的出网副本末尾
    ///（`stream::messages_for_request` 消费），绝不写入会话历史——同一目标会反复下发推进指令，
    /// 落盘只会污染转录并打穿 provider 前缀缓存。请求组装后由 drive 清空（一次性）。
    pub goal_transient: Option<String>,
}

impl Default for DriveParams {
    fn default() -> Self {
        DriveParams {
            max_steps: MAX_STEPS,
            exclude_tools: Vec::new(),
            exclude_mcp: false,
            system_extra: String::new(),
            budget_notice: false,
            emit_events: true,
            force_report: false,
            finish_on_text: true,
            main_session: false,
            parent_cancel: None,
            idle_policy: IdlePolicy::default(),
            sub_base: None,
            goal_transient: None,
        }
    }
}

/// 主会话 run（[docs/p0-plan](../../../../docs/p0-plan.md) §5.2）：会话装载 → drive → 检查点/统计/事件。
pub async fn run_chat(
    core: Arc<AgentCore>,
    rt: Arc<SessionRuntime>,
    user: Message,
    run_id: String,
) {
    let sink = core.sink.clone();
    let run_started = std::time::Instant::now();
    rt.injected_plan_snapshot_for_run
        .store(false, Ordering::SeqCst);
    // plan 软提醒（batch::maybe_emit_plan_hint）每 run 至多一次：run 起点同模式复位
    rt.plan_hint_emitted.store(false, Ordering::SeqCst);

    // 会话装载：内存为空但磁盘有历史 → 加载（历史 + todos）
    if rt.history.lock().unwrap().is_empty() {
        if let Ok(disk) = core.store.load_history(&rt.id) {
            *rt.history.lock().unwrap() = disk;
        }
        let todos = core.store.load_todos(&rt.id);
        if !todos.is_empty() {
            *rt.todos.lock().unwrap() = todos.clone();
            sink.emit(
                &rt.id,
                "plan:update",
                serde_json::json!({ "session": rt.id, "todos": todos }),
            );
        }
        // 目标模式边车（goal mode）：目标存档**保留**（用户要能回看），恢复会话即装回内存态
        // ——不装回的话，「已完成的目标」在恢复后会退化成「未登记」并把档位语义带偏。
        if rt.goal_snapshot().is_none() {
            if let Some(g) = core.store.load_goal(&rt.id) {
                rt.set_goal(Some(g.clone()));
                sink.emit(
                    &rt.id,
                    "goal:update",
                    serde_json::json!({ "session": rt.id, "goal": g }),
                );
            }
        }
    }

    // 首条消息判定：装载磁盘历史后仍为空 = 本会话首条消息，用于触发自动命名
    // （[docs/session-auto-title](../../../../docs/session-auto-title.md)）
    let history_was_empty = rt.history.lock().unwrap().is_empty();
    let first_user_text = if history_was_empty {
        user.content.iter().find_map(|c| match c {
            crate::core::types::Content::Text { text } => Some(text.clone()),
            _ => None,
        })
    } else {
        None
    };
    rt.history.lock().unwrap().push(user.stamped());
    sink.emit(
        &rt.id,
        "run:start",
        serde_json::json!({ "session": rt.id, "run_id": run_id }),
    );
    {
        let first_line: String = rt
            .history
            .lock()
            .unwrap()
            .last()
            .and_then(|m| {
                m.content.iter().find_map(|c| match c {
                    crate::core::types::Content::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        let prefs = rt.prefs();
        let model = {
            let cfg = core.cfg.read().unwrap();
            crate::core::prefs::effective_model(&cfg, &prefs)
                .map(|m| m.name.clone())
                .unwrap_or_default()
        };
        session_log::info(
            &rt,
            &format!(
                "run {run_id} 开始 mode={:?} model={model}：{first_line}",
                prefs.approval_mode
            ),
        );
    }

    // 自动命名（[docs/session-auto-title](../../../../docs/session-auto-title.md)）：首条含文本消息 → 并行生成精炼标题，
    // 不阻塞本 run；失败静默保留 10 字符兜底
    if let Some(text) = first_user_text.filter(|t| !t.trim().is_empty()) {
        let fallback = lock_ok(&rt.title).clone();
        let core_t = core.clone();
        let rt_t = rt.clone();
        tokio::spawn(async move {
            crate::core::title::generate_and_apply(core_t, rt_t, text, fallback).await;
        });
    }

    // 审批档位 → 主会话驱动参数（Plan：排除写工具与 MCP，提示模型先出方案；
    // Goal：按目标阶段分档收紧工具集，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
    let params = main_drive_params(&rt.prefs(), rt.goal_snapshot().as_ref());
    // panic 兜底（G-panic）：drive 逃逸的 panic 被路由进常规错误路径——
    // catch 层在收尾前拦截，下方 running 复位 / 检查点 / run:error 照常执行，
    // 会话不再因 panic 卡死在「运行中」。
    let (result, run_usage, suggest_out) = match futures::FutureExt::catch_unwind(
        std::panic::AssertUnwindSafe(drive_agent(&core, &rt, params, &run_id)),
    )
    .await
    {
        Ok(v) => v,
        Err(payload) => {
            let msg = panic_msg(&payload);
            session_log::error(&rt, &format!("run {run_id} 内部 panic：{msg}"));
            (
                Err(ProviderError::Protocol(format!("agent 内部错误：{msg}"))),
                crate::provider::RunUsage::default(),
                None,
            )
        }
    };
    // C1 修复：drive 结束（成功/失败/取消）都必须复位，否则会话永远拒绝第二次 run
    rt.running.store(false, Ordering::SeqCst);

    match &result {
        Ok(_) => {
            // 保存结果接入 run:done：不干净（拒存 / 剥图 / 丢轮 / P4 的体积告警与熔断）时带 JSON 载荷告知前端
            let save = checkpoint(&core, &rt).await;
            session_log::info(
                &rt,
                &format!(
                    "run {run_id} 完成 input {} / output {} tokens 耗时 {}s",
                    run_usage.input,
                    run_usage.output,
                    run_started.elapsed().as_secs()
                ),
            );
            // 唯一的 run:done，在 running 复位之后发（[docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md) 队列回归：前端
            // 以此出队）；建议项（如有）并入 payload，取代此前 drive_agent 内部提前
            // 发的 done
            let mut payload = serde_json::json!({ "session": rt.id, "run_id": run_id });
            if let Some(items) = suggest_out {
                payload["suggestions"] = serde_json::json!(items);
            }
            // 保存不干净才带上（干净路径零打扰，前端据此 push 会话内提示）。
            // P4：体积状态（`warned` / `fused`）同样由 `is_clean()` 纳入——`SaveReport.history_status`
            // 随 `to_value` 一起上 wire（形状与索引侧 `SessionMeta.history_status` 同源，前端共用一套文案）
            if let Some(r) = save.filter(|r| !r.is_clean()) {
                payload["history_save"] = serde_json::to_value(&r).unwrap_or_default();
            }
            sink.emit(&rt.id, "run:done", payload);
        }
        Err(ProviderError::Cancelled) => {
            mark_cancelled(&core, &rt, &run_id).await;
            session_log::warn(
                &rt,
                &format!(
                    "run {run_id} 被用户取消（耗时 {}s）",
                    run_started.elapsed().as_secs()
                ),
            );
        }
        Err(e) => {
            let _ = checkpoint(&core, &rt).await;
            session_log::error(&rt, &format!("run {run_id} 失败：{e}"));
            sink.emit(
                &rt.id,
                "run:error",
                serde_json::json!({
                    "session": rt.id,
                    "run_id": run_id,
                    "error": e.to_string(),
                    // [docs/auth-error-guidance](../../../../docs/auth-error-guidance.md)：结构化类别（前端据 kind 把鉴权/计费问题引导到供应商设置）
                    "kind": e.kind_tag(),
                }),
            );
        }
    }

    // 统计落盘（P2-H）：归属会话生效模型（覆盖优先，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
    if run_usage.input + run_usage.output > 0 {
        let model_id = {
            let cfg = core.cfg.read().unwrap();
            crate::core::prefs::effective_model(&cfg, &rt.prefs())
                .map(|m| m.id.clone())
                .unwrap_or_default()
        };
        core.stats.record_timed(
            crate::core::stats::UsageRecord {
                session: rt.id.clone(),
                model_id,
                workspace: rt.workspace.to_string_lossy().into_owned(),
                input: run_usage.input,
                output: run_usage.output,
                cache_read: run_usage.cache_read,
                cache_write: run_usage.cache_write,
                runs: 1,
                kind: crate::core::stats::KIND_MAIN.into(),
            },
            // 本 run 的耗时/TTFT 观测（仅主会话步计入：压缩/命名/子代理/任务走 record，计时全 0）
            *rt.run_timing.lock().unwrap(),
        );
    }
}

/// 写类工具名：plan 档排除（`apply_plan_mode`）与只读子代理的额外排除
/// （`tools::subagent::apply_role_policy`）共用同一份名单——两处各自内联会导致
/// 「新增写工具」时漏改一处。经 `core::agent` re-export 为 `crate::core::agent::WRITE_TOOLS`。
/// 注意：`command` 刻意不在其列（只读调研需要 git status 等命令，由 fence 逐条把关）。
pub const WRITE_TOOLS: &[&str] = &[
    "edit",
    "create",
    "delete",
    "write_document",
    "edit_document",
];

/// 主会话 DriveParams：按会话审批档位追加排除项与提示文本（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）。
/// Plan 档收紧（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）：排除写工具 / 后台服务 / 任务运行 + MCP；
/// shell 保留但受只读 fence 白名单约束（plan_readonly，白名单外一律确认）；
/// 子代理继承同样语义。
/// 目标档（goal mode）再分两段：澄清期严格只读（登记目标前不动工作区）、执行期零提问
///（`apply_goal_mode`）。
/// 主会话 run 每步按当前偏好 + 当前目标状态重算限制（ask 批准切档 / 目标阶段推进在下一步生效）；
/// 子代理每步按父会话实时档位从基座重建（`subagent_drive_params`，B1）；
/// 任务运行的参数由 spawn 时冻结（其档位在 `core::scheduler` 侧显式置 FullAccess）。
///
/// `goal` = 当前会话目标状态快照（`None` = 未登记，按澄清期处理）。调用方传快照而非
/// `&SessionRuntime`：本函数是**纯装配**，不持锁、不 await，便于单测与幂等重算。
pub fn main_drive_params(
    prefs: &crate::core::prefs::SessionPrefs,
    goal: Option<&GoalState>,
) -> DriveParams {
    let mut params = DriveParams {
        main_session: true,
        ..DriveParams::default()
    };
    apply_plan_mode(&mut params, prefs);
    apply_goal_mode(&mut params, prefs, goal);
    params
}

/// Plan 档限制追加（主会话每步重算时调用）。
pub(super) fn apply_plan_mode(params: &mut DriveParams, prefs: &crate::core::prefs::SessionPrefs) {
    if prefs.approval_mode != crate::core::prefs::ApprovalMode::Plan {
        return;
    }
    // 写工具三件套走共享常量（值不变）；plan 档自有部分（后台服务 / 计划任务）就地保留
    params.exclude_tools.extend(
        WRITE_TOOLS
            .iter()
            .copied()
            .chain(["service", "scheduled_task"])
            .map(String::from),
    );
    params.exclude_mcp = true;
    params.system_extra =
        "\n<plan-mode>计划模式：只读调研，不执行任何修改。文件写入、后台服务与计划任务工具不可用，MCP 工具不可用；shell 仅放行只读命令白名单（ls/cd/head/grep/git log、gh pr view/gh run view 等；gh 按子命令放行——gh pr merge、gh release edit、gh api -X POST、gh secret set 这类远端写会被拦，其余命令会被直接拦截，不会弹确认——请把需要执行的命令纳入方案，经批准后运行）；子代理同样仅限只读。严格按阶段流程推进（docs/plan-mode-workflow），不得跳步：\nP0 接到请求先声明分类（需求/缺陷/问答/混合）与一句话依据；问答类直接回答，不进流程。\nP1 有关键歧义先澄清，无歧义则声明假设继续。\nP2 需求类调用 product-manager 子代理产出结构化需求分析（用户故事/AC/边界/非目标/开放问题）；缺陷类调用 tester 产出复现步骤/根因/影响面/修复建议与回归要点；开放问题回流澄清（≤2 轮）。分析不设用户确认门，产出后直接进入 P3。\nP3 基于分析编写技术方案（文件级改动点/风险/回滚；git 仓库内拟定分支名 <type>/<slug>，slug ≤24 字符、基线当前 HEAD，非 git 仓库注明跳过），用 plan 工具登记 todos（必须包含验证项），然后用 ask 工具询问用户（题干与 plan 文本列明分支名；批准 = 预授权创建并切换分支）：批准门为 ask 单题并携带 switchToAutoEdit=true，提供两个批准类选项——「以自动编辑档执行」（id=approve，切自动编辑档，recommended）与「以完全访问档执行」（id=approve_full，切完全访问档）：批准类选项必须带 mode 字段声明「选中后把会话切到哪个权限档」（mode=\"auto_edit\" / mode=\"full_access\"，缺省回落 auto_edit）——不声明就会回落自动编辑档，用户选「完全访问执行」会被静默降级；系统按用户所选档位切档并指示你立即执行；有意见则选「补充意见」（id=revise）——修订时逐条回应（采纳/不采纳+理由），基于上一版做增量更新，不重做分析；同一方案 3 轮未收敛则把争议点拆成多个选项逐项询问；要看效果则选「先看预览」（id=preview，不带 mode）：先加载 preview 技能渲染方案预览，再重发同一询问——看预览不是批准、不算有效应答，不切档也不计入修订轮次，绝不静默批准。禁止未经 P2 分析、或 todos 缺失/含验证项时就发起询问。\n轻量路径：改动预计 ≤2 文件、无删除、无新依赖、无跨层改动时，P2 可用内置简析替代子代理调用（在回复中明示「轻量路径」）；批准询问不可省略。用户明确说「直接改/不用分析」时同样跳过 P2，但仍需登记 todos 并经批准。\n批准后：git 仓库内先执行 git switch -c <分支名>（已存在则改 -2 后缀并说明；失败如实报告请用户处理）再动工；严格按已确认 todos 顺序执行，超范围写操作先询问；多文件/跨层变更完成后调用 code-reviewer 审查（🔴 必须修复），最后汇报变更摘要、验证结果与本轮偏差记录。</plan-mode>"
            .into();
}

/// 目标档限制追加（主会话每步重算时调用）。
///
/// 两阶段的工具集是「澄清不动任何东西 / 执行期零提问」这两条承诺的**机制保证**：
/// - 澄清期（`GoalPhase::Clarify`，含未登记与已登记未进入执行）：排除写工具三件套
///   （共享常量 `WRITE_TOOLS`）**并额外排除 `command` / `service` / `scheduled_task`**
///   ——严格只读，模型的任何「顺手改一下」都会被工具层硬拒；
///   `ask` 保留（澄清靠它提问）、`goal` 保留（登记用）、只读工具与 `subagent` 调研保留
///   （子代理经父档合并继承同样的只读语义）。
/// - 执行期（`GoalPhase::Execute`）：**排除 `ask`**（硬保证零提问：歧义自行按
///   「最小惊讶 + 可回滚」自决并记入 `decisions`）；写工具与命令全部放开，范围控制交给
///   账本（越界由工具层硬拦，不弹审批）；`goal` 保留（勾选验收标准）。
///
/// `idle_policy`：**两个阶段都用 `NudgeOnly`**（空转层只提醒不终止——「未达成前不要停下」与空转
/// 看门狗硬终止直接冲突，停滞判定由驱动层两段式负责；失败重复层与步数门照常生效）。
/// 澄清期也必须 `NudgeOnly`：只读调研是澄清期的**常态**，默认 `Stop` 会在 14 批空转时误杀
/// 「仍在只读调研」的澄清（[docs/subagent-idle-watchdog-misfire](../../../../docs/subagent-idle-watchdog-misfire.md)
/// 同因）。停滞自停只在执行期生效（`goal_bookkeep`），澄清期不设自停门。
///
/// 系统提示块用**赋值**语义（`system_extra = render_goal_block(..)`）：与 `<plan-mode>`
/// 块互斥，档位互斥故安全；每步从基座重建、绝不增量追加（旧块残留是已记录的坑）。
pub(super) fn apply_goal_mode(
    params: &mut DriveParams,
    prefs: &crate::core::prefs::SessionPrefs,
    goal: Option<&GoalState>,
) {
    if prefs.approval_mode != crate::core::prefs::ApprovalMode::Goal {
        return;
    }
    let phase = goal::goal_phase(goal).unwrap_or(GoalPhase::Clarify);
    match phase {
        GoalPhase::Clarify => {
            params.exclude_tools.extend(
                WRITE_TOOLS
                    .iter()
                    .copied()
                    .chain(["command", "service", "scheduled_task", "http_request"])
                    .map(String::from),
            );
            params.exclude_mcp = true;
            params.idle_policy = IdlePolicy::NudgeOnly;
        }
        GoalPhase::Execute => {
            params.exclude_tools.push("ask".into());
            params.exclude_tools.push("http_request".into());
            params.exclude_mcp = true;
            params.idle_policy = IdlePolicy::NudgeOnly;
        }
    }
    params.system_extra = goal::render_goal_block(goal);
}

// ===================== 目标模式（goal mode）驱动层 =====================
//
// 阶段化的工具集 / 提示块 / idle 策略见 `apply_goal_mode`；本段承载**推进**语义：纯文本回合的
// 瞬态推进指令、停滞两段式、达成 / 硬停 / 账本漂移三路收尾与自动回落前档。
// 边界：只作用于主会话 run（`goal_mode_active`）——子代理继承父档位但不得替父会话推进目标。

/// 目标模式推进指令的瞬态标记（`stream::messages_for_request` 据此识别并保留尾部瞬态）。
pub(super) const GOAL_ADVANCE_TAG: &str = "<goal-advance";

/// 目标模式的进展快照（进展信号 ② 的比对基线）。
///
/// 刻意**不含** `decisions` / `pending` / `blocked`：那是记账字段，反复追加即可躲过停滞判定
///（「我记了三条决策」不是进展）；而 text / criteria 标题 / ledger 属于**合同**，登记与修订
/// 是真进展（澄清期的主要产出正是它）。本结构是驱动层观测态，不进 `GoalState`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GoalProgressKey {
    /// 合同键（目标正文 + 验收标准标题 + 账本路径与程序）
    contract: String,
    /// 验收标准完成状态
    criteria_done: Vec<bool>,
    /// 生命周期状态
    status: GoalStatus,
}

/// 由目标状态生成进展快照。
pub(super) fn goal_progress_key(state: &GoalState) -> GoalProgressKey {
    let mut contract = state.text.clone();
    for c in &state.criteria {
        contract.push('\n');
        contract.push_str(&c.title);
    }
    for item in state
        .ledger
        .paths
        .iter()
        .chain(state.ledger.programs.iter())
    {
        contract.push('\n');
        contract.push_str(item);
    }
    GoalProgressKey {
        contract,
        criteria_done: state.criteria.iter().map(|c| c.done).collect(),
        status: state.status,
    }
}

/// 目标模式是否作用于本 run：仅主会话目标档。
/// 子代理的 `prefs.approval_mode` 会被同步成父档（B1），不加这道门，子代理会替父会话推进目标、
/// 并按目标模式的文本轮语义跑（破坏子代理的 `<report>` 收尾契约）。
pub(super) fn goal_mode_active(rt: &SessionRuntime, params: &DriveParams) -> bool {
    params.main_session && rt.prefs().approval_mode == crate::core::prefs::ApprovalMode::Goal
}

/// 当前目标阶段（无状态时按澄清期处理，与 `goal::goal_phase` 同源）。
pub(super) fn goal_phase_of(rt: &SessionRuntime) -> Option<GoalPhase> {
    let g = rt.goal_snapshot();
    goal::goal_phase(g.as_ref())
}

/// 发 `goal:update`（键名与载荷形态与 goal 工具一致：session + goal）。
fn emit_goal_update(sink: &Arc<dyn EventSink>, rt: &SessionRuntime) {
    if let Some(g) = rt.goal_snapshot() {
        sink.emit(
            &rt.id,
            "goal:update",
            serde_json::json!({ "session": rt.id, "goal": g }),
        );
    }
}

/// 档位兜底（主会话每步调用）：会话档位已不是目标档，但目标仍停在「执行中」→ 置「已暂停」
///（内存 + 边车 + `goal:update`），返回是否发生了状态迁移。
///
/// 语义与 `SessionRuntime::transition_prefs` 的「离开目标档即暂停执行中的目标」一致，
/// 覆盖那些**不经过 `set_session_prefs`** 的档位变动（如 ask 批准时用户选了非目标档、
/// 外部直改 prefs）。不补这道兜底会留下「档位非目标档 + 目标 executing + 账本闸门关闭」的
/// 静默不一致：目标自称在跑，实际既无账本保护也无推进语义。
///
/// 注意：`goal_execute_phase` 的档位门**必须保留**（`goal_phase(Paused)` 仍属执行期，
/// 去掉档位门会让完全访问档在目标暂停后仍被账本闸门限制），空窗由本函数在 step 边界收口。
pub(super) fn pause_goal_if_mode_left(core: &Arc<AgentCore>, rt: &Arc<SessionRuntime>) -> bool {
    if rt.prefs().approval_mode == crate::core::prefs::ApprovalMode::Goal {
        return false;
    }
    let Some(mut g) = rt.goal_snapshot() else {
        return false;
    };
    if g.status != GoalStatus::Executing {
        return false;
    }
    g.status = GoalStatus::Paused;
    rt.set_goal(Some(g.clone()));
    let _ = core.store.save_goal(&rt.id, &Some(g));
    emit_goal_update(&core.sink, rt);
    true
}

/// 内联列举（最多 3 项，其余折叠为「等 N 项」；空列表回「未登记」）。
fn join_inline(items: &[String]) -> String {
    if items.is_empty() {
        return "未登记".to_string();
    }
    let head: Vec<&str> = items.iter().take(3).map(|s| s.as_str()).collect();
    if head.len() == items.len() {
        head.join("、")
    } else {
        format!("{} 等 {} 项", head.join("、"), items.len())
    }
}

/// 子代理的只读目标上下文：给在跑子代理一份「总体目标 + 本包任务」摘要。
/// 子代理不持有 `goal` 工具（`SUB_BASE_EXCLUDES`），故这里只给**读**视图：目标正文、
/// 未达成验收标准、账本范围，并明确「不得改动验收合同、不得向用户提问」。
pub(super) fn render_sub_goal_context(goal: Option<&GoalState>) -> String {
    let body = match goal {
        None => "父会话处于目标模式澄清期，尚未登记目标：本次任务包按父会话下发的范围只读推进（写工具不可用），把发现写进最终汇报。".to_string(),
        Some(g) => {
            let mut s = format!("父会话总体目标：{}\n状态：{}\n", g.text, g.status.label());
            let left: Vec<&str> = g
                .criteria
                .iter()
                .filter(|c| !c.done)
                .map(|c| c.title.as_str())
                .collect();
            if left.is_empty() {
                s.push_str("未达成的验收标准：（无）\n");
            } else {
                s.push_str("未达成的验收标准：\n");
                for t in left {
                    s.push_str(&format!("- {t}\n"));
                }
            }
            s.push_str(&format!(
                "账本范围：路径 {}；程序 {}\n",
                join_inline(&g.ledger.paths),
                join_inline(&g.ledger.programs)
            ));
            s
        }
    };
    format!(
        "\n<goal-context read-only=\"true\">\n{body}\n本包任务见首条 <subagent-task> 消息。子代理不持有 goal 工具：不得改动验收合同（目标正文 / 验收标准 / 账本），只按本包任务推进；不得向用户提问，歧义写进最终汇报。\n</goal-context>"
    )
}

/// 目标模式推进指令（**瞬态**，只附在下一次请求的出网副本上、绝不写入历史）：
/// 第 N 轮 + 目标正文 + 未达成清单 + 账本摘要 + 三条硬约束。
pub(super) fn render_goal_advance(state: &GoalState, reminder: bool) -> String {
    let mut s = format!("{GOAL_ADVANCE_TAG} round=\"{}\">\n", state.rounds);
    s.push_str(
        "你在上一回合只输出了文字、没有调用工具，而目标尚未达成——本 run 继续推进（这不是收尾）。\n",
    );
    s.push_str(&format!("目标：{}\n", state.text));
    let left: Vec<&str> = state
        .criteria
        .iter()
        .filter(|c| !c.done)
        .map(|c| c.title.as_str())
        .collect();
    if left.is_empty() {
        s.push_str("未达成的验收标准：（无）——若确已全部达成，用 goal 工具把 status 置 done。\n");
    } else {
        s.push_str(&format!("未达成的验收标准（{} 项）：\n", left.len()));
        for t in left {
            s.push_str(&format!("- {t}\n"));
        }
    }
    s.push_str(&format!(
        "账本：路径 {}；程序 {}\n",
        join_inline(&state.ledger.paths),
        join_inline(&state.ledger.programs)
    ));
    s.push_str("硬约束：① 不得向用户提问（ask 工具已不可用），歧义自行判断；② 遇到未澄清的歧义按「最小惊讶 + 可回滚」自决，并把决策追加进 goal 的 decisions；③ 未达成全部验收标准前不要停下。\n");
    if reminder {
        s.push_str(&format!("注意：本 run 的纯文本回合已达上限 {GOAL_TEXT_TURN_LIMIT}，下一回合必须发起工具调用推进，或给出收尾报告。\n"));
    }
    s.push_str("</goal-advance>");
    s
}

/// 停滞提醒文案（`STALL_NUDGE_AT` 步无进展；与监督纠偏同机制——进历史，模型下一步可自查）。
fn goal_stall_nudge(streak: u32) -> String {
    format!(
        "<goal-stall-notice>目标模式：已连续 {streak} 步没有实质进展（没有非只读工具调用，验收标准也没有推进）。请立即自查并改变做法：确认是否卡在只读调研、重复读取或反复试探被拒的操作；把已完成的验收标准用 goal 工具勾选，把无法自行解决的阻塞记入 blocked 并停下报告。连续 {} 步无进展将自动暂停本目标。</goal-stall-notice>",
        goal::STALL_STOP_AT
    )
}

/// 停滞记账（执行期每步调用）：进展判定 → `stall_streak` 更新 → 两段式裁决。
///
/// 进展信号（任一为真即清零 `stall_streak`）：
/// ① 本步有非只读工具调用（`BatchDigest.has_non_readonly`，与空转看门狗同源）；
/// ② 目标实质状态相对上一步有变化（`GoalProgressKey` 本地快照比对）。
/// `Nudge` 的纠偏文案进历史（与监督纠偏同机制，模型下一步可自查）；`Stop` 由调用方收尾。
pub(super) fn goal_account_step(
    sink: &Arc<dyn EventSink>,
    rt: &Arc<SessionRuntime>,
    progressed_by_tools: bool,
    last_key: &mut Option<GoalProgressKey>,
) -> StallVerdict {
    let Some(mut g) = rt.goal_snapshot() else {
        return StallVerdict::Continue;
    };
    let key = goal_progress_key(&g);
    // 首次快照（None → Some）视为进展：不能把「刚登记目标」那一步算成停滞
    let progressed = progressed_by_tools || last_key.as_ref() != Some(&key);
    *last_key = Some(key);
    g.stall_streak = if progressed {
        0
    } else {
        g.stall_streak.saturating_add(1)
    };
    let verdict = goal::stall_verdict(g.stall_streak);
    rt.set_goal(Some(g.clone()));
    match verdict {
        StallVerdict::Continue => {}
        StallVerdict::Nudge => {
            session_log::warn(
                rt,
                &format!("目标模式停滞提醒：连续 {} 步无实质进展", g.stall_streak),
            );
            rt.history
                .lock()
                .unwrap()
                .push(Message::user_text(goal_stall_nudge(g.stall_streak)).stamped());
            emit_goal_update(sink, rt);
        }
        StallVerdict::Stop => {
            session_log::warn(
                rt,
                &format!("目标模式停滞自停：连续 {} 步无实质进展", g.stall_streak),
            );
            emit_goal_update(sink, rt);
        }
    }
    verdict
}

/// 推进轮次 +1（写回内存态 + 发 `goal:update`），返回更新后的状态。
/// 轮次只在「纯文本回合后下发推进指令」时递增：它对应一次「模型停下来又被推回」的循环，
/// 与 LLM 步数无关（步数由 `step_count` 上报）。事件在轮边界发即为节流：不每步都发。
fn bump_goal_round(sink: &Arc<dyn EventSink>, rt: &Arc<SessionRuntime>) -> Option<GoalState> {
    let mut g = rt.goal_snapshot()?;
    g.rounds = g.rounds.saturating_add(1);
    rt.set_goal(Some(g.clone()));
    emit_goal_update(sink, rt);
    Some(g)
}

/// 目标收尾成因（决定报告标题与状态流转）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GoalCloseCause {
    /// 模型通过 goal 工具声明达成（地基已校验「全部 criteria.done」）
    Done,
    /// L3 高危命令被硬拦（`rt.goal_abort` 置位）
    Blocked,
    /// 账本连续越界被拒达上限（`LEDGER_DENIAL_RETRY_LIMIT`）
    LedgerDrift,
    /// 连续多步无实质进展（`STALL_STOP_AT`）
    Stalled,
    /// 执行期纯文本回合超限（推进不动，收尾出报告）
    TextLimit,
}

impl GoalCloseCause {
    /// 收尾后的目标状态：达成保持 Done（存档保留供回看），其余一律 Paused
    ///（工作区不再有执行期授权，用户确认后可恢复执行）。
    fn status(self) -> GoalStatus {
        match self {
            GoalCloseCause::Done => GoalStatus::Done,
            _ => GoalStatus::Paused,
        }
    }

    /// 报告标题。
    fn title(self) -> &'static str {
        match self {
            GoalCloseCause::Done => "目标已达成",
            GoalCloseCause::Blocked => "目标执行被硬停（高危命令被拦）",
            GoalCloseCause::LedgerDrift => "目标执行被硬停（账本连续越界）",
            GoalCloseCause::Stalled => "目标执行被暂停（连续无实质进展）",
            GoalCloseCause::TextLimit => "目标执行被暂停（多轮只输出文字、未推进）",
        }
    }
}

/// 收尾报告正文：达成标准逐条结果 / 账本与实际改动 / 自行决策 / 待拍板项 / 被拦项。
/// 复用 `goal::render_goal_summary`（已逐条列出 [x]/[ ] 与决策/待办/阻塞分节）。
pub(super) fn render_goal_report(state: &GoalState, cause: GoalCloseCause) -> String {
    let mut s = format!(
        "【{}】\n\n{}",
        cause.title(),
        goal::render_goal_summary(state)
    );
    s.push_str(&format!(
        "\n实际改动：均落在账本授权范围内（越界请求被拒 {} 次）",
        state.ledger_denials
    ));
    if cause == GoalCloseCause::Done {
        s.push_str("\n权限模式已自动回落到进入目标模式前的档位。");
    } else {
        s.push_str("\n目标已暂停：确认后可重新发起运行恢复执行。");
    }
    s
}

/// 达成收尾后的回落档位（纯函数便于单测）：优先进入目标档前的快照，
/// 缺省走全局默认（`approval.enabled` → Plan / FullAccess）。
pub(super) fn fallback_mode(
    prev: Option<crate::core::prefs::ApprovalMode>,
    cfg: &crate::core::config::ConfigState,
) -> crate::core::prefs::ApprovalMode {
    prev.unwrap_or_else(|| crate::core::prefs::ApprovalMode::from_global(cfg.approval.enabled))
}

/// 应用回落档位（写 prefs + 清快照），返回落到的档位。
fn apply_goal_fallback(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
) -> crate::core::prefs::ApprovalMode {
    let prev = rt.goal_prev_mode();
    let mode = {
        let cfg = core.cfg.read().unwrap();
        fallback_mode(prev, &cfg)
    };
    let mut prefs = rt.prefs();
    prefs.approval_mode = mode;
    rt.set_prefs(prefs);
    rt.set_goal_prev_mode(None);
    mode
}

/// 目标收尾（达成 / 硬停 / 账本漂移 / 停滞自停 / 文本轮超限 五路共用）：
/// 1. 状态流转（Done 保持；其余 → Paused）+ 落边车 + 发 `goal:update`（用户回看）
/// 2. 收尾报告作为**助手消息**进历史（落盘可回看）+ 发一次 `DeltaText` 帧（当场可见）
/// 3. 达成时自动回落前档（`goal_prev_mode`，缺省走全局默认）并注入 `[system]` 说明
///
/// 返回报告正文（调用方作为 `final_text` 返回；主会话的展示以帧与历史为准）。
pub(super) async fn goal_close_out(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    cause: GoalCloseCause,
) -> String {
    // 五路收尾（含 Done）共用：先取消本会话在跑的子代理——目标已落 Done/Paused，
    // 残留子代理不得再按执行期账本动工作区。
    cancel_session_subagents(core, rt);
    let Some(mut g) = rt.goal_snapshot() else {
        return String::new();
    };
    let target = cause.status();
    if g.status != target {
        g.status = target;
        rt.set_goal(Some(g.clone()));
        let _ = core.store.save_goal(&rt.id, &Some(g.clone()));
        emit_goal_update(&core.sink, rt);
    }
    session_log::info(
        rt,
        &format!(
            "目标收尾（{}）：标准 {}/{} 达成，账本越界被拒 {} 次，第 {} 轮",
            cause.title(),
            g.criteria.iter().filter(|c| c.done).count(),
            g.criteria.len(),
            g.ledger_denials,
            g.rounds
        ),
    );
    let report = render_goal_report(&g, cause);
    // 助手消息进历史（落盘；wire 层合并相邻同角色消息，与上一条 assistant 相邻也合法）
    rt.history.lock().unwrap().push(
        Message {
            role: Role::Assistant,
            content: vec![Content::Text {
                text: report.clone(),
            }],
            created_at: None,
        }
        .stamped(),
    );
    // 当场可见：直接发一帧文本增量（前端并入当前助手气泡）；历史才是回看的事实源
    core.sink.channel_frame(
        &rt.id,
        &Frame::DeltaText {
            generation: rt.stream.generation(),
            text: format!("\n\n{report}"),
        },
    );
    if cause == GoalCloseCause::Done {
        let mode = apply_goal_fallback(core, rt);
        rt.history.lock().unwrap().push(
            Message::user_text(format!(
                "[system] 目标已达成，权限模式已回落到{}",
                approval_mode_label(mode)
            ))
            .stamped(),
        );
    }
    report
}

/// 每步收尾裁决（批次后调用）：达成 / 硬停 / 账本漂移 / 停滞两段式。
/// 返回 `Some(收尾报告)` = 本 run 就此收尾（调用方 `break`；收尾经 `Ok` 走 run:done，不静默）。
pub(super) async fn goal_bookkeep(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    sink: &Arc<dyn EventSink>,
    idle_digest: &BatchDigest,
    goal_key: &mut Option<GoalProgressKey>,
) -> Option<String> {
    let g = rt.goal_snapshot()?;
    // 达成：模型已用 goal 工具声明（地基校验过「全部 criteria.done」）
    if g.status == GoalStatus::Done {
        return Some(goal_close_out(core, rt, GoalCloseCause::Done).await);
    }
    // 账本漂移优先于通用硬停：越界达上限时 `ledger_gate` 会同时置 abort 标记，成因取更具体的
    // 那个（LedgerDrift）；顺手消费掉标记，避免收尾被重复触发。
    if goal::goal_phase(Some(&g)) == Some(GoalPhase::Execute)
        && g.ledger_denials >= LEDGER_DENIAL_RETRY_LIMIT
    {
        let _ = rt.take_goal_abort();
        return Some(goal_close_out(core, rt, GoalCloseCause::LedgerDrift).await);
    }
    // 硬停标记（L3 高危/灾难被硬拦）：无论阶段立即收尾
    if rt.take_goal_abort() {
        return Some(goal_close_out(core, rt, GoalCloseCause::Blocked).await);
    }
    // 停滞与账本漂移只在执行期判定：澄清期用户在场（澄清本身就是对话过程，纯只读调研
    // 不设自停门，空转看门狗照常兜底）；执行期是无人值守推进，才需要自停门。
    if goal::goal_phase(Some(&g)) != Some(GoalPhase::Execute) {
        return None;
    }
    match goal_account_step(sink, rt, idle_digest.has_non_readonly, goal_key) {
        StallVerdict::Stop => Some(goal_close_out(core, rt, GoalCloseCause::Stalled).await),
        _ => None,
    }
}

/// 子代理档位基座（B1）：spawn 时冻结一次，此后每步按父会话**实时**档位重建。
/// **重建语义（本改造的核心约束）**：排除集与 system_extra 一律从本基座 clone 重算，
/// 绝不增量追加——增量追加会让 Plan→AutoEdit 后旧的写工具排除与 `<plan-mode>` 块
/// 永久残留（子代理已能写文件却仍被告知「计划模式只读」）。
#[derive(Debug, Clone)]
pub struct SubBase {
    /// 根会话 id（= 子 rt 的 `root_session_id`，嵌套派发时仍指向主会话）：每步据此取实时父档
    pub root_session_id: Option<crate::core::types::SessionId>,
    /// 角色名（原样字符串，供 `tools::subagent::apply_role_policy` 归一查表）
    pub role: String,
    /// 步数预算（重建角色纪律块时复用）
    pub max_steps: usize,
    /// spawn 时的档位：与当前档位相同时逐字复用冻结的角色纪律块（少一次拼装）
    pub spawn_mode: crate::core::prefs::ApprovalMode,
    /// 基座排除集（spawn 冻结的内部 8 项：ask/subagent/plan/skill/scheduled_task/suggest/wait/goal）
    pub base_excludes: Vec<String>,
    /// 基座 system_extra（角色纪律块 + 角色定义）
    pub base_system_extra: String,
    /// 基座 idle_policy（只读角色 = NudgeOnly；与档位解耦，不随档位变化）
    pub idle_policy: IdlePolicy,
}

/// 按**当前父档**从基座重建子代理参数——spawn 与每步重算共用这一条装配路径
///（同源保证「spawn 时的参数」与「第一步重算后的参数」逐字一致）。
///
/// 装配顺序与既有语义一致：基座排除集 → 父档派生（Plan 档 = 写工具三件套 +
/// `service` + `scheduled_task`，另加 MCP 排除与 `<plan-mode>` 块；Goal 档 = 按阶段收紧的
/// 排除集，见 `apply_goal_mode`）→ 角色派生（只读角色在非 FullAccess 档下排除写工具，B3）。
/// 重复调用幂等：反复切档不会累积排除项，也不会残留旧档位的提示块。
///
/// `parent_goal` = 父会话目标状态快照：目标档下子代理拿一份**只读**目标上下文
/// （`render_sub_goal_context`），**替换**而非叠加父档的 `<goal-mode>` 块——后者含
/// 「用 goal 工具勾选完成」这类子代理做不到的指令（子代理不持有 goal 工具）。
pub fn subagent_drive_params(
    base: &SubBase,
    parent_prefs: &crate::core::prefs::SessionPrefs,
    parent_goal: Option<&GoalState>,
) -> DriveParams {
    // 角色纪律块：档位变了才重拼（只读提示句在 FullAccess 档下是授权说明，B3）
    let base_extra = if parent_prefs.approval_mode == base.spawn_mode {
        base.base_system_extra.clone()
    } else {
        crate::tools::subagent::base_extra(&base.role, base.max_steps, parent_prefs.approval_mode)
    };
    let mut params = DriveParams {
        exclude_tools: base.base_excludes.clone(),
        system_extra: base_extra,
        idle_policy: base.idle_policy,
        ..DriveParams::default()
    };
    let parent = main_drive_params(parent_prefs, parent_goal);
    params.exclude_tools.extend(parent.exclude_tools);
    params.exclude_mcp = params.exclude_mcp || parent.exclude_mcp;
    if parent_prefs.approval_mode == crate::core::prefs::ApprovalMode::Goal {
        // 目标档：父档 system_extra 恰是面向父会话的 <goal-mode> 块（`apply_goal_mode` 赋值语义），
        // 故此处用只读目标上下文**替换**它，而不是叠加
        params
            .system_extra
            .push_str(&render_sub_goal_context(parent_goal));
    } else if !parent.system_extra.is_empty() {
        params.system_extra.push_str(&parent.system_extra);
    }
    crate::tools::subagent::apply_role_policy(&mut params, &base.role, parent_prefs.approval_mode);
    dedup_excludes(&mut params.exclude_tools);
    params
}

/// 排除集去重（保序）：基座 ∪ 父档 ∪ 角色三路来源本有重叠（如 `scheduled_task`），
/// 去重后重建结果稳定可比，也让「反复切档长度不增」可被直接断言。
fn dedup_excludes(tools: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    tools.retain(|t| seen.insert(t.clone()));
}

/// 取子代理的**实时**父档（B1）：经根会话 id 查活跃会话表。
/// 父会话已删除 / 未注册 → `None`，调用方保持现状（优雅降级，绝不 panic）。
fn live_parent_prefs(
    core: &AgentCore,
    base: &SubBase,
    rt: &SessionRuntime,
) -> Option<crate::core::prefs::SessionPrefs> {
    let root = base
        .root_session_id
        .clone()
        .or_else(|| rt.root_session_id.clone())?;
    core.session(&root).map(|p| p.prefs())
}

/// 取子代理的**实时**父目标（与 `live_parent_prefs` 同源：经根会话 id 查活跃会话表）。
/// 父会话已删除 / 未注册 / 无目标 → `None`（子代理侧相应回落到「澄清期只读」文案）。
fn live_parent_goal(core: &AgentCore, base: &SubBase, rt: &SessionRuntime) -> Option<GoalState> {
    let root = base
        .root_session_id
        .clone()
        .or_else(|| rt.root_session_id.clone())?;
    core.session(&root).and_then(|p| p.goal_snapshot())
}

/// 子代理每步档位同步（B1，`run_tool_batch` 的重算点）：按父会话实时档位重建参数
///（工具集 / `<plan-mode>` 块 / idle 策略），并把根会话的 `approval_mode` 写进子 rt 的
/// prefs——fence（`ToolCtx::fence_policy`）与写审批门读的都是它，不同步会出现
/// 「工具集松了但 fence 还锁着」的错位。
///
/// 只同步 `approval_mode`：`model_id` / `reasoning_effort` 保持 spawn 快照（产品决策）。
/// 档位**真的变化**时才注入 `[system]` 消息并记日志——未变不动历史，避免污染历史与
/// 打穿 provider 前缀缓存。
pub(super) fn refresh_subagent_mode(
    core: &AgentCore,
    rt: &Arc<SessionRuntime>,
    params: &mut DriveParams,
) {
    let Some(base) = params.sub_base.clone() else {
        return;
    };
    let Some(parent_prefs) = live_parent_prefs(core, &base, rt) else {
        return;
    };
    // 父目标同样每步取实时快照（子代理的只读目标上下文随父会话目标推进刷新）
    let parent_goal = live_parent_goal(core, &base, rt);
    let fresh = subagent_drive_params(&base, &parent_prefs, parent_goal.as_ref());
    params.exclude_tools = fresh.exclude_tools;
    params.exclude_mcp = fresh.exclude_mcp;
    params.system_extra = fresh.system_extra;
    params.idle_policy = fresh.idle_policy;

    let prev = rt.prefs().approval_mode;
    if prev == parent_prefs.approval_mode {
        return;
    }
    let mut prefs = rt.prefs();
    prefs.approval_mode = parent_prefs.approval_mode;
    rt.set_prefs(prefs);
    rt.history.lock().unwrap().push(
        Message::user_text(format!(
            "[system] 权限模式已变更为 {}",
            approval_mode_label(parent_prefs.approval_mode)
        ))
        .stamped(),
    );
    session_log::info(
        rt,
        &format!(
            "子代理档位跟随父会话：{} → {}（工具集与 fence 已同步）",
            approval_mode_label(prev),
            approval_mode_label(parent_prefs.approval_mode)
        ),
    );
}

/// 审批档位的中文名（注入文案与日志共用，B1/B4）。
pub(super) fn approval_mode_label(mode: crate::core::prefs::ApprovalMode) -> &'static str {
    use crate::core::prefs::ApprovalMode;
    match mode {
        ApprovalMode::ConfirmEach => "逐项确认模式",
        ApprovalMode::AutoEdit => "自动编辑模式",
        ApprovalMode::Plan => "计划模式",
        ApprovalMode::FullAccess => "完全访问模式",
        ApprovalMode::Goal => "目标模式",
    }
}

/// 低预算提醒触发步：剩余预算 20% 时（`step` 从 0 计已耗步数，
/// 故落在 max_steps - max_steps / 5）。
/// max_steps < 5 时该值落在循环范围之外，提醒永不触发。
pub(super) fn budget_notice_step(max_steps: usize) -> usize {
    max_steps - max_steps / 5
}

/// 最终汇报标记：非主会话 run（子代理 / 任务运行）的纯文本回合只有在带此标记时才
/// 视为按约定收尾（[docs/subagent-text-turn-premature-exit]）。
pub(super) const REPORT_TAG: &str = "<report>";
/// `REPORT_TAG` 的闭合标记。
pub(super) const REPORT_TAG_END: &str = "</report>";
/// 非主会话 run 允许的连续「无工具调用回合」上限：超出即以显式错误终止本 run，
/// 绝不静默当作成功收尾（宁可显式失败让主代理重派，也不交付一句过程旁白）。
/// 注：仅思考（thinking-only）回合 `joined` 为空、同样计一次——「只在思考」视为无进展
/// 是保守取舍（上限 3 连，代价可控）。
pub(super) const MAX_TEXT_TURNS: u32 = 3;

/// 被拒调用（参数 JSON 不可修复）的反馈文案。两处消费：① 空 content 回合（唯一调用被拒、
/// 文本也被滤空）；② 正文非空但全部调用被拒的回合。抽为单函数防止两处文案漂移——
/// 合并集成修复前，①处直接盲重试，模型看不到拒绝原因（本提示一度为不可达死代码）。
fn tool_args_rejected_notice(n: usize) -> String {
    format!(
        "<tool-args-rejected>你上一回合有 {n} 个工具调用因参数 JSON 无法解析而被拒绝、未执行。\
         请修正参数后重新发起该调用；不要就此结束任务。</tool-args-rejected>"
    )
}

/// 被拒调用（参数 JSON 不可修复）的诊断行：工具名 + 参数长度 + 首尾摘要 + serde 错误原文。
///
/// 为什么需要（[docs/rejected-call-silent-finish]）：被拒调用的 args 既不进历史也不上 wire，
/// 会话日志此前只有「工具名 + 长度」，事后无法回答「这段 JSON 为什么不可解析」——
/// 8531 字符的 `ask` 参数即因此成为永久悬案（模型侧与用户侧都拿不到方案全文）。
fn log_rejected_calls(
    rt: &SessionRuntime,
    step: usize,
    assembled: &crate::provider::dto::Assembled,
) {
    for c in &assembled.tool_calls {
        // 可打捞的调用不在此列（只有 parse_or_salvage 判定不可修复的才是「被拒」）
        if repair::parse_or_salvage(&c.args_raw).is_some() {
            continue;
        }
        session_log::warn(
            rt,
            &format!(
                "step {step} 被拒调用 {name}：{diag}",
                name = c.name,
                diag = repair::diagnose_unparsable(&c.args_raw)
            ),
        );
    }
}

/// 无工具调用回合的处置（`text_turn_action` 的返回值）。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum TextTurnAction {
    /// 视为本 run 的自然收尾（主会话语义；或已带最终汇报标记；或目标模式文本轮超限）
    Finish,
    /// 注入提示后继续下一步（消耗步数预算）
    Continue,
    /// 目标模式执行期已达文本轮上限：注入提醒后再给一轮（下一轮仍纯文本才收尾）
    ContinueWithReminder,
    /// 连续无工具调用达上限：以显式错误终止，绝不伪装成功
    StopWithLimit,
}

/// 文本轮上限：目标模式执行期用 `GOAL_TEXT_TURN_LIMIT`（8，先提醒后收尾），
/// 其余场景保持 `MAX_TEXT_TURNS`（3，超限显式失败）——**非目标档行为逐字不变**（回归红线）。
/// 被拒调用与纯文本共用同一计数器，故上限必须同源（混用两个上限会让计数失去意义）。
pub(super) fn text_turn_limit(goal_execute: bool) -> u32 {
    if goal_execute {
        GOAL_TEXT_TURN_LIMIT
    } else {
        MAX_TEXT_TURNS
    }
}

/// 无工具调用回合（含「唯一调用被拒」的空文本回合）如何处置——纯函数便于矩阵单测。
///
/// 判定顺序（`rejected` 先于主会话语义，是有意为之，见下）：
/// 1. `rejected`（本回合有调用因参数 JSON 不可解析被拒）→ 未达上限则 `Continue`：
///    拒绝提示已注入历史，必须让模型看到后修正重发；上限由 `text_turn_limit` 兜底，
///    不无限续跑。**必须先于 `finish_on_text` 判定**：主会话「正文非空 + 全部调用被拒」
///    此前直接 `Finish`，run 静默成功、提示永不被模型看到、方案从未产出
///    （[docs/rejected-call-silent-finish]：会话 5100ea0c 的 8531 字符 `ask`）。
/// 2. `goal_execute`（目标模式执行期）→ 纯文本**不是**收尾：目标未达成前不许停下，
///    未达上限 `Continue`（调用方注入瞬态推进指令）、达上限先 `ContinueWithReminder`
///    提醒一轮、超限才 `Finish` 收尾出报告。判定排在 `finish_on_text` 之前是有意为之
///    ——主会话的 `finish_on_text` 恒为 true，否则本分支永不可达；
/// 3. `finish_on_text`（主会话）→ `Finish`：对主会话而言「无工具调用 = 回答完毕」语义不变
///    （无被拒调用、非目标档的回合逐字节不变）；
/// 4. 文本含 `<report>` 标记 → `Finish`：显式最终汇报；
/// 5. `text_turns >= MAX_TEXT_TURNS` → `StopWithLimit`：不收敛则显式失败；
/// 6. 其余 → `Continue`。
pub(super) fn text_turn_action(
    text: &str,
    finish_on_text: bool,
    rejected: bool,
    text_turns: u32,
    goal_execute: bool,
) -> TextTurnAction {
    // ① 被拒调用：提示已注入，绝不能就此收尾（主会话亦然）；上限仍生效
    if rejected {
        return if text_turns >= text_turn_limit(goal_execute) {
            TextTurnAction::StopWithLimit
        } else {
            TextTurnAction::Continue
        };
    }
    // ② 目标模式执行期：纯文本 = 推进暂停，不是收尾
    if goal_execute {
        if text.contains(REPORT_TAG) {
            return TextTurnAction::Finish;
        }
        if text_turns < GOAL_TEXT_TURN_LIMIT {
            return TextTurnAction::Continue;
        }
        if text_turns == GOAL_TEXT_TURN_LIMIT {
            return TextTurnAction::ContinueWithReminder;
        }
        return TextTurnAction::Finish;
    }
    if finish_on_text {
        return TextTurnAction::Finish;
    }
    if text.contains(REPORT_TAG) {
        return TextTurnAction::Finish;
    }
    if text_turns >= MAX_TEXT_TURNS {
        return TextTurnAction::StopWithLimit;
    }
    TextTurnAction::Continue
}

/// 剥离 `<report>…</report>` 包裹，返回（正文，是否带标记）。
///
/// 语义（有意约定，非缺陷）：无标记 → 原样（仅 trim）；标记未闭合 → 其后全部视为正文；
/// 多个标记 → 只取第一个；**闭合标记之后的文本丢弃**（标记即汇报边界）；
/// 只有闭合标记（模型写坏的常见形态）→ 视为不带标记。
/// 标记只用于收尾判定，不进入汇报正文（tool_result / 任务日志 / 卡片展示）。
pub(crate) fn split_report(raw: &str) -> (String, bool) {
    let Some(start) = raw.find(REPORT_TAG) else {
        return (raw.trim().to_string(), false);
    };
    let body_start = start + REPORT_TAG.len();
    let body = match raw[body_start..].find(REPORT_TAG_END) {
        Some(i) => &raw[body_start..body_start + i],
        None => &raw[body_start..],
    };
    (body.trim().to_string(), true)
}

/// 参数化的 agent 驱动主循环：主会话 / 子代理 / 任务运行共用（[docs/p2-plan](../../../../docs/p2-plan.md) §2.2）。
/// 返回（最终文本或错误，usage 合计，suggest 跟进项）。本函数绝不发
/// run:done——run_chat 在复位 running 后发唯一一次（[docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md) 队列回归）。
pub async fn drive_agent(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    mut params: DriveParams,
    run_id: &str,
) -> (
    Result<String, ProviderError>,
    crate::provider::RunUsage,
    Option<Vec<String>>,
) {
    let sink = core.sink.clone();
    // 每 run 重置：system 冻结与历史代际断点锚点只在单个 run 内有效
    //（新 run 重新组装 system / 重定位锚点；run 中途的文件变更在下一条用户消息生效）
    *rt.system_frozen.lock().unwrap() = None;
    *rt.cache_gen_anchor.lock().unwrap() = None;
    // 文本形态 ask 的待剥标注同样只在一个 run 内有效（[docs/text-form-ask-fallback]）：
    // 正常路径由 AskTool 打开卡片时 take 走；但若兜底命中后该调用没走到工具（例如用户在
    // 批次执行前停止），残留会成为下一轮**真实** ask 的过期标注——这里兜底清一次。
    *rt.text_ask_block.lock().unwrap() = None;
    // 父令牌存在（子代理）时派生 child_token：父取消 → 子取消；子仍可被单独停止（stop_subagent）
    let run_token = match params.parent_cancel.take() {
        Some(parent) => parent.child_token(),
        None => CancellationToken::new(),
    };
    *rt.active_cancel.lock().unwrap() = Some(run_token.clone());
    // 非主 runtime（子代理/任务运行）：文件写认领随 drive 生命周期——
    // 正常结束与 panic unwind 都经 Drop 释放（[docs/subagent-file-isolation]）
    let _claims = (!params.main_session).then(|| crate::tools::claims::ReleaseGuard::arm(&rt.id));

    // 64ms 流式刷新 ticker（无通道注册时帧被静默丢弃）
    let flush_stop = CancellationToken::new();
    tokio::spawn(stream_flush_loop(
        sink.clone(),
        rt.clone(),
        flush_stop.clone(),
    ));
    // panic unwind 兜底：停 ticker 并清 cancel token（正常路径在收尾前提前解除武装——
    // 那里必须先 sleep 让在途迭代排空，而 Drop 不能 await，故有序收尾只能留在主路径）
    let mut unwind = DriveUnwindGuard {
        rt: rt.clone(),
        flush_stop: flush_stop.clone(),
        armed: true,
    };

    let mut sanitized_once = false;
    let mut attempt: u32 = 0;
    let mut run_usage = crate::provider::RunUsage::default();
    // 本 run 的生成耗时/TTFT 观测每 run 复位（[docs/composer-token-rate](../../../../docs/composer-token-rate.md)）：
    // 子代理/任务运行也复位各自的 runtime，不会跨 run 累积。
    *rt.run_timing.lock().unwrap() = crate::core::stats::UsageTiming::default();
    let mut final_text = String::new();
    let mut outcome: Result<(), ProviderError> = Ok(());
    // suggest 路径产出的跟进建议：经返回值交给 run_chat 并入 run:done
    // （复位 running 后发唯一一次）
    let mut suggest_out: Option<Vec<String>> = None;
    // 自动压缩失败冷却：连续失败 ≥2 次后本次 run 内不再尝试，
    // 阈值持续超限时每步不再各堵一个完整超时。
    let mut compact_fail_streak: u32 = 0;
    // 运行监督（[docs/subagent-file-isolation]）：重复失败/重复调用先纠偏、不收敛则终止；
    // 另有空转看门狗（feed_batch）检测零进展只读循环——策略由 params.idle_policy 决定
    //（[docs/subagent-idle-watchdog-misfire]：只读 run 的空转层只纠偏不终止；失败重复层
    // 与步数/汇报门不受 policy 影响，照常终止）
    let mut supervision = SupervisionState::with_idle_policy(params.idle_policy);
    // 连续「无工具调用回合」计数（仅非主会话 run 消费；有工具调用或压缩成功时复位）：
    // 用于 <continue-notice> 续跑与 MAX_TEXT_TURNS 显式失败门
    //（[docs/subagent-text-turn-premature-exit]）
    let mut text_turns: u32 = 0;
    // 文本形态 ask 兜底（[docs/text-form-ask-fallback]）：每 run 至多一次——既救
    // 「端点偶发漏 tool_use」，也不给提示注入留反复重试的窗口。
    let mut text_ask_used = false;
    // 目标模式进展快照（停滞判定基线）：执行期每步比对一次，见 `goal_account_step`
    let mut goal_key: Option<GoalProgressKey> = None;

    'steps: for step in 0..params.max_steps {
        // 真实步数上报（sub:step 进度采样消费；取代 history.len() 失真口径）
        rt.step_count.store(step + 1, Ordering::SeqCst);
        // ① 取消检查
        if run_token.is_cancelled() {
            outcome = Err(ProviderError::Cancelled);
            break 'steps;
        }
        // ①’ 目标模式硬停（工具层拦下 L3 高危命令后置位）：立即收尾，不再进入下一步。
        // 正常路径由 `goal_bookkeep` 在批次后消费；此处兜住「标记在批次之外置位」的情形
        //（`take` 语义 → 两处不会重复收尾）。
        if goal_mode_active(rt, &params) && rt.take_goal_abort() {
            goal_close_out(core, rt, GoalCloseCause::Blocked).await;
            break 'steps;
        }
        // ①’’ 档位兜底（主会话每步）：档位已不是目标档但目标仍停在「执行中」→ 置「已暂停」
        //（落边车 + 发 goal:update）。覆盖不经 `set_session_prefs` 的档位变动（如 ask 批准时
        // 用户选了非目标档），避免「档位非目标档 + 目标 executing + 账本闸门关闭」的静默不一致。
        // 只对主会话生效：子代理不得替父会话改写目标状态。
        if params.main_session {
            pause_goal_if_mode_left(core, rt);
        }

        // ② 消化注入队列（主会话语义）
        {
            let mut rx_guard = rt.inject_rx.lock().unwrap();
            if let Some(rx) = rx_guard.as_mut() {
                let mut injected = 0;
                while let Ok(msg) = rx.try_recv() {
                    rt.history.lock().unwrap().push(msg.stamped());
                    injected += 1;
                    if injected >= INJECT_BUFFER {
                        break;
                    }
                }
                if injected > 0 && params.emit_events {
                    sink.emit(&rt.id, "run:inject", serde_json::json!({ "session": rt.id, "run_id": run_id, "count": injected }));
                }
            }
        }

        // ③④ 实时上下文计账（主会话）+ 阈值自动压缩
        // （失败冷却 + 压缩互斥 + 进度事件，[docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md)）
        step_auto_compact(
            core,
            rt,
            &sink,
            &params,
            &run_token,
            &mut compact_fail_streak,
            // 压缩成功即清空空转计数与无工具调用计数（历史细节被丢弃，模型需要重读/重建上下文）
            &mut || {
                supervision.reset_idle();
                text_turns = 0;
            },
        )
        .await;

        // ⑤ 预算提醒（子代理）：剩余 20% 时提醒一次
        if params.budget_notice && step == budget_notice_step(params.max_steps) {
            rt.history.lock().unwrap().push(
                Message::user_text(
                    "<budget-notice>步数预算即将耗尽，请尽快收敛并输出汇报。</budget-notice>",
                )
                .stamped(),
            );
        }
        // ⑥ 强制汇报轮：最后一步
        if params.force_report && step == params.max_steps.saturating_sub(1) {
            rt.history.lock().unwrap().push(Message::user_text(
                "<final-report>已到达步数上限。停止调用工具，立即输出最终汇报：已完成、未完成、结论。",
            ).stamped());
        }

        // ⑦ 流式请求 + 轮内重试
        let (model, mut req) = match build_stream_request(core, rt, &params).await {
            Ok(v) => v,
            Err(e) => {
                outcome = Err(ProviderError::Protocol(e));
                break 'steps;
            }
        };
        rt.stream.reset();
        // 目标模式推进指令是**一次性**的：本次请求已把它附进出网副本（`req.messages` 末尾），
        // 立即从 params 清掉——同一指令反复下发只会污染上下文与打穿前缀缓存。
        params.goal_transient = None;

        // [docs/session-logging-report](../../../../docs/session-logging-report.md) 会话级细粒度日志：verbose 记录完整请求（key 已脱敏）；
        // 每轮结果（成功/重试/失败）各占一行
        let verbose = session_log::verbose_enabled(&core.cfg.read().unwrap());
        if verbose {
            session_log::info(
                rt,
                &format!(
                    "step {step} 请求全文 {}",
                    session_log::trunc(&req.redacted_json(), VERBOSE_BODY_CAP)
                ),
            );
        }
        let step_started = std::time::Instant::now();
        // 可观测性（「卡死」排查，2026-09-09）：请求开始即落日志——此前只在完成时打一行，
        // 「模型慢慢响应」与「真挂死」在日志里无法区分
        session_log::info(
            rt,
            &format!(
                "step {step} llm 请求开始 model={} in≈{}ms",
                model.name,
                step_started.elapsed().as_millis()
            ),
        );

        let mut assembled = match run_llm_turn(
            core,
            rt,
            &sink,
            &params,
            &model,
            &mut req,
            &run_token,
            run_id,
            step,
            step_started,
            verbose,
            &mut sanitized_once,
            &mut attempt,
            &mut run_usage,
        )
        .await
        {
            Ok(asm) => asm,
            Err(e) => {
                outcome = Err(e);
                break 'steps;
            }
        };

        // ⑧ 空响应兜底（M5）：空响应且重试预算耗尽则结束 run，
        // 不让空 assistant 消息进历史
        if assembled.is_empty() {
            outcome = Err(ProviderError::Protocol(
                "模型返回空响应且重试预算已耗尽".into(),
            ));
            break 'steps;
        }

        // 诊断口径取**兜底之前**的原始响应形态：兜底会凭空补一个调用，取在之后就分不清
        // 「端点真的返回了 tool_use」与「我们补的」——而这正是这行日志要回答的问题。
        let raw_tool_calls = assembled.tool_calls.len();
        let raw_names = assembled
            .tool_calls
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let raw_text_chars = assembled_text_chars(&assembled);

        // ⑧’ 文本形态 ask 兜底（[docs/text-form-ask-fallback]）：BYOK 端点偶发把工具调用
        // 当正文透传（本回合无 tool_use，正文末尾漂着 `<ask>…</ask>` 原文）。此前这段协议
        // 原文只被当普通 Markdown 渲染——问题不弹卡、也不留痕，用户只看到裸 XML。
        // 条件门：仅主会话 + ask 确实在工具集里（覆盖目标档执行期与子代理）+ 本回合无调用
        // + 每 run 一次；命中后剥掉正文里的块并补一个等价 ask 调用，交回既有批次路径执行
        //（G2/G3 门、mode 切档、switchToAutoEdit、plan 落盘全在工具层，与调用从哪来无关）。
        if !text_ask_used && assembled.tool_calls.is_empty() && ask_available(&params) {
            if let Some(salvaged) = text_ask::salvage_text_ask(&assembled.joined_text()) {
                if strip_text_block(&mut assembled, &salvaged.block) {
                    let index = assembled.tool_calls.len();
                    assembled.tool_calls.push(AssembledToolCall {
                        index,
                        id: format!("text_ask_{run_id}_{step}"),
                        name: "ask".into(),
                        args_raw: salvaged.args.to_string(),
                    });
                    assembled.blocks.push(AsmBlock::Tool(index));
                    text_ask_used = true;
                    // 交前端剥离当轮气泡里的残留原文（流式帧已下发、无法回收）
                    *rt.text_ask_block.lock().unwrap() = Some(salvaged.block.clone());
                    // 记块首一段：恢复后原文在历史与正文里都被剥掉，不留样则事后无从判断
                    // 「什么内容触发了这张卡」（每 run 至多一条；换行压成空格保持单行日志）
                    let sample: String = salvaged
                        .block
                        .lines()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(200)
                        .collect();
                    session_log::warn(
                        rt,
                        &format!(
                            "step {step} 模型未按工具协议提问：正文里的 <ask> 块（{} 字）已恢复为 ask 调用；块首 200 字：{sample}",
                            salvaged.block.chars().count()
                        ),
                    );
                }
            }
        }

        // step 级响应形态诊断（同上）：端点把工具调用当正文透传时，「为什么模型没返回
        // tool_use」此前无从取证（原始 SSE 不落盘）。一行摘要即可区分三种可能：
        // 请求侧工具集为空 / 响应侧确实没有 tool_use / 模型把调用写成了正文（recovered=true）。
        session_log::info(
            rt,
            &format!(
                "step {step} 响应形态 tools_sent={} tool_calls={} names=[{}] text≈{}字 recovered={}",
                req.tools.len(),
                raw_tool_calls,
                raw_names,
                raw_text_chars,
                text_ask_used
            ),
        );

        // ⑨ 组装 assistant 消息
        let (assistant_msg, calls, synth_results) = build_assistant_message(&assembled);
        let joined = assembled.joined_text();
        if !joined.is_empty() {
            // clone：下方 text_turn_action 仍需读本回合文本（final_text 只保留最后一段非空文本）
            final_text = joined.clone();
        }
        // 空消息守卫（缺陷修复）：content 全被滤空（空 text/thinking 块、args 不可解析被
        // 拒的调用）的 assistant 消息一旦进历史，此后每次请求都会带上它并被判非法
        //（content 为 null 且无 tool_calls → 400 Invalid assistant message）。故不入历史。
        if assistant_msg.content.is_empty() {
            // 分支一：调用被拒（参数 JSON 不可修复）——必须让模型知道发生了什么。
            // 盲重试（原语义）会让它重复同一个坏参数：拒绝原因对它不可见；且被拒调用
            // 没有 tool_use 块，孤立 tool_result 对 API 非法，故以 user 角色提示反馈
            //（[docs/subagent-text-turn-premature-exit]）。
            if !synth_results.is_empty() {
                session_log::warn(
                    rt,
                    &format!(
                        "step {step} {} 个工具调用参数不可解析被拒绝，已反馈给模型",
                        synth_results.len()
                    ),
                );
                log_rejected_calls(rt, step, &assembled);
                rt.history.lock().unwrap().push(
                    Message::user_text(tool_args_rejected_notice(synth_results.len())).stamped(),
                );
                // 与纯文本回合共用上限：连续无进展不无限续跑，超限显式失败
                if text_turns >= MAX_TEXT_TURNS {
                    let msg = format!(
                        "连续 {} 步未发起任何有效工具调用（调用参数反复不可解析），本 run 终止；\
                         已产生的历史保留，请基于现状收尾。",
                        text_turns + 1
                    );
                    session_log::warn(rt, &msg);
                    rt.history.lock().unwrap().push(Message::user_text(
                        "<text-turn-limit>连续多步的工具调用均因参数 JSON 不可解析被拒绝，本 run 已终止。\
                         若用户重新发起运行，先用 ask 工具确认：继续（换一种方式推进）或就此收尾。</text-turn-limit>",
                    ).stamped());
                    outcome = Err(ProviderError::Protocol(msg));
                    break 'steps;
                }
                text_turns += 1;
                continue 'steps;
            }
            // 分支二：真正的空响应（无被拒调用）——按既有「空响应」语义重试一次，
            // 重试预算耗尽则终止本步。
            if retry::should_retry(&ProviderError::Server("空响应".into()), attempt) {
                attempt += 1;
                rt.stream.reset();
                session_log::warn(
                    rt,
                    &format!("step {step} 组装出的 assistant 消息为空，第 {attempt} 次重试"),
                );
                tracing::warn!(
                    "session {} step {step} 组装出的 assistant 消息为空，重试 #{attempt}",
                    rt.id
                );
                if params.emit_events {
                    emit_retry(&sink, rt, run_id, attempt);
                }
                sleep_backoff(attempt).await;
                continue 'steps;
            }
            outcome = Err(ProviderError::Protocol(
                "模型返回空响应且重试预算已耗尽".into(),
            ));
            break 'steps;
        }
        // 内容非空（空内容已在上方分支返回）——入历史
        rt.history.lock().unwrap().push(assistant_msg.stamped());

        let last_step = params.force_report && step + 1 == params.max_steps;
        if calls.is_empty() {
            // 正文非空但调用全被拒（参数 JSON 不可修复）：被拒调用不产生 tool_use 块——
            // 孤立 tool_result 对 API 非法，故同样以 user 角色提示反馈（与 <budget-notice>
            // 同机制：user 消息永远合法，wire 层合并相邻 user 消息）。
            if !synth_results.is_empty() {
                session_log::warn(
                    rt,
                    &format!(
                        "step {step} {} 个工具调用参数不可解析被拒绝，已反馈给模型",
                        synth_results.len()
                    ),
                );
                rt.history.lock().unwrap().push(
                    Message::user_text(tool_args_rejected_notice(synth_results.len())).stamped(),
                );
                log_rejected_calls(rt, step, &assembled);
            }
            // 被拒调用（参数 JSON 不可修复）：提示已注入历史，绝不能就此收尾——此前这里的
            // `text_turn_action(…, finish_on_text = true)` 直接 Finish，run 报成功而拒绝提示
            // 永不被模型看到（[docs/rejected-call-silent-finish]）。
            let rejected = !synth_results.is_empty();
            // 目标模式执行期：纯文本回合**不是**收尾（目标未达成前不许停下），判定见 `text_turn_action`。
            let goal_execute =
                goal_mode_active(rt, &params) && goal_phase_of(rt) == Some(GoalPhase::Execute);
            let action = text_turn_action(
                &joined,
                params.finish_on_text,
                rejected,
                text_turns,
                goal_execute,
            );
            let reminder = action == TextTurnAction::ContinueWithReminder;
            match action {
                TextTurnAction::Finish => {
                    // 目标档执行期走到这里 = 文本轮上限（推进不动）或模型自己以 <report> 收尾：
                    // 目标尚未达成而 run 就此结束 → 出收尾报告并落 Paused（不留「执行中」的悬空授权）
                    if goal_execute {
                        goal_close_out(core, rt, GoalCloseCause::TextLimit).await;
                    }
                    break 'steps;
                }
                TextTurnAction::Continue | TextTurnAction::ContinueWithReminder => {
                    text_turns += 1;
                    if rejected {
                        // 被拒回合：<tool-args-rejected> 已注入，不再叠加 <continue-notice>——
                        // 后者「你只输出了文字、没有发起工具调用」对被拒场景不实且与之矛盾。
                        session_log::warn(
                            rt,
                            &format!(
                                "step {step} 被拒调用回合（第 {text_turns}/{MAX_TEXT_TURNS} 次），已注入拒绝提示后继续"
                            ),
                        );
                        continue 'steps;
                    }
                    if goal_execute {
                        // 目标档执行期：递增轮次 + 下发**瞬态**推进指令（只进下一次请求的出网副本，
                        // 绝不写入历史）+ 停滞记账（本回合无工具调用 → 恒记一次无进展）
                        if let Some(g) = bump_goal_round(&sink, rt) {
                            params.goal_transient = Some(render_goal_advance(&g, reminder));
                        }
                        let verdict = goal_account_step(&sink, rt, false, &mut goal_key);
                        session_log::warn(
                            rt,
                            &format!(
                                "step {step} 目标模式纯文本回合（第 {text_turns}/{GOAL_TEXT_TURN_LIMIT} 轮），已下发推进指令（停滞 {verdict:?}）"
                            ),
                        );
                        continue 'steps;
                    }
                    session_log::warn(
                        rt,
                        &format!(
                            "step {step} 无工具调用回合（第 {text_turns}/{MAX_TEXT_TURNS} 次），注入续跑提示后继续"
                        ),
                    );
                    rt.history.lock().unwrap().push(Message::user_text(
                        "<continue-notice>你在上一回合只输出了文字、没有发起工具调用。若任务尚未完成，\
                         立即继续调用工具推进；全部完成时以 <report>…</report> 包裹输出最终汇报。</continue-notice>",
                    ).stamped());
                    // continue 跳过循环尾的 checkpoint 与空转看门狗：前者对非主会话直接
                    // return（本分支只可能在非主会话 run 命中，finish_on_text=false），
                    // 后者只按「有工具调用的批次」喂入——均为有意为之，勿挪到主会话语义。
                    continue 'steps;
                }
                TextTurnAction::StopWithLimit => {
                    // 文案按成因分流：同一句「只输出文字」用在被拒场景会把排查方向带偏
                    //（[docs/rejected-call-silent-finish] 的 8531 字符 ask 即属被拒成因）。
                    let msg = if rejected {
                        format!(
                            "连续 {} 步未发起任何有效工具调用（调用参数反复不可解析），本 run 终止；\
                             已产生的历史保留，请基于现状收尾。",
                            text_turns + 1
                        )
                    } else {
                        format!(
                            "连续 {} 步未发起工具调用（模型只输出文字、任务无进展），本 run 终止；\
                             已产生的历史保留，请基于现状收尾。",
                            text_turns + 1
                        )
                    };
                    session_log::warn(rt, &msg);
                    tracing::warn!("session {} {msg}", rt.id);
                    // 终止引导（与监督终止同模式）：下一 run 先用 ask 问用户如何处置
                    rt.history.lock().unwrap().push(Message::user_text(
                        "<text-turn-limit>连续多步未能发起有效工具调用（只输出文字，或调用参数反复不可解析），\
                         本 run 已终止。若用户重新发起运行，先用 ask 工具确认：继续（说明已准备的新推进方式）或就此收尾。</text-turn-limit>",
                    ).stamped());
                    outcome = Err(ProviderError::Protocol(msg));
                    break 'steps;
                }
            }
        }
        // 有工具调用的回合：无工具调用计数复位（续跑需重新计数）
        text_turns = 0;
        // 空转看门狗：每步一次摘要喂入（进展信号 = 非只读工具 / 首次读新文件）。
        // 只读判定按工具名集合（core 不依赖 tools 类型）；read 路径归一化后去重。
        // 在 calls 被 move 进批次执行前构造摘要。
        let idle_digest = batch_digest(&calls);

        // 混合批次（部分调用被拒 + 部分被执行）取证：被拒调用没有 tool_use 块，它那条合成
        // ToolResult 会在出网前被 repair 当孤儿结果删除（[docs/empty-assistant-and-request-rebuild-fix]
        // §7 已登记）——模型看不到拒绝原因，会话日志是唯一留痕点，而此前该路径连日志都没有
        //（code-review 2026-09-16 🟡：三个被拒分支中只有另两处调了 log_rejected_calls）。
        if !synth_results.is_empty() {
            log_rejected_calls(rt, step, &assembled);
        }

        // ⑨ 执行工具批次 → 追加结果 → 下一步
        let (batch_suggest, batch_done, call_sigs) = run_tool_batch(
            core,
            rt,
            &mut params,
            calls,
            synth_results,
            &run_token,
            run_id,
            last_step,
        )
        .await;
        if let Some(items) = batch_suggest {
            suggest_out = Some(items);
        }
        // 监督裁决（[docs/subagent-file-isolation]）：纠偏消息进历史（主会话可见、
        // 子代理经 sub:step 采样）；硬介入 → 以监督错误终止本 run（历史已含本批结果，
        // 不留悬空 tool_use）
        let mut escalated: Option<String> = None;
        for sig in call_sigs {
            match supervision.feed(sig) {
                Verdict::None => {}
                Verdict::Nudge(text) => {
                    session_log::warn(rt, "监督纠偏：注入 supervision-notice");
                    rt.history
                        .lock()
                        .unwrap()
                        .push(Message::user_text(text).stamped());
                }
                Verdict::Escalate(text) => escalated = Some(text),
            }
        }
        if let Some(text) = escalated {
            // 终止引导：监督终止注入一条历史消息，指示下一 run 先用 ask 问用户是否继续
            //（监督状态机每 run 独立、新 run 重新计数，用户答复「继续」即正常重入；
            // wire 层会合并相邻 user 消息，与 budget-notice 同模式）
            rt.history.lock().unwrap().push(Message::user_text(
                "<supervision-escalated>上一次 run 因重复失败循环未收敛而被监督终止。若用户重新发起运行，先用 ask 工具向用户确认：继续（说明已准备的新策略）或就此收尾；未经用户选择，不要再次执行与终止原因同类的操作。</supervision-escalated>".to_string(),
            ).stamped());
            outcome = Err(ProviderError::Protocol(text));
            break 'steps;
        }
        // 空转看门狗裁决（在失败重复裁决之后；纠偏同样进历史，终止同样终止本 run）
        match supervision.feed_batch(&idle_digest) {
            Verdict::None => {}
            Verdict::Nudge(text) => {
                session_log::warn(rt, "监督纠偏（空转）：注入 supervision-notice");
                rt.history
                    .lock()
                    .unwrap()
                    .push(Message::user_text(text).stamped());
            }
            Verdict::Escalate(text) => {
                session_log::warn(rt, "监督终止（空转）");
                rt.history.lock().unwrap().push(Message::user_text(
                    "<supervision-escalated>上一次 run 因连续多步无实质进展而被监督终止。若用户重新发起运行，先用 ask 工具向用户确认：继续（换一种推进方式，如直接执行/提问/换文件）或就此收尾；未经用户选择，不要继续重复读取。</supervision-escalated>".to_string(),
                ).stamped());
                outcome = Err(ProviderError::Protocol(text));
                break 'steps;
            }
        }
        // 目标模式收尾裁决（批次后）：达成 / 硬停 / 账本漂移 / 停滞两段式。
        // 放在 `batch_done` 之前：本批用 goal 工具声明达成时收尾报告必须出得来，否则 run 直接结束、
        // 目标状态悬在「执行中」。停滞检测与轮次推进只在执行阶段生效（`goal_bookkeep` 内部门）。
        if goal_mode_active(rt, &params) {
            if let Some(report) = goal_bookkeep(core, rt, &sink, &idle_digest, &mut goal_key).await
            {
                if final_text.trim().is_empty() {
                    final_text = report;
                }
                break 'steps;
            }
        }
        // M6：强制汇报轮也在工具批次完成后才结束（历史不留悬空 tool_use）
        if batch_done {
            break 'steps;
        }

        if step % CHECKPOINT_EVERY_STEPS == CHECKPOINT_EVERY_STEPS - 1 {
            let _ = checkpoint(core, rt).await;
        }
    }

    // 先停 ticker 并让在途迭代排空（至多一个节流窗口），再做最终冲刷——
    // 避免两个冲刷者乱序竞争（评审 C4）
    unwind.armed = false;
    flush_stop.cancel();
    tokio::time::sleep(Duration::from_millis(STREAM_THROTTLE_MS)).await;
    if let Some(buf) = rt.stream.take_final() {
        flush_segments(&sink, &rt.id, &buf);
    }
    *rt.active_cancel.lock().unwrap() = None;
    let result = match outcome {
        Ok(()) => Ok(final_text),
        Err(e) => Err(e),
    };
    (result, run_usage, suggest_out)
}

/// 400 文案里与 reasoning_content 回传相关的两类语义（错误文案匹配是启发式，两个方向的误判后果不对称：
/// `Rejected`→`Demanded` 方向误判只退化为今日行为——多一次 400 重试；反向误判会短暂把会话锁在
/// 「剥思考」态（要求回传的端点每轮 400），由 [`update_reasoning_sticky`] 的 `Demanded` 复位自愈）。
///
/// 为什么需要区分（[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）：
/// 同为 400，「不认这个字段」与「必须回传这个字段」的修复方向恰好相反——前者要丢掉思考，
/// 后者丢掉思考只会越修越坏（把思考抹了下次还是同一条 400）。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum Reasoning400 {
    /// 上游**拒收**该字段（不认 / 不接受）→ 本会话后续不再回传思考
    Rejected,
    /// 上游**要求**回传该字段（缺了才 400）→ 绝不能把思考数据抹掉
    Demanded,
    /// 与该字段无关
    Unrelated,
}

/// 按 400 文案分类（大小写不敏感、对 message 全文匹配）：
/// 命中 `reasoning_content` / `thinking mode` 才算相关；其中带「必须回传」语义（`passed back`）
/// 的是 `Demanded`，其余相关命中一律视为 `Rejected`。真实用例：
/// ``The `reasoning_content` in the thinking mode must be passed back to the API.`` → `Demanded`。
pub(super) fn classify_reasoning_400(msg: &str) -> Reasoning400 {
    let lower = msg.to_lowercase();
    let related = lower.contains("reasoning_content") || lower.contains("thinking mode");
    if !related {
        return Reasoning400::Unrelated;
    }
    // 「必须回传」语义：缺了它才 400，思考数据是解药而不是病根
    if lower.contains("must be passed back") || lower.contains("passed back") {
        Reasoning400::Demanded
    } else {
        Reasoning400::Rejected
    }
}

/// 依 400 分类更新会话级粘性标记，返回是否发生变化（[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）。
/// `Rejected` → 置位（此后本会话出网副本不再回传思考）；`Demanded` → **复位**（自愈阀：标记可能是误判或来自切换前的端点，
/// 继续剥思考只会让「要求回传」的端点每轮都 400）；`Unrelated` → 不变。
/// 用 swap 一次完成「读取旧值 + 写入新值」，返回值即状态是否翻转（并发调用下不会重复报告）。
pub(super) fn update_reasoning_sticky(rt: &SessionRuntime, verdict: Reasoning400) -> bool {
    match verdict {
        Reasoning400::Rejected => !rt.reasoning_rejected.swap(true, Ordering::SeqCst),
        Reasoning400::Demanded => rt.reasoning_rejected.swap(false, Ordering::SeqCst),
        Reasoning400::Unrelated => false,
    }
}

/// 单轮 LLM 流式请求 + 轮内重试/退避（drive_agent 第 ⑦ 步），自原内联循环逐字拆出：
/// 可变状态经引用穿引，行为不变——`*run_usage` 连内部重试一并累计，`*attempt`
/// 成功即复位（M3），`*sanitized_once` 在 BadRequest 时做一次性历史 sanitize/repair（M2）。
/// BadRequest 分支内还会把 400 文案经 [`classify_reasoning_400`] 分类，并交由
/// [`update_reasoning_sticky`] 更新会话级粘性标记 `rt.reasoning_rejected`
/// （[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）：
/// `Rejected` 置位（此后出网副本不再回传思考，由 `stream::messages_for_request` 消费）；
/// `Demanded` **复位**（自愈阀：误判或切端点后不至于把思考永久剥掉）；`Unrelated` 不变。
/// 历史修复：`Demanded` 用 [`repair::sanitize_keep_thinking`]（只剥图、保思考），
/// 其余情况维持 `repair::sanitize`。
/// Ok = 本轮组装结果；Err = 终止 run 的 outcome 错误（取消 / 不可重试 / 预算耗尽）。
#[allow(clippy::too_many_arguments)]
async fn run_llm_turn(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    sink: &Arc<dyn EventSink>,
    params: &DriveParams,
    model: &crate::core::config::ModelConfig,
    req: &mut crate::provider::dto::StreamRequest,
    run_token: &CancellationToken,
    run_id: &str,
    step: usize,
    step_started: std::time::Instant,
    verbose: bool,
    sanitized_once: &mut bool,
    attempt: &mut u32,
    run_usage: &mut crate::provider::RunUsage,
) -> Result<crate::provider::dto::Assembled, ProviderError> {
    // 流停滞阈值（serde default 300s；本地大上下文模型预首 token 慢可调大）
    let stall_timeout = Duration::from_secs(
        core.cfg
            .read()
            .unwrap()
            .stall_timeout_seconds
            .clamp(5, 3600),
    );
    loop {
        // 本**尝试**的计时起点（[docs/composer-token-rate](../../../../docs/composer-token-rate.md)）：
        // 必须在环内——环外的 `step_started` 含上一步的退避等待，拿它当 duration 会把重试等待算进去。
        let attempt_started = std::time::Instant::now();
        let (tx, rx) = mpsc::channel::<crate::provider::StreamDelta>(1024);
        let collector = tokio::spawn(collect_deltas(rx, rt.stream.clone(), attempt_started));
        // 多 key 池：每次尝试挑一把 key（鉴权/瞬时失败冷却并 failover）；
        // 冷却按 provider 粒度共享（[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）
        let resolved_keys = crate::host::keyring::resolve_keys(model);
        let (key_idx, key) = core
            .key_pool
            .pick(&model.provider_id, &resolved_keys)
            .map(|(i, k)| (i, Some(k)))
            .unwrap_or((0, None));
        // spawn + 停滞看门狗：流完成/报错与「无 delta 超过 stall_timeout」竞速；
        // 停滞时取消本次尝试（child token），按可重试 Server 错误走既有退避重试
        let attempt_token = run_token.child_token();
        let model_owned = model.clone();
        let client = core.client.read().unwrap().clone();
        let req_owned = req.clone();
        let token_for_task = attempt_token.clone();
        let fut = tokio::spawn(async move {
            crate::provider::stream_model(&client, &model_owned, key, req_owned, tx, token_for_task)
                .await
        });
        let watchdog = tokio::spawn(stall_watchdog(
            rt.stream.clone(),
            stall_timeout,
            attempt_token.clone(),
        ));
        let (res, stalled) = tokio::select! {
            r = fut => (r.unwrap_or_else(|join_err| {
                Err(ProviderError::Protocol(format!("流任务异常终止：{join_err}")))
            }), false),
            _ = watchdog => (Err(ProviderError::Server("流停滞".into())), true),
        };
        if !stalled {
            // 本次尝试已结束：停掉看门狗轮询任务（否则每次尝试各泄漏一个）
            attempt_token.cancel();
        }
        if stalled {
            if run_token.is_cancelled() {
                // 用户停止与停滞同帧就绪：按用户取消收尾，不误报停滞
                let _ = collector.await;
                return Err(ProviderError::Cancelled);
            }
            attempt_token.cancel();
            let _ = collector.await;
            *attempt += 1;
            rt.stream.reset(); // M4：重试前丢弃半刷残帧
            if !retry::should_retry(&ProviderError::Server("流停滞".into()), *attempt) {
                return Err(ProviderError::Server(format!(
                    "LLM 流停滞超过 {}s 且重试预算已耗尽",
                    stall_timeout.as_secs()
                )));
            }
            session_log::warn(
                rt,
                &format!(
                    "step {step} 流停滞超过 {}s，取消本次尝试（第 {} 次重试）",
                    stall_timeout.as_secs(),
                    *attempt
                ),
            );
            if params.emit_events {
                emit_retry(sink, rt, run_id, *attempt);
            }
            sleep_backoff(*attempt).await;
            continue;
        }
        match res {
            Ok(usage) => {
                core.key_pool
                    .report(&model.provider_id, &resolved_keys, key_idx, None);
                *run_usage = *run_usage + usage;
                // 上游回报的真实输入量（口径按协议分叉，见 RunUsage::prompt_total）：自动压缩
                // 触发同时看它——本地估算对图片 base64 少算 2.3 倍
                rt.last_input_tokens
                    .store(usage.prompt_total(&model.api_format), Ordering::SeqCst);
                // 收干流（= 本次生成结束）后才计耗时：duration 是「请求发出 → 流失结束」的
                // **成功尝试**窗口（退避与失败尝试不进此窗口）
                let (asm, ttft_ms) = collector.await.unwrap_or_default();
                let duration_ms = attempt_started.elapsed().as_millis() as u64;
                rt.run_timing
                    .lock()
                    .unwrap()
                    .add_step(Some(duration_ms), ttft_ms);
                if params.emit_events {
                    sink.channel_frame(
                        &rt.id,
                        &Frame::Usage {
                            input: usage.input,
                            output: usage.output,
                            cache_read: usage.cache_read,
                            cache_write: usage.cache_write,
                            duration_ms: Some(duration_ms),
                            ttft_ms,
                        },
                    );
                }
                // 空响应按可重试处理
                if asm.is_empty()
                    && retry::should_retry(&ProviderError::Server("空响应".into()), *attempt)
                {
                    *attempt += 1;
                    rt.stream.reset(); // M4：重试前丢弃半刷残帧
                    session_log::warn(rt, &format!("step {step} 空响应，第 {} 次重试", *attempt));
                    tracing::warn!("session {} step {step} 空响应，重试 #{}", rt.id, *attempt);
                    if params.emit_events {
                        emit_retry(sink, rt, run_id, *attempt);
                    }
                    sleep_backoff(*attempt).await;
                    continue;
                }
                *attempt = 0; // M3：成功即复位重试预算（按轮，不按 run）
                session_log::info(
                    rt,
                    &format!(
                        "step {step} llm 完成 model={} 耗时 {}ms tokens in={}/out={} cache r={}/w={}",
                        model.name,
                        step_started.elapsed().as_millis(),
                        usage.input,
                        usage.output,
                        usage.cache_read,
                        usage.cache_write
                    ),
                );
                if verbose {
                    let joined = asm.joined_text();
                    if !joined.is_empty() {
                        session_log::info(
                            rt,
                            &format!(
                                "step {step} 响应全文 {}",
                                session_log::trunc(&joined, VERBOSE_BODY_CAP)
                            ),
                        );
                    }
                }
                return Ok(asm);
            }
            Err(ProviderError::Cancelled) => {
                let _ = collector.await;
                return Err(ProviderError::Cancelled);
            }
            Err(e) => {
                let _ = collector.await;
                if e.is_bad_request() && !*sanitized_once {
                    *sanitized_once = true;
                    // 400 语义分类（每次请求都能自愈的关键，[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）：
                    // 「拒收 reasoning_content」的端点每次回传都会 400，故置会话级粘性标记让
                    // 后续出网副本不再回传思考（本 run 后续 step 与新 run 均生效）；
                    // 「要求回传」的端点绝不能置位（置位会让它彻底失效），修复也必须保思考。
                    let verdict = match &e {
                        ProviderError::BadRequest { message, .. } => {
                            classify_reasoning_400(message)
                        }
                        _ => Reasoning400::Unrelated,
                    };
                    // 依分类更新粘性标记；`changed` = 状态是否翻转（日志只在真正翻转时才值得记）
                    let changed = update_reasoning_sticky(rt, verdict);
                    match &verdict {
                        Reasoning400::Rejected => {
                            session_log::warn(
                                rt,
                                &format!(
                                    "step {step} 该上游拒收 reasoning_content，本会话后续请求不再回传思考：{}",
                                    session_log::trunc(&e.to_string(), ERROR_CAP)
                                ),
                            );
                        }
                        // 只在真正复位（此前置位）时记录：说明先前的置位是误判或来自切换前的端点
                        Reasoning400::Demanded if changed => {
                            session_log::warn(
                                rt,
                                &format!(
                                    "step {step} 该端点要求回传 reasoning_content，已恢复回传思考（复位本会话剥思考标记）：{}",
                                    session_log::trunc(&e.to_string(), ERROR_CAP)
                                ),
                            );
                        }
                        _ => {}
                    }
                    let mut h = rt.history.lock().unwrap();
                    // 「要求回传」场景必须保思考：把思考剥了只会把 400 修成常态。
                    // 其余（Rejected / Unrelated）维持今日语义：剥图 + 丢思考的降级阀。
                    match &verdict {
                        Reasoning400::Demanded => {
                            repair::sanitize_keep_thinking(&mut h);
                        }
                        _ => {
                            repair::sanitize(&mut h);
                        }
                    }
                    repair::repair(&mut h);
                    drop(h);
                    // 缺陷修复：历史修好了，但请求体还是构建时那份快照——必须重建，否则
                    // 重试发出的 body 与首次逐字节相同，必然复现同一个 400（会话 d9941c4b
                    // 实测：相隔 2.45s 的两条同文 400，修复从未真正生效）。
                    refresh_request_messages(rt, req, model.vision.unwrap_or(false));
                    rt.stream.reset();
                    session_log::warn(
                        rt,
                        &format!(
                            "step {step} BadRequest，sanitize/repair 历史后重试：{}",
                            session_log::trunc(&e.to_string(), ERROR_CAP)
                        ),
                    );
                    tracing::warn!(
                        "session {} step {step} BadRequest sanitize 重试：{e}",
                        rt.id
                    );
                    continue;
                }
                if retry::should_retry(&e, *attempt) {
                    core.key_pool
                        .report(&model.provider_id, &resolved_keys, key_idx, Some(&e));
                    *attempt += 1;
                    rt.stream.reset(); // M4
                    session_log::warn(
                        rt,
                        &format!(
                            "step {step} 请求失败（第 {} 次重试）：{}",
                            *attempt,
                            session_log::trunc(&e.to_string(), ERROR_CAP)
                        ),
                    );
                    tracing::warn!(
                        "session {} step {step} 请求失败，重试 #{}：{e}",
                        rt.id,
                        *attempt
                    );
                    if params.emit_events {
                        emit_retry(sink, rt, run_id, *attempt);
                    }
                    sleep_backoff(*attempt).await;
                    continue;
                }
                core.key_pool
                    .report(&model.provider_id, &resolved_keys, key_idx, Some(&e));
                session_log::error(
                    rt,
                    &format!(
                        "step {step} 请求失败（不可重试/重试预算耗尽）：{}",
                        session_log::trunc(&e.to_string(), ERROR_CAP)
                    ),
                );
                tracing::error!("session {} step {step} LLM 请求最终失败：{e}", rt.id);
                return Err(e);
            }
        }
    }
}

/// 实时上下文计账上报 + 带失败冷却的阈值自动压缩（drive_agent 第 ③④ 步），
/// 自原内联块逐字拆出（emit_events 守卫随迁）。压缩成功时回调 `on_compacted`
///（空转看门狗联动：清空已读文件集与空转计数，允许一轮无惩罚重读）。
async fn step_auto_compact(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    sink: &Arc<dyn EventSink>,
    params: &DriveParams,
    run_token: &CancellationToken,
    compact_fail_streak: &mut u32,
    on_compacted: &mut impl FnMut(),
) {
    // 只覆盖主会话（子代理 / 任务运行的 emit_events=false 在此早退，历史上就不跑自动压缩）：
    // 子代理读大图仍可能直接顶到上游上限，其上下文安全依赖调用方控制步数/范围。
    if !params.emit_events {
        return;
    }
    let bd = context::breakdown(core, rt).await;
    sink.emit(
        &rt.id,
        "tokens:update",
        serde_json::json!({ "session": rt.id, "breakdown": bd }),
    );

    // ④ 阈值自动压缩（失败冷却 + 压缩互斥 + 进度事件，[docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md)）
    let threshold = core.cfg.read().unwrap().compact_threshold.clamp(0.05, 0.95) as f64;
    // 判定同时看两路：本地估算（bd.ratio）与上游回报的真实输入占比。只看估算会漏掉
    // 「估算少算数倍」的内容（图片 base64 实测：估算 0.57 / 真实 0.99），压缩于是全程不触发，
    // 上下文一路顶到上游上限并被 400 拒绝（会话 6bca80f4）。
    let reported_ratio =
        rt.last_input_tokens.load(Ordering::SeqCst) as f64 / bd.context_window.max(1) as f64;
    // 注：该值是**上一次**请求回报的规模（本步历史又长了一点），故会滞后一步——首次带上
    // 超大图的请求仍可能先撞一次上限，下一步必然触发压缩。滞后是刻意的：本地估算与真实值
    // 各有盲区，宁可晚一步，也不能凭一次读数把用户还需要的历史压掉。
    let trigger = context::compact_trigger(bd.ratio, reported_ratio, threshold);
    if trigger != context::CompactTrigger::None && *compact_fail_streak < 2 {
        let timeout_secs = core
            .cfg
            .read()
            .unwrap()
            .compact_timeout_seconds
            .clamp(30, 3600);
        let messages = lock_ok(&rt.history).len();
        session_log::info(
            rt,
            &format!(
                "自动压缩触发 ratio={:.2} / threshold={threshold:.2} tokens={} reported_ratio={reported_ratio:.2} source={trigger:?} timeout={timeout_secs}s",
                bd.ratio, bd.total_tokens
            ),
        );
        sink.emit(
            &rt.id,
            "run:compacting",
            serde_json::json!({ "session": rt.id, "tokens_before": bd.total_tokens, "messages": messages, "timeout_ms": timeout_secs * 1000 }),
        );
        // RAII 槽位：compact_history panic unwind 时 Drop 仍会复位 compacting；
        // 否则 start_chat 永远拒绝新 run（会话变砖，[docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md) 评审修复）
        let compact_result = match CompactingGuard::acquire(rt) {
            Some(_compact_guard) => context::compact_history(core, rt, false, run_token).await,
            _ => {
                // 理论不可达（run 外手动压缩已被 running 阻断）；
                // 按失败处理并冷却，继续本步
                session_log::warn(rt, "自动压缩跳过：压缩互斥被占位");
                Err("压缩互斥被占位".into())
            }
        };
        match compact_result {
            Ok(usage) => {
                *compact_fail_streak = 0;
                // 空转看门狗联动：压缩丢细节后模型需要重读文件重建上下文——
                // 清空已读文件集与空转计数，允许一轮无惩罚重读
                on_compacted();
                session_log::info(
                    rt,
                    &format!(
                        "自动压缩完成（压缩调用 tokens in={}/out={}）",
                        usage.input, usage.output
                    ),
                );
                sink.emit(
                    &rt.id,
                    "run:compacted",
                    serde_json::json!({ "session": rt.id, "tokens_before": bd.total_tokens }),
                );
            }
            Err(e) => {
                *compact_fail_streak += 1;
                if *compact_fail_streak >= 2 {
                    session_log::warn(
                        rt,
                        &format!(
                            "自动压缩连续失败 {} 次，本 run 内不再自动压缩：{e}",
                            *compact_fail_streak
                        ),
                    );
                } else {
                    session_log::warn(rt, &format!("自动压缩失败（继续原历史）：{e}"));
                }
                tracing::warn!("自动压缩失败（继续原历史）：{e}");
                sink.emit(
                    &rt.id,
                    "run:compact_failed",
                    serde_json::json!({ "session": rt.id, "error": e }),
                );
            }
        }
    }
}

/// 执行本步工具批次并接续后续流程（drive_agent 第 ⑨ 步），自原内联块逐字拆出：
/// 结果追加进历史、注入计划批准指令、实时重算主会话参数（切档下一步生效）、
/// 再决定流程走向。返回（suggest 项，本批后是否结束 run，调用监督摘要）——`last_step`
/// 优先于 suggest，与原 break 顺序一致。
#[allow(clippy::too_many_arguments)]
async fn run_tool_batch(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    params: &mut DriveParams,
    calls: Vec<NormalizedCall>,
    synth_results: Vec<crate::core::types::Content>,
    run_token: &CancellationToken,
    run_id: &str,
    last_step: bool,
) -> (Option<Vec<String>>, bool, Vec<CallSig>) {
    let mut results = synth_results;
    let batch_out = execute_batch(
        core,
        rt,
        calls,
        &params.exclude_tools,
        params.exclude_mcp,
        params.main_session,
        run_token.clone(),
        run_id,
    )
    .await;
    results.extend(batch_out.results);
    rt.history
        .lock()
        .unwrap()
        .push(Message::tool_results(results).stamped());

    // 主会话：计划经 ask 批准 → 切档已发生（ask 工具内部）；注入执行指令。
    // 下一步的 ⑦ 会重算 system prompt 与工具集，解除计划限制。
    // 文案按**实际落到的档位**渲染（B4）：批准门可能切到自动编辑档，也可能切到完全访问档
    //（[docs/mode-gate-and-subagent-sync]），写死「自动编辑模式」会与事实不符。
    if batch_out.plan_approved && params.emit_events {
        rt.history.lock().unwrap().push(
            Message::user_text(format!(
                "[system] 方案已获批准，会话已切换到{}。请立即按方案开始执行，无需再次询问。",
                approval_mode_label(rt.prefs().approval_mode)
            ))
            .stamped(),
        );
    }

    // 重算三分支（B1）：主会话按当前偏好重算 / 子代理按父会话实时档位重建 / 任务运行保持冻结。
    // main_drive_params 内部已置 main_session = true，无需再显式赋值
    if params.main_session {
        *params = main_drive_params(&rt.prefs(), rt.goal_snapshot().as_ref());
    } else if params.sub_base.is_some() {
        refresh_subagent_mode(core, rt, params);
    }

    // M6：强制汇报轮也在工具批次完成后才结束（历史不留悬空 tool_use）
    if last_step {
        return (None, true, batch_out.call_summary);
    }

    // suggest 执行成功 → run 收尾（[docs/p1-plan](../../../../docs/p1-plan.md) §2.5）。此处不发 run:done：
    // 本函数返回后 run_chat 还要复位 running（提前发 done 会让前端出队后 startChat
    // 撞上未复位窗口，报「该会话已有运行中的任务」并丢弃队列项）——建议经
    // 返回值交给 run_chat 并入唯一一次 done。
    if let Some(items) = batch_out.suggest_items {
        return (Some(items), true, batch_out.call_summary);
    }
    (None, false, batch_out.call_summary)
}

/// 任务运行执行（[docs/p2-plan](../../../../docs/p2-plan.md) §3）：隔离上下文 + 30 步预算 + 无 MCP。
/// 档位由 `core::scheduler` 在创建 runtime 时显式置 FullAccess（无人值守需写文件与执行命令，
/// 且审批弹窗无人应答）；本参数集保持冻结，不参与每步重算。
/// 返回（结果，usage）——usage 由调用方记入统计（L10）与任务日志（B7）。
pub async fn run_task_agent(
    core: Arc<AgentCore>,
    rt: Arc<SessionRuntime>,
    task: crate::core::scheduler::ScheduledTask,
) -> (Result<String, String>, crate::provider::RunUsage) {
    let params = DriveParams {
        max_steps: crate::core::scheduler::TASK_BUDGET_STEPS,
        exclude_mcp: true,
        exclude_tools: vec![
            "ask".into(),
            "subagent".into(),
            "scheduled_task".into(),
            "suggest".into(),
            "wait".into(),
        ],
        system_extra: format!(
            "\n<task-run>你正在执行计划任务「{}」。完成后用简短汇报结束，不要向用户提问；最终汇报必须用 <report>…</report> 包裹（未包裹的文字不会被当作任务结果）。指令：{}</task-run>",
            task.name, task.instruction
        ),
        budget_notice: true,
        emit_events: false,
        force_report: true,
        finish_on_text: false,
        main_session: false,
        parent_cancel: None,
        // 任务运行按默认 Stop（与引入 idle_policy 前逐字一致）
        idle_policy: IdlePolicy::default(),
        // 任务运行不参与每步档位重算（sub_base 仅在子代理 spawn 时置位）
        sub_base: None,
        // 任务运行无目标模式推进语义（目标档位只在主会话生效）
        goal_transient: None,
    };
    // 指令成为首条用户消息（隔离 runtime 无既有历史）
    rt.history
        .lock()
        .unwrap()
        .push(Message::user_text(task.instruction.clone()).stamped());
    let run_id = format!("task_{}", task.id);
    let (result, usage, _) = drive_agent(&core, &rt, params, &run_id).await;
    // 任务结果剥离 <report> 包裹（标记只用于收尾判定，不进入任务日志与通知）
    (
        result
            .map(|r| split_report(&r).0)
            .map_err(|e| e.to_string()),
        usage,
    )
}

/// 按尝试次数取退避间隔并休眠（attempt 从 1 起，与 retry 层约定一致）。
async fn sleep_backoff(attempt: u32) {
    tokio::time::sleep(retry::delay_for_attempt(attempt.saturating_sub(1))).await;
}

/// 空转看门狗的批次摘要：从批次调用列表提取非只读标志与 read 路径（workspace 归一化后）。
fn batch_digest(calls: &[NormalizedCall]) -> BatchDigest {
    let mut d = BatchDigest::default();
    for c in calls {
        let goal_read = c.name == "goal"
            && c.args
                .as_object()
                .is_some_and(|fields| fields.values().all(serde_json::Value::is_null));
        let readonly = goal_read || super::supervise::READONLY_TOOLS.contains(&c.name.as_str());
        if !readonly {
            d.has_non_readonly = true;
        }
        if c.name == "read" || c.name == "batch_read" {
            // `read` 与兼容别名 `batch_read` 共用**同一 wire 契约**：`{"files":[{"path","startLine","endLine"}]}`
            //——`tools/read.rs` 的 `Args.files` 即 schema 的 `required: ["files"]`，而
            //`tools/batch_read.rs` 直接复用 `ReadTool.schema()`（两者入参逐字相同）。
            //
            // 历史缺陷（本批修复，[docs/subagent-idle-watchdog-misfire]）：此两分支曾按工具名把入参键
            // **写反**——`read` 分支取顶层 `args["path"]`（对真实契约恒为 `None`）而 `batch_read` 分支
            // 取 `files` 数组。于是 `read_paths` 对真实 read 调用**恒空**，`feed_batch` 的进展信号 2
            //（首次读新文件）从未生效：只读 run 每步只读都被计为空转，第 14 步被
            //`IdlePolicy::Stop` 硬终止（真实事故：explore 子代理被杀）。
            if let Some(files) = c.args["files"].as_array() {
                for f in files {
                    if f["path"].as_str().is_some() {
                        d.read_paths.push(normalize_read_path(f));
                    }
                }
            } else if c.args["path"].as_str().is_some() {
                // 防御性兼容：顶层单 `path` 旧形态（历史会话留下的调用）仍算进展，
                // 归一化语义与 files 分支完全一致（同一函数）。
                d.read_paths.push(normalize_read_path(&c.args));
            }
        }
    }
    d
}

/// read 路径归一化：trim + 统一分隔符，保证同一文件不同写法落到同一 key。
fn normalize_read_path(args: &serde_json::Value) -> String {
    args["path"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_start_matches("./")
        .replace('\\', "/")
        .to_string()
}

/// 停滞判定（纯函数便于单测）：距最后活动超过 timeout_ms 即停滞（时钟回拨时
/// saturating_sub 保护为 0 → 不停滞）。
fn stalled(now_ms: u64, last_ms: u64, timeout_ms: u64) -> bool {
    now_ms.saturating_sub(last_ms) >= timeout_ms
}

/// 流停滞看门狗：轮询 stream 的最后活动时间，超过 `timeout` 无任何 delta 即返回
///（select 据此取消本次尝试）。token 被取消（含用户停止级联）时立即退出。
async fn stall_watchdog(
    stream: Arc<crate::util::throttle::ThrottledStream>,
    timeout: Duration,
    token: CancellationToken,
) {
    let timeout_ms = timeout.as_millis() as u64;
    loop {
        if token.is_cancelled() {
            return;
        }
        if stalled(
            crate::util::throttle::now_ms(),
            stream.last_activity_ms(),
            timeout_ms,
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 从 panic payload（&str / String / 其他）提取可读消息。
fn panic_msg(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NormalizedCall, Reasoning400, batch_digest, classify_reasoning_400, stalled,
        update_reasoning_sticky,
    };
    use crate::core::sessions::SaveReport;

    /// 构造归一化工具调用（`batch_digest` 单测用）。
    fn call(name: &str, args: serde_json::Value) -> NormalizedCall {
        NormalizedCall {
            id: format!("c-{name}"),
            name: name.into(),
            args,
            index: 0,
        }
    }

    #[test]
    fn stalled_boundary_and_clock_skew() {
        assert!(stalled(1500, 1000, 500));
        assert!(!stalled(1499, 1000, 500));
        // 时钟回拨：saturating_sub 保护为 0 → 不停滞
        assert!(!stalled(0, 5000, 500));
    }

    // 真实用例：本产品用户遇到的原文——上游「要求」回传历史思考，缺了就 400。
    // 这一例必须不被误判为 Rejected，否则粘性标记会把该端点的刚需字段永久抹掉。
    #[test]
    fn classify_reasoning_400_demanded_when_must_be_passed_back() {
        assert_eq!(
            classify_reasoning_400(
                "The `reasoning_content` in the thinking mode must be passed back to the API. (HTTP 400)"
            ),
            Reasoning400::Demanded
        );
    }

    // 拒收用例：上游不认该字段（OpenAI 兼容中转常见形态）→ 置粘性标记，后续不再回传思考
    #[test]
    fn classify_reasoning_400_rejected_when_field_unrecognized() {
        assert_eq!(
            classify_reasoning_400(
                "Unrecognized request argument supplied: reasoning_content (HTTP 400)"
            ),
            Reasoning400::Rejected
        );
        assert_eq!(
            classify_reasoning_400("reasoning_content is not accepted by this model"),
            Reasoning400::Rejected
        );
    }

    // 无关用例：与思考字段无关的 400 不得置粘性标记，修复走原降级阀
    #[test]
    fn classify_reasoning_400_unrelated_for_other_400s() {
        assert_eq!(
            classify_reasoning_400(
                "Invalid assistant message: content or tool_calls must be set (HTTP 400)"
            ),
            Reasoning400::Unrelated
        );
        assert_eq!(classify_reasoning_400(""), Reasoning400::Unrelated);
    }

    // 大小写不敏感 + 无 HTTP 后缀变体（上游文案形态不受控）
    #[test]
    fn classify_reasoning_400_is_case_insensitive() {
        assert_eq!(
            classify_reasoning_400("REASONING_CONTENT IN THE THINKING MODE MUST BE PASSED BACK"),
            Reasoning400::Demanded
        );
        assert_eq!(
            classify_reasoning_400("Thinking Mode: reasoning_content invalid"),
            Reasoning400::Rejected
        );
    }

    // 粘性标记三分支：Rejected 置位、Demanded 复位（自愈阀）、Unrelated 不变；
    // 返回值 = 状态是否翻转（调用方据此只在真正翻转时记日志）。
    #[test]
    fn update_reasoning_sticky_sets_resets_and_keeps() {
        use crate::core::agent::runtime::SessionRuntime;
        use std::sync::atomic::Ordering;
        let rt = SessionRuntime::new(
            "s-sticky-unit".into(),
            std::env::temp_dir(),
            std::env::temp_dir(),
        );

        // 未置位态：Unrelated 不变、也不报告变化
        assert!(!rt.reasoning_rejected.load(Ordering::SeqCst));
        assert!(!update_reasoning_sticky(&rt, Reasoning400::Unrelated));
        assert!(!rt.reasoning_rejected.load(Ordering::SeqCst));

        // Rejected → 置位且返回 true
        assert!(update_reasoning_sticky(&rt, Reasoning400::Rejected));
        assert!(rt.reasoning_rejected.load(Ordering::SeqCst));
        // 已置位态：Unrelated 不变；Rejected 幂等（保持 true，第二次不再报告变化）
        assert!(!update_reasoning_sticky(&rt, Reasoning400::Unrelated));
        assert!(rt.reasoning_rejected.load(Ordering::SeqCst));
        assert!(!update_reasoning_sticky(&rt, Reasoning400::Rejected));
        assert!(rt.reasoning_rejected.load(Ordering::SeqCst));

        // Demanded → 复位（先显式造出置位态，断言变为 false 且返回 true）
        rt.reasoning_rejected.store(true, Ordering::SeqCst);
        assert!(update_reasoning_sticky(&rt, Reasoning400::Demanded));
        assert!(!rt.reasoning_rejected.load(Ordering::SeqCst));
        // 已复位态：Demanded 幂等（不再报告变化）
        assert!(!update_reasoning_sticky(&rt, Reasoning400::Demanded));
        assert!(!rt.reasoning_rejected.load(Ordering::SeqCst));
    }

    // 回归叙事（复审遗留 🟡）：误判置位 → 锁死症状 → Demanded 复位 → 下一个请求重新带上思考。
    // 「误判」直接用 store(true) 模拟（真实链路 = 要求回传的 400 文案变体不含 `passed back` 子串，
    // 被判成 Rejected）；「自愈」走真实分类函数 + 真实出网副本构造。
    #[test]
    fn sticky_mark_self_heals_when_reasoning_is_demanded() {
        use crate::core::agent::runtime::SessionRuntime;
        use crate::core::agent::stream::messages_for_request;
        use crate::core::types::{Content, Message, Role};
        use std::sync::atomic::Ordering;
        fn has_thinking(msgs: &[Message]) -> bool {
            msgs.iter()
                .flat_map(|m| m.content.iter())
                .any(|c| matches!(c, Content::Thinking { .. }))
        }
        let rt = SessionRuntime::new(
            "s-sticky-heal".into(),
            std::env::temp_dir(),
            std::env::temp_dir(),
        );
        let history = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking { text: "想".into() },
                    Content::Text {
                        text: "答案".into(),
                    },
                ],
                created_at: None,
            },
        ];
        *rt.history.lock().unwrap() = history.clone();

        // 误判置位：此后每个请求的出网副本都被剥掉思考（锁死期间的症状）
        rt.reasoning_rejected.store(true, Ordering::SeqCst);
        assert!(
            !has_thinking(&messages_for_request(&rt, false, None)),
            "粘性置位后出网副本必须无思考（锁死症状：要求回传的端点每轮 400）"
        );

        // 自愈：该端点的 400 被判为 Demanded → 标记复位
        let verdict = classify_reasoning_400(
            "The `reasoning_content` in the thinking mode must be passed back to the API. (HTTP 400)",
        );
        assert_eq!(verdict, Reasoning400::Demanded);
        assert!(
            update_reasoning_sticky(&rt, verdict),
            "复位必须报告状态变化"
        );
        assert!(
            !rt.reasoning_rejected.load(Ordering::SeqCst),
            "Demanded 必须复位粘性标记"
        );

        // 复位后的下一个请求重新带上思考；转录全程未被改写
        assert!(
            has_thinking(&messages_for_request(&rt, false, None)),
            "复位后下一个请求必须重新回传思考（要求回传的端点靠它拿数据）"
        );
        assert_eq!(
            rt.history.lock().unwrap().as_slice(),
            history.as_slice(),
            "全程不得改写 rt.history"
        );
    }

    // ---- run:done 的 history_save 载荷（保存不干净时的当场上报）----

    /// 干净保存 = 零打扰：不带 history_save 键。
    #[test]
    fn history_save_key_absent_when_save_is_clean() {
        let clean = SaveReport {
            saved: true,
            stripped_images: 0,
            dropped_rounds: 0,
            bytes: 4096,
            history_status: None,
        };
        let saved: Option<SaveReport> = Some(clean);
        assert!(
            saved.filter(|r| !r.is_clean()).is_none(),
            "干净保存不得进入载荷（前端不 push 提示）"
        );
        // None（非主会话）同样不带
        assert!((None::<SaveReport>).filter(|r| !r.is_clean()).is_none());
    }

    /// 不干净保存 = 带上 history_save，字段名与前端契约一致。
    #[test]
    fn history_save_payload_shape_when_degraded() {
        let degraded = SaveReport {
            saved: true,
            stripped_images: 2,
            dropped_rounds: 3,
            bytes: 1024,
            // P4 新增字段：缺省不序列化（干净 / 旧形态的载荷仍是 4 个键）
            history_status: None,
        };
        let r = Some(degraded)
            .filter(|r| !r.is_clean())
            .expect("降级保存必须进载荷");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["saved"], serde_json::json!(true));
        assert_eq!(v["stripped_images"], serde_json::json!(2));
        assert_eq!(v["dropped_rounds"], serde_json::json!(3));
        assert_eq!(v["bytes"], serde_json::json!(1024));
        assert_eq!(
            v.as_object().map(|o| o.len()),
            Some(4),
            "字段形状固定为 4 个键（前端已按此实现）"
        );
    }

    /// P4：体积状态随载荷一起上 wire（键名 `history_status`，形状与索引侧同源），
    /// 且**带体积与阈值**——前端据此格式化 MB / GB 并说清「多大 / 限到多少」。
    #[test]
    fn history_save_payload_carries_size_status() {
        use crate::core::sessions::HistoryStatus;
        let at = "2026-09-24T00:00:00+00:00".to_string();

        // 软告警：saved 仍为 true，但必须进载荷（否则前端永远看不到提示）
        let warned = SaveReport {
            saved: true,
            stripped_images: 0,
            dropped_rounds: 0,
            bytes: 300 * 1024 * 1024,
            history_status: Some(HistoryStatus::Warned {
                bytes: 300 * 1024 * 1024,
                threshold: 200 * 1024 * 1024,
                at: at.clone(),
            }),
        };
        let r = Some(warned)
            .filter(|r| !r.is_clean())
            .expect("软告警必须进载荷");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["history_status"]["kind"], serde_json::json!("warned"));
        assert_eq!(
            v["history_status"]["bytes"],
            serde_json::json!(300u64 * 1024 * 1024)
        );
        assert_eq!(
            v["history_status"]["threshold"],
            serde_json::json!(200u64 * 1024 * 1024)
        );

        // 硬熔断：同样进载荷，`saved` 仍是 true——熔断是「停写」而不是「保存失败」
        let fused = SaveReport {
            saved: true,
            stripped_images: 0,
            dropped_rounds: 0,
            bytes: 1024 * 1024 * 1024,
            history_status: Some(HistoryStatus::Fused {
                bytes: 1024 * 1024 * 1024,
                threshold: 1024 * 1024 * 1024,
                at,
            }),
        };
        let r = Some(fused)
            .filter(|r| !r.is_clean())
            .expect("熔断必须进载荷");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["saved"], serde_json::json!(true), "熔断不是保存失败");
        assert_eq!(v["history_status"]["kind"], serde_json::json!("fused"));
    }

    /// 拒存（saved=false）同样算不干净——前端据此提示「历史未完整保存」。
    #[test]
    fn history_save_payload_includes_rejected() {
        let r = Some(SaveReport::rejected())
            .filter(|r| !r.is_clean())
            .expect("拒存必须进载荷");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["saved"], serde_json::json!(false));
        assert_eq!(v["bytes"], serde_json::json!(0));
    }

    // ---- 空转看门狗的批次摘要（[docs/subagent-idle-watchdog-misfire]）----

    /// `read` 的 wire 契约是 `{"files":[{"path":…}]}`（tools/read.rs 的 `Args::files` 即 schema
    /// 的 `required: ["files"]`）——批次摘要必须从中收集路径。
    /// 修复前该分支按顶层 `path` 取值（对 read 恒为 `None`）→ read_paths 恒空 →
    ///「首次读新文件即进展」信号从未生效（只读 run 第 14 步被空转看门狗硬终止的根因）。
    #[test]
    fn batch_digest_counts_read_files() {
        let d = batch_digest(&[call(
            "read",
            serde_json::json!({"files": [{"path": "a.ts"}, {"path": "b.ts"}]}),
        )]);
        assert_eq!(
            d.read_paths,
            vec!["a.ts".to_string(), "b.ts".to_string()],
            "read 必须按 files 数组收集路径（修复前恒空）"
        );
        assert!(!d.has_non_readonly, "read 是只读工具");
    }

    /// 兼容别名 `batch_read` 与 `read` 共用同一 wire 契约（tools/batch_read.rs 直接复用 read 的
    /// schema）——两个工具名必须走同一解析分支，钉住一致性防再次写反。
    #[test]
    fn batch_digest_counts_batch_read_alias() {
        let d = batch_digest(&[call(
            "batch_read",
            serde_json::json!({"files": [{"path": "a.ts"}, {"path": "b.ts"}]}),
        )]);
        assert_eq!(
            d.read_paths,
            vec!["a.ts".to_string(), "b.ts".to_string()],
            "batch_read 与 read 必须收集到同一组路径"
        );
        assert!(!d.has_non_readonly, "batch_read 是只读工具");
    }

    /// 防御性兼容：顶层单 `path` 形态（旧形态/非常规调用）仍应被收集，
    /// 且归一化语义不变（trim + 去 `./` 前缀 + 反斜杠转正斜杠，顺序与
    /// `normalize_read_path` 逐字一致——先 trim_start_matches 再转分隔符）。
    #[test]
    fn batch_digest_path_fallback() {
        let d = batch_digest(&[call("read", serde_json::json!({"path": "  ./src\\a.ts  "}))]);
        assert_eq!(d.read_paths, vec!["src/a.ts".to_string()]);
    }

    /// 非 read 的只读调用（如 grep）不产生路径：其无进展信号必须留给空转计数器，
    /// 且不得被误判为「有副作用」的进展。
    #[test]
    fn batch_digest_grep_only_yields_no_paths() {
        let d = batch_digest(&[call("grep", serde_json::json!({"pattern": "fn main"}))]);
        assert!(d.read_paths.is_empty(), "grep 不产生 read 路径");
        assert!(!d.has_non_readonly, "grep 是只读工具");
    }

    /// 非只读工具（写/命令等）置 `has_non_readonly`——空转看门狗的首要进展信号。
    #[test]
    fn batch_digest_non_readonly_flag() {
        let edit = batch_digest(&[call(
            "edit",
            serde_json::json!({"files": [{"path": "a.ts"}]}),
        )]);
        assert!(edit.has_non_readonly, "edit 是写工具");
        assert!(edit.read_paths.is_empty(), "edit 不得计入 read 路径");

        let command = batch_digest(&[call("command", serde_json::json!({"command": "ls"}))]);
        assert!(command.has_non_readonly, "command 走进展信号 1");
        assert!(command.read_paths.is_empty());
    }

    #[test]
    fn batch_digest_does_not_count_document_or_goal_reads_as_progress() {
        for (name, args) in [
            ("read_document", serde_json::json!({"path": "report.pdf"})),
            ("goal", serde_json::json!({})),
        ] {
            let digest = batch_digest(&[call(name, args)]);
            assert!(!digest.has_non_readonly, "{name} 只读调用不得重置停滞计数");
        }
        assert!(
            batch_digest(&[call("goal", serde_json::json!({"decisions": ["x"]}))]).has_non_readonly
        );
    }
}

/// 发 run:retry 事件（带 gen 代数与退避时延，前端据此丢弃旧帧并等待）。
pub(super) fn emit_retry(
    sink: &Arc<dyn EventSink>,
    rt: &SessionRuntime,
    run_id: &str,
    attempt: u32,
) {
    sink.emit(
        &rt.id,
        "run:retry",
        serde_json::json!({
            "session": rt.id, "run_id": run_id, "attempt": attempt,
            "gen": rt.stream.generation(),
            "delay_ms": retry::delay_for_attempt(attempt.saturating_sub(1)).as_millis() as u64,
        }),
    );
}

/// 取消某会话在跑的全部子代理（按父会话过滤后逐个 `cancel_active`）。
///
/// `core.subs` 是**全进程注册表**（key = sub_id），必须过滤父会话，否则会误杀其它会话的
/// 子代理。归属字段用 `root_session_id`（`SessionRuntime::new_sub` 写入「父的归属 ?? 父 id」，
/// 嵌套派发的子代理同样指向根会话）。
///
/// 为什么需要显式取消：`cancel_run` 只取消主会话的 token，而目标收尾（达成 / 硬停 /
/// 账本漂移 / 停滞 / 文本轮超限）根本不取消 token——run 只是跳出主循环。不显式收口就会出现
/// 「界面已显示已停止，子代理仍在写文件、跑命令」（目标模式下更糟：目标已 Paused/Done，
/// 子代理仍按执行期账本动工作区）。
///
/// 先收集再取消（不在 DashMap 迭代中持分片锁做副作用）：无子代理时是纯 no-op。
pub(super) fn cancel_session_subagents(core: &Arc<AgentCore>, rt: &Arc<SessionRuntime>) {
    let targets: Vec<Arc<SessionRuntime>> = core
        .subs
        .iter()
        .filter(|e| e.value().root_session_id.as_deref() == Some(rt.id.as_str()))
        .map(|e| e.value().clone())
        .collect();
    for sub in &targets {
        sub.cancel_active();
    }
    if !targets.is_empty() {
        session_log::info(
            rt,
            &format!("已取消本会话在跑的子代理 {} 个", targets.len()),
        );
    }
}

/// 取消收尾：历史落取消标记 + 检查点 + run:cancelled 事件。
/// 目标模式例外一条：用户取消 = 执行期授权收回，`Executing` → `Paused` 并发一次 `goal:update`
///（复用既有取消链路：不新增事件键、`<run-cancelled/>` 语义与顺序不变）。
/// 用户停止 = 本会话整条执行链停下：先取消在跑的子代理（否则界面已显示「已停止」
/// 而子代理仍在写文件、跑命令），再走既有取消收尾。
pub(super) async fn mark_cancelled(core: &Arc<AgentCore>, rt: &Arc<SessionRuntime>, run_id: &str) {
    cancel_session_subagents(core, rt);
    lock_ok(&rt.history).push(Message::user_text("<run-cancelled/>").stamped());
    if let Some(mut g) = rt.goal_snapshot() {
        if g.status == GoalStatus::Executing {
            g.status = GoalStatus::Paused;
            rt.set_goal(Some(g.clone()));
            let _ = core.store.save_goal(&rt.id, &Some(g));
            emit_goal_update(&core.sink, rt);
        }
    }
    let _ = checkpoint(core, rt).await;
    core.sink.emit(
        &rt.id,
        "run:cancelled",
        serde_json::json!({ "session": rt.id, "run_id": run_id }),
    );
}

/// 检查点保存（run 结束/取消/每 N 步）。返回本次保存结果：
/// `None` = 非主会话（子代理 / 任务运行，本就不落主索引）；
/// `Some(report)` = 尝试过保存（含 `SaveReport::rejected()` = 失败）。
/// 调用方按需把它并入事件载荷向用户上报（run 成功路径）。
pub(super) async fn checkpoint(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
) -> Option<SaveReport> {
    // H5：会话已删除（如 delete_project 级联）——迟到收尾不得回写索引复活幽灵会话
    if rt.zombie.load(Ordering::SeqCst) {
        tracing::info!("会话 {} 已删除，跳过迟到检查点", rt.id);
        return None;
    }
    // 非主会话（子代理 sub_* / 任务运行 task_*）绝不落主索引与主历史：子代理过程历史
    // 由 save_sub_history 边车接管（subagent 工具收尾时落盘，[docs/subagent-interaction-drawer]
    // （../../../docs/subagent-interaction-drawer.md）），任务运行无持久化语义——否则内部运行
    // 会以 untitled 幽灵会话形态泄漏进会话列表
    if !rt.is_main_session {
        return None;
    }
    let history = lock_ok(&rt.history).clone();
    let title = lock_ok(&rt.title).clone();
    // 检查点 model_id 记录会话生效模型（覆盖优先）
    let model_id = {
        let cfg = core.cfg.read().unwrap();
        crate::core::prefs::effective_model(&cfg, &rt.prefs()).map(|m| m.id.clone())
    };
    let ws = rt.workspace.to_string_lossy().into_owned();
    match core.store.save_history(
        &rt.id,
        &title,
        &ws,
        model_id.as_deref(),
        rt.project_id.as_deref(),
        &rt.roots,
        &history,
    ) {
        Ok(report) => Some(report),
        Err(e) => {
            // 会话日志（不受全局日志级别过滤，恒开启）留痕，便于用户事后排查
            session_log::warn(rt, &format!("检查点保存失败：{e}"));
            Some(SaveReport::rejected())
        }
    }
}
/// start_chat 的 IPC 请求体（host 层反序列化后转调 AgentCore::start_chat）。
#[derive(Debug, Deserialize)]
pub struct StartChatBody {
    /// 目标会话 id
    pub session_id: String,
    /// 用户文本
    pub text: String,
    /// 图片附件（可选）
    #[serde(default)]
    pub images: Vec<crate::core::prefs::ImageIn>,
}
