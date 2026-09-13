// 注意：部分字段/方法为 P1/P2 预留接口（[docs/p1-plan](../../docs/p1-plan.md)、05）；P0 期间允许 dead_code。
#![allow(dead_code)]

// CodeWave 后端装配（G1-G10）：插件注册、状态管理、命令注册。
// 分层规则（[docs/technical-design](../../docs/technical-design.md) §2.2）：仅本文件与 host/ 模块允许 use tauri::*。

pub mod agents;
mod core;
mod git;
mod host;
pub mod mcp;
pub mod memory;
pub mod provider;
pub mod safety;
pub mod skills;
mod tools;
mod util;

use core::agent::AgentCore;
use core::config::ConfigState;
use core::sessions::SessionStore;
use host::events::{ChannelRegistry, TauriSink};
use std::sync::Arc;

/// 读取当前全局配置的快照（托盘/菜单事件等无 State 上下文处用；core 未就绪返回默认值）。
fn core_cfg(app: &tauri::AppHandle) -> crate::core::config::ConfigState {
    use tauri::Manager;
    app.try_state::<Arc<AgentCore>>()
        .map(|c| c.cfg.read().unwrap().clone())
        .unwrap_or_default()
}

/// 应用入口：初始化日志/插件/状态/命令/托盘/菜单，进入 Tauri 事件循环。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri::Manager;
    core::logging::init();
    let channels = Arc::new(ChannelRegistry::default());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // 自绘标题栏（[docs/custom-font-and-titlebar](../../docs/custom-font-and-titlebar.md)）：必须在任何 webview 创建之前注册
        .plugin(tauri_plugin_decoration::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            use tauri::Manager;
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.unminimize();
                let _ = win.set_focus();
            }
        }))
        .manage(channels.clone())
        .setup(move |app| {
            let handle = app.handle().clone();
            let sink = Arc::new(TauriSink::new(handle, channels.clone()));
            let mut cfg = ConfigState::load();
            // keyring 迁移（[docs/p1-plan](../../docs/p1-plan.md) §7.2）：明文 key 迁入系统钥匙串；失败保留明文
            let (changed, warn) = host::keyring::migrate(&mut cfg);
            if let Some(w) = warn {
                tracing::warn!("keyring 迁移未完成：{w}");
            }
            if changed {
                let _ = cfg.save();
            }
            // 日志级别跟随 config（RUST_LOG 环境变量在场时优先）+ 过期滚动日志清理（[docs/session-logging-report](../../docs/session-logging-report.md)）
            core::logging::apply_config_level(&cfg.log.level);
            core::logging::prune_expired_logs();
            let client = provider::proxy::build_client(&cfg);
            let data_dir = crate::core::config::data_dir();
            let store = Arc::new(SessionStore::new(data_dir.clone()));
            // 存量修复：checkpoint 曾把 sub_*/task_* 运行写进主索引（untitled 幽灵会话）——启动时清扫
            store.purge_non_session_entries();
            let core = Arc::new(AgentCore::new(cfg, sink, store, client, data_dir));
            // supervisor 在 tauri async runtime 上启动（setup 同步上下文不能直接 tokio::spawn）
            // shell 探测预热（探测含子进程 spawn，后台执行不阻塞启动；PROBED 缓存后零开销）：
            // setup 同步上下文不能直接 tokio::spawn，走 tauri::async_runtime（踩坑清单）
            tauri::async_runtime::spawn(async move {
                let _ = tauri::async_runtime::spawn_blocking(crate::tools::command::detect_all_shells).await;
            });
            let sup_core = core.clone();
            tauri::async_runtime::spawn(async move {
                crate::core::scheduler::start_supervisor(sup_core).await;
            });
            app.manage(core);

            // 托盘（P2-I）：显示主窗口 / 新建会话（聚焦）/ 退出
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
            let show = MenuItem::with_id(app, "show", "显示 CodeWave", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("CodeWave")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                })
                .build(app)?;

            // 关闭到托盘（设置 ui.close_to_tray，默认开——托盘常驻）
            let win = app.get_webview_window("main").unwrap();
            let app_handle = app.handle().clone();
            // 自绘标题栏显示窗口看门狗（[docs/custom-font-and-titlebar](../../docs/custom-font-and-titlebar.md) 评审 Y1）：窗口以 visible:false 起动，依赖
            // activate_and_show 在前端挂载后显示；若前端 JS 启动失败（dev 编译报错等）
            // invoke 永不触发 → 5 秒仍不可见则强制显示（原生装饰回退路径仍可用）
            let watchdog_win = win.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if !watchdog_win.is_visible().unwrap_or(true) {
                    let _ = watchdog_win.show();
                }
            });
            win.on_window_event(move |e| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    let cfg = core_cfg(&app_handle);
                    if cfg.ui.close_to_tray {
                        api.prevent_close();
                        if let Some(w) = app_handle.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                }
            });

            // macOS 应用菜单（「关于」对话框批次后续）：CodeWave 自带 Tauri 的
            // 默认菜单，其应用子菜单缺少「关于/设置/检查更新」条目。
            // 构建一个最小原生应用菜单（Rust 侧，与托盘同侧）并经 set_as_app_menu 安装
            //（只 build 不安装不会挂载任何东西——默认菜单仍在，
            // 这正是第一版只显示默认 About/Services/Hide/Quit 的原因）。
            // 三个条目全部为自定义项，经 menu:action 路由到 webview，
            // 因此「关于」打开的是应用内 AboutModal（而非原生面板）。AboutMetadata 保留备用。
            // Edit 预定义条目保住 webview 文本快捷键（macOS 上的复制/粘贴/全选）。
            // SubmenuBuilder.text() 无法附加快捷键（tauri 内部写死 None），快捷键条目改用 MenuItemBuilder。
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{AboutMetadataBuilder, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
                use tauri::Emitter;
                let lang = core_cfg(app.handle()).ui.language;
                let (about_label, settings_label, check_updates_label, edit_label, window_label) = if lang.starts_with("en") {
                    ("About CodeWave…", "Settings…", "Check for Updates…", "Edit", "Window")
                } else {
                    ("关于 CodeWave…", "设置…", "检查更新…", "编辑", "窗口")
                };
                let _meta = AboutMetadataBuilder::new()
                    .version(Some(env!("CARGO_PKG_VERSION").to_string()))
                    .authors(Some(vec!["yangshifu".to_string()]))
                    .build();
                let about_item = MenuItemBuilder::with_id("menu-about", about_label).build(app)?;
                let settings_item = MenuItemBuilder::with_id("menu-settings", settings_label)
                    .accelerator("CmdOrCtrl+Comma")
                    .build(app)?;
                let check_updates_item = MenuItemBuilder::with_id("menu-check-updates", check_updates_label)
                    .accelerator("CmdOrCtrl+Shift+U")
                    .build(app)?;
                let app_submenu = SubmenuBuilder::new(app, "CodeWave")
                    .item(&about_item)
                    .separator()
                    .item(&settings_item)
                    .item(&check_updates_item)
                    .separator()
                    .hide()
                    .hide_others()
                    .show_all()
                    .separator()
                    .quit()
                    .build()?;
                let edit_submenu = SubmenuBuilder::new(app, edit_label)
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;
                let window_submenu = SubmenuBuilder::new(app, window_label)
                    .minimize()
                    .maximize()
                    .separator()
                    .close_window()
                    .build()?;
                let menu = MenuBuilder::new(app)
                    .item(&app_submenu)
                    .item(&edit_submenu)
                    .item(&window_submenu)
                    .build()?;
                // 第一版漏掉的步骤：把菜单挂载为应用菜单，
                // 替换 Tauri 默认菜单（否则默认菜单仍在显示）。
                menu.set_as_app_menu()?;
                // 自定义动作路由到前端（契约测试会扫描 emit("x:y") 字面量）
                let menu_handle = app.handle().clone();
                app.on_menu_event(move |_app, event| match event.id.as_ref() {
                    "menu-about" | "menu-settings" | "menu-check-updates" => {
                        let action = event.id.as_ref();
                        let _ = menu_handle.emit_to(
                            tauri::EventTarget::labeled("main"),
                            "menu:action",
                            serde_json::json!({ "action": action }),
                        );
                    }
                    _ => {}
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            host::commands::ping,
            host::commands::restart_app,
            host::commands::activate_and_show,
            host::commands::get_config,
            host::commands::save_config,
            host::commands::list_available_shells,
            host::commands::list_projects,
            host::commands::save_project,
            host::commands::delete_project,
            host::commands::create_session,
            host::commands::select_workspace_dir,
            host::commands::list_sessions,
            host::commands::load_session,
            host::commands::load_subagent_history,
            host::commands::delete_session,
            host::commands::rename_session,
            host::commands::start_chat,
            host::commands::cancel_run,
            host::commands::session_running,
            host::commands::set_session_prefs,
            host::commands::get_session_prefs,
            host::commands::inject_run_message,
            host::commands::resolve_ask,
            host::commands::compact_session,
            host::commands::search_workspace_paths,
            host::commands::read_workspace_file,
            host::commands::save_workspace_file,
            host::commands::list_session_files,
            host::commands::read_workspace_file_base64,
            host::commands::git_status,
            host::commands::git_diff,
            host::commands::git_recent_log,
            host::commands::git_user_info,
            host::commands::get_token_breakdown,
            host::commands::stop_service,
            host::commands::get_mcp_config,
            host::commands::save_mcp_config,
            host::commands::connect_mcp,
            host::commands::mcp_status,
            host::commands::list_agents,
            host::commands::list_skills,
            host::commands::get_skill,
            host::commands::toggle_skill,
            host::commands::reload_skills,
            host::commands::delete_skill,
            host::commands::list_workspace_dir,
            host::commands::list_scheduled_tasks,
            host::commands::create_scheduled_task,
            host::commands::delete_scheduled_task,
            host::commands::get_token_stats,
            host::commands::stop_subagent,
            host::commands::list_log_files,
            host::commands::read_log_file,
            host::commands::read_session_log,
            host::commands::open_logs_dir,
            host::commands::app_version,
            host::commands::open_data_dir,
            host::commands::open_dir,
            host::commands::open_url,
            host::notify::notify_system,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
