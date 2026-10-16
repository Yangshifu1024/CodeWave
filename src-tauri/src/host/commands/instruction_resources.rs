//! Host review entry points; no model tool can call these transactions.
use super::util::Core;
use crate::core::instruction_resources::review;

#[tauri::command]
pub async fn inspect_instruction_resources(
    core: Core<'_>,
    session_id: Option<String>,
) -> Result<review::Inspection, String> {
    review::inspect(&core, session_id.as_deref()).await
}

#[tauri::command]
pub async fn prepare_instruction_review(
    core: Core<'_>,
    session_id: Option<String>,
    resource_id: String,
    expected_version: String,
    action: review::Action,
) -> Result<review::Challenge, String> {
    review::prepare(
        &core,
        session_id.as_deref(),
        &resource_id,
        &expected_version,
        action,
    )
    .await
}

#[tauri::command]
pub async fn confirm_instruction_review(
    core: Core<'_>,
    session_id: Option<String>,
    confirmation: String,
) -> Result<review::Receipt, String> {
    review::confirm(&core, session_id.as_deref(), &confirmation).await
}

#[tauri::command]
pub async fn instruction_review_operation(
    core: Core<'_>,
    session_id: Option<String>,
    operation_id: String,
) -> Result<review::Receipt, String> {
    review::operation(&core, session_id.as_deref(), &operation_id).await
}

#[tauri::command]
pub fn discard_instruction_review(core: Core<'_>, confirmation: String) {
    review::discard(&core, &confirmation);
}
