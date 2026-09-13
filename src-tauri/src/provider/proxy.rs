//! 代理感知的 HTTP client（[docs/p0-plan](../../../docs/p0-plan.md) §4.6）。
//! 优先级：手动配置 > 系统代理（macOS scutil / Windows 注册表 / Linux 环境变量）> 直连。
//! 手动模式 fail-closed：配了手动代理就用它，不做探测回退。
//!
//! 代理语义（reqwest 0.13 默认启用 system-proxy 特性，会自动读系统代理）：
//! `proxy = null`（从未配置过）保持 reqwest 默认行为——存量用户零变化；
//! 只要配置过代理（无论哪种模式），代理行为完全由配置显式决定：
//! 显式 `Proxy::all`（显式代理会关闭 reqwest 默认系统探测）或显式 `no_proxy`（保证真直连）。

use crate::core::config::{ConfigState, ProxyMode};
use std::time::Duration;

/// 构建全局共享 HTTP client：10s 连接超时 + 最多 10 跳重定向 + 按 config 解析代理。
pub fn build_client(cfg: &ConfigState) -> reqwest::Client {
    build_client_with(cfg, reqwest::redirect::Policy::limited(10))
}

/// 自定 redirect 策略的构建入口：http_request / web_fetch 逐跳 SSRF 校验需 redirect none。
pub fn build_client_with(
    cfg: &ConfigState,
    redirect: reqwest::redirect::Policy,
) -> reqwest::Client {
    let b = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(redirect);
    apply_proxy(b, cfg).build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 把 config 的代理决议套到 builder 上（[docs/network-proxy-settings](../../../docs/network-proxy-settings.md)）：
/// - `proxy = null`（从未配置）→ 原样返回，保持 reqwest 默认（跟随系统）——存量用户零变化；
/// - 解析为 None（显式无代理 / 系统未开代理）→ 显式 `no_proxy`（屏蔽 reqwest 默认 system-proxy 自动探测，保证真直连）；
/// - 解析为 Some(url) → 显式 `Proxy::all`（显式代理会关闭 reqwest 默认系统探测），并固定 loopback 直连例外。
fn apply_proxy(b: reqwest::ClientBuilder, cfg: &ConfigState) -> reqwest::ClientBuilder {
    let Some(resolved) = resolve_explicit_proxy(cfg) else {
        return b;
    };
    let Some(url) = resolved else {
        return b.no_proxy();
    };
    match reqwest::Proxy::all(&url) {
        // loopback 恒直连：本地模型网关 / 127.0.0.1 的 MCP server 经代理几乎必不通，且 allow_private_network 另有闸门
        Ok(p) => b.proxy(p.no_proxy(reqwest::NoProxy::from_string("localhost,127.0.0.1,::1"))),
        Err(e) => {
            tracing::warn!("代理 URL 无效({})：{e}，使用直连", sanitize_proxy_url(&url));
            b.no_proxy()
        }
    }
}

/// 代理显式化决议：`None` = config.proxy 为 null（不干预，保持 reqwest 默认）；
/// `Some(None)` = 显式直连；`Some(Some(url))` = 显式代理。拆成纯函数便于测试区分两种 None 来源。
fn resolve_explicit_proxy(cfg: &ConfigState) -> Option<Option<String>> {
    let pc = cfg.proxy.as_ref()?;
    Some(match pc.mode {
        ProxyMode::None => None,
        ProxyMode::Manual if pc.url.trim().is_empty() => None,
        ProxyMode::Manual => Some(pc.url.trim().to_string()),
        ProxyMode::System => system_proxy_url(),
    })
}

/// 按 config 的 ProxyMode 解析代理 URL：None=直连；Manual=配置值（空串视为直连）；System=系统探测。
/// pub：host 层 resolve_proxy 命令（前端检查更新传参 + 设置页系统代理回显）转调此函数。
/// 注意 config.proxy 为 null 时返回 None（语义 = 「跟随默认」，与显式直连不可区分——
/// 命令层对 null 另做系统探测回显，见 project.rs::resolve_proxy）。
pub fn resolve_proxy(cfg: &ConfigState) -> Option<String> {
    resolve_explicit_proxy(cfg).flatten()
}

/// 日志脱敏：剥离 userinfo（manual 地址可含 user:pass@host:port，凭据不得落日志）。
fn sanitize_proxy_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://***@{host}"),
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

/// 探测系统代理：macOS 走 `scutil --proxy`，Windows 走注册表，Linux 走环境变量。
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
        windows_registry_proxy()
    }
}

/// Windows 注册表探测：`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`
/// 的 `ProxyEnable`（DWORD，非 0 = 开）+ `ProxyServer`（SZ）。
#[cfg(target_os = "windows")]
fn windows_registry_proxy() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let settings = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let enabled: u32 = settings.get_value("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let server: String = settings.get_value("ProxyServer").ok()?;
    parse_windows_proxyserver(&server)
}

