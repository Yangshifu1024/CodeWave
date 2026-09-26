use super::util::{Core, err};

/// 停止会话内启动的后台 service 并从注册表移除。
#[tauri::command]
pub async fn stop_service(
    core: Core<'_>,
    session_id: String,
    service_id: String,
) -> Result<(), String> {
    let _ = session_id;
    let Some(h) = core.services.get(&service_id) else {
        return Err(format!("服务不存在：{service_id}"));
    };
    crate::tools::service::stop_service(&h).await?;
    core.services.remove(&service_id);
    Ok(())
}

// ---------- git diff/log（[docs/p1-plan](../../../../docs/p1-plan.md) §6.4）----------

/// 列出计划任务（含下次运行时间与最近状态）。
#[tauri::command]
pub async fn list_scheduled_tasks(
    core: Core<'_>,
    _session_id: String,
) -> Result<Vec<crate::core::scheduler::ScheduledTask>, String> {
    Ok(core.tasks.list().await)
}

/// 新建计划任务：校验 schedule、生成短 id、归属当前项目后落盘。
#[tauri::command]
pub async fn create_scheduled_task(
    core: Core<'_>,
    session_id: String,
    name: String,
    instruction: String,
    schedule: String,
) -> Result<crate::core::scheduler::ScheduledTask, String> {
    let next = crate::core::scheduler::initial_next(&schedule).map_err(err)?;
    let Some(next) = next else {
        return Err("once 时间已过去".into());
    };
    let project_id = core
        .session(&session_id)
        .and_then(|rt| rt.project_id.clone());
    let task = crate::core::scheduler::ScheduledTask {
        id: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
        name,
        instruction,
        schedule,
        next_run: Some(next),
        last_status: None,
        last_summary: None,
        project_id,
        enabled: true,
        runs: Vec::new(),
    };
    core.tasks.upsert(task.clone()).await; // upsert 内部自持久化（项目附属任务）
    Ok(task)
}

/// 删除计划任务（不存在时报错）。
#[tauri::command]
pub async fn delete_scheduled_task(
    core: Core<'_>,
    _session_id: String,
    id: String,
) -> Result<(), String> {
    core.tasks
        .remove(&id)
        .await
        .ok_or_else(|| "任务不存在".to_string())?;
    Ok(())
}

/// 编辑计划任务（名称 / 指令 / 计划）：计划变化时由 core 侧重算 next_run。
/// 全局任务表操作，与当前会话无关，故不收 session_id（只校验 + 转调 core）。
#[tauri::command]
pub async fn update_scheduled_task(
    core: Core<'_>,
    id: String,
    name: String,
    instruction: String,
    schedule: String,
) -> Result<crate::core::scheduler::ScheduledTask, String> {
    core.tasks.update(&id, name, instruction, schedule).await
}

/// 暂停 / 启用计划任务（暂停保留 next_run；启用重算，once 已过期则报错）。
#[tauri::command]
pub async fn set_scheduled_task_enabled(
    core: Core<'_>,
    id: String,
    enabled: bool,
) -> Result<crate::core::scheduler::ScheduledTask, String> {
    core.tasks.set_enabled(&id, enabled).await
}

/// 立即运行一次计划任务（不修改 next_run；已有任务在跑时直接拒绝）。
#[tauri::command]
pub async fn run_scheduled_task_now(core: Core<'_>, id: String) -> Result<(), String> {
    crate::core::scheduler::trigger_now(core.inner().clone(), &id).await
}

// ---------- 统计查询（P2-H）----------
