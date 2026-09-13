//! 全局诊断日志（[docs/session-logging-report](../../../docs/session-logging-report.md)）：tracing 初始化（reload 层支持级别热切换）
//! + 滚动日志保留期清理。
//! 与 core/session_log.rs 的分工：本模块记简短全局诊断；会话级细粒度轨迹在 session_log。

use std::path::Path;
use std::sync::OnceLock;
use tracing_subscriber::EnvFilter;

/// reload 句柄类型别名（EnvFilter 挂在 Registry 之上，供热切换持有）。
type ReloadHandle = tracing_subscriber::reload::Handle<EnvFilter, tracing_subscriber::Registry>;

static RELOAD: OnceLock<ReloadHandle> = OnceLock::new();

/// 滚动日志保留天数。
pub const KEEP_DAYS: i64 = 14;

/// 全局滚动日志基础文件名（滚动归档为 `<LOG_BASE_NAME>.YYYY-MM-DD`）。
pub const LOG_BASE_NAME: &str = "codewave.log";

/// 进程内一次性初始化：按天滚动到 `~/.codewave/logs/codewave.log.YYYY-MM-DD`，
/// 只写文件不写 stdout（release 构建使用 windows_subsystem，本来也没有控制台）。
/// 存在 RUST_LOG 时完全由其接管；否则从 info 起步，待 setup 读入配置后由
/// apply_config_level 修正。
pub fn init() {
    let _ = crate::core::config::ensure_dirs();
    let logs = crate::core::config::data_dir().join("logs");
    let appender = tracing_appender::rolling::daily(&logs, LOG_BASE_NAME);
    let (writer, guard) = tracing_appender::non_blocking(appender);
    // guard 必须全程存活：直接泄漏（单次初始化，量可忽略）
    std::mem::forget(guard);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // reload 层必须是 registry 的第一个 `with` 项（S = Registry），Handle 类型才能与
    // 静态声明匹配
    let (filter, handle) = tracing_subscriber::reload::Layer::new(filter);
    let _ = RELOAD.set(handle);
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .try_init()
        .ok();
}

/// RUST_LOG 是否构成有效覆盖（存在、非空、可解析）：构成则配置级别永不生效（开发者场景优先）。
/// 无效的 RUST_LOG 不算覆盖——否则配置级别会被永久忽略、实际日志停在 info，难以排查。
pub fn env_overrides() -> bool {
    match std::env::var("RUST_LOG") {
        Ok(v) if !v.trim().is_empty() => EnvFilter::try_new(&v).is_ok(),
        _ => false,
    }
}

/// 应用配置的日志级别（setup 初始化与 save_config 热切换共用）。
/// 无效级别 warn 并保持原级别；RUST_LOG 覆盖时为 no-op。
pub fn apply_config_level(level: &str) {
    if env_overrides() {
        return;
    }
    let Some(handle) = RELOAD.get() else { return };
    match EnvFilter::try_new(level) {
        Ok(f) => {
            let _ = handle.reload(f);
        }
        Err(e) => tracing::warn!("日志级别 `{level}` 无效，保持原级别：{e}"),
    }
}

/// 启动期清理入口：失败只 warn，绝不阻塞启动。
pub fn prune_expired_logs() {
    let dir = crate::core::config::data_dir().join("logs");
    match prune_logs_dir(&dir, chrono::Local::now(), KEEP_DAYS) {
        Ok(0) => {}
        Ok(n) => tracing::info!("已清理 {n} 个过期滚动日志（保留 {KEEP_DAYS} 天）"),
        Err(e) => tracing::warn!("滚动日志清理失败：{e}"),
    }
}

/// 删除日期早于保留窗口的 `<LOG_BASE_NAME>.YYYY-MM-DD` 历史滚动文件；返回删除数。
/// 只有「LOG_BASE_NAME + . + 严格 YYYY-MM-DD」后缀才计数——当前活跃文件（日期 = 今天）
/// 天然不会被删；会话日志 `<session-id>.log` 与目录中其他文件一概不碰。
pub fn prune_logs_dir(
    dir: &Path,
    now: chrono::DateTime<chrono::Local>,
    keep_days: i64,
) -> std::io::Result<usize> {
    let cutoff = now.date_naive() - chrono::Duration::days(keep_days);
    let mut removed = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.metadata()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(date) = dated_log_name(&name) else {
            continue;
        };
        if date < cutoff {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!("删除过期日志 {name} 失败：{e}"),
            }
        }
    }
    Ok(removed)
}

