//! 批次执行策略（[docs/p0-plan](../../../docs/p0-plan.md) §6.1.1 / §6.3.3）：
//! Interactive 工具必须独占批次 → 写冲突拒绝 → 文件写串行 / 其余并发上限 4 → panic 兜底。

use crate::core::agent::{AgentCore, EventSink, NormalizedCall, SessionRuntime};
use crate::core::session_log;
use crate::core::types::Content;
use crate::tools::{ToolCtx, ToolKind, ToolOutcome, collect_unknown_fields};
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// 批次执行结果：模型侧结果 + suggest 语义（成功即结束 run）+ plan 批准语义（档位已切换）。
pub struct BatchOutcome {
    /// 按调用顺序排列的模型侧 ToolResult 内容（read 读图时其后还会跟图片块：
    /// 出网副本层 stream.rs::route_tool_images 会把它们搬进紧随其后的用户消息）。
    pub results: Vec<Content>,
    /// suggest 工具成功时携带的建议列表（run 循环据此结束 run）。
    pub suggest_items: Option<Vec<String>>,
    /// ask 批准形询问被批准时为 true（run 循环据此走批准后流程）。
    pub plan_approved: bool,
    /// 监督摘要：每个调用的（工具名, 失败码, args 哈希），供 drive 层重复操作检测
    /// （[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）。
    pub call_summary: Vec<crate::core::agent::CallSig>,
}

/// 执行一个批次的全部工具调用：先做批次级策略判定（Interactive 独占、同批次同路径写冲突、
/// 本 run 工具排除集硬门、plan 纪律硬门），再并发执行（文件写串行、其余并发上限 4），最后组装模型侧结果。
/// 硬门在 spawn 前拒绝，绝不真实执行；每个调用独立 panic 兜底（E_TOOL_PANIC）。
// 8 个参数分别来自运行上下文的不同来源（core / rt / 本批调用 / 排除集 / 主会话标记 /
// 取消令牌 / run_id），都必要；收成结构体只是换壳，却要连带改本文件测试模块里 17 处调用点
// 与 drive.rs 的 1 处调用点（测试代码本次范围禁改），收益不划算。
#[allow(clippy::too_many_arguments)]
pub async fn execute_batch(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    mut calls: Vec<NormalizedCall>,
    effective_excludes: &[String],
    effective_exclude_mcp: bool,
    main_session: bool,
    cancel: CancellationToken,
    run_id: &str,
) -> BatchOutcome {
    let batch_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let sink = core.sink.clone();

    // provider 侧下标（anthropic 的 content_block_start 内容块下标，会被 thinking / text 块顶偏）
    // 不得泄漏到前端 key：此处收敛为批内位置，使进度帧 index、`tool:start` / `tool:result` 的
    // call_key、ToolCtx.call_index / call_key（command 工具进度帧据此续接）全部同源——
    // 否则同一张卡在两条通道上拿到两个 key，前端会建出两张卡（运行中一张永久转圈 + 结果一张）。
    for (i, c) in calls.iter_mut().enumerate() {
        c.index = i;
    }

    // (1) Interactive 工具必须独占批次
    let has_interactive = calls
        .iter()
        .any(|c| core.tools.get(&c.name).map(|t| t.kind()) == Some(ToolKind::Interactive));
    if has_interactive && calls.len() > 1 {
        let mut results = Vec::new();
        for c in &calls {
            let out = ToolOutcome::err(
                "E_BATCH_POLICY",
                "ask/wait/suggest 类工具必须是批次中唯一的调用",
            );
            emit_result(&sink, rt, run_id, &batch_id, c, &out, 0);
            // vision 传 false：本路径（ask/wait/suggest 违反独占）只会产出错误结果，不可能带图片
            results.push(model_content(core, c, &out, None, false));
        }
        return BatchOutcome {
            results,
            suggest_items: None,
            plan_approved: false,
            call_summary: Vec::new(),
        };
    }

    // (2) 同一批次内对同一物理路径的多次写入全部拒绝
    let write_paths: Vec<Option<String>> = calls
        .iter()
        .map(|c| {
            let kind = core.tools.get(&c.name).map(|t| t.kind());
            match kind {
                Some(ToolKind::FileWrite) => {
                    Some(canonical_arg_paths(&rt.workspace, &c.args).join("|"))
                }
                _ => None,
            }
        })
        .collect();
    let conflict: Vec<bool> = (0..calls.len())
        .map(|i| {
            let Some(p) = &write_paths[i] else {
                return false;
            };
            if p.is_empty() {
                return false;
            }
            (0..calls.len()).any(|j| j != i && write_paths[j].as_deref() == Some(p.as_str()))
        })
        .collect();

    // (3) 执行：写串行（file_ops 锁）、其余并发上限 4（信号量）；
    // 收口与 cancel 竞速：取消时 abort 全部在途工具任务（闸/锁等待不再永久挂起，
    // command 任务经 kill_on_drop 回收子进程），并为未完成调用合成 E_CANCELLED 结果——
    // 批次必有完整结果，历史不留悬空 tool_use，drive_agent 得以返回并走「被用户取消」收尾
    let mut outcomes: Vec<Option<(ToolOutcome, Vec<Content>)>> = vec![None; calls.len()];
    let mut join = tokio::task::JoinSet::new();
    // 同批乐观豁免：本批含 plan 调用 → 本轮写调用放行（模型同批建计划说明意图存在；
    // 门基于批次入口快照判定，JoinSet 并发下共享同一快照行为一致，不做逐 call 重评估）
    let batch_has_plan = calls.iter().any(|c| c.name == "plan");
    // 批次入口快照：JoinSet 并发共享同一判定，门语义 = 入口时刻状态
    let todos_snapshot = rt.todos.lock().unwrap().clone();

    for (i, call) in calls.iter().enumerate() {
        // Plan 档硬门（缺陷修复）：exclude_tools 此前只过滤发给模型的工具列表；
        // 模型坚持调用被排除工具（如 plan 档下的 edit）时仍会真实执行——它拿到
        // 「请重新 read」式错误并按提示重试，形成死循环。现于 spawn 前按本 run 生效的
        // 排除集拒绝（与模型工具列表同源；ask 批准切档后下一 step 重算）；
        // 错误为终结性提示，明示重试 / 重读无法解除。
        if effective_excludes.contains(&call.name) {
            let out = ToolOutcome::err(
                "E_TOOL_BLOCKED",
                format!(
                    "「{}」在当前权限模式下不可用（本 run 工具集已排除该工具）。此限制不会因重试、重新 read 或改写参数而解除：请改用可用工具完成目标，或经用户批准切换权限模式后再继续。",
                    call.name
                ),
            );
            emit_result(&sink, rt, run_id, &batch_id, call, &out, 0);
            outcomes[i] = Some((out, Vec::new()));
            continue;
        }
        // 同构硬门（评审项）：exclude_mcp 同样只过滤了列表；幻觉出的
        // mcp__server__tool 调用会命中 mcp__ 前缀分支真实执行（先于 E_UNKNOWN_TOOL）。
        // 与上面的写工具硬门同语义：spawn 前拒绝不执行，终结性提示。
        if effective_exclude_mcp && call.name.starts_with("mcp__") {
            let out = ToolOutcome::err(
                "E_TOOL_BLOCKED",
                format!(
                    "「{}」在当前权限模式下不可用（MCP 工具已排除）。此限制不会因重试或改写参数而解除：请改用可用工具完成目标。",
                    call.name
                ),
            );
            emit_result(&sink, rt, run_id, &batch_id, call, &out, 0);
            outcomes[i] = Some((out, Vec::new()));
            continue;
        }
        // plan 纪律硬门（批次层确定性落地）：实现类工作先建计划（E_PLAN_REQUIRED）；
        // 计划已全部完成后继续写入需先更新计划（E_PLAN_STALE）。与上面两道门同模式：
        // spawn 前拒绝不执行、终结性提示。豁免：非主会话（dev 等子代理被排除 plan 工具，
        // 不豁免将无法写文件）；本批含 plan 调用（同批乐观豁免，见 batch_has_plan）；
        // **目标档执行期**（`goal_execute_phase`，两错误码一起豁免）——该阶段的提示块通篇讲账本与
        // 验收标准、只字未提「必须先建计划」，叠这道门会让执行期第一次 edit 就被拒，与「零提问 +
        // 自主推进」直接冲突：目标执行期的范围控制由账本承担（下面的 goal_write_gate），不叠本门。
        // 分工边界：command 工具的 shell 重定向写不经本门，归 fence/G3 范围门兜底（见 run_tool）。
        let is_write = core.tools.get(&call.name).map(|t| t.kind()) == Some(ToolKind::FileWrite);
        if main_session
            && is_write
            && !batch_has_plan
            && !crate::core::agent::goal::goal_execute_phase(rt)
        {
            if let Some(code) = plan_gate_verdict(&todos_snapshot, is_write) {
                let message = if code == "E_PLAN_REQUIRED" {
                    format!(
                        "「{}」被拒绝：实现类工作开始前必须先用 plan 工具建立计划（建最简计划即可，一条 todo 也算）。此限制不会因重试或改写参数而解除：请调用 plan 登记计划后再重试。",
                        call.name
                    )
                } else {
                    format!(
                        "「{}」被拒绝：当前计划已全部完成，继续写入属于计划外工作。请先用 plan 工具更新计划（新增待办条目）后再继续。",
                        call.name
                    )
                };
                let out = ToolOutcome::err(code, message);
                emit_result(&sink, rt, run_id, &batch_id, call, &out, 0);
                outcomes[i] = Some((out, Vec::new()));
                continue;
            }
        }
        // 目标档执行期账本硬门（判定见 core/agent/goal.rs 的 ledger_gate）：写入目标必须在
        // 账本内——越界即拒（不弹审批、不问人），与 plan 门同模式：spawn 前拒绝、不真实执行。
        if is_write {
            if let Some(out) = goal_write_gate(core, rt, call) {
                emit_result(&sink, rt, run_id, &batch_id, call, &out, 0);
                outcomes[i] = Some((out, Vec::new()));
                continue;
            }
        }
        let core = core.clone();
        let rt = rt.clone();
        let call = call.clone();
        let batch_id = batch_id.clone();
        let cancel = cancel.clone();
        let conflict_i = conflict[i];
        let sem = rt.sem_tools.clone();
        let file_ops = rt.file_ops.clone();

        join.spawn(async move {
            if conflict_i {
                return (
                    i,
                    ToolOutcome::err(
                        "E_WRITE_BATCH_CONFLICT",
                        "同批次对同一文件的多次写入已被拒绝",
                    ),
                    Vec::new(),
                    0,
                );
            }
            // 并发闸（取消盲区修复）：等待闸期间 cancel 置位 → 立即放弃，不进闸
            let _guard = if is_write {
                let file = file_ops.clone();
                tokio::select! {
                    g = file.lock_owned() => Gate::File(g),
                    _ = cancel.cancelled() => {
                        return (i, cancelled_outcome(), Vec::new(), 0);
                    }
                }
            } else {
                let sem = sem.clone();
                tokio::select! {
                    p = sem.acquire_owned() => {
                        Gate::Parallel(p.expect("工具信号量不会关闭"))
                    }
                    _ = cancel.cancelled() => {
                        return (i, cancelled_outcome(), Vec::new(), 0);
                    }
                }
            };
            let (out, extra, dur) =
                run_tool(&core, &rt, &call, &batch_id, i, cancel, main_session).await;
            (i, out, extra, dur)
        });
    }

    loop {
        tokio::select! {
            res = join.join_next() => {
                let Some(res) = res else { break; };
                let Ok((i, out, extra, dur)) = res else {
                    continue;
                };
                let call = &calls[i];
                outcomes[i] = Some((out.clone(), extra));
                emit_result(&sink, rt, run_id, &batch_id, call, &out, dur);
            }
            _ = cancel.cancelled() => {
                // 取消收口：强杀全部在途任务（abort 触发 command 的 kill_on_drop 回收子进程、
                // guard drop 释放闸与写锁），未完成调用合成 E_CANCELLED，批次立即返回
                tracing::info!("session {run_id} 批次取消：abort 在途工具任务并合成取消结果");
                join.abort_all();
                while let Some(res) = join.join_next().await {
                    let Ok((i, out, extra, dur)) = res else { continue; };
                    let call = &calls[i];
                    outcomes[i] = Some((out.clone(), extra));
                    emit_result(&sink, rt, run_id, &batch_id, call, &out, dur);
                }
                for (i, slot) in outcomes.iter_mut().enumerate() {
                    if slot.is_none() {
                        let out = cancelled_outcome();
                        emit_result(&sink, rt, run_id, &batch_id, &calls[i], &out, 0);
                        *slot = Some((out, Vec::new()));
                    }
                }
                break;
            }
        }
    }

    // (4) 组装模型侧结果 + suggest 语义 + plan 批准语义
    // 会话生效模型是否勾选「支持图片输入」：决定模型侧文本怎么写、图片块是否随历史带下去
    let vision = {
        let cfg = core.cfg.read().unwrap();
        crate::core::prefs::effective_model(&cfg, &rt.prefs())
            .and_then(|m| m.vision)
            .unwrap_or(false)
    };
    let mut results = Vec::new();
    let mut suggest_items: Option<Vec<String>> = None;
    let mut plan_approved = false;
    let mut call_summary = Vec::with_capacity(calls.len());
    for (i, call) in calls.iter().enumerate() {
        let (out, extra) = outcomes[i].clone().unwrap_or_else(|| {
            (
                ToolOutcome::err("E_TOOL_PANIC", "工具执行异常终止"),
                Vec::new(),
            )
        });
        if call.name == "suggest" && out.ok {
            if let Some(items) = out.data["suggestions"].as_array() {
                suggest_items = Some(
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect(),
                );
            }
        }
        if call.name == "ask" && out.ok && out.data["plan_approved"].as_bool() == Some(true) {
            plan_approved = true;
        }
        call_summary.push(crate::core::agent::CallSig {
            name: call.name.clone(),
            error: out.error.as_ref().map(|e| e.code.clone()),
            args_hash: {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                call.args.to_string().hash(&mut h);
                h.finish()
            },
        });
        // 提醒不是独立 Text 块（wire 层会丢弃 Tool 消息里的非 ToolResult 块），
        // 而是追加到本 ToolResult 的 content 尾部随正文到达模型；
        // extra 里可能同时有图片块（read 读图）与文本（plan 软提醒 / edit 的修正说明），
        // 这里只取首个 Text 当提醒，图片由下面的分支单独收集
        let hint = extra.iter().find_map(|c| match c {
            Content::Text { text } => Some(text.clone()),
            _ => None,
        });
        results.push(model_content(core, call, &out, hint, vision));
        // 图片块（read 读图）跟在本条 ToolResult 之后进同一条工具消息：出网副本层
        // （agent/stream.rs::route_tool_images）再把它们搬进紧随其后的用户消息 —— 工具消息里的
        // 图片块两个协议的 convert_message 都会丢弃，而图片本体也绝不能挤进 ToolResult 文本
        //（那段 base64 按约 1.6 字符/token 计费，实测 944KB 截图 ≈ 79 万 token）。
        // 未勾选「支持图片输入」时不带下去：模型侧说明已如实写明看不到。
        if vision {
            results.extend(
                extra
                    .iter()
                    .filter(|c| matches!(c, Content::Image { .. }))
                    .cloned(),
            );
        }
    }
    BatchOutcome {
        results,
        suggest_items,
        plan_approved,
        call_summary,
    }
}

