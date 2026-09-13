use super::util::Core;

#[tauri::command]
pub async fn git_status(core: Core<'_>, session_id: String) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    // 多根聚合：逐根独立探测是否仓库（只收真实仓库）；条目带 root 标注
    let roots = session_roots(&rt);
    let mut entries: Vec<serde_json::Value> = Vec::new();
    let mut any_repo = false;
    let mut branch: Option<String> = None;
    for root in &roots {
        // M4：逐根容错——某根仓库损坏不拖垮整个聚合面板
        match crate::git::status::status(std::path::Path::new(root)) {
            Ok(Some(info)) => {
                any_repo = true;
                if branch.is_none() {
                    branch = Some(info.branch);
                }
                for e in info.entries {
                    entries.push(serde_json::json!({ "root": root, "entry": e }));
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("git status 跳过根 {root}：{e}"),
        }
    }
    Ok(serde_json::json!({ "repo": any_repo, "entries": entries, "branch": branch }))
}

/// 会话的全部根（主目录 + extra_roots），供 git/list 等聚合遍历用。
fn session_roots(rt: &crate::core::agent::SessionRuntime) -> Vec<String> {
    let mut roots = vec![rt.workspace.to_string_lossy().into_owned()];
    roots.extend(rt.extra_roots.lock().unwrap().iter().cloned());
    roots
}

#[tauri::command]
pub async fn git_diff(
    core: Core<'_>,
    session_id: String,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_roots(&rt);
    let mut files: Vec<crate::git::status::GitDiffFile> = Vec::new();
    for root in &roots {
        // M4：逐根容错
        match crate::git::status::diff_workspace(std::path::Path::new(root)) {
            Ok(mut files_of_root) => {
                for mut f in files_of_root.drain(..) {
                    f.root = Some(root.clone());
                    if let Some(p) = &path {
                        if f.path != *p {
                            continue;
                        }
                    }
                    files.push(f);
                }
            }
            Err(e) => tracing::warn!("git diff 跳过根 {root}：{e}"),
        }
    }
    Ok(serde_json::json!({ "files": files, "truncated": files.len() >= 100 }))
}

#[tauri::command]
pub async fn git_recent_log(
    core: Core<'_>,
    session_id: String,
    n: Option<usize>,
) -> Result<Vec<crate::git::status::GitLogEntry>, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_roots(&rt);
    let n = n.unwrap_or(20);
    let mut merged: Vec<crate::git::status::GitLogEntry> = Vec::new();
    for root in &roots {
        // M4：逐根容错
        match crate::git::status::recent_log(std::path::Path::new(root), n) {
            Ok(entries_of_root) => {
                for mut e in entries_of_root {
                    e.root = Some(root.clone());
                    merged.push(e);
                }
            }
            Err(e) => tracing::warn!("git log 跳过根 {root}：{e}"),
        }
    }
    merged.sort_by_key(|c| std::cmp::Reverse(c.time));
    merged.truncate(n);
    Ok(merged)
}

/// git 提交身份（只读）：带会话时读主目录仓库的 config 快照
///（repo > global > system，与 `git config` 生效优先级一致）；无会话回退
/// 全局 config。只读，无任何写路径。
#[derive(serde::Serialize)]
pub struct GitUserInfo {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[tauri::command]
pub async fn git_user_info(
    core: Core<'_>,
    session_id: Option<String>,
) -> Result<GitUserInfo, String> {
    let dir = session_id
        .as_deref()
        .and_then(|id| core.session(id))
        .map(|rt| rt.workspace.clone());
    let (name, email) = crate::git::status::user_info(dir.as_deref());
    Ok(GitUserInfo { name, email })
}

// ---------- Skills（[docs/p1-plan](../../../../docs/p1-plan.md) §5.1）----------

