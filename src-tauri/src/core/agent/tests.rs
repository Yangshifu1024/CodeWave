use super::drive::budget_notice_step;
use super::drive::main_drive_params;
use super::runtime::{Frame, SessionRuntime};
use super::stream::{build_assistant_message, build_stream_request, flush_segments};
use super::*;
use crate::core::types::{Content, Message, Role, SessionId};
use crate::provider::dto::{AsmBlock, Assembled, AssembledToolCall};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// [docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)：子代理帧信封的序列化形态（前端 types.ts Frame 联合类型的 serde 锚点）。
#[test]
fn frame_sub_envelope_serialization_shape() {
    let f = Frame::Sub {
        sub_id: "sub_ab12cd34".into(),
        frame: Box::new(Frame::DeltaText {
            generation: 3,
            text: "hi".into(),
        }),
    };
    let v: serde_json::Value = serde_json::to_value(&f).unwrap();
    assert_eq!(v["type"], "sub");
    assert_eq!(v["sub_id"], "sub_ab12cd34");
    assert_eq!(v["frame"]["type"], "delta_text");
    assert_eq!(v["frame"]["gen"], 3);
    assert_eq!(v["frame"]["text"], "hi");
}

/// 低预算提醒在剩余 20% 时触发：step 从 0 计已耗步数，故触发点在
/// max_steps - max_steps / 5（旧实现是 max_steps / 5——在已耗 20% 时就触发了）。
#[test]
fn budget_notice_step_at_20_percent_remaining() {
    assert_eq!(budget_notice_step(25), 20); // 子代理默认：剩 5 步
    assert_eq!(budget_notice_step(30), 24); // 任务运行预算：剩 6 步
    assert_eq!(budget_notice_step(1000), 800);
    // max_steps < 5：值落在 0..max_steps 之外，提醒永不触发
    for m in 1..5 {
        assert!(budget_notice_step(m) >= m);
    }
}

/// 无工具调用回合的处置矩阵（[docs/subagent-text-turn-premature-exit](../../../../docs/subagent-text-turn-premature-exit.md)）：
/// 主会话语义不变；子代理/任务须带 `<report>` 标记收尾，否则有界续跑，超限显式失败
///（绝不静默把过程旁白当成功收尾）。
#[test]
fn text_turn_action_matrix() {
    use super::drive::{MAX_TEXT_TURNS, TextTurnAction, text_turn_action};
    // ① 主会话：纯文本回合即完成（行为不变）
    assert_eq!(
        text_turn_action("答完了", true, false, 0, false),
        TextTurnAction::Finish
    );
    assert_eq!(
        text_turn_action("", true, false, 9, false),
        TextTurnAction::Finish
    );
    // ①’ 主会话 + 被拒调用 → 继续（**本缺陷的锚点**：[docs/rejected-call-silent-finish]）
    assert_eq!(
        text_turn_action("下面是完整方案", true, true, 0, false),
        TextTurnAction::Continue
    );
    // ①’’ 被拒也受 MAX_TEXT_TURNS 硬上限约束（不无限续跑）
    assert_eq!(
        text_turn_action("下面是完整方案", true, true, MAX_TEXT_TURNS, false),
        TextTurnAction::StopWithLimit
    );
    // ①’’’ 被拒优先于 <report>：子代理「已写汇报但同回合有调用被拒」再多走一步
    //（有意取舍：被拒调用尚未被模型知晓；已登记为遗留）
    assert_eq!(
        text_turn_action("<report>完成</report>", false, true, 0, false),
        TextTurnAction::Continue
    );
    // ② 非主会话 + <report> 标记 → 完成（不计数）
    assert_eq!(
        text_turn_action("<report>完成 A，未完成 B</report>", false, false, 0, false),
        TextTurnAction::Finish
    );
    assert_eq!(
        text_turn_action(
            "回报如下 <report>x</report>",
            false,
            false,
            MAX_TEXT_TURNS,
            false
        ),
        TextTurnAction::Finish
    );
    // ③ 非主会话纯旁白 / 空文本（唯一调用被拒）→ 继续
    assert_eq!(
        text_turn_action("接下来我来改 AppShell", false, false, 0, false),
        TextTurnAction::Continue
    );
    assert_eq!(
        text_turn_action("", false, false, MAX_TEXT_TURNS - 1, false),
        TextTurnAction::Continue
    );
    // ④ 触上限 → 显式失败（不伪装成功）
    assert_eq!(
        text_turn_action("仍然只是旁白", false, false, MAX_TEXT_TURNS, false),
        TextTurnAction::StopWithLimit
    );
    assert_eq!(
        text_turn_action("", false, false, MAX_TEXT_TURNS + 5, false),
        TextTurnAction::StopWithLimit
    );
}

/// `<report>` 标记剥离：标记只用于收尾判定，不进入汇报正文。
#[test]
fn split_report_strips_tag() {
    use super::drive::split_report;
    assert_eq!(split_report("普通汇报"), ("普通汇报".to_string(), false));
    assert_eq!(
        split_report("<report>完成 A，未完成 B</report>"),
        ("完成 A，未完成 B".to_string(), true)
    );
    // 标记之外的话术不属于汇报正文
    assert_eq!(
        split_report("我这就收尾\n<report>结论</report>"),
        ("结论".to_string(), true)
    );
    // 未闭合标记：其后全部视为正文
    assert_eq!(split_report("<report>未闭合"), ("未闭合".to_string(), true));
    // 多个标记：只取第一个（标记即汇报边界）
    assert_eq!(
        split_report("<report>第一段</report> <report>第二段</report>"),
        ("第一段".to_string(), true)
    );
    // 闭合标记之后的尾随正文丢弃
    assert_eq!(
        split_report("<report>A</report> 附注"),
        ("A".to_string(), true)
    );
    // 只有闭合标记（模型写坏的常见形态）→ 视为不带标记
    assert_eq!(
        split_report("结论</report>"),
        ("结论</report>".to_string(), false)
    );
}

/// 连续无工具调用触上限：显式失败（绝不静默当成功）+ 终止引导消息进历史。
/// 文档 §4 承诺的端到端用例（修复前：第 1 个纯文本回合就以旁白为最终汇报成功退出）。
#[tokio::test]
async fn consecutive_text_turns_stop_at_limit() {
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"content":"仍然只是旁白"}}]}"#,
        SSE_STOP,
    ]);
    // 4 连纯文本：MAX_TEXT_TURNS(3) 次续跑后第 4 步终止
    let (port, hits) =
        spawn_scripted_sse(vec![body.clone(), body.clone(), body.clone(), body]).await;
    let (core, rt, _ws, _dd) = scripted_core(port, "sub-text-limit");
    let params = DriveParams {
        max_steps: 8,
        finish_on_text: false,
        emit_events: false,
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_text_limit").await;
    assert!(
        result.is_err(),
        "连续无工具调用应以显式错误终止，而非成功收尾：{:?}",
        result.ok()
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        4,
        "3 次续跑提示后第 4 步终止（不是无限续跑，也不是第 1 步就收尾）"
    );
    let texts = history_texts(&rt);
    assert!(
        texts.iter().any(|t| t.contains("<text-turn-limit>")),
        "应注入终止引导（下一 run 先用 ask 确认），实际历史：{texts:?}"
    );
}

#[tokio::test]
async fn stream_request_bytes_stable_across_calls() {
    // [docs/p1-plan](../../../../docs/p1-plan.md) §8 质量门：同一会话连续请求必须前缀字节相等（缓存优先）
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    std::fs::write(ws.path().join("AGENTS.md"), "rule").unwrap();
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "cache-test",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    rt.history
        .lock()
        .unwrap()
        .push(crate::core::types::Message::user_text("hello"));
    let params = DriveParams::default();
    let (_, r1) = build_stream_request(&core, &rt, &params).await.unwrap();
    let (_, r2) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert_eq!(r1.system_core, r2.system_core, "system 必须字节稳定");
    let t1: Vec<&str> = r1.tools.iter().map(|d| d.name.as_str()).collect();
    let t2: Vec<&str> = r2.tools.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(t1, t2);
    assert_eq!(
        serde_json::to_string(&r1.tools).unwrap(),
        serde_json::to_string(&r2.tools).unwrap()
    );
    // 排序不变量
    let mut sorted = t1.clone();
    sorted.sort();
    assert_eq!(t1, sorted, "工具列表必须按名排序");
    // 消息数组同样必须逐字节稳定：本轮新增的出网副本 repair 是唯一可能每步改写消息
    // 数组的逻辑（repair 幂等 ⇒ 副本 = f(history) 确定），否则每步都打穿前缀缓存
    assert_eq!(
        serde_json::to_string(&r1.messages).unwrap(),
        serde_json::to_string(&r2.messages).unwrap(),
        "messages 必须逐字节稳定（prompt cache 前缀）"
    );
}

/// 缺陷修复回归：脏历史（悬空 tool_use + 空 assistant + 孤儿 tool_result）下，
/// 出网副本的修复必须幂等——连续两次构建的 messages 逐字节相等。
#[tokio::test]
async fn dirty_history_messages_stay_byte_stable() {
    let roots = test_roots();
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "cache-dirty",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    {
        let mut h = rt.history.lock().unwrap();
        h.push(Message::user_text("q"));
        // 悬空 tool_use：无对应结果
        h.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                id: "t1".into(),
                name: "read".into(),
                args: serde_json::json!({ "files": [] }),
            }],
            created_at: None,
        });
        h.push(Message {
            role: Role::Assistant,
            content: Vec::new(),
            created_at: None,
        });
        // 孤儿 tool_result：无对应 tool_use
        h.push(Message::tool_results(vec![Content::ToolResult {
            tool_use_id: "ghost".into(),
            content: "o".into(),
            is_error: false,
        }]));
    }
    let params = DriveParams::default();
    let (_, r1) = build_stream_request(&core, &rt, &params).await.unwrap();
    let (_, r2) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert_eq!(
        serde_json::to_string(&r1.messages).unwrap(),
        serde_json::to_string(&r2.messages).unwrap(),
        "脏历史修复必须幂等（否则每步打穿前缀缓存）"
    );
    assert!(
        r1.messages
            .iter()
            .all(|m| !(m.role == Role::Assistant && m.content.is_empty())),
        "出网副本不得含空 assistant 消息"
    );
    // 悬空 tool_use 在副本上被补 [interrupted] 结果
    let answered: Vec<&str> = r1
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|c| match c {
            Content::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(answered.contains(&"t1"));
    assert!(!answered.contains(&"ghost"));
}

#[tokio::test]
async fn system_prompt_frozen_within_run() {
    // [docs/prompt-caching-hardening]：run 内 system 稳定主块冻结——文件变更不穿透当前 run
    //（保 provider 前缀缓存），新 run（drive_agent 起点清空冻结）才重新组装纳入新内容
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    std::fs::write(ws.path().join("AGENTS.md"), "rule-v1").unwrap();
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "freeze-test",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    let params = DriveParams::default();
    let (_, r1) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert!(r1.system_core.contains("rule-v1"));
    // run 中途改文件：冻结生效，system 主块字节不变
    std::fs::write(ws.path().join("AGENTS.md"), "rule-v2").unwrap();
    let (_, r2) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert_eq!(r1.system_core, r2.system_core, "run 内冻结：文件变更不穿透");
    // 新 run：清空冻结（drive_agent 同款操作）→ 重新组装
    *rt.system_frozen.lock().unwrap() = None;
    let (_, r3) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert!(r3.system_core.contains("rule-v2"), "新 run 重新组装");
}

