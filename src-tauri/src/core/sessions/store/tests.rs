use super::*;
use crate::core::types::{Content, Role};
// 经 `core::sessions` 重导出引用（与 `core/agent/drive.rs` 等调用方同路径）：
// 这同时钉住重导出存在——包 Y 的 checkpoint 返回值就靠它
use crate::core::sessions::{HistoryStatus, SaveReport};

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
        last_opened_at: None,
        history_status: None,
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
    store
        .save_sub_history("parent-1", "sub_abcd1234", &msgs)
        .unwrap();

    // 往返：内容一致
    let loaded = store.load_sub_history("parent-1", "sub_abcd1234").unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(matches!(&loaded[1].content[0], Content::Text { text } if text == "结论"));

    // 不进会话索引（子代理不是会话）
    assert!(
        store
            .load_index()
            .sessions
            .iter()
            .all(|m| m.id != "sub_abcd1234")
    );

    // 文件缺失返回空（旧会话降级）
    assert!(
        store
            .load_sub_history("parent-1", "sub_missing")
            .unwrap()
            .is_empty()
    );

    // 删除父会话 → 子历史目录级联清理
    store.remove("parent-1").unwrap();
    assert!(
        store
            .load_sub_history("parent-1", "sub_abcd1234")
            .unwrap()
            .is_empty()
    );
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
    assert!(matches!(&loaded[1].content[0], Content::Text { text } if text.contains("omitted")));
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

/// 上限阶梓的**拒存**分支已移除（AC-6）：剥图也无从减负的巨量文本现在照常落盘，
/// 且「不留半成品」的保证仍成立——写盘成功则段文件与索引条目都在，读回内容完整。
#[test]
fn save_history_huge_text_is_saved_not_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    // 全程不含图片：剥图无从减负（旧实现在这里拒存）
    let msgs = vec![Message::user_text(incompressible_b64(16 * 1024 * 1024))];
    let id = "s-over-cap-text";
    let report = store
        .save_history(id, "t", ".", None, None, &["/ws".into()], &msgs)
        .expect("拒存分支已移除：必须照常落盘");
    assert!(report.saved && report.is_clean(), "{report:?}");
    // 段文件与索引条目都在（不留半成品：内容完整可读）
    assert!(
        crate::core::sessions::segments::dir_bytes(&store.history_dir(id)) > 8 * 1024 * 1024,
        "巨量文本必须真的落盘"
    );
    assert!(store.get(id).is_some(), "保存成功必须写入索引条目");
    assert_eq!(store.load_history(id).unwrap().len(), 1);
    assert!(store.get(id).unwrap().history_status.is_none());
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
                last_opened_at: None,
                history_status: None,
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

/// 旧格式历史损坏：**不再整会话失败**（容错优先）——记 warn，按空历史处理。
/// 旧实现在 gz 损坏时返回 Err，会让该会话直接打不开。
#[test]
fn corrupted_legacy_history_loads_empty_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    std::fs::create_dir_all(dir.path().join("histories")).unwrap();
    std::fs::write(dir.path().join("histories/bad.json.gz"), b"not gzip at all").unwrap();
    let loaded = store
        .load_history_full("bad")
        .expect("坏历史不得让会话打不开");
    assert!(loaded.wire.is_empty());
    assert!(loaded.display.is_empty());
    assert_eq!(loaded.format, crate::core::sessions::HistoryFormat::Legacy);
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
        .save_history(
            "new-1",
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("x")],
        )
        .unwrap();
    assert!(store.get("new-1").unwrap().running);
    assert_eq!(store.running_ids(), vec!["new-1".to_string()]);

    // 再次检查点不得抹掉标记
    store
        .save_history(
            "new-1",
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("y")],
        )
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
        m.interrupted
            .as_ref()
            .map(|i| (i.kind.as_str(), i.at.as_str())),
        Some(("crash", at.as_str()))
    );
    // 检查点保留标记（否则重启后中断痕迹会消失）
    store
        .save_history(
            "s1",
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("x")],
        )
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

/// [docs/session-cleanup](../../../../../docs/session-cleanup.md)：上次打开时间落盘后不被检查点抹掉；
/// 缺席会话不得凭空造条目；旧索引（无该字段）仍可读。
#[test]
fn last_opened_at_survives_checkpoint_and_legacy_index_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("s1")).unwrap();
    let t = Utc::now();
    assert!(store.touch_session_open("s1", t).unwrap(), "首次必须写");
    assert_eq!(
        store.get("s1").unwrap().last_opened_at.as_deref(),
        Some(t.to_rfc3339().as_str())
    );
    // 检查点（save_history）必须透传保留，否则「最近打开时间」每存一次就丢
    store
        .save_history(
            "s1",
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("x")],
        )
        .unwrap();
    assert_eq!(
        store.get("s1").unwrap().last_opened_at.as_deref(),
        Some(t.to_rfc3339().as_str())
    );

    // 索引里没有的会话：不写、不造条目
    assert!(!store.touch_session_open("ghost", Utc::now()).unwrap());
    assert!(store.get("ghost").is_none());

    // 旧索引（无 last_opened_at）可读，缺失即 None（serde default，不迁移）
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir2.path().join("sessions")).unwrap();
    std::fs::write(
        dir2.path().join("sessions/index.json"),
        br#"{"version":1,"sessions":[{"id":"old-1","title":"t","workspace":".","created_at":"2026-01-01T00:00:00+00:00","updated_at":"2026-01-01T00:00:00+00:00"}]}"#,
    )
    .unwrap();
    let store2 = SessionStore::new(dir2.path().to_path_buf());
    let m = store2.get("old-1").expect("旧索引必须仍可读");
    assert!(m.last_opened_at.is_none());
    assert!(!dir2.path().join("sessions/index.json.corrupt").exists());
}

/// [docs/session-cleanup](../../../../../docs/session-cleanup.md) §3-24：10 分钟窗口内重复打开不写索引。
#[test]
fn touch_open_is_throttled_within_ten_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("s1")).unwrap();
    let t0 = Utc::now();
    assert!(store.touch_session_open("s1", t0).unwrap());
    let first = store.get("s1").unwrap().last_opened_at;

    // 1 分钟后再打开：跳过写入（索引写次数不得增加、值不变）
    let writes = store.index_write_count();
    assert!(
        !store
            .touch_session_open("s1", t0 + chrono::Duration::minutes(1))
            .unwrap()
    );
    assert_eq!(store.index_write_count(), writes, "节流窗口内不得写索引");
    assert_eq!(store.get("s1").unwrap().last_opened_at, first);

    // 正好 10 分钟：窗口边界应写
    let t10 = t0 + chrono::Duration::minutes(10);
    assert!(store.touch_session_open("s1", t10).unwrap());
    assert_eq!(
        store.get("s1").unwrap().last_opened_at.as_deref(),
        Some(t10.to_rfc3339().as_str())
    );

    // 纯函数据口径（无记录 / 不可解析 / 时间落在未来 → 行为）
    assert!(needs_open_touch(None, t10));
    assert!(needs_open_touch(Some("not-a-time"), t10));
    assert!(!needs_open_touch(
        Some(&t10.to_rfc3339()),
        t10 + chrono::Duration::seconds(599)
    ));
    assert!(needs_open_touch(
        Some(&t10.to_rfc3339()),
        t10 + chrono::Duration::seconds(600)
    ));
}

/// [docs/session-cleanup](../../../../../docs/session-cleanup.md)：计划文件产物的登记/过滤/粘性。
#[test]
fn artifact_kind_plan_registered_filtered_and_sticky() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store
        .append_artifact_kind("s1", "/ws/plan.md", ArtifactOp::Create, ArtifactKind::Plan)
        .unwrap();
    store
        .append_artifact("s1", "/ws/a.md", ArtifactOp::Create)
        .unwrap();

    let all = store.load_artifacts("s1");
    assert_eq!(all.len(), 2);
    assert_eq!(
        all.iter().find(|a| a.path == "/ws/plan.md").unwrap().kind,
        ArtifactKind::Plan
    );
    assert_eq!(
        all.iter().find(|a| a.path == "/ws/a.md").unwrap().kind,
        ArtifactKind::File
    );
    // 右栏「文件」数据源只看普通产物
    let files = store.load_file_artifacts("s1");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "/ws/a.md");

    // Plan 粘性：后续普通产物登记不得把计划文件降级为 file
    store
        .append_artifact("s1", "/ws/plan.md", ArtifactOp::Edit)
        .unwrap();
    assert_eq!(
        store
            .load_artifacts("s1")
            .iter()
            .find(|a| a.path == "/ws/plan.md")
            .unwrap()
            .kind,
        ArtifactKind::Plan
    );
    // wire 形态固定小写
    let json = std::fs::read_to_string(dir.path().join("sessions/s1.artifacts.json")).unwrap();
    assert!(json.contains("\"kind\": \"plan\""), "{json}");
    assert!(json.contains("\"kind\": \"file\""), "{json}");
}

/// [docs/session-cleanup](../../../../../docs/session-cleanup.md)：批量删除索引行只写一次索引，
/// 并清掉内存运行集合（否则迟到写入会带着 running 复活索引行）。
#[test]
fn remove_many_writes_index_once_and_clears_memory_running() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("a")).unwrap();
    store.upsert_meta(meta("b")).unwrap();
    store.upsert_meta(meta("c")).unwrap();
    store.mark_running("a", true).unwrap();
    store.mark_running("ghost", true).unwrap(); // 只在内存集合里（尚未入索引）

    let before = store.index_write_count();
    let removed = store
        .remove_many(&["a".to_string(), "ghost".to_string()])
        .unwrap();
    assert_eq!(
        removed,
        vec!["a".to_string()],
        "索引中不存在的 id 不计入已删结果"
    );
    assert_eq!(store.index_write_count(), before + 1, "整批只写一次索引");
    assert_eq!(store.list().len(), 2);
    assert!(
        store.running_in_memory().is_empty(),
        "内存运行集合也要清（防迟到检查点复活索引行）"
    );

    // 空列表：不产生任何索引写
    let before = store.index_write_count();
    assert!(store.remove_many(&[]).unwrap().is_empty());
    assert_eq!(store.index_write_count(), before);
}

