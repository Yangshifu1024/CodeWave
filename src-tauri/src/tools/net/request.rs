//! One bounded, cancellable HTTP execution path for network tools.
//! LLM and MCP clients deliberately keep their separate endpoint policies.

use super::{HopError, MAX_BODY_BYTES, MAX_REDIRECTS, is_private_ip, read_body_limited};
use crate::core::config::{ConfigState, ProxyMode};
use hyper_util::client::proxy::matcher::{Intercept, Matcher};
use reqwest::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap};
use reqwest::{Method, StatusCode, Url};
use serde_json::Value;
use std::net::SocketAddr;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(in crate::tools) struct HttpRequest {
    pub url: Url,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Option<Value>,
}

pub(in crate::tools) struct HttpResponse {
    pub url: Url,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

fn error(code: &'static str, message: impl Into<String>) -> HopError {
    HopError {
        code,
        message: message.into(),
    }
}

fn validate_url(url: &Url) -> Result<(), HopError> {
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(error("E_URL", "仅支持带主机名的 HTTP(S) URL"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(error("E_URL", "URL 不允许内嵌凭据，请使用请求头"));
    }
    Ok(())
}

async fn proxy_matcher(cfg: &ConfigState) -> Result<Matcher, HopError> {
    let Some(proxy) = &cfg.proxy else {
        // Native system discovery must not block cancellation/deadline polling.
        // The OS lookup itself may finish after the request has been cancelled.
        return tokio::task::spawn_blocking(Matcher::from_system)
            .await
            .map_err(|_| error("E_PROXY", "系统代理探测失败"));
    };
    let selected = match proxy.mode {
        ProxyMode::None => None,
        ProxyMode::Manual if proxy.url.trim().is_empty() => None,
        ProxyMode::Manual => Some(proxy.url.trim().to_owned()),
        ProxyMode::System => system_proxy_url().await?,
    };
    let Some(selected) = selected else {
        return Ok(Matcher::builder().build());
    };
    let selected = normalize_proxy_url(&selected)?;
    let matcher = Matcher::builder()
        .all(selected.as_str())
        .no("localhost,127.0.0.1,::1")
        .build();
    // Matcher silently discards unsupported proxies. Never interpret that as a
    // user-approved direct route, even if reqwest accepted the URL constructor.
    if matcher
        .intercept(&"https://proxy-route-check.invalid/".parse().unwrap())
        .is_none()
    {
        return Err(error("E_PROXY", "代理配置不受支持，未改用直连"));
    }
    Ok(matcher)
}

fn normalize_proxy_url(raw: &str) -> Result<Url, HopError> {
    let invalid = || error("E_PROXY", "代理配置无效，未改用直连");
    let input = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let mut url = Url::parse(&input).map_err(|_| invalid())?;
    if !matches!(
        url.scheme(),
        "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
    ) {
        return Err(invalid());
    }
    let host = url.host_str().ok_or_else(invalid)?;
    // Non-special SOCKS URLs need the same IDNA normalization as HTTP URLs.
    let normalized = Url::parse(&format!("http://{host}")).map_err(|_| invalid())?;
    url.set_host(normalized.host_str()).map_err(|_| invalid())?;
    if url.scheme().starts_with("socks") && url.port().is_none() {
        url.set_port(Some(1080)).map_err(|_| invalid())?;
    }
    reqwest::Proxy::all(url.as_str()).map_err(|_| invalid())?;
    Ok(url)
}

async fn system_proxy_url() -> Result<Option<String>, HopError> {
    #[cfg(target_os = "macos")]
    {
        // The subprocess is owned by this future and killed on deadline/cancel.
        let output = tokio::process::Command::new("/usr/sbin/scutil")
            .arg("--proxy")
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|_| error("E_PROXY", "系统代理探测失败"))?;
        if !output.status.success() {
            return Err(error("E_PROXY", "系统代理探测失败"));
        }
        Ok(crate::provider::proxy::parse_scutil(
            &String::from_utf8_lossy(&output.stdout),
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        tokio::task::spawn_blocking(crate::provider::proxy::system_proxy_url)
            .await
            .map_err(|_| error("E_PROXY", "系统代理探测失败"))
    }
}

fn remote_dns(proxy: &Intercept) -> bool {
    !matches!(proxy.uri().scheme_str(), Some("socks4" | "socks5"))
}

fn check_proxy_target(
    url: &Url,
    proxy: Option<&Intercept>,
    allow_private: bool,
) -> Result<(), HopError> {
    let literal_target = url
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok();
    if !allow_private && !literal_target && proxy.is_some_and(remote_dns) {
        return Err(error(
            "E_SSRF_PROXY_UNSUPPORTED",
            "当前代理在远端解析目标域名，无法固定已校验 IP；请求未发送且未改用直连。请使用本地 DNS SOCKS 代理，或明确开启允许内网访问。",
        ));
    }
    Ok(())
}

fn checked_addresses(
    addrs: Vec<SocketAddr>,
    allow_private: bool,
) -> Result<Vec<SocketAddr>, HopError> {
    if addrs.is_empty() {
        return Err(error("E_DNS", "目标域名无解析结果"));
    }
    if !allow_private && addrs.iter().any(|addr| is_private_ip(addr.ip())) {
        return Err(error(
            "E_SSRF_BLOCKED",
            "目标解析到内网或保留地址，已被 SSRF 守卫拒绝",
        ));
    }
    Ok(addrs)
}

async fn resolve(url: &Url, allow_private: bool) -> Result<Vec<SocketAddr>, HopError> {
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = if let Ok(ip) = host.parse() {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| error("E_DNS", "目标域名解析失败"))?
            .collect()
    };
    checked_addresses(addrs, allow_private)
}

fn pinned_client(
    url: &Url,
    addresses: &[SocketAddr],
    proxy: Option<Intercept>,
    allow_private: bool,
) -> Result<reqwest::Client, HopError> {
    check_proxy_target(url, proxy.as_ref(), allow_private)?;
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .no_proxy();
    if let Some(proxy) = proxy {
        // CONNECT and remote-DNS SOCKS proxies resolve the destination themselves.
        // resolve_to_addrs cannot prove their target. Refuse rather than bypassing
        // the user's route or claiming that a proxy address proves SSRF safety.
        let mut configured = reqwest::Proxy::all(proxy.uri().to_string())
            .map_err(|_| error("E_PROXY", "代理配置无效"))?;
        if let Some(auth) = proxy.basic_auth() {
            configured = configured.custom_http_auth(auth.clone());
        }
        if let Some((user, password)) = proxy.raw_auth() {
            configured = configured.basic_auth(user, password);
        }
        builder = builder.proxy(configured);
    }
    if !addresses.is_empty() {
        builder = builder.resolve_to_addrs(url.host_str().unwrap_or_default(), addresses);
    }
    builder
        .build()
        .map_err(|_| error("E_NETWORK", "无法建立受保护的 HTTP 客户端"))
}

fn redirect(request: &mut HttpRequest, status: StatusCode, next: Url) -> Result<(), HopError> {
    validate_url(&next)?;
    if request.url.scheme() == "https" && next.scheme() == "http" {
        return Err(error(
            "E_REDIRECT_UNSAFE",
            "拒绝 HTTPS 降级到 HTTP 的重定向",
        ));
    }
    let becomes_get = (status == StatusCode::SEE_OTHER && request.method != Method::HEAD)
        || (matches!(status.as_u16(), 301 | 302) && request.method == Method::POST);
    if becomes_get {
        request.method = Method::GET;
    }
    if becomes_get || status == StatusCode::SEE_OTHER {
        request.body = None;
        for name in [CONTENT_TYPE, CONTENT_LENGTH, CONTENT_ENCODING] {
            request.headers.remove(name);
        }
    }
    if request.url.origin() != next.origin() {
        if request.body.is_some() {
            return Err(error(
                "E_REDIRECT_UNSAFE",
                "跨源重定向需要重放请求体，已拒绝发送",
            ));
        }
        // Custom header names can carry credentials too. Strip all caller headers.
        request.headers.clear();
    }
    request.url = next;
    Ok(())
}

/// The deadline includes proxy discovery, DNS, throttling, every hop and decoded bytes.
pub(in crate::tools) async fn execute(
    cfg: &ConfigState,
    request: HttpRequest,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<HttpResponse, HopError> {
    execute_with_throttle(
        cfg,
        request,
        cancel,
        timeout,
        super::super::web_fetch::throttle_pub(),
    )
    .await
}

async fn execute_with_throttle(
    cfg: &ConfigState,
    mut request: HttpRequest,
    cancel: &CancellationToken,
    timeout: Duration,
    throttle: &super::HostThrottle,
) -> Result<HttpResponse, HopError> {
    let operation = async {
        validate_url(&request.url)?;
        let proxies = proxy_matcher(cfg).await?;
        for hop in 0..=MAX_REDIRECTS {
            let uri = request
                .url
                .as_str()
                .parse()
                .map_err(|_| error("E_URL", "URL 无效"))?;
            let proxy = proxies.intercept(&uri);
            check_proxy_target(
                &request.url,
                proxy.as_ref(),
                cfg.network.allow_private_network,
            )?;
            let addresses =
                if cfg.network.allow_private_network && proxy.as_ref().is_some_and(remote_dns) {
                    // Preserve split-DNS/remote-only names: the configured proxy owns
                    // DNS here. Do not first resolve through the local network.
                    Vec::new()
                } else {
                    resolve(&request.url, cfg.network.allow_private_network).await?
                };
            let client = pinned_client(
                &request.url,
                &addresses,
                proxy,
                cfg.network.allow_private_network,
            )?;
            throttle
                .wait(request.url.host_str().unwrap_or_default())
                .await;
            let mut outgoing = client
                .request(request.method.clone(), request.url.clone())
                .header(reqwest::header::USER_AGENT, crate::util::USER_AGENT)
                .headers(request.headers.clone());
            if let Some(body) = &request.body {
                outgoing = outgoing.json(body);
            }
            let response = outgoing.send().await.map_err(|err| {
                error("E_NETWORK", format!("HTTP 请求失败：{}", err.without_url()))
            })?;
            let status = response.status();
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECTS {
                    return Err(error("E_REDIRECTS", "重定向过多"));
                }
                let next = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| request.url.join(v).ok())
                    .ok_or_else(|| error("E_URL", "重定向地址无效"))?;
                redirect(&mut request, status, next)?;
                continue;
            }
            if response
                .content_length()
                .is_some_and(|len| len > MAX_BODY_BYTES)
            {
                return Err(error("E_TOO_LARGE", "响应体超过 50MiB 上限"));
            }
            let headers = response.headers().clone();
            let body = read_body_limited(response, MAX_BODY_BYTES).await?;
            return Ok(HttpResponse {
                url: request.url,
                status,
                headers,
                body,
            });
        }
        Err(error("E_REDIRECTS", "重定向过多"))
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(error("E_CANCELLED", "网络请求已取消")),
        result = tokio::time::timeout(timeout, operation) => result
            .unwrap_or_else(|_| Err(error("E_NETWORK", "网络请求超时（包括解析、节流、重定向和正文读取）"))),
    }
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