#[tokio::test]
async fn cache_gen_anchor_hysteresis() {
    // [docs/prompt-caching-hardening]：代际断点锚点滞回——漂移未超 1/4 保持不动（命中刷新
    // TTL），超阈值才前移（触发一次段重写）；历史不足 16 条不启用
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "gen-anchor-test",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    let params = DriveParams::default();
    {
        let mut h = rt.history.lock().unwrap();
        for i in 0..20 {
            h.push(Message::user_text(format!("m{i}")));
        }
    }
    let (_, r1) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert_eq!(
        r1.cache_gen_index,
        Some(12),
        "n=20 → 目标位 min(n-8, 3n/4)=12"
    );
    {
        let mut h = rt.history.lock().unwrap();
        for i in 20..23 {
            h.push(Message::user_text(format!("m{i}")));
        }
    }
    let (_, r2) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert_eq!(
        r2.cache_gen_index,
        Some(12),
        "目标 15 vs 锚点 12+3：漂移未超 1/4 → 保持"
    );
    {
        let mut h = rt.history.lock().unwrap();
        for i in 23..27 {
            h.push(Message::user_text(format!("m{i}")));
        }
    }
    let (_, r3) = build_stream_request(&core, &rt, &params).await.unwrap();
    assert_eq!(r3.cache_gen_index, Some(19), "漂移超阈值 → 前移到目标位");
}

#[tokio::test]
async fn running_flag_resets_after_run_ends() {
    // C1 回归：run 结束（此处为快速失败）后 running 必须复位，第二次 start 才能成功
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    // 本地 mock 返回 401 → Auth 错误不可重试 → run 立即结束
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // mock 返回 401；客户端可能开多条连接（Windows 上单次 accept 后关闭 listener
    // 会让后续连接悬挂到 RST），因此用非阻塞 accept 循环在 10s 窗口内一律应答。
    // Windows 特有：关闭时若接收缓冲仍有未读数据（POST body 从不读取），连接以
    // RST 收场，客户端看不到 401 → 被判可重试 → 退避重试拖爆测试时限。
    // 故先排空请求，再应答 + 显式 shutdown。
    listener.set_nonblocking(true).unwrap();
    let start = std::time::Instant::now();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        while start.elapsed() < std::time::Duration::from_secs(10) {
            match listener.accept() {
                Ok((mut sock, _)) => {
                    let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(100)));
                    let mut buf = [0u8; 8192];
                    loop {
                        match sock.read(&mut buf) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(_) => break, // 100ms 无数据 = 请求已收全
                        }
                    }
                    let _ = sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    let _ = sock.flush();
                    let _ = sock.shutdown(std::net::Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    {
        let mut cfg = core.cfg.write().unwrap();
        cfg.providers[0].base_url = format!("http://127.0.0.1:{port}/v1");
    }
    let rt = core.get_or_create_session("c1", roots.workspace.clone(), None, vec![], None, vec![]);
    let run1 = core.start_chat(rt.clone(), "first".into(), vec![]).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while rt.running.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!rt.running.load(Ordering::SeqCst), "run 应在超时前结束");
    let _ = run1;
    let run2 = core.start_chat(rt, "second".into(), vec![]);
    assert!(run2.is_ok(), "C1 回归：第二次 start 被拒：{:?}", run2.err());
}

#[derive(Default)]
struct CaptureSink(std::sync::Mutex<Vec<Frame>>);
impl EventSink for CaptureSink {
    fn channel_frame(&self, _s: &SessionId, f: &Frame) {
        self.0.lock().unwrap().push(f.clone());
    }
    fn emit(&self, _s: &SessionId, _e: &str, _p: serde_json::Value) {}
}

#[test]
fn flush_segments_emits_frames_in_segment_order() {
    // 评审 T1：段顺序到帧顺序的直接契约（[docs/thinking-interleave-report](../../../../docs/thinking-interleave-report.md)）
    let sink = Arc::new(CaptureSink::default());
    let mut buf = crate::util::throttle::StreamBuffer {
        generation: 7,
        ..Default::default()
    };
    buf.segments = vec![
        crate::util::throttle::Segment::Reasoning("r".into()),
        crate::util::throttle::Segment::Text("t".into()),
    ];
    flush_segments(
        &(sink.clone() as Arc<dyn EventSink>),
        &"s".to_string(),
        &buf,
    );
    let frames = sink.0.lock().unwrap();
    assert_eq!(frames.len(), 2);
    assert!(
        matches!(&frames[0], Frame::DeltaThinking { generation: 7, text } if text.as_str() == "r")
    );
    assert!(matches!(&frames[1], Frame::DeltaText { generation: 7, text } if text.as_str() == "t"));
}

#[test]
fn assistant_assembly_salvage_and_reject() {
    // 正常：text/thinking/tool_use 按真实到达顺序成块（本例工具块在最后）
    let mut asm = Assembled::default();
    asm.push_text("doing");
    asm.push_thinking("thinking");
    asm.tool_calls.push(AssembledToolCall {
        index: 0,
        id: "t1".into(),
        name: "read".into(),
        args_raw: r#"{"files":[]}"#.into(),
    });
    asm.blocks.push(AsmBlock::Tool(0));
    let (msg, calls, synth) = build_assistant_message(&asm);
    assert_eq!(calls.len(), 1);
    assert_eq!(msg.content.len(), 3); // text + thinking + tool_use
    assert!(matches!(&msg.content[0], Content::Text { .. }));
    assert!(matches!(&msg.content[1], Content::Thinking { .. }));
    assert!(matches!(&msg.content[2], Content::ToolUse { id, .. } if id == "t1"));
    assert!(synth.is_empty());

    // 截断且无法修复 → 拒绝并合成错误结果
    let mut asm = Assembled::default();
    asm.tool_calls.push(AssembledToolCall {
        index: 0,
        id: "t2".into(),
        name: "edit".into(),
        args_raw: r#"{"files": "unclosed"#.into(),
    });
    let (msg, calls, synth) = build_assistant_message(&asm);
    assert!(calls.is_empty());
    assert_eq!(synth.len(), 1);
    assert!(matches!(
        &synth[0],
        Content::ToolResult { is_error: true, .. }
    ));
    assert!(msg.content.is_empty());
}

#[test]
fn assistant_assembly_interleaves_blocks_in_order() {
    // 思考/正文穿插到达 → 历史消息内容保持真实顺序
    let mut asm = Assembled::default();
    asm.push_thinking("t1");
    asm.push_text("text1");
    asm.push_thinking("t2");
    asm.push_text("text2");
    let (msg, _, _) = build_assistant_message(&asm);
    let kinds: Vec<&str> = msg
        .content
        .iter()
        .map(|c| match c {
            Content::Text { .. } => "text",
            Content::Thinking { .. } => "thinking",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["thinking", "text", "thinking", "text"]);
}

// ===== 会话级运行偏好（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)） =====

fn prefs_of(
    mode: crate::core::prefs::ApprovalMode,
    model_id: Option<String>,
) -> crate::core::prefs::SessionPrefs {
    crate::core::prefs::SessionPrefs {
        approval_mode: mode,
        model_id,
        reasoning_effort: Some(crate::core::prefs::EffortLevel::High),
    }
}

#[test]
fn sub_runtime_inherits_prefs() {
    let ws = tempfile::tempdir().unwrap();
    let rt = test_support::make_runtime(ws.path().to_path_buf(), ws.path().to_path_buf(), vec![]);
    rt.set_prefs(prefs_of(
        crate::core::prefs::ApprovalMode::Plan,
        Some("m".into()),
    ));
    let sub = SessionRuntime::new_sub(&rt, "sub-1".into());
    assert_eq!(
        sub.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    );
    assert_eq!(sub.prefs().model_id.as_deref(), Some("m"));
}

#[tokio::test]
async fn new_session_prefs_follow_global_switch() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots); // approval.enabled 默认 true
    let rt = core.get_or_create_session("p0", roots.workspace.clone(), None, vec![], None, vec![]);
    assert_eq!(
        rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    );
    {
        let mut cfg = core.cfg.write().unwrap();
        cfg.approval.enabled = false;
    }
    let rt2 = core.get_or_create_session("p1", roots.workspace.clone(), None, vec![], None, vec![]);
    assert_eq!(
        rt2.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::FullAccess
    );
}

#[tokio::test]
async fn plan_mode_excludes_write_tools() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    let rt =
        core.get_or_create_session("plan", roots.workspace.clone(), None, vec![], None, vec![]);
    rt.set_prefs(prefs_of(crate::core::prefs::ApprovalMode::Plan, None));
    rt.history
        .lock()
        .unwrap()
        .push(crate::core::types::Message::user_text("hi"));
    let params = main_drive_params(&rt.prefs(), None);
    let (_, req) = build_stream_request(&core, &rt, &params).await.unwrap();
    let names: Vec<&str> = req.tools.iter().map(|d| d.name.as_str()).collect();
    for t in ["edit", "create", "delete"] {
        assert!(!names.contains(&t), "计划模式不得包含写工具 {t}");
    }
    assert!(names.contains(&"read"), "只读工具保留");
    // system prompt 携带 plan-mode 说明
    assert!(req.system_full().contains("plan-mode"));
    // 对照：AutoEdit 保留写工具
    rt.set_prefs(prefs_of(crate::core::prefs::ApprovalMode::AutoEdit, None));
    let params = main_drive_params(&rt.prefs(), None);
    let (_, req) = build_stream_request(&core, &rt, &params).await.unwrap();
    let names: Vec<&str> = req.tools.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"edit"));
}

#[tokio::test]
async fn compacting_guard_resets_flag_on_panic_unwind() {
    // [docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md) 评审修复回归：压缩互斥 RAII——compact_history panic unwind 时
    // Drop 仍复位 compacting，否则 start_chat 永远拒绝新 run（会话变砖，
    // 与已修复的 panic 死锁同类）
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session("cg", roots.workspace.clone(), None, vec![], None, vec![]);

    let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
        let _guard = CompactingGuard::acquire(&rt).expect("空闲会话占位应成功");
        assert!(rt.compacting.load(Ordering::SeqCst));
        panic!("模拟 compact_history 内部 panic");
    }))
    .await;
    assert!(result.is_err(), "panic 应被 catch_unwind 捕获");
    assert!(
        !rt.compacting.load(Ordering::SeqCst),
        "unwind 后 compacting 必须复位，否则会话变砖"
    );

    // acquire 语义：占位期间二次 acquire 返回 None（且不清槽）；释放后槽位可再抢
    let g = CompactingGuard::acquire(&rt).unwrap();
    assert!(CompactingGuard::acquire(&rt).is_none());
    assert!(rt.compacting.load(Ordering::SeqCst));
    drop(g);
    assert!(CompactingGuard::acquire(&rt).is_some());
}