/// [docs/session-cleanup](../../../../../docs/session-cleanup.md)：索引可信判定——
/// 文件缺失或内容损坏都不可信（清理据此拒绝扫索引外残留），能解析才可信；
/// 而且「曾损坏未复原」（index.json.corrupt 遗留）同样是持久的不可信信号——
/// 否则真实启动序列会把守卫顶掉：改名保全后紧接着写回的合法空索引看起来完全可信。
#[test]
fn index_is_trusted_requires_readable_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    assert!(!store.index_is_trusted(), "索引文件不存在 → 不可信");

    store.upsert_meta(meta("a")).unwrap();
    assert!(store.index_is_trusted(), "刚写过索引 → 可信");

    std::fs::write(dir.path().join("sessions/index.json"), b"{ not json").unwrap();
    assert!(!store.index_is_trusted(), "坏 JSON → 不可信");

    // 曾损坏未复原（`index.json.corrupt` 还在）→ 仍不可信：真实启动序列会在改名保全后
    // 立刻把一份合法空索引写回 index.json，只看这一份文件就会被顶掉。
    store.load_index(); // 触发改名保全（损坏 → index.json.corrupt）
    store.upsert_meta(meta("b")).unwrap(); // 写回可解析的索引
    assert_eq!(store.load_index().sessions.len(), 1, "索引本身能解析");
    assert!(
        dir.path().join("sessions/index.json.corrupt").exists(),
        "损坏备份已落盘"
    );
    assert!(
        !store.index_is_trusted(),
        "索引曾损坏未复原 → 不可信，哪怕 index.json 此刻可解析"
    );

    // 用户（或人工修复流程）移除保留证据后，索引重新变得可信：不是永久失能
    std::fs::remove_file(dir.path().join("sessions/index.json.corrupt")).unwrap();
    assert!(store.index_is_trusted(), "损坏备份移除后重新可信");
}

/// untitled 幽灵会话存量清理：sub_*/task_* 前缀条目连 gz 与边车一起删除，
/// 真会话条目原样保留；幂等（二次调用不报错）。
#[test]
fn purge_non_session_entries_removes_ghosts_keeps_real() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    // 真会话 + 两类幽灵条目
    store
        .upsert_meta(meta("8f2c1a9e-1234-4abc-9def-001122334455"))
        .unwrap();
    store.upsert_meta(meta("sub_deadbeef")).unwrap();
    store.upsert_meta(meta("task_daily-1")).unwrap();
    // 幽灵 gz 与边车
    std::fs::create_dir_all(dir.path().join("histories")).unwrap();
    std::fs::write(dir.path().join("histories/sub_deadbeef.json.gz"), b"x").unwrap();
    std::fs::write(dir.path().join("sessions/sub_deadbeef.todos.json"), b"[]").unwrap();
    std::fs::write(
        dir.path().join("sessions/task_daily-1.artifacts.json"),
        b"[]",
    )
    .unwrap();

    let removed = store.purge_non_session_entries();
    assert_eq!(removed, 2);
    let ids: Vec<String> = store.list().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["8f2c1a9e-1234-4abc-9def-001122334455"]);
    assert!(!dir.path().join("histories/sub_deadbeef.json.gz").exists());
    assert!(!dir.path().join("sessions/sub_deadbeef.todos.json").exists());
    assert!(
        !dir.path()
            .join("sessions/task_daily-1.artifacts.json")
            .exists()
    );

    // 幂等
    assert_eq!(store.purge_non_session_entries(), 0);
}

// ---------- 历史 8MB 上限：图片外置 / 越限状态 / 按轮降级 ----------
// （[docs/session-history-limits](../../../../../docs/session-history-limits.md)）

/// 测试用：一段 JSON 文本 gzip 落盘。
fn gzip_bytes(s: &str) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, s.as_bytes()).unwrap();
    enc.finish().unwrap()
}

/// 测试用：把某会话的全部段文件按序号拼成一个字符串（`gunzip_text` 的历史位置）。
fn read_all_segments(store: &SessionStore, id: &str) -> String {
    let mut out = String::new();
    for (_, path) in crate::core::sessions::segments::list_segments(&store.history_dir(id)) {
        out.push_str(&String::from_utf8_lossy(&std::fs::read(path).unwrap()));
    }
    out
}
/// 测试用：某 owner 的 blob 文件数。
fn blob_count(store: &SessionStore, owner: &str) -> usize {
    match std::fs::read_dir(store.image_blobs_dir(owner)) {
        Ok(rd) => rd.flatten().filter(|e| e.path().is_file()).count(),
        Err(_) => 0,
    }
}

/// 测试用：一条只含图片的 user 消息。
fn image_msg(data: String) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Image {
            media_type: "image/png".into(),
            data,
        }],
        created_at: None,
    }
}

/// 图片外置：历史文件里不再有 base64 原文、blob 目录里有一份，往返后图片原样回来，
/// 且**不触发任何降级**（契约自洽：合法贴图不再必然降级）。
#[test]
fn save_history_externalizes_images_without_degrading() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let data = incompressible_b64(2 * 1024 * 1024);
    let msgs = vec![Message::user_text("q"), image_msg(data.clone())];
    let report = store
        .save_history("s-ext", "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();
    assert!(report.is_clean(), "外置后不该有任何降级：{report:?}");
    assert_eq!((report.stripped_images, report.dropped_rounds), (0, 0));

    // 段文件：只有引用，没有 base64 原文；体积与图片本身无关（几十 KB 量级）
    let json = read_all_segments(&store, "s-ext");
    assert!(json.contains("image_blob"), "落盘形态应是引用：{json}");
    assert!(!json.contains(&data[..1024]), "历史里不得内联 base64 原文");
    let bytes = crate::core::sessions::segments::dir_bytes(&store.history_dir("s-ext"));
    assert!(bytes < 64 * 1024, "外置后段文件应远小于 8MB：{bytes}");
    assert_eq!(report.bytes as u64, bytes);
    // blob 目录：恰好一份，内容就是原 base64
    let blobs: Vec<_> = std::fs::read_dir(store.image_blobs_dir("s-ext"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(blobs.len(), 1);
    assert_eq!(std::fs::read_to_string(blobs[0].path()).unwrap(), data);

    // 往返：图片原样回来；干净保存 → 索引里没有状态
    let loaded = store.load_history("s-ext").unwrap();
    assert!(matches!(&loaded[1].content[0], Content::Image { data: d, .. } if d == &data));
    assert!(store.get("s-ext").unwrap().history_status.is_none());
}

/// 旧数据兜底路径：图片大到无法外置（超单图上限 → 保持内联）时仍是「剥图降级」，
/// 但状态挂上索引（重启后可见）、剥图张数如实上报。
#[test]
fn save_history_degraded_strips_images_and_records_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    // 16M 字符 base64 > 单图外置上限 → 保持内联 → gz 必超 8MB → 剥图降级
    let msgs = vec![
        Message::user_text("q"),
        image_msg(incompressible_b64(16 * 1024 * 1024)),
    ];
    let report = store
        .save_history("s-degraded", "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();
    assert!(report.saved && !report.is_clean());
    assert_eq!(report.stripped_images, 1, "一张图片被剥：{report:?}");
    assert_eq!(report.dropped_rounds, 0);
    assert_eq!(blob_count(&store, "s-degraded"), 0, "内联的图不产生 blob");

    let loaded = store.load_history("s-degraded").unwrap();
    assert!(matches!(&loaded[1].content[0], Content::Text { text } if text.contains("omitted")));
    assert!(matches!(
        store.get("s-degraded").unwrap().history_status,
        Some(HistoryStatus::Degraded {
            stripped_images: 1,
            dropped_rounds: 0,
            ..
        })
    ));
}

/// AC-1（本批核心）：**落盘不再 trim**。旧实现在超预算时按轮降级（只保最后一轮），
/// 超预算的旧轮次在磁盘上永久消失；现在内存里有就落盘，`dropped_rounds` 恒为 0。
#[test]
fn save_history_no_longer_trims_rounds_away() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    // 两轮、每轮 ≈8M 不可压缩字符：gzip 后远超 8MB（旧实现会剥图（无效）+ 按轮降级）
    let big = incompressible_b64(8 * 1024 * 1024);
    let msgs = vec![
        Message::user_text(format!("round1 {big}")),
        Message::user_text(format!("round2 {big}")),
    ];
    let report = store
        .save_history("s-rounds", "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();
    assert!(report.saved, "{report:?}");
    assert_eq!(report.dropped_rounds, 0, "按轮降级已移除：{report:?}");
    assert_eq!(report.stripped_images, 0, "全程无图片");

    // 落盘不再裁剪：两轮都在磁盘上
    let loaded = store.load_history_full("s-rounds").unwrap();
    assert_eq!(loaded.display.len(), 2, "内存里有就落盘（AC-1）");
    assert!(loaded.display[0].text_joined().starts_with("round1"));
    assert!(loaded.display[1].text_joined().starts_with("round2"));
    assert_eq!(loaded.wire.len(), 2, "wire 与落盘一致（两轮都在）");
    assert!(store.get("s-rounds").unwrap().history_status.is_none());
}
/// 上限阶梓的**拒存**分支已移除（AC-6/AC-7）：单轮巨量文本现在照常落盘，
/// 既有段一个字节都不动（只新增段），索引里也不会再出现 `Rejected` 状态。
#[test]
fn save_history_huge_history_is_no_longer_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-huge";
    store
        .save_history(
            id,
            "原标题",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("hi")],
        )
        .unwrap();
    let seg1 = segment_bytes(&store, id, "0001.jsonl");

    // 单轮巨量文本（无图片：剥图无从减负；旧实现会在此拒存）
    let msgs = vec![Message::user_text(incompressible_b64(16 * 1024 * 1024))];
    let report = store
        .save_history(id, "新标题", ".", None, None, &["/ws".into()], &msgs)
        .expect("上限拒存分支已移除：超量历史必须照常落盘");
    assert!(report.saved, "{report:?}");
    assert_eq!((report.stripped_images, report.dropped_rounds), (0, 0));

    // 既有段只被追加封口行，前缀仍逐字节未变（只新增段，绝不覆盖/截断）
    assert!(segment_bytes(&store, id, "0001.jsonl").starts_with(&seg1));
    // 索引：不再产生 Rejected
    let meta = store.get(id).unwrap();
    assert!(
        meta.history_status.is_none(),
        "P1 起不再产生 Rejected：{:?}",
        meta.history_status
    );
    assert_eq!(meta.title, "新标题");
    // 内容完整可读
    let loaded = store.load_history_full(id).unwrap();
    assert_eq!(loaded.wire.len(), 1);
    assert!(loaded.wire[0].text_joined().len() > 1024 * 1024);
    assert!(
        loaded.segments.len() >= 2,
        "旧段仍在（展示侧可回溯）：{:?}",
        loaded.segments
    );
    assert_eq!(loaded.format, crate::core::sessions::HistoryFormat::New);
    assert!(
        !SaveReport::rejected().saved,
        "`rejected()` 仍用于真实写盘失败"
    );
}
/// 状态自愈：下一次干净保存把 `Degraded` 清掉（否则提示会永远挂着）。
#[test]
fn clean_save_clears_degraded_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    // 触发剥图兜底（内联巨图超单图外置上限）→ Degraded 挂上索引
    let heavy = vec![
        Message::user_text("q"),
        image_msg(incompressible_b64(16 * 1024 * 1024)),
    ];
    store
        .save_history("s-heal", "t", ".", None, None, &["/ws".into()], &heavy)
        .unwrap();
    assert!(
        matches!(
            store.get("s-heal").unwrap().history_status,
            Some(HistoryStatus::Degraded {
                stripped_images: 1,
                dropped_rounds: 0,
                ..
            })
        ),
        "剥图兜底必须挂上 Degraded：{:?}",
        store.get("s-heal").unwrap().history_status
    );

    // 再正常跑一轮（小历史、无图）→ 状态清除
    let report = store
        .save_history(
            "s-heal",
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("短")],
        )
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(
        store.get("s-heal").unwrap().history_status.is_none(),
        "干净保存必须清除降级状态"
    );
    // 检查点（upsert_meta）不得误清状态：再存一次仍为 None（幂等）
    store.upsert_meta(meta("s-heal")).unwrap();
    assert!(store.get("s-heal").unwrap().history_status.is_none());
}
/// 修 1：`set_history_status` 在「传入值与索引现值相同」时**不产生任何磁盘写**。
///
/// 检查点路径里它紧跟 `upsert_meta`（已写一次索引），值没变时那次写盘纯属冗余；
/// 语义不变——值变了照写、该清还是清。
#[test]
fn set_history_status_unchanged_value_skips_index_write() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("s1")).unwrap();
    let index_path = dir.path().join("sessions/index.json");
    let degraded = HistoryStatus::Degraded {
        stripped_images: 2,
        dropped_rounds: 1,
        at: "2026-01-01T00:00:00+00:00".into(),
    };

    // 首次：None → Degraded 必须写盘
    let writes = store.index_write_count();
    store
        .set_history_status("s1", Some(degraded.clone()))
        .unwrap();
    assert_eq!(store.index_write_count(), writes + 1, "值变了必须写盘");
    let after_first = std::fs::read(&index_path).unwrap();
    assert_eq!(
        store.get("s1").unwrap().history_status,
        Some(degraded.clone())
    );

    // 第二次同值：不得写盘（写次数不增、文件内容一字不改）
    let writes = store.index_write_count();
    store
        .set_history_status("s1", Some(degraded.clone()))
        .unwrap();
    assert_eq!(store.index_write_count(), writes, "值未变不得写索引");
    assert_eq!(std::fs::read(&index_path).unwrap(), after_first);
    assert_eq!(store.get("s1").unwrap().history_status, Some(degraded));

    // 值变了（清空）：照写——语义不变，该清还是清
    let writes = store.index_write_count();
    store.set_history_status("s1", None).unwrap();
    assert_eq!(store.index_write_count(), writes + 1, "清除状态必须写盘");
    assert!(store.get("s1").unwrap().history_status.is_none());

    // 已是 None 再清：幂等，不写盘
    let writes = store.index_write_count();
    store.set_history_status("s1", None).unwrap();
    assert_eq!(store.index_write_count(), writes, "幂等清除不得写索引");
}

