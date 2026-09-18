use super::*;
    use crate::core::types::{Content, Role};

    fn meta(id: &str) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            title: format!("t-{id}"),
            workspace: ".".into(),
            model_id: None,
            created_at: now(),
            updated_at: now(),
            message_count: 0,
            project_id: None,
            roots: vec!["/ws".into()],
            running: false,
            interrupted: None,
        }
    }

    /// M8：并发 upsert 不得互相覆盖丢条目。
    #[test]
    fn concurrent_upsert_keeps_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(SessionStore::new(dir.path().to_path_buf()));
        std::thread::scope(|s| {
            for i in 0..8 {
                let st = store.clone();
                let id = format!("s-{i}");
                s.spawn(move || st.upsert_meta(meta(&id)).unwrap());
            }
        });
        assert_eq!(store.load_index().sessions.len(), 8);
    }

    /// [docs/subagent-interaction-drawer](../../../../../docs/subagent-interaction-drawer.md)：子代理过程历史存取往返；不进索引；缺失返回空；
    /// 删除父会话级联清理。
    #[test]
    fn sub_history_round_trip_not_indexed_and_cascade_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let msgs = vec![
            Message::user_text("<subagent-task role=\"explore\">\n调研\n</subagent-task>"),
            Message {
                role: Role::Assistant,
                content: vec![Content::Text {
                    text: "结论".into(),
                }],
                created_at: None,
            },
        ];
        store.save_sub_history("parent-1", "sub_abcd1234", &msgs).unwrap();

        // 往返：内容一致
        let loaded = store.load_sub_history("parent-1", "sub_abcd1234").unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(matches!(&loaded[1].content[0], Content::Text { text } if text == "结论"));

        // 不进会话索引（子代理不是会话）
        assert!(store.load_index().sessions.iter().all(|m| m.id != "sub_abcd1234"));

        // 文件缺失返回空（旧会话降级）
        assert!(store.load_sub_history("parent-1", "sub_missing").unwrap().is_empty());

        // 删除父会话 → 子历史目录级联清理
        store.remove("parent-1").unwrap();
        assert!(store.load_sub_history("parent-1", "sub_abcd1234").unwrap().is_empty());
    }

    /// 图片保留（未超上限随历史保存；重开后附件仍显示）与超上限降级
    /// （剥图但仍持久化）。
    #[test]
    fn save_history_image_kept_under_cap_stripped_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());

        // 未超上限：图片 payload 保留在历史中
        let small = vec![
            Message::user_text("q"),
            Message {
                role: Role::User,
                content: vec![Content::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                }],
                created_at: None,
            },
        ];
        store
            .save_history("s-img", "t", ".", None, None, &["/ws".into()], &small)
            .unwrap();
        let loaded = store.load_history("s-img").unwrap();
        assert!(matches!(&loaded[1].content[0], Content::Image { data, .. } if data == "AAAA"));

        // 超上限：16MB 不可压缩 base64（LCG 伪随机）gzip 后必超 8MB
        // → 降级剥图；保存不失败
        let data = incompressible_b64(16 * 1024 * 1024);
        let big = vec![
            Message::user_text("q"),
            Message {
                role: Role::User,
                content: vec![Content::Image {
                    media_type: "image/png".into(),
                    data,
                }],
                created_at: None,
            },
        ];
        store
            .save_history("s-big", "t", ".", None, None, &["/ws".into()], &big)
            .unwrap();
        let loaded = store.load_history("s-big").unwrap();
        assert!(
            matches!(&loaded[1].content[0], Content::Text { text } if text.contains("omitted"))
        );
    }

    /// 不可压缩的 base64 载荷（LCG 伪随机）：字符取自 64 符号表，熵约 6 bit/char，
    /// gzip 后 ≈0.75 byte/char——16MiB 字符即 ≈12MiB gz，稳定越过 8MB 上限。
    fn incompressible_b64(len: usize) -> String {
        const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut data = String::with_capacity(len);
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..len {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            data.push(B64[((x >> 33) % 64) as usize] as char);
        }
        data
    }

    /// 一条带思考块的 assistant 消息（思考在前、正文在后，模拟真实流式落块顺序）。
    fn thinking_assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![
                Content::Thinking { text: text.into() },
                Content::Text {
                    text: "答案".into(),
                },
            ],
            created_at: None,
        }
    }

    /// [docs/reasoning-content-passthrough](../../../../../docs/reasoning-content-passthrough.md)：
    /// 8MB 上限回退路径只剥图、**绝不丢思考**。丢思考会让该会话重启后无 `reasoning_content`
    /// 可回传，OpenAI 兼容 thinking 上游多轮必然 400 且不可自愈；而剥图已足够减负。
    /// 这是 store 层对 `repair::sanitize_keep_thinking` 的端到端守护（保存 → 读回）。
    #[test]
    fn save_history_over_cap_fallback_keeps_thinking() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let msgs = vec![
            Message::user_text("q"),
            Message {
                role: Role::User,
                content: vec![Content::Image {
                    media_type: "image/png".into(),
                    data: incompressible_b64(16 * 1024 * 1024),
                }],
                created_at: None,
            },
            thinking_assistant("必须留存的推理"),
            Message::user_text("再问"),
        ];
        store
            .save_history("s-over-cap", "t", ".", None, None, &["/ws".into()], &msgs)
            .unwrap();

        let loaded = store.load_history("s-over-cap").unwrap();
        // 回退路径生效：图片 payload 变占位文本
        assert!(
            loaded
                .iter()
                .flat_map(|m| m.content.iter())
                .any(|c| matches!(c, Content::Text { text } if text.contains("omitted"))),
            "超上限历史必须剥图落盘"
        );
        // 思考必须留存（回退路径丢思考 = 重启后无法回传 reasoning_content）
        let thinking: Vec<String> = loaded
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|c| match c {
                Content::Thinking { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            thinking,
            vec!["必须留存的推理".to_string()],
            "8MB 回退路径丢了思考 → 重启后无法回传 reasoning_content，多轮必然 400"
        );
        // 正文仍在（不是把整条 assistant 消息丢掉）
        assert!(
            loaded
                .iter()
                .flat_map(|m| m.content.iter())
                .any(|c| matches!(c, Content::Text { text } if text == "答案"))
        );
    }

    /// [docs/reasoning-content-passthrough](../../../../../docs/reasoning-content-passthrough.md)：
    /// 回退分支的兜底——剥图（`sanitize_keep_thinking`）后**仍**超 `MAX_HISTORY_BYTES`
    /// 时必须拒绝保存并点名 8MB 上限，且不得留下半成品历史。该分支此前零覆盖；
    /// 「落盘保思考」令压缩后体积变大，触发概率理论上上升，因此钉死其行为。
    #[test]
    fn save_history_still_over_cap_after_stripping_images_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        // 全程不含图片：回退分支的剥图无从减负，剥离前后体积相同（仍 >8MB）。
        // 单条 user 消息 = 仅 1 个用户轮（≤ keep_last=2），`repair::trim` 在轮边界检查处
        // 直接 early-return（repair.rs:228），这段巨量文本因此不会被裁掉。
        let msgs = vec![Message::user_text(incompressible_b64(16 * 1024 * 1024))];
        let id = "s-over-cap-text";
        let err = store
            .save_history(id, "t", ".", None, None, &["/ws".into()], &msgs)
            .expect_err("剥图后仍超 8MB 必须拒绝保存");
        assert!(
            err.to_string().contains("8MB"),
            "错误必须点名 8MB 上限，实际：{err}"
        );
        // 拒绝发生在落盘之前：不产生半成品历史文件，也不写入索引条目
        assert!(
            !dir.path().join("histories").join(format!("{id}.json.gz")).exists(),
            "保存失败不得留下半成品历史文件"
        );
        assert!(store.get(id).is_none(), "保存失败不得写入索引条目");
    }

    /// M8：损坏索引先备份保全证据，绝不静默覆写。
    #[test]
    fn corrupt_index_backed_up_before_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(dir.path().join("sessions/index.json"), b"not json at all").unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store.upsert_meta(meta("a")).unwrap();
        assert!(dir.path().join("sessions/index.json.corrupt").exists());
        assert_eq!(store.load_index().sessions.len(), 1);
    }

    #[test]
    fn index_lru_and_orphan_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        for i in 0..(MAX_INDEX_ENTRIES + 5) {
            store
                .upsert_meta(SessionMeta {
                    id: format!("s{i}"),
                    title: format!("t{i}"),
                    workspace: ".".into(),
                    model_id: None,
                    created_at: now(),
                    updated_at: now(),
                    message_count: 0,
                    project_id: None,
                    roots: vec!["/ws".into()],
                    running: false,
                    interrupted: None,
                })
                .unwrap();
        }
        assert_eq!(store.list().len(), MAX_INDEX_ENTRIES);
        // 造一个孤儿 gz
        std::fs::create_dir_all(dir.path().join("histories")).unwrap();
        std::fs::write(dir.path().join("histories/orphan-1.json.gz"), b"x").unwrap();
        assert_eq!(store.discover_orphans(), vec!["orphan-1".to_string()]);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let msgs = vec![
            Message::user_text("q"),
            Message {
                role: crate::core::types::Role::Assistant,
                created_at: None,
                content: vec![
                    Content::Text { text: "a".into() },
                    Content::ToolUse {
                        id: "t".into(),
                        name: "read".into(),
                        args: serde_json::json!({"files":[]}),
                    },
                ],
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t".into(),
                content: "r".into(),
                is_error: false,
            }]),
        ];
        store
            .save_history("s1", "标题", "/ws", None, None, &["/ws".into()], &msgs)
            .unwrap();
        let loaded = store.load_history("s1").unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[1].tool_uses().len(), 1);
    }

    #[test]
    fn corrupted_gz_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("histories")).unwrap();
        std::fs::write(dir.path().join("histories/bad.json.gz"), b"not gzip at all").unwrap();
        assert!(store.load_history("bad").is_err());
        // 其他会话不受影响
        store
            .save_history(
                "ok",
                "t",
                "/w",
                None,
                None,
                &["/w".into()],
                &[Message::user_text("x")],
            )
            .unwrap();
        assert!(store.load_history("ok").is_ok());
    }

    /// [docs/session-artifacts-and-files-tab](../../../../../docs/session-artifacts-and-files-tab.md)：同路径去重合并（first/last/count 语义）；不同路径各自成条。
    #[test]
    fn artifact_append_dedups_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store
            .append_artifact("s1", "/ws/a.md", ArtifactOp::Create)
            .unwrap();
        store
            .append_artifact("s1", "/ws/a.md", ArtifactOp::Edit)
            .unwrap();
        store
            .append_artifact("s1", "/ws/b.png", ArtifactOp::Create)
            .unwrap();
        let items = store.load_artifacts("s1");
        assert_eq!(items.len(), 2);
        let a = items.iter().find(|a| a.path == "/ws/a.md").unwrap();
        assert_eq!(a.first_op, ArtifactOp::Create);
        assert_eq!(a.last_op, ArtifactOp::Edit);
        assert_eq!(a.count, 2);
        let b = items.iter().find(|a| a.path == "/ws/b.png").unwrap();
        assert_eq!(b.first_op, ArtifactOp::Create);
        assert_eq!(b.last_op, ArtifactOp::Create);
        assert_eq!(b.count, 1);
        // 会话隔离
        assert!(store.load_artifacts("s2").is_empty());
    }

    /// [docs/session-artifacts-and-files-tab](../../../../../docs/session-artifacts-and-files-tab.md)：remove 级联产物边车，也顺带删除曾泄漏的 todos 边车。
    #[test]
    fn remove_cascades_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store
            .save_history(
                "s1",
                "t",
                "/w",
                None,
                None,
                &["/w".into()],
                &[Message::user_text("x")],
            )
            .unwrap();
        store
            .append_artifact("s1", "/ws/a.md", ArtifactOp::Create)
            .unwrap();
        store.save_todos("s1", &[]).unwrap();
        assert!(dir.path().join("sessions/s1.artifacts.json").exists());
        assert!(dir.path().join("sessions/s1.todos.json").exists());
        store.remove("s1").unwrap();
        assert!(!dir.path().join("sessions/s1.artifacts.json").exists());
        assert!(!dir.path().join("sessions/s1.todos.json").exists());
        assert!(store.load_artifacts("s1").is_empty());
    }

    /// [docs/session-artifacts-and-files-tab](../../../../../docs/session-artifacts-and-files-tab.md)：损坏边车静默回空（登记表只是辅助视图，绝不阻断会话）。
    #[test]
    fn corrupted_artifacts_sidecar_isolated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(dir.path().join("sessions/bad.artifacts.json"), b"not json").unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        assert!(store.load_artifacts("bad").is_empty());
        // append 之后自愈为合法列表
        store
            .append_artifact("bad", "/ws/a.md", ArtifactOp::Create)
            .unwrap();
        assert_eq!(store.load_artifacts("bad").len(), 1);
    }

    /// 批1：`running` 标记落盘后不被检查点（upsert_meta）抹掉；新会话首次 upsert 会带上内存里的 running。
    #[test]
    fn mark_running_survives_checkpoint_and_covers_unindexed_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());

        // 尚未入索引的新会话：先记内存集合（返回 false = 索引无此条目）
        assert!(!store.mark_running("new-1", true).unwrap());
        // 首次检查点：新条目必须带上 running=true（否则首次 run 崩溃后无从标记）
        store
            .save_history("new-1", "t", ".", None, None, &["/ws".into()], &[Message::user_text("x")])
            .unwrap();
        assert!(store.get("new-1").unwrap().running);
        assert_eq!(store.running_ids(), vec!["new-1".to_string()]);

        // 再次检查点不得抹掉标记
        store
            .save_history("new-1", "t", ".", None, None, &["/ws".into()], &[Message::user_text("y")])
            .unwrap();
        assert!(store.get("new-1").unwrap().running);

        // 收尾清除
        assert!(store.mark_running("new-1", false).unwrap());
        assert!(!store.get("new-1").unwrap().running);
        assert!(store.running_ids().is_empty());
        // 重复清除幂等
        assert!(store.mark_running("new-1", false).unwrap());
        assert!(!store.get("new-1").unwrap().running);
    }

    /// 批1：中断标记写入 / 清除 / 幂等；检查点不抹掉既有标记。
    #[test]
    fn interrupted_mark_write_clear_and_checkpoint_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store.upsert_meta(meta("s1")).unwrap();
        let at = now();
        assert!(store.mark_interrupted("s1", "crash", &at).unwrap());
        let m = store.get("s1").unwrap();
        assert_eq!(
            m.interrupted.as_ref().map(|i| (i.kind.as_str(), i.at.as_str())),
            Some(("crash", at.as_str()))
        );
        // 检查点保留标记（否则重启后中断痕迹会消失）
        store
            .save_history("s1", "t", ".", None, None, &["/ws".into()], &[Message::user_text("x")])
            .unwrap();
        assert_eq!(store.get("s1").unwrap().interrupted.unwrap().kind, "crash");
        // 前端已读/续跑后清除
        assert!(store.clear_interrupted("s1").unwrap());
        assert!(store.get("s1").unwrap().interrupted.is_none());
        assert!(store.clear_interrupted("s1").unwrap());
        // 缺席会话：不凭空造条目
        assert!(!store.mark_interrupted("ghost", "quit", &at).unwrap());
        assert!(!store.clear_interrupted("ghost").unwrap());
        assert!(store.get("ghost").is_none());
    }

    /// 批1：批量中断标记（崩溃/退出收尾）一次完成置标记 + 清 running，缺席会话跳过。
    #[test]
    fn batch_interrupt_marks_and_clears_running() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        store.upsert_meta(meta("a")).unwrap();
        store.upsert_meta(meta("b")).unwrap();
        store.mark_running("a", true).unwrap();
        store.mark_running("b", true).unwrap();
        let n = store.mark_interrupted_batch(
            &["a".to_string(), "b".to_string(), "ghost".to_string()],
            "quit",
            &now(),
        );
        assert_eq!(n, 2);
        for id in ["a", "b"] {
            let m = store.get(id).unwrap();
            assert!(!m.running);
            assert_eq!(m.interrupted.unwrap().kind, "quit");
        }
        assert!(store.running_ids().is_empty());
        // 空列表直接返回 0（不产生索引写）
        assert_eq!(store.mark_interrupted_batch(&[], "quit", &now()), 0);
    }

    /// 批1：旧 index.json 无反序列化失败（serde default 向前兼容：running 回 false、interrupted 回 None）。
    #[test]
    fn legacy_index_without_mark_fields_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(
            dir.path().join("sessions/index.json"),
            br#"{"version":1,"sessions":[{"id":"old-1","title":"t","workspace":".","created_at":"2026-01-01T00:00:00+00:00","updated_at":"2026-01-01T00:00:00+00:00"}]}"#,
        )
        .unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let m = store.get("old-1").expect("旧索引必须仍可读");
        assert!(!m.running);
        assert!(m.interrupted.is_none());
        // 不含标记字段的旧文件不会被误判为损坏
        assert!(!dir.path().join("sessions/index.json.corrupt").exists());
    }

    fn now() -> String {
        Utc::now().to_rfc3339()
    }

    /// untitled 幽灵会话存量清理：sub_*/task_* 前缀条目连 gz 与边车一起删除，
    /// 真会话条目原样保留；幂等（二次调用不报错）。
    #[test]
    fn purge_non_session_entries_removes_ghosts_keeps_real() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        // 真会话 + 两类幽灵条目
        store.upsert_meta(meta("8f2c1a9e-1234-4abc-9def-001122334455")).unwrap();
        store.upsert_meta(meta("sub_deadbeef")).unwrap();
        store.upsert_meta(meta("task_daily-1")).unwrap();
        // 幽灵 gz 与边车
        std::fs::create_dir_all(dir.path().join("histories")).unwrap();
        std::fs::write(dir.path().join("histories/sub_deadbeef.json.gz"), b"x").unwrap();
        std::fs::write(dir.path().join("sessions/sub_deadbeef.todos.json"), b"[]").unwrap();
        std::fs::write(dir.path().join("sessions/task_daily-1.artifacts.json"), b"[]").unwrap();

        let removed = store.purge_non_session_entries();
        assert_eq!(removed, 2);
        let ids: Vec<String> = store
            .list()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["8f2c1a9e-1234-4abc-9def-001122334455"]);
        assert!(!dir.path().join("histories/sub_deadbeef.json.gz").exists());
        assert!(!dir.path().join("sessions/sub_deadbeef.todos.json").exists());
        assert!(!dir.path().join("sessions/task_daily-1.artifacts.json").exists());

        // 幂等
        assert_eq!(store.purge_non_session_entries(), 0);
    }