#[tokio::test]
async fn session_model_override_and_dangling_fallback() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    {
        let mut cfg = core.cfg.write().unwrap();
        let m2 = crate::core::config::ProviderModel {
            id: "model-b".into(),
            ..Default::default()
        };
        cfg.providers[0].models.push(m2);
    }
    let rt = core.get_or_create_session("ov", roots.workspace.clone(), None, vec![], None, vec![]);
    rt.history
        .lock()
        .unwrap()
        .push(crate::core::types::Message::user_text("hi"));

    // 覆盖命中 → 使用会话模型
    rt.set_prefs(prefs_of(
        crate::core::prefs::ApprovalMode::AutoEdit,
        Some("model-b".into()),
    ));
    let (model, _) = build_stream_request(&core, &rt, &DriveParams::default())
        .await
        .unwrap();
    assert_eq!(model.id, "model-b");

    // 覆盖悬空 → 回落全局 active
    rt.set_prefs(prefs_of(
        crate::core::prefs::ApprovalMode::AutoEdit,
        Some("gone".into()),
    ));
    let (model, _) = build_stream_request(&core, &rt, &DriveParams::default())
        .await
        .unwrap();
    assert_eq!(
        model.id,
        core.cfg.read().unwrap().active_model_id.clone().unwrap()
    );

    // 力度覆盖到达请求
    rt.set_prefs(prefs_of(
        crate::core::prefs::ApprovalMode::AutoEdit,
        Some("model-b".into()),
    ));
    let (_, req) = build_stream_request(&core, &rt, &DriveParams::default())
        .await
        .unwrap();
    assert_eq!(
        req.reasoning_effort,
        Some(crate::core::prefs::EffortLevel::High)
    );
}

#[test]
fn start_chat_builds_image_message() {
    // 附件消息构造：text + image 块（构造与 run 入口解耦；此处直接验证 start_chat 前奏）
    let images = vec![
        crate::core::prefs::ImageIn {
            mime: "image/png".into(),
            data: "AAAA".into(),
        },
        crate::core::prefs::ImageIn {
            mime: "image/jpeg".into(),
            data: "BBBB".into(),
        },
    ];
    let text = "看这两张图".to_string();
    let mut content = vec![Content::Text { text: text.clone() }];
    for img in images {
        content.push(Content::Image {
            media_type: img.mime,
            data: img.data,
        });
    }
    let msg = Message {
        role: Role::User,
        content,
        created_at: None,
    };
    assert_eq!(msg.content.len(), 3);
    assert!(matches!(&msg.content[0], Content::Text { text } if text == "看这两张图"));
    assert!(
        matches!(&msg.content[1], Content::Image { media_type, .. } if media_type == "image/png")
    );
}

#[test]
fn assistant_assembly_places_tool_use_at_real_position() {
    // 修复回归：tool_use 不再统一挪到末尾；真实穿插顺序保留
    // （重开会话后工具卡位置一致）
    let mut asm = Assembled::default();
    asm.push_thinking("t1");
    asm.tool_calls.push(AssembledToolCall {
        index: 0,
        id: "a".into(),
        name: "read".into(),
        args_raw: "{}".into(),
    });
    asm.blocks.push(AsmBlock::Tool(0));
    asm.push_thinking("t2");
    asm.tool_calls.push(AssembledToolCall {
        index: 1,
        id: "b".into(),
        name: "grep".into(),
        args_raw: "{}".into(),
    });
    asm.blocks.push(AsmBlock::Tool(1));
    asm.push_text("done");
    let (msg, calls, _) = build_assistant_message(&asm);
    let kinds: Vec<&str> = msg
        .content
        .iter()
        .map(|c| match c {
            Content::Text { .. } => "text",
            Content::Thinking { .. } => "thinking",
            Content::ToolUse { .. } => "tool",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["thinking", "tool", "thinking", "tool", "text"]);
    assert_eq!(calls.len(), 2);
    assert!(matches!(&calls[0], NormalizedCall { id, .. } if id == "a"));
}

// ---------- checkpoint 身份早退（untitled 幽灵会话缺陷回归） ----------

/// 测试用 WriteRoots（workspace/data_dir 均指向临时目录）。
fn test_roots() -> crate::tools::pathutil::WriteRoots {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    }
}

/// 缺陷修复（会话 d9941c4b 的 400 根因之一）：BadRequest 后 sanitize/repair 只改写
/// `rt.history`，若请求体不重建，重试发出的 body 与首次相同 → 必然复现同一个 400。
/// 本用例锚定「重建后 req.messages 取自当前（已修复的）历史，且已不含空 assistant」。
#[tokio::test]
async fn refresh_request_messages_rewrites_body_from_repaired_history() {
    let roots = test_roots();
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "fix-refresh",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    // 历史进入待重试状态：一条脏结构（空 assistant 消息）
    rt.history.lock().unwrap().push(Message::user_text("q"));
    rt.history.lock().unwrap().push(Message {
        role: Role::Assistant,
        content: Vec::new(),
        created_at: None,
    });
    let mut req = crate::provider::dto::StreamRequest {
        model: crate::core::config::ModelConfig::default(),
        system_core: "sys".into(),
        system_extra: String::new(),
        messages: vec![Message::user_text("stale")], // 构建时快照
        tools: vec![],
        cache_key: None,
        cache_gen_index: None,
        reasoning_effort: None,
        session_id: None,
    };
    super::stream::refresh_request_messages(&rt, &mut req, false);
    // 不再是构建时那份陈旧快照（仅含 "stale"）
    assert!(
        !matches!(&req.messages[0].content[0], Content::Text { text } if text == "stale"),
        "请求体必须重建，不能沿用构建时的快照"
    );
    // 取自当前历史：脏结构（空 assistant）已被 repair 剔除
    assert_eq!(req.messages.len(), 1, "空 assistant 消息应在重建时被清理");
    assert!(matches!(&req.messages[0].content[0], Content::Text { text } if text == "q"));
    assert!(
        req.messages
            .iter()
            .all(|m| !(m.role == Role::Assistant && m.content.is_empty())),
        "重建后的请求体不得含空 assistant 消息"
    );
}

/// checkpoint 不把子代理 runtime 写进主索引/主历史：过程历史由 save_sub_history
/// 边车接管（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)），绝不泄漏为 untitled 幽灵会话。
#[tokio::test]
async fn checkpoint_skips_subagent_runtime() {
    let roots = test_roots();
    let core = test_support::make_core(&roots);
    let parent = core.get_or_create_session(
        "parent-1",
        roots.workspace.clone(),
        Some("proj-1".into()),
        vec![roots.workspace.to_string_lossy().into_owned()],
        None,
        vec![],
    );
    let sub = SessionRuntime::new_sub(&parent, "sub_test1234".into());
    sub.history
        .lock()
        .unwrap()
        .push(Message::user_text("子代理任务"));
    super::drive::checkpoint(&core, &sub).await;
    // 索引无条目（不随项目出现在会话列表）
    assert!(core.store.list().is_empty());
    // 主历史 gz 不存在
    assert!(
        !roots
            .data_dir
            .join("histories/sub_test1234.json.gz")
            .exists()
    );
    assert!(!sub.is_main_session);
}

/// checkpoint 不把任务运行 runtime 写进主索引/主历史（任务运行无持久化语义）。
#[tokio::test]
async fn checkpoint_skips_task_runtime() {
    let roots = test_roots();
    let core = test_support::make_core(&roots);
    let rt = SessionRuntime::new_task(
        "task_test1".into(),
        roots.data_dir.clone(),
        roots.workspace.clone(),
        roots.data_dir.clone(),
    );
    rt.history
        .lock()
        .unwrap()
        .push(Message::user_text("计划任务"));
    super::drive::checkpoint(&core, &rt).await;
    assert!(core.store.list().is_empty());
    assert!(!roots.data_dir.join("histories/task_test1.json.gz").exists());
    assert!(!rt.is_main_session);
}

/// 反向防回归：主会话 checkpoint 照常持久化（索引条目 + gz 都在）。
#[tokio::test]
async fn checkpoint_persists_main_session() {
    let roots = test_roots();
    let core = test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "main-1",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    rt.history
        .lock()
        .unwrap()
        .push(Message::user_text("主会话消息"));
    super::drive::checkpoint(&core, &rt).await;
    let metas = core.store.list();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].id, "main-1");
    assert!(roots.data_dir.join("histories/main-1.json.gz").exists());
    assert!(rt.is_main_session);
}

// ---- 端到端：无工具调用回合的收尾判定（[docs/subagent-text-turn-premature-exit](../../../../docs/subagent-text-turn-premature-exit.md)）----

/// 脚本化 SSE mock：按连接顺序逐个应答，并统计连接数（多回合端到端断言用）。
/// 脚本用尽后重复最后一条，避免断言失败时后续连接悬挂。
async fn spawn_scripted_sse(script: Vec<Vec<u8>>) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        let mut idx = 0usize;
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 8192];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            if let Some(resp) = script.get(idx).or_else(|| script.last()) {
                let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, resp).await;
                let _ = tokio::io::AsyncWriteExt::flush(&mut sock).await;
            }
            idx += 1;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
    (port, hits)
}