/// 解析 `ProxyServer` 注册表值（纯函数，跨平台可测）。兼容两种形态：
/// 全局 `host:port`（客户端到代理是 HTTP CONNECT，scheme 取 http）；
/// 分协议 `http=h:p;https=h:p;socks=h:p`（优先级 socks > https > http，与 macOS 解析一致）。
/// PAC 脚本形态（`http://...` URL）不解析，视为未检测到。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn parse_windows_proxyserver(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with("http://") || raw.starts_with("https://") {
        return None;
    }
    if !raw.contains('=') {
        return Some(format!("http://{raw}"));
    }
    let mut best: Option<(u8, &'static str, &str)> = None; // (优先级, scheme, addr)
    for part in raw.split(';') {
        let Some((proto, addr)) = part.split_once('=') else {
            continue;
        };
        let addr = addr.trim();
        if addr.is_empty() {
            continue;
        }
        let (rank, scheme) = match proto.trim().to_ascii_lowercase().as_str() {
            "socks" => (3u8, "socks5"),
            "https" => (2, "http"),
            "http" => (1, "http"),
            _ => continue,
        };
        if best.map_or(true, |(r, _, _)| rank > r) {
            best = Some((rank, scheme, addr));
        }
    }
    let (_, scheme, addr) = best?;
    Some(format!("{scheme}://{addr}"))
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

    #[test]
    fn windows_proxyserver_global() {
        assert_eq!(
            parse_windows_proxyserver("127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn windows_proxyserver_https_beats_http() {
        assert_eq!(
            parse_windows_proxyserver("http=1.1.1.1:8080;https=2.2.2.2:8443").as_deref(),
            Some("http://2.2.2.2:8443")
        );
    }

    #[test]
    fn windows_proxyserver_socks_wins() {
        assert_eq!(
            parse_windows_proxyserver("socks=3.3.3.3:1080;https=2.2.2.2:8443;http=1.1.1.1:80")
                .as_deref(),
            Some("socks5://3.3.3.3:1080")
        );
    }

    #[test]
    fn windows_proxyserver_pac_and_empty_ignored() {
        // PAC 脚本形态不解析（按未检测到处理，直连）
        assert_eq!(parse_windows_proxyserver("http://pac.example.com/wpad.dat"), None);
        assert_eq!(parse_windows_proxyserver("  "), None);
        // 分协议但条目全空
        assert_eq!(parse_windows_proxyserver("http=;https="), None);
        // 未知协议条目跳过，回退已知优先级最高者
        assert_eq!(
            parse_windows_proxyserver("ftp=1.1.1.1:21;http=2.2.2.2:80").as_deref(),
            Some("http://2.2.2.2:80")
        );
    }

    #[test]
    fn resolve_explicit_by_mode() {
        use crate::core::config::{ProxyConfig, ProxyMode};
        let mut cfg = ConfigState::default();
        // proxy = null（从未配置）：决议 None——apply_proxy 不干预（保持 reqwest 默认跟随系统），
        // 与显式直连（Some(None)）严格区分
        assert_eq!(resolve_explicit_proxy(&cfg), None);
        // 显式无代理
        cfg.proxy = Some(ProxyConfig { mode: ProxyMode::None, url: String::new() });
        assert_eq!(resolve_explicit_proxy(&cfg), Some(None));
        // 手动空串 = 显式直连；手动值 trim 后原样（可含 userinfo）
        cfg.proxy = Some(ProxyConfig { mode: ProxyMode::Manual, url: "   ".into() });
        assert_eq!(resolve_explicit_proxy(&cfg), Some(None));
        cfg.proxy = Some(ProxyConfig { mode: ProxyMode::Manual, url: "socks5://h:1".into() });
        assert_eq!(resolve_proxy(&cfg).as_deref(), Some("socks5://h:1"));
        cfg.proxy = Some(ProxyConfig { mode: ProxyMode::Manual, url: " http://u:p@h:8080 ".into() });
        assert_eq!(resolve_proxy(&cfg).as_deref(), Some("http://u:p@h:8080"));
        // System：结果与系统探测同源（不校验具体值，跨平台确定性交给探测函数自身单测）
        cfg.proxy = Some(ProxyConfig { mode: ProxyMode::System, url: String::new() });
        assert_eq!(resolve_proxy(&cfg), system_proxy_url());
    }

    #[test]
    fn sanitize_proxy_url_strips_userinfo() {
        assert_eq!(sanitize_proxy_url("http://user:secret@h:8080"), "http://***@h:8080");
        assert_eq!(sanitize_proxy_url("socks5://h:1080"), "socks5://h:1080");
        assert_eq!(sanitize_proxy_url("not-a-url"), "not-a-url");
    }
}
