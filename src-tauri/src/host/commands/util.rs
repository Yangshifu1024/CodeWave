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

/// 共享助手：在系统文件管理器中 reveal 目录（按平台分支，
/// 不依赖 tauri-plugin-opener；与 open_logs_dir 同一模式）。
pub(super) fn open_dir_in_file_manager(dir: &std::path::Path, err_label: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map_err(|e| format!("{err_label}：{e}"))?;
    Ok(())
}

