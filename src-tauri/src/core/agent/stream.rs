use crate::core::types::{Content, Message, Role, SessionId};
use crate::provider::dto::{AsmBlock, Assembled, AssembledToolCall, StreamRequest};
use crate::tools::compact::compact_for_model;
use crate::util::throttle::ThrottledStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use super::drive::{DriveParams, NormalizedCall};
use super::runtime::{AgentCore, EventSink, Frame, SessionRuntime, STREAM_THROTTLE_MS};

/// 会话日志 verbose 全文（请求/响应）的单会话上限（字符数；防止超大响应撑爆日志）
pub(super) const VERBOSE_BODY_CAP: usize = 64 * 1024;
/// 会话日志错误摘要上限（字符数）
pub(super) const ERROR_CAP: usize = 500;

/// 出网消息副本（缺陷修复）：当前历史 clone + 新用户轮次首个请求的计划瞬态快照，
/// **末尾再跑一次 repair** 作为出网兜底。
///
/// 为什么兜底要放在出网侧：历史里的脏结构（空 assistant 消息、悬空 tool_use、孤儿
/// tool_result）一旦上 wire 就会被 provider 判为非法并 400；而 400 后的重试若发的还是
/// 同一份快照，就会复现同一个错误（会话 d9941c4b 实测：相隔 2.45s 的两条逐字节相同的
/// 400）。这里统一保证「无论历史为何，出网副本必满足 wire 不变量」。不改写
/// `rt.history`（但会置位每 run 一次的瞬态注入标志）。
pub(super) fn messages_for_request(rt: &Arc<SessionRuntime>) -> Vec<Message> {
    let mut messages = rt.history.lock().unwrap().clone();
    // 新用户轮次的首个请求：附加当前计划瞬态快照（不落盘；Anthropic cache 断点
    // 落在其之前最后一条非瞬态消息上，保前缀缓存）
    if !rt.injected_plan_snapshot_for_run.load(Ordering::SeqCst) {
        let todos = rt.todos.lock().unwrap().clone();
        if !todos.is_empty() {
            messages.push(Message::user_text(format!(
                "<current-plan-transient>\n{}\n</current-plan-transient>",
                crate::tools::plan::render_todos(&todos)
            )));
            rt.injected_plan_snapshot_for_run
                .store(true, Ordering::SeqCst);
        }
    }
    repair_before_send(messages)
}

/// 出网副本的修复步骤（纯函数，便于单测）：复用会话修复管线的 `repair`——
/// 补悬空 tool_use 的 [interrupted] 结果、清孤儿 tool_result、丢空 assistant 消息。
pub(super) fn repair_before_send(mut messages: Vec<Message>) -> Vec<Message> {
    crate::core::sessions::repair::repair(&mut messages);
    messages
}

/// 重试前用当前历史重建请求体消息（缺陷修复）：BadRequest 分支的 sanitize/repair 只改写
/// `rt.history`，而请求体是构建时的一次性快照——不重建则重试必然复现同一个错误。
///
/// 两处显式取舍：
/// - **保留**本 run 首轮已注入的计划瞬态快照：`messages_for_request` 只在注入标志未置位时
///   注入，重试时该标志已置位，故这里把原 body 尾部的瞬态消息补回，使重试 body 是首次的
///   延拓（否则模型在重试那一轮会莫名失去计划视图）。
/// - 不重算 `cache_gen_index`：修复会缩短历史，该锚点可能落到别的消息上或失效，最坏结果是
///   本次重试少一个代际缓存断点（一次性 1.25x 前缀重写），不影响正确性。
pub(super) fn refresh_request_messages(rt: &Arc<SessionRuntime>, req: &mut StreamRequest) {
    let transient = req.messages.last().filter(|m| is_plan_transient(m)).cloned();
    req.messages = messages_for_request(rt);
    if let Some(t) = transient {
        if !req.messages.last().map(is_plan_transient).unwrap_or(false) {
            req.messages.push(t);
        }
    }
}

/// 是否为 `build_stream_request` 注入的尾部计划瞬态快照消息。
fn is_plan_transient(m: &Message) -> bool {
    m.first_text()
        .map(|t| t.starts_with("<current-plan-transient>"))
        .unwrap_or(false)
}