/// plan 纪律门三态判定（纯函数便于单测）：返回 Some(错误码) 表示拦截。
/// - todos 为空且是写工具 → E_PLAN_REQUIRED（实现类工作开始前必须先建计划）；
/// - todos 非空且全部 Completed 且是写工具 → E_PLAN_STALE（计划已收尾，继续写入属计划外工作）；
/// - 其余（存在 Pending/InProgress，或非写工具）→ 放行。
///
/// 目标档执行期的豁免（`goal_execute_phase`）在调用点判定：本函数保持纯函数、不读会话状态。
fn plan_gate_verdict(todos: &[crate::tools::plan::Todo], is_write: bool) -> Option<&'static str> {
    if !is_write {
        return None;
    }
    if todos.is_empty() {
        return Some("E_PLAN_REQUIRED");
    }
    if todos
        .iter()
        .all(|t| t.status == crate::tools::plan::TodoStatus::Completed)
    {
        return Some("E_PLAN_STALE");
    }
    None
}

/// plan 纪律软提醒（不阻断）：写工具成功后若计划非空且没有任何 InProgress 条目，
/// 返回要追加到该结果模型侧 ToolResult content 尾部的提醒文本（wire 层只透传
/// ToolResult，附加的 Text 块到不了模型，见 provider 三协议 Role::Tool 分支）。
/// 原子 CAS 保证每 run 至多一次；run 起点复位见 run_chat（与 injected_plan_snapshot_for_run 同位）。
fn maybe_emit_plan_hint(rt: &Arc<SessionRuntime>) -> Option<String> {
    let blocked = {
        let todos = rt.todos.lock().unwrap();
        todos.is_empty()
            || todos
                .iter()
                .any(|t| t.status == crate::tools::plan::TodoStatus::InProgress)
    };
    if blocked {
        return None;
    }
    if rt
        .plan_hint_emitted
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return None;
    }
    Some("\n计划提醒：当前计划没有进行中条目，完成后请用 plan 工具标记状态。".into())
}

/// 并发闸的二选一：文件写独占锁，或并行信号量许可。
enum Gate {
    /// 文件写闸：进程级 file_ops 独占锁。
    File(tokio::sync::OwnedMutexGuard<()>),
    /// 并行闸：工具信号量许可（上限 4）。
    Parallel(tokio::sync::OwnedSemaphorePermit),
}

/// 取消路径的统一合成结果（闸放弃 / 收口强杀后补齐）。
fn cancelled_outcome() -> ToolOutcome {
    ToolOutcome::err("E_CANCELLED", "命令被用户取消")
}