/// 修 2：历史文件已写成功之后，**索引写失败不得被上报成「拒存」**。
///
/// 失败注入：把 `sessions/` 换成同名普通文件 → 索引（`sessions/index.json`）必然写失败，
/// 而历史在 `histories/` 下不受影响。旧实现把索引错误 `?` 上抛，调用方
/// （`core/agent/drive.rs::checkpoint`）映射成 `SaveReport::rejected()`，前端于是谎报
/// 「历史未能保存（超过 8MB 上限）」——历史其实已经写成功了。
#[test]
fn index_write_failure_after_history_saved_still_reports_saved() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sessions"), b"not a dir").unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let msgs = vec![Message::user_text("hi")];

    let report = store
        .save_history("s-idx-fail", "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();
    assert!(
        report.saved,
        "历史已落盘，索引写失败不得上报拒存：{report:?}"
    );
    assert!(report.is_clean(), "无降级：{report:?}");
    assert_eq!(
        report.bytes as u64,
        crate::core::sessions::segments::dir_bytes(&store.history_dir("s-idx-fail")),
        "bytes 如实上报段文件字节总和"
    );
    // 数据本体在（历史可读回），派生缓存缺失（索引写失败只告警）
    assert_eq!(
        store.load_history("s-idx-fail").unwrap()[0].text_joined(),
        "hi"
    );
    assert!(!dir.path().join("sessions/index.json").exists());
}

/// 🔴 GC 的引用集合必须是「磁盘上**全部保留段**引用的 blob 并集」。
///
/// 旧实现按「本次保存的集合」回收：历史缩短（压缩 / 只留最后一轮）时会把**旧段仍在引用**
/// 的 blob 删掉——旧段还留在磁盘上、展示侧还能翻到它，删图是不可逆的数据丢失。
/// 现在旧段一律保留，因此它们的 blob 也必须保留。
#[test]
fn gc_keeps_blobs_referenced_by_retained_segments() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let first = incompressible_b64(4096);
    let second = incompressible_b64(8192);
    let msgs = vec![
        Message::user_text("r1"),
        image_msg(first.clone()),
        Message::user_text("r2"),
        image_msg(second.clone()),
    ];
    store
        .save_history("s-gc", "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();
    assert_eq!(blob_count(&store, "s-gc"), 2);

    // 历史缩短（模拟压缩后只剩最后一轮）：新增基线段 + 压缩边界，旧段一律保留
    let keep = vec![Message::user_text("r2"), image_msg(second.clone())];
    let report = store
        .save_history("s-gc", "t", ".", None, None, &["/ws".into()], &keep)
        .unwrap();
    assert!(report.saved && report.is_clean(), "{report:?}");
    assert_eq!(
        blob_count(&store, "s-gc"),
        2,
        "旧段仍引用第一张图 → GC 不得删它（用「本次集合」回收就会删掉）"
    );
    // 旧内容仍可读（展示侧能翻到压缩前），两张图都完好
    let loaded = store.load_history_full("s-gc").unwrap();
    assert_eq!(loaded.display.len(), 4, "display = 全部 message 记录");
    assert_eq!(loaded.wire.len(), 2, "wire = 最后一个重置点之后");
    let imgs: Vec<String> = loaded
        .display
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|c| match c {
            Content::Image { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        imgs,
        vec![first, second],
        "两张图都必须还在（含旧段引用那张）"
    );
}
/// blob 缺失（被手工删掉 / 磁盘损坏）→ 该图降级为占位文本，会话仍能加载。
#[test]
fn load_history_with_missing_blob_degrades_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let msgs = vec![Message::user_text("q"), image_msg("AAAA".into())];
    store
        .save_history("s-miss", "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();
    let blob_dir = store.image_blobs_dir("s-miss");
    for e in std::fs::read_dir(&blob_dir).unwrap().flatten() {
        std::fs::remove_file(e.path()).unwrap();
    }

    let loaded = store.load_history("s-miss").expect("blob 缺失不得阻断加载");
    assert!(matches!(
        &loaded[1].content[0],
        Content::Text { text } if text == "[image image/png 丢失]"
    ));
    assert_eq!(loaded[0].text_joined(), "q", "其余内容完好");
}

/// 旧数据兼容：本批之前写下的历史（内联 `image` tag + base64 原文）照常可读，无不可逆迁移。
#[test]
fn load_history_reads_legacy_inline_image() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    std::fs::create_dir_all(store.histories_dir()).unwrap();
    let json =
        r#"[{"role":"user","content":[{"type":"image","media_type":"image/png","data":"AAAA"}]}]"#;
    std::fs::write(store.history_path("s-legacy"), gzip_bytes(json)).unwrap();
    let loaded = store.load_history("s-legacy").unwrap();
    assert!(matches!(&loaded[0].content[0], Content::Image { data, .. } if data == "AAAA"));
}

/// 级联删除（`remove` 路径）：`.toolres/`、`.imgblob/` 与子历史的 blob 目录都随会话消失
///（此前 `remove` 连 `.toolres/` 都没删，与 cleanup 路径口径不一致）。
#[test]
fn remove_cascades_toolres_and_image_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-cascade";
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("q"), image_msg("AAAA".into())],
        )
        .unwrap();
    // 手工造出工具结果 sidecar 目录与子历史（带自己的 blob）
    std::fs::create_dir_all(store.tool_results_dir(id)).unwrap();
    store
        .save_sub_history(
            id,
            "sub_cascade",
            &[Message::user_text("s"), image_msg("BBBB".into())],
        )
        .unwrap();
    assert!(store.image_blobs_dir(id).is_dir());
    assert!(store.tool_results_dir(id).is_dir());
    assert!(store.sub_image_blobs_dir(id, "sub_cascade").is_dir());

    store.remove(id).unwrap();
    assert!(
        !store.image_blobs_dir(id).exists(),
        "会话 blob 目录应级联删除"
    );
    assert!(
        !store.tool_results_dir(id).exists(),
        "工具结果目录应级联删除"
    );
    assert!(
        !store.sub_image_blobs_dir(id, "sub_cascade").exists(),
        "子历史的 blob 目录应随父会话级联删除"
    );
    assert!(!store.sub_histories_dir(id).exists());
}

/// 幽灵条目清理同步删掉两个托管目录（否则 sub_*/task_* 的 blob 会永久泄漏）。
#[test]
fn purge_non_session_entries_removes_managed_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("sub_ghost")).unwrap();
    std::fs::create_dir_all(store.image_blobs_dir("sub_ghost")).unwrap();
    std::fs::create_dir_all(store.tool_results_dir("sub_ghost")).unwrap();
    assert_eq!(store.purge_non_session_entries(), 1);
    assert!(!store.image_blobs_dir("sub_ghost").exists());
    assert!(!store.tool_results_dir("sub_ghost").exists());
}

/// 子代理历史同阶梯：图片同样外置、越限按轮降级，但**绝不拒存**（子代理没有 UI 载体）。
#[test]
fn sub_history_shares_ladder_but_never_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    // 图片外置：blob 归子历史自己（不在父会话的目录里）
    let img = incompressible_b64(4096);
    store
        .save_sub_history("parent-1", "sub_img", &[image_msg(img.clone())])
        .unwrap();
    assert_eq!(
        blob_count(&store, &sub_blob_owner("parent-1", "sub_img")),
        1
    );
    assert_eq!(blob_count(&store, "parent-1"), 0);
    let loaded = store.load_sub_history("parent-1", "sub_img").unwrap();
    assert!(matches!(&loaded[0].content[0], Content::Image { data, .. } if data == &img));

    // 单轮巨量文本：主历史会拒存，子历史必须仍然落盘（不拒存）
    let heavy = vec![Message::user_text(incompressible_b64(16 * 1024 * 1024))];
    let report = store
        .save_sub_history("parent-1", "sub_heavy", &heavy)
        .expect("子代理历史绝不拒存");
    assert!(report.saved);
    assert!(
        !store
            .load_sub_history("parent-1", "sub_heavy")
            .unwrap()
            .is_empty(),
        "降级后仍应落盘"
    );
}

