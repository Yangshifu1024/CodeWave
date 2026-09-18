//! host 命令层共享助手（state 别名、错误格式化、跨领域小工具）。

use crate::core::agent::AgentCore;
use std::sync::Arc;
use tauri::State;

/// 命令层的共享 state 别名：全局 AgentCore。
pub(super) type Core<'a> = State<'a, Arc<AgentCore>>;

/// 错误统一转字符串（IPC 边界只传 String）。
pub(super) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 共享助手：在系统文件管理器中打开目录（平台分支在 core::openers：Windows 优先 Files 应用，
/// 探测不到则 explorer；与 open_logs_dir / open_data_dir 同一路径）。
pub(super) fn open_dir_in_file_manager(dir: &std::path::Path, err_label: &str) -> Result<(), String> {
    crate::core::openers::open_dir(dir).map_err(|e| format!("{err_label}：{e}"))
}