/// 单个工具的执行（含 MCP 分发），带 panic 兜底。返回（结果，模型侧附加内容，耗时 ms）。
/// 附加内容当前只有 plan 软提醒（0/1 条 Text 块，由批次层拼进 ToolResult content 尾部）。
async fn run_tool(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    call: &NormalizedCall,
    batch_id: &str,
    index: usize,
    cancel: CancellationToken,
    main_session: bool,
) -> (ToolOutcome, Vec<Content>, u128) {
    // MCP 工具分发：mcp__<server>__<tool>
    if call.name.starts_with("mcp__") {
        emit_tool_start(core, rt, call, batch_id, index, "running");
        let started = Instant::now();
        // 审批门：server 未声明 read_only / always_allow 时逐次确认。
        // 只阻塞该次 MCP 调用——其它会话与内置工具不受影响。
        if let Some((server, tool_name)) = core.mcp.approval_target(&rt.id, &call.name).await {
            emit_tool_start(core, rt, call, batch_id, index, "waiting");
            let auto_confirm = core.cfg.read().unwrap().approval.auto_confirm;
            let verdict = crate::safety::approval::confirm(
                rt,
                &core.sink,
                crate::safety::approval::ApprovalRequest {
                    title: format!("允许调用 MCP 工具 {tool_name}？"),
                    detail: format!(
                        "server：{server}\n工具：{tool_name}\n参数：{}",
                        serde_json::to_string_pretty(&call.args).unwrap_or_default()
                    ),
                    allow_always: true,
                    auto_confirm,
                },
                &cancel,
            )
            .await;
            if !verdict.approved {
                return (
                    ToolOutcome::err(
                        "E_MCP_DENIED",
                        format!("用户拒绝或未响应对 MCP 工具 {tool_name} 的调用"),
                    ),
                    Vec::new(),
                    started.elapsed().as_millis(),
                );
            }
            // 「总是允许」：持久化到该 server 所在作用域的条目（粒度 = server）
            if verdict.always {
                if let Some(scope) = core.mcp.server_scope(&rt.id, &server).await {
                    let sref = match scope {
                        crate::mcp::Scope::Global => Some(crate::mcp::global_scope(&rt.data_dir)),
                        crate::mcp::Scope::Project => {
                            rt.project_dir.as_deref().map(crate::mcp::project_scope)
                        }
                    };
                    if let Some(sref) = sref {
                        if let Err(e) = crate::mcp::set_always_allow(&sref, &server, true) {
                            tracing::warn!("写回 always_allow 失败：{e}");
                        }
                    }
                }
            }
        }
        let mcp = core.mcp.clone();
        let sid = rt.id.clone();
        let fname = call.name.clone();
        let args = call.args.clone();
        let call_cancel = cancel.clone();
        let out =
            match tokio::spawn(async move { mcp.call(&sid, &fname, args, &call_cancel).await })
                .await
            {
                Ok(Ok(o)) => {
                    // server 侧 is_error 显式落成失败结果（不再折叠进 Ok 里丢掉语义）；
                    // 多模态内容（图片等）走「仅注入给模型」的通道，不进前端 outcome JSON。
                    let crate::mcp::McpCallOutput {
                        data,
                        text,
                        is_error,
                        blocks,
                    } = o;
                    let mut outcome = if is_error {
                        ToolOutcome::err("E_MCP_TOOL", text)
                    } else {
                        ToolOutcome::ok(data)
                    };
                    outcome.extra_model_content = blocks;
                    outcome
                }
                Ok(Err(e)) => ToolOutcome::err(
                    match e.kind {
                        crate::mcp::McpErrorKind::Cancelled => "E_MCP_CANCELLED",
                        _ => "E_MCP",
                    },
                    e.message,
                ),
                Err(e) => ToolOutcome::err("E_TOOL_PANIC", format!("MCP 调用异常：{e}")),
            };
        return (out, Vec::new(), started.elapsed().as_millis());
    }
    let Some(tool) = core.tools.get(&call.name) else {
        return (
            ToolOutcome::err(
                "E_UNKNOWN_TOOL",
                format!("未知工具：{}（可用工具见系统提示词工具列表）", call.name),
            ),
            Vec::new(),
            0,
        );
    };
    let ctx = ToolCtx {
        core: core.clone(),
        rt: rt.clone(),
        batch_id: batch_id.to_string(),
        call_index: index,
        call_key: format!("{batch_id}:{index}"),
        cancel,
    };
    // ConfirmEach（先确认后变更）：FileWrite 工具执行前必经审批（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）。
    let needs_write_approval = ctx.approval_mode() == crate::core::prefs::ApprovalMode::ConfirmEach
        && tool.kind() == ToolKind::FileWrite;
    // G3 范围门（[docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md) §7）：批准后新增的计划外步骤，首次写入前弹范围确认。
    // 覆盖两条写通道（R2 评审修复）：FileWrite 工具；携带写目标的 command 工具。
    // 后者用专门的「写目标探针」策略（confirm_inside_writes=true），与执行档判定解耦：
    // 批准后会话已切 AutoEdit，其执行档 fence 会把范围内写重定向判为 Allow；若无探针，
    // shell 重定向可完全绕过确认。纯只读命令探针得 Allow，不打扰。
    // 批准 = 本会话放行（后续新增静默纳入）；拒绝 = 保留标记，下次写入再问。基线缺失（异常路径）不阻塞。
    //
    // 封成闭包而非提前求值：门 1 的审批是 await，同批并发写可能在该窗口改动 scope_expanded /
    // approved_plan，提前求值会让安全门 fail-open（等一次审批的工夫把 G3 条件判成 false）。
    // 「发 waiting 相」与「门本体」两处各调用一次，既是单一真相（共用同一份表达式），又不改变求值时点。
    let g3_gate = |ctx: &ToolCtx| -> bool {
        let g3_applies = match tool.kind() {
            ToolKind::FileWrite => true,
            ToolKind::Network => false,
            _ => {
                // command 工具：检测到写目标才需要确认
                if call.name == "command" {
                    if let Some(cmd) = call.args["command"].as_str() {
                        let probe = crate::safety::fence::FencePolicy {
                            approval_enabled: true,
                            confirm_outside_create: ctx.confirm_outside_create(),
                            confirm_inside_writes: true,
                            plan_readonly: false,
                        };
                        matches!(
                            crate::safety::fence::check_command_policy(
                                cmd,
                                &ctx.rt.workspace,
                                &ctx.write_roots(),
                                probe,
                            ),
                            crate::safety::fence::Verdict::Confirm(_)
                        )
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        };
        g3_applies
            // 目标档显式关闭 G3（双保险：目标档批准路径本就不冻结 approved_plan 基线）：
            // 执行期的范围控制由账本承担（越界即拒），不弹「计划外步骤确认」。
            && ctx.approval_mode() != crate::core::prefs::ApprovalMode::Goal
            && ctx
                .rt
                .scope_expanded
                .load(std::sync::atomic::Ordering::SeqCst)
            && !ctx
                .rt
                .scope_allowed
                .load(std::sync::atomic::Ordering::SeqCst)
            && ctx.rt.approved_plan.lock().unwrap().is_some()
    };
    // 门之前：本次调用若还要等确认（写入审批 / 计划外步骤范围确认），先把卡片建出来——
    // 否则审批弹框期间界面上什么都没有（用户看不到这次调用与它的参数）。
    if needs_write_approval || g3_gate(&ctx) {
        emit_tool_start(core, rt, call, batch_id, index, "waiting");
    }
    // detail 优先用工具的语义化预览（edit/create 的变更 diff，[docs/tools-optimization-and-gap-fill-plan](../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 3）；
    // 无预览能力的工具（如 delete）回退为入参 JSON 展示。
    if needs_write_approval {
        let detail = tool
            .approval_detail(&ctx, &call.args)
            .await
            .unwrap_or_else(|| serde_json::to_string_pretty(&call.args).unwrap_or_default());
        let auto_confirm = ctx.core.cfg.read().unwrap().approval.auto_confirm;
        let ok = crate::safety::approval::confirm(
            &ctx.rt,
            &ctx.core.sink,
            crate::safety::approval::ApprovalRequest {
                title: format!("文件写入确认：{}", call.name),
                detail,
                allow_always: false,
                auto_confirm,
            },
            &ctx.cancel,
        )
        .await;
        if !ok.approved {
            return (
                ToolOutcome::err("E_APPROVAL_DENIED", "用户拒绝或未响应该写入"),
                Vec::new(),
                0,
            );
        }
    }
    if g3_gate(&ctx) {
        let new_titles: Vec<String> = {
            let approved = ctx.rt.approved_plan.lock().unwrap();
            let todos = ctx.rt.todos.lock().unwrap();
            crate::tools::plan::diff_new_todos(
                approved.as_deref().unwrap_or(&[]),
                &todos.iter().map(|t| t.title.clone()).collect::<Vec<_>>(),
            )
        };
        // Y1（评审）：模型已移除新增 todos（diff 为空）→ 状态与基线一致；清标记放行，不弹空列表
        if new_titles.is_empty() {
            ctx.rt
                .scope_expanded
                .store(false, std::sync::atomic::Ordering::SeqCst);
        } else {
            let auto_confirm = ctx.core.cfg.read().unwrap().approval.auto_confirm;
            let ok = crate::safety::approval::confirm(
                &ctx.rt,
                &ctx.core.sink,
                crate::safety::approval::ApprovalRequest {
                    title: "计划外步骤确认".into(),
                    detail: format!(
                        "执行中出现已批准方案之外的新步骤：\n{}\n\n即将执行：{} {}\n批准 = 本会话允许后续新增步骤；拒绝 = 模型需收窄范围。",
                        new_titles.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n"),
                        call.name,
                        serde_json::to_string(&call.args).unwrap_or_default(),
                    ),
                    allow_always: false,
                    auto_confirm,
                },
                &ctx.cancel,
            )
            .await;
            if ok.approved {
                ctx.rt
                    .scope_allowed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                ctx.rt
                    .scope_expanded
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            } else {
                ctx.rt
                    .scope_denials
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return (
                    ToolOutcome::err(
                        "E_SCOPE_DENIED",
                        "用户拒绝了计划外步骤的写入。请收窄到已批准的 todos 范围，或向用户说明理由后重试。",
                    ),
                    Vec::new(),
                    0,
                );
            }
        }
    }
    // 门全部通过、真正要执行了：发 running 相，让前端把「等待确认」翻成「正在运行」
    emit_tool_start(core, rt, call, batch_id, index, "running");
    let warnings = collect_unknown_fields(&call.args, tool.schema());
    let started = Instant::now();

    let args = call.args.clone();
    let tool_kind = tool.kind();
    let fut = async move { tool.run(&ctx, args).await };
    let joined = tokio::spawn(futures::FutureExt::catch_unwind(
        std::panic::AssertUnwindSafe(fut),
    ))
    .await;

    let mut outcome = match joined {
        Ok(Ok(out)) => out,
        Ok(Err(_panic)) => ToolOutcome::err(
            "E_TOOL_PANIC",
            "工具执行 panic，已捕获（进程与历史未受影响）",
        ),
        Err(join_err) => ToolOutcome::err("E_TOOL_PANIC", format!("工具任务异常终止：{join_err}")),
    };
    if !warnings.is_empty() {
        outcome = outcome.with_warnings(warnings);
    }
    // plan 纪律软提醒（不阻断）：仅主会话（子代理豁免 plan 纪律，提醒对无 plan 工具的
    // 子代理无意义）；写工具成功且计划没有进行中条目时，一次性追加到模型侧结果尾部
    let mut extra: Vec<Content> = outcome.extra_model_content.clone();
    if main_session && tool_kind == ToolKind::FileWrite && outcome.error.is_none() {
        if let Some(hint) = maybe_emit_plan_hint(rt) {
            extra.push(Content::Text { text: hint });
        }
    }
    (outcome, extra, started.elapsed().as_millis())
}

/// 把工具结果包装为模型侧 ToolResult 内容（经 compact 瘦身，错误结果带 is_error 标记）。
/// model_hint_non_empty：非空时追加到 content 尾部（plan 软提醒走 ToolResult 通道到达模型）。
fn model_content(
    core: &Arc<AgentCore>,
    call: &NormalizedCall,
    out: &ToolOutcome,
    model_hint_non_empty: Option<String>,
    vision: bool,
) -> Content {
    let kind = core
        .tools
        .get(&call.name)
        .map(|t| t.kind())
        .unwrap_or(ToolKind::Meta);
    let mut text = crate::tools::compact::compact_for_model(kind, &call.name, out, vision);
    if let Some(hint) = model_hint_non_empty {
        text.push_str(&hint);
    }
    Content::ToolResult {
        tool_use_id: call.id.clone(),
        content: text,
        is_error: !out.ok,
    }
}

/// 入参 JSON 预览串（`tool:start` 与 `tool:result` / `tool:error` 共用，行为逐字节一致）。
/// H3 修复：截断必须保持 JSON 可解析（此前硬切 2000 字符会悄悄打断前端 diff）。
fn args_preview_json(args: &serde_json::Value) -> String {
    let s = serde_json::to_string(args).unwrap_or_default();
    if s.len() <= 200_000 {
        s
    } else {
        serde_json::json!({ "_args_truncated": true, "hint": "参数过大，前端不展示 diff" })
            .to_string()
    }
}

/// 向前端发一条 `tool:start` 事件：前端建工具卡的**唯一来源**，`phase` 区分「等确认」与「已开始执行」。
///
/// 为什么需要：前端建「运行中」工具卡的唯一实时来源此前是 `tool_progress` 帧，而只有 `command`
/// 工具在输出累计 ≥2KB 时才会发帧——于是读文件 / 搜索 / 改文件 / 计划 / 子代理这类调用
/// 在整个执行期间界面上什么都没有，卡片只会在 `tool:result` 到达（即调用结束）时才冒出来
/// （用户报「工具调用有时在调用结束后才显示」）。现在由本事件承担建卡，前端立刻按 `tool` 建卡。
///
/// 与 `tool:result` / `tool:error` 的关系（同一张卡绝不能有第二个 key）：
/// - `call_key` = `<batch_id>:<批内位置>`，批内位置在 `execute_batch` 入口由 provider 侧下标收敛而来，
///   与结果事件的 `call_key`、`ToolCtx.call_key`、command 工具进度帧的 index 全部同源；
/// - `phase = "waiting"` 在审批 / 范围确认门之前发出——弹框期间界面上已经有卡片与参数；
///   `phase = "running"` 在门全部通过、真正要执行时发出，把同一张卡从「等待确认」翻成「正在运行」；
/// - `args_preview` 让运行中即可看到入参（前端 diff 预览的数据源）。
fn emit_tool_start(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    call: &NormalizedCall,
    batch_id: &str,
    index: usize,
    phase: &str,
) {
    core.sink.emit(
        &rt.id,
        "tool:start",
        serde_json::json!({
            "session": rt.id,
            "batch_id": batch_id,
            "call_index": index,
            "call_key": format!("{batch_id}:{index}"),
            "tool": call.name,
            "args_preview": args_preview_json(&call.args),
            "phase": phase,
        }),
    );
}

/// 向前端发出 tool:result / tool:error 事件并写会话日志（每个工具调用一行）。
#[allow(clippy::too_many_arguments)]
fn emit_result(
    sink: &Arc<dyn EventSink>,
    rt: &Arc<SessionRuntime>,
    run_id: &str,
    batch_id: &str,
    call: &NormalizedCall,
    out: &ToolOutcome,
    duration_ms: u128,
) {
    let event = if out.ok { "tool:result" } else { "tool:error" };
    // [docs/session-logging-report](../../../docs/session-logging-report.md) 会话日志：每个工具调用一行（覆盖策略拒绝 / 未知工具 / panic 兜底，全路径）
    session_log::info(
        rt,
        &format!(
            "tool {} {} 耗时 {duration_ms}ms args={}",
            call.name,
            if out.ok { "ok" } else { "ERR" },
            session_log::trunc(&serde_json::to_string(&call.args).unwrap_or_default(), 400),
        ),
    );
    if let Some(err) = &out.error {
        session_log::warn(
            rt,
            &format!(
                "tool {} 失败 [{}]：{}",
                call.name,
                err.code,
                session_log::trunc(&err.message, 400)
            ),
        );
        tracing::warn!(
            "session {} tool {} 失败 [{}]：{}",
            rt.id,
            call.name,
            err.code,
            err.message
        );
    }
    let args_preview = args_preview_json(&call.args);
    sink.emit(
        &rt.id,
        event,
        serde_json::json!({
            "session": rt.id, "run_id": run_id,
            "batch_id": batch_id, "call_index": call.index,
            "call_key": format!("{batch_id}:{}", call.index),
            "tool": call.name,
            "args_preview": args_preview,
            "outcome": out,
            "duration_ms": duration_ms,
        }),
    );
}

/// 目标档执行期的写入账本门（判定 + 拒绝计数副作用）：任一写入目标越界即返回拒绝结果。
/// 非目标档 / 澄清期一律 None（绝不干预其它档位）；未解析出目标路径时也放行
///（参数畸形由工具自身报 E_ARGS）。
///
/// 判定用**账本作用域 runtime**（`goal_gate_rt`）：子代理 runtime 自身不持有目标状态，
/// 直接读它会把「子代理在目标执行期写账本外路径」放行——目标状态一律取根会话。
fn goal_write_gate(
    core: &AgentCore,
    rt: &Arc<SessionRuntime>,
    call: &NormalizedCall,
) -> Option<ToolOutcome> {
    use crate::core::agent::goal::{
        LedgerTarget, goal_execute_phase, goal_gate_rt, ledger_denial_message, ledger_gate,
    };
    let gov = goal_gate_rt(core, rt);
    if !goal_execute_phase(&gov) {
        return None;
    }
    for p in canonical_arg_paths(&rt.workspace, &call.args) {
        if let Err(code) = ledger_gate(&gov, LedgerTarget::Path(&p)) {
            return Some(ToolOutcome::err(code, ledger_denial_message(&gov, &p)));
        }
    }
    None
}

/// 从工具入参中提取候选写路径（批次写冲突检测用）。
fn canonical_arg_paths(workspace: &std::path::Path, args: &serde_json::Value) -> Vec<String> {
    // M1 修复：相对路径先拼到工作区再规范化（此前相对进程 cwd，漏判冲突）
    let resolve = |p: &str| -> String {
        let joined = workspace.join(p.trim_start_matches("./"));
        crate::tools::pathutil::canonical_best_effort(&joined)
            .to_string_lossy()
            .into_owned()
    };
    let mut out = Vec::new();
    if let Some(p) = args["path"].as_str() {
        out.push(resolve(p));
    }
    if let Some(files) = args["files"].as_array() {
        for f in files {
            if let Some(p) = f["path"].as_str() {
                out.push(resolve(p));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 批次取消盲区修复回归：批次执行中途置位取消 → 收口 abort 全部在途任务，
    /// 所有未完成调用合成 E_CANCELLED 结果（批次必有完整结果，不留悬空 tool_use）。
    #[tokio::test]
    async fn mid_batch_cancel_synthesizes_results_for_all_calls() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "bcancel",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        // 两条长命令（sleep 30s）：收口取消必须立即返回而非等 sleep 走完
        let mk = |id: &str, index: usize| crate::core::agent::NormalizedCall {
            id: id.into(),
            name: "command".into(),
            args: serde_json::json!({"command": "Start-Sleep -Seconds 30", "timeoutSeconds": 60, "fullOutput": true}),
            index,
        };
        let calls = vec![mk("c1", 0), mk("c2", 1)];
        let cancel = tokio_util::sync::CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let out = execute_batch(&core, &rt, calls, &[], false, false, cancel, "run1").await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "取消后批次应立即返回，而非等 sleep(30s)/超时走完"
        );
        // 全部调用都有结果且为取消错误（历史不留悬空 tool_use）
        assert_eq!(out.results.len(), 2, "每个调用都必须有结果");
        let errs: Vec<String> = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(errs.len(), 2, "两个调用都应为取消错误: {errs:?}");
        assert!(
            errs.iter()
                .all(|e| e.contains("E_CANCELLED") || e.contains("E_PLAN_READONLY")),
            "两个调用都应在取消时立即终止（either 合成取消结果或 fence/计划门拦截，绝不能是 30s sleep 走完的成功结果）: {errs:?}"
        );
        // 监督摘要同样覆盖全部调用（call_summary 与 results 同长）
        assert_eq!(out.call_summary.len(), 2);
    }

    /// 帧与事件的到达顺序记录（`make_core` 用的是 NoopSink，观察不到帧）。
    type Log = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    struct RecordingSink(Log);

    impl crate::core::agent::EventSink for RecordingSink {
        fn channel_frame(&self, _s: &crate::core::types::SessionId, f: &crate::core::agent::Frame) {
            let tag = match f {
                crate::core::agent::Frame::ToolProgress { name, chunk, .. } => format!(
                    "frame:tool_progress:{name}:{}",
                    if chunk.is_empty() { "empty" } else { "chunk" }
                ),
                other => format!("frame:{other:?}"),
            };
            self.0.lock().unwrap().push(tag);
        }
        fn emit(&self, _s: &crate::core::types::SessionId, e: &str, p: serde_json::Value) {
            let mut entries = self.0.lock().unwrap();
            entries.push(format!("event:{e}"));
            // 加性扩展（既有精确断言不变）：把事件载荷里的调用点信息也记下来。
            // payload 缺字段用 `-` 占位（tool:result / tool:error 没有 phase）。
            entries.push(format!(
                "eventdetail:{e}:{}:{}",
                p["call_key"].as_str().unwrap_or("-"),
                p["phase"].as_str().unwrap_or("-")
            ));
            entries.push(format!(
                "eventargs:{e}:{}",
                p["args_preview"].as_str().unwrap_or("-")
            ));
        }
    }

    /// 从 `eventdetail:<event>:<call_key>:<phase>` 条目解析 (call_key, phase) 列表；
    /// call_key 形如 `<batch_id>:<批内位置>`（batch_id 无冒号，rsplit 安全）。
    fn event_details(entries: &[String], event: &str) -> Vec<(String, String)> {
        let prefix = format!("eventdetail:{event}:");
        entries
            .iter()
            .filter_map(|e| {
                let rest = e.strip_prefix(&prefix)?;
                let (key, phase) = rest.rsplit_once(':')?;
                Some((key.to_string(), phase.to_string()))
            })
            .collect()
    }

    /// 带记录事件汇的测试核心 + 已切 AutoEdit 的 runtime。
    /// 临时目录有意泄漏（`Box::leak`）：core/rt 在整个用例里要用它们，不能随函数返回被删。
    fn recording_core(session: &str) -> (Arc<AgentCore>, Arc<SessionRuntime>, Log) {
        let ws = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let dd = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let log: Log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut cfg = crate::core::config::ConfigState::default();
        cfg.providers.push(crate::core::config::ProviderConfig {
            models: vec![crate::core::config::ProviderModel::default()],
            ..Default::default()
        });
        cfg.active_model_id = Some(cfg.providers[0].models[0].id.clone());
        let store = Arc::new(crate::core::sessions::SessionStore::new(
            roots.data_dir.clone(),
        ));
        let core = Arc::new(AgentCore::new(
            cfg,
            Arc::new(RecordingSink(log.clone())),
            store,
            reqwest::Client::new(),
            roots.data_dir.clone(),
        ));
        let rt = core.get_or_create_session(
            session,
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::AutoEdit,
            model_id: None,
            reasoning_effort: None,
        });
        (core, rt, log)
    }

    /// 回归（用户报「工具调用有时在调用结束后才显示」）：**非 command 工具**也必须能实时建卡——
    /// 执行前先发一条 `tool:start` 事件（running 相），且它必须早于结果事件。
    /// （原断言的空 chunk `tool_progress` 起始帧已移除：建卡职责改由本事件承担。）
    #[tokio::test]
    async fn non_command_tool_emits_start_event_before_result() {
        let (core, rt, log) = recording_core("tstart");
        let call = NormalizedCall {
            id: "c1".into(),
            name: "calculate".into(),
            args: serde_json::json!({ "expression": "1+1" }),
            index: 0,
        };
        // 走真实批次入口：结果事件由批次层（emit_result）发出，直接调 run_tool 看不到它
        let res = execute_batch(
            &core,
            &rt,
            vec![call],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert_eq!(res.results.len(), 1, "批次必有结果");

        let entries = log.lock().unwrap().clone();
        let start = entries
            .iter()
            .position(|e| e.starts_with("eventdetail:tool:start:") && e.ends_with(":running"))
            .unwrap_or_else(|| panic!("缺 tool:start（running 相）事件：{entries:?}"));
        let result = entries
            .iter()
            .position(|e| e == "event:tool:result")
            .unwrap_or_else(|| panic!("缺结果事件：{entries:?}"));
        assert!(start < result, "tool:start 必须早于结果事件：{entries:?}");
    }

    /// 核心回归（工具卡重复且卡死）：同一张卡的两条通道必须共用同一个 `call_key`。
    /// 故意把 `NormalizedCall.index` 设为 5 / 9（模拟 anthropic 的 `content_block_start`
    /// 内容块下标被 thinking / text 顶偏），断言 `execute_batch` 入口已把它收敛为批内位置：
    /// `tool:start` 与 `tool:result` 的 call_key 逐一相等，且 provider 侧下标不出现在任何 key 上。
    #[tokio::test]
    async fn start_event_and_result_share_same_call_key() {
        let (core, rt, log) = recording_core("tkey");
        let mk = |id: &str, expr: &str, index: usize| NormalizedCall {
            id: id.into(),
            name: "calculate".into(),
            args: serde_json::json!({ "expression": expr }),
            index,
        };
        // 走真实批次入口：call_key 的收敛点就在 execute_batch 开头
        let out = execute_batch(
            &core,
            &rt,
            vec![mk("c1", "1+1", 5), mk("c2", "2+2", 9)],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert_eq!(out.results.len(), 2, "批次必有结果");

        let entries = log.lock().unwrap().clone();
        let starts = event_details(&entries, "tool:start");
        let results = event_details(&entries, "tool:result");
        assert_eq!(starts.len(), 2, "两个调用各一条 tool:start：{entries:?}");
        assert_eq!(results.len(), 2, "两个调用各一条 tool:result：{entries:?}");
        assert!(
            starts.iter().all(|(_, phase)| phase.as_str() == "running"),
            "无门批次的两条 tool:start 都是 running 相：{starts:?}"
        );
        let batch = starts[0]
            .0
            .rsplit_once(':')
            .map(|(b, _)| b.to_string())
            .expect("call_key 形如 <batch_id>:<批内位置>");
        let (key0, key1) = (format!("{batch}:0"), format!("{batch}:1"));
        let mut start_keys: Vec<&str> = starts.iter().map(|(k, _)| k.as_str()).collect();
        let mut result_keys: Vec<&str> = results.iter().map(|(k, _)| k.as_str()).collect();
        start_keys.sort();
        result_keys.sort();
        assert_eq!(
            start_keys,
            vec![key0.as_str(), key1.as_str()],
            "call_key 必须是批内位置 0 / 1：{entries:?}"
        );
        assert_eq!(
            start_keys, result_keys,
            "同一张卡的两条通道必须逐一共用同一 call_key：{entries:?}"
        );
        // provider 侧下标（5 / 9）不得泄漏
        assert!(
            !entries
                .iter()
                .any(|e| e.contains(&format!("{batch}:5")) || e.contains(&format!("{batch}:9"))),
            "provider 侧下标不得出现在前端 key 上：{entries:?}"
        );
    }

    /// ConfirmEach 档下审批弹框期间界面上必须有卡片与参数：写工具门之前先发 waiting 相、放行后再发
    /// running 相；同批只读工具没有门，只发 running。
    #[tokio::test]
    async fn start_event_phase_is_waiting_when_confirm_required() {
        let (core, rt, log) = recording_core("twait");
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::ConfirmEach,
            model_id: None,
            reasoning_effort: None,
        });
        // 写工具指向 workspace 内的真实文件（edit 要求文件存在）
        let target = rt.workspace.join("wait_target.txt");
        std::fs::write(&target, b"hello\n").unwrap();
        let edit = NormalizedCall {
            id: "c1".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "files": [{
                    "path": target.to_string_lossy(),
                    "changes": [{ "oldText": "hello", "newText": "X" }],
                }]
            }),
            index: 0,
        };
        let calc = NormalizedCall {
            id: "c2".into(),
            name: "calculate".into(),
            args: serde_json::json!({ "expression": "1+1" }),
            index: 1,
        };
        // 审批门无人应答即永久等待（auto_confirm 默认 false）：另起任务轮询 asks 表，出现即批准，
        // 否则用例会在审批门上永久挂起
        let rt_answer = rt.clone();
        let answerer = tokio::spawn(async move {
            for _ in 0..1000 {
                let id = rt_answer.asks.lock().unwrap().keys().next().cloned();
                if let Some(id) = id {
                    return rt_answer.resolve_ask(&id, serde_json::json!({ "approved": true }));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            false
        });
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_batch(
                &core,
                &rt,
                vec![edit, calc],
                &[],
                false,
                false, // 非主会话：跳过 plan 纪律硬门，本用例专测审批门两相
                tokio_util::sync::CancellationToken::new(),
                "run1",
            ),
        )
        .await
        .expect("审批被应答后批次必须返回（不得永久挂起）");
        assert!(answerer.await.unwrap(), "审批请求必须出现并被应答");
        assert_eq!(out.results.len(), 2, "批次必有结果");

        let entries = log.lock().unwrap().clone();
        let waiting = entries
            .iter()
            .position(|e| e.starts_with("eventdetail:tool:start:") && e.ends_with(":waiting"))
            .unwrap_or_else(|| panic!("写工具在审批门之前必须发 waiting 相：{entries:?}"));
        let write_key = entries[waiting]
            .strip_prefix("eventdetail:tool:start:")
            .and_then(|r| r.rsplit_once(':'))
            .map(|(k, _)| k.to_string())
            .expect("waiting 条目形如 eventdetail:tool:start:<call_key>:waiting");
        let running = entries
            .iter()
            .position(|e| e == &format!("eventdetail:tool:start:{write_key}:running"))
            .unwrap_or_else(|| panic!("放行后必须发 running 相：{entries:?}"));
        assert!(waiting < running, "waiting 必须先于 running：{entries:?}");
        // 只读工具无门：只有 running
        let starts = event_details(&entries, "tool:start");
        let read_keys: Vec<String> = starts
            .iter()
            .filter(|(k, _)| *k != write_key)
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(
            read_keys.len(),
            1,
            "同批只读工具也应发一条 tool:start：{entries:?}"
        );
        let read_key = &read_keys[0];
        assert!(
            entries.contains(&format!("eventdetail:tool:start:{read_key}:running")),
            "只读工具应发 running 相：{entries:?}"
        );
        assert!(
            !entries.contains(&format!("eventdetail:tool:start:{read_key}:waiting")),
            "只读工具不经审批门，不应有 waiting 相：{entries:?}"
        );
        // 审批放行后写入真实发生（两相语义与真实执行路径一致）
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "X\n");
    }

    /// 反向回归（拒绝路径不得留下「正在运行」的假象）：审批被**拒绝**时只发 waiting 相，
    /// 绝不能再发同 key 的 running 相——否则同一张卡会永远转圈，且结果到达前界面谎称已在执行。
    #[tokio::test]
    async fn gate_denied_start_event_never_reaches_running() {
        let (core, rt, log) = recording_core("tdeny");
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::ConfirmEach,
            model_id: None,
            reasoning_effort: None,
        });
        // 写工具指向 workspace 内的真实文件（edit 要求文件存在）；拒绝后它必须原封不动
        let target = rt.workspace.join("deny_target.txt");
        std::fs::write(&target, b"hello\n").unwrap();
        let edit = NormalizedCall {
            id: "c1".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "files": [{
                    "path": target.to_string_lossy(),
                    "changes": [{ "oldText": "hello", "newText": "X" }],
                }]
            }),
            index: 0,
        };
        // 审批门无人应答即永久等待（auto_confirm 默认 false）：另起任务轮询 asks 表，出现即**拒绝**
        // （拒绝应答形状由 safety/approval.rs 的 `v["approved"].as_bool()` 决定：`{"approved": false}`）
        let rt_answer = rt.clone();
        let answerer = tokio::spawn(async move {
            for _ in 0..1000 {
                let id = rt_answer.asks.lock().unwrap().keys().next().cloned();
                if let Some(id) = id {
                    return rt_answer.resolve_ask(&id, serde_json::json!({ "approved": false }));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            false
        });
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            execute_batch(
                &core,
                &rt,
                vec![edit],
                &[],
                false,
                false, // 非主会话：跳过 plan 纪律硬门，本用例专测审批拒绝路径
                tokio_util::sync::CancellationToken::new(),
                "run1",
            ),
        )
        .await
        .expect("拒绝应答后批次必须返回（不得永久挂起）");
        assert!(answerer.await.unwrap(), "审批请求必须出现并被应答");
        assert_eq!(out.results.len(), 1, "批次必有结果");

        let entries = log.lock().unwrap().clone();
        let starts = event_details(&entries, "tool:start");
        assert_eq!(
            starts.len(),
            1,
            "被拒绝的调用只应有 waiting 相一条 tool:start：{entries:?}"
        );
        let (deny_key, phase) = &starts[0];
        assert_eq!(
            phase.as_str(),
            "waiting",
            "门之前必须是 waiting 相：{entries:?}"
        );
        assert!(
            !entries.contains(&format!("eventdetail:tool:start:{deny_key}:running")),
            "门被拒绝绝不能补发 running 相（否则卡片永远转圈）：{entries:?}"
        );
        // 结果通道：拒绝落为 tool:error / E_APPROVAL_DENIED
        assert_eq!(
            event_details(&entries, "tool:error").len(),
            1,
            "拒绝必须发 tool:error：{entries:?}"
        );
        assert!(
            event_details(&entries, "tool:result").is_empty(),
            "拒绝不得发成功结果：{entries:?}"
        );
        let errs: Vec<String> = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(errs.len(), 1, "拒绝必须落为错误结果：{errs:?}");
        assert!(
            errs[0].contains("E_APPROVAL_DENIED"),
            "拒绝的错误码必须是 E_APPROVAL_DENIED：{errs:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "hello\n",
            "审批被拒绝时写入不得发生"
        );
    }

    /// 反向回归（批层硬门不建卡）：被 exclude_tools 硬门在 spawn 前拒绝的调用只有结果事件，
    /// 一条 `tool:start` 都不许发——工具从未开始执行，前端不该为它建卡。
    #[tokio::test]
    async fn batch_level_rejection_emits_no_start_event() {
        let (core, rt, log) = recording_core("tblock");
        let call = NormalizedCall {
            id: "c1".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "files": [{
                    "path": "blocked.txt",
                    "changes": [{ "oldText": "a", "newText": "b" }],
                }]
            }),
            index: 0,
        };
        // 排除集 = plan 档参数（与模型工具列表同源）：批层硬门在 spawn 前拒绝
        let out = execute_batch(
            &core,
            &rt,
            vec![call],
            &["edit".to_string()],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert_eq!(out.results.len(), 1, "批次必有结果");
        let err = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .next()
            .expect("被排除工具应被硬门拒绝");
        assert!(err.contains("E_TOOL_BLOCKED"), "{err}");

        let entries = log.lock().unwrap().clone();
        assert!(
            event_details(&entries, "tool:start").is_empty(),
            "批层硬门拒绝的调用不得发任何相位的 tool:start：{entries:?}"
        );
        assert_eq!(
            event_details(&entries, "tool:error").len(),
            1,
            "硬门拒绝仍必须有结果事件：{entries:?}"
        );
    }

    /// 反向回归（未知工具不建卡）：幻觉出的不存在的工具名在注册表查找即失败，
    /// 只有 E_UNKNOWN_TOOL 结果事件，不得发任何 `tool:start`。
    #[tokio::test]
    async fn unknown_tool_emits_no_start_event() {
        let (core, rt, log) = recording_core("tunknown");
        let call = NormalizedCall {
            id: "c1".into(),
            name: "definitely_not_a_tool".into(),
            args: serde_json::json!({}),
            index: 0,
        };
        let out = execute_batch(
            &core,
            &rt,
            vec![call],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert_eq!(out.results.len(), 1, "批次必有结果");
        let err = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .next()
            .expect("未知工具必须落为错误结果");
        assert!(err.contains("E_UNKNOWN_TOOL"), "{err}");

        let entries = log.lock().unwrap().clone();
        assert!(
            event_details(&entries, "tool:start").is_empty(),
            "未知工具不得发任何相位的 tool:start：{entries:?}"
        );
        assert_eq!(
            event_details(&entries, "tool:error").len(),
            1,
            "未知工具必须有结果事件：{entries:?}"
        );
    }

    /// 回归（空 chunk 起始帧已移除）：非 command 工具不再靠「空进度帧」建卡，
    /// 因此 calculate 这类无流式输出的工具**一条 tool_progress 帧都不发**（更不会有 empty 帧）——
    /// 若空帧回来，卡片会出现两条建卡通道（事件 + 帧），前端又会建出重复卡。
    #[tokio::test]
    async fn progress_frame_only_carries_real_output_chunks() {
        let (core, rt, log) = recording_core("tframe");
        let call = NormalizedCall {
            id: "c1".into(),
            name: "calculate".into(),
            args: serde_json::json!({ "expression": "1+1" }),
            index: 0,
        };
        let (out, _, _) = run_tool(
            &core,
            &rt,
            &call,
            "b1",
            0,
            tokio_util::sync::CancellationToken::new(),
            false,
        )
        .await;
        assert!(out.ok, "{out:?}");
        // command 的帧由独立任务异步发出：留出窗口，确保结论是「根本不发」而非「还没发到」
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let entries = log.lock().unwrap().clone();
        assert!(
            !entries
                .iter()
                .any(|e| e.starts_with("frame:tool_progress:") && e.ends_with(":empty")),
            "空 chunk 起始帧已移除，不得再出现：{entries:?}"
        );
        assert!(
            !entries
                .iter()
                .any(|e| e.starts_with("frame:tool_progress:calculate:")),
            "无流式输出的工具不该有进度帧（建卡职责已归 tool:start）：{entries:?}"
        );
    }

    /// `tool:start` 必须携带入参预览（运行中即可看到参数 / 前端 diff 预览的数据源）。
    #[tokio::test]
    async fn start_event_carries_args_preview() {
        let (core, rt, log) = recording_core("targs");
        let call = NormalizedCall {
            id: "c1".into(),
            name: "calculate".into(),
            args: serde_json::json!({ "expression": "1+1" }),
            index: 0,
        };
        let out = execute_batch(
            &core,
            &rt,
            vec![call],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert_eq!(out.results.len(), 1, "批次必有结果");
        let entries = log.lock().unwrap().clone();
        let raw = entries
            .iter()
            .find_map(|e| e.strip_prefix("eventargs:tool:start:"))
            .unwrap_or_else(|| panic!("缺 tool:start 的 args_preview：{entries:?}"));
        let v: serde_json::Value =
            serde_json::from_str(raw).expect("args_preview 必须是合法 JSON（前端 diff 直接解析）");
        assert_eq!(v["expression"].as_str(), Some("1+1"), "{v}");
    }

    /// 回归：命令首帧进度不受 2KB 阈值限制——短命令（`echo hi`）也必须发帧，
    /// 否则短命令的卡片只会在调用结束后才出现。
    #[tokio::test]
    async fn short_command_emits_progress_frame_despite_gate() {
        let (core, rt, log) = recording_core("tshort");
        let call = NormalizedCall {
            id: "c1".into(),
            name: "command".into(),
            args: serde_json::json!({ "command": "echo hi" }),
            index: 0,
        };
        let (out, _, _) = run_tool(
            &core,
            &rt,
            &call,
            "b1",
            0,
            tokio_util::sync::CancellationToken::new(),
            false,
        )
        .await;
        assert!(out.ok, "{out:?}");

        // 进度帧由独立任务异步发出（读通道 + 节流）：轮询等它到位
        let mut seen = false;
        for _ in 0..100 {
            if log
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "frame:tool_progress:command:chunk")
            {
                seen = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(seen, "短命令必须发首帧进度：{:?}", log.lock().unwrap());
    }

    #[test]
    fn arg_path_extraction() {
        let ws = tempfile::tempdir().unwrap();
        let args = serde_json::json!({"files":[{"path":"a.txt"},{"path":"b.txt"}]});
        // macOS 的 /tmp 是指向 /private/tmp 的符号链接：比较规范化形态而非字面路径
        let expect = |name: &str| {
            crate::tools::pathutil::canonical_best_effort(&ws.path().join(name))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(
            canonical_arg_paths(ws.path(), &args),
            vec![expect("a.txt"), expect("b.txt")]
        );
        let args = serde_json::json!({"path":"x.txt"});
        assert_eq!(canonical_arg_paths(ws.path(), &args), vec![expect("x.txt")]);
        // M1 回归：./ 前缀归一后必须与裸路径判同
        let a = canonical_arg_paths(ws.path(), &serde_json::json!({"path":"f.txt"}));
        let b = canonical_arg_paths(ws.path(), &serde_json::json!({"path":"./f.txt"}));
        assert_eq!(a, b, "./f.txt 与 f.txt 应判为同一路径");
    }

    #[tokio::test]
    async fn confirm_each_blocks_file_write_without_approval() {
        // [docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)：ConfirmEach 档下 FileWrite 工具执行前必经审批；预取消 token = 立即拒绝
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("ce", roots.workspace.clone(), None, vec![], None, vec![]);
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::ConfirmEach,
            model_id: None,
            reasoning_effort: None,
        });
        std::fs::write(ws.path().join("f.txt"), b"hello").unwrap();
        let calls = vec![crate::core::agent::NormalizedCall {
            id: "t1".into(),
            name: "edit".into(),
            args: serde_json::json!({"files":[{"path":"f.txt","changes":[{"lineRange":"1-1","newText":"X"}]}]}),
            index: 0,
        }];
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let out = execute_batch(&core, &rt, calls, &[], false, false, cancel, "run1").await;
        let err = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .next()
            .expect("应有错误结果");
        assert!(err.contains("E_CANCELLED"), "预取消应在闸层立即拦截：{err}");
        // 文件未被修改
        assert_eq!(std::fs::read(ws.path().join("f.txt")).unwrap(), b"hello");
    }

    /// [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：ConfirmEach 下命中「始终允许本项目」白名单的命令免审批直接执行；
    /// 白名单之外的相似命令仍需审批（预取消 token = 立即拒绝）。
    #[tokio::test]
    async fn command_allowlist_skips_approval() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("al", roots.workspace.clone(), None, vec![], None, vec![]);
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::ConfirmEach,
            model_id: None,
            reasoning_effort: None,
        });
        // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：白名单 key = cwd + \u{1} + 完整命令文本（cwd 是会话默认命令目录且随项目走）
        let allow_key = format!("{}\u{1}echo hello > out.txt", roots.workspace.display());
        core.cfg
            .write()
            .unwrap()
            .approval
            .command_allowlist
            .push(allow_key);

        let make_calls = |name: &str, cmd: &str| {
            vec![crate::core::agent::NormalizedCall {
                id: "t1".into(),
                name: name.into(),
                args: serde_json::json!({ "command": cmd }),
                index: 0,
            }]
        };

        // 白名单命中：无需审批应答即成功执行（out.txt 真实落盘）
        let out = execute_batch(
            &core,
            &rt,
            make_calls("command", "echo hello > out.txt"),
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(
            !out.results.iter().any(|c| matches!(
                c,
                crate::core::types::Content::ToolResult { is_error: true, .. }
            )),
            "白名单命中不应报错"
        );
        assert!(ws.path().join("out.txt").exists(), "命令应已真实执行");

        // 白名单之外：Confirm 需要应答；预取消 = 拒绝
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let out2 = execute_batch(
            &core,
            &rt,
            make_calls("command", "echo world > out2.txt"),
            &[],
            false,
            false,
            cancel,
            "run2",
        )
        .await;
        let err = out2
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .next()
            .expect("应有错误结果");
        assert!(
            err.contains("E_CANCELLED"),
            "预取消应在闸层立即拦截（白名单外命令）: {err}"
        );
        assert!(!ws.path().join("out2.txt").exists(), "未放行命令不应执行");
    }

    /// G3 用例共享夹具：AutoEdit 档（批准后切换的等价态）+ 已批准基线 + 计划外新 todo + scope_expanded 标记。
    fn g3_scope_fixture() -> (
        tempfile::TempDir,
        Arc<crate::core::agent::AgentCore>,
        Arc<crate::core::agent::SessionRuntime>,
    ) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("g3", roots.workspace.clone(), None, vec![], None, vec![]);
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::AutoEdit,
            model_id: None,
            reasoning_effort: None,
        });
        // 冻结已批准基线（等价 ask.rs 批准路径）；当前 todos 多出一条计划外步骤
        *rt.approved_plan.lock().unwrap() = Some(vec!["已批准的步骤".into()]);
        rt.todos.lock().unwrap().push(crate::tools::plan::Todo {
            title: "计划外新增步骤".into(),
            status: crate::tools::plan::TodoStatus::Pending,
        });
        rt.scope_expanded
            .store(true, std::sync::atomic::Ordering::SeqCst);
        (ws, core, rt)
    }

    #[tokio::test]
    async fn g3_command_redirect_requires_scope_approval() {
        // G3 回归：批准后命令的范围内写重定向必须触发计划外步骤确认（预取消 token = 拒绝）
        let (ws, core, rt) = g3_scope_fixture();
        let calls = vec![crate::core::agent::NormalizedCall {
            id: "t1".into(),
            name: "command".into(),
            args: serde_json::json!({ "command": "echo x > g3_probe.txt" }),
            index: 0,
        }];
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let out = execute_batch(&core, &rt, calls, &[], false, false, cancel, "run1").await;
        let err = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .next()
            .expect("应有 G3 拒绝错误");
        assert!(
            err.contains("E_CANCELLED"),
            "预取消应在闸层立即拦截（G3 范围门）: {err}"
        );
        assert!(!ws.path().join("g3_probe.txt").exists(), "被拒命令不应执行");
    }

    #[tokio::test]
    async fn g3_pure_read_command_skips_scope_gate() {
        // G3 回归：同一范围状态下纯只读命令不触发范围确认（防误伤）
        let (_ws, core, rt) = g3_scope_fixture();
        let calls = vec![crate::core::agent::NormalizedCall {
            id: "t1".into(),
            name: "command".into(),
            args: serde_json::json!({ "command": "echo hello" }),
            index: 0,
        }];
        let out = execute_batch(
            &core,
            &rt,
            calls,
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        let result = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content, is_error, ..
                } => Some((content.clone(), *is_error)),
                _ => None,
            })
            .next()
            .expect("应有工具结果");
        assert!(!result.1, "纯只读命令不应被 G3 拦截：{}", result.0);
        assert!(result.0.contains("hello"), "{}", result.0);
    }

    /// Plan 档硬门回归：模型坚持调用被排除的 edit 也必须拒绝不执行；
    /// 错误为终结性提示（含 E_TOOL_BLOCKED 且不诱导重读重试）。
    #[tokio::test]
    async fn plan_mode_hard_gate_blocks_excluded_edit() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("pg", roots.workspace.clone(), None, vec![], None, vec![]);
        std::fs::write(ws.path().join("f.txt"), b"hello").unwrap();
        let calls = vec![crate::core::agent::NormalizedCall {
            id: "t1".into(),
            name: "edit".into(),
            args: serde_json::json!({"files":[{"path":"f.txt","changes":[{"lineRange":"1-1","newText":"X"}]}]}),
            index: 0,
        }];
        // 排除集 = plan 档参数（等价 main_drive_params），与模型工具列表同源
        let out = execute_batch(
            &core,
            &rt,
            calls,
            &["edit".to_string()],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        let err = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .next()
            .expect("被排除工具应被硬门拒绝");
        assert!(err.contains("E_TOOL_BLOCKED"), "{err}");
        assert!(err.contains("重新 read"), "错误需为终结性提示：{err}");
        assert_eq!(
            std::fs::read(ws.path().join("f.txt")).unwrap(),
            b"hello",
            "被排除工具不应真实执行"
        );
    }

    // ===== plan 纪律门（批次硬门 E_PLAN_REQUIRED / E_PLAN_STALE + 软提醒）=====

    #[test]
    fn plan_gate_verdict_three_states() {
        let todo = |status: crate::tools::plan::TodoStatus| crate::tools::plan::Todo {
            title: "t".into(),
            status,
        };
        // 空列表 + 写工具 → 必须先建计划；空列表 + 非写工具 → 放行
        assert_eq!(plan_gate_verdict(&[], true), Some("E_PLAN_REQUIRED"));
        assert_eq!(plan_gate_verdict(&[], false), None);
        // 全部 completed + 写工具 → 计划已收尾；非写工具 → 放行
        let all_done = vec![
            todo(crate::tools::plan::TodoStatus::Completed),
            todo(crate::tools::plan::TodoStatus::Completed),
        ];
        assert_eq!(plan_gate_verdict(&all_done, true), Some("E_PLAN_STALE"));
        assert_eq!(plan_gate_verdict(&all_done, false), None);
        // 部分 completed（还有 pending/in_progress）+ 写工具 → 放行
        let partial = vec![
            todo(crate::tools::plan::TodoStatus::Completed),
            todo(crate::tools::plan::TodoStatus::Pending),
        ];
        assert_eq!(plan_gate_verdict(&partial, true), None);
        let running = vec![todo(crate::tools::plan::TodoStatus::InProgress)];
        assert_eq!(plan_gate_verdict(&running, true), None);
    }

    /// plan 纪律门集成用例共享夹具：AutoEdit 档（免逐写审批）+ 预置目标文件 + 可选预置 todos。
    fn plan_gate_fixture(
        id: &str,
        todos: Vec<crate::tools::plan::Todo>,
    ) -> (
        tempfile::TempDir,
        Arc<crate::core::agent::AgentCore>,
        Arc<crate::core::agent::SessionRuntime>,
    ) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session(id, roots.workspace.clone(), None, vec![], None, vec![]);
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::AutoEdit,
            model_id: None,
            reasoning_effort: None,
        });
        *rt.todos.lock().unwrap() = todos;
        std::fs::write(ws.path().join("f.txt"), b"hello").unwrap();
        (ws, core, rt)
    }

    fn plan_todo(title: &str, status: crate::tools::plan::TodoStatus) -> crate::tools::plan::Todo {
        crate::tools::plan::Todo {
            title: title.into(),
            status,
        }
    }

    fn edit_call() -> crate::core::agent::NormalizedCall {
        crate::core::agent::NormalizedCall {
            id: "t1".into(),
            name: "edit".into(),
            args: serde_json::json!({"files":[{"path":"f.txt","changes":[{"lineRange":"1-1","newText":"X"}]}]}),
            index: 0,
        }
    }

    fn plan_call() -> crate::core::agent::NormalizedCall {
        crate::core::agent::NormalizedCall {
            id: "t2".into(),
            name: "plan".into(),
            args: serde_json::json!({}),
            index: 1,
        }
    }

    fn first_error_text(out: &BatchOutcome) -> Option<String> {
        out.results.iter().find_map(|c| match c {
            crate::core::types::Content::ToolResult {
                content,
                is_error: true,
                ..
            } => Some(content.clone()),
            _ => None,
        })
    }

    fn model_text_chunks(out: &BatchOutcome) -> Vec<String> {
        out.results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// 提取全部成功 ToolResult 的模型侧 content 文本（软提醒应追加在其尾部）。
    fn ok_toolresult_texts(out: &BatchOutcome) -> Vec<String> {
        out.results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: false,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    /// a：主会话 + todos 空 + edit → E_PLAN_REQUIRED，目标文件未被写入。
    #[tokio::test]
    async fn plan_gate_requires_plan_before_first_write() {
        let (ws, core, rt) = plan_gate_fixture("pgate-a", vec![]);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        let err = first_error_text(&out).expect("无计划写文件应被硬门拒绝");
        assert!(err.contains("E_PLAN_REQUIRED"), "{err}");
        assert!(err.contains("plan 工具建立计划"), "{err}");
        assert_eq!(
            std::fs::read(ws.path().join("f.txt")).unwrap(),
            b"hello",
            "被拒写不应真实执行"
        );
    }

    /// b：主会话 + todos 全 completed + edit → E_PLAN_STALE，目标文件未被写入。
    #[tokio::test]
    async fn plan_gate_blocks_write_when_all_completed() {
        let todos = vec![
            plan_todo("已完成步骤一", crate::tools::plan::TodoStatus::Completed),
            plan_todo("已完成步骤二", crate::tools::plan::TodoStatus::Completed),
        ];
        let (ws, core, rt) = plan_gate_fixture("pgate-b", todos);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        let err = first_error_text(&out).expect("计划全部完成后继续写应被拒绝");
        assert!(err.contains("E_PLAN_STALE"), "{err}");
        assert!(err.contains("计划外工作"), "{err}");
        assert_eq!(
            std::fs::read(ws.path().join("f.txt")).unwrap(),
            b"hello",
            "被拒写不应真实执行"
        );
    }

    /// c：主会话 + todos 含 pending/in_progress + edit → 放行。
    #[tokio::test]
    async fn plan_gate_allows_write_with_active_todos() {
        let todos = vec![
            plan_todo("已完成步骤", crate::tools::plan::TodoStatus::Completed),
            plan_todo("进行中步骤", crate::tools::plan::TodoStatus::InProgress),
            plan_todo("待办步骤", crate::tools::plan::TodoStatus::Pending),
        ];
        let (_ws, core, rt) = plan_gate_fixture("pgate-c", todos);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(
            first_error_text(&out).is_none(),
            "有进行中/待办计划时写不应被拦"
        );
    }

    /// d：非主会话（子代理）+ todos 空 + edit → 豁免放行。
    #[tokio::test]
    async fn plan_gate_skipped_for_non_main_session() {
        let (_ws, core, rt) = plan_gate_fixture("pgate-d", vec![]);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(first_error_text(&out).is_none(), "子代理应豁免 plan 纪律门");
    }

    /// e：同批 [plan, edit] + todos 空 → 同批乐观豁免，edit 放行且真实执行。
    #[tokio::test]
    async fn plan_gate_optimistic_for_same_batch_plan() {
        let (ws, core, rt) = plan_gate_fixture("pgate-e", vec![]);
        let out = execute_batch(
            &core,
            &rt,
            vec![plan_call(), edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(first_error_text(&out).is_none(), "同批含 plan 时写应放行");
        assert!(
            std::fs::read_to_string(ws.path().join("f.txt"))
                .unwrap()
                .contains('X'),
            "edit 应已真实执行"
        );
    }

    /// f：软提醒——写成功且计划无 InProgress 条目时模型侧一次性提醒；
    /// 重复写不重复提醒；todos 含 in_progress 时不提醒。
    #[tokio::test]
    async fn plan_soft_hint_once_and_skip_with_in_progress() {
        // f1：todos = [completed, pending] → 写成功且模型侧恰有一条提醒
        let todos = vec![
            plan_todo("已完成步骤", crate::tools::plan::TodoStatus::Completed),
            plan_todo("待办步骤", crate::tools::plan::TodoStatus::Pending),
        ];
        let (_ws, core, rt) = plan_gate_fixture("pgate-f1", todos);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(first_error_text(&out).is_none());
        let hints = ok_toolresult_texts(&out)
            .iter()
            .filter(|t| t.contains("计划提醒："))
            .count();
        assert_eq!(
            hints,
            1,
            "模型侧应恰有一条计划提醒：{:?}",
            ok_toolresult_texts(&out)
        );
        assert!(
            model_text_chunks(&out)
                .iter()
                .all(|t| !t.contains("计划提醒：")),
            "提醒不应再以独立 Text 块出现（wire 层会丢弃该通道）：{:?}",
            model_text_chunks(&out)
        );
        assert!(
            rt.plan_hint_emitted
                .load(std::sync::atomic::Ordering::SeqCst)
        );

        // f2：重复写 → 不再重复提醒（CAS 已消费）
        let out2 = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run2",
        )
        .await;
        assert!(first_error_text(&out2).is_none());
        assert!(
            !ok_toolresult_texts(&out2)
                .iter()
                .any(|t| t.contains("计划提醒：")),
            "第二次写不应重复提醒：{:?}",
            ok_toolresult_texts(&out2)
        );

        // f3：todos 含 in_progress → 写放行且无提醒（CAS 也未被消费）
        let todos = vec![
            plan_todo("进行中步骤", crate::tools::plan::TodoStatus::InProgress),
            plan_todo("待办步骤", crate::tools::plan::TodoStatus::Pending),
        ];
        let (_ws, core, rt) = plan_gate_fixture("pgate-f3", todos);
        let out3 = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(first_error_text(&out3).is_none());
        assert!(
            !ok_toolresult_texts(&out3)
                .iter()
                .any(|t| t.contains("计划提醒：")),
            "有进行中条目时不应提醒"
        );
        assert!(
            !rt.plan_hint_emitted
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    /// 🟡4a：同批两个不同文件写 + todos 空 → 并发批次共享同一入口快照，
    /// 两个写调用都被 E_PLAN_REQUIRED 拒绝（行为与逐 call 判定一致）。
    #[tokio::test]
    async fn plan_gate_shared_snapshot_rejects_all_writes_in_batch() {
        let (ws, core, rt) = plan_gate_fixture("pgate-g", vec![]);
        let mut edit_a = edit_call();
        edit_a.args = serde_json::json!({"files":[{"path":"a.txt","changes":[{"lineRange":"1-1","newText":"A"}]}]});
        let mut edit_b = edit_call();
        edit_b.id = "t3".into();
        edit_b.index = 1;
        edit_b.args = serde_json::json!({"files":[{"path":"b.txt","changes":[{"lineRange":"1-1","newText":"B"}]}]});
        std::fs::write(ws.path().join("a.txt"), b"aaa").unwrap();
        std::fs::write(ws.path().join("b.txt"), b"bbb").unwrap();
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_a, edit_b],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        let errs = out
            .results
            .iter()
            .filter_map(|c| match c {
                crate::core::types::Content::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(errs.len(), 2, "两个写都应被拒：{errs:?}");
        assert!(
            errs.iter().all(|e| e.contains("E_PLAN_REQUIRED")),
            "两个错误都应是 E_PLAN_REQUIRED：{errs:?}"
        );
        assert_eq!(
            std::fs::read(ws.path().join("a.txt")).unwrap(),
            b"aaa",
            "写 a 不应真实执行"
        );
        assert_eq!(
            std::fs::read(ws.path().join("b.txt")).unwrap(),
            b"bbb",
            "写 b 不应真实执行"
        );
    }

    /// 🟡4b：同批 [plan（入参非法，调用本身失败）, edit] + todos 空 → edit 仍乐观放行。
    /// 豁免判定只看「本批是否含 plan 调用」，不看 plan 的执行结果——钉死该意图。
    #[tokio::test]
    async fn plan_gate_optimistic_exempt_ignores_plan_failure() {
        let (ws, core, rt) = plan_gate_fixture("pgate-h", vec![]);
        let mut bad_plan = plan_call();
        // 非法状态值：plan 调用本身失败（E_ARGS），todos 仍为空
        bad_plan.args = serde_json::json!({"todos":[{"title":"x","status":"bogus"}]});
        let out = execute_batch(
            &core,
            &rt,
            vec![bad_plan, edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        // plan 失败但 edit 放行且真实执行
        assert!(
            std::fs::read_to_string(ws.path().join("f.txt"))
                .unwrap()
                .contains('X'),
            "plan 调用失败不影响同批写的乐观豁免"
        );
        // 只看 edit（t1）的结果；plan（t2）自身因入参非法失败属预期
        let edit_err = out.results.iter().find_map(|c| match c {
            crate::core::types::Content::ToolResult {
                tool_use_id,
                content,
                is_error: true,
            } if tool_use_id == "t1" => Some(content.clone()),
            _ => None,
        });
        assert!(
            edit_err.is_none(),
            "edit 不应被拦（豁免不看 plan 调用结果）：{edit_err:?}"
        );
    }

    // ---------- 目标档执行期：写入账本门 + G3 关闭 ----------

    /// 目标档夹具：工作区 f.txt（hello）+ 指定档位/阶段；账本 = 工作区，程序 = cargo。
    /// 第二个 TempDir 是数据目录：调用方必须绑住它（边车落盘断言需要目录真实存在）。
    fn goal_fixture(
        session: &str,
        mode: crate::core::prefs::ApprovalMode,
        status: crate::core::agent::goal::GoalStatus,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<AgentCore>,
        Arc<SessionRuntime>,
    ) {
        use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState};
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            session,
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: mode,
            model_id: None,
            reasoning_effort: None,
        });
        rt.set_goal(Some(GoalState {
            text: "把 X 改成 Y".into(),
            criteria: vec![GoalCriterion {
                title: "改完 X".into(),
                done: false,
            }],
            ledger: GoalLedger {
                paths: vec![roots.workspace.to_string_lossy().into_owned()],
                programs: vec!["cargo".into()],
            },
            status,
            decisions: Vec::new(),
            pending: Vec::new(),
            blocked: Vec::new(),
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
        }));
        // 目标档执行期由账本接管范围控制，不叠 plan 纪律门（`execute_batch` 的
        // `goal_execute_phase` 豁免）：故这里**刻意不预置 todos**——本组用例同时钉死
        // 「执行期第一次 edit 不会被 E_PLAN_REQUIRED 拦下」。
        std::fs::write(ws.path().join("f.txt"), b"hello").unwrap();
        (ws, dd, core, rt)
    }

    /// ①（接线）目标档执行期：账本内写入放行并真实执行；账本外拒绝且不落盘。
    #[tokio::test]
    async fn goal_write_gate_allows_inside_and_rejects_outside() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let (ws, _dd, core, rt) = goal_fixture("gwg1", ApprovalMode::Goal, GoalStatus::Executing);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(
            first_error_text(&out).is_none(),
            "{:?}",
            first_error_text(&out)
        );
        assert!(
            std::fs::read_to_string(ws.path().join("f.txt"))
                .unwrap()
                .contains('X'),
            "账本内写入应真实执行"
        );

        let outside = std::env::temp_dir().join("codewave-goal-outside-probe.txt");
        let mut call = edit_call();
        call.args = serde_json::json!({"files":[{"path": outside.to_string_lossy(), "changes":[{"lineRange":"1-1","newText":"X"}]}]});
        let out = execute_batch(
            &core,
            &rt,
            vec![call],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run2",
        )
        .await;
        let err = first_error_text(&out).expect("账本外写必须被拒");
        assert!(err.contains("E_GOAL_OUTSIDE_LEDGER"), "{err}");
        assert!(!outside.exists(), "被拒写不得落盘");
        assert_eq!(rt.goal_snapshot().unwrap().ledger_denials, 1);
    }

    /// ②（🟡-1 接线）子代理 runtime 的写入同样过账本：作用域取根会话（父会话）的账本——
    /// 修复前子 runtime 自身无目标 → 闸门直接放行，「账本 = 执行期唯一约束」在子代理下失效。
    #[tokio::test]
    async fn goal_write_gate_covers_subagent_runtime() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let (ws, dd, core, main) =
            goal_fixture("gwg-sub", ApprovalMode::Goal, GoalStatus::Executing);
        // 子代理 runtime：不持有目标状态、也非主会话（`new_sub` 语义）
        let sub = SessionRuntime::new_sub(&main, "sub_gwg".to_string());
        assert!(!sub.is_main_session);
        assert!(sub.goal_snapshot().is_none(), "前提：子代理不持有目标状态");
        // 账本内写入：放行并真实执行
        let out = execute_batch(
            &core,
            &sub,
            vec![edit_call()],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(
            first_error_text(&out).is_none(),
            "{:?}",
            first_error_text(&out)
        );
        assert!(
            std::fs::read_to_string(ws.path().join("f.txt"))
                .unwrap()
                .contains('X'),
            "账本内写入应真实执行"
        );
        // 账本外写入：拒（且不落盘）
        let outside = std::env::temp_dir().join("codewave-goal-sub-outside-probe.txt");
        let mut call = edit_call();
        call.args = serde_json::json!({"files":[{"path": outside.to_string_lossy(), "changes":[{"lineRange":"1-1","newText":"X"}]}]});
        let out = execute_batch(
            &core,
            &sub,
            vec![call],
            &[],
            false,
            false,
            tokio_util::sync::CancellationToken::new(),
            "run2",
        )
        .await;
        let err = first_error_text(&out).expect("子代理账本外写必须被拒");
        assert!(err.contains("E_GOAL_OUTSIDE_LEDGER"), "{err}");
        assert!(!outside.exists(), "被拒写不得落盘");
        // 越界记在根会话（父会话）上（自停收敛口径一致），子代理不留目标边车（脏文件）
        let g = main.goal_snapshot().unwrap();
        assert_eq!(g.ledger_denials, 1);
        assert!(
            g.blocked.iter().any(|b| b.contains("账本外操作被拒")),
            "{:?}",
            g.blocked
        );
        let store = crate::core::sessions::SessionStore::new(dd.path().to_path_buf());
        assert_eq!(
            store.load_goal(&main.id).unwrap().ledger_denials,
            1,
            "越界应落根会话边车"
        );
        assert!(
            !store.goal_path(&sub.id).exists(),
            "子代理不得留下 sessions/sub_*.goal.json"
        );
    }

    /// ③（接线，回归红线）非目标档 / 目标档澄清期完全不受账本门影响。
    #[tokio::test]
    async fn goal_write_gate_inert_outside_goal_execute() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        for (mode, status) in [
            (ApprovalMode::AutoEdit, GoalStatus::Executing),
            (ApprovalMode::Plan, GoalStatus::Executing),
            (ApprovalMode::Goal, GoalStatus::Clarify),
        ] {
            let (_ws, _dd, core, rt) = goal_fixture("gwg2", mode, status);
            let outside = std::env::temp_dir().join(format!("codewave-goal-out-{mode:?}.txt"));
            let mut call = edit_call();
            call.args = serde_json::json!({"files":[{"path": outside.to_string_lossy(), "changes":[{"lineRange":"1-1","newText":"X"}]}]});
            let out = execute_batch(
                &core,
                &rt,
                vec![call],
                &[],
                false,
                true,
                tokio_util::sync::CancellationToken::new(),
                "run1",
            )
            .await;
            // 工具自身可能因目标文件不存在而失败，但绝不能是账本拒绝
            if let Some(err) = first_error_text(&out) {
                assert!(
                    !err.contains("E_GOAL_OUTSIDE_LEDGER"),
                    "{mode:?}/{status:?} 被账本门误伤：{err}"
                );
            }
            assert_eq!(
                rt.goal_snapshot().unwrap().ledger_denials,
                0,
                "{mode:?}/{status:?} 不得记越界"
            );
        }
    }

    /// ⑧ 目标档执行期不叠 plan 纪律门（范围控制由账本承担）：todos 为空（第一次写）与
    /// todos 全部完成（计划已收尾）两种情况写入都放行且真实执行——两个错误码一起豁免。
    #[tokio::test]
    async fn goal_execute_phase_exempts_plan_discipline_gate() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        // a：todos 为空 → 执行期第一次 edit 不被 E_PLAN_REQUIRED 拦下
        let (ws, _dd, core, rt) =
            goal_fixture("gplan-a", ApprovalMode::Goal, GoalStatus::Executing);
        assert!(rt.todos.lock().unwrap().is_empty(), "夹具刻意不预置计划");
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(
            first_error_text(&out).is_none(),
            "执行期首次写入不得被 plan 纪律门拦下：{:?}",
            first_error_text(&out)
        );
        assert!(
            std::fs::read_to_string(ws.path().join("f.txt"))
                .unwrap()
                .contains('X'),
            "账本内写入应真实执行"
        );
        // b：todos 全部完成 → 同样豁免 E_PLAN_STALE
        let (ws, _dd, core, rt) =
            goal_fixture("gplan-b", ApprovalMode::Goal, GoalStatus::Executing);
        *rt.todos.lock().unwrap() = vec![plan_todo(
            "已完成步骤",
            crate::tools::plan::TodoStatus::Completed,
        )];
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        assert!(
            first_error_text(&out).is_none(),
            "计划全完成时执行期继续写入不得被 E_PLAN_STALE 拦下：{:?}",
            first_error_text(&out)
        );
        assert!(
            std::fs::read_to_string(ws.path().join("f.txt"))
                .unwrap()
                .contains('X')
        );
    }

    /// ⑨（反向，回归红线）非目标档不受该豁免影响：档位是 AutoEdit 时 plan 纪律门逐字不变
    ///（目标即使已登记且「执行中」也不算——判定入口 `goal_execute_phase` = 目标档 ∧ 执行期）。
    #[tokio::test]
    async fn plan_gate_still_applies_when_not_in_goal_mode() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let (ws, _dd, core, rt) =
            goal_fixture("gplan-c", ApprovalMode::AutoEdit, GoalStatus::Executing);
        let out = execute_batch(
            &core,
            &rt,
            vec![edit_call()],
            &[],
            false,
            true,
            tokio_util::sync::CancellationToken::new(),
            "run1",
        )
        .await;
        let err = first_error_text(&out).expect("非目标档 + 无计划写文件必须仍被拒");
        assert!(err.contains("E_PLAN_REQUIRED"), "{err}");
        assert_eq!(
            std::fs::read(ws.path().join("f.txt")).unwrap(),
            b"hello",
            "被拒写不应真实执行"
        );
    }

    /// ⑦ 目标档下 G3（计划外步骤确认）不触发：即使人为造出 G3 前置，也不弹审批、不挂起。
    #[tokio::test]
    async fn goal_mode_disables_g3_scope_gate() {
        use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState, GoalStatus};
        use crate::core::prefs::{ApprovalMode, SessionPrefs};
        let (core, rt, log) = recording_core("gg3");
        rt.set_prefs(SessionPrefs {
            approval_mode: ApprovalMode::Goal,
            model_id: None,
            reasoning_effort: None,
        });
        rt.set_goal(Some(GoalState {
            text: "把 X 改成 Y".into(),
            criteria: vec![GoalCriterion {
                title: "改完 X".into(),
                done: false,
            }],
            ledger: GoalLedger {
                paths: vec![rt.workspace.to_string_lossy().into_owned()],
                programs: vec![],
            },
            status: GoalStatus::Executing,
            decisions: Vec::new(),
            pending: Vec::new(),
            blocked: Vec::new(),
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
        }));
        rt.todos
            .lock()
            .unwrap()
            .push(plan_todo("步骤", crate::tools::plan::TodoStatus::Pending));
        // 人为制造 G3 前置（正常目标档批准路径不冻结基线，这里是双保险验证）
        *rt.approved_plan.lock().unwrap() = Some(vec!["已批准的步骤".into()]);
        rt.scope_expanded
            .store(true, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(rt.workspace.join("f.txt"), b"hello").unwrap();
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            execute_batch(
                &core,
                &rt,
                vec![edit_call()],
                &[],
                false,
                true,
                tokio_util::sync::CancellationToken::new(),
                "run1",
            ),
        )
        .await
        .expect("G3 在目标档必须关闭：否则会挂在审批等待上");
        assert!(
            first_error_text(&out).is_none(),
            "{:?}",
            first_error_text(&out)
        );
        assert!(
            std::fs::read_to_string(rt.workspace.join("f.txt"))
                .unwrap()
                .contains('X')
        );
        assert!(
            !log.lock().unwrap().iter().any(|e| e.contains("ask:opened")),
            "目标档不得产生审批请求：{:?}",
            log.lock().unwrap()
        );
    }
}
