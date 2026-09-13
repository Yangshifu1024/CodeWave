use super::util::{err, Core};

/// 用户级 mcp.json 的路径（全局数据目录下）。
fn mcp_config_path() -> std::path::PathBuf {
    crate::core::config::data_dir().join("mcp.json")
}

/// 用户级 mcp.json 原文（表单编辑器与 JSON 模式共用）。
#[tauri::command]
pub async fn get_mcp_config() -> Result<String, String> {
    Ok(std::fs::read_to_string(mcp_config_path())
        .unwrap_or_else(|_| "{\n  \"mcpServers\": {}\n}".into()))
}

/// 保存用户级 mcp.json 并热重载（restart all；工作区级配置经 connect_mcp 生效）。
#[tauri::command]
pub async fn save_mcp_config(core: Core<'_>, json: String) -> Result<(), String> {
    // 校验
    serde_json::from_str::<serde_json::Value>(&json).map_err(|e| format!("JSON 无效：{e}"))?;
    crate::util::atomic::atomic_write(&mcp_config_path(), json.as_bytes()).map_err(err)?;
    // 热重载：restart all（简化实现；[docs/p1-plan](../../../../docs/p1-plan.md) §4.1 的「先停旧再起新」在此落地）
    core.mcp.stop_all().await;
    Ok(())
}

/// 为本工作区连接所有应启用的 MCP server（用户级 + 项目级），逐 server 上报状态。
#[tauri::command]
pub async fn connect_mcp(core: Core<'_>, session_id: String) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let extra: Vec<String> = rt.extra_roots.lock().unwrap().clone();
    let configs = crate::mcp::load_configs(
        &rt.data_dir,
        &rt.workspace,
        rt.project_dir.as_deref(),
        &extra,
    );
    let mut started = Vec::new();
    let mut failed = Vec::new();
    for (name, cfg) in configs {
        // streamable-http 连接走代理感知的共享 client（读锁 clone，std 锁不跨 await）
        let http = core.client.read().unwrap().clone();
        match core.mcp.start(&name, cfg, http).await {
            Ok(tools) => started.push(serde_json::json!({ "name": name, "tools": tools.len() })),
            Err(e) => failed.push(serde_json::json!({ "name": name, "error": e })),
        }
        if let Some((n, st, count)) = core
            .mcp
            .status()
            .await
            .into_iter()
            .find(|(n, _, _)| *n == name)
        {
            core.sink.emit(
                &rt.id,
                "mcp:status",
                serde_json::json!({
                    "session": rt.id, "server": n, "state": st, "tools": count,
                }),
            );
        }
    }
    Ok(serde_json::json!({ "started": started, "failed": failed }))
}

/// 当前 MCP 连接状态。
#[tauri::command]
pub async fn mcp_status(core: Core<'_>) -> Result<Vec<serde_json::Value>, String> {
    let v: Vec<serde_json::Value> = core
        .mcp
        .status()
        .await
        .into_iter()
        .map(|(name, state, tools)| serde_json::json!({ "name": name, "state": state, "tools": tools }))
        .collect();
    Ok(v)
}

// ---------- 后台 service 停止（与前端 C2 配对）----------