/// 组装一次 LLM 流式请求：解析生效模型（会话覆盖 → 全局 active）、思考力度、
/// 六层 system prompt（含计划瞬态快照注入）与统一排序的工具集（内置 + MCP）。
pub(super) async fn build_stream_request(
    core: &Arc<AgentCore>,
    rt: &Arc<SessionRuntime>,
    params: &DriveParams,
) -> Result<(crate::core::config::ModelConfig, StreamRequest), String> {
    let cfg = core.cfg.read().unwrap().clone();
    let prefs = rt.prefs();
    // 会话模型覆盖优先；覆盖悬空（模型已删）回落全局 active（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
    let model = match crate::core::prefs::effective_model(&cfg, &prefs) {
        Some(m) => m,
        None => {
            tracing::warn!(
                "会话 {} 配置的模型 {} 已不存在，回落全局 active 模型",
                rt.id,
                prefs.model_id.as_deref().unwrap_or("")
            );
            cfg.active_model().ok_or("未配置模型")?
        }
    };
    // 思考力度：会话覆盖 → 模型配置默认（未知字符串视为未设置）
    let reasoning_effort = prefs.reasoning_effort.or_else(|| {
        model
            .reasoning_effort
            .as_deref()
            .and_then(crate::core::prefs::EffortLevel::parse)
    });
    // system prompt 稳定主块 run 内字节冻结：首步组装后存入 rt，后续步直接复用。
    // 防 run 中途文件变更（agent 自编辑 AGENTS.md 等）打穿 provider 前缀缓存，
    // 同时省每步 10+ 文件重读；变更在下一条用户消息（新 run，drive_agent 清空冻结）生效。
    // 注意先把 clone 落到局部变量再 match：scrutinee 里的临时 MutexGuard 会活到 match 结束，
    // None 分支内再锁同一把锁会自锁死锁
    let cached = rt.system_frozen.lock().unwrap().clone();
    let system_core = match cached {
        Some(core) => core,
        None => {
            // shell 描述跟随配置 selection（与 command 工具执行 shell 同一事实源）
            let shell = crate::tools::command::shell_description(cfg.shell.selection.as_deref());
            let extra = rt.extra_roots.lock().unwrap().clone();
            // 第 2/3 层：技能与记忆索引（字节稳定）
            let skills = core.skills.list(
                &rt.workspace,
                &rt.data_dir,
                &cfg.disabled_skills,
                rt.project_dir.as_deref(),
            );
            let skills_listing = crate::skills::prompt_listing(&skills);
            let memories = crate::memory::scan(&rt.data_dir, rt.project_dir.as_deref());
            let memory_listing = crate::memory::prompt_listing(&memories);
            // 项目段：项目名 + 项目目录 + temps 暂存区（数据目录在 <project>/.codewave，单目录语义）
            let project_section = rt.project_id.as_ref().map(|pid| {
                let project = crate::core::projects::find(&core.data_dir, pid);
                let name = project
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| pid.clone());
                let temps = project
                    .as_ref()
                    .map(|p| crate::core::projects::project_data_dir(&core.data_dir, p).join("temps"))
                    .unwrap_or_else(|| {
                        crate::core::projects::data_dir_by_id(&core.data_dir, pid).join("temps")
                    })
                    .to_string_lossy()
                    .into_owned();
                format!(
                    "\n<project>\n- Project: {name}\n- Project directory: {}\n- Scratch dir & command working directory: {temps}\n  Throwaway files go here; commands start here.\n</project>\n",
                    rt.workspace.to_string_lossy()
                )
            });
            let core = crate::core::prompt::assemble(
                &rt.workspace,
                &cfg,
                &extra,
                shell.as_str(),
                &skills_listing,
                &memory_listing,
                project_section.as_deref(),
                rt.project_dir.as_deref(),
            );
            *rt.system_frozen.lock().unwrap() = Some(core.clone());
            core
        }
    };
    let messages = messages_for_request(rt);
    // 工具集：内置（按排除集过滤）+ MCP（可选），统一按名排序
    let mut tools: Vec<crate::provider::ToolDef> = core
        .tools
        .tool_defs()
        .into_iter()
        .filter(|d| !params.exclude_tools.contains(&d.name))
        .collect();
    if !params.exclude_mcp {
        let mcp_tools = core.mcp.all_tools().await;
        for mt in mcp_tools {
            tools.push(crate::provider::ToolDef {
                name: crate::mcp::server_function_name(&mt.server, &mt.name),
                description: mt.description,
                schema_json: mt.schema_json,
            });
        }
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    // 历史代际断点锚点（滞回前移；历史不足 16 条不启用，压缩后 n 骤减自动重置）
    let cache_gen_index = {
        let n = messages.len();
        let mut anchor = rt.cache_gen_anchor.lock().unwrap();
        let next = gen_anchor_next(*anchor, n);
        *anchor = next;
        next
    };
    Ok((
        model.clone(),
        StreamRequest {
            model,
            system_core,
            system_extra: params.system_extra.clone(),
            messages,
            tools,
            cache_key: Some(rt.id.clone()),
            cache_gen_index,
            reasoning_effort,
            session_id: Some(rt.id.clone()),
        },
    ))
}

