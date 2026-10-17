//! 工具共享网络层：SSRF 守卫（拨内网拒绝）+ 手动重定向逐跳校验 + 按主机节流。
//! 仅 web_fetch / http_request 工具使用；Provider 客户端绕过本层（base_url 是用户配置的可信端点）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(super) mod request;

/// 重定向跳数上限。
pub const MAX_REDIRECTS: usize = 10;
/// 解压后响应体字节上限（50 MiB）。
pub const MAX_BODY_BYTES: u64 = 50 * 1024 * 1024;

/// 内网/保留地址段判定（v4 + v6）。
pub fn is_private_ip(ip: IpAddr) -> bool {
    // M13：判定前先把 IPv4-mapped IPv6 归一为 IPv4
    if let IpAddr::V6(v6) = ip
        && let Some(v4) = v6.to_ipv4_mapped()
    {
        return is_private_ip(IpAddr::V4(v4));
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

/// 按主机的最小间隔节流（[docs/technical-design](../../../docs/technical-design.md)：1s）。
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
#[derive(Debug)]
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
        let chunk = stream.next().await;
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
}
