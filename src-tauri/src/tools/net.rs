//! 工具共享网络层：SSRF 守卫（拨内网拒绝）+ 手动重定向逐跳校验 + 按主机节流。
//! 仅 web_fetch / http_request 工具使用；Provider 客户端绕过本层（base_url 是用户配置的可信端点）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 重定向跳数上限。
pub const MAX_REDIRECTS: usize = 10;
/// 响应体字节上限（50MB）。
pub const MAX_BODY_BYTES: u64 = 50 * 1024 * 1024;

/// 内网/保留地址段判定（v4 + v6）。
pub fn is_private_ip(ip: IpAddr) -> bool {
    // M13：判定前先把 IPv4-mapped IPv6 归一为 IPv4
    if let IpAddr::V6(v6) = ip {
        if let Some(v4) = v6.to_ipv4_mapped() {
            return is_private_ip(IpAddr::V4(v4));
        }
    }
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // 100.64.0.0/10 CGNAT（第二字节 64-127）
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 /* fc00::/7 ULA */
                || (v6.segments()[0] & 0xffc0) == 0xfe80 /* link-local */
        }
    }
}

/// 按主机的最小间隔节流（[docs/p1-plan](../../../docs/p1-plan.md) §2.1：1s）。
pub struct HostThrottle {
    /// 主机 → 上次请求时刻。
    last: Mutex<HashMap<String, Instant>>,
    /// 同主机两次请求的最小间隔。
    pub min_interval: Duration,
}

impl HostThrottle {
    /// 创建节流器（默认间隔 1s）。
    pub fn new() -> Self {
        HostThrottle {
            last: Mutex::new(HashMap::new()),
            min_interval: Duration::from_secs(1),
        }
    }

