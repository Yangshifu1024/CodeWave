use crate::core::context::{self};
use crate::core::session_log;
use crate::core::sessions::repair;
use crate::core::types::Message;
use crate::provider::dto::ProviderError;
use crate::provider::retry;
use crate::tools::batch::execute_batch;
use serde::Deserialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use super::guards::{CompactingGuard, DriveUnwindGuard, lock_ok};
use super::runtime::{AgentCore, CHECKPOINT_EVERY_STEPS, EventSink, Frame, INJECT_BUFFER, MAX_STEPS, SessionRuntime, STREAM_THROTTLE_MS};
use super::supervise::{BatchDigest, CallSig, SupervisionState, Verdict};
use super::stream::{ERROR_CAP, VERBOSE_BODY_CAP};
use super::stream::{build_assistant_message, build_stream_request, collect_deltas, flush_segments, refresh_request_messages, stream_flush_loop};

/// 归一化后的工具调用（参数已修复为合法 JSON object；供批次执行层消费）。
#[derive(Debug, Clone)]
pub struct NormalizedCall {
    /// 调用 id（与 tool_use 对应）
    pub id: String,
    /// 工具名
    pub name: String,
    /// 归一化后的参数（非 object 值包成 {"value": ...}）
    pub args: serde_json::Value,
    /// provider 侧调用序号
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
    rt.plan_hint_emitted
        .store(false, Ordering::SeqCst);

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

    // 审批档位 → 主会话驱动参数（Plan：排除写工具与 MCP，提示模型先出方案，
    // [docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
    let params = main_drive_params(&rt.prefs());
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
            checkpoint(&core, &rt).await;
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
            checkpoint(&core, &rt).await;
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
        core.stats.record(crate::core::stats::UsageRecord {
            session: rt.id.clone(),
            model_id,
            workspace: rt.workspace.to_string_lossy().into_owned(),
            input: run_usage.input,
            output: run_usage.output,
            cache_read: run_usage.cache_read,
            cache_write: run_usage.cache_write,
            runs: 1,
            kind: crate::core::stats::KIND_MAIN.into(),
        });
    }
}

/// 主会话 DriveParams：按会话审批档位追加排除项与提示文本（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）。
/// Plan 档收紧（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）：排除写工具 / 后台服务 / 任务运行 + MCP；
/// shell 保留但受只读 fence 白名单约束（plan_readonly，白名单外一律确认）；
/// 子代理继承同样语义。
/// 主会话 run 每步按当前偏好重算限制（ask 批准切到 AutoEdit 在下一步生效）；
/// 子代理/任务 run 由父参数冻结，不受会话中途切档影响。
pub fn main_drive_params(prefs: &crate::core::prefs::SessionPrefs) -> DriveParams {
    let mut params = DriveParams {
        main_session: true,
        ..DriveParams::default()
    };
    apply_plan_mode(&mut params, prefs);
    params
}

/// Plan 档限制追加（主会话每步重算时调用）。
pub(super) fn apply_plan_mode(params: &mut DriveParams, prefs: &crate::core::prefs::SessionPrefs) {
    if prefs.approval_mode != crate::core::prefs::ApprovalMode::Plan {
        return;
    }
    params.exclude_tools.extend(
        ["edit", "create", "delete", "service", "scheduled_task"]
            .iter()
            .map(|s| s.to_string()),
    );
    params.exclude_mcp = true;
    params.system_extra =
        "\n<plan-mode>计划模式：只读调研，不执行任何修改。文件写入、后台服务与计划任务工具不可用，MCP 工具不可用；shell 仅放行只读命令白名单（ls/cd/head/grep/git log 等，其余命令会被直接拦截，不会弹确认——请把需要执行的命令纳入方案，经批准后运行）；子代理同样仅限只读。严格按阶段流程推进（docs/plan-mode-workflow），不得跳步：\nP0 接到请求先声明分类（需求/缺陷/问答/混合）与一句话依据；问答类直接回答，不进流程。\nP1 有关键歧义先澄清，无歧义则声明假设继续。\nP2 需求类调用 product-manager 子代理产出结构化需求分析（用户故事/AC/边界/非目标/开放问题）；缺陷类调用 tester 产出复现步骤/根因/影响面/修复建议与回归要点；开放问题回流澄清（≤2 轮）。分析不设用户确认门，产出后直接进入 P3。\nP3 基于分析编写技术方案（文件级改动点/风险/回滚；git 仓库内拟定分支名 <type>/<slug>，slug ≤24 字符、基线当前 HEAD，非 git 仓库注明跳过），用 plan 工具登记 todos（必须包含验证项），然后用 ask 工具询问用户（题干与 plan 文本列明分支名；批准 = 预授权创建并切换分支）：同意则选「执行方案」（系统将自动切换到自动编辑模式并指示你立即执行），有意见则选「补充意见」——修订时逐条回应（采纳/不采纳+理由），基于上一版做增量更新，不重做分析；同一方案 3 轮未收敛则把争议点拆成多个选项逐项询问。禁止未经 P2 分析、或 todos 缺失/含验证项时就发起询问。\n轻量路径：改动预计 ≤2 文件、无删除、无新依赖、无跨层改动时，P2 可用内置简析替代子代理调用（在回复中明示「轻量路径」）；批准询问不可省略。用户明确说「直接改/不用分析」时同样跳过 P2，但仍需登记 todos 并经批准。\n批准后：git 仓库内先执行 git switch -c <分支名>（已存在则改 -2 后缀并说明；失败如实报告请用户处理）再动工；严格按已确认 todos 顺序执行，超范围写操作先询问；多文件/跨层变更完成后调用 code-reviewer 审查（🔴 必须修复），最后汇报变更摘要、验证结果与本轮偏差记录。</plan-mode>"
            .into();
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
    /// 视为本 run 的自然收尾（主会话语义；或已带最终汇报标记）
    Finish,
    /// 注入提示后继续下一步（消耗步数预算）
    Continue,
    /// 连续无工具调用达上限：以显式错误终止，绝不伪装成功
    StopWithLimit,
}

