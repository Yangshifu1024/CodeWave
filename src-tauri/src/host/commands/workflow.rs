//! Thin desktop workflow commands. All validation, commits and transitions live in core.
use super::util::Core;
use crate::core::workflow::{project, projection};
#[tauri::command]
pub async fn read_project_workflow(
    core: Core<'_>,
    session_id: String,
) -> Result<project::ProjectView, String> {
    let rt = core.session(&session_id).ok_or("Session missing")?;
    if rt.project_id.is_none() {
        return Err("A project session is required".into());
    }
    let cfg = core.cfg.read().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || project::read(&rt.data_dir, &rt.workspace, &cfg))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn validate_project_workflow(
    core: Core<'_>,
    session_id: String,
    text: String,
) -> Result<Vec<crate::core::workflow::config::Issue>, String> {
    let rt = core.session(&session_id).ok_or("Session missing")?;
    let cfg = core.cfg.read().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || project::validate(&text, &rt.workspace, &cfg))
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn save_project_workflow(
    core: Core<'_>,
    session_id: String,
    text: String,
    expected_revision: u64,
    expected_digest: String,
    operation_id: String,
) -> Result<project::ProjectView, String> {
    let rt = core.session(&session_id).ok_or("Session missing")?;
    if rt.project_id.is_none() {
        return Err("A project session is required".into());
    }
    let cfg = core.cfg.read().unwrap().clone();
    let mut user = crate::core::delivery::DeliveryService::identity_of(&rt);
    user.actor_id = "user".into();
    tauri::async_runtime::spawn_blocking(move || {
        project::save(
            &rt.data_dir,
            &rt.workspace,
            &cfg,
            &text,
            expected_revision,
            &expected_digest,
            &operation_id,
            &user,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn get_workflow_projection(
    core: Core<'_>,
    session_id: String,
) -> Result<Option<projection::Projection>, String> {
    let rt = core.session(&session_id).ok_or("Session missing")?;
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || projection::read(&core, &rt))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn continue_workflow(
    core: Core<'_>,
    session_id: String,
    task_id: String,
    expected_revision: u64,
) -> Result<String, String> {
    let rt = core.session(&session_id).ok_or("Session missing")?;
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::core::workflow::lifecycle::continue_task(&core, rt, &task_id, expected_revision)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn override_workflow_route(
    core: Core<'_>,
    session_id: String,
    task_id: String,
    expected_revision: u64,
    kind: crate::core::workflow::routing::TaskKind,
    reason: String,
) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("Session missing")?;
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        projection::override_route(&core, &rt, &task_id, expected_revision, kind, &reason)
    })
    .await
    .map_err(|e| e.to_string())?
}
