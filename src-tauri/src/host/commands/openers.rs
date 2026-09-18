//! 打开器 IPC：文件管理器（`open_dir` 在 system.rs，行为已切到 core::openers）与编辑器。
//! 命令只做参数校验 + 转调 core，业务逻辑全在 `core::openers`
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。

use super::system::validate_open_dir;
use crate::core::openers::{self, EditorInfo};

/// 已检测到的编辑器列表（候选表顺序：VS Code → Cursor → Windsurf → Zed → Sublime Text →
/// Notepad++ → JetBrains 组；前端默认用其中第一个）。
/// async：探测要遍历 PATH × 扩展名并扫 JetBrains 安装根，不能让同步命令堵主线程。
#[tauri::command]
pub async fn list_editors() -> Vec<EditorInfo> {
    openers::detect_editors()
}

/// 在指定编辑器中打开目录（`editor_id` 必须是 `list_editors` 返回过的 id）。
#[tauri::command]
pub async fn open_in_editor(editor_id: String, path: String) -> Result<(), String> {
    let dir = validate_open_dir(&path)?;
    openers::open_in_editor(&editor_id, std::path::Path::new(dir))
        .map_err(|e| format!("打开编辑器失败：{e}"))
}