/// `codewave.log.YYYY-MM-DD` → 日期；否则 None。
fn dated_log_name(name: &str) -> Option<chrono::NaiveDate> {
    let suffix = name
        .strip_prefix(LOG_BASE_NAME)
        .and_then(|rest| rest.strip_prefix('.'))?;
    if suffix.len() != 10 {
        return None;
    }
    chrono::NaiveDate::parse_from_str(suffix, "%Y-%m-%d").ok()
}

// ---------- 日志读取（供 IPC，[docs/session-logging-report](../../../docs/session-logging-report.md)；host 层只转发） ----------

/// 日志文件列表项（RightBar 日志查看器的文件清单）。
#[derive(serde::Serialize)]
pub struct LogFileEntry {
    /// 文件名（不含路径）
    pub name: String,
    /// 字节数
    pub size: u64,
    /// 修改时间（本地时区格式化串；取不到为 None）
    pub modified: Option<String>,
}

/// 日志尾部读取结果。
#[derive(serde::Serialize)]
pub struct LogFileContent {
    /// 尾部窗口内的文本行（\n 连接）
    pub content: String,
    /// 头部被截断或内容超出 tail_lines 窗口：存在更早内容
    pub truncated: bool,
}

/// 日志查看器默认尾部行数。
pub const DEFAULT_TAIL_LINES: usize = 500;
/// 单次读取允许的最大尾部行数。
const MAX_TAIL_LINES: usize = 2000;
/// 尾读前最多回看 2 MiB（防超大文件整读进内存）。
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// 全局滚动日志文件名白名单：`codewave.log` 或 `codewave.log.YYYY-MM-DD`（防路径穿越）。
pub fn valid_global_log_name(name: &str) -> bool {
    if name == LOG_BASE_NAME {
        return true;
    }
    match name
        .strip_prefix(LOG_BASE_NAME)
        .and_then(|rest| rest.strip_prefix('.'))
    {
        Some(suffix) => {
            suffix.len() == 10 && chrono::NaiveDate::parse_from_str(suffix, "%Y-%m-%d").is_ok()
        }
        None => false,
    }
}

