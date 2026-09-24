//! subagent 工具（[docs/p2-plan](../../../docs/p2-plan.md) §2）：必填 maxSteps 预算与角色；独立 runtime +
//! drive_agent 复用（排除集 + 低预算提示 + 强制汇报）；全局并发 4。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::agent::{DriveParams, SessionRuntime, SubBase};
use crate::core::prefs::ApprovalMode;
use serde::Deserialize;
use serde_json::{Value, json};
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

/// 子代理基座排除集（B1）：spawn 时冻结的内部 7 项（交互类 / 元工具），与档位无关。
/// 父档派生（Plan 档的写工具 / 后台服务 / 计划任务）与角色派生（只读角色的写工具）
/// 在 `subagent_drive_params` 重建时另加。
pub(crate) const SUB_BASE_EXCLUDES: &[&str] = &[
    "ask",
    "subagent",
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
    // 免得它反复试探被拒而白烧步数（[docs/subagent-idle-watchdog-misfire]）。
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
        "\n<subagent-discipline>你是子代理（角色：{}）。纪律：不得向用户提问（无 ask 工具）；不得派生子代理；不得写全局记忆；步数预算 {} 步，耗尽前必须输出最终汇报（已完成/未完成/结论），且最终汇报必须用 <report>…</report> 包裹——过程旁白（如「接下来我来改 X」）不会被当作汇报；只输出文字而不发起工具调用的回合会被视为未完成并提示你继续（[docs/subagent-text-turn-premature-exit]）。写操作遇 E_FILE_CLAIMED = 文件已被兄弟任务认领（严格文件隔离）：不得重试或等待，剔除该文件并在汇报「未完成」中列明，由主代理统一处理。{}</subagent-discipline>",
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
) -> SubBase {
    SubBase {
        root_session_id: None,
        role: role.to_string(),
        max_steps,
        spawn_mode,
        base_excludes: SUB_BASE_EXCLUDES.iter().map(|s| (*s).to_string()).collect(),
        base_system_extra: build_system_extra(role, max_steps, def, spawn_mode),
        idle_policy: idle_policy_for(role),
    }
}

/// 只读角色判定：注册表 `readonly` 标记的单一事实源（explore/reviewer/code-reviewer = true）。
/// 与 `is_analysis_role` 同风格：走 `crate::agents::find`，别名与大小写归一同源——
/// 谓词只此一份，避免「改一处漏一处」。
fn is_readonly_role(role: &str) -> bool {
    crate::agents::find(role).is_some_and(|d| d.readonly)
}

/// 空转看门狗策略（[docs/subagent-idle-watchdog-misfire]）：只读角色 → `NudgeOnly`
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

/// 子代理角色策略装配（[docs/subagent-idle-watchdog-misfire]）：一步到位置 `idle_policy`
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
        let description = args
            .description
            .clone()
            .unwrap_or_else(|| args.role.clone());
        // task 全量下发：抽屉「分配的任务」要能看到完整任务。此前在此 trunc 2000 字符，实时面板只能
        // 显示「…(truncated)」，而落盘历史存的是全文——恢复会话后反而完整，实时与归档两路不一致。
        // 下方日志行仍按 300 字符截断（那是日志文件，不是 UI 载荷）。
        ctx.core.sink.emit(
            &ctx.rt.id,
            "sub:spawn",
            json!({ "session": ctx.rt.id, "sub_id": sub_id, "role": args.role,
                    "name": agent_def.map(|d| d.name), "task": args.task.clone(),
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

        // 档位基座（B1）：内部排除集 + 角色纪律块 + idle 策略在 spawn 冻结一次；
        // 此后每步由 drive 层按父会话**实时**档位从基座重建（`subagent_drive_params`）——
        // 子代理不再冻结在 spawn 时的档位上。
        let mut base = sub_base(&args.role, max_steps, agent_def, parent_prefs.approval_mode);
        base.root_session_id = sub_rt.root_session_id.clone();
        // 权限继承（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md) 收紧）：子代理权限 ⊆ 父会话权限。
        // 父 Plan 档的写排除 / MCP 排除 / plan 档提示由 subagent_drive_params 从基座合并进子参数，
        // 堵住「借子代理绕过 plan 档」的洞；只读角色的策略（含 FullAccess 下解锁写工具，B3）
        // 同样在其中按档位置位。spawn 与每步重算共用这一条装配路径，两处不会漂移。
        let mut params = crate::core::agent::subagent_drive_params(&base, &parent_prefs);
        params.max_steps = max_steps;
        params.budget_notice = true;
        params.emit_events = false;
        params.force_report = true;
        // 子代理：纯文本回合不等于完成（防止过程旁白被当成最终汇报提前退出，
        // [docs/subagent-text-turn-premature-exit]）
        params.finish_on_text = false;
        // 取消级联：主会话停止（cancel_run）→ 父令牌取消 → 本子代理 child_token 取消，
        // LLM 流与审批等待随之中止（[docs/subagent-file-isolation]）
        params.parent_cancel = Some(ctx.cancel.clone());
        // 档位重算基座：drive 层每步据此按父档重建（None = 不重算）
        params.sub_base = Some(base);

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
                // 当前档位（B2）：子 rt 的 prefs 由 drive 层每步按父档同步，故此处即实时值。
                // 载荷只**新增**字段，事件键名与 29 键契约不变（前端据它显示子代理卡档位）。
                let approval_mode = step_rt.prefs().approval_mode;
                sink.emit(&session, "sub:step", json!({ "session": session, "sub_id": sub2, "step": steps, "tool": last_tool, "detail": detail, "approval_mode": approval_mode }));
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
            if let Err(e) = ctx.core.store.save_sub_history(&ctx.rt.id, &sub_id, &h) {
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
                crate::core::session_log::warn(
                    &ctx.rt,
                    &format!("子代理 [{sub_id}] 取消：{ui_err}"),
                );
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
            if let Err(e) = self
                .core
                .store
                .save_sub_history(&self.session, &self.sub_id, &h)
            {
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
        let mut p = DriveParams::default();
        // 模拟父档位已合并集（其中 edit 与只读写排除重叠）
        p.exclude_tools = vec!["ask".into(), "edit".into()];
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
