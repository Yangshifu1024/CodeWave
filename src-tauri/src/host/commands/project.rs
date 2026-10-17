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
/// 入参用 ProjectSaveInput（allowed_dirs 三态：缺省 = 沿用旧值），落盘仍是 ProjectEntry。
/// 校验在这里做（IPC 命令只做校验 + 转调 core）；**刻意不校验 name 以外的历史脏数据**——
/// 内部回写路径（create_session）不经过本命令，core 层也不拦空名，历史项目仍可建会话。
#[tauri::command]
pub async fn save_project(
    core: Core<'_>,
    entry: crate::core::projects::ProjectSaveInput,
) -> Result<(), String> {
    persist_project(&core.data_dir, &entry)
}

/// 命令体的「先校验、再转调 core」抽成同步函数，让**接线顺序**本身进测试边界：
/// 只测 core 的 `validate_save_input` 纯函数的话，把命令体里这行校验删掉仍然全绿，
/// 而 name 在 core 层是刻意不拦的（历史空名项目要能建会话）——空名项目就会从 IPC 入口直接落盘。
/// （与 `apply_page_save_shape` 同一用意：原缺陷是「入口少做了一次」，只测辅助函数守不住调用点。）
fn persist_project(
    data_dir: &std::path::Path,
    input: &crate::core::projects::ProjectSaveInput,
) -> Result<(), String> {
    crate::core::projects::validate_save_input(&input.name, &input.directory).map_err(err)?;
    crate::core::projects::save_project_input(data_dir, input).map_err(err)
}

// ---------- 配置 ----------

use crate::core::config_commit::{
    ConfigCommitError, ConfigPatch, ConfigSaveResult, ConfigSnapshot, ConfigVersion,
    CredentialEdits,
};

/// The authoritative snapshot includes field-domain versions and startup security warnings.
#[tauri::command]
pub async fn get_config(core: Core<'_>) -> Result<ConfigSnapshot, ConfigCommitError> {
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.config_commits.snapshot(&core.cfg))
        .await
        .map_err(|_| commit_worker_error())
}

#[tauri::command]
pub async fn get_config_operation(
    core: Core<'_>,
    operation_id: String,
) -> Result<ConfigSaveResult, ConfigCommitError> {
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        core.config_commits.operation(&core.cfg, &operation_id)
    })
    .await
    .map_err(|_| commit_worker_error())?
}

fn commit_worker_error() -> ConfigCommitError {
    ConfigCommitError {
        code: "OperationUncertain".into(),
        message:
            "Configuration worker stopped. Query the operation receipt or refresh before retrying"
                .into(),
    }
}

/// All blocking keyring/file operations run on the blocking pool. The coordinator owns validation,
/// immutable credentials, field ownership, revision checks, the commit point and effect selection.
#[tauri::command]
pub async fn save_config(
    core: Core<'_>,
    config: ConfigState,
    expected_version: ConfigVersion,
    operation_id: String,
    credential_edits: CredentialEdits,
    skip_cleanup: Option<bool>,
) -> Result<ConfigSaveResult, ConfigCommitError> {
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut result = core.commit_config(
            ConfigPatch::Page {
                config: Box::new(config),
                expected: expected_version,
                edits: credential_edits.0,
                skip_cleanup: skip_cleanup.unwrap_or(false),
            },
            &operation_id,
        )?;
        if !result.replayed {
            let mut post_completed = true;
            if let Some(days) = result.effects.cleanup_days {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    super::session::execute_cleanup(&core, days)
                })) {
                    Ok(outcome) => result.cleanup = Some(outcome),
                    Err(_) => {
                        post_completed = false;
                        result.snapshot.warnings.push(
                            "Configuration saved; session cleanup failed and needs review".into(),
                        );
                    }
                }
            }
            if post_completed
                && core
                    .config_commits
                    .mark_post_processed(&operation_id)
                    .is_err()
            {
                result
                    .snapshot
                    .warnings
                    .push("Configuration saved; post-processing receipt needs retry".into());
            }
        }
        Ok(result)
    })
    .await
    .map_err(|_| commit_worker_error())?
}

#[tauri::command]
pub async fn set_subagent_model(
    core: Core<'_>,
    role: String,
    model_id: Option<String>,
    expected_version: ConfigVersion,
    operation_id: String,
) -> Result<ConfigSaveResult, ConfigCommitError> {
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        core.commit_config(
            ConfigPatch::Role {
                role,
                model_id,
                expected: expected_version,
            },
            &operation_id,
        )
    })
    .await
    .map_err(|_| commit_worker_error())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // 六个字体字段保持 IPC 命名参数，统一交给原子提交器。
