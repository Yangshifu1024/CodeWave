use super::util::{err, Core};
use crate::core::config::ConfigState;

/// 列出全部已注册项目。
#[tauri::command]
pub async fn list_projects(
    core: Core<'_>,
) -> Result<Vec<crate::core::projects::ProjectEntry>, String> {
    Ok(crate::core::projects::load(&core.data_dir))
}

/// 保存（新增或更新）项目；data_dir 随保存归一化持久化。
#[tauri::command]
pub async fn save_project(
    core: Core<'_>,
    entry: crate::core::projects::ProjectEntry,
) -> Result<(), String> {
    crate::core::projects::save_project(&core.data_dir, &entry).map_err(err)
}

// ---------- 配置 ----------

/// 读取当前配置（key 脱敏后返回前端）。
#[tauri::command]
pub async fn get_config(core: Core<'_>) -> Result<ConfigState, String> {
    Ok(core.cfg.read().unwrap().sanitized())
}

/// 保存配置：掩码回填 → 落盘 → keyring 迁移 → 日志级别热切换 → 冷却清零。
#[tauri::command]
pub async fn save_config(core: Core<'_>, config: ConfigState) -> Result<(), String> {
    let mut updated = config;
    // [docs/provider-custom-headers](../../../../docs/provider-custom-headers.md)：落盘前校验自定义请求头（头名合法/非保留/不重复、值无换行）
    for p in &updated.providers {
        crate::core::config::validate_request_headers(&p.headers)
            .map_err(|e| format!("供应商「{}」：{e}", p.name))?;
    }
    {
        let current = core.cfg.read().unwrap().clone();
        updated.unmask_from(&current);
    }
    updated.save().map_err(err)?;
    // 明文 key 保存时直接迁入 keyring（不留明文窗口，[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）；失败保留明文并告警
    let (changed, warn) = crate::host::keyring::migrate(&mut updated);
    if let Some(w) = warn {
        tracing::warn!("keyring 迁移未完成：{w}");
    }
    if changed {
        updated.save().map_err(err)?;
    }
    // [docs/session-logging-report](../../../../docs/session-logging-report.md)：日志级别热切换（无条件调用更便宜且自愈漂移；RUST_LOG 覆盖时为 no-op）
    crate::core::logging::apply_config_level(&updated.log.level);
    // 代理热生效：无条件重建共享 client（顺带完成 System 模式重探测；与日志级别同一「便宜且自愈」思路，
    // [docs/network-proxy-settings](../../../../docs/network-proxy-settings.md)）。运行中请求持旧 client clone 不受影响
    *core.client.write().unwrap() = crate::provider::proxy::build_client(&updated);
    *core.cfg.write().unwrap() = updated;
    // [docs/auth-error-guidance](../../../../docs/auth-error-guidance.md)：保存的 key 下次尝试即生效——残留的认证/瞬时冷却不得在用户修好配置后
    // 仍然 failover 到其他 key
    core.key_pool.reset_all();
    Ok(())
}

/// 当前配置解析出的代理 URL（None = 直连）：设置页「系统代理」回显与前端更新检查传参共用
/// （tauri-plugin-updater 的 check 命令原生接受 proxy，[docs/network-proxy-settings](../../../../docs/network-proxy-settings.md)）。
/// proxy = null（从未配置）时返回系统探测结果——null 的实际出网行为就是跟随系统，回显与更新传参同口径。
#[tauri::command]
pub async fn resolve_proxy(core: Core<'_>) -> Result<Option<String>, String> {
    let cfg = core.cfg.read().unwrap().clone();
    match cfg.proxy.as_ref() {
        None => Ok(crate::provider::proxy::system_proxy_url()),
        Some(_) => Ok(crate::provider::proxy::resolve_proxy(&cfg)),
    }
}

/// 列出本机可用 shell（PATH + 常见安装位置探测），供设置面板选择。
/// 探测含子进程 spawn（有界超时），走 spawn_blocking 避免阻塞 async runtime worker。
#[tauri::command]
pub async fn list_available_shells() -> Result<Vec<crate::tools::command::ShellInfo>, String> {
    tauri::async_runtime::spawn_blocking(crate::tools::command::detect_all_shells)
        .await
        .map_err(|e| format!("shell 探测任务异常终止：{e}"))
}

// ---------- 会话 ----------

/// 删除项目：级联删除其全部会话（先取消运行中的）+ 项目托管目录；用户代码目录永不动。
#[tauri::command]
pub async fn delete_project(
    core: Core<'_>,
    project_id: String,
) -> Result<serde_json::Value, String> {
    let sessions: Vec<String> = core
        .store
        .list()
        .into_iter()
        .filter(|m| m.project_id.as_deref() == Some(project_id.as_str()))
        .map(|m| m.id)
        .collect();
    for sid in &sessions {
        match core.session(sid) { Some(rt) => {
            // H5：先标 zombie——即使 run 未及时响应取消，迟到的 checkpoint 也不能复活幽灵会话
            rt.zombie.store(true, std::sync::atomic::Ordering::SeqCst);
            if rt.running.load(std::sync::atomic::Ordering::SeqCst) {
                rt.cancel_active();
                // 等待 run 收尾退出（尽力而为；zombie 保证超时后不复活）
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    while rt.running.load(std::sync::atomic::Ordering::SeqCst) {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                })
                .await;
            }
            core.sessions.remove(sid);
            let _ = core.store.remove(sid);
        } _ => {
            let _ = core.store.remove(sid);
        }}
    }
    let mut n = sessions.len();
    crate::core::projects::delete_project(&core.data_dir, &project_id).map_err(err)?;
    // H5：最终清扫——快照之后同一项目新建的会话一并删除
    let leftovers: Vec<String> = core
        .store
        .list()
        .into_iter()
        .filter(|m| m.project_id.as_deref() == Some(project_id.as_str()))
        .map(|m| m.id)
        .collect();
    for sid in leftovers {
        if let Some(rt) = core.session(&sid) {
            rt.zombie.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        core.sessions.remove(&sid);
        let _ = core.store.remove(&sid);
        n += 1;
    }
    Ok(serde_json::json!({ "deleted_sessions": n }))
}

