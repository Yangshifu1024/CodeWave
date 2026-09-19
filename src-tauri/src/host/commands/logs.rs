use super::util::open_dir_in_file_manager;
use super::util::{Core, err};

/// 列出全局滚动日志文件（14 天清理策略下的现存文件）。
#[tauri::command]
pub async fn list_log_files() -> Result<Vec<crate::core::logging::LogFileEntry>, String> {
    crate::core::logging::list_log_files().map_err(err)
}

/// 读取全局日志文件内容（支持只取尾部 N 行）。
#[tauri::command]
pub async fn read_log_file(
    name: String,
    tail_lines: Option<usize>,
) -> Result<crate::core::logging::LogFileContent, String> {
    crate::core::logging::read_global_log(
        &name,
        tail_lines.unwrap_or(crate::core::logging::DEFAULT_TAIL_LINES),
    )
}

/// 读取单个会话的黑匣子日志（run/LLM 请求/工具/审批等全轨迹，尾部 N 行）。
#[tauri::command]
pub async fn read_session_log(
    core: Core<'_>,
    session_id: String,
    project_id: Option<String>,
    tail_lines: Option<usize>,
) -> Result<crate::core::logging::LogFileContent, String> {
    crate::core::logging::read_session_log(
        &core.data_dir,
        core.session(&session_id).as_deref(),
        &session_id,
        project_id.as_deref(),
        tail_lines.unwrap_or(crate::core::logging::DEFAULT_TAIL_LINES),
    )
}

/// 在系统文件管理器中打开全局 logs 目录（不存在则先建）。
#[tauri::command]
pub async fn open_logs_dir() -> Result<(), String> {
    let dir = crate::core::config::data_dir().join("logs");
    std::fs::create_dir_all(&dir).map_err(err)?;
    open_dir_in_file_manager(&dir, "打开日志目录失败")
}
