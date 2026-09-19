use super::util::{Core, err};
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
/// 会话保留期变化且未跳过时，保存后再跑一次会话清理并把结果回传
///（[docs/session-cleanup](../../../../docs/session-cleanup.md)：前端先预览/确认，取消时传 skip_cleanup = true）。
/// 返回 `None` = 本次没有触发清理（保留期未变 / 明确跳过 / 新保留期为「不清理」）。
#[tauri::command]
pub async fn save_config(
    core: Core<'_>,
    config: ConfigState,
    skip_cleanup: Option<bool>,
) -> Result<Option<crate::core::sessions::CleanupOutcome>, String> {
    let mut updated = config;
    // [docs/provider-custom-headers](../../../../docs/provider-custom-headers.md)：落盘前校验自定义请求头（头名合法/非保留/不重复、值无换行）
    for p in &updated.providers {
        crate::core::config::validate_request_headers(&p.headers)
            .map_err(|e| format!("供应商「{}」：{e}", p.name))?;
    }
    {
        let current = core.cfg.read().unwrap().clone();
        apply_page_save_shape(&mut updated, &current);
    }
    // 保留期是否变化必须在覆盖内存配置**之前**比较（下面会写 core.cfg）
    let cleanup_days = cleanup_due(
        core.cfg.read().unwrap().sessions.retention_days,
        updated.sessions.retention_days,
        skip_cleanup,
    );
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
    let Some(days) = cleanup_days else {
        return Ok(None);
    };
    // 清理要删文件（阻塞 IO）：放到阻塞线程上跑，不占住 async worker 让其它命令排队
    //（与启动清理同一处理）。行为不变：仍然等出结果再返回。
    // 清理自己炸掉时配置已经落盘——只记 warning 并按「本次不清理」回答，
    // 不让前端把保存当失败（宁可不报清理结果，也不误报保存失败）。
    let cleanup_core = core.inner().clone();
    match tauri::async_runtime::spawn_blocking(move || {
        super::session::execute_cleanup(&cleanup_core, days)
    })
    .await
    {
        Ok(outcome) => Ok(Some(outcome)),
        Err(e) => {
            tracing::warn!("会话保留期清理任务异常结束（配置已保存，本次不清理）：{e}");
            Ok(None)
        }
    }
}

/// 保存配置后是否需要执行清理：保留期**确实变化**、未明确跳过、且新保留期是合法档位时
/// 返回要用的天数（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 12/27 条）。
/// 0 与其他白名单之外的怪值一律不触发（手上改配置/旧值都不该删数据）。
/// 抽成纯函数是为了让判断本身可单测（命令体需要 Tauri State，测不到）。
fn cleanup_due(
    old_days: Option<u32>,
    new_days: Option<u32>,
    skip_cleanup: Option<bool>,
) -> Option<u32> {
    let new = new_days?;
    if !crate::core::sessions::cleanup::is_valid_retention_days(new)
        || skip_cleanup == Some(true)
        || old_days == new_days
    {
        return None;
    }
    Some(new)
}

/// 只写字体偏好（界面字体 / 等宽字体）并**落盘**。
///
/// 为什么单独一条命令：字体是「即改即生效」的 UI 偏好，不该拖到页级「保存」才生效；
/// 也就不能走 `save_config`（那是整份覆盖，会把用户还没保存的其他改动一并写进去）。
/// 与 `lsp_enable` 同一纪律：**先落盘再改内存**，且只 patch 这两个字段。
/// 值净化（引号/控制字符、折叠空白）由前端 `utils/fonts.ts` 负责，这里只做 trim。
#[tauri::command]
pub async fn set_font_prefs(core: Core<'_>, sans: String, mono: String) -> Result<(), String> {
    let mut snapshot = core.cfg.read().unwrap().clone();
    apply_font_prefs(&mut snapshot.ui, &sans, &mono);
    snapshot.save().map_err(err)?;
    {
        let mut guard = core.cfg.write().unwrap();
        apply_font_prefs(&mut guard.ui, &sans, &mono);
    }
    tracing::debug!("字体偏好已落盘");
    Ok(())
}

/// 字体偏好写入 `ui` 段（空串 = 默认字体链）。
fn apply_font_prefs(ui: &mut crate::core::config::UiPrefs, sans: &str, mono: &str) {
    ui.font_sans = sans.trim().to_string();
    ui.font_mono = mono.trim().to_string();
}

