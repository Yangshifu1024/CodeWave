//! 会话中断标记与崩溃恢复（会话保存与恢复优化 · 批1，需求共识 20/23）。
//!
//! 两条时间线：
//! 1. **索引标记**：会话的 `running` / `interrupted` 落在 `sessions/index.json` 的 SessionMeta 上，
//!    run 开始置 `running`、收尾清除（见 `InterruptWatchSink`），非正常退出遗留的 `running`
//!    在下次启动时转成 `interrupted { kind: "crash" }`。
//! 2. **进程运行标记**：`<data_dir>/running.marker` 启动时创建、正常退出时删除；下次启动只要
//!    见到它，就说明上次是崩溃/强杀（正常退出路径一定删了它）。
//!
//! 分层规则：core 不依赖 tauri——run 收尾靠装饰 `EventSink`（事件面本身不变），不侵入 agent 层。

use super::store::SessionStore;
use crate::core::agent::{EventSink, Frame};
use crate::core::types::SessionId;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 进程运行标记文件名（位于数据根目录）。
pub const MARKER_FILE: &str = "running.marker";
/// 中断类型：进程崩溃/被强杀（下次启动见到 marker）。
pub const KIND_CRASH: &str = "crash";
/// 中断类型：正常退出但用户在跑的 run 被中断（「中断并保存后退出」/ 立即退出）。
pub const KIND_QUIT: &str = "quit";

/// run 收尾事件面（与 agent 层发出的键名一一对应：成功 / 失败 / 取消）。
const RUN_END_EVENTS: [&str; 3] = ["run:done", "run:error", "run:cancelled"];

// ---------- 进程运行标记 ----------

/// 运行标记文件路径。
pub fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MARKER_FILE)
}

/// 运行标记是否存在（= 上次退出是否非正常）。
pub fn marker_exists(data_dir: &Path) -> bool {
    marker_path(data_dir).exists()
}

/// 创建运行标记（启动时调用；原子写，失败向上抛——崩溃检测失效必须可见）。
pub fn create_marker(data_dir: &Path) -> std::io::Result<()> {
    crate::util::atomic::atomic_write(&marker_path(data_dir), b"running")
}

