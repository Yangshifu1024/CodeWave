//! 系统通知（通知点击回跳批次）：tauri-plugin-notification 桌面端 JS API 不暴露点击回调
//!（action 支持仅限移动端），因此本模块按平台直驱原生通知并注册回调——点击后向主窗口
//! emit `notify:activate`（payload { session_id }），前端据此 reveal 对应会话。
//!
//! 分层：host 层允许 use tauri::*（[docs/technical-design](../../../docs/technical-design.md) §2.2）；core 不感知通知。

use serde::Serialize;

/// notify:activate 事件的载荷：携带发起通知的会话 id，供前端回跳。
#[derive(Serialize, Clone)]
pub struct NotifyActivatePayload {
    /// 通知所属的会话 id
    pub session_id: String,
}

/// IPC 命令：发送系统通知。原生路径失败时返回 Err，由前端回退插件路径（仅 debug 记日志）。
#[tauri::command]
pub fn notify_system(
    app: tauri::AppHandle,
    session_id: String,
    title: String,
    body: String,
) -> Result<String, String> {
    tracing::info!("系统通知请求：session={session_id} title={title}");
    match native_notify(&app, &session_id, &title, &body) {
        Ok(()) => {
            tracing::info!("系统通知已按原生路径投递（AUMID 归属见上）");
            Ok("native".into())
        }
        Err(e) => {
            tracing::warn!("系统通知原生路径失败（前端回退插件路径）：{e}");
            Err(e)
        }
    }
}

/// 把主窗口拉回前台。通知点击是后台进程交互，Windows 前台锁会拒绝直接 set_focus
///（任务栏只闪不弹）——用 show/unminimize + 置顶瞬间切换绕过；Rust 侧调用不经
/// IPC capabilities 门控（前端 JS 的窗口 API 此前一直被权限拒绝，这正是「点击通知
/// 连窗口都拉不起来」的根因）。
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn raise_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_always_on_top(true);
        let _ = win.set_focus();
        let _ = win.set_always_on_top(false);
    }
}

#[cfg(target_os = "windows")]
fn native_notify(
    app: &tauri::AppHandle,
    session_id: &str,
    title: &str,
    body: &str,
) -> Result<(), String> {
    use tauri::Emitter;

    // AUMID 策略（通知缺陷修复，[docs/windows-toast-aumid](../../../docs/windows-toast-aumid.md)）：
    // toast 的图标/标题归属与点击激活都跟随 AUMID 属主——此前借用 POWERSHELL_APP_ID，通知显示为
    // PowerShell 图标/标题，且点击激活被 PowerShell 抢走（进程内 on_activated 不触发，回跳失效）。
    // AUMID 注册在**安装时**由 NSIS 钩子完成（windows/installer-hooks.nsh 写
    // HKCU\Software\Classes\AppUserModelId\<identifier>，绝不运行时写注册表/拉起子进程——防杀软误报）；
    // 运行时只读检测：键存在 → 用自家 identifier（品牌归属 + 点击回跳）；不存在（dev/便携）→
    // 回退 POWERSHELL_APP_ID，只保显示。
    let app_id = if aumid_registered(app.config().identifier.as_str()) {
        app.config().identifier.clone()
    } else {
        tauri_winrt_notification::Toast::POWERSHELL_APP_ID.to_string()
    };

    let payload_session = session_id.to_string();
    let app = app.clone();
    tauri_winrt_notification::Toast::new(&app_id)
        .title(title)
        .text1(body)
        .on_activated(move |_action| {
            // 点击回调：先原生拉起主窗口（前台锁绕行），再发事件让前端切 Tab
            //（窗口 reveal/聚焦统一由后端完成；前端只做 revealSession，见 AppShell）
            tracing::info!("通知点击激活：session={payload_session}（raise main + emit notify:activate）");
            raise_main_window(&app);
            let payload = NotifyActivatePayload {
                session_id: payload_session.clone(),
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = app.emit_to(
                    tauri::EventTarget::labeled("main"),
                    "notify:activate",
                    payload,
                );
            });
            Ok(())
        })
        .show()
        .map_err(|e| e.to_string())
}

