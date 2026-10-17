use super::util::Core;
use crate::core::activity::ActivitySnapshot;

/// 总览只取有界进程内摘要；不加载历史，也不依赖前端打开的 Tab。
#[tauri::command]
pub fn get_activity_snapshot(core: Core<'_>) -> ActivitySnapshot {
    crate::core::workflow::projection::activity_snapshot(&core)
}

/// 原询问只在对应 run 及 responder 仍有效时返回；应答仍走既有 resolve_ask。
#[tauri::command]
pub fn get_activity_ask(
    core: Core<'_>,
    session_id: String,
    run_id: String,
    ask_id: String,
) -> Option<serde_json::Value> {
    core.activity.ask(&session_id, &run_id, &ask_id)
}