pub async fn set_font_prefs(
    core: Core<'_>,
    sans: String,
    content: String,
    mono: String,
    font_size_sans: f32,
    font_size_content: f32,
    font_size_mono: f32,
    operation_id: Option<String>,
) -> Result<ConfigSaveResult, ConfigCommitError> {
    let core = core.inner().clone();
    let operation_id = operation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    tauri::async_runtime::spawn_blocking(move || {
        core.commit_config(
            ConfigPatch::Fonts {
                sans,
                content,
                mono,
                font_size_sans,
                font_size_content,
                font_size_mono,
            },
            &operation_id,
        )
    })
    .await
    .map_err(|_| commit_worker_error())?
}

#[cfg(test)]
fn cleanup_due(old: Option<u32>, new: Option<u32>, skip: Option<bool>) -> Option<u32> {
    new.filter(|d| {
        old != new
            && skip != Some(true)
            && crate::core::sessions::cleanup::is_valid_retention_days(*d)
    })
}
#[cfg(test)]
fn apply_font_prefs(
    ui: &mut crate::core::config::UiPrefs,
    sans: &str,
    content: &str,
    mono: &str,
    font_size_sans: f32,
    font_size_content: f32,
    font_size_mono: f32,
) {
    ui.font_sans = sans.trim().into();
    ui.font_content = content.trim().into();
    ui.font_mono = mono.trim().into();
    ui.font_size_sans = font_size_sans;
    ui.font_size_content = font_size_content;
    ui.font_size_mono = font_size_mono;
}
#[cfg(test)]
fn apply_page_save_shape(updated: &mut ConfigState, current: &ConfigState) {
    crate::core::config_commit::preserve_instant_fields(updated, current);
}