/// openai_chat 协议 SSE 响应体（与 provider/tests_integration.rs 的 sse() 同构）。
fn sse_body(chunks: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for c in chunks {
        body.push_str(&format!("data: {c}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
}

const SSE_STOP: &str = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;

/// 指向脚本化 mock 的 core + runtime（两个 tempdir 由调用方绑定保活）。
#[allow(clippy::type_complexity)]
fn scripted_core(
    port: u16,
    name: &str,
) -> (
    Arc<AgentCore>,
    Arc<SessionRuntime>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    {
        let mut cfg = core.cfg.write().unwrap();
        cfg.providers[0].base_url = format!("http://127.0.0.1:{port}/v1");
        cfg.providers[0].keys = vec!["test-key".into()];
    }
    let rt = core.get_or_create_session(name, roots.workspace.clone(), None, vec![], None, vec![]);
    (core, rt, ws, dd)
}

fn history_texts(rt: &Arc<SessionRuntime>) -> Vec<String> {
    rt.history
        .lock()
        .unwrap()
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// 非主会话（子代理 / 任务运行）的纯文本回合不再结束 run：注入续跑提示继续，
/// 直到带 `<report>` 标记才收尾。修复前第 1 个纯文本回合即以旁白为最终汇报成功退出。
#[tokio::test]
async fn subagent_text_only_turn_does_not_end_run() {
    let (port, hits) = spawn_scripted_sse(vec![
        sse_body(&[
            r#"{"choices":[{"delta":{"content":"我先看看文件"}}]}"#,
            SSE_STOP,
        ]),
        sse_body(&[
            r#"{"choices":[{"delta":{"content":"<report>完成 A，未完成 B</report>"}}]}"#,
            SSE_STOP,
        ]),
    ])
    .await;
    let (core, rt, _ws, _dd) = scripted_core(port, "sub-text-turn");
    let params = DriveParams {
        max_steps: 6,
        finish_on_text: false,
        // 跳过上下文压缩分支（子代理同样是 emit_events: false）
        emit_events: false,
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_text_turn").await;
    assert!(result.is_ok(), "应正常收尾：{:?}", result.err());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "第 1 个纯文本回合不应结束 run（修复前连接数会是 1——旁白被当成最终汇报）"
    );
    let texts = history_texts(&rt);
    assert!(
        texts.iter().any(|t| t.contains("<continue-notice>")),
        "应注入续跑提示，实际历史：{texts:?}"
    );
    assert!(result.unwrap().contains("<report>"));
}

/// 主会话对照：纯文本回合仍是「回答完毕」——行为逐字节不变（连接数 1）。
#[tokio::test]
async fn main_session_text_only_turn_ends_run() {
    let (port, hits) = spawn_scripted_sse(vec![sse_body(&[
        r#"{"choices":[{"delta":{"content":"答完了"}}]}"#,
        SSE_STOP,
    ])])
    .await;
    let (core, rt, _ws, _dd) = scripted_core(port, "main-text-turn");
    let params = DriveParams {
        max_steps: 6,
        emit_events: false,
        // finish_on_text 取默认 true = 主会话语义
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_main_text").await;
    assert!(result.is_ok());
    assert_eq!(hits.load(Ordering::SeqCst), 1, "主会话纯文本回合应即结束");
    assert_eq!(result.unwrap(), "答完了");
    assert!(
        !history_texts(&rt)
            .iter()
            .any(|t| t.contains("<continue-notice>")),
        "主会话不应收到续跑提示"
    );
}

/// 被拒调用（参数 JSON 不可修复）：不再静默把该回合当成功收尾——
/// 以 user 角色提示反馈给模型并继续；空 assistant 消息不入历史。
#[tokio::test]
async fn rejected_call_turn_injects_hint_and_continues() {
    let (port, hits) = spawn_scripted_sse(vec![
            sse_body(&[
                // 在字符串中部截断 → parse_or_salvage 返回 None → 调用被拒
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"edit","arguments":"{\"files\": [{\"path\": \"unfinished"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            ]),
            sse_body(&[
                r#"{"choices":[{"delta":{"content":"<report>改动未执行，原因见上</report>"}}]}"#,
                SSE_STOP,
            ]),
        ])
        .await;
    let (core, rt, _ws, _dd) = scripted_core(port, "sub-rejected-call");
    let params = DriveParams {
        max_steps: 6,
        finish_on_text: false,
        emit_events: false,
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_rejected").await;
    assert!(result.is_ok(), "应正常收尾：{:?}", result.err());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "被拒调用回合应继续而非立即收尾"
    );
    let texts = history_texts(&rt);
    assert!(
        texts.iter().any(|t| t.contains("<tool-args-rejected>")),
        "应把被拒情况反馈给模型，实际历史：{texts:?}"
    );
    // 空 assistant 消息不入历史（否则后续请求会带上非法消息被判 400）
    let empty_assistant = rt
        .history
        .lock()
        .unwrap()
        .iter()
        .any(|m| m.role == Role::Assistant && m.content.is_empty());
    assert!(!empty_assistant, "空 assistant 消息不应进入历史");
}

/// 缺陷回归锚点（[docs/rejected-call-silent-finish]）：主会话「正文非空 + 全部调用被拒」的
/// 回合必须继续一步，不得静默收尾。会话 5100ea0c 实测：模型输出「方案已登记为 todos。
/// 下面是完整方案（含分支名 …）」，同回合那个 8531 字符的 `ask` 参数不可解析被拒 → run
/// 报成功结束，方案卡从未产出（用户视角＝「卡死、没有继续输出方案」）。
/// 同时守护：主会话不得注入 <continue-notice>（被拒场景已由 <tool-args-rejected> 反馈）。
#[tokio::test]
async fn main_session_text_with_rejected_call_does_not_silently_finish() {
    let (port, hits) = spawn_scripted_sse(vec![
            sse_body(&[
                r#"{"choices":[{"delta":{"content":"方案已登记为 todos。下面是完整方案。"}}]}"#,
                // 字符串中部截断 → parse_or_salvage 返回 None → 调用被拒（与 5100ea0c 同形态）
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"ask","arguments":"{\"questions\": [{\"id\": \"approve\", \"question\": \"是否继续"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            ]),
            sse_body(&[
                r#"{"choices":[{"delta":{"content":"已重发 ask"}}]}"#,
                SSE_STOP,
            ]),
        ])
        .await;
    let (core, rt, _ws, _dd) = scripted_core(port, "main-rejected-call");
    let params = DriveParams {
        max_steps: 6,
        emit_events: false,
        // finish_on_text 取默认 true = 主会话语义（缺陷正在此处：被拒回合不得走 Finish）
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_main_rejected").await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "被拒调用回合不得静默收尾（修复前连接数为 1，run 却报成功）"
    );
    assert_eq!(
        result.unwrap(),
        "已重发 ask",
        "final_text 不应停留在被拒回合的正文"
    );
    let texts = history_texts(&rt);
    assert!(
        texts.iter().any(|t| t.contains("<tool-args-rejected>")),
        "应把被拒情况反馈给模型，实际历史：{texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("<continue-notice>")),
        "主会话不应注入续跑提示，实际历史：{texts:?}"
    );
}

/// 同上的错位变体：回合只有 thinking（`content` 非空但 `joined_text()` 为空）+ 调用被拒。
/// 修复前同样走主会话 `Finish`，且 `final_text` 保留空串/上一回合文本当最终答复。
#[tokio::test]
async fn main_session_thinking_only_rejected_call_turn_continues() {
    let (port, hits) = spawn_scripted_sse(vec![
            sse_body(&[
                r#"{"choices":[{"delta":{"reasoning_content":"我在想怎么把方案放进去"}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"ask","arguments":"{\"questions\": [{\"id\": \"approve\", \"question\": \"是否继续"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            ]),
            sse_body(&[
                r#"{"choices":[{"delta":{"content":"已重发 ask"}}]}"#,
                SSE_STOP,
            ]),
        ])
        .await;
    let (core, rt, _ws, _dd) = scripted_core(port, "main-thinking-rejected");
    let params = DriveParams {
        max_steps: 6,
        emit_events: false,
        ..DriveParams::default()
    };
    let (result, _, _) =
        super::drive::drive_agent(&core, &rt, params, "run_main_thinking_rejected").await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "仅思考的被拒回合同样不得静默收尾"
    );
    assert_eq!(result.unwrap(), "已重发 ask");
}

/// 被拒调用与纯文本回合共用 MAX_TEXT_TURNS 上限（非主会话）：连续被拒不无限续跑。
#[tokio::test]
async fn sub_rejected_call_shares_text_turn_budget() {
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"content":"我重发一次"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"edit","arguments":"{\"files\": [{\"path\": \"unfinished"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]);
    let (port, hits) =
        spawn_scripted_sse(vec![body.clone(), body.clone(), body.clone(), body]).await;
    let (core, rt, _ws, _dd) = scripted_core(port, "sub-rejected-budget");
    let params = DriveParams {
        max_steps: 8,
        finish_on_text: false,
        emit_events: false,
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_rejected_budget").await;
    assert!(
        result.is_err(),
        "连续被拒应以显式错误终止，而非成功收尾：{:?}",
        result.ok()
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        4,
        "MAX_TEXT_TURNS(3) 次续跑后第 4 步终止"
    );
    let texts = history_texts(&rt);
    assert!(
        texts.iter().any(|t| t.contains("<text-turn-limit>")),
        "应注入终止引导，实际历史：{texts:?}"
    );
    // 被拒回合只注入 <tool-args-rejected>，不得叠加 <continue-notice>（后者文案
    //「你只输出了文字、没有发起工具调用」对被拒场景不实且与之矛盾）
    assert!(
        !texts.iter().any(|t| t.contains("<continue-notice>")),
        "被拒回合不应注入续跑提示，实际历史：{texts:?}"
    );
}

/// 被拒调用的诊断摘要（取证增强）：长度 + 首尾 + serde 错误原文，且不改变解析决策。
#[test]
fn diagnose_unparsable_reports_length_and_edges() {
    use crate::core::sessions::repair::{diagnose_unparsable, parse_or_salvage};
    // 字符串中部截断（不可打捞）——诊断须给出长度与错误原文
    let raw = "{\"files\": [{\"path\": \"unfinished";
    assert!(parse_or_salvage(raw).is_none());
    let diag = diagnose_unparsable(raw);
    assert!(diag.contains(&format!("len={}", raw.chars().count())));
    assert!(diag.contains("head="), "应给出首部摘要：{diag}");
    assert!(diag.contains("err="), "应给出 serde 错误原文：{diag}");
    // 长参数：首尾都给（短参数只给 head，避免重复）
    let long = format!("{}{}", "a".repeat(500), "{\"unclosed\": \"x");
    let diag_long = diagnose_unparsable(&long);
    assert!(
        diag_long.contains("tail="),
        "长参数应给出尾部摘要：{diag_long}"
    );
    // 换行转义（多行参数在日志里必须单行可读）
    let multiline = "{\n \"a\": \"b";
    let diag_nl = diagnose_unparsable(multiline);
    assert!(
        diag_nl.contains("\\n") && !diag_nl.contains('\n'),
        "真换行须转义为单行文本：{diag_nl}"
    );
}

// ---- 端到端：只读 run 读新文件不得被判空转（[docs/subagent-idle-watchdog-misfire]）----

/// openai_chat 协议的 `read` 工具调用 SSE 响应：args 用真实契约形态
/// `{"files":[{"path":…}]}`（tools/read.rs 的 `Args`）。
fn read_call_body(id: &str, path: &str) -> Vec<u8> {
    let args = serde_json::json!({"files": [{"path": path}]}).to_string();
    let delta = serde_json::json!({
        "choices": [{"delta": {"tool_calls": [{"index": 0, "id": id,
            "function": {"name": "read", "arguments": args}}]}}]
    })
    .to_string();
    let finish =
        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}).to_string();
    sse_body(&[delta.as_str(), finish.as_str()])
}

/// 空转看门狗误杀回归：只读 run 连续多步各读一个**不同的新文件**是实质进展，
/// 不得被判空转终止（真实事故：explore 子代理在第 14 步被硬终止）。
///
/// 修复前 `batch_digest` 把 `read` / `batch_read` 两个分支的入参键写反（read 取顶层
/// `path`，对真实契约恒为 `None`）→ `read_paths` 恒空 → 进展信号 2（首次读新文件）
/// 从未生效 → 第 8 步纠偏、第 14 步被 `IdlePolicy::Stop` 硬终止。本用例修前红、修后绿。
///
/// 两个陷阱（有意规避，勿改）：路径必须真实存在（读取失败会走「失败重复检测」层，
/// 污染空转结论）；每步必须读不同文件（重复读同一文件不构成进展）。
#[tokio::test]
async fn read_only_run_over_new_files_is_not_idle() {
    const FILES: usize = 16;
    let mut script: Vec<Vec<u8>> = (0..FILES)
        .map(|i| read_call_body(&format!("c{i}"), &format!("f{i:02}.txt")))
        .collect();
    script.push(sse_body(&[
        r#"{"choices":[{"delta":{"content":"<report>已读完 16 个文件</report>"}}]}"#,
        SSE_STOP,
    ]));
    let (port, hits) = spawn_scripted_sse(script).await;
    let (core, rt, ws, _dd) = scripted_core(port, "idle-read-new-files");
    for i in 0..FILES {
        std::fs::write(ws.path().join(format!("f{i:02}.txt")), "line1\nline2\n").unwrap();
    }
    let params = DriveParams {
        max_steps: FILES + 4,
        finish_on_text: false,
        emit_events: false,
        // idle_policy 取默认 Stop（8 步纠偏 / 14 步终止）——只读 run 同样受此阈值约束
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_idle_new_files").await;
    assert!(
        result.is_ok(),
        "读新文件属进展，不得被空转看门狗终止：{:?}",
        result.err()
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        FILES + 1,
        "应跑满 {FILES} 步读取 + 1 步汇报（修前第 14 步即被终止）"
    );
    let texts = history_texts(&rt);
    assert!(
        !texts.iter().any(|t| t.contains("supervision-escalated")),
        "不得注入空转终止引导，实际历史：{texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("supervision-notice")),
        "读新文件不应触发任何空转纠偏，实际历史：{texts:?}"
    );
}

// ---- 生成耗时 / TTFT（[docs/composer-token-rate](../../../../docs/composer-token-rate.md)）----

/// AC-1/AC-2：usage 帧的两个可选字段——旧载荷（四个 token 字段）反序列化不报错（None）；
/// 新载荷序列化带两键，键名 `type`/`usage` 不变。
#[test]
fn usage_frame_timing_fields_are_optional() {
    let legacy: Frame = serde_json::from_value(serde_json::json!({
        "type": "usage",
        "input": 10,
        "output": 4,
        "cache_read": 2,
        "cache_write": 1
    }))
    .unwrap();
    match legacy {
        Frame::Usage {
            input,
            output,
            duration_ms,
            ttft_ms,
            ..
        } => {
            assert_eq!((input, output), (10, 4));
            assert_eq!(duration_ms, None, "旧载荷缺字段 → None（不报错）");
            assert_eq!(ttft_ms, None);
        }
        other => panic!("应反序列化为 Usage，实际：{other:?}"),
    }

    let v = serde_json::to_value(Frame::Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        duration_ms: Some(1200),
        ttft_ms: Some(300),
    })
    .unwrap();
    assert_eq!(v["type"], "usage", "事件/帧键名不变");
    assert_eq!(v["input"], 1);
    assert_eq!(v["duration_ms"], 1200);
    assert_eq!(v["ttft_ms"], 300);

    // 无数据：显式 null（前端按「无数据」处理，不累加不显示）
    let v = serde_json::to_value(Frame::Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        duration_ms: None,
        ttft_ms: None,
    })
    .unwrap();
    assert!(v["duration_ms"].is_null() && v["ttft_ms"].is_null());
}

/// Q2 口径：TTFT 取**首个任意类型**增量（thinking 也算），且不等到第二个增量。
#[tokio::test]
async fn collect_deltas_reports_ttft_from_first_delta_including_thinking() {
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::provider::StreamDelta>(4);
    let stream = Arc::new(crate::util::throttle::ThrottledStream::new());
    let anchor = std::time::Instant::now();
    // 收集端必须与发送端并发（否则带缓冲的通道不会唤醒消费者，首增量时刻被推迟到全发完）
    let collecting = tokio::spawn(super::stream::collect_deltas(rx, stream, anchor));
    // 首个增量是 thinking（25ms 后），第二个（正文）125ms 后才到——TTFT 必须落在前者
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    tx.send(crate::provider::StreamDelta::Reasoning {
        text: "先想".into(),
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    tx.send(crate::provider::StreamDelta::Text {
        text: "再答".into(),
    })
    .await
    .unwrap();
    drop(tx);
    let (asm, ttft_ms) = collecting.await.unwrap();
    assert_eq!(asm.joined_text(), "再答");
    let ttft = ttft_ms.expect("有增量就必须报 TTFT");
    assert!(ttft >= 20, "TTFT 应为实测首增量时刻，实际 {ttft}ms");
    assert!(
        ttft < 120,
        "TTFT 取的是首个增量（含思考），不得等到第二个增量：{ttft}ms"
    );
}

/// 全程无增量（空流/仅收尾）→ 无 TTFT 样本（`None`，不进平均）。
#[tokio::test]
async fn collect_deltas_without_deltas_reports_no_ttft() {
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::provider::StreamDelta>(4);
    drop(tx);
    let stream = Arc::new(crate::util::throttle::ThrottledStream::new());
    let (asm, ttft_ms) = super::stream::collect_deltas(rx, stream, std::time::Instant::now()).await;
    assert!(asm.is_empty());
    assert_eq!(ttft_ms, None, "无增量 = 无 TTFT 样本");
}

/// 同 `scripted_core`，但 core.sink 换成可捕获帧的 sink（usage 帧字段断言用）。
#[allow(clippy::type_complexity)]
fn scripted_core_capturing(
    port: u16,
    name: &str,
) -> (
    Arc<AgentCore>,
    Arc<SessionRuntime>,
    Arc<CaptureSink>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let mut cfg = crate::core::config::ConfigState::default();
    cfg.providers.push(crate::core::config::ProviderConfig {
        models: vec![crate::core::config::ProviderModel::default()],
        ..Default::default()
    });
    cfg.active_model_id = Some(cfg.providers[0].models[0].id.clone());
    cfg.providers[0].base_url = format!("http://127.0.0.1:{port}/v1");
    cfg.providers[0].keys = vec!["test-key".into()];
    let sink = Arc::new(CaptureSink::default());
    let store = Arc::new(crate::core::sessions::SessionStore::new(
        roots.data_dir.clone(),
    ));
    let core = Arc::new(AgentCore::new(
        cfg,
        sink.clone() as Arc<dyn EventSink>,
        store,
        reqwest::Client::new(),
        roots.data_dir.clone(),
    ));
    let rt = core.get_or_create_session(name, roots.workspace.clone(), None, vec![], None, vec![]);
    (core, rt, sink, ws, dd)
}

/// 脚本化 SSE mock（带响应前延迟）：进度可测要求服务端真的等一会儿——否则
/// 本机回环上一个 step 可能不到 1ms，`duration_ms` 取整为 0 而整步不计。
async fn spawn_delayed_sse(
    script: Vec<(u64, Vec<u8>)>,
) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        let mut idx = 0usize;
        while let Ok((mut sock, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 8192];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let (delay_ms, resp) = script.get(idx).or_else(|| script.last()).unwrap().clone();
            idx += 1;
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, &resp).await;
            let _ = tokio::io::AsyncWriteExt::flush(&mut sock).await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
    (port, hits)
}

/// AC-1：每个 step 结束的 usage 帧带 `duration_ms`/`ttft_ms`，且 `ttft_ms <= duration_ms`；
/// 本 run 的耗时观测（`run_timing`）与帧一致（steps=1）。
#[tokio::test]
async fn usage_frame_carries_step_timing() {
    let (port, hits) = spawn_delayed_sse(vec![(
        30,
        sse_body(&[r#"{"choices":[{"delta":{"content":"答完了"}}]}"#, SSE_STOP]),
    )])
    .await;
    let (core, rt, sink, _ws, _dd) = scripted_core_capturing(port, "usage-timing");
    let params = DriveParams {
        max_steps: 6,
        emit_events: true,
        ..DriveParams::default()
    };
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_usage_timing").await;
    assert!(result.is_ok(), "run 应成功：{:?}", result.err());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let frames = sink.0.lock().unwrap().clone();
    let usages: Vec<(Option<u64>, Option<u64>)> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Usage {
                duration_ms,
                ttft_ms,
                ..
            } => Some((*duration_ms, *ttft_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(usages.len(), 1, "每个 step 结束一帧 usage");
    let (duration_ms, ttft_ms) = usages[0];
    let duration_ms = duration_ms.expect("成功 step 必须有 duration_ms");
    let ttft_ms = ttft_ms.expect("有增量就必须有 ttft_ms");
    assert!(duration_ms > 0, "服务端延迟 30ms，duration 必须 > 0");
    assert!(
        ttft_ms <= duration_ms,
        "ttft({ttft_ms}) 不得超过 duration({duration_ms})"
    );

    let timing = *rt.run_timing.lock().unwrap();
    assert_eq!(timing.steps, 1);
    assert_eq!(timing.ttft_count, 1);
    assert!(timing.gen_ms > 0);
    assert!(timing.ttft_ms <= timing.gen_ms);
}

/// Q1/AC-1：计时只含**成功那次尝试**——500 → 退避 500ms → 成功，退避不得进生成窗口。
#[tokio::test]
async fn step_timing_excludes_retry_backoff() {
    let server_error =
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
    let ok = sse_body(&[
        r#"{"choices":[{"delta":{"content":"第二次就绪"}}]}"#,
        SSE_STOP,
    ]);
    let (port, hits) = spawn_delayed_sse(vec![(0, server_error), (30, ok)]).await;
    let (core, rt, sink, _ws, _dd) = scripted_core_capturing(port, "usage-backoff");
    let params = DriveParams {
        max_steps: 6,
        emit_events: true,
        ..DriveParams::default()
    };
    let started = std::time::Instant::now();
    let (result, _, _) = super::drive::drive_agent(&core, &rt, params, "run_backoff").await;
    let wall_ms = started.elapsed().as_millis() as u64;
    assert!(result.is_ok(), "重试后应成功：{:?}", result.err());
    assert_eq!(hits.load(Ordering::SeqCst), 2, "第一次 500、第二次成功");

    let timing = *rt.run_timing.lock().unwrap();
    assert_eq!(timing.steps, 1, "失败尝试不计步数");
    assert!(timing.gen_ms > 0);
    assert!(
        wall_ms >= timing.gen_ms + 450,
        "退避的 500ms 不得进生成窗口：wall={wall_ms}ms gen={}ms",
        timing.gen_ms
    );
    // 失败尝试不发 usage 帧（只有成功那次发）
    let usage_frames = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|f| matches!(f, Frame::Usage { .. }))
        .count();
    assert_eq!(usage_frames, 1);
}

// ===== B1：子代理档位实时同步（每步从基座重建，不累积、不残留） =====

/// 测试用子代理基座：内部 8 项（含 `goal`）+ explore（只读角色）纪律块。
fn test_sub_base(spawn_mode: crate::core::prefs::ApprovalMode) -> SubBase {
    test_sub_base_role("explore", spawn_mode)
}

/// 指定角色的测试基座（档位重算与角色策略交叉验证用）。
fn test_sub_base_role(role: &str, spawn_mode: crate::core::prefs::ApprovalMode) -> SubBase {
    crate::tools::subagent::sub_base(role, 25, crate::agents::find(role), spawn_mode)
}

/// 父档 Plan → AutoEdit 后子代理的排除集与提示块按父档重建；来回切不累积。
/// 用**可写角色**（backend-dev）：写工具的唯一来源是父档，故能直接验证档位重算。
#[test]
fn subagent_params_rebuild_from_base_on_mode_switch() {
    use crate::core::prefs::ApprovalMode;
    let base = test_sub_base_role("backend-dev", ApprovalMode::Plan);

    // 父档 Plan：写工具被排除 + 恰一个 <plan-mode> 块
    let plan = subagent_drive_params(&base, &prefs_of(ApprovalMode::Plan, None), None);
    for t in ["edit", "create", "delete"] {
        assert!(
            plan.exclude_tools.iter().any(|e| e == t),
            "Plan 档应排除 {t}：{:?}",
            plan.exclude_tools
        );
    }
    assert_eq!(plan.system_extra.matches("<plan-mode>").count(), 1);
    assert!(plan.system_extra.contains("<subagent-discipline>"));
    assert!(plan.exclude_mcp, "Plan 档排除 MCP");

    // 父档切 AutoEdit：写工具回归 + plan 块消失（从基座重建，旧档提示不残留）
    let auto = subagent_drive_params(&base, &prefs_of(ApprovalMode::AutoEdit, None), None);
    for t in ["edit", "create", "delete"] {
        assert!(
            !auto.exclude_tools.iter().any(|e| e == t),
            "AutoEdit 档不应排除 {t}：{:?}",
            auto.exclude_tools
        );
    }
    assert!(!auto.system_extra.contains("<plan-mode>"));
    assert!(auto.system_extra.contains("<subagent-discipline>"));
    assert!(!auto.exclude_mcp);

    // 来回切：长度稳定（不累积）、plan 块出现次数恰为 0/1、基座 7 项恒在
    let (plan_len, auto_len) = (plan.exclude_tools.len(), auto.exclude_tools.len());
    for mode in [
        ApprovalMode::AutoEdit,
        ApprovalMode::Plan,
        ApprovalMode::AutoEdit,
        ApprovalMode::Plan,
    ] {
        let next = subagent_drive_params(&base, &prefs_of(mode, None), None);
        assert_eq!(
            next.exclude_tools.len(),
            if mode == ApprovalMode::Plan {
                plan_len
            } else {
                auto_len
            },
            "{mode:?} 档排除集长度应稳定（不累积）：{:?}",
            next.exclude_tools
        );
        assert_eq!(
            next.system_extra.matches("<plan-mode>").count(),
            if mode == ApprovalMode::Plan { 1 } else { 0 }
        );
        for t in crate::tools::subagent::SUB_BASE_EXCLUDES {
            assert!(
                next.exclude_tools.iter().any(|e| e == t),
                "缺基座排除项 {t}"
            );
        }
    }
}

/// B1：子 rt 的 prefs 跟随根会话档位（仅 approval_mode；模型 / 力度保持 spawn 快照），
/// 档位真变化时恰好注入一条 `[system]`。
#[tokio::test]
async fn subagent_prefs_follow_parent_mode_without_touching_model() {
    use crate::core::prefs::{ApprovalMode, EffortLevel, SessionPrefs};
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    let root =
        core.get_or_create_session("root", roots.workspace.clone(), None, vec![], None, vec![]);
    // spawn 时的父档：Plan + 指定模型 / 力度（子代理快照）
    root.set_prefs(SessionPrefs {
        approval_mode: ApprovalMode::Plan,
        model_id: Some("m-snapshot".into()),
        reasoning_effort: Some(EffortLevel::Low),
    });
    let sub = SessionRuntime::new_sub(&root, "sub-1".into());
    // 可写角色：写工具只受父档控制，便于断言「档位变了工具集跟着变」
    let mut base = test_sub_base_role("backend-dev", ApprovalMode::Plan);
    base.root_session_id = sub.root_session_id.clone();
    let mut params = subagent_drive_params(&base, &root.prefs(), None);
    params.max_steps = 25;
    params.sub_base = Some(base);
    let before = sub.history.lock().unwrap().len();

    // 档位未变：不注入、不动历史
    super::drive::refresh_subagent_mode(&core, &sub, &mut params);
    assert_eq!(
        sub.history.lock().unwrap().len(),
        before,
        "档位未变不得注入 [system] 消息"
    );

    // 父会话切到 AutoEdit 并换模型：子代理只跟档位
    root.set_prefs(SessionPrefs {
        approval_mode: ApprovalMode::AutoEdit,
        model_id: Some("m-changed".into()),
        reasoning_effort: Some(EffortLevel::High),
    });
    super::drive::refresh_subagent_mode(&core, &sub, &mut params);
    assert_eq!(sub.prefs().approval_mode, ApprovalMode::AutoEdit);
    assert_eq!(
        sub.prefs().model_id.as_deref(),
        Some("m-snapshot"),
        "模型必须保持 spawn 快照"
    );
    assert_eq!(
        sub.prefs().reasoning_effort,
        Some(EffortLevel::Low),
        "力度必须保持 spawn 快照"
    );
    // 参数同步：写工具回归 + plan 块消失
    for t in ["edit", "create", "delete"] {
        assert!(!params.exclude_tools.iter().any(|e| e == t));
    }
    assert!(!params.system_extra.contains("<plan-mode>"));
    {
        let h = sub.history.lock().unwrap();
        assert_eq!(h.len(), before + 1, "档位变化恰好注入一条消息");
        let injected = h
            .last()
            .unwrap()
            .content
            .iter()
            .find_map(|c| match c {
                Content::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            injected.contains("[system] 权限模式已变更为 自动编辑模式"),
            "注入文案不符：{injected}"
        );
    }

    // 再刷一次（档位已同）：不再注入
    super::drive::refresh_subagent_mode(&core, &sub, &mut params);
    assert_eq!(sub.history.lock().unwrap().len(), before + 1);
}

/// B1 降级：父会话查不到（已删除）时保持子 rt 现有档位与参数，不 panic、不注入。
#[tokio::test]
async fn subagent_mode_refresh_degrades_when_parent_missing() {
    use crate::core::prefs::ApprovalMode;
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    let root =
        core.get_or_create_session("gone", roots.workspace.clone(), None, vec![], None, vec![]);
    root.set_prefs(prefs_of(ApprovalMode::Plan, None));
    let sub = SessionRuntime::new_sub(&root, "sub-2".into());
    let mut base = test_sub_base(ApprovalMode::Plan);
    // 指向不存在的会话（父会话已删除）
    base.root_session_id = Some("ghost".into());
    let mut params = subagent_drive_params(&base, &root.prefs(), None);
    params.sub_base = Some(base);
    let before = sub.history.lock().unwrap().len();

    super::drive::refresh_subagent_mode(&core, &sub, &mut params);
    assert_eq!(
        sub.prefs().approval_mode,
        ApprovalMode::Plan,
        "降级：档位保持"
    );
    assert_eq!(
        sub.history.lock().unwrap().len(),
        before,
        "降级不注入 [system]"
    );
    for t in ["edit", "create", "delete"] {
        assert!(
            params.exclude_tools.iter().any(|e| e == t),
            "降级后参数应保持 spawn 时的 Plan 形态"
        );
    }
}

/// B3：父档 FullAccess 时只读子代理解锁写工具（提示同步为授权说明）；其余档仍只读。
#[test]
fn subagent_readonly_role_unlocked_under_full_access() {
    use crate::core::prefs::ApprovalMode;
    let base = test_sub_base(ApprovalMode::FullAccess);
    let p = subagent_drive_params(&base, &prefs_of(ApprovalMode::FullAccess, None), None);
    for t in crate::core::agent::WRITE_TOOLS.iter().copied() {
        assert!(
            !p.exclude_tools.iter().any(|e| e == t),
            "完全访问档不应排除 {t}：{:?}",
            p.exclude_tools
        );
    }
    assert!(p.system_extra.contains("完全访问档"));
    assert!(!p.system_extra.contains("你是只读角色，没有写工具"));
    // 🟡5 返工：只读角色的定义正文自带只读禁令（explore「只读调研：不修改任何文件」），
    // 与授权说明打架 → 完全访问档下正文末尾必须带覆盖声明
    assert!(p.system_extra.contains("上述角色定义中的只读约束"));
    assert_eq!(p.idle_policy, IdlePolicy::NudgeOnly, "idle 策略与档位解耦");

    // 父档 AutoEdit（≠ 基座 spawn 档）：重新生成纪律块 → 回到只读约束
    let q = subagent_drive_params(&base, &prefs_of(ApprovalMode::AutoEdit, None), None);
    for t in crate::core::agent::WRITE_TOOLS.iter().copied() {
        assert!(
            q.exclude_tools.iter().any(|e| e == t),
            "AutoEdit 档应排除 {t}"
        );
    }
    assert!(q.system_extra.contains("你是只读角色，没有写工具"));
    assert!(!q.system_extra.contains("完全访问档"));
    assert!(!q.system_extra.contains("上述角色定义中的只读约束"));
}

/// B4：批准后注入文案按实际档位渲染（AutoEdit 与旧文案逐字一致）。
#[test]
fn approval_mode_labels_cover_all_modes() {
    use crate::core::prefs::ApprovalMode;
    assert_eq!(
        super::drive::approval_mode_label(ApprovalMode::AutoEdit),
        "自动编辑模式"
    );
    assert_eq!(
        super::drive::approval_mode_label(ApprovalMode::FullAccess),
        "完全访问模式"
    );
    assert_eq!(
        super::drive::approval_mode_label(ApprovalMode::Plan),
        "计划模式"
    );
    assert_eq!(
        super::drive::approval_mode_label(ApprovalMode::ConfirmEach),
        "逐项确认模式"
    );
}

/// B5：`<plan-mode>` 块的 S5 描述与批准门（两个批准类选项 + 补充意见）保持一致。
#[test]
fn plan_mode_block_describes_two_approve_options() {
    use crate::core::prefs::ApprovalMode;
    let p = main_drive_params(&prefs_of(ApprovalMode::Plan, None), None);
    assert!(p.system_extra.contains("switchToAutoEdit"), "保留锚点词");
    assert!(p.system_extra.contains("完全访问执行"));
    assert!(p.system_extra.contains("补充意见"));
    assert!(p.system_extra.contains("<plan-mode>"));
    // 🟡3 返工：必须显式要求批准类选项声明 mode（否则模型不声明 → 后端 selected_target_mode
    // 回落 AutoEdit →「完全访问执行」被静默降级）；措辞与 ask 工具 description / core/prompt.rs 的 S5 行对齐。
    assert!(p.system_extra.contains("mode=\"auto_edit\""));
    assert!(p.system_extra.contains("mode=\"full_access\""));
}

// ===== 目标模式（goal mode）驱动层：阶段工具集 / 文本轮语义 =====

/// 测试用目标状态（两条验收标准、一条账本路径 + 一个程序）。
fn goal_state(status: crate::core::agent::goal::GoalStatus) -> crate::core::agent::goal::GoalState {
    use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState};
    GoalState {
        text: "把 X 改成 Y".into(),
        criteria: vec![
            GoalCriterion {
                title: "改完 X".into(),
                done: false,
            },
            GoalCriterion {
                title: "测试通过".into(),
                done: false,
            },
        ],
        ledger: GoalLedger {
            paths: vec![r"D:\Work\Proj\src".into()],
            programs: vec!["cargo".into()],
        },
        status,
        decisions: Vec::new(),
        pending: Vec::new(),
        blocked: Vec::new(),
        rounds: 0,
        stall_streak: 0,
        ledger_denials: 0,
    }
}

/// ① 澄清阶段（未登记 / 已登记未执行）：严格只读——排除写工具与 command/service/scheduled_task，
/// 但 `ask`（提问）与 `goal`（登记）仍在。
#[test]
fn goal_mode_clarify_phase_is_readonly_and_keeps_ask() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    // 未登记（None）与已登记但未进入执行（Clarify）都是澄清阶段
    for goal in [None, Some(goal_state(GoalStatus::Clarify))] {
        let p = main_drive_params(&prefs_of(ApprovalMode::Goal, None), goal.as_ref());
        for t in WRITE_TOOLS
            .iter()
            .copied()
            .chain(["command", "service", "scheduled_task"])
        {
            assert!(
                p.exclude_tools.iter().any(|e| e == t),
                "澄清期应排除 {t}：{:?}",
                p.exclude_tools
            );
        }
        for t in ["ask", "goal", "read", "grep", "subagent"] {
            assert!(
                !p.exclude_tools.iter().any(|e| e == t),
                "澄清期不应排除 {t}：{:?}",
                p.exclude_tools
            );
        }
        assert!(p.system_extra.contains("<goal-mode>"));
        // 澄清期也必须 NudgeOnly：只读调研是澄清期的常态，默认 Stop 会在 14 批空转时误杀
        assert_eq!(p.idle_policy, IdlePolicy::NudgeOnly);
    }
}

/// ② 执行阶段：排除 `ask`（「零提问」的机制保证），写工具与命令全部放开
///（范围控制交给账本）；Paused 与 Executing 同属执行阶段。
#[test]
fn goal_mode_execute_phase_drops_ask_and_unlocks_writes() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    for status in [GoalStatus::Executing, GoalStatus::Paused] {
        let p = main_drive_params(
            &prefs_of(ApprovalMode::Goal, None),
            Some(&goal_state(status)),
        );
        assert!(
            p.exclude_tools.iter().any(|e| e == "ask"),
            "执行期必须排除 ask：{:?}",
            p.exclude_tools
        );
        for t in WRITE_TOOLS.iter().copied().chain(["command", "service"]) {
            assert!(
                !p.exclude_tools.iter().any(|e| e == t),
                "执行期不应排除 {t}：{:?}",
                p.exclude_tools
            );
        }
        assert!(
            !p.exclude_tools.iter().any(|e| e == "goal"),
            "执行期保留 goal（勾选验收标准）"
        );
        assert_eq!(p.idle_policy, IdlePolicy::NudgeOnly);
        assert!(p.system_extra.contains("<goal-mode>"));
    }
    // 非目标档不受目标状态影响：既不排除 ask，也不注入目标块
    let a = main_drive_params(
        &prefs_of(ApprovalMode::AutoEdit, None),
        Some(&goal_state(GoalStatus::Executing)),
    );
    assert!(!a.exclude_tools.iter().any(|e| e == "ask"));
    assert!(!a.system_extra.contains("<goal-mode>"));
}

/// ③ 目标档执行期的纯文本回合 = 推进暂停（**不是**收尾）；
/// 非目标档行为逐字不变（主会话纯文本即收尾 / 子代理 3 轮硬终止）。
#[test]
fn goal_execute_text_turn_continues_instead_of_finishing() {
    use super::drive::{MAX_TEXT_TURNS, TextTurnAction, text_turn_action};
    use crate::core::agent::goal::GOAL_TEXT_TURN_LIMIT;
    // 主会话的 finish_on_text 恒为 true，故这一组直接钉死「目标档分支优先于主会话语义」
    assert_eq!(
        text_turn_action("我先说明一下思路", true, false, 0, true),
        TextTurnAction::Continue
    );
    assert_eq!(
        text_turn_action(
            "我先说明一下思路",
            true,
            false,
            GOAL_TEXT_TURN_LIMIT - 1,
            true
        ),
        TextTurnAction::Continue
    );
    // 达上限先提醒一轮，下一轮仍纯文本才收尾
    assert_eq!(
        text_turn_action("我先说明一下思路", true, false, GOAL_TEXT_TURN_LIMIT, true),
        TextTurnAction::ContinueWithReminder
    );
    assert_eq!(
        text_turn_action(
            "我先说明一下思路",
            true,
            false,
            GOAL_TEXT_TURN_LIMIT + 1,
            true
        ),
        TextTurnAction::Finish
    );
    // 显式汇报标记 → 收尾（与子代理同一标记语义）
    assert_eq!(
        text_turn_action("<report>做完了</report>", true, false, 0, true),
        TextTurnAction::Finish
    );
    // 被拒调用仍优先于目标档分支，且上限与纯文本同源（同一个计数器，上限必须同源）
    assert_eq!(
        text_turn_action("方案如下", true, true, GOAL_TEXT_TURN_LIMIT - 1, true),
        TextTurnAction::Continue
    );
    assert_eq!(
        text_turn_action("方案如下", true, true, GOAL_TEXT_TURN_LIMIT, true),
        TextTurnAction::StopWithLimit
    );
    // 非目标档：逐字不变
    assert_eq!(
        text_turn_action("答完了", true, false, 0, false),
        TextTurnAction::Finish
    );
    assert_eq!(
        text_turn_action("旁白", false, false, 0, false),
        TextTurnAction::Continue
    );
    assert_eq!(
        text_turn_action("旁白", false, false, MAX_TEXT_TURNS, false),
        TextTurnAction::StopWithLimit
    );
}

// ===== 目标模式驱动层：停滞 / 收尾 / 回落 / 瞬态 / 子代理 =====

/// 测试用 core + 主会话 rt（目标模式用例共用；data_dir 与 workspace 各自独立临时目录）。
fn goal_core_and_rt() -> (Arc<AgentCore>, Arc<SessionRuntime>) {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = test_support::make_core(&roots);
    let rt = test_support::make_runtime(roots.workspace.clone(), roots.data_dir.clone(), vec![]);
    (core, rt)
}

fn noop_sink() -> Arc<dyn EventSink> {
    Arc::new(test_support::NoopSink)
}

/// ④ 停滞两段式：连续 5 步无实质进展 → 提醒（纠偏文案进历史）；连续 10 步 → 自停；
/// 有非只读工具调用的那一步清零。
#[tokio::test]
async fn goal_stall_nudge_at_five_and_stop_at_ten() {
    use crate::core::agent::goal::{GoalStatus, STALL_NUDGE_AT, STALL_STOP_AT, StallVerdict};
    let (_core, rt) = goal_core_and_rt();
    rt.set_goal(Some(goal_state(GoalStatus::Executing)));
    let sink = noop_sink();
    // 预置基线快照：首次快照视为进展（不能把「刚登记目标」那一步算成停滞），
    // 故这里手动置位，让计数从 0 起算
    let mut key = Some(super::drive::goal_progress_key(
        &rt.goal_snapshot().unwrap(),
    ));
    let step = |key: &mut Option<super::drive::GoalProgressKey>, progressed: bool| {
        super::drive::goal_account_step(&sink, &rt, progressed, key)
    };
    for i in 0..STALL_NUDGE_AT - 1 {
        assert_eq!(
            step(&mut key, false),
            StallVerdict::Continue,
            "第 {} 步",
            i + 1
        );
    }
    let before = rt.history.lock().unwrap().len();
    assert_eq!(step(&mut key, false), StallVerdict::Nudge);
    assert_eq!(rt.goal_snapshot().unwrap().stall_streak, STALL_NUDGE_AT);
    {
        let h = rt.history.lock().unwrap();
        assert_eq!(h.len(), before + 1, "Nudge 必须注入一条纠偏消息");
        assert!(
            h.last()
                .unwrap()
                .first_text()
                .unwrap()
                .contains("<goal-stall-notice>"),
            "纠偏文案形态不符"
        );
    }
    for _ in 0..STALL_STOP_AT - STALL_NUDGE_AT - 1 {
        assert_eq!(step(&mut key, false), StallVerdict::Nudge);
    }
    assert_eq!(step(&mut key, false), StallVerdict::Stop);
    assert_eq!(rt.goal_snapshot().unwrap().stall_streak, STALL_STOP_AT);
    // 非只读工具调用 = 进展 → 清零
    assert_eq!(step(&mut key, true), StallVerdict::Continue);
    assert_eq!(rt.goal_snapshot().unwrap().stall_streak, 0);
}

/// ④’ 停滞与账本漂移只在**执行阶段**裁决：澄清期（含未登记）不设自停门。
#[tokio::test]
async fn goal_bookkeep_stall_gate_only_in_execute_phase() {
    use crate::core::agent::goal::{GoalStatus, STALL_STOP_AT};
    use crate::core::agent::supervise::BatchDigest;
    let (core, rt) = goal_core_and_rt();
    let sink = noop_sink();
    let idle = BatchDigest::default();
    // 澄清期：连续 12 批零进展也不得收尾（自停门只在执行期）
    rt.set_goal(Some(goal_state(GoalStatus::Clarify)));
    let mut key = None;
    for i in 0..12 {
        let out = super::drive::goal_bookkeep(&core, &rt, &sink, &idle, &mut key).await;
        assert!(out.is_none(), "澄清期第 {} 批不得收尾：{out:?}", i + 1);
    }
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Clarify);
    assert_eq!(rt.goal_snapshot().unwrap().stall_streak, 0);
    // 执行期：同样零进展，第 10 步自停并落 Paused
    rt.set_goal(Some(goal_state(GoalStatus::Executing)));
    let mut key = Some(super::drive::goal_progress_key(
        &rt.goal_snapshot().unwrap(),
    ));
    let mut closed = None;
    for _ in 0..STALL_STOP_AT {
        closed = super::drive::goal_bookkeep(&core, &rt, &sink, &idle, &mut key).await;
        if closed.is_some() {
            break;
        }
    }
    let report = closed.expect("执行期连续 10 步无进展必须自停");
    assert!(report.contains("连续无实质进展"), "{report}");
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Paused);
}

/// ⑤ 用户取消 run：目标从「执行中」落「已暂停」并落边车（复用取消链路，
/// 不新增事件键、`<run-cancelled/>` 语义不变）；非 Executing 的目标不被改写。
#[tokio::test]
async fn cancel_pauses_executing_goal() {
    use crate::core::agent::goal::GoalStatus;
    let (core, rt) = goal_core_and_rt();
    rt.set_goal(Some(goal_state(GoalStatus::Executing)));
    super::drive::mark_cancelled(&core, &rt, "run_cancel").await;
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Paused);
    assert_eq!(
        core.store.load_goal(&rt.id).unwrap().status,
        GoalStatus::Paused,
        "暂停必须落边车（恢复会话后仍可回看）"
    );
    assert!(
        rt.history
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.first_text() == Some("<run-cancelled/>")),
        "取消标记语义不变"
    );
    // Done 存档不被改写（用户回看得到真实结局）
    rt.set_goal(Some(goal_state(GoalStatus::Done)));
    super::drive::mark_cancelled(&core, &rt, "run_cancel2").await;
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Done);
}