// ---------- 并发保存：GC 不得删掉磁盘历史仍引用的 blob ----------
// （[docs/session-history-limits](../../../../../docs/session-history-limits.md)）

/// 测试用：从落盘历史 JSON 里取出全部图片 blob 引用（不依赖落盘 DTO 的可见性）。
fn blob_refs(json: &str) -> Vec<String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(b)) = map.get("blob") {
                    out.push(b.clone());
                }
                for child in map.values() {
                    walk(child, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|c| walk(c, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    // 段式落盘是 JSONL（一行一条记录）：逐行解析；旧格式的单行 JSON 数组同样适用
    for line in json.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        walk(&v, &mut out);
    }
    out
}

/// 🔴 并发保存时，GC 绝不得删掉磁盘历史仍引用的 blob。
///
/// 竞态（修前）：`save_history` 的 GC 判据是**本次保存内存快照**的引用集合，而保存路径没有
/// per-session 串行化——A（旧快照）的 GC 若晚于 B（新快照，含新图）的历史写盘，A 会删掉
/// B 仍引用的 blob，该会话重开后那张图只剩占位文本（不可逆数据丢失）。
/// 修法：`save_lock` 串行化「快照（外置 blob 写）→ 写历史 → upsert_meta → set_history_status → GC」。
#[test]
fn concurrent_saves_never_gc_blobs_referenced_by_disk_history() {
    const ROUNDS: usize = 25;
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(SessionStore::new(dir.path().to_path_buf()));
    std::thread::scope(|s| {
        for t in 0..2usize {
            let st = store.clone();
            s.spawn(move || {
                for r in 0..ROUNDS {
                    // 两路图片内容互不相同（长度即内容种子）：任一方被误删都必现
                    let data = incompressible_b64(16 * 1024 + t * 4096 + r);
                    let msgs = vec![Message::user_text(format!("t{t}-r{r}")), image_msg(data)];
                    st.save_history("s-race", "t", ".", None, None, &["/ws".into()], &msgs)
                        .unwrap();
                }
            });
        }
    });

    // 磁盘上全部保留段引用的每个 blob 都必须存在（否则重开后那张图是占位文本）
    let referenced = blob_refs(&read_all_segments(&store, "s-race"));
    assert!(!referenced.is_empty(), "历史里应有图片引用");
    let blob_dir = store.image_blobs_dir("s-race");
    for blob in &referenced {
        assert!(
            blob_dir.join(blob).is_file(),
            "磁盘历史仍引用 {blob}，GC 不得删掉它（目录内容：{:?}）",
            std::fs::read_dir(&blob_dir)
                .map(|rd| rd.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
        );
    }
    // 语义层：读回后没有任何图片被降级成占位文本
    let loaded = store.load_history("s-race").unwrap();
    assert!(
        loaded.iter().all(|m| m
            .content
            .iter()
            .all(|c| !matches!(c, Content::Text { text } if text.contains("丢失")))),
        "并发保存后图片不得变成占位文本"
    );
}

// ---------- 子历史 blob 的 owner 命名空间（<父会话 id>__<sub>） ----------

/// 🔴 不同父会话下的**同名 sub** 必须各用各的 blob 目录。
///
/// `sub_id` 只是 uuid 前 8 位十六进制（32 bit，见 `tools/subagent.rs`），两个父会话下
/// 同名并非不可能；共用 `sessions/<sub>.imgblob/` 时，后一次保存的 GC 会把前一次
/// 仍引用的图删掉（读回即占位文本）。
#[test]
fn same_sub_name_under_different_parents_keeps_isolated_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let img_a = incompressible_b64(4096);
    let img_b = incompressible_b64(8192);
    store
        .save_sub_history("parent-a", "sub_deadbeef", &[image_msg(img_a.clone())])
        .unwrap();
    store
        .save_sub_history("parent-b", "sub_deadbeef", &[image_msg(img_b.clone())])
        .unwrap();

    // 两个目录互不重叠，且不再有裸 sub 命名的目录
    let dir_a = store.sub_image_blobs_dir("parent-a", "sub_deadbeef");
    let dir_b = store.sub_image_blobs_dir("parent-b", "sub_deadbeef");
    assert_ne!(dir_a, dir_b, "同名 sub 在两个父会话下必须是两个目录");
    assert_eq!(
        blob_count(&store, &sub_blob_owner("parent-a", "sub_deadbeef")),
        1
    );
    assert_eq!(
        blob_count(&store, &sub_blob_owner("parent-b", "sub_deadbeef")),
        1
    );
    assert!(
        !store.image_blobs_dir("sub_deadbeef").exists(),
        "不得再用裸 sub 当 owner"
    );

    // 各自读回自己的图（共用目录时后一次保存的 GC 已删掉先前的图 → 这里必现占位文本）
    let a = store.load_sub_history("parent-a", "sub_deadbeef").unwrap();
    assert!(matches!(&a[0].content[0], Content::Image { data, .. } if data == &img_a));
    let b = store.load_sub_history("parent-b", "sub_deadbeef").unwrap();
    assert!(matches!(&b[0].content[0], Content::Image { data, .. } if data == &img_b));
}

/// 🔴 删掉一个父会话，不得连带删掉另一个父会话下同名 sub 的图。
#[test]
fn removing_one_parent_keeps_other_parents_sub_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("parent-a")).unwrap();
    store.upsert_meta(meta("parent-b")).unwrap();
    let img_b = incompressible_b64(8192);
    store
        .save_sub_history(
            "parent-a",
            "sub_deadbeef",
            &[image_msg(incompressible_b64(4096))],
        )
        .unwrap();
    store
        .save_sub_history("parent-b", "sub_deadbeef", &[image_msg(img_b.clone())])
        .unwrap();

    store.remove("parent-a").unwrap();

    assert!(
        !store
            .sub_image_blobs_dir("parent-a", "sub_deadbeef")
            .exists(),
        "被删父会话的子历史 blob 目录应随之消失"
    );
    assert!(
        store
            .sub_image_blobs_dir("parent-b", "sub_deadbeef")
            .is_dir(),
        "另一个父会话的子历史 blob 目录必须原样保留"
    );
    assert_eq!(
        blob_count(&store, &sub_blob_owner("parent-b", "sub_deadbeef")),
        1
    );
    let b = store.load_sub_history("parent-b", "sub_deadbeef").unwrap();
    assert!(matches!(&b[0].content[0], Content::Image { data, .. } if data == &img_b));
}

// ---------- 分段 append-only JSONL 存储（批2 P1） ----------
//
// 守护本批的核心不变量：**落盘不再 trim**、**任何 fallback 都只新增段**、
// 增量追加只动尾部字节、压缩边界可逐字节还原 wire、GC 用「磁盘上全部保留段引用的并集」。

/// 测试用：段文件名清单（按序号）。
fn segment_files(store: &SessionStore, id: &str) -> Vec<String> {
    crate::core::sessions::segments::list_segments(&store.history_dir(id))
        .into_iter()
        .map(|(_, p)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

/// 测试用：段文件字节。
fn segment_bytes(store: &SessionStore, id: &str, file: &str) -> Vec<u8> {
    std::fs::read(store.history_dir(id).join(file)).unwrap()
}

/// §7.1 增量追加：连续多次保存只**追加**新消息——既有字节前缀逐字节不变，不重写、不覆盖。
#[test]
fn append_only_increments_never_rewrite_existing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-append";
    let ws = ["/ws".to_string()];
    store
        .save_history(id, "t", ".", None, None, &ws, &[Message::user_text("m1")])
        .unwrap();
    let before = segment_bytes(&store, id, "0001.jsonl");

    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("m1"), Message::user_text("m2")],
        )
        .unwrap();
    let after = segment_bytes(&store, id, "0001.jsonl");
    assert!(after.len() > before.len(), "应追加了新字节");
    assert!(
        after.starts_with(&before),
        "既有字节前缀必须逐字节不变（只追加、不重写）"
    );
    assert_eq!(
        segment_files(&store, id),
        vec!["0001.jsonl"],
        "未到阈值不切段"
    );

    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[
                Message::user_text("m1"),
                Message::user_text("m2"),
                Message::user_text("m3"),
            ],
        )
        .unwrap();
    let after2 = segment_bytes(&store, id, "0001.jsonl");
    assert!(after2.starts_with(&after), "第二次追加同样只追加");
    assert_eq!(store.load_history(id).unwrap().len(), 3);
}

/// §7.1（后半）增量游标命中：`len == written` 且指纹一致 → **不产生任何写**
///（段文件字节不变、索引写次数不增）。
#[test]
fn identical_history_save_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-noop";
    let ws = ["/ws".to_string()];
    let msgs = vec![Message::user_text("q"), Message::user_text("q2")];
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    let files = segment_files(&store, id);
    let snap: Vec<Vec<u8>> = files.iter().map(|f| segment_bytes(&store, id, f)).collect();
    let writes = store.index_write_count();

    let report = store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    assert!(report.saved && report.is_clean());
    assert_eq!(
        files
            .iter()
            .map(|f| segment_bytes(&store, id, f))
            .collect::<Vec<_>>(),
        snap,
        "无写入的保存不得改动段文件"
    );
    assert_eq!(
        store.index_write_count(),
        writes,
        "无写入的保存不得重写索引"
    );
}

/// §7.2 前缀被改写（长度不变、内容变了）→ 走**基线段**，且**旧段逐字节保留**。
#[test]
fn prefix_rewrite_writes_base_segment_and_keeps_old_segments() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-rewrite";
    let ws = ["/ws".to_string()];
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("q1"), Message::user_text("q2")],
        )
        .unwrap();
    let old = segment_bytes(&store, id, "0001.jsonl");

    // sanitize/repair 原地改写了已有消息（长度不变 → 指纹不匹配）
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("q1-改"), Message::user_text("q2")],
        )
        .unwrap();

    assert_eq!(
        segment_files(&store, id),
        vec!["0001.jsonl", "0002.jsonl"],
        "只新增基线段，绝不覆盖旧段"
    );
    assert!(
        segment_bytes(&store, id, "0001.jsonl").starts_with(&old),
        "旧段内容逐字节保留"
    );
    assert!(
        read_all_segments(&store, id)
            .contains(r#"{"kind":"header","schema":1,"session":"s-rewrite","seq":2,"base":true"#),
        "新段必须标为基线段（base: true）"
    );
    let loaded = store.load_history_full(id).unwrap();
    assert_eq!(loaded.wire.len(), 2);
    assert_eq!(
        loaded.wire[0].text_joined(),
        "q1-改",
        "wire 用最后一个重置点"
    );
    assert_eq!(
        loaded.display.len(),
        4,
        "display = 全部 message 记录（旧段 + 基线段）"
    );
    assert_eq!(loaded.format, crate::core::sessions::HistoryFormat::New);
}

