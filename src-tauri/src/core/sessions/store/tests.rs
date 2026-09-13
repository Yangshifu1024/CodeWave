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
        const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut data = String::with_capacity(16 * 1024 * 1024);
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for _ in 0..16 * 1024 * 1024 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            data.push(B64[((x >> 33) % 64) as usize] as char);
        }
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
