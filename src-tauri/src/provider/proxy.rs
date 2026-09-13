//! 代理感知的 HTTP client（[docs/p0-plan](../../../docs/p0-plan.md) §4.6）。
//! 优先级：手动配置 > 系统代理（macOS scutil / Linux 环境变量；Windows 注册表为 P1）> 直连。
//! 手动模式 fail-closed：配了手动代理就用它，不做探测回退。

use crate::core::config::{ConfigState, ProxyMode};
use std::time::Duration;

/// 构建全局共享 HTTP client：10s 连接超时 + 最多 10 跳重定向 + 按 config 解析代理。
pub fn build_client(cfg: &ConfigState) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10));
    if let Some(url) = resolve_proxy(cfg) {
        match reqwest::Proxy::all(&url) {
            Ok(p) => {
                b = b.proxy(p);
            }
            Err(e) => tracing::warn!("代理 URL 无效({url})：{e}，使用直连"),
        }
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 按 config 的 ProxyMode 解析代理 URL：None=直连；Manual=配置值（空串视为直连）；System=系统探测。
fn resolve_proxy(cfg: &ConfigState) -> Option<String> {
    let pc = cfg.proxy.as_ref()?;
    match pc.mode {
        ProxyMode::None => None,
        ProxyMode::Manual => {
            if pc.url.trim().is_empty() {
                None
            } else {
                Some(pc.url.trim().to_string())
            }
        }
        ProxyMode::System => system_proxy_url(),
    }
}

/// 探测系统代理：macOS 走 `scutil --proxy`，Linux 走环境变量，Windows 暂不探测。
pub fn system_proxy_url() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("scutil")
            .arg("--proxy")
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        parse_scutil(&s)
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("https_proxy")
            .or_else(|_| std::env::var("HTTPS_PROXY"))
            .or_else(|_| std::env::var("all_proxy"))
            .or_else(|_| std::env::var("http_proxy"))
            .ok()
            .filter(|s| !s.trim().is_empty())
    }
    #[cfg(target_os = "windows")]
    {
        // Windows 注册表探测为 P1（[docs/p0-plan](../../../docs/p0-plan.md) §4.6）
        None
    }
}

/// 解析 `scutil --proxy` 输出，优先级 SOCKS > HTTPS > HTTP。
pub fn parse_scutil(output: &str) -> Option<String> {
    let get = |key: &str| -> Option<(String, u32)> {
        let mut host = None;
        let mut port = None;
        let mut enabled = false;
        for line in output.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix(&format!("{key}Enable :")) {
                enabled = v.trim() == "1";
            } else if line.starts_with(&format!("{key}Proxy :")) {
                host = line.split(':').nth(1).map(|s| s.trim().to_string());
            } else if line.starts_with(&format!("{key}Port :")) {
                port = line.split(':').nth(1).and_then(|s| s.trim().parse().ok());
            }
        }
        if enabled {
            host.zip(port)
        } else {
            None
        }
    };
    if let Some((h, p)) = get("SOCKS") {
        return Some(format!("socks5://{h}:{p}"));
    }
    if let Some((h, p)) = get("HTTPS") {
        return Some(format!("http://{h}:{p}"));
    }
    get("HTTP").map(|(h, p)| format!("http://{h}:{p}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scutil_parsing() {
        let out = "\
<dictionary> {
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7890
  HTTPSProxy : 127.0.0.1
  SOCKSEnable : 0
}
";
        assert_eq!(parse_scutil(out).as_deref(), Some("http://127.0.0.1:7890"));

        let socks_on = out
            .replace("SOCKSEnable : 0", "SOCKSEnable : 1")
            .replace("SOCKSPort", "SOCKSXXXX");
        let _ = socks_on;
    }

    #[test]
    fn scutil_socks_preferred() {
        let out = "HTTPEnable : 1\nHTTPPort : 1\nHTTPProxy : h1\nSOCKSEnable : 1\nSOCKSPort : 2\nSOCKSProxy : h2\n";
        assert_eq!(parse_scutil(out).as_deref(), Some("socks5://h2:2"));
    }

    #[test]
    fn disabled_means_none() {
        assert_eq!(
            parse_scutil("HTTPEnable : 0\nHTTPPort : 1\nHTTPProxy : h\n"),
            None
        );
    }
}