/// 历史代际断点锚点滞回：目标位取「距末尾 8 条」与「75% 处」较小者；现锚点仍在有效界内
/// 且目标漂移未超其 1/4 时保持不动（锚点稳定期间该前缀缓存条目被每步请求命中刷新，
/// 5min TTL 不会过期），漂移超阈值才前移到目标位（该段触发一次 1.25x 重写）。
fn gen_anchor_next(cur: Option<usize>, n: usize) -> Option<usize> {
    if n < 16 {
        return None;
    }
    let target = (n - 8).min(n * 3 / 4).max(1);
    match cur {
        Some(c) if c >= 1 && c <= n - 2 && target < c + c / 4 + 1 => Some(c),
        _ => Some(target),
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::gen_anchor_next;

    // 缺陷修复：出网副本必须自我修复脏历史——空 assistant 消息被丢弃、悬空 tool_use 被补
    // [interrupted] 结果、孤儿 tool_result 被清掉，保证「历史不合法 ⇒ 请求体不合法」
    // 这条链路被切断（会话 d9941c4b 的 400 根因之一）。
    #[test]
    fn repair_before_send_sanitizes_dirty_history() {
        use crate::core::types::{Content, Message, Role};
        let dirty = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolUse {
                    id: "t1".into(),
                    name: "read".into(),
                    args: serde_json::json!({ "files": [] }),
                }],
                created_at: None,
            },
            Message {
                role: Role::Assistant,
                content: Vec::new(),
                created_at: None,
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "ghost".into(),
                content: "orphan".into(),
                is_error: false,
            }]),
        ];
        let out = super::repair_before_send(dirty);
        assert!(
            out.iter()
                .all(|m| !(m.role == Role::Assistant && m.content.is_empty())),
            "出网副本不得含空 assistant 消息"
        );
        let answered: Vec<&str> = out
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|c| match c {
                Content::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert!(answered.contains(&"t1"), "悬空 tool_use 应被补 [interrupted] 结果");
        assert!(!answered.contains(&"ghost"), "孤儿 tool_result 应被清掉");
    }

    /// n < 16 不启用代际断点（含压缩后历史骤减的场景）。
    #[test]
    fn gen_anchor_disabled_when_history_short() {
        assert_eq!(gen_anchor_next(None, 0), None);
        assert_eq!(gen_anchor_next(Some(12), 15), None);
    }

    /// 旧锚点越界（压缩后残留大值）时直接落到新目标位，而非保持非法旧值。
    #[test]
    fn gen_anchor_resets_when_out_of_range() {
        assert_eq!(gen_anchor_next(Some(50), 20), Some(12));
    }
}

/// 增量收集：StreamDelta → 流缓冲（节流）+ Assembled 双路写入。
pub(super) async fn collect_deltas(
    mut rx: mpsc::Receiver<crate::provider::StreamDelta>,
    stream: Arc<ThrottledStream>,
) -> Assembled {
    let mut asm = Assembled::default();
    while let Some(d) = rx.recv().await {
        // 每帧刷新流活跃度（含不经 stream 缓冲的 ToolCall 增量）——停滞看门狗的观测点
        stream.touch();
        match d {
            crate::provider::StreamDelta::Text { text } => {
                asm.push_text(&text);
                stream.push_text(&text);
            }
            crate::provider::StreamDelta::Reasoning { text } => {
                asm.push_thinking(&text);
                stream.push_reasoning(&text);
            }
            crate::provider::StreamDelta::ToolCallBegin { index, id, name } => {
                asm.tool_calls.push(AssembledToolCall {
                    index,
                    id,
                    name,
                    args_raw: String::new(),
                });
                // 块记录：捕获工具调用的真实穿插位置（值 = tool_calls 下标）
                asm.blocks.push(AsmBlock::Tool(asm.tool_calls.len() - 1));
            }
            crate::provider::StreamDelta::ToolCallArgsDelta { index, fragment } => {
                if let Some(c) = asm.tool_calls.iter_mut().find(|c| c.index == index) {
                    c.args_raw.push_str(&fragment);
                }
            }
            crate::provider::StreamDelta::ToolCallEnd { .. } => {}
        }
    }
    asm
}

