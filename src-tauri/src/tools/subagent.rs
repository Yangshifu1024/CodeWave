//! subagent 工具（[docs/p2-plan](../../../docs/p2-plan.md) §2）：必填 maxSteps 预算与角色；独立 runtime +
//! drive_agent 复用（排除集 + 低预算提示 + 强制汇报）；全局并发 4。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::agent::{DriveParams, SessionRuntime};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 子代理默认步数预算。
pub const DEFAULT_STEPS: usize = 25;
/// 子代理步数预算上限。
pub const MAX_STEPS: usize = 1000;
/// 全局子代理并发上限。
pub const MAX_CONCURRENT: usize = 4;

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

/// 组装子代理的 system_extra：公共纪律 + 注册表命中时的完整角色定义（<agent-definition>）。
/// 抽成纯函数便于单测；纪律块在前，保证追加角色正文不破坏既有前缀语义。
/// 纪律块展示规范角色名（命中时），避免「PM」之类别名造成展示歧义。
fn build_system_extra(
    role: &str,
    max_steps: usize,
    def: Option<&crate::agents::AgentDef>,
) -> String {
    let display_role = def.map(|d| d.name).unwrap_or(role);
    let mut s = format!(
        "\n<subagent-discipline>你是子代理（角色：{}）。纪律：不得向用户提问（无 ask 工具）；不得派生子代理；不得写全局记忆；步数预算 {} 步，耗尽前必须输出最终汇报（已完成/未完成/结论），且最终汇报必须用 <report>…</report> 包裹——过程旁白（如「接下来我来改 X」）不会被当作汇报；只输出文字而不发起工具调用的回合会被视为未完成并提示你继续（[docs/subagent-text-turn-premature-exit]）。写操作遇 E_FILE_CLAIMED = 文件已被兄弟任务认领（严格文件隔离）：不得重试或等待，剔除该文件并在汇报「未完成」中列明，由主代理统一处理。</subagent-discipline>",
        display_role, max_steps
    );
    if let Some(d) = def {
        s.push_str(&format!(
            "\n<agent-definition name=\"{}\">\n{}\n</agent-definition>",
            d.name, d.body
        ));
    }
    s
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
    "cleanContext": {"type": "boolean", "description": "默认 true"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!(
                        "参数解析失败：{e}。入参要求：task/role 非空字符串、maxSteps 1–1000 整数、键名 camelCase（maxSteps/cleanContext）。此错误不计失败：请修正参数后立即重发。"
                    ),
                )
            }
        };
        let max_steps = args.max_steps.unwrap_or(DEFAULT_STEPS).clamp(1, MAX_STEPS);
        if args.task.trim().is_empty() || args.role.trim().is_empty() {
            return ToolOutcome::err(
                "E_ARGS",
                "task 与 role 均不能为空。此错误不计失败：请修正参数后立即重发。",
            );
        }
        // title 是内部自动命名角色（[docs/session-auto-title](../../../docs/session-auto-title.md)）：注册表中存在但不可被主代理委派，
        // 把 agents/mod.rs 头注释中的软约定变成硬约束
        if crate::agents::find(&args.role).map(|d| d.name) == Some("title") {
            return ToolOutcome::err(
                "E_ARGS",
                "title 为内部角色（会话自动命名），不可经 subagent 委派",
            );
        }
        // 注册表命中 → 注入完整角色定义（[docs/arch-orchestrator](../../../docs/arch-orchestrator.md)）；未命中保持自由字符串旧行为
        let agent_def = crate::agents::find(&args.role);
        let system_extra = build_system_extra(&args.role, max_steps, agent_def);

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

        let sub_id = format!(
            "sub_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let description = args
            .description
            .clone()
            .unwrap_or_else(|| args.role.clone());
        ctx.core.sink.emit(
            &ctx.rt.id,
            "sub:spawn",
            json!({ "session": ctx.rt.id, "sub_id": sub_id, "role": args.role,
                    "name": agent_def.map(|d| d.name), "task": crate::core::session_log::trunc(&args.task, 2000),
                    "description": description, "max_steps": max_steps }),
        );
        // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：子代理绑定父会话通道——子代理流式帧经 TauriSink 包装为
        // Frame::Sub 信封借父通道下发；前端按 sub_id 路由到过程抽屉自己的消息流
        ctx.core.sink.bind_sub_channel(&ctx.rt.id, &sub_id);
        // [docs/session-logging-report](../../../docs/session-logging-report.md) 会话日志：spawn 记入父会话日志；子代理自身的 LLM/工具轨迹落子日志文件
        crate::core::session_log::info(
            &ctx.rt,
            &format!(
                "子代理 spawn [{sub_id}] role={} steps={max_steps} task={}",
                args.role,
                crate::core::session_log::trunc(&args.task, 300)
            ),
        );

        // 独立 runtime（继承 workspace/data_dir/extra_roots）+ 注册（支持 StopSubagent）
        let sub_rt = SessionRuntime::new_sub(&ctx.rt, sub_id.clone());
        ctx.core.subs.insert(sub_id.clone(), sub_rt.clone());
        // clean_context=false 时携带主会话最后 6 条消息作背景
        if args.clean_context == Some(false) {
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
                args.role, args.task
            )));

        let mut params = DriveParams {
            max_steps,
            exclude_tools: vec![
                "ask".into(),
                "subagent".into(),
                "plan".into(),
                "skill".into(),
                "scheduled_task".into(),
                "suggest".into(),
                "wait".into(),
            ],
            exclude_mcp: false,
            system_extra,
            budget_notice: true,
            emit_events: false,
            force_report: true,
            // 子代理：纯文本回合不等于完成（防止过程旁白被当成最终汇报提前退出，
            // [docs/subagent-text-turn-premature-exit]）
            finish_on_text: false,
            main_session: false,
            parent_cancel: None,
        };
        // 权限继承（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md) 收紧）：子代理权限 ⊆ 父会话权限。
        // 父 Plan 档的写排除 / MCP 排除 / plan 档提示合并进子参数，堵住「借子代理绕过 plan 档」的洞。
        let parent = crate::core::agent::main_drive_params(&ctx.rt.prefs());
        params.exclude_tools.extend(parent.exclude_tools);
        params.exclude_mcp = params.exclude_mcp || parent.exclude_mcp;
        if !parent.system_extra.is_empty() {
            params.system_extra.push_str(&parent.system_extra);
        }
        // 取消级联：主会话停止（cancel_run）→ 父令牌取消 → 本子代理 child_token 取消，
        // LLM 流与审批等待随之中止（[docs/subagent-file-isolation]）
        params.parent_cancel = Some(ctx.cancel.clone());

        let core = ctx.core.clone();
        let sink = ctx.core.sink.clone();
        let session = ctx.rt.id.clone();
        let sub2 = sub_id.clone();
        let step_rt = sub_rt.clone();
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
                sink.emit(&session, "sub:step", json!({ "session": session, "sub_id": sub2, "step": steps, "tool": last_tool, "detail": detail }));
            }
        });

        // panic unwind 兜底：停掉进度轮询、注销、通知前端收尾子代理卡
        //（此前 panic 会让轮询器永远发 sub:step、前端卡卡在「运行中」）。
        // 正常路径在返回前解除武装——sub:done / sub:error 已由下方分支发出。
        let mut cleanup = SubCleanupGuard {
            core: core.clone(),
            session: ctx.rt.id.clone(),
            sub_id: sub_id.clone(),
            sub_rt: sub_rt.clone(),
            progress,
            armed: true,
        };

        let run_id = format!("sub_{}", sub_id);
        let (result, usage, _) =
            crate::core::agent::drive_agent(&core, &sub_rt, params, &run_id).await;

        // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：子代理完整执行历史落盘（ok/err 两路在此覆盖；panic 路由 SubCleanupGuard 兜底），
        // 过程抽屉在会话恢复后可重建完整消息流
        {
            let h = sub_rt.history.lock().unwrap();
            if let Err(e) = ctx
                .core
                .store
                .save_sub_history(&ctx.rt.id, &sub_id, &h)
            {
                tracing::warn!("子代理 [{sub_id}] 过程历史落盘失败：{e}");
            }
        }

        // L10：子代理 run 的 usage 计入统计（kind=sub，挂在父会话名下）
        if usage.input + usage.output > 0 {
            let model_id = core
                .cfg
                .read()
                .unwrap()
                .active_model_id
                .clone()
                .unwrap_or_default();
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
        let outcome = match result {
            Ok(report) => {
                // <report> 标记只用于收尾判定（drive 层 text_turn_action），不进入汇报正文
                //（[docs/subagent-text-turn-premature-exit]）
                let (clean_report, tagged) = crate::core::agent::split_report(&report);
                // 收尾原因：带标记 = 按约定汇报；步数用尽 = 预算耗尽；否则 = 未按约定汇报即结束
                //（前端据此显示橙色警示而非绿色钩，不再让提前退出伪装成成功）
                 let steps_used = sub_rt.step_count.load(std::sync::atomic::Ordering::SeqCst);
                 // step_count 是「已启动步数」（每步开头写 step+1，见驱动循环顶）：
                 // 真正跑完预算时 steps_used == max_steps，故用 >= 而非 >=
                 let ended = if tagged {
                     "report"
                 } else if steps_used >= max_steps {
                     "budget"
                 } else {
                     "no_report"
                 };
                crate::core::session_log::info(
                    &ctx.rt,
                    &format!(
                        "子代理 [{sub_id}] 返回（input {}/output {} tokens，步数 {steps_used}/{max_steps}，收尾 {ended}）：{}",
                        usage.input,
                        usage.output,
                        crate::core::session_log::trunc(&clean_report, 400)
                    ),
                );
                // G2（[docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md) §7）+ arch 批准闸（[docs/arch-orchestrator](../../../docs/arch-orchestrator.md)）：pm/tester 分析子代理成功返回 → 置分析产物标记。
                // 跨档置位：标记只在批准执行点（ask 批准闸）消费，其余档位置位无副作用；
                // arch 流程在 ConfirmEach 档发起批准同样依赖该标记。
                if is_analysis_role(&args.role) {
                    ctx.rt
                        .analysis_done
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                // 最终汇报先进卡 → usage → done（前端在关闭进度条前展示汇报）
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:report",
                    json!({ "session": ctx.rt.id, "sub_id": sub_id, "report": clean_report.clone() }),
                );
                // sub:usage 先于 sub:done（前端卡先展示 token 数再完成）
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:usage",
                    json!({ "session": ctx.rt.id, "sub_id": sub_id, "usage": {
                        "input": usage.input, "output": usage.output,
                    } }),
                );
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:done",
                    json!({ "session": ctx.rt.id, "sub_id": sub_id, "usage": usage,
                            "steps_used": steps_used, "ended": ended }),
                );
                ToolOutcome::ok(
                    // sub_id 放首位：它能在 tool_result 头部截断后幸存，
                    // 供会话恢复关联过程历史文件（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）
                    json!({ "sub_id": sub_id, "role": args.role, "steps_budget": max_steps,
                            "steps_used": steps_used, "ended": ended, "report": clean_report }),
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
                crate::core::session_log::warn(&ctx.rt, &format!("子代理 [{sub_id}] 取消：{ui_err}"));
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:error",
                    json!({ "session": ctx.rt.id, "sub_id": sub_id, "error": ui_err }),
                );
                ToolOutcome::err("E_SUBAGENT_STOPPED", tool_msg)
            }
            Err(e) => {
                crate::core::session_log::error(&ctx.rt, &format!("子代理 [{sub_id}] 失败：{e}"));
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "sub:error",
                    json!({ "session": ctx.rt.id, "sub_id": sub_id, "error": e.to_string() }),
                );
                ToolOutcome::err("E_SUBAGENT", format!("子代理执行失败：{e}"))
            }
        };
        cleanup.armed = false;
        drop(cleanup); // 立即执行清理（中止已结束的轮询 / 注销均为幂等）
        outcome
    }
}

