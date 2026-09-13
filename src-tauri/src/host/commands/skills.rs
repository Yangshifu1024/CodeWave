use super::util::{err, Core};
use std::path::PathBuf;

/// 技能扫描上下文：有会话用会话快照 (workspace, data_dir, project_dir)；无会话回退全局 data_dir。
fn skill_scope(core: &Core<'_>, session_id: Option<&str>) -> (PathBuf, PathBuf, Option<PathBuf>) {
    match session_id.and_then(|id| core.session(id)) {
        Some(rt) => (
            rt.workspace.clone(),
            rt.data_dir.clone(),
            rt.project_dir.clone(),
        ),
        None => (core.data_dir.clone(), core.data_dir.clone(), None),
    }
}

/// 列出可用技能（内置 + ~/.claude/skills 与 ~/.agents/skills 用户级兼容 + 工作区 .claude/.agents
/// + 托管 .codewave/skills，排除已禁用项）。
#[tauri::command]
pub async fn list_skills(
    core: Core<'_>,
    session_id: Option<String>,
) -> Result<Vec<crate::skills::SkillMeta>, String> {
    let (workspace, data_dir, project_dir) = skill_scope(&core, session_id.as_deref());
    let disabled = core.cfg.read().unwrap().disabled_skills.clone();
    Ok(core
        .skills
        .list(&workspace, &data_dir, &disabled, project_dir.as_deref()))
}

/// 读取单个技能详情（元数据 + SKILL.md 正文；被禁用或不存在时返回 None）。
#[tauri::command]
pub async fn get_skill(
    core: Core<'_>,
    session_id: Option<String>,
    name: String,
) -> Result<Option<crate::skills::Skill>, String> {
    let (workspace, data_dir, project_dir) = skill_scope(&core, session_id.as_deref());
    let disabled = core.cfg.read().unwrap().disabled_skills.clone();
    Ok(core
        .skills
        .get(&workspace, &data_dir, &disabled, project_dir.as_deref(), &name))
}

/// 重新加载技能：清空 SkillIndex TTL 缓存后重扫（设置页「重新加载」入口），返回最新列表。
#[tauri::command]
pub async fn reload_skills(
    core: Core<'_>,
    session_id: Option<String>,
) -> Result<Vec<crate::skills::SkillMeta>, String> {
    let (workspace, data_dir, project_dir) = skill_scope(&core, session_id.as_deref());
    let disabled = core.cfg.read().unwrap().disabled_skills.clone();
    core.skills.invalidate();
    Ok(core
        .skills
        .list(&workspace, &data_dir, &disabled, project_dir.as_deref()))
}

/// 删除托管技能（仅全局/项目 .codewave/skills；内置与 .claude/.agents 兼容目录拒绝），成功后缓存自动失效。
#[tauri::command]
pub async fn delete_skill(
    core: Core<'_>,
    session_id: Option<String>,
    name: String,
) -> Result<(), String> {
    let (workspace, data_dir, project_dir) = skill_scope(&core, session_id.as_deref());
    core.skills
        .delete_skill(&workspace, &data_dir, project_dir.as_deref(), &name)
}

/// 启用/禁用技能（写入 config 的 disabled_skills 并落盘）。
#[tauri::command]
pub async fn toggle_skill(
    core: Core<'_>,
    _session_id: Option<String>,
    name: String,
    disabled: bool,
) -> Result<(), String> {
    let mut cfg = core.cfg.read().unwrap().clone();
    cfg.disabled_skills.retain(|s| s != &name);
    if disabled {
        cfg.disabled_skills.push(name);
    }
    cfg.save().map_err(err)?;
    *core.cfg.write().unwrap() = cfg;
    Ok(())
}

// ---------- 工作区目录（Explorer）----------