/// §7.3 段封口（条数阈值）：200 条触发切段，且**绝不切在 tool_use / tool_result 配对中间**。
#[test]
fn segment_seals_by_message_count_and_never_splits_tool_pairing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-seal";
    // 三条一组的 [user, assistant(tool_use), tool(result)]：第 200 条（下标 199）是带 tool_use 的
    // assistant，它的结果在第 201 条——阈值恰好落在配对中间，必须推迟封口
    let mut msgs: Vec<Message> = Vec::new();
    for i in 0..80 {
        msgs.push(Message::user_text(format!("u{i}")));
        msgs.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                id: format!("t{i}"),
                name: "read".into(),
                args: serde_json::json!({"files": []}),
            }],
            created_at: None,
        });
        msgs.push(Message::tool_results(vec![Content::ToolResult {
            tool_use_id: format!("t{i}"),
            content: "ok".into(),
            is_error: false,
        }]));
    }
    let ws = ["/ws".to_string()];
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();

    let files = segment_files(&store, id);
    assert!(files.len() >= 2, "200 条阈值必须切段：{files:?}");
    // 逐段校验：段末不得有未配对的 tool_use（封口不得切在配对中间）
    for file in &files {
        let text = String::from_utf8(segment_bytes(&store, id, file)).unwrap();
        let mut pending: Vec<String> = Vec::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v["kind"] != "message" {
                continue;
            }
            for c in v["msg"]["content"].as_array().unwrap() {
                match c["type"].as_str().unwrap() {
                    "tool_use" => pending.push(c["id"].as_str().unwrap().to_string()),
                    "tool_result" => {
                        let id = c["tool_use_id"].as_str().unwrap().to_string();
                        pending.retain(|p| p != &id);
                    }
                    _ => {}
                }
            }
        }
        assert!(
            pending.is_empty(),
            "段 {file} 末尾有未配对的 tool_use：{pending:?}"
        );
    }
    // 内容不丢：全部 message 记录完整可读
    assert_eq!(
        store.load_history_full(id).unwrap().display.len(),
        msgs.len()
    );
}

/// §7.3（体积阈值）：段文件超 512KB 切段（与条数阈值「先触者为准」）。
#[test]
fn segment_seals_by_byte_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-bytes";
    let big = "x".repeat(300 * 1024); // 单条 ≈300KB < 512KB
    let ws = ["/ws".to_string()];
    let msgs = vec![
        Message::user_text(format!("a {big}")),
        Message::user_text(format!("b {big}")),
        Message::user_text(format!("c {big}")),
    ];
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    let files = segment_files(&store, id);
    assert_eq!(files.len(), 2, "两条即超 512KB → 切段：{files:?}");
    assert!(segment_bytes(&store, id, &files[0]).len() >= 512 * 1024);
    let scan = crate::core::sessions::segments::scan(&store.history_dir(id), true);
    assert!(scan.segments[0].sealed, "切走的段必须已封口");
    assert_eq!(scan.state.written, 3);
}

/// §7.4 崩溃残留（尾部半行）：加载丢弃残行、其余完整；**不报「历史损坏」**。
#[test]
fn half_line_at_segment_tail_is_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-half";
    let ws = ["/ws".to_string()];
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("q"), Message::user_text("q2")],
        )
        .unwrap();
    // 手工追加半行（模拟进程被杀）
    let path = store.history_dir(id).join("0001.jsonl");
    let mut raw = std::fs::read(&path).unwrap();
    raw.extend_from_slice(br#"{"kind":"message","msg":{"role":"user","conte"#);
    std::fs::write(&path, raw).unwrap();

    let loaded = store.load_history_full(id).expect("半行不得让会话打不开");
    assert_eq!(loaded.wire.len(), 2, "残行丢弃，完整记录保留");
    assert_eq!(loaded.display.len(), 2);
    assert_eq!(loaded.format, crate::core::sessions::HistoryFormat::New);
}

/// §7.5 坏行 / 坏段：跳过并继续，**绝不因容错而报「历史损坏」整会话失败**。
#[test]
fn bad_line_and_bad_segment_are_skipped_without_failing_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-bad";
    let ws = ["/ws".to_string()];
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("good1")],
        )
        .unwrap();
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("good1"), Message::user_text("good2")],
        )
        .unwrap();
    // 坏行插进段中间
    let path = store.history_dir(id).join("0001.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.insert(1, "{ 这不是 JSON }");
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    // 坏段：0002 缺头记录
    std::fs::write(
        store.history_dir(id).join("0002.jsonl"),
        "{\"kind\":\"message\"}\n",
    )
    .unwrap();

    let loaded = store
        .load_history_full(id)
        .expect("坏行 / 坏段不得让会话打不开");
    assert_eq!(loaded.display.len(), 2, "坏行跳过，其余照常");
    let scan = crate::core::sessions::segments::scan(&store.history_dir(id), true);
    assert_eq!(scan.bad_segments, 1, "缺头记录的段被跳过");
    assert_eq!(
        scan.segments.len(),
        1,
        "坏段不进段清单（P2 分页不会指向它）"
    );
}

/// 🔴-1 格式裁决与清理入口同源：新格式**没有可读消息**时，旧文件才是那份内容唯一可读的副本
/// → 回落旧格式。（此前只判「段目录存在」，用户打开这类会话看到的是**空历史**，数据其实还在旧文件里。）
#[test]
fn new_format_without_readable_messages_falls_back_to_legacy() {
    let ws = ["/ws".to_string()];
    let json = r#"[{"role":"user","content":[{"type":"text","text":"旧内容"}]}]"#;

    // ① 空段目录 + 旧文件 → 旧格式，内容非空
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    std::fs::create_dir_all(store.history_dir("s-empty-dir")).unwrap();
    std::fs::write(store.history_path("s-empty-dir"), gzip_bytes(json)).unwrap();
    assert!(
        !store.reads_new_format("s-empty-dir"),
        "空段目录不算新格式权威"
    );
    let loaded = store.load_history_full("s-empty-dir").unwrap();
    assert_eq!(loaded.format, crate::core::sessions::HistoryFormat::Legacy);
    assert_eq!(
        loaded.wire[0].text_joined(),
        "旧内容",
        "旧文件内容必须看得见"
    );
    assert_eq!(
        store.load_history("s-empty-dir").unwrap().len(),
        1,
        "wire 路径与全量路径同源裁决"
    );

    // ② 段目录全是坏段 + 旧文件 → 同上（坏段不算「有历史」）
    let dir2 = tempfile::tempdir().unwrap();
    let store2 = SessionStore::new(dir2.path().to_path_buf());
    std::fs::create_dir_all(store2.history_dir("s-bad-only")).unwrap();
    std::fs::write(
        store2.history_dir("s-bad-only").join("0001.jsonl"),
        "{ 这不是头记录 }\n",
    )
    .unwrap();
    std::fs::write(store2.history_path("s-bad-only"), gzip_bytes(json)).unwrap();
    assert!(
        !store2.reads_new_format("s-bad-only"),
        "全是坏段 = 没有可读消息"
    );
    let loaded2 = store2.load_history_full("s-bad-only").unwrap();
    assert_eq!(loaded2.format, crate::core::sessions::HistoryFormat::Legacy);
    assert_eq!(loaded2.wire[0].text_joined(), "旧内容");

    // ③ 有可读段 + 旧文件 → 以新格式为权威（现状保持）
    let dir3 = tempfile::tempdir().unwrap();
    let store3 = SessionStore::new(dir3.path().to_path_buf());
    std::fs::create_dir_all(store3.histories_dir()).unwrap();
    std::fs::write(store3.history_path("s-both"), gzip_bytes(json)).unwrap();
    store3
        .save_history(
            "s-both",
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("新内容")],
        )
        .unwrap();
    assert!(store3.reads_new_format("s-both"));
    let both = store3.load_history_full("s-both").unwrap();
    assert_eq!(both.format, crate::core::sessions::HistoryFormat::New);
    assert_eq!(both.wire[0].text_joined(), "新内容");
    assert!(
        store3.history_path("s-both").exists(),
        "旧文件不自动删（只在显式入口清理）"
    );

    // ④ 只有段目录（无旧文件）→ 现状保持：走新格式路径，绝不因「没内容」报错
    let dir4 = tempfile::tempdir().unwrap();
    let store4 = SessionStore::new(dir4.path().to_path_buf());
    std::fs::create_dir_all(store4.history_dir("s-empty-only")).unwrap();
    assert!(
        store4.reads_new_format("s-empty-only"),
        "没有旧文件可回落 → 维持新格式路径"
    );
    assert!(store4.load_history("s-empty-only").unwrap().is_empty());
    assert_eq!(
        store4.load_history_full("s-empty-only").unwrap().format,
        crate::core::sessions::HistoryFormat::New
    );
}

/// 🟡-4：历史没变（`noop`）时改名也要落进索引；**真·无变化**时一个索引写都不产生。
#[test]
fn noop_save_writes_renamed_title_but_no_change_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-rename";
    let ws = ["/ws".to_string()];
    let msgs = vec![Message::user_text("q")];
    store
        .save_history(id, "旧标题", ".", None, None, &ws, &msgs)
        .unwrap();
    assert_eq!(store.get(id).unwrap().title, "旧标题");

    // 历史逐字节不变 + 标题变了 → 段文件不动，但索引里的标题必须更新
    let before = read_all_segments(&store, id);
    let report = store
        .save_history(id, "新标题", ".", None, None, &ws, &msgs)
        .unwrap();
    assert!(report.saved && report.is_clean(), "{report:?}");
    assert_eq!(
        read_all_segments(&store, id),
        before,
        "历史没变就不该碰段文件"
    );
    assert_eq!(store.get(id).unwrap().title, "新标题", "改名必须落进索引");

    // 真·无变化（标题 / 模型都一致）→ 不产生任何索引写（同值跳过语义不破）
    let writes = store.index_write_count();
    store
        .save_history(id, "新标题", ".", None, None, &ws, &msgs)
        .unwrap();
    assert_eq!(store.index_write_count(), writes, "同值的保存不得重写索引");
}