/// 页级保存的形状修正：掩码回填（不回写脱敏占位）+ 护住由专门命令独占维护的字段。
///
/// 抽成独立函数是为了让**调用点**也进测试边界：原缺陷正是「页级保存少护了一次」，
/// 只测 preserve 辅助函数的话，删掉调用点仍然全绿。
fn apply_page_save_shape(updated: &mut ConfigState, current: &ConfigState) {
    updated.unmask_from(current);
    // 字体偏好由 `set_font_prefs` 独占维护（即时生效、不进草稿）：页级「保存」不得用
    // 「打开设置页时的旧快照」覆盖它（否则「改完字体 → 保存其他设置 → 重启」会静默回滚）
    updated.ui.font_sans = current.ui.font_sans.clone();
    updated.ui.font_mono = current.ui.font_mono.clone();
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
        match core.session(sid) {
            Some(rt) => {
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
            }
            _ => {
                let _ = core.store.remove(sid);
            }
        }
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
    // LSP：关掉该项目的全部 server 实例（项目删除会级联删会话，绝不留孤儿 server）
    core.lsp.shutdown_project(&project_id).await;
    Ok(serde_json::json!({ "deleted_sessions": n }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ConfigState;

    /// 字体偏好只改 `ui.font_sans` / `ui.font_mono`，且两侧都 trim（空串 = 默认链）。
    #[test]
    fn font_prefs_patch_only_font_fields() {
        let mut cfg = ConfigState::default();
        cfg.ui.language = "en-US".into();
        cfg.ui.font_size = 17.0;
        cfg.ui.close_to_tray = false;

        apply_font_prefs(&mut cfg.ui, "  PingFang SC ", " JetBrains Mono ");
        assert_eq!(cfg.ui.font_sans, "PingFang SC");
        assert_eq!(cfg.ui.font_mono, "JetBrains Mono");
        // 同段其他字段一律不动（命令是即时生效的，不得顺手改写别人的偏好）
        assert_eq!(cfg.ui.language, "en-US");
        assert_eq!(cfg.ui.font_size, 17.0);
        assert!(!cfg.ui.close_to_tray);

        // 清空 = 恢复默认链（存空串，不删字段）
        apply_font_prefs(&mut cfg.ui, "", "");
        assert_eq!(cfg.ui.font_sans, "");
        assert_eq!(cfg.ui.font_mono, "");
    }

    /// [docs/session-cleanup](../../../../docs/session-cleanup.md)：保留期变化时触发清理；
    /// 未变化 / 跳过 / 改成「不清理」/ 非法天数都不触发。
    #[test]
    fn cleanup_due_only_when_retention_changes() {
        assert_eq!(
            cleanup_due(None, Some(7), None),
            Some(7),
            "从「不清理」改成 7 天"
        );
        assert_eq!(
            cleanup_due(Some(3), Some(1), None),
            Some(1),
            "收紧保留期也要清"
        );
        assert_eq!(cleanup_due(Some(7), Some(7), None), None, "没变不做事");
        assert_eq!(cleanup_due(None, None, None), None);
        assert_eq!(
            cleanup_due(Some(7), None, None),
            None,
            "改成「不清理」不做事"
        );
        assert_eq!(
            cleanup_due(None, Some(7), Some(true)),
            None,
            "用户选了「暂不清理」"
        );
        assert_eq!(cleanup_due(None, Some(7), Some(false)), Some(7));
        assert_eq!(cleanup_due(None, Some(0), None), None, "非法天数不触发");
        assert_eq!(
            cleanup_due(None, Some(2), None),
            None,
            "白名单之外的档位（1/3/7/14/30）不触发"
        );
        assert_eq!(
            cleanup_due(None, Some(30), None),
            Some(30),
            "合法档位照常触发"
        );
    }

    /// 页级保存的形状修正（调用点同一函数）：字体按当前生效值护住，其他字段照常采用提交值。
    /// 🔴 审查发现：不护住就会被「改完字体 → 保存其他设置 → 重启」静默回滚。
    #[test]
    fn page_save_shape_preserves_live_font_prefs_only() {
        // 当前生效值（用户刚改完、已由 set_font_prefs 落盘）
        let mut current = ConfigState::default();
        apply_font_prefs(&mut current.ui, "PingFang SC", "JetBrains Mono");
        // 前端提交的整份配置：字体还是「打开设置页时」的旧值（空），其他字段是用户的新改动
        let mut updated = ConfigState::default();
        updated.ui.font_size = 18.0;
        updated.ui.language = "en-US".into();
        updated.ui.ai_language = Some("English".into());
        updated.compact_threshold = 0.8;

        apply_page_save_shape(&mut updated, &current);

        assert_eq!(updated.ui.font_sans, "PingFang SC");
        assert_eq!(updated.ui.font_mono, "JetBrains Mono");
        // 同段其他字段照常采用本次提交的值（护住的只有字体两项）
        assert_eq!(updated.ui.font_size, 18.0);
        assert_eq!(updated.ui.language, "en-US");
        assert_eq!(updated.ui.ai_language.as_deref(), Some("English"));
        assert_eq!(updated.compact_threshold, 0.8);
    }

    /// 旧配置没这两个字段也必须能读（serde default 向前兼容，不得报错）。
    #[test]
    fn old_config_without_font_fields_still_deserializes() {
        let json = r#"{ "schema_version": 2, "ui": { "font_size": 15.0, "language": "zh-CN" } }"#;
        let cfg: ConfigState = serde_json::from_str(json).expect("缺字体字段的旧配置必须可读");
        assert_eq!(cfg.ui.font_sans, "");
        assert_eq!(cfg.ui.font_mono, "");
        assert_eq!(cfg.ui.font_size, 15.0);
    }
}