/// 无工具调用回合（含「唯一调用被拒」的空文本回合）如何处置——纯函数便于矩阵单测。
///
/// 判定顺序（`rejected` 先于主会话语义，是有意为之，见下）：
/// 1. `rejected`（本回合有调用因参数 JSON 不可解析被拒）→ 未达上限则 `Continue`：
///    拒绝提示已注入历史，必须让模型看到后修正重发；上限仍由 `MAX_TEXT_TURNS` 兜底，
///    不无限续跑。**必须先于 `finish_on_text` 判定**：主会话「正文非空 + 全部调用被拒」
///    此前直接 `Finish`，run 静默成功、提示永不被模型看到、方案从未产出
///    （[docs/rejected-call-silent-finish]：会话 5100ea0c 的 8531 字符 `ask`）。
/// 2. `finish_on_text`（主会话）→ `Finish`：对主会话而言「无工具调用 = 回答完毕」语义不变
///    （无被拒调用的回合逐字节不变）；
/// 3. 文本含 `<report>` 标记 → `Finish`：显式最终汇报；
/// 4. `text_turns >= MAX_TEXT_TURNS` → `StopWithLimit`：不收敛则显式失败；
/// 5. 其余 → `Continue`。
pub(super) fn text_turn_action(
    text: &str,
    finish_on_text: bool,
    rejected: bool,
    text_turns: u32,
) -> TextTurnAction {
    // ① 被拒调用：提示已注入，绝不能就此收尾（主会话亦然）；上限仍生效
    if rejected {
        return if text_turns >= MAX_TEXT_TURNS {
            TextTurnAction::StopWithLimit
        } else {
            TextTurnAction::Continue
        };
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
    // 父令牌存在（子代理）时派生 child_token：父取消 → 子取消；子仍可被单独停止（stop_subagent）
    let run_token = match params.parent_cancel.take() {
        Some(parent) => parent.child_token(),
        None => CancellationToken::new(),
    };
    *rt.active_cancel.lock().unwrap() = Some(run_token.clone());
    // 非主 runtime（子代理/任务运行）：文件写认领随 drive 生命周期——
    // 正常结束与 panic unwind 都经 Drop 释放（[docs/subagent-file-isolation]）
    let _claims =
        (!params.main_session).then(|| crate::tools::claims::ReleaseGuard::arm(&rt.id));

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
    let mut final_text = String::new();
    let mut outcome: Result<(), ProviderError> = Ok(());
    // suggest 路径产出的跟进建议：经返回值交给 run_chat 并入 run:done
    // （复位 running 后发唯一一次）
    let mut suggest_out: Option<Vec<String>> = None;
    // 自动压缩失败冷却：连续失败 ≥2 次后本次 run 内不再尝试，
    // 阈值持续超限时每步不再各堵一个完整超时。
    let mut compact_fail_streak: u32 = 0;
    // 运行监督（[docs/subagent-file-isolation]）：重复失败/重复调用先纠偏、不收敛则终止；
    // 另有空转看门狗（feed_batch）检测零进展只读循环
    let mut supervision = SupervisionState::default();
    // 连续「无工具调用回合」计数（仅非主会话 run 消费；有工具调用或压缩成功时复位）：
    // 用于 <continue-notice> 续跑与 MAX_TEXT_TURNS 显式失败门
    //（[docs/subagent-text-turn-premature-exit]）
    let mut text_turns: u32 = 0;

    'steps: for step in 0..params.max_steps {
        // 真实步数上报（sub:step 进度采样消费；取代 history.len() 失真口径）
        rt.step_count.store(step + 1, Ordering::SeqCst);
        // ① 取消检查
        if run_token.is_cancelled() {
            outcome = Err(ProviderError::Cancelled);
            break 'steps;
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
            rt.history.lock().unwrap().push(Message::user_text(
                "<budget-notice>步数预算即将耗尽，请尽快收敛并输出汇报。</budget-notice>",
            ).stamped());
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

        let assembled = match run_llm_turn(
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
                    emit_retry(&sink, &rt, run_id, attempt);
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
            match text_turn_action(&joined, params.finish_on_text, rejected, text_turns) {
                TextTurnAction::Finish => break 'steps,
                TextTurnAction::Continue => {
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
        // M6：强制汇报轮也在工具批次完成后才结束（历史不留悬空 tool_use）
        if batch_done {
            break 'steps;
        }

        if step % CHECKPOINT_EVERY_STEPS == CHECKPOINT_EVERY_STEPS - 1 {
            checkpoint(core, rt).await;
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
    let stall_timeout =
        Duration::from_secs(core.cfg.read().unwrap().stall_timeout_seconds.clamp(5, 3600));
    loop {
        let (tx, rx) = mpsc::channel::<crate::provider::StreamDelta>(1024);
        let collector = tokio::spawn(collect_deltas(rx, rt.stream.clone()));
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
            crate::provider::stream_model(
                &client,
                &model_owned,
                key,
                req_owned,
                tx,
                token_for_task,
            )
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
                if params.emit_events {
                    sink.channel_frame(
                        &rt.id,
                        &Frame::Usage {
                            input: usage.input,
                            output: usage.output,
                            cache_read: usage.cache_read,
                            cache_write: usage.cache_write,
                        },
                    );
                }
                let asm = collector.await.unwrap_or_default();
                // 空响应按可重试处理
                if asm.is_empty()
                    && retry::should_retry(&ProviderError::Server("空响应".into()), *attempt) {
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
                        ProviderError::BadRequest { message, .. } => classify_reasoning_400(message),
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
                    refresh_request_messages(rt, req);
                    rt.stream.reset();
                    session_log::warn(
                        rt,
                        &format!(
                            "step {step} BadRequest，sanitize/repair 历史后重试：{}",
                            session_log::trunc(&e.to_string(), ERROR_CAP)
                        ),
                    );
                    tracing::warn!("session {} step {step} BadRequest sanitize 重试：{e}", rt.id);
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
                    tracing::warn!("session {} step {step} 请求失败，重试 #{}：{e}", rt.id, *attempt);
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
    let threshold = core.cfg.read().unwrap().compact_threshold.clamp(0.05, 0.95);
    if bd.ratio > threshold as f64 && *compact_fail_streak < 2 {
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
                "自动压缩触发 ratio={:.2} / threshold={threshold:.2} tokens={} timeout={timeout_secs}s",
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
        let compact_result = match CompactingGuard::acquire(rt) { Some(_compact_guard) => {
            context::compact_history(core, rt, false, run_token).await
        } _ => {
            // 理论不可达（run 外手动压缩已被 running 阻断）；
            // 按失败处理并冷却，继续本步
            session_log::warn(rt, "自动压缩跳过：压缩互斥被占位");
            Err("压缩互斥被占位".into())
        }};
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
                        &format!("自动压缩连续失败 {} 次，本 run 内不再自动压缩：{e}", *compact_fail_streak),
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
    if batch_out.plan_approved && params.emit_events {
        rt.history.lock().unwrap().push(Message::user_text(
            "[system] 方案已获批准，会话已切换到自动编辑模式。请立即按方案开始执行，无需再次询问。",
        ).stamped());
    }

    // 主会话每步重算计划限制（切档即时生效）；子代理/任务参数保持冻结。
    // main_drive_params 内部已置 main_session = true，无需再显式赋值
    if params.main_session {
        *params = main_drive_params(&rt.prefs());
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
/// 返回（结果，usage）——usage 由调用方记入统计（L10）与任务日志（B7）。
pub async fn run_task_agent(
    core: Arc<AgentCore>,
    rt: Arc<SessionRuntime>,
    task: crate::core::scheduler::ScheduledTask,
) -> (Result<String, String>, crate::provider::RunUsage) {
    let params = DriveParams {
        max_steps: crate::core::scheduler::TASK_BUDGET_STEPS,
        exclude_mcp: true,
        exclude_tools: vec!["ask".into(), "subagent".into(), "scheduled_task".into(), "suggest".into(), "wait".into()],
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
        result.map(|r| split_report(&r).0).map_err(|e| e.to_string()),
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
        let readonly = super::supervise::READONLY_TOOLS.contains(&c.name.as_str());
        if !readonly {
            d.has_non_readonly = true;
        }
        if c.name == "read" {
            if c.args["path"].as_str().is_some() {
                d.read_paths.push(normalize_read_path(&c.args));
            }
        } else if c.name == "batch_read" {
            if let Some(files) = c.args["files"].as_array() {
                for f in files {
                    if f["path"].as_str().is_some() {
                        d.read_paths
                            .push(normalize_read_path(&serde_json::json!({"path": f["path"]})));
                    }
                }
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
    use super::{classify_reasoning_400, stalled, update_reasoning_sticky, Reasoning400};

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
            classify_reasoning_400("Invalid assistant message: content or tool_calls must be set (HTTP 400)"),
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
                    Content::Thinking {
                        text: "想".into(),
                    },
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
            !has_thinking(&messages_for_request(&rt)),
            "粘性置位后出网副本必须无思考（锁死症状：要求回传的端点每轮 400）"
        );

        // 自愈：该端点的 400 被判为 Demanded → 标记复位
        let verdict = classify_reasoning_400(
            "The `reasoning_content` in the thinking mode must be passed back to the API. (HTTP 400)",
        );
        assert_eq!(verdict, Reasoning400::Demanded);
        assert!(update_reasoning_sticky(&rt, verdict), "复位必须报告状态变化");
        assert!(
            !rt.reasoning_rejected.load(Ordering::SeqCst),
            "Demanded 必须复位粘性标记"
        );

        // 复位后的下一个请求重新带上思考；转录全程未被改写
        assert!(
            has_thinking(&messages_for_request(&rt)),
            "复位后下一个请求必须重新回传思考（要求回传的端点靠它拿数据）"
        );
        assert_eq!(
            rt.history.lock().unwrap().as_slice(),
            history.as_slice(),
            "全程不得改写 rt.history"
        );
    }
}

/// 发 run:retry 事件（带 gen 代数与退避时延，前端据此丢弃旧帧并等待）。
pub(super) fn emit_retry(sink: &Arc<dyn EventSink>, rt: &SessionRuntime, run_id: &str, attempt: u32) {
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

/// 取消收尾：历史落取消标记 + 检查点 + run:cancelled 事件。
pub(super) async fn mark_cancelled(core: &Arc<AgentCore>, rt: &Arc<SessionRuntime>, run_id: &str) {
    lock_ok(&rt.history).push(Message::user_text("<run-cancelled/>").stamped());
    checkpoint(core, rt).await;
    core.sink.emit(
        &rt.id,
        "run:cancelled",
        serde_json::json!({ "session": rt.id, "run_id": run_id }),
    );
}

/// 检查点保存（run 结束/取消/每 N 步）。
pub(super) async fn checkpoint(core: &Arc<AgentCore>, rt: &Arc<SessionRuntime>) {
    // H5：会话已删除（如 delete_project 级联）——迟到收尾不得回写索引复活幽灵会话
    if rt.zombie.load(Ordering::SeqCst) {
        tracing::info!("会话 {} 已删除，跳过迟到检查点", rt.id);
        return;
    }
    // 非主会话（子代理 sub_* / 任务运行 task_*）绝不落主索引与主历史：子代理过程历史
    // 由 save_sub_history 边车接管（subagent 工具收尾时落盘，[docs/subagent-interaction-drawer]
    // （../../../docs/subagent-interaction-drawer.md）），任务运行无持久化语义——否则内部运行
    // 会以 untitled 幽灵会话形态泄漏进会话列表
    if !rt.is_main_session {
        return;
    }
    let history = lock_ok(&rt.history).clone();
    let title = lock_ok(&rt.title).clone();
    // 检查点 model_id 记录会话生效模型（覆盖优先）
    let model_id = {
        let cfg = core.cfg.read().unwrap();
        crate::core::prefs::effective_model(&cfg, &rt.prefs()).map(|m| m.id.clone())
    };
    let ws = rt.workspace.to_string_lossy().into_owned();
    if let Err(e) = core.store.save_history(
        &rt.id,
        &title,
        &ws,
        model_id.as_deref(),
        rt.project_id.as_deref(),
        &rt.roots,
        &history,
    ) {
        tracing::warn!("检查点保存失败：{e}");
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
