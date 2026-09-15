pub mod atomic;
pub mod crockford;
pub mod throttle;
pub mod token_est;

/// 全应用出网 UA 标识（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）：OpenCode Go 等网关
/// 要求客户端使用自身专属 UA 而非通用 HTTP 库名；`tools/web_fetch` 与所有 LLM provider 请求共用同一常量。
pub const USER_AGENT: &str =
    concat!("CodeWave/", env!("CARGO_PKG_VERSION"), " (+local-first desktop agent)");
