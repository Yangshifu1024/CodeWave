use super::util::err;
use super::util::open_dir_in_file_manager;
use tauri_plugin_decoration::WebviewWindowExt;

/// IPC 连通性探测。
#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

/// 应用重启（自动更新下载安装完成后由「检查更新」流程调用——updater 替换产物后
/// 必须 relaunch 才运行新版本；「关于」对话框亦用）。restart 不返回，进程直接重启。
#[tauri::command]
pub fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

// ---------- 自绘标题栏（tauri-plugin-decoration v3，[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)）----------

/// 激活失败兜底：恢复原生装饰并 reveal 窗口。窗口以 visible:false 起动，
/// 任何代码路径最终都必须 reveal。
async fn restore_and_show(
    window: &tauri::WebviewWindow,
    activation_error: impl std::fmt::Display,
) -> Result<&'static str, String> {
    let restore_error = window.restore_decoration().await.err();
    let show_error = window.show().err();
    match (restore_error, show_error) {
        (None, None) => Ok("native"),
        (Some(restore), None) => Err(format!(
            "custom decoration failed ({activation_error}); native restoration failed ({restore}), but the window was revealed"
        )),
        (None, Some(show)) => Err(format!(
            "custom decoration failed ({activation_error}); native fallback could not be revealed ({show})"
        )),
        (Some(restore), Some(show)) => Err(format!(
            "custom decoration failed ({activation_error}); native restoration failed ({restore}); revealing the fallback also failed ({show})"
        )),
    }
}

/// 前端挂载完成后调用：激活自绘标题栏（Windows/Linux 由插件注入 HTML 控制按钮、
/// 保留 Win11 Snap Layout；macOS 保留原生红绿灯 overlay），成功后 reveal 窗口；
/// 失败回退原生标题栏。返回 "custom" | "native" 供前端切换顶栏布局（native 表示未注入插件控制条）。
#[tauri::command]
pub async fn activate_and_show(window: tauri::WebviewWindow) -> Result<&'static str, String> {
    if let Err(error) = window.activate_decoration().await {
        return restore_and_show(&window, error).await;
    }

    // macOS：红绿灯内缩对齐 50px 顶栏的视觉中心（其他平台此调用被 cfg 掉）
    #[cfg(target_os = "macos")]
    if let Err(error) = window.set_traffic_lights_inset(14.0, 17.0).await {
        return restore_and_show(&window, error).await;
    }

    match window.show() {
        Ok(()) => Ok("custom"),
        Err(error) => restore_and_show(&window, error).await,
    }
}

// ---------- 项目注册表 ----------

/// 「关于」对话框用的应用版本（单一事实源：crate 版本，
/// 与发布时打进 tauri.conf.json 的值一致）。
/// build.rs 编译期注入 HEAD commit short-sha（WS_COMMIT_SHA）时，
/// 追加括号段如 `0.2.0 (a1b2c3d)`；无 git 环境构建则仅显示纯版本号。
#[tauri::command]
pub fn app_version() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("WS_COMMIT_SHA") {
        Some(sha) if !sha.is_empty() => format!("{version} ({sha})"),
        _ => version.to_string(),
    }
}

/// 是否以 AppImage 形式运行（Linux）：仅 AppImage 安装能自替换二进制。
/// deb / rpm 装在系统路径下、且无 `APPIMAGE` 环境变量，更新器无法就地替换，
/// 前端据此把升级降级为「打开发布页手动下载」（与 GitWave 同一套判定）。
/// 该环境变量只在 AppImage 包裹运行时由 AppRun 注入，其它平台恒为 false。
#[tauri::command]
pub fn is_appimage() -> bool {
    std::env::var("APPIMAGE").is_ok_and(|v| !v.is_empty())
}

#[cfg(test)]
mod app_version_tests {
    use super::app_version;

    /// 输出必须是 `语义化版本` 或 `语义化版本 (<7位十六进制 sha>)`，
    /// 绝不允许空括号或其他非法形态。
    #[test]
    fn format_is_version_with_optional_short_sha() {
        let text = app_version();
        let (version, sha) = text
            .split_once(" (")
            .map_or((text.as_str(), None), |(v, rest)| {
                let sha = rest.strip_suffix(")").expect("sha 括号段必须以 ) 结尾");
                (v, Some(sha))
            });
        let mut semver = version.split('.');
        for part in semver.by_ref().take(2) {
            assert!(
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
                "版本段必须是数字：{version}"
            );
        }
        match sha {
            Some(sha) => {
                assert_eq!(sha.len(), 7, "sha 必须是 7 位：{sha}");
                assert!(
                    sha.bytes().all(|b| b.is_ascii_hexdigit()),
                    "sha 必须是十六进制：{sha}"
                );
            }
            None => {} // 无 git 环境（如 cargo package）：允许纯版本号
        }
        assert!(
            !text.contains(" ()"),
            "不允许空括号占位：{text}"
        );
    }
}

/// 在系统文件管理器中打开全局数据目录（~/.codewave，「关于」对话框用）。
#[tauri::command]
pub async fn open_data_dir() -> Result<(), String> {
    let dir = crate::core::config::data_dir();
    std::fs::create_dir_all(&dir).map_err(err)?;
    open_dir_in_file_manager(&dir, "打开数据目录失败")
}

