use crate::agents::AgentMeta;

/// 列出内置子代理角色（composer $ 菜单数据源；title 为内部角色，不在清单内）。
/// 纯转调 core 注册表，无会话上下文。
#[tauri::command]
pub async fn list_agents() -> Result<Vec<AgentMeta>, String> {
    Ok(crate::agents::delegable())
}