/// 子代理异常退出（panic unwind）兜底：停进度轮询、注销；armed 时补发 sub:error 让前端收尾
///（正常路径已发 sub:done/sub:error，先解除武装防重复）；并尽力落盘过程历史。
/// Drop 覆盖三条路径（ok/err/panic）：panic 路在 unwind 中兑现；正常路径显式 drop（L353）提前清理。
struct SubCleanupGuard {
    core: std::sync::Arc<crate::core::agent::AgentCore>,
    session: String,
    sub_id: String,
    sub_rt: std::sync::Arc<crate::core::agent::SessionRuntime>,
    progress: tokio::task::JoinHandle<()>,
    armed: bool,
}
impl Drop for SubCleanupGuard {
    fn drop(&mut self) {
        self.progress.abort();
        self.core.subs.remove(&self.sub_id);
        // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md) 配对注销：防止 subs 注册表无限增长（内存泄漏 + 向陈旧 sub_id 误投递一段窗口）。
        // NoopSink（测试）默认无操作；无通道时零副作用。
        self.core.sink.unbind_sub_channel(&self.sub_id);
        if self.armed {
            // panic 路同样尽力落盘已产生的过程历史（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）
            let h = self.sub_rt.history.lock().unwrap();
            if let Err(e) = self.core.store.save_sub_history(&self.session, &self.sub_id, &h) {
                tracing::warn!("子代理 [{}] panic 路过程历史落盘失败：{e}", self.sub_id);
            }
            drop(h);
            self.core.sink.emit(
                &self.session,
                "sub:error",
                json!({ "session": self.session, "sub_id": self.sub_id, "error": "子代理内部异常终止" }),
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_constants() {
        assert_eq!(DEFAULT_STEPS, 25);
        assert_eq!(MAX_STEPS, 1000);
        assert_eq!(MAX_CONCURRENT, 4);
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
        assert!(
            !acquire_slot(&cancelled, std::time::Duration::from_secs(30)).await
        );
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
        let s = build_system_extra("backend-dev", 60, Some(def));
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
        let s = build_system_extra("my-custom-role", 25, None);
        assert!(s.contains("<subagent-discipline>"));
        assert!(s.contains("角色：my-custom-role"));
        assert!(!s.contains("<agent-definition"));
    }

    #[test]
    fn system_extra_alias_shows_canonical_name() {
        let s = build_system_extra("PM", 30, crate::agents::find("PM"));
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
}