/// 检测安装器是否已注册本应用的 AUMID（只读；键由 NSIS 钩子 POSTINSTALL 写入、PREUNINSTALL 删除）。
#[cfg(target_os = "windows")]
fn aumid_registered(identifier: &str) -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(format!(r"Software\Classes\AppUserModelId\{identifier}"), KEY_READ)
        .is_ok()
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use std::time::Duration;

    /// 手动探针（真实 toast + 点击回调检测，人工配合）：
    ///   cargo test --lib probe_own_aumid_toast -- --ignored --nocapture
    /// 前置：AUMID 已注册（安装版自动；dev 手动 `reg add`，命令见 docs/windows-toast-aumid.md）。
    /// 弹出后 **180 秒内点击通知本体**——回调触发则打印「点击回调已触发」并判定通过；
    /// 超时未点击失败（仅表示未点，不代表链路坏）。
    #[test]
    #[ignore = "manual Windows probe: shows a real toast and waits 180s for a click"]
    fn probe_own_aumid_toast() {
        assert!(
            super::aumid_registered("xyz.yangshifu.codewave"),
            "AUMID 未注册：先执行 docs/windows-toast-aumid.md 中的 reg add 命令或走安装器"
        );
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        tauri_winrt_notification::Toast::new("xyz.yangshifu.codewave")
            .title("CodeWave 通知探针")
            .text1("60 秒内点击本通知本体（不是关闭按钮）")
            .on_activated(move |_action| {
                println!("=== 通知点击回调已触发 ===");
                let _ = tx.send(());
                Ok(())
            })
            .show()
            .expect("toast 显示失败");
        println!("toast 已弹出，等待点击（180s）…");
        let clicked = rx.recv_timeout(Duration::from_secs(180)).is_ok();
        println!("点击回调触发: {clicked}");
        assert!(clicked, "60 秒内未收到点击回调");
    }
}
#[cfg(target_os = "macos")]
fn native_notify(
    app: &tauri::AppHandle,
    session_id: &str,
    title: &str,
    body: &str,
) -> Result<(), String> {
    use mac_notification_sys::{Notification, NotificationResponse};

    let app = app.clone();
    let session_id = session_id.to_string();
    let title = title.to_string();
    let body = body.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        // [docs/macos-notify-use-default-dialog-fix](../../../docs/macos-notify-use-default-dialog-fix.md)：在首次 send_notification 前消费 crate 内部 `Once`。
        // 否则 mac-notification-sys 会经 AppleScript `get id of application "use_default"`
        //（写死的假应用名）解析投递应用，macOS 在首次发送通知时给用户弹
        // 「Choose Application」系统模态框（如 ask:opened 失焦通知，[docs/session-nav-row-states](../../../docs/session-nav-row-states.md)）。
        // 设置自己的 bundle id（打包版）——即使设置失败（dev 未注册 bundle）——都会把 Once
        // 标记为已完成，`use_default` 查询从此不再运行。错误有意忽略：
        // 投递仍经库内 Finder 兜底工作。
        let bundle_id = app.config().identifier.clone();
        let _ = mac_notification_sys::set_application(&bundle_id);

        // send_notification 阻塞直至用户交互（点击 → Click / 按钮 → ActionButton / 超时 → None）
        let resp = mac_notification_sys::send_notification(
            &title,
            None,
            &body,
            Some(
                Notification::new()
                    .main_button(mac_notification_sys::MainButton::SingleAction("查看")),
            ),
        )
        .map_err(|e| e.to_string())?;
        if matches!(
            resp,
            NotificationResponse::Click | NotificationResponse::ActionButton(_)
        ) {
            use tauri::Emitter;
            raise_main_window(&app);
            let _ = app.emit_to(
                tauri::EventTarget::labeled("main"),
                "notify:activate",
                NotifyActivatePayload { session_id },
            );
        }
        Ok::<(), String>(())
    });
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn native_notify(
    _app: &tauri::AppHandle,
    _session_id: &str,
    _title: &str,
    _body: &str,
) -> Result<(), String> {
    Err("unsupported platform".into())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    // [docs/macos-notify-use-default-dialog-fix](../../../docs/macos-notify-use-default-dialog-fix.md) 探针：首次通知绝不能弹系统「Choose Application
    // (Where is use_default?)」对话框。库经 AppleScript `get id of application "use_default"`
    // 解析投递应用——除非先调用 set_application；我们在 native_notify 里每次发送前
    // 设置自己的 bundle id。手动验证探针：
    //   cargo test --release -p codewave probe_macos_notification_no_choose_application -- --ignored --nocapture
    // 预期：出现原生通知，且不带「Choose Application」模态框。
    #[test]
    #[ignore = "manual macOS probe: shows a real notification on this machine"]
    fn probe_macos_notification_no_choose_application() {
        let bundle_id = "xyz.yangshifu.codewave"; // 必须与 tauri.conf.json identifier 一致
        let set = mac_notification_sys::set_application(bundle_id);
        assert!(
            set.is_ok(),
            "set_application({bundle_id}) failed: {set:?} — registered app expected on a release build; on dev builds the failure is tolerated but this probe asserts the packaged path"
        );
        let resp = mac_notification_sys::send_notification(
            "CodeWave",
            None,
            "docs/macos-notify-use-default-dialog-fix probe: this notification must not trigger the Choose Application dialog",
            None,
        )
        .expect("send_notification failed");
        // 任意响应变体（Click / Close / 自动消失 None）都证明通知管线端到端跑通；
        // 上面的断言只保证 send 未报错。
        let _ = resp;
    }
}