/// ⑤’ 档位兜底（主会话每步）：档位已不是目标档但目标仍停在「执行中」→ 置「已暂停」
/// 并落边车（与 `transition_prefs` 同语义，覆盖不经 `set_session_prefs` 的档位变动）；
/// 档位仍是目标档 / 目标不在执行中 / 未登记目标 → 不动。
#[tokio::test]
async fn mode_left_goal_pauses_executing_goal() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    let (core, rt) = goal_core_and_rt();
    // 档位切走（如 ask 批准时用户选了非目标档）+ 目标执行中 → 置 Paused
    rt.set_prefs(prefs_of(ApprovalMode::AutoEdit, None));
    rt.set_goal(Some(goal_state(GoalStatus::Executing)));
    assert!(
        super::drive::pause_goal_if_mode_left(&core, &rt),
        "应发生迁移"
    );
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Paused);
    assert_eq!(
        core.store.load_goal(&rt.id).unwrap().status,
        GoalStatus::Paused,
        "兜底迁移必须落边车"
    );
    // 幂等：再调一次不迁移（已不是执行中）
    assert!(!super::drive::pause_goal_if_mode_left(&core, &rt));
    // 档位仍是目标档 → 不动（执行中保持执行中）
    rt.set_prefs(prefs_of(ApprovalMode::Goal, None));
    rt.set_goal(Some(goal_state(GoalStatus::Executing)));
    assert!(!super::drive::pause_goal_if_mode_left(&core, &rt));
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Executing);
    // 非执行中（澄清期）→ 不动
    rt.set_prefs(prefs_of(ApprovalMode::AutoEdit, None));
    rt.set_goal(Some(goal_state(GoalStatus::Clarify)));
    assert!(!super::drive::pause_goal_if_mode_left(&core, &rt));
    assert_eq!(rt.goal_snapshot().unwrap().status, GoalStatus::Clarify);
    // 未登记目标 → 不动、不凭空登记
    rt.set_goal(None);
    assert!(!super::drive::pause_goal_if_mode_left(&core, &rt));
    assert!(rt.goal_snapshot().is_none());
}

