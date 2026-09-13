use super::util::{err, Core};
use tauri::AppHandle;

/// 弹出系统目录选择框，返回所选目录（取消为 None）。
#[tauri::command]
pub async fn select_workspace_dir(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    // M15 修复：阻塞式对话框放到阻塞线程执行
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| format!("dialog join: {e}"))?;
    Ok(picked
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

/// 工作区内按关键字模糊搜索文件路径（@ 提及/文件联想用，默认上限 30）。
#[tauri::command]
pub async fn search_workspace_paths(
    core: Core<'_>,
    session_id: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    Ok(crate::tools::list_files::search_workspace_paths(
        &rt,
        &query,
        limit.unwrap_or(30).min(80),
    ))
}

/// 会话的全部可写根（主目录 + extra_roots + data_dir）。
/// host 层文件读/写/列的唯一构造点——评审 C1：此前三处各自构造丢了 extra_roots，
/// 多根项目的文件树/编辑器完全无法访问 extra 根。
fn session_write_roots(
    rt: &crate::core::agent::SessionRuntime,
) -> crate::tools::pathutil::WriteRoots {
    let extra = rt
        .extra_roots
        .lock()
        .unwrap()
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    crate::tools::pathutil::WriteRoots {
        workspace: rt.workspace.clone(),
        extra,
        data_dir: rt.data_dir.clone(),
    }
}

/// 读工作区文本文件（根校验 + 1MB 上限）。
#[tauri::command]
pub async fn read_workspace_file(
    core: Core<'_>,
    session_id: String,
    path: String,
) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    let resolved = crate::tools::pathutil::resolve_read(&roots, &path)
        .map_err(|(c, m)| format!("{c}: {m}"))?;
    let meta = std::fs::metadata(&resolved).map_err(err)?;
    if meta.len() > 1024 * 1024 {
        return Err("文件超过 1MB，请在编辑器外查看".into());
    }
    let bytes = std::fs::read(&resolved).map_err(err)?;
    Ok(serde_json::json!({
        "path": path,
        "size": meta.len(),
        "content": String::from_utf8_lossy(&bytes),
    }))
}

/// 保存工作区文本文件（根校验 + 原子写）。
#[tauri::command]
pub async fn save_workspace_file(
    core: Core<'_>,
    session_id: String,
    path: String,
    content: String,
) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    let resolved = crate::tools::pathutil::resolve_write(&roots, &path)
        .map_err(|(c, m)| format!("{c}: {m}"))?;
    crate::util::atomic::atomic_write(&resolved, content.as_bytes()).map_err(err)
}

/// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：本会话的产物清单（create/edit 边车登记 + 存在性/大小 stat），按 last_at 倒序。
#[tauri::command]
pub async fn list_session_files(
    core: Core<'_>,
    session_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let owner = rt.root_session_id.clone().unwrap_or_else(|| rt.id.clone());
    Ok(session_files_payload(&core.store, &owner))
}

/// 清单体（独立成函数便于测试）：读边车 → 按 last_at 倒序 → stat 补 exists/size。
fn session_files_payload(
    store: &crate::core::sessions::SessionStore,
    owner: &str,
) -> Vec<serde_json::Value> {
    let mut items = store.load_artifacts(owner);
    items.sort_by(|a, b| b.last_at.cmp(&a.last_at));
    items
        .into_iter()
        .map(|a| {
            let (exists, size) = std::fs::metadata(&a.path)
                .map(|m| (true, m.len()))
                .unwrap_or((false, 0));
            serde_json::json!({
                "path": a.path,
                "first_op": a.first_op,
                "last_op": a.last_op,
                "first_at": a.first_at,
                "last_at": a.last_at,
                "count": a.count,
                "exists": exists,
                "size": size,
            })
        })
        .collect()
}

/// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：二进制文件读取（base64，供图片预览）；根校验与 1MB 上限同 read_workspace_file。
#[tauri::command]
pub async fn read_workspace_file_base64(
    core: Core<'_>,
    session_id: String,
    path: String,
) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let roots = session_write_roots(&rt);
    read_file_base64(&roots, &path)
}

/// base64 读取体（独立成函数便于测试）。
fn read_file_base64(
    roots: &crate::tools::pathutil::WriteRoots,
    path: &str,
) -> Result<serde_json::Value, String> {
    let resolved =
        crate::tools::pathutil::resolve_read(roots, path).map_err(|(c, m)| format!("{c}: {m}"))?;
    let meta = std::fs::metadata(&resolved).map_err(err)?;
    if meta.len() > 1024 * 1024 {
        return Err("文件超过 1MB，无法预览".into());
    }
    let bytes = std::fs::read(&resolved).map_err(err)?;
    use base64::Engine as _;
    Ok(serde_json::json!({
        "path": path,
        "size": meta.len(),
        "content": base64::engine::general_purpose::STANDARD.encode(&bytes),
    }))
}

// ---------- git / 上下文 ----------

/// 列出工作区目录：多根项目空路径返回虚拟顶层（每根一条目，路径为绝对路径供前端继续下钻）。