/// 🟡-3：图片外置**失败**（走 `image_inline`）+ 无边车（冷启动回退全量扫描）时，
/// 两侧指纹必须仍然同源：前缀校验命中 → 一个字节都不写（不得退化成基线段）。
#[test]
fn inline_image_without_watermark_sidecar_keeps_prefix_check() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-img-inline";
    let ws = ["/ws".to_string()];
    // 让外置必定失败：blob 目录的位置先占成一个**文件**（create_dir_all 必失败）
    std::fs::create_dir_all(store.sessions_dir()).unwrap();
    std::fs::write(store.image_blobs_dir(id), b"not a dir").unwrap();
    let msgs = vec![Message::user_text("q"), image_msg("AAAA".into())];

    // 两侧指纹直接对拍（外置失败 ⇒ 落盘形态是 `image_inline`）
    let (persisted, referenced) = crate::core::sessions::persist::to_persisted(&store, id, &msgs);
    assert!(referenced.is_empty(), "外置失败 → 没有 blob 引用");
    assert_eq!(
        crate::core::sessions::segments::sig_messages(&msgs),
        crate::core::sessions::segments::sig_persisted(&persisted),
        "外置失败时两侧指纹不得分叉"
    );

    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    let raw = read_all_segments(&store, id);
    assert!(raw.contains("image_inline"), "落盘形态确实是内联：{raw}");

    // 冷启动（新 store 实例）+ 删掉水位边车 → 水位由**整目录扫描**推导
    let _ = std::fs::remove_file(
        store
            .history_dir(id)
            .join(crate::core::sessions::segments::META_FILE),
    );
    let cold = SessionStore::new(dir.path().to_path_buf());
    let before = segment_files(&store, id);
    let report = cold
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    assert!(report.saved && report.is_clean(), "{report:?}");
    assert_eq!(
        segment_files(&cold, id),
        before,
        "指纹同源 → 一个字节都不该写（更不得退化成基线段）"
    );
    assert_eq!(
        cold.load_history_full(id).unwrap().display.len(),
        msgs.len(),
        "display 不得因基线段而重复"
    );
}

/// 🟡-3：超单图上限的内联图片（本批之前的旧数据形态）两侧同口径（都按原文算）。
#[test]
fn oversize_inline_image_sig_agrees_across_forms() {
    let big = "a".repeat(crate::core::sessions::image_blobs::MAX_DATA_CHARS + 1);
    let memory = vec![Message {
        role: Role::User,
        content: vec![Content::Image {
            media_type: "image/png".into(),
            data: big.clone(),
        }],
        created_at: None,
    }];
    let persisted = vec![crate::core::sessions::persist::PersistedMessage {
        role: Role::User,
        content: vec![
            crate::core::sessions::persist::PersistedContent::ImageInline {
                media_type: "image/png".into(),
                data: big,
            },
        ],
        created_at: None,
    }];
    assert_eq!(
        crate::core::sessions::segments::sig_messages(&memory),
        crate::core::sessions::segments::sig_persisted(&persisted),
        "超限内联图片两侧同口径"
    );
}

/// §7.6 旧格式回落：无新目录 → 读 `.json.gz`（标记 legacy）；新旧并存 → **以新格式为准**。
#[test]
fn legacy_gz_fallback_and_new_format_wins_when_both_present() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    std::fs::create_dir_all(store.histories_dir()).unwrap();
    let json = r#"[{"role":"user","content":[{"type":"text","text":"旧内容"}]}]"#;
    std::fs::write(store.history_path("s-legacy-only"), gzip_bytes(json)).unwrap();

    let only = store.load_history_full("s-legacy-only").unwrap();
    assert_eq!(only.format, crate::core::sessions::HistoryFormat::Legacy);
    assert_eq!(only.wire.len(), 1);
    assert_eq!(only.wire[0].text_joined(), "旧内容");
    assert_eq!(only.display, only.wire, "旧格式 wire = display = 原结果");
    assert!(only.segments.is_empty());

    // 同一会话写入新格式（旧文件保留不动）→ 以新格式为准
    store
        .save_history(
            "s-legacy-only",
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("新内容")],
        )
        .unwrap();
    assert!(
        store.history_path("s-legacy-only").exists(),
        "旧文件不自动删（只在显式入口清理）"
    );
    let both = store.load_history_full("s-legacy-only").unwrap();
    assert_eq!(both.format, crate::core::sessions::HistoryFormat::New);
    assert_eq!(both.wire[0].text_joined(), "新内容");
}

/// §7.7 压缩边界：磁盘保留**完整转录**，压缩只记一条边界；
/// `display` 含压缩前的全部消息，`wire` 与「压缩后那份历史」逐字节一致。
#[test]
fn compaction_boundary_keeps_display_and_restores_wire() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-compact";
    let ws = ["/ws".to_string()];
    let original: Vec<Message> = (1..=4)
        .map(|i| Message::user_text(format!("m{i}")))
        .collect();
    store
        .save_history(id, "t", ".", None, None, &ws, &original)
        .unwrap();

    // 模拟 `context::compact_history`：历史被整体替换为「摘要首条 + 压缩前最后一条 user」
    let summary = format!("{HANDOFF_SUMMARY_PREFIX}摘要正文\n</handoff-summary>");
    let compacted = vec![Message::user_text(summary), original[3].clone()];
    let report = store
        .save_history(id, "t", ".", None, None, &ws, &compacted)
        .unwrap();
    assert!(report.is_clean(), "{report:?}");

    let loaded = store.load_history_full(id).unwrap();
    assert_eq!(
        loaded.display.len(),
        4,
        "display = 压缩前的完整转录（旧段保留）"
    );
    assert_eq!(
        loaded
            .display
            .iter()
            .map(|m| m.text_joined())
            .collect::<Vec<_>>(),
        vec!["m1", "m2", "m3", "m4"]
    );
    assert_eq!(loaded.wire.len(), 2, "wire = 边界 head（摘要 + 尾部）");
    assert_eq!(loaded.wire[0].text_joined(), compacted[0].text_joined());
    assert_eq!(loaded.wire[1].text_joined(), "m4");
    assert_eq!(loaded.boundaries.len(), 1, "压缩只记一条边界");
    assert_eq!(loaded.boundaries[0].source, "compact");
    assert_eq!(loaded.boundaries[0].head_messages, 2);

    // 压缩后继续对话 → wire = 边界 + 之后的消息（与内存那份历史一致）
    let mut after = compacted.clone();
    after.push(Message::user_text("m5"));
    store
        .save_history(id, "t", ".", None, None, &ws, &after)
        .unwrap();
    let loaded2 = store.load_history_full(id).unwrap();
    assert_eq!(loaded2.wire.len(), 3);
    assert_eq!(loaded2.wire[2].text_joined(), "m5");
    assert_eq!(
        loaded2.display.len(),
        5,
        "display = 旧段 4 条 + 压缩后新增 1 条"
    );
    assert_eq!(loaded2.display[4].text_joined(), "m5");
}

/// §7.10（AC-1，本批核心）：落盘路径**不再 trim**——超 256k token 预算的历史，
/// 旧轮次仍完整留在磁盘上（`display` 全量）；wire 侧照旧按预算裁剪（token/计费零变化）。
#[test]
fn disk_history_keeps_rounds_beyond_wire_budget() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-budget";
    // 每轮 ≈70k token（约 280k 字符），5 轮合计远超 256k 预算
    let filler = "x".repeat(280_000);
    let mut msgs: Vec<Message> = Vec::new();
    for i in 1..=5 {
        msgs.push(Message::user_text(format!("round{i} {filler}")));
        msgs.push(Message {
            role: Role::Assistant,
            content: vec![Content::Text {
                text: format!("ans{i}"),
            }],
            created_at: None,
        });
    }
    store
        .save_history(id, "t", ".", None, None, &["/ws".into()], &msgs)
        .unwrap();

    let loaded = store.load_history_full(id).unwrap();
    assert_eq!(
        loaded.display.len(),
        msgs.len(),
        "落盘不裁剪：内存里有就落盘（AC-1）"
    );
    assert!(
        loaded.display[0].text_joined().starts_with("round1"),
        "最早的轮次必须还在磁盘上（旧实现会在落盘时把它裁掉）"
    );
    // wire 仍守预算：被裁的轮次只是不进运行上下文
    assert!(
        loaded.wire.len() < msgs.len(),
        "wire 仍按 256k 预算裁剪（token/计费零变化）：{}",
        loaded.wire.len()
    );
    assert_eq!(loaded.wire.last().unwrap().text_joined(), "ans5");
}

/// §7.9 生命周期（store 侧）：`remove` 删掉段目录与旧格式文件，两者都不再残留。
#[test]
fn remove_drops_segment_dir_and_legacy_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-rm";
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &["/ws".into()],
            &[Message::user_text("q")],
        )
        .unwrap();
    std::fs::write(store.history_path(id), b"legacy").unwrap();
    assert!(store.history_dir(id).is_dir() && store.history_path(id).is_file());
    store.remove(id).unwrap();
    assert!(!store.history_dir(id).exists(), "段目录随会话删除");
    assert!(!store.history_path(id).exists(), "旧格式文件也一并删除");
}

/// §7.9 幽灵条目清扫同步删掉新格式段目录（否则 sub_*/task_* 的段目录永久泄漏）。
#[test]
fn purge_non_session_entries_removes_segment_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    store.upsert_meta(meta("sub_ghost2")).unwrap();
    std::fs::create_dir_all(store.history_dir("sub_ghost2")).unwrap();
    std::fs::write(store.history_dir("sub_ghost2").join("0001.jsonl"), "x").unwrap();
    assert_eq!(store.purge_non_session_entries(), 1);
    assert!(!store.history_dir("sub_ghost2").exists());
}

/// 子代理过程历史同待遇：走同一套段式模块（增量追加 + 基线段），不留第二套上限逻辑。
#[test]
fn sub_history_uses_segments_and_is_append_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let parent = "parent-seg";
    let mut second = vec![Message::user_text("s1")];
    store.save_sub_history(parent, "sub_x", &second).unwrap();
    let before = std::fs::read(store.sub_history_dir(parent, "sub_x").join("0001.jsonl")).unwrap();
    second.push(Message::user_text("s2"));
    store.save_sub_history(parent, "sub_x", &second).unwrap();
    let after = std::fs::read(store.sub_history_dir(parent, "sub_x").join("0001.jsonl")).unwrap();
    assert!(after.starts_with(&before), "子历史同样只追加");
    assert_eq!(store.load_sub_history(parent, "sub_x").unwrap().len(), 2);
    // 段目录形态也随父会话级联删除
    store.remove(parent).unwrap();
    assert!(!store.sub_histories_dir(parent).exists());
}