/// ⑤’’ 取消主会话 run / 目标收尾必须连带取消**本会话**在跑的子代理（按父会话过滤，
/// 绝不误杀其它会话的子代理——`core.subs` 是全进程注册表）。
#[tokio::test]
async fn cancel_and_goal_close_out_stop_only_this_sessions_subagents() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    use tokio_util::sync::CancellationToken;
    let (core, rt) = goal_core_and_rt();
    // 本会话的子代理（new_sub 把 root_session_id 置为父 id）
    let mine = SessionRuntime::new_sub(&rt, "sub-mine".into());
    // 另一会话的子代理：父 id 不同 → 不得被本会话的收尾误杀
    let other_parent = SessionRuntime::new(
        "other-session".into(),
        rt.workspace.clone(),
        rt.data_dir.clone(),
    );
    let theirs = SessionRuntime::new_sub(&other_parent, "sub-theirs".into());
    let (mine_tok, theirs_tok) = (CancellationToken::new(), CancellationToken::new());
    *mine.active_cancel.lock().unwrap() = Some(mine_tok.clone());
    *theirs.active_cancel.lock().unwrap() = Some(theirs_tok.clone());
    core.subs.insert("sub-mine".into(), mine.clone());
    core.subs.insert("sub-theirs".into(), theirs.clone());

    // ① 用户取消主 run → 本会话子代理被取消，其它会话不受影响
    super::drive::mark_cancelled(&core, &rt, "run_cancel_sub").await;
    assert!(mine_tok.is_cancelled(), "本会话子代理必须被取消");
    assert!(!theirs_tok.is_cancelled(), "其它会话的子代理不得被误杀");

    // ② 目标收尾（达成 / 硬停 / 账本漂移 / 停滞 / 文本轮超限五路共用）同样取消本会话子代理
    rt.set_prefs(prefs_of(ApprovalMode::Goal, None));
    rt.set_goal(Some(goal_state(GoalStatus::Executing)));
    let closing = SessionRuntime::new_sub(&rt, "sub-close".into());
    let closing_tok = CancellationToken::new();
    *closing.active_cancel.lock().unwrap() = Some(closing_tok.clone());
    core.subs.insert("sub-close".into(), closing.clone());
    super::drive::goal_close_out(&core, &rt, super::drive::GoalCloseCause::Done).await;
    assert!(closing_tok.is_cancelled(), "目标收尾必须取消残留子代理");
    assert!(!theirs_tok.is_cancelled(), "目标收尾同样不得误杀其它会话");
}