#[tauri::command]
pub async fn list_workspace_dir(
    core: Core<'_>,
    session_id: String,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let extra: Vec<String> = rt.extra_roots.lock().unwrap().clone();
    let mut all_roots = vec![rt.workspace.to_string_lossy().into_owned()];
    all_roots.extend(extra.iter().cloned());
    let roots = session_write_roots(&rt);
    let roots_json = serde_json::json!(all_roots);

    // 多根项目：空路径 → 虚拟顶层（每根一条目，path 是绝对路径供前端继续下钻）
    let is_empty_path = path.as_deref().map(|p| p.is_empty()).unwrap_or(true);
    if is_empty_path && !extra.is_empty() {
        let entries: Vec<serde_json::Value> = all_roots
            .iter()
            .map(|r| {
                let name = std::path::Path::new(r)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.clone());
                serde_json::json!({ "name": name, "is_dir": true, "path": r, "virtual_root": true })
            })
            .collect();
        return Ok(
            serde_json::json!({ "entries": entries, "roots": roots_json, "virtual_top": true }),
        );
    }

    let base = match &path {
        Some(p) if !p.is_empty() => {
            crate::tools::pathutil::resolve_read(&roots, p).map_err(|(c, m)| format!("{c}: {m}"))?
        }
        _ => rt.workspace.clone(),
    };
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            entries.push(serde_json::json!({ "name": name, "is_dir": e.path().is_dir() }));
        }
    }
    Ok(serde_json::json!({ "entries": entries, "roots": roots_json }))
}

// ---------- 计划任务（P2-G）----------


#[cfg(test)]
mod tests {
    use super::*;

    /// C1 回归：host 层文件命令构造的 WriteRoots 必须包含 extra_roots——
    /// 多根项目的文件树/编辑器依赖它允许对 extra 根 list/read/save。
    #[test]
    fn session_write_roots_include_extra() {
        let ws = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let rt = crate::core::agent::test_support::make_runtime(
            std::fs::canonicalize(ws.path()).unwrap(),
            std::fs::canonicalize(dd.path()).unwrap(),
            vec![extra.path().to_string_lossy().into_owned()],
        );
        let roots = session_write_roots(&rt);
        let extra_abs = extra.path().to_string_lossy().into_owned();
        assert_eq!(roots.extra, vec![extra_abs]);
        // Explorer 流程：绝对路径（虚拟顶层签发的 key）可读可写
        let f = extra.path().join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let abs = f.to_string_lossy().into_owned();
        assert!(crate::tools::pathutil::resolve_read(&roots, &abs).is_ok());
        assert!(crate::tools::pathutil::resolve_write(&roots, &abs).is_ok());
        // 相对路径跨根回退命中 extra
        assert!(crate::tools::pathutil::resolve_read(&roots, "a.txt").is_ok());
        // 越界仍然拒绝
        let out = tempfile::tempdir().unwrap();
        let outside = out.path().join("x").to_string_lossy().into_owned();
        assert_eq!(
            crate::tools::pathutil::resolve_read(&roots, &outside)
                .unwrap_err()
                .0,
            "E_PATH_OUTSIDE"
        );
    }

    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：产物清单体——按 last_at 倒序 + stat 补 exists/size；缺失文件 exists=false。
    #[test]
    fn session_files_payload_sorts_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::core::sessions::SessionStore::new(dir.path().to_path_buf());
        store
            .append_artifact(
                "s1",
                "/ws/old.md",
                crate::core::sessions::ArtifactOp::Create,
            )
            .unwrap();
        store
            .append_artifact("s1", "/ws/new.md", crate::core::sessions::ArtifactOp::Edit)
            .unwrap();
        // real.md 真实存在；/ws/*.md 有意缺失（验证 exists=false）
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("real.md"), b"hello").unwrap();
        store
            .append_artifact(
                "s1",
                &ws.path().join("real.md").to_string_lossy(),
                crate::core::sessions::ArtifactOp::Create,
            )
            .unwrap();
        let payload = session_files_payload(&store, "s1");
        assert_eq!(payload.len(), 3);
        // 倒序：最后登记的 real.md 排最前
        assert_eq!(
            payload[0]["path"],
            serde_json::json!(ws.path().join("real.md").to_string_lossy().into_owned())
        );
        assert_eq!(payload[0]["exists"], serde_json::json!(true));
        assert_eq!(payload[0]["size"], serde_json::json!(5));
        // 不存在的路径：exists=false、size=0
        let missing = payload.iter().find(|p| p["path"] == "/ws/old.md").unwrap();
        assert_eq!(missing["exists"], serde_json::json!(false));
        assert_eq!(missing["size"], serde_json::json!(0));
        // op 序列化契约：create / edit
        assert_eq!(payload[0]["first_op"], serde_json::json!("create"));
    }

    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：base64 读取——roots 外拒绝、内容编码正确、二进制安全。
    #[test]
    fn read_file_base64_bounds_and_encoding() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        // 1x1 透明 PNG 头（含非 UTF-8 字节，验证二进制安全）
        let png: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(ws.path().join("img.png"), png).unwrap();
        let out = read_file_base64(&roots, "img.png").unwrap();
        assert_eq!(out["size"], serde_json::json!(8));
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(out["content"].as_str().unwrap())
            .unwrap();
        assert_eq!(decoded, png);
        // roots 外拒绝
        let outside = tempfile::tempdir().unwrap();
        let p = outside.path().join("x.png").to_string_lossy().into_owned();
        let e = read_file_base64(&roots, &p).unwrap_err();
        assert!(e.starts_with("E_PATH_OUTSIDE"), "{e}");
    }
}

