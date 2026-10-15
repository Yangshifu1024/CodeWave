//! 只用于界面恢复的 run 边界与耗时；不进入转录或 provider wire。
//! 每轮单文件，文件名含时间范围，分页只打开与本页相交的记录。
use super::{SessionStore, cleanup::is_safe_session_id};
use crate::core::types::Message;
use serde::{Deserialize, Serialize};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunDisplayStatus {
    Done,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDisplay {
    pub run_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    pub status: RunDisplayStatus,
    pub user_created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_answer_created_at: Option<String>,
}

fn millis(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

pub(super) fn save(store: &SessionStore, id: &str, run: &RunDisplay) -> anyhow::Result<()> {
    let from = millis(&run.user_created_at).ok_or_else(|| anyhow::anyhow!("无效的开始时间"))?;
    let to = millis(&run.finished_at).ok_or_else(|| anyhow::anyhow!("无效的结束时间"))?;
    let bytes = serde_json::to_vec(run)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_FILE_BYTES, "任务展示元数据过大");
    // UUID/任意 provider run id 都不作为路径片段：hash 保留完整身份且避免截断碰撞。
    use sha2::{Digest, Sha256};
    let key: String = Sha256::digest(run.run_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = store.task_runs_dir(id);
    std::fs::create_dir_all(&dir)?;
    Ok(crate::util::atomic::atomic_write(
        &dir.join(format!("{from}_{to}_{key}.json")),
        &bytes,
    )?)
}

/// 只读本页时间范围相交的文件内容。目录项枚举随 run 数量增长，不读取其它历史或 run 正文。
/// 缺失、损坏、过大、符号链接均跳过，恢复增强失败不能阻止打开转录。
pub fn load_for_messages(store: &SessionStore, id: &str, messages: &[Message]) -> Vec<RunDisplay> {
    if !is_safe_session_id(id) {
        return Vec::new();
    }
    let mut times = messages
        .iter()
        .filter_map(|m| millis(m.created_at.as_deref()?));
    let Some(first) = times.next() else {
        return Vec::new();
    };
    let (min, max) = times.fold((first, first), |(min, max), at| (min.min(at), max.max(at)));
    let dir = store.task_runs_dir(id);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut runs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let mut parts = name.split('_');
        let (Some(from), Some(to)) = (
            parts.next().and_then(|n| n.parse::<i64>().ok()),
            parts.next().and_then(|n| n.parse::<i64>().ok()),
        ) else {
            continue;
        };
        if from > max || to < min || !name.ends_with(".json") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(run) = serde_json::from_slice::<RunDisplay>(&bytes) else {
            continue;
        };
        if millis(&run.user_created_at) == Some(from) && millis(&run.finished_at) == Some(to) {
            runs.push(run);
        }
    }
    runs.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then(a.run_id.cmp(&b.run_id))
    });
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn record(run_id: &str, from: &str, to: &str) -> RunDisplay {
        RunDisplay {
            run_id: run_id.into(),
            started_at: from.into(),
            finished_at: to.into(),
            user_created_at: from.into(),
            duration_ms: 120_000,
            status: RunDisplayStatus::Done,
            final_answer: Some("完成".into()),
            final_answer_created_at: Some(to.into()),
        }
    }

    fn message(at: &str) -> Message {
        let mut m = Message::user_text("页内消息");
        m.created_at = Some(at.into());
        m
    }

    #[test]
    fn run_display_page_intersection_and_corrupt_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_path_buf());
        let start = "2026-10-15T01:00:00Z";
        let end = "2026-10-15T01:02:00Z";
        let run = record("run/with/unsafe/path", start, end);
        save(&store, "s", &run).unwrap();
        save(
            &store,
            "s",
            &record("old", "2026-10-14T01:00:00Z", "2026-10-14T01:02:00Z"),
        )
        .unwrap();
        let page = [message("2026-10-15T01:01:00Z")];
        let back = load_for_messages(&store, "s", &page);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].run_id, run.run_id);
        assert_eq!(back[0].duration_ms, 120_000);
        assert!(load_for_messages(&store, "s", &[]).is_empty());
        assert!(load_for_messages(&store, "../s", &page).is_empty());
        let prefix = format!("{}_", millis(start).unwrap());
        let path = std::fs::read_dir(store.task_runs_dir("s"))
            .unwrap()
            .flatten()
            .find(|p| p.file_name().to_str().unwrap().starts_with(&prefix))
            .unwrap()
            .path();
        std::fs::write(path, b"broken json").unwrap();
        assert!(load_for_messages(&store, "s", &page).is_empty());
    }

    #[test]
    fn run_display_zombie_and_delete_do_not_recreate_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_path_buf());
        let from = "2026-10-15T01:00:00Z";
        let run = record("r", from, "2026-10-15T01:02:00Z");
        store.save_task_run("s", &run, &AtomicBool::new(false));
        assert!(
            !store.task_runs_dir("s").exists(),
            "无索引的迟到 run 不得创建目录"
        );
        store
            .save_history("s", "s", "/tmp", None, None, &[], &[message(from)])
            .unwrap();
        store.save_task_run("s", &run, &AtomicBool::new(true));
        assert!(
            !store.task_runs_dir("s").exists(),
            "zombie 不能写仍在索引中的会话"
        );
        store.save_task_run("s", &run, &AtomicBool::new(false));
        assert!(store.task_runs_dir("s").exists());
        store.remove("s").unwrap();
        assert!(!store.task_runs_dir("s").exists());
        store.save_task_run("s", &run, &AtomicBool::new(true));
        assert!(!store.task_runs_dir("s").exists());
    }
}