// ---------- P4：磁盘约束（软告警 / 硬熔断 / 自愈）与可见性 ----------
//
// 守护的不变量：① 超软线**照常写**，只多一条可见状态；② 超硬线**停止 append**，
// 但**绝不删任何既有段 / blob**（既有字节逐字节不变）；③ 体积回落后状态**自愈**；
// ④ `is_clean()` 必须把新状态盖住（`drive.rs` 靠它决定要不要上报）；⑤ 子代理路径不受熔断影响。

/// 阈值裁决是纯函数：三条分支逐条断言（含**自愈**分支 = 低于软线即 `None`）。
#[test]
fn size_status_three_branches_including_heal() {
    let lim = HistoryLimits {
        soft: 1000,
        hard: 5000,
    };
    let at = "2026-09-24T00:00:00+00:00";

    // 低于软线 → None（状态不留痕；自愈判定本身也走这条）
    assert_eq!(size_status(0, lim, at), None);
    assert_eq!(size_status(999, lim, at), None);
    // 恰好软线 → Warned（照常写）
    assert_eq!(
        size_status(1000, lim, at),
        Some(HistoryStatus::Warned {
            bytes: 1000,
            threshold: 1000,
            at: at.into()
        })
    );
    // 两线之间 → 仍是 Warned（阈值仍是软线）
    assert!(matches!(
        size_status(4999, lim, at),
        Some(HistoryStatus::Warned {
            bytes: 4999,
            threshold: 1000,
            ..
        })
    ));
    // 恰好硬线 → Fused（阈值是硬线）
    assert_eq!(
        size_status(5000, lim, at),
        Some(HistoryStatus::Fused {
            bytes: 5000,
            threshold: 5000,
            at: at.into()
        })
    );
}

/// `is_clean()` 是「新状态能不能发出去」的总闸（`drive.rs` 用 `filter(|r| !r.is_clean())`）：
/// 三个非干净变体逐一断言，并钉住 wire 上的 `kind` 名（前端按它分派文案）。
#[test]
fn is_clean_covers_all_status_variants() {
    let at = "2026-09-24T00:00:00+00:00".to_string();
    let clean = SaveReport {
        saved: true,
        stripped_images: 0,
        dropped_rounds: 0,
        bytes: 1,
        history_status: None,
    };
    assert!(clean.is_clean(), "无状态 = 干净（前端零打扰）");

    for (st, kind) in [
        (
            HistoryStatus::Warned {
                bytes: 1,
                threshold: 1,
                at: at.clone(),
            },
            "warned",
        ),
        (
            HistoryStatus::Fused {
                bytes: 1,
                threshold: 1,
                at: at.clone(),
            },
            "fused",
        ),
        (
            HistoryStatus::Degraded {
                stripped_images: 0,
                dropped_rounds: 0,
                at: at.clone(),
            },
            "degraded",
        ),
    ] {
        let r = SaveReport {
            history_status: Some(st),
            ..clean.clone()
        };
        assert!(!r.is_clean(), "有状态即不干净（必须进上传载荷）：{r:?}");
        assert_eq!(
            serde_json::to_value(&r).unwrap()["history_status"]["kind"],
            serde_json::json!(kind),
            "wire 上的 kind 名"
        );
    }

    // 拒存仍是干净的反面，且不带体积状态
    assert!(!SaveReport::rejected().is_clean());
    assert!(SaveReport::rejected().history_status.is_none());
}

/// 软线：**照常写盘**（内容完整、后续仍可追加）+ 状态 `Warned` 挂索引（重启后可见）。
#[test]
fn soft_line_warns_but_keeps_writing() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf()).with_history_limits(HistoryLimits {
        soft: 4096,
        hard: u64::MAX,
    });
    let id = "s-soft";
    let ws = ["/ws".to_string()];
    let big = incompressible_b64(64 * 1024);
    let report = store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text(big.clone())],
        )
        .unwrap();

    let total = store.session_history_bytes(id);
    assert!(total >= 4096, "前置条件：已越过软线：{total}");
    assert!(report.saved, "软线不是失败：{report:?}");
    assert!(!report.is_clean(), "软告警必须进载荷：{report:?}");
    assert!(
        matches!(
            report.history_status,
            Some(HistoryStatus::Warned {
                bytes,
                threshold: 4096,
                ..
            }) if bytes == total
        ),
        "报告要如实带体积与阈值：{report:?}"
    );
    // 照常写盘：内容完整可读
    assert_eq!(store.load_history(id).unwrap().len(), 1);

    // 状态挂索引（重启后仍可见的载体），且下一次保存仍照常追加（不因告警而停写）
    assert!(matches!(
        store.get(id).unwrap().history_status,
        Some(HistoryStatus::Warned { .. })
    ));
    store
        .save_history(
            id,
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text(big), Message::user_text("第二轮")],
        )
        .unwrap();
    assert_eq!(
        store.load_history(id).unwrap().len(),
        2,
        "软告警期间照常追加（数据完整性优先于体积）"
    );
    assert!(store.session_history_bytes(id) > total);
}

/// 硬线：停止 append——段文件清单、字节、可读条数、总体积全部不变（**连一字节都不删**）；
/// 状态 `Fused` 如实上报并挂索引（否则重启后提示就没了）。
#[test]
fn hard_line_fuses_without_losing_any_history() {
    let dir = tempfile::tempdir().unwrap();
    // 常量而非字面量表达式：`matches!` 的模式里不允许算术表达式
    const SOFT: u64 = 4096;
    const HARD: u64 = 16 * 1024;
    let store = SessionStore::new(dir.path().to_path_buf()).with_history_limits(HistoryLimits {
        soft: SOFT,
        hard: HARD,
    });
    let id = "s-fuse";
    let ws = ["/ws".to_string()];

    // ① 首次保存：远低于硬线 → 正常落盘
    store
        .save_history(id, "t", ".", None, None, &ws, &[Message::user_text("one")])
        .unwrap();
    assert!(store.get(id).unwrap().history_status.is_none());

    // ② 第二次保存：预判仍在硬线之下（照常写），写后量得已超线 → Fused
    let big = incompressible_b64(256 * 1024);
    let written_msgs = vec![Message::user_text("one"), Message::user_text(big.clone())];
    let r2 = store
        .save_history(id, "t", ".", None, None, &ws, &written_msgs)
        .unwrap();
    let total = store.session_history_bytes(id);
    assert!(total >= HARD, "前置条件：已越过硬线：{total}");
    assert!(
        matches!(r2.history_status, Some(HistoryStatus::Fused { .. })),
        "越线后本次保存即落 Fused：{r2:?}"
    );

    // 快照：段文件清单 + 逐字节内容 + 可读条数
    let files = segment_files(&store, id);
    let snap: Vec<Vec<u8>> = files.iter().map(|f| segment_bytes(&store, id, f)).collect();
    let display_before = store.load_history_full(id).unwrap().display.len();

    // ③ 熔断：内存里多了一条，但本次**一个字节也不写**
    let mut grown = written_msgs.clone();
    grown.push(Message::user_text("three"));
    let report = store
        .save_history(id, "t", ".", None, None, &ws, &grown)
        .unwrap();

    assert!(report.saved, "熔断不是失败：{report:?}");
    assert!(!report.is_clean(), "熔断必须进载荷（否则前端看不到提示）");
    assert!(matches!(
        report.history_status,
        Some(HistoryStatus::Fused {
            bytes,
            threshold: HARD,
            ..
        }) if bytes == total
    ));
    assert_eq!(
        segment_files(&store, id),
        files,
        "熔断不得新增 / 删除任何段文件"
    );
    assert_eq!(
        files
            .iter()
            .map(|f| segment_bytes(&store, id, f))
            .collect::<Vec<_>>(),
        snap,
        "既有段逐字节未变（只停写，绝不删数据）"
    );
    assert_eq!(
        store.load_history_full(id).unwrap().display.len(),
        display_before,
        "熔断期间历史不再增长（新消息未落盘）"
    );
    assert_eq!(store.session_history_bytes(id), total, "熔断期间总体积不变");
    // 状态挂索引：重启后仍能提示
    assert!(matches!(
        store.get(id).unwrap().history_status,
        Some(HistoryStatus::Fused { .. })
    ));
}

/// 自愈：体积回落（外部清理 / 手工删段模拟）到软线之下后，下一次保存把状态清成 `None`，
/// 报告重新变干净（前端不再挂提示）。用**新 store**（冷启动）模拟重启后的第一次保存。
#[test]
fn size_drop_clears_status() {
    let dir = tempfile::tempdir().unwrap();
    let limits = HistoryLimits {
        soft: 4096,
        hard: 16 * 1024,
    };
    let id = "s-heal-fuse";
    let ws = ["/ws".to_string()];
    {
        let store = SessionStore::new(dir.path().to_path_buf()).with_history_limits(limits);
        store
            .save_history(
                id,
                "t",
                ".",
                None,
                None,
                &ws,
                &[Message::user_text(incompressible_b64(256 * 1024))],
            )
            .unwrap();
        assert!(matches!(
            store.get(id).unwrap().history_status,
            Some(HistoryStatus::Fused { .. })
        ));
        // 模拟外部回落：段文件被删（P5 旧格式清理 / 手工删段），总量变小
        for f in segment_files(&store, id) {
            std::fs::remove_file(store.history_dir(id).join(f)).unwrap();
        }
    }

    let store = SessionStore::new(dir.path().to_path_buf()).with_history_limits(limits);
    let report = store
        .save_history(id, "t", ".", None, None, &ws, &[Message::user_text("短")])
        .unwrap();
    assert!(
        store.session_history_bytes(id) < limits.soft,
        "前置条件：已回落到软线之下"
    );
    assert!(report.is_clean(), "回落后报告必须重新干净：{report:?}");
    assert_eq!(
        store.get(id).unwrap().history_status,
        None,
        "回落后必须自愈清除状态（否则提示永远挂着）"
    );
}

