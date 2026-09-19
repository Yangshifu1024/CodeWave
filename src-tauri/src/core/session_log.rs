//! 会话级细粒度日志（黑匣子，[docs/session-logging-report](../../../docs/session-logging-report.md)）：一会话一文件，不受全局日志级别过滤，恒开启。
//! 落点遵循 W5 路由：项目会话与子代理（继承父会话 project_dir）= 项目托管目录 logs/<session-id>.log；
//! 临时/自由会话 = 全局数据目录 logs/<session-id>.log。
//! zombie 会话一律跳过（H5：迟到写入不得复活已删除会话的日志）。

use crate::core::agent::SessionRuntime;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// 进程级写锁：并发任务写同一文件时防止行交错（每次 open-write-close 都很短）。
static WRITE_LOCK: Mutex<()> = Mutex::new(());
/// 已确认存在的日志目录缓存：热路径免得每写一行都付一次 create_dir_all 系统调用。
static DIRS_OK: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// 会话日志目录。zombie = None（调用方静默跳过）。
pub fn dir_for(rt: &SessionRuntime) -> Option<PathBuf> {
    if rt.zombie.load(Ordering::SeqCst) {
        return None;
    }
    Some(
        rt.project_dir
            .clone()
            .unwrap_or_else(|| rt.data_dir.clone())
            .join("logs"),
    )
}

/// 会话日志文件路径（供 IPC 读取与测试）。
pub fn file_for(rt: &SessionRuntime) -> Option<PathBuf> {
    dir_for(rt).map(|d| d.join(format!("{}.log", rt.id)))
}

/// 确保目录存在；成功后记入 DIRS_OK 缓存，失败返回 false（调用方放弃本次写）。
fn ensure_dir(dir: &Path) -> bool {
    if let Ok(set) = DIRS_OK.lock() {
        if set.as_ref().is_some_and(|s| s.contains(dir)) {
            return true;
        }
    }
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    if let Ok(mut set) = DIRS_OK.lock() {
        set.get_or_insert_with(HashSet::new)
            .insert(dir.to_path_buf());
    }
    true
}

/// 追加一行 `[ts] [LEVEL] line`。任何失败都静默——日志绝不拖垮主流程。
pub fn log(rt: &SessionRuntime, level: &str, line: &str) {
    let Some(dir) = dir_for(rt) else { return };
    if !ensure_dir(&dir) {
        return;
    }
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
    let path = dir.join(format!("{}.log", rt.id));
    let guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{ts}] [{level}] {line}");
    }
    drop(guard);
}

/// 记 INFO 级会话日志。
pub fn info(rt: &SessionRuntime, line: &str) {
    log(rt, "INFO", line);
}

/// 记 WARN 级会话日志。
pub fn warn(rt: &SessionRuntime, line: &str) {
    log(rt, "WARN", line);
}

/// 记 ERROR 级会话日志。
pub fn error(rt: &SessionRuntime, line: &str) {
    log(rt, "ERROR", line);
}

/// verbose 模式（config.log.session_verbose）：会话日志额外记录完整 LLM 请求/响应文本。
pub fn verbose_enabled(cfg: &crate::core::config::ConfigState) -> bool {
    cfg.log.session_verbose
}

/// 摘要截断：按字符边界截到 max 个字符，截断时追加省略标记。
pub fn trunc(s: &str, max: usize) -> String {
    let mut head_end = s.len();
    let mut overflow = false;
    for (i, (idx, _)) in s.char_indices().enumerate() {
        if i == max {
            head_end = idx;
            overflow = true;
            break;
        }
    }
    if !overflow {
        return s.to_string();
    }
    format!("{}…(truncated)", &s[..head_end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rt(id: &str) -> (tempfile::TempDir, std::sync::Arc<SessionRuntime>) {
        use crate::tools::pathutil::WriteRoots;
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session(id, roots.workspace.clone(), None, vec![], None, vec![]);
        (dd, rt)
    }

    #[test]
    fn slog_writes_lines() {
        let (_dd, rt) = make_rt("slog-write");
        info(&rt, "run 开始");
        warn(&rt, "写第二行");
        assert!(
            file_for(&rt)
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("slog-write"),
            "文件名应为会话 id"
        );
        let content = std::fs::read_to_string(file_for(&rt).unwrap()).unwrap();
        assert!(content.contains("[INFO] run 开始"), "{content}");
        assert!(content.contains("[WARN] 写第二行"), "{content}");
    }

    #[test]
    fn slog_zombie_skips() {
        let (_dd, rt) = make_rt("slog-zombie");
        rt.zombie.store(true, Ordering::SeqCst);
        info(&rt, "不应落盘");
        assert_eq!(
            file_for(&rt),
            None,
            "zombie 会话连路径都不解析（H5：不复活日志文件）"
        );
    }

    #[test]
    fn trunc_respects_char_boundary() {
        assert_eq!(trunc("hello", 10), "hello");
        let s = trunc("你好世界", 2);
        assert!(s.starts_with("你好"));
        assert!(s.contains("(truncated)"), "{s}");
        // emoji 等多字节内容不得 panic
        let _ = trunc("😀😀😀", 2);
    }
}