/// ⑥ 达成收尾后回落进入目标档前的档位；快照缺失（None）时回落全局默认
///（`approval.enabled` → Plan / FullAccess）。
#[tokio::test]
async fn goal_done_falls_back_to_previous_mode() {
    use crate::core::prefs::ApprovalMode;
    // 纯函数面
    let cfg = crate::core::config::ConfigState::default();
    assert_eq!(
        super::drive::fallback_mode(Some(ApprovalMode::AutoEdit), &cfg),
        ApprovalMode::AutoEdit
    );
    assert_eq!(
        super::drive::fallback_mode(None, &cfg),
        ApprovalMode::from_global(cfg.approval.enabled)
    );
    assert_eq!(super::drive::fallback_mode(None, &cfg), ApprovalMode::Plan);

    // 端到端：Done 收尾 → prefs 回落 + 快照清空
    let (core, rt) = goal_core_and_rt();
    rt.set_prefs(prefs_of(ApprovalMode::Goal, None));
    rt.set_goal_prev_mode(Some(ApprovalMode::ConfirmEach));
    rt.set_goal(Some(done_goal()));
    let report = super::drive::goal_close_out(&core, &rt, super::drive::GoalCloseCause::Done).await;
    assert!(report.contains("目标已达成"), "{report}");
    assert_eq!(rt.prefs().approval_mode, ApprovalMode::ConfirmEach);
    assert_eq!(
        rt.goal_prev_mode(),
        None,
        "回落快照必须清空（否则下次进目标档会误用旧档）"
    );
    // 无快照：回落全局默认（默认 approval.enabled = true → Plan）
    rt.set_prefs(prefs_of(ApprovalMode::Goal, None));
    rt.set_goal(Some(done_goal()));
    super::drive::goal_close_out(&core, &rt, super::drive::GoalCloseCause::Done).await;
    assert_eq!(rt.prefs().approval_mode, ApprovalMode::Plan);
}