    /// 等待直到该主机距上次请求已超过最小间隔（每主机独立记账）。
    pub async fn wait(&self, host: &str) {
        loop {
            let wait = {
                let mut g = self.last.lock().unwrap();
                let now = Instant::now();
                match g.get(host) {
                    Some(t) => {
                        let elapsed = now.duration_since(*t);
                        if elapsed >= self.min_interval {
                            g.insert(host.to_string(), now);
                            return;
                        }
                        self.min_interval - elapsed
                    }
                    None => {
                        g.insert(host.to_string(), now);
                        return;
                    }
                }
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// 单跳失败的结构化错误。
pub struct HopError {
    /// 稳定错误码（E_SSRF_BLOCKED / E_DNS / E_NETWORK / E_TOO_LARGE）。
    pub code: &'static str,
    /// 面向模型与用户的描述。
    pub message: String,
}

/// 限量流式读取响应体（M17：chunked 响应同样受限）。
pub async fn read_body_limited(resp: reqwest::Response, max: u64) -> Result<Vec<u8>, HopError> {
    let mut out: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    loop {
        let chunk = tokio::select! {
            item = stream.next() => item,
        };
        let Some(item) = chunk else { break };
        let bytes = item.map_err(|e| HopError {
            code: "E_NETWORK",
            message: format!("读取响应失败：{e}"),
        })?;
        if out.len() as u64 + bytes.len() as u64 > max {
            return Err(HopError {
                code: "E_TOO_LARGE",
                message: format!("响应体超过 {max} 字节上限"),
            });
        }
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// 解析主机并做内网段检查（SSRF 守卫核心）。
pub async fn guard_host(host: &str, port: u16, allow_private: bool) -> Result<(), HopError> {
    if allow_private {
        return Ok(());
    }
    // IP 字面量
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(HopError {
                code: "E_SSRF_BLOCKED",
                message: format!(
                    "目标 {host}:{port} 是内网/保留地址，已被 SSRF 守卫拒绝（如需访问本地服务，在设置中开启 allow_private_network）"
                ),
            });
        }
        return Ok(());
    }
    // 域名：解析 DNS 后逐个 IP 检查
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| HopError {
            code: "E_DNS",
            message: format!("域名解析失败 {host}：{e}"),
        })?
        .collect();
    if addrs.is_empty() {
        return Err(HopError {
            code: "E_DNS",
            message: format!("域名无解析结果：{host}"),
        });
    }
    for a in &addrs {
        if is_private_ip(a.ip()) {
            return Err(HopError {
                code: "E_SSRF_BLOCKED",
                message: format!("{host} 解析到内网地址 {}，已被 SSRF 守卫拒绝", a.ip()),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ip_matrix() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("169.254.1.1".parse().unwrap()));
        assert!(is_private_ip("100.64.0.1".parse().unwrap()));
        assert!(is_private_ip("0.0.0.0".parse().unwrap()));
        assert!(is_private_ip("::1".parse().unwrap()));
        assert!(is_private_ip("fe80::1".parse().unwrap()));
        assert!(is_private_ip("fd12::1".parse().unwrap()));
        // 公网地址放行
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip("2606:4700::1111".parse().unwrap()));
    }

    #[test]
    fn private_ip_v4_boundary_matrix() {
        // 10/8 边界
        assert!(is_private_ip("10.255.255.255".parse().unwrap()));
        assert!(!is_private_ip("11.0.0.1".parse().unwrap()));
        // 172.16/12 边界：172.15.x 公网、172.31.x 私网、172.32.x 公网
        assert!(!is_private_ip("172.15.255.255".parse().unwrap()));
        assert!(is_private_ip("172.31.255.255".parse().unwrap()));
        assert!(!is_private_ip("172.32.0.1".parse().unwrap()));
        // 192.168/16 边界
        assert!(is_private_ip("192.168.255.255".parse().unwrap()));
        assert!(!is_private_ip("192.169.0.1".parse().unwrap()));
        // 链路本地 169.254/16
        assert!(is_private_ip("169.254.255.254".parse().unwrap()));
        assert!(!is_private_ip("169.255.0.1".parse().unwrap()));
        // CGNAT 100.64.0.0/10：第二字节 64..=127
        assert!(!is_private_ip("100.63.255.255".parse().unwrap()));
        assert!(is_private_ip("100.64.0.0".parse().unwrap()));
        assert!(is_private_ip("100.127.255.255".parse().unwrap()));
        assert!(!is_private_ip("100.128.0.1".parse().unwrap()));
        // 广播与未指定地址
        assert!(is_private_ip("255.255.255.255".parse().unwrap()));
        assert!(is_private_ip("0.0.0.0".parse().unwrap()));
        // 特殊段附近的常规单播公网地址
        assert!(!is_private_ip("100.200.1.1".parse().unwrap()));
        assert!(!is_private_ip("9.9.9.9".parse().unwrap()));
    }

    #[test]
    fn private_ip_v6_matrix() {
        // 环回 / 未指定
        assert!(is_private_ip("::1".parse().unwrap()));
        assert!(is_private_ip("::".parse().unwrap()));
        // ULA fc00::/7（fc00..fdff）
        assert!(is_private_ip("fc00::".parse().unwrap()));
        assert!(is_private_ip("fdff::1".parse().unwrap()));
        assert!(
            !is_private_ip("fe00::1".parse().unwrap()),
            "fe00 is outside fc00::/7"
        );
        assert!(!is_private_ip("fb00::1".parse().unwrap()));
        // link-local fe80::/10（掩码 0xffc0）：fe80..febf
        assert!(is_private_ip("fe80::".parse().unwrap()));
        assert!(is_private_ip("febf:ffff::1".parse().unwrap()));
        assert!(
            !is_private_ip("fec0::1".parse().unwrap()),
            "fec0 (old site-local) is not treated as private"
        );
        // 全局单播保持公网
        assert!(!is_private_ip("2001:db8::1".parse().unwrap()));
        assert!(!is_private_ip("2606:4700::1111".parse().unwrap()));
    }

    #[test]
    fn ipv4_mapped_ipv6_is_normalized() {
        // M13：::ffff:a.b.c.d 必须按内嵌的 IPv4 地址判定
        assert!(is_private_ip("::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:169.254.1.1".parse().unwrap()));
        assert!(is_private_ip("::ffff:100.64.0.1".parse().unwrap()));
        assert!(!is_private_ip("::ffff:8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("::ffff:1.1.1.1".parse().unwrap()));
    }

    /// guard_host：IP 字面量主机按 is_private_ip 分类；allow_private 直接短路放行。
    #[tokio::test]
    async fn guard_host_blocks_private_literal_and_honors_allow() {
        // 私网字面量且未开 allow_private → SSRF 拦截
        let err = guard_host("10.0.0.1", 80, false).await.unwrap_err();
        assert_eq!(err.code, "E_SSRF_BLOCKED");
        // allow_private 在任何拨号前短路
        assert!(guard_host("10.0.0.1", 80, true).await.is_ok());
        assert!(guard_host("127.0.0.1", 80, true).await.is_ok());
        // 公网字面量不拨号直接放行
        assert!(guard_host("8.8.8.8", 443, false).await.is_ok());
        // "localhost"（hosts 文件）解析到回环地址 → 拦截
        let err = guard_host("localhost", 80, false).await.unwrap_err();
        assert_eq!(err.code, "E_SSRF_BLOCKED");
    }

    /// read_body_limited：限额内完整流式读取；超限 → E_TOO_LARGE。
    #[tokio::test]
    async fn read_body_limited_enforces_cap() {
        // 一次性本地 HTTP 服务：每次调用在新端口上回 `body`
        async fn serve_once(body: String) -> std::net::SocketAddr {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            });
            addr
        }

        let addr = serve_once("x".repeat(300)).await;
        let client = reqwest::Client::new();
        let resp = client.get(format!("http://{addr}/b")).send().await.unwrap();
        let ok = match read_body_limited(resp, 1000).await {
            Ok(b) => b,
            Err(e) => panic!(
                "within-limit read must succeed, got {}: {}",
                e.code, e.message
            ),
        };
        assert_eq!(ok.len(), 300);

        let addr = serve_once("y".repeat(300)).await;
        let resp = client.get(format!("http://{addr}/b")).send().await.unwrap();
        let err = read_body_limited(resp, 50).await.unwrap_err();
        assert_eq!(err.code, "E_TOO_LARGE");
    }
}