/// 删除运行标记（正常退出收尾；缺失视为已删除，幂等）。
pub fn remove_marker(data_dir: &Path) {
    match std::fs::remove_file(marker_path(data_dir)) {
        Ok(()) => tracing::info!("运行标记已删除（本次为正常退出）"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("运行标记删除失败（下次启动会按崩溃处理）：{e}"),
    }
}

// ---------- 崩溃恢复 / 退出中断 ----------

/// 崩溃恢复：启动时若 marker 存在 ⇒ 上次非正常退出。
/// 对所有 `running == true` 的会话写 `interrupted { kind: "crash" }` 并清 `running`，
/// 返回处理条数；marker 不存在（正常退出）→ 0，不动索引。
pub fn recover_after_crash(store: &SessionStore, data_dir: &Path) -> usize {
    if !marker_exists(data_dir) {
        return 0;
    }
    let at = chrono::Utc::now().to_rfc3339();
    let ids = store.running_ids();
    let n = store.mark_interrupted_batch(&ids, KIND_CRASH, &at);
    if n > 0 {
        tracing::warn!("检测到上次非正常退出（marker 残留）：{n} 个会话已标记为中断（crash）");
    }
    n
}

/// 正常退出时的中断落盘：给定会话写 `interrupted { kind: "quit" }` 并清 `running`，返回命中条数。
pub fn mark_quit_interrupted(store: &SessionStore, ids: &[String]) -> usize {
    let at = chrono::Utc::now().to_rfc3339();
    store.mark_interrupted_batch(ids, KIND_QUIT, &at)
}

// ---------- run 收尾观察 ----------

/// 事件汇装饰器：把「会话已不在跑」回落到索引的 `running` 字段。
///
/// run 生命周期在 agent 层（`drive.rs` 收尾复位 `running` 原子量并发出 run:done / run:error /
/// run:cancelled），而标记落盘属持久化层——用装饰器在 host 装配处收口，避免持久化细节侵入
/// agent 主循环。装饰器只补旁路动作，帧与事件按原样透传（前端契约面不变）。
pub struct InterruptWatchSink {
    /// 被装饰的事件汇（TauriSink；测试里为记录用 sink）
    inner: Arc<dyn EventSink>,
    /// 会话存储（标记落盘）
    store: Arc<SessionStore>,
}

impl InterruptWatchSink {
    /// 构造装饰器。
    pub fn new(inner: Arc<dyn EventSink>, store: Arc<SessionStore>) -> Self {
        InterruptWatchSink { inner, store }
    }
}

impl EventSink for InterruptWatchSink {
    fn channel_frame(&self, session: &SessionId, frame: &Frame) {
        self.inner.channel_frame(session, frame);
    }

    /// run 收尾事件 → 清索引 `running`（子代理/任务运行不在索引中：mark_running 命中不到条目，
    /// 保持 no-op）；其余事件一律只在下方透传。
    fn emit(&self, session: &SessionId, event: &str, payload: serde_json::Value) {
        if RUN_END_EVENTS.contains(&event) {
            match self.store.mark_running(session, false) {
                Ok(_) => {}
                Err(e) => tracing::warn!("会话 [{session}] 收尾清 running 失败：{e}"),
            }
        }
        self.inner.emit(session, event, payload);
    }

    fn bind_sub_channel(&self, parent: &SessionId, sub: &SessionId) {
        self.inner.bind_sub_channel(parent, sub);
    }

    fn unbind_sub_channel(&self, sub: &SessionId) {
        self.inner.unbind_sub_channel(sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sessions::SessionMeta;
    use std::sync::Mutex;

    /// 记录事件用 sink（保持调用顺序，验证透传）。
    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<(String, String)>>,
    }

    impl EventSink for RecordingSink {
        fn channel_frame(&self, _s: &SessionId, _f: &Frame) {}
        fn emit(&self, s: &SessionId, e: &str, _p: serde_json::Value) {
            self.events.lock().unwrap().push((s.clone(), e.to_string()));
        }
    }

    fn store(dir: &Path) -> Arc<SessionStore> {
        Arc::new(SessionStore::new(dir.to_path_buf()))
    }

    fn meta(id: &str) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            title: format!("t-{id}"),
            workspace: ".".into(),
            model_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            message_count: 0,
            project_id: None,
            roots: vec!["/ws".into()],
            running: false,
            interrupted: None,
            last_opened_at: None,
            history_status: None,
        }
    }

    #[test]
    fn marker_lifecycle() {
        let d = tempfile::tempdir().unwrap();
        assert!(!marker_exists(d.path()));
        create_marker(d.path()).unwrap();
        assert!(marker_exists(d.path()));
        remove_marker(d.path());
        assert!(!marker_exists(d.path()));
        // 幂等：重复删除不报错
        remove_marker(d.path());
    }

    #[test]
    fn crash_recovery_marks_running_sessions_only_when_marker_present() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        st.upsert_meta(meta("a")).unwrap();
        st.upsert_meta(meta("b")).unwrap();
        st.mark_running("a", true).unwrap();

        // 正常退出（无 marker）：不动任何标记
        assert_eq!(recover_after_crash(&st, d.path()), 0);
        assert!(st.get("a").unwrap().running);
        assert!(st.get("a").unwrap().interrupted.is_none());

        // 崩溃（marker 残留）：running 会话转中断标记并清 running；未跑的会话保持干净
        create_marker(d.path()).unwrap();
        assert_eq!(recover_after_crash(&st, d.path()), 1);
        let a = st.get("a").unwrap();
        assert!(!a.running);
        let info = a.interrupted.expect("崩溃会话必须有中断标记");
        assert_eq!(info.kind, KIND_CRASH);
        assert!(!info.at.is_empty());
        assert!(st.get("b").unwrap().interrupted.is_none());
    }

    #[test]
    fn quit_interrupt_batch_marks_and_clears() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        st.upsert_meta(meta("a")).unwrap();
        st.upsert_meta(meta("b")).unwrap();
        st.mark_running("a", true).unwrap();
        st.mark_running("b", true).unwrap();

        let n = mark_quit_interrupted(&st, &["a".to_string(), "b".to_string()]);
        assert_eq!(n, 2);
        for id in ["a", "b"] {
            let m = st.get(id).unwrap();
            assert!(!m.running);
            assert_eq!(m.interrupted.unwrap().kind, KIND_QUIT);
        }
        // 缺席会话跳过（子代理/任务运行）
        assert_eq!(mark_quit_interrupted(&st, &["ghost".to_string()]), 0);
    }

    #[test]
    fn watch_sink_clears_running_on_run_end_events_and_passes_events_through() {
        let d = tempfile::tempdir().unwrap();
        let st = store(d.path());
        st.upsert_meta(meta("s1")).unwrap();
        st.mark_running("s1", true).unwrap();
        let inner = Arc::new(RecordingSink::default());
        let sink = InterruptWatchSink::new(inner.clone(), st.clone());
        let s1: SessionId = "s1".into();

        // 非收尾事件：running 保持（用真实存在的事件键，避免污染前端事件契约扫描）
        sink.emit(&s1, "tokens:update", serde_json::json!({}));
        assert!(st.get("s1").unwrap().running);
        // 收尾事件：running 清除
        for ev in RUN_END_EVENTS {
            st.mark_running("s1", true).unwrap();
            sink.emit(&s1, ev, serde_json::json!({}));
            assert!(!st.get("s1").unwrap().running, "事件 {ev} 应收尾清 running");
        }
        // 事件透传（顺序与内容不变）
        let events: Vec<String> = inner
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|(s, e)| format!("{s}:{e}"))
            .collect();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0], "s1:tokens:update");
        assert_eq!(events[3], "s1:run:cancelled");

        // 索引中不存在的会话（子代理/任务运行）：不得凭空造条目
        sink.emit(
            &"sub_deadbeef".to_string(),
            "run:done",
            serde_json::json!({}),
        );
        assert!(st.get("sub_deadbeef").is_none());
    }
}
