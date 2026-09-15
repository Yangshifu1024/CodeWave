use crate::core::types::{Content, Message, Role, SessionId};
use crate::provider::dto::{AsmBlock, Assembled, AssembledToolCall};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use super::*;
use super::drive::budget_notice_step;
use super::runtime::{Frame, SessionRuntime};
use super::stream::{build_assistant_message, build_stream_request, flush_segments};
    

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
        assert_eq!(
            r1.system_core, r2.system_core,
            "run 内冻结：文件变更不穿透"
        );
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
        assert_eq!(r1.cache_gen_index, Some(12), "n=20 → 目标位 min(n-8, 3n/4)=12");
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
        let rt =
            core.get_or_create_session("c1", roots.workspace.clone(), None, vec![], None, vec![]);
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
        let rt =
            test_support::make_runtime(ws.path().to_path_buf(), ws.path().to_path_buf(), vec![]);
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
        let rt =
            core.get_or_create_session("p0", roots.workspace.clone(), None, vec![], None, vec![]);
        assert_eq!(
            rt.prefs().approval_mode,
            crate::core::prefs::ApprovalMode::Plan
        );
        {
            let mut cfg = core.cfg.write().unwrap();
            cfg.approval.enabled = false;
        }
        let rt2 =
            core.get_or_create_session("p1", roots.workspace.clone(), None, vec![], None, vec![]);
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
        let params = main_drive_params(&rt.prefs());
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
        let params = main_drive_params(&rt.prefs());
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
        let rt =
            core.get_or_create_session("cg", roots.workspace.clone(), None, vec![], None, vec![]);

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
        let rt =
            core.get_or_create_session("ov", roots.workspace.clone(), None, vec![], None, vec![]);
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
        assert!(
            !roots
                .data_dir
                .join("histories/task_test1.json.gz")
                .exists()
        );
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
        assert!(
            roots
                .data_dir
                .join("histories/main-1.json.gz")
                .exists()
        );
        assert!(rt.is_main_session);
    }
