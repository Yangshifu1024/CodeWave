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
    /// 按调用顺序排列的模型侧 ToolResult 内容。
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
    calls: Vec<NormalizedCall>,
    effective_excludes: &[String],
    effective_exclude_mcp: bool,
    main_session: bool,
    cancel: CancellationToken,
    run_id: &str,
) -> BatchOutcome {
    let batch_id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let sink = core.sink.clone();

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
            results.push(model_content(core, c, &out, None));
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
        // 不豁免将无法写文件）；本批含 plan 调用（同批乐观豁免，见 batch_has_plan）。
        // 分工边界：command 工具的 shell 重定向写不经本门，归 fence/G3 范围门兜底（见 run_tool）。
        let is_write = core.tools.get(&call.name).map(|t| t.kind()) == Some(ToolKind::FileWrite);
        if main_session && is_write && !batch_has_plan {
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
        // extra 当前只有 plan 软提醒（0/1 条 Text 块），取首个 Text 的文本拼接
        let hint = extra.iter().find_map(|c| match c {
            Content::Text { text } => Some(text.clone()),
            _ => None,
        });
        results.push(model_content(core, call, &out, hint));
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
        let started = Instant::now();
        let args = call.args.clone();
        let data_dir = rt.data_dir.clone();
        let workspace = rt.workspace.clone();
        let project_dir = rt.project_dir.clone();
        let extra_roots = rt.extra_roots.lock().unwrap().clone();
        let mcp = core.mcp.clone();
        // streamable-http 重连路径复用代理感知 client（读锁 clone，std 锁不跨 await）
        let http = core.client.read().unwrap().clone();
        let fname = call.name.clone();
        let out = match tokio::spawn(async move {
            mcp.call(
                &fname,
                args,
                &data_dir,
                &workspace,
                project_dir.as_deref(),
                &extra_roots,
                http,
            )
            .await
        })
        .await
        {
            Ok(Ok(text)) => {
                let v: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or(serde_json::json!({ "text": text }));
                ToolOutcome::ok(v)
            }
            Ok(Err(e)) => ToolOutcome::err("E_MCP", e),
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
    // detail 优先用工具的语义化预览（edit/create 的变更 diff，[docs/tools-optimization-and-gap-fill-plan](../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 3）；
    // 无预览能力的工具（如 delete）回退为入参 JSON 展示。
    if ctx.approval_mode() == crate::core::prefs::ApprovalMode::ConfirmEach
        && tool.kind() == ToolKind::FileWrite
    {
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
    // G3 范围门（[docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md) §7）：批准后新增的计划外步骤，首次写入前弹范围确认。
    // 覆盖两条写通道（R2 评审修复）：FileWrite 工具；携带写目标的 command 工具。
    // 后者用专门的「写目标探针」策略（confirm_inside_writes=true），与执行档判定解耦：
    // 批准后会话已切 AutoEdit，其执行档 fence 会把范围内写重定向判为 Allow；若无探针，
    // shell 重定向可完全绕过确认。纯只读命令探针得 Allow，不打扰。
    // 批准 = 本会话放行（后续新增静默纳入）；拒绝 = 保留标记，下次写入再问。基线缺失（异常路径）不阻塞。
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
    if g3_applies
        && ctx
            .rt
            .scope_expanded
            .load(std::sync::atomic::Ordering::SeqCst)
        && !ctx
            .rt
            .scope_allowed
            .load(std::sync::atomic::Ordering::SeqCst)
        && ctx.rt.approved_plan.lock().unwrap().is_some()
    {
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
) -> Content {
    let kind = core
        .tools
        .get(&call.name)
        .map(|t| t.kind())
        .unwrap_or(ToolKind::Meta);
    let mut text = crate::tools::compact::compact_for_model(kind, &call.name, out);
    if let Some(hint) = model_hint_non_empty {
        text.push_str(&hint);
    }
    Content::ToolResult {
        tool_use_id: call.id.clone(),
        content: text,
        is_error: !out.ok,
    }
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
    // H3 修复：截断必须保持 JSON 可解析（此前硬切 2000 字符会悄悄打断前端 diff）
    let args_preview: String = {
        let s = serde_json::to_string(&call.args).unwrap_or_default();
        if s.len() <= 200_000 {
            s
        } else {
            serde_json::json!({ "_args_truncated": true, "hint": "参数过大，前端不展示 diff" })
                .to_string()
        }
    };
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
}
