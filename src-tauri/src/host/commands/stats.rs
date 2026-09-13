use super::util::Core;

/// 会话上下文 token 构成明细（system / 历史 / 工具等分项）。
#[tauri::command]
pub async fn get_token_breakdown(
    core: Core<'_>,
    session_id: String,
) -> Result<crate::core::context::ContextBreakdown, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    Ok(crate::core::context::breakdown(&core, &rt).await)
}

// ---------- MCP（[docs/p1-plan](../../../../docs/p1-plan.md) §4.1）----------

/// 按天聚合的 token 用量统计（默认近 30 天）。
#[tauri::command]
pub async fn get_token_stats(
    core: Core<'_>,
    days: Option<u32>,
) -> Result<Vec<crate::core::stats::DailyStats>, String> {
    Ok(crate::core::stats::query(
        &core.data_dir,
        days.unwrap_or(30),
    ))
}

// ---------- 子代理（P2-F）----------