/// 子代理路径**有意不熔断**（无 UI 载体，停写无人可见）：主路径已熔断时子历史仍照常落盘，
/// 也不拒存；它的「不干净」仍只由剥图决定（与 P1 判据等价）。
#[test]
fn sub_history_ignores_fuse_line_purposely() {
    let dir = tempfile::tempdir().unwrap();
    // 任何非零体积都超线：主路径必然熔断
    let store = SessionStore::new(dir.path().to_path_buf())
        .with_history_limits(HistoryLimits { soft: 1, hard: 1 });
    let ws = ["/ws".to_string()];

    // 主路径：第二次保存即熔断（不写段）
    store
        .save_history(
            "s-main",
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("a")],
        )
        .unwrap();
    let files = segment_files(&store, "s-main");
    store
        .save_history(
            "s-main",
            "t",
            ".",
            None,
            None,
            &ws,
            &[Message::user_text("a"), Message::user_text("b")],
        )
        .unwrap();
    assert_eq!(
        segment_files(&store, "s-main"),
        files,
        "主路径已熔断：不再写段"
    );

    // 子路径：同一把锁、同一套段模块，但不做体积裁决
    let parent = "parent-fuse";
    let mut msgs = vec![Message::user_text("子代理过程 1")];
    let r1 = store.save_sub_history(parent, "sub_a", &msgs).unwrap();
    assert!(r1.saved && r1.is_clean(), "子历史不熔断也不降级：{r1:?}");
    msgs.push(Message::user_text("子代理过程 2"));
    let r2 = store.save_sub_history(parent, "sub_a", &msgs).unwrap();
    assert!(r2.saved && r2.is_clean(), "{r2:?}");
    assert_eq!(
        store.load_sub_history(parent, "sub_a").unwrap().len(),
        2,
        "子历史照常增长（无 UI 载体故不熔断）"
    );
}

// ---------- 有界装载（批2 回归修复：打开 / 恢复会话不再 O(全部历史字节)） ----------
//
// 落盘取消 8MB 上限后，「整份回放」变成 O(全部历史) 的读取，与本批要保住的「大会话秒开」
// 直接冲突。这里钉死两件事：**有界读到的 wire 与全量扫描逐条一致**，以及**读取量确实有界**
//（冷启动水位推导同理）。

/// 造一条「每轮即一段」的大历史：每轮 ≈`chars` 字节（> [`SEGMENT_MAX_BYTES`] → 单轮自封一段）。
fn fat_history(rounds: usize, chars: usize) -> Vec<Message> {
    let filler = "x".repeat(chars);
    let mut msgs: Vec<Message> = Vec::new();
    for i in 1..=rounds {
        msgs.push(Message::user_text(format!("round{i} {filler}")));
        msgs.push(Message {
            role: Role::Assistant,
            content: vec![Content::Text {
                text: format!("ans{i}"),
            }],
            created_at: None,
        });
    }
    msgs
}

/// 测试用：段目录里所有段文件的字节总和。
fn history_segment_bytes(store: &SessionStore, id: &str) -> u64 {
    segment_files(store, id)
        .iter()
        .map(|f| segment_bytes(store, id, f).len() as u64)
        .sum()
}

/// 对拍 + 量化：有界装载的 wire == 全量扫描的 wire；且只读了尾部少量段。
#[test]
fn bounded_wire_matches_full_scan_and_reads_far_less() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-bounded";
    let ws = ["/ws".to_string()];
    let msgs = fat_history(12, 700_000);
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();

    let files = segment_files(&store, id);
    let total_bytes = history_segment_bytes(&store, id);
    assert!(
        files.len() >= 10,
        "本用例要求足够多的段（实际 {}）",
        files.len()
    );

    // 基准：全量路径（display + ledger 整份回放）
    let full = store.load_history_full(id).unwrap();
    assert_eq!(full.display.len(), msgs.len(), "display 仍是全量语义");

    // 有界路径：只从段尾向前读
    crate::core::sessions::segments::reset_read_stats();
    let bounded = store.load_history(id).unwrap();
    let stats = crate::core::sessions::segments::read_stats();

    assert_eq!(bounded, full.wire, "有界 wire 必须与全量扫描逐条逐字节一致");
    println!(
        "有界 wire：读 {} / {total_bytes} 字节（{:.1}%）、{} / {} 段",
        stats.full_bytes,
        stats.full_bytes as f64 * 100.0 / total_bytes as f64,
        stats.full_segments,
        files.len()
    );
    assert_eq!(
        bounded.last().unwrap().text_joined(),
        "ans12",
        "窗口右端就是时间线末尾"
    );
    assert!(
        stats.full_segments * 2 < files.len() as u64,
        "读的段数必须远小于全量：读了 {} / {} 段",
        stats.full_segments,
        files.len()
    );
    assert!(
        stats.full_bytes * 2 < total_bytes,
        "读取字节必须远小于全量：读了 {} / {total_bytes} 字节",
        stats.full_bytes
    );
    assert_eq!(
        stats.window_bytes, 0,
        "wire 装载不该依赖段头/段尾窗口（那是分页元信息的路径）"
    );
}

/// 水位推导对拍：有界边车路径 == 全量扫描——尤其 `blobs` 并集（漏一个就是不可逆的图丢失）。
#[test]
fn bounded_watermark_derivation_matches_full_scan() {
    use crate::core::sessions::segments;
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-watermark";
    let ws = ["/ws".to_string()];
    // ① 250 条消息（按条数封口 → 跨段）+ 一张外置图片（blob 并集）
    let mut msgs: Vec<Message> = (1..=250)
        .map(|i| Message::user_text(format!("m{i}")))
        .collect();
    msgs.push(image_msg("AAAA".into()));
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    // ② 增量追加
    msgs.push(Message::user_text("tail"));
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    // ③ 前缀缩短（上下文压缩）→ 基线段 + compaction 边界；旧段仍引用那张图
    let summary = format!("{HANDOFF_SUMMARY_PREFIX}摘要\n</handoff-summary>");
    let compacted = vec![Message::user_text(summary), msgs[msgs.len() - 1].clone()];
    store
        .save_history(id, "t", ".", None, None, &ws, &compacted)
        .unwrap();

    let d = store.history_dir(id);
    let bounded = segments::state_from_meta(&d).expect("边车应可用（刚由保存路径写下）");
    let scanned = segments::scan(&d, false).state;
    assert_eq!(bounded.written, scanned.written, "水位条数");
    assert_eq!(bounded.sig, scanned.sig, "水位指纹");
    assert_eq!(bounded.next_seq, scanned.next_seq);

    let mut a: Vec<String> = bounded.blobs.iter().cloned().collect();
    let mut b: Vec<String> = scanned.blobs.iter().cloned().collect();
    a.sort();
    b.sort();
    assert_eq!(a.len(), 1, "本用例应有一张外置图");
    assert_eq!(
        a, b,
        "blob 并集必须与全量扫描一致（它决定图 GC 会不会误删旧段仍在引用的图）"
    );

    let (ta, tb) = (
        bounded.open_tail.as_ref().expect("末段未封口"),
        scanned.open_tail.as_ref().expect("末段未封口"),
    );
    assert_eq!(
        (ta.seq, ta.base, ta.messages, ta.bytes, ta.pending.clone()),
        (tb.seq, tb.base, tb.messages, tb.bytes, tb.pending.clone())
    );

    // 边车失效（尾部多出半行 = 崩溃残留）→ 必须判不可用并回退全量扫描
    let last = segment_files(&store, id).pop().unwrap();
    let p = store.history_dir(id).join(&last);
    let mut raw = std::fs::read(&p).unwrap();
    raw.extend_from_slice(b"{\"kind\":\"message\",\"msg\":{\"rol");
    std::fs::write(&p, raw).unwrap();
    assert!(
        segments::state_from_meta(&d).is_none(),
        "字节数变了必须回退（拿旧水位去比前缀会写成基线段）"
    );
    assert_eq!(
        SessionStore::derive_watermark(&d).written,
        segments::scan(&d, false).state.written,
        "回退路径仍与全量扫描同值"
    );
}

/// 冷启动（新进程语义：全新实例、水位缓存为空）必须**有界**，且前缀指纹吻合时只追加新消息。
#[test]
fn cold_start_uses_bounded_watermark_and_appends_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let id = "s-cold";
    let ws = ["/ws".to_string()];
    let mut msgs = fat_history(12, 700_000);
    SessionStore::new(dir.path().to_path_buf())
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();

    // 全新实例 = 冷启动（水位缓存为空，只能靠边车或整目录扫描推导）
    let store = SessionStore::new(dir.path().to_path_buf());
    let total_bytes = history_segment_bytes(&store, id);
    msgs.push(Message::user_text("restart tail"));
    crate::core::sessions::segments::reset_read_stats();
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();
    let stats = crate::core::sessions::segments::read_stats();
    println!(
        "冷启动水位推导：读 {} / {total_bytes} 字节（{:.1}%）",
        stats.full_bytes,
        stats.full_bytes as f64 * 100.0 / total_bytes as f64
    );

    assert!(
        stats.full_bytes * 2 < total_bytes,
        "冷启动水位推导必须有界：读了 {} / {total_bytes} 字节",
        stats.full_bytes
    );
    assert!(
        history_segment_bytes(&store, id) < total_bytes * 3 / 2,
        "水位命中 ⇒ 只追加新增消息（写基线段会把整份历史重写一遍）"
    );
    assert_eq!(
        store
            .load_history(id)
            .unwrap()
            .last()
            .unwrap()
            .text_joined(),
        "restart tail",
        "冷启动后的 wire 仍以时间线末尾收尾"
    );
}

/// 时间线**开头不是 User**（如中断残留的孤儿 tool_result：`repair` 会清空内容，但消息本身留在
/// 开头）且只有两轮时，绝不能靠「裁到最老的 User」提前收尾：全量那份 `trim` 此时一轮都不丢，
/// 会**保留**开头那条非 User 消息，而裁过的窗口已经把它丢了（两侧就此分叉）。
#[test]
fn bounded_wire_keeps_leading_non_user_message_on_two_round_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().to_path_buf());
    let id = "s-leading";
    let ws = ["/ws".to_string()];
    let filler = "x".repeat(600_000);
    let msgs = vec![
        Message::tool_results(vec![Content::ToolResult {
            tool_use_id: "gone".into(),
            content: "r".into(),
            is_error: true,
        }]),
        Message::user_text(format!("round1 {filler}")),
        Message {
            role: Role::Assistant,
            content: vec![Content::Text { text: "a1".into() }],
            created_at: None,
        },
        Message::user_text(format!("round2 {filler}")),
        Message {
            role: Role::Assistant,
            content: vec![Content::Text { text: "a2".into() }],
            created_at: None,
        },
    ];
    store
        .save_history(id, "t", ".", None, None, &ws, &msgs)
        .unwrap();

    let full = store.load_history_full(id).unwrap();
    assert_eq!(
        full.display[0].role,
        Role::Tool,
        "前置条件：时间线开头不是 User"
    );
    assert!(
        full.wire.len() >= 4,
        "两轮都在 wire 里（一轮都没丢）：{}",
        full.wire.len()
    );
    let bounded = store.load_history(id).unwrap();
    assert_eq!(
        bounded, full.wire,
        "两轮时间线 + 开头非 User：只能读到底，不得按最老的 User 截断"
    );
}