/// 当前配置解析出的代理 URL（None = 直连）：设置页「系统代理」回显与前端更新检查传参共用
/// （tauri-plugin-updater 的 check 命令原生接受 proxy，[docs/providers-and-network](../../../../docs/providers-and-network.md)）。
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
    // #36-C0 前置检查：本进程正在跑的**不算冲突**，别的进程占着才是。
    // 删掉别人正在写的历史是不可逆的，而"稍后再删"随时可以做。
    let foreign: Vec<&String> = sessions
        .iter()
        .filter(|sid| core.store.session_held_by_other(sid))
        .collect();
    if !foreign.is_empty() {
        return Err(format!(
            "项目正被其它实例占用，无法删除（会话：{}）。请先关闭那边的 CodeWave 再试。",
            foreign
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("、")
        ));
    }
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
                core.mcp.release_session(sid).await;
                core.sessions.remove(sid);
                let _ = core.store.remove(sid);
            }
            _ => {
                let _ = core.store.remove(sid);
            }
        }
    }
    let mut n = sessions.len();
    core.tasks.delete_project(&project_id).await.map_err(err)?;
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
        core.mcp.release_session(&sid).await;
        core.sessions.remove(&sid);
        let _ = core.store.remove(&sid);
        n += 1;
    }
    // LSP 机制已随 post-write-check 删除：项目删除不再需要关闭常驻 server
    Ok(serde_json::json!({ "deleted_sessions": n }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ConfigState;

    /// 字体偏好只改界面、正文、代码三组字体与字号，
    /// 且字串两侧都 trim（空串 = 默认链）。
    #[test]
    fn font_prefs_patch_only_font_fields() {
        let mut cfg = ConfigState::default();
        cfg.ui.language = "en-US".into();
        cfg.ui.close_to_tray = false;

        apply_font_prefs(
            &mut cfg.ui,
            "  PingFang SC ",
            "  Noto Sans SC ",
            " JetBrains Mono ",
            16.0,
            18.0,
            14.0,
        );
        assert_eq!(cfg.ui.font_sans, "PingFang SC");
        assert_eq!(cfg.ui.font_content, "Noto Sans SC");
        assert_eq!(cfg.ui.font_mono, "JetBrains Mono");
        assert_eq!(cfg.ui.font_size_sans, 16.0);
        assert_eq!(cfg.ui.font_size_content, 18.0);
        assert_eq!(cfg.ui.font_size_mono, 14.0);
        // 同段其他字段一律不动（命令是即时生效的，不得顺手改写别人的偏好）
        assert_eq!(cfg.ui.language, "en-US");
        assert!(!cfg.ui.close_to_tray);

        // 清空 = 恢复默认链；三组字号分别恢复 14 / 14 / 12。
        apply_font_prefs(&mut cfg.ui, "", "", "", 14.0, 14.0, 12.0);
        assert_eq!(cfg.ui.font_sans, "");
        assert_eq!(cfg.ui.font_content, "");
        assert_eq!(cfg.ui.font_mono, "");
        assert_eq!(cfg.ui.font_size_sans, 14.0);
        assert_eq!(cfg.ui.font_size_content, 14.0);
        assert_eq!(cfg.ui.font_size_mono, 12.0);
    }

    /// [docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)：保留期变化时触发清理；
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

    /// 页级保存的形状修正（调用点同一函数）：字体 6 项（族×3、字号×3）按当前生效值护住，
    /// 其他字段照常采用提交值。
    /// 🔴 审查发现：不护住就会被「改完字体 → 保存其他设置 → 重启」静默回滚。
    #[test]
    fn page_save_shape_preserves_live_font_prefs_only() {
        // 当前生效值（用户刚改完、已由 set_font_prefs 落盘）
        let mut current = ConfigState::default();
        apply_font_prefs(
            &mut current.ui,
            "PingFang SC",
            "Noto Sans SC",
            "JetBrains Mono",
            17.0,
            18.0,
            14.0,
        );
        // 前端提交的整份配置：字体还是「打开设置页时」的旧值（空），其他字段是用户的新改动
        let mut updated = ConfigState::default();
        updated.ui.language = "en-US".into();
        updated.ui.ai_language = Some("English".into());
        updated.compact_threshold = 0.8;

        apply_page_save_shape(&mut updated, &current);

        assert_eq!(updated.ui.font_sans, "PingFang SC");
        assert_eq!(updated.ui.font_content, "Noto Sans SC");
        assert_eq!(updated.ui.font_mono, "JetBrains Mono");
        assert_eq!(updated.ui.font_size_sans, 17.0);
        assert_eq!(updated.ui.font_size_content, 18.0);
        assert_eq!(updated.ui.font_size_mono, 14.0);
        // 同段其他字段照常采用本次提交的值（护住的只有字体六项）
        assert_eq!(updated.ui.language, "en-US");
        assert_eq!(updated.ui.ai_language.as_deref(), Some("English"));
        assert_eq!(updated.compact_threshold, 0.8);
    }

    /// save_project 命令体的接线（命令调用的同一个函数）：空名 / 空主目录必须在**落盘前**被拦下。
    /// 🔴 防的回归：把命令体的校验那行删掉——只测 core 纯函数时全绿，而 name 在 core 层刻意不拦，
    /// 空名项目就能从 IPC 入口落盘（L5「save_project 后端零校验」的回归入口）。
    #[test]
    fn save_project_command_validates_before_writing() {
        // 主目录也放临时目录里：万一校验被删，落盘只会脏到 tempdir，绝不碰用户目录或进程 cwd
        let data_dir = tempfile::tempdir().expect("临时数据目录");
        let home = tempfile::tempdir().expect("临时项目主目录");
        let input = |name: &str, directory: &str| crate::core::projects::ProjectSaveInput {
            id: "p1".into(),
            name: name.into(),
            directory: directory.into(),
            data_dir: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            allowed_dirs: None,
        };
        let managed = home.path().join(crate::core::config::MANAGED_DIR_NAME);
        let index = data_dir.path().join("projects").join("directories.json");

        // 空项目名（含全空白）
        for name in ["", "   "] {
            let e = persist_project(data_dir.path(), &input(name, home.path().to_str().unwrap()))
                .expect_err("空项目名必须被拦下");
            assert!(e.contains("项目名称"), "应报名称错，实际：{e}");
        }
        // 空主目录（含全空白）
        for dir in ["", "   "] {
            let e = persist_project(data_dir.path(), &input("真实项目", dir))
                .expect_err("空项目主目录必须被拦下");
            assert!(e.contains("项目主目录"), "应报主目录错，实际：{e}");
        }

        // 失败路径无副作用：既没写 project.json（连托管目录都没建），也没登记发现索引
        assert!(
            !managed.join("project.json").exists(),
            "校验失败不得留下 project.json"
        );
        assert!(!index.exists(), "校验失败不得写入发现索引");
        assert!(
            crate::core::projects::load(data_dir.path()).is_empty(),
            "校验失败后不得有项目可被列出"
        );

        // 合法值照常落盘（否则上面前面的断言会因「压根没写」而失去意义）
        persist_project(
            data_dir.path(),
            &input("真实项目", home.path().to_str().unwrap()),
        )
        .expect("合法入参必须保存成功");
        assert!(
            managed.join("project.json").is_file(),
            "合法入参必须落 project.json"
        );
        assert!(index.is_file(), "合法入参必须登记发现索引");
        let listed = crate::core::projects::load(data_dir.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "真实项目");
    }

    /// 缺少新增正文组字段时使用默认值，已有合法界面/代码显式值保留。
    #[test]
    fn old_config_without_font_fields_still_deserializes() {
        let json = r#"{ "schema_version": 2, "ui": { "font_size_sans": 15.0, "font_size_mono": 15.0, "language": "zh-CN" } }"#;
        let cfg: ConfigState = serde_json::from_str(json).expect("缺字体字段的旧配置必须可读");
        assert_eq!(cfg.ui.font_sans, "");
        assert_eq!(cfg.ui.font_content, "");
        assert_eq!(cfg.ui.font_mono, "");
        assert_eq!(cfg.ui.font_size_sans, 15.0);
        assert_eq!(cfg.ui.font_size_content, 14.0);
        assert_eq!(cfg.ui.font_size_mono, 15.0);
    }
}