/// 全部验收标准已勾选、状态 Done 的目标（达成收尾用例用）。
fn done_goal() -> crate::core::agent::goal::GoalState {
    let mut g = goal_state(crate::core::agent::goal::GoalStatus::Done);
    for c in g.criteria.iter_mut() {
        c.done = true;
    }
    g
}

/// ⑦ 子代理：基座排除集含 `goal`（不得替父会话改验收合同），目标档下拿到的是**只读**目标上下文
///（`<goal-context read-only="true">`），而非父会话的 `<goal-mode>` 块（后者含子代理做不到的指令）。
#[test]
fn subagent_excludes_goal_and_gets_readonly_goal_context() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    assert!(
        crate::tools::subagent::SUB_BASE_EXCLUDES.contains(&"goal"),
        "子代理基座必须排除 goal"
    );
    let base = test_sub_base_role("backend-dev", ApprovalMode::Goal);
    let p = subagent_drive_params(
        &base,
        &prefs_of(ApprovalMode::Goal, None),
        Some(&goal_state(GoalStatus::Executing)),
    );
    assert!(
        p.exclude_tools.iter().any(|e| e == "goal"),
        "子代理不得持有 goal 工具：{:?}",
        p.exclude_tools
    );
    assert!(p.exclude_tools.iter().any(|e| e == "ask"));
    assert!(
        p.system_extra.contains("<goal-context read-only=\"true\">"),
        "目标档子代理必须拿到只读目标上下文"
    );
    assert!(
        !p.system_extra.contains("<goal-mode>"),
        "父档 <goal-mode> 块必须被替换而非叠加：{}",
        p.system_extra
    );
    assert!(p.system_extra.contains("把 X 改成 Y"), "目标正文未下发");
    // 未登记目标：同样只读上下文（澄清期文案）
    let q = subagent_drive_params(&base, &prefs_of(ApprovalMode::Goal, None), None);
    assert!(q.system_extra.contains("<goal-context read-only=\"true\">"));
    assert!(!q.system_extra.contains("<goal-mode>"));
}

/// ⑧ 推进指令是**瞬态**：只进本次请求的出网副本（作为末尾一条用户消息），
/// 绝不写入持久化历史；重试重建请求时该尾部瞬态被保留。
#[tokio::test]
async fn goal_advance_transient_never_reaches_history() {
    use crate::core::agent::goal::GoalStatus;
    let (core, rt) = goal_core_and_rt();
    rt.history
        .lock()
        .unwrap()
        .push(Message::user_text("原始提问").stamped());
    let advance = super::drive::render_goal_advance(&goal_state(GoalStatus::Executing), false);
    assert!(advance.starts_with("<goal-advance"), "{advance}");
    assert!(advance.contains("未达成的验收标准"), "{advance}");
    assert!(advance.contains("不得向用户提问"), "{advance}");
    let before = rt.history.lock().unwrap().clone();

    // 无瞬态：出网副本不含推进指令
    let plain = super::stream::messages_for_request(&rt, false, None);
    assert!(!plain.iter().any(|m| {
        m.first_text()
            .map(|t| t.starts_with("<goal-advance"))
            .unwrap_or(false)
    }));
    // 有瞬态：作为末尾用户消息附上，历史一字不改
    let out = super::stream::messages_for_request(&rt, false, Some(&advance));
    assert_eq!(out.last().unwrap().first_text(), Some(advance.as_str()));
    assert!(matches!(out.last().unwrap().role, Role::User));
    assert_eq!(
        rt.history.lock().unwrap().as_slice(),
        before.as_slice(),
        "瞬态绝不写入 rt.history"
    );

    // 端到端：请求组装（DriveParams.goal_transient）→ 重试重建仍保留尾部瞬态
    let params = DriveParams {
        main_session: true,
        goal_transient: Some(advance.clone()),
        ..DriveParams::default()
    };
    let (_model, mut req) = super::stream::build_stream_request(&core, &rt, &params)
        .await
        .expect("测试 core 已配默认模型");
    assert_eq!(
        req.messages.last().unwrap().first_text(),
        Some(advance.as_str())
    );
    super::stream::refresh_request_messages(&rt, &mut req, false);
    assert_eq!(
        req.messages.last().unwrap().first_text(),
        Some(advance.as_str()),
        "重试重建必须保留尾部瞬态（否则模型在重试那一轮失去推进指令）"
    );
    assert_eq!(
        rt.history.lock().unwrap().as_slice(),
        before.as_slice(),
        "组装与重建均不得写历史"
    );
}