/// 在系统文件管理器中打开任意目录（右侧栏信息页签「项目目录」图标按钮用）。
/// 命令只做校验 + 转调共享助手；路径来自前端展示值（本机桌面应用，与 open_logs_dir 同信任级别）。
#[tauri::command]
pub async fn open_dir(path: String) -> Result<(), String> {
    let dir = validate_open_dir(&path)?;
    open_dir_in_file_manager(std::path::Path::new(dir), "打开目录失败")
}

/// open_dir 的校验：trim 后非空、路径存在且为目录（错误信息中文，直接面向用户提示）。
pub(super) fn validate_open_dir(path: &str) -> Result<&str, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径为空".to_string());
    }
    let p = std::path::Path::new(trimmed);
    if !p.exists() {
        return Err(format!("目录不存在：{trimmed}"));
    }
    if !p.is_dir() {
        return Err(format!("不是目录：{trimmed}"));
    }
    Ok(trimmed)
}

/// Windows 打开外部链接的命令：`cmd /C start "" <url>`。
/// 第三个参数是**空标题占位**（`start` 会把紧跟的第一个引号参数当窗口标题，缺了它 URL 不会被打开）；
/// 同时统一带 `CREATE_NO_WINDOW`：cmd.exe 是控制台程序，不设该标志会闪一下黑框（与 core::openers 同处理）。
#[cfg(target_os = "windows")]
fn windows_open_command(url: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new("cmd");
    command.args(["/C", "start", "", url]);
    command.creation_flags(crate::core::openers::CREATE_NO_WINDOW);
    command
}

/// 在默认浏览器打开外部 http(s) 链接（「关于」对话框的仓库链接）。
/// 协议限定 http/https 且硬拒空白/控制字符；
/// opener 插件非依赖项，因此沿用与 open_logs_dir 相同的原生进程
/// 模式。纯校验逻辑在 `validate_open_url`（有单测）。
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), String> {
    let trimmed = validate_open_url(&url)?;
    #[cfg(target_os = "windows")]
    let mut command = windows_open_command(trimmed);
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(trimmed);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(trimmed);
        c
    };
    command
        .spawn()
        .map_err(|e| format!("打开链接失败：{e}"))?;
    Ok(())
}

/// open_url 的校验：必须 http/https 协议，且硬拒
/// 空白/控制字符（该命令是通用 IPC 入口，校验必须对任意调用方成立，
/// 而不只是「关于」对话框）。
fn validate_open_url(url: &str) -> Result<&str, String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(format!("仅允许打开 http(s) 链接：{trimmed}"));
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("URL 包含非法字符（空白/控制字符）".to_string());
    }
    Ok(trimmed)
}


#[cfg(test)]
mod about_commands_tests {
    use super::*;

    #[test]
    fn validate_open_url_accepts_http_https() {
        assert_eq!(
            validate_open_url("https://github.com/Yangshifu1024/CodeWave").unwrap(),
            "https://github.com/Yangshifu1024/CodeWave"
        );
        assert_eq!(validate_open_url("  http://example.com  ").unwrap(), "http://example.com");
    }

    #[test]
    fn validate_open_url_rejects_other_schemes() {
        assert!(validate_open_url("file:///etc/passwd").is_err());
        assert!(validate_open_url("ftp://example.com").is_err());
        assert!(validate_open_url("javascript:alert(1)").is_err());
        assert!(validate_open_url("").is_err());
    }

    #[test]
    fn validate_open_url_rejects_whitespace_and_control_chars() {
        // 内嵌空白/控制字符是注入面 → 拒绝
        assert!(validate_open_url("https://example.com/a b").is_err());
        assert!(validate_open_url("https://example.com/a\nb").is_err());
        assert!(validate_open_url("https://example.com/a\tb").is_err());
        assert!(validate_open_url("https://example.com/\u{0}").is_err());
        // 首尾空白在校验前已 trim → 容忍（复制粘贴友好）
        assert_eq!(validate_open_url("https://example.com/\n").unwrap(), "https://example.com/");
    }

    #[test]
    fn validate_open_dir_accepts_existing_dir() {
        let base = std::env::temp_dir().join("ws_open_dir_ok");
        std::fs::create_dir_all(&base).unwrap();
        // 首尾空白容忍（trim 后有效）
        let padded = format!("  {}  ", base.display());
        assert_eq!(validate_open_dir(&padded).unwrap(), base.to_str().unwrap());
        let _ = std::fs::remove_dir(&base);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_open_command_keeps_cmd_start_shape() {
        // 形状钉子：缺了空标题占位，`start` 会把 URL 当窗口标题、根本不打开浏览器
        let command = windows_open_command("https://example.com/a?b=c");
        assert_eq!(command.get_program(), "cmd");
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["/C", "start", "", "https://example.com/a?b=c"]);
        // 无控制台窗口标志本身不可从 std::process::Command 读出，
        // 只能靠「共享 CREATE_NO_WINDOW 常量 + openers 侧同一条路径」保证（见 core::openers）。
        assert_eq!(crate::core::openers::CREATE_NO_WINDOW, 0x0800_0000);
    }

    #[test]
    fn validate_open_dir_rejects_empty_missing_and_file() {
        assert!(validate_open_dir("   ").is_err());
        assert!(validate_open_dir("").is_err());
        assert!(validate_open_dir("Z:/definitely/not/exist/ws_test").is_err());
        // 存在但不是目录 → 拒绝
        let f = std::env::temp_dir().join("ws_open_dir_file.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(validate_open_dir(f.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&f);
    }
}