/// 组装 assistant 消息 + 归一化调用；参数无法修复的调用直接拒绝，合成错误结果。
/// text/thinking/tool_use 全部按真实到达顺序（AsmBlock 顺序）产出内容——
/// 此前 tool_use 被统一挪到末尾，重开会话后工具卡穿插位置丢失（已修复，与流式 UI 对齐）。
pub(super) fn build_assistant_message(asm: &Assembled) -> (Message, Vec<NormalizedCall>, Vec<Content>) {
    // 先归一化全部调用（保持顺序），再把 ToolUse 块插回真实位置
    let mut calls: Vec<NormalizedCall> = Vec::new();
    let mut synth = Vec::new();
    let mut ord_of: std::collections::HashMap<usize, usize> = std::collections::HashMap::new(); // asm 工具序号 → calls 下标
    for (ord, c) in asm.tool_calls.iter().enumerate() {
        match crate::core::sessions::repair::parse_or_salvage(&c.args_raw) {
            Some(v) => {
                let args = if v.is_object() {
                    v
                } else {
                    serde_json::json!({ "value": v })
                };
                ord_of.insert(ord, calls.len());
                calls.push(NormalizedCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    args,
                    index: c.index,
                });
            }
            None => {
                let msg = format!(
                    "工具 {c_name} 的参数 JSON 无法解析（长度 {len}），调用被拒绝",
                    c_name = c.name,
                    len = c.args_raw.len()
                );
                tracing::warn!("{msg}");
                synth.push(Content::ToolResult {
                    tool_use_id: c.id.clone(),
                    content: msg,
                    is_error: true,
                });
            }
        }
    }
    let mut content = Vec::new();
    for b in &asm.blocks {
        match b {
            AsmBlock::Text(t) if !t.is_empty() => content.push(Content::Text { text: t.clone() }),
            AsmBlock::Thinking(t) if !t.is_empty() => {
                content.push(Content::Thinking { text: t.clone() })
            }
            AsmBlock::Tool(ord) => {
                if let Some(&ci) = ord_of.get(ord) {
                    let c = &calls[ci];
                    content.push(Content::ToolUse {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        args: c.args.clone(),
                    });
                }
            }
            _ => {} // 空 text/thinking 块不进历史
        }
    }
    (
        Message {
            role: Role::Assistant,
            content,
            created_at: None,
        },
        calls,
        synth,
    )
}

/// 模型侧双通道压缩包装（供批次执行层调用）。
pub fn model_side_result(
    kind: crate::tools::ToolKind,
    name: &str,
    outcome: &crate::tools::ToolOutcome,
) -> String {
    compact_for_model(kind, name, outcome)
}

/// 将一个节流批次按段顺序拆成多条单通道帧依次下发：帧到达序 = 显示顺序。
pub(super) fn flush_segments(
    sink: &Arc<dyn EventSink>,
    session: &SessionId,
    buf: &crate::util::throttle::StreamBuffer,
) {
    for seg in &buf.segments {
        let frame = match seg {
            crate::util::throttle::Segment::Text(t) => Frame::DeltaText {
                generation: buf.generation,
                text: t.clone(),
            },
            crate::util::throttle::Segment::Reasoning(t) => Frame::DeltaThinking {
                generation: buf.generation,
                text: t.clone(),
            },
        };
        sink.channel_frame(session, &frame);
    }
}

/// 64ms 流式刷新 ticker（run 期间全程存活）。
pub async fn stream_flush_loop(
    sink: Arc<dyn EventSink>,
    rt: Arc<SessionRuntime>,
    stop: CancellationToken,
) {
    let interval = Duration::from_millis(STREAM_THROTTLE_MS);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                if let Some(buf) = rt.stream.try_take(interval) {
                    flush_segments(&sink, &rt.id, &buf);
                }
            }
        }
    }
}