/// 会话/项目 id 白名单：ASCII 字母数字加 - _（防路径穿越）。
pub fn valid_entity_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 读文件尾部：先 seek 到最多最后 2 MiB，再保留最后 tail_lines 行。
fn read_tail(path: &Path, tail_lines: usize) -> std::io::Result<LogFileContent> {
    use std::io::{Read, Seek, SeekFrom};
    let tail_lines = tail_lines.clamp(1, MAX_TAIL_LINES);
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let start = size.saturating_sub(MAX_READ_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    // 从文件中部起读时丢弃首行（可能是半行）
    if start > 0 {
        if let Some(pos) = buf.find('\n') {
            buf = buf[pos + 1..].to_string();
        }
    }
    let lines: Vec<&str> = buf.lines().collect();
    let total = lines.len();
    let window: &[&str] = if total > tail_lines {
        &lines[total - tail_lines..]
    } else {
        lines.as_slice()
    };
    Ok(LogFileContent {
        content: window.join("\n"),
        truncated: start > 0 || total > tail_lines,
    })
}

/// 列出全局滚动日志（最新在前）。
pub fn list_log_files() -> std::io::Result<Vec<LogFileEntry>> {
    let dir = crate::core::config::data_dir().join("logs");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if !valid_global_log_name(&name) {
            continue;
        }
        let modified = meta.modified().ok().map(|t| {
            chrono::DateTime::<chrono::Local>::from(t)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        });
        out.push(LogFileEntry {
            name,
            size: meta.len(),
            modified,
        });
    }
    // 文件名内嵌日期，按名倒序 = 最新在前
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

/// 读全局滚动日志尾部；名称非法或文件缺失返回 Err。
pub fn read_global_log(name: &str, tail_lines: usize) -> Result<LogFileContent, String> {
    if !valid_global_log_name(name) {
        return Err("E_LOG_NAME: 非法的日志文件名".into());
    }
    read_tail(
        &crate::core::config::data_dir().join("logs").join(name),
        tail_lines,
    )
    .map_err(|e| e.to_string())
}

/// 读会话日志尾部。路径解析：内存中存在 runtime 时以其实际位置（项目托管目录 / 全局）优先；
/// 否则 project_id 经项目注册表解析，None 落全局 logs 目录；文件尚不存在（会话从未运行）
/// 返回空内容而非错误。
pub fn read_session_log(
    data_dir: &Path,
    rt: Option<&crate::core::agent::SessionRuntime>,
    session_id: &str,
    project_id: Option<&str>,
    tail_lines: usize,
) -> Result<LogFileContent, String> {
    if !valid_entity_id(session_id) {
        return Err("E_SESSION_ID: 非法的会话 id".into());
    }
    let path = match rt.and_then(crate::core::session_log::file_for) {
        Some(p) if p.exists() => p,
        _ => match project_id {
            Some(pid) => {
                if !valid_entity_id(pid) {
                    return Err("E_PROJECT_ID: 非法的项目 id".into());
                }
                crate::core::projects::session_log_path(data_dir, pid, session_id)
            }
            None => data_dir.join("logs").join(format!("{session_id}.log")),
        },
    };
    if !path.exists() {
        return Ok(LogFileContent {
            content: String::new(),
            truncated: false,
        });
    }
    read_tail(&path, tail_lines).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dated_log_name_strict() {
        let dated = |d: &str| format!("{LOG_BASE_NAME}.{d}");
        assert_eq!(
            dated_log_name(&dated("2026-09-01")),
            chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
        );
        // 当前活跃文件名以及会话/无关日志都不是滚动归档
        assert_eq!(dated_log_name(LOG_BASE_NAME), None);
        assert_eq!(dated_log_name("sess-abc.log"), None);
        assert_eq!(dated_log_name(&dated("2026-9-1")), None);
        assert_eq!(dated_log_name(&dated("backup")), None);
        assert_eq!(dated_log_name("other.log.2026-09-01"), None);
    }

    #[test]
    fn prune_only_expired_dated_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        std::fs::write(p(&format!("{LOG_BASE_NAME}.2020-01-01")), b"old").unwrap();
        std::fs::write(p(&format!("{LOG_BASE_NAME}.2099-01-01")), b"fresh").unwrap();
        std::fs::write(p(LOG_BASE_NAME), b"active").unwrap();
        std::fs::write(p("free-session-1.log"), b"session").unwrap();
        std::fs::write(p("unrelated.txt"), b"x").unwrap();

        let now = chrono::Local::now();
        let removed = prune_logs_dir(dir.path(), now, 14).unwrap();
        assert_eq!(removed, 1);
        assert!(
            !p(&format!("{LOG_BASE_NAME}.2020-01-01")).exists(),
            "过期滚动日志应被删"
        );
        assert!(p(&format!("{LOG_BASE_NAME}.2099-01-01")).exists());
        assert!(p(LOG_BASE_NAME).exists(), "活跃文件不动");
        assert!(p("free-session-1.log").exists(), "会话日志不动");
        assert!(p("unrelated.txt").exists());
    }

    /// [docs/session-logging-report](../../../docs/session-logging-report.md)：日志文件名白名单拒绝路径穿越与目录逃逸。
    #[test]
    fn global_log_name_whitelist() {
        assert!(valid_global_log_name(LOG_BASE_NAME));
        assert!(valid_global_log_name(&format!("{LOG_BASE_NAME}.2026-09-01")));
        for bad in [
            "../config.json",
            "..\\x",
            &format!("{LOG_BASE_NAME}.2026-9-1"),
            &format!("{LOG_BASE_NAME}.extra"),
            "sess-1.log",
            &format!("{LOG_BASE_NAME}.2026-09-01/../x"),
            "",
        ] {
            assert!(!valid_global_log_name(bad), "{bad} 不应通过白名单");
        }
    }

    #[test]
    fn entity_id_whitelist() {
        assert!(valid_entity_id("abc123"));
        assert!(valid_entity_id("sub_ab-12"));
        assert!(!valid_entity_id(""));
        assert!(!valid_entity_id("../x"));
        assert!(!valid_entity_id("白板"));
        assert!(!valid_entity_id("a/b"));
    }

    #[test]
    fn read_tail_lines_and_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        std::fs::write(
            &path,
            (1..=10).map(|i| format!("line{i}\n")).collect::<String>(),
        )
        .unwrap();
        let out = read_tail(&path, 3).unwrap();
        assert_eq!(out.content, "line8\nline9\nline10");
        assert!(out.truncated, "窗口外的更早行应标记 truncated");
        let out = read_tail(&path, 50).unwrap();
        assert_eq!(out.content.lines().count(), 10);
        assert!(!out.truncated);
        // 文件缺失报错
        assert!(read_tail(&dir.path().join("nope.log"), 10).is_err());
    }

    /// 会话日志缺失（会话从未运行）= 空内容而非错误（前端空态依赖该语义）。
    #[test]
    fn read_session_log_missing_file_is_empty() {
        let dd = tempfile::tempdir().unwrap();
        let out = read_session_log(dd.path(), None, "no-such-session", None, 10).unwrap();
        assert_eq!(out.content, "");
        assert!(!out.truncated);
        // 非法 id 仍报错
        assert!(read_session_log(dd.path(), None, "../x", None, 10).is_err());
    }
}
