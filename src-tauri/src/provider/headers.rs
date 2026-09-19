//! 供应商级自定义请求头应用（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）：
//! 三个协议适配器在设好 `Content-Type` / 鉴权头后统一调用，为请求附加默认 UA 与用户配置头。
//!
//! 语义：
//! - 默认 `User-Agent: CodeWave/<version>` 恒发送，用户可用同名自定义头覆盖；
//! - 保留名（协议/传输必需）忽略自定义值：`rb.headers()` 为覆盖语义，跳过是为了不覆盖适配器已设的协议/鉴权头
//!   （`Content-Type` / `Authorization` / `x-api-key` / `anthropic-version`），而非防重复头；
//! - 值中的 `${session_id}` 替换为会话 uuid（无会话时替换为空串）；
//! - 非法头名/值跳过并 warn，不使整个请求失败。

use crate::core::config::{HeaderPair, RESERVED_REQUEST_HEADERS};
use crate::util::USER_AGENT;
use reqwest::RequestBuilder;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT as UA_HEADER};

/// `${session_id}` 占位符字面量（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）。
const SESSION_PLACEHOLDER: &str = "${session_id}";

/// 把默认 UA + 供应商自定义头合并进请求。保留名被忽略；`${session_id}` 完成替换。
pub fn apply_request_headers(
    rb: RequestBuilder,
    headers: &[HeaderPair],
    session_id: Option<&str>,
) -> RequestBuilder {
    let mut map = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(USER_AGENT) {
        map.insert(UA_HEADER, v);
    }
    for h in headers {
        let name = h.name.trim();
        if name.is_empty() {
            continue;
        }
        if RESERVED_REQUEST_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            tracing::warn!(header = %name, "自定义请求头命中保留名，已忽略");
            continue;
        }
        let value = substitute_session(&h.value, session_id);
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            (Ok(name), Ok(value)) => {
                map.insert(name, value); // insert = 覆盖（同名后者胜，可覆盖默认 UA）
            }
            _ => tracing::warn!(header = %name, "自定义请求头名称或值非法，已忽略"),
        }
    }
    rb.headers(map)
}

/// 替换值中的 `${session_id}`；无会话替换为空串（避免把字面占位符发到线上）。
fn substitute_session(value: &str, session_id: Option<&str>) -> String {
    if !value.contains(SESSION_PLACEHOLDER) {
        return value.to_string();
    }
    value.replace(SESSION_PLACEHOLDER, session_id.unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, value: &str) -> HeaderPair {
        HeaderPair {
            name: name.into(),
            value: value.into(),
        }
    }

    fn build(headers: &[HeaderPair], session_id: Option<&str>) -> reqwest::Request {
        apply_request_headers(
            reqwest::Client::new().post("http://localhost/v1/messages"),
            headers,
            session_id,
        )
        .build()
        .unwrap()
    }

    #[test]
    fn default_ua_always_sent() {
        let r = build(&[], None);
        assert_eq!(r.headers().get("user-agent").unwrap(), USER_AGENT);
    }

    #[test]
    fn custom_ua_overrides_default() {
        let r = build(&[pair("User-Agent", "my-agent/2.0")], None);
        assert_eq!(r.headers().get("user-agent").unwrap(), "my-agent/2.0");
    }

    #[test]
    fn session_placeholder_substituted() {
        let r = build(
            &[pair("x-opencode-session", "${session_id}")],
            Some("sess-abc"),
        );
        assert_eq!(r.headers().get("x-opencode-session").unwrap(), "sess-abc");
    }

    #[test]
    fn session_placeholder_without_session_is_empty() {
        let r = build(&[pair("x-opencode-session", "${session_id}")], None);
        assert_eq!(r.headers().get("x-opencode-session").unwrap(), "");
    }

    #[test]
    fn reserved_headers_not_overridden() {
        let rb = reqwest::Client::new()
            .post("http://localhost/")
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01");
        let r = apply_request_headers(
            rb,
            &[
                pair("Content-Type", "text/plain"),
                pair("anthropic-version", "evil"),
                pair("Authorization", "Bearer leaked"),
            ],
            None,
        )
        .build()
        .unwrap();
        assert_eq!(r.headers().get("content-type").unwrap(), "application/json");
        assert_eq!(r.headers().get("anthropic-version").unwrap(), "2023-06-01");
        assert!(r.headers().get("authorization").is_none());
    }

    #[test]
    fn invalid_header_skipped() {
        // 非法头名（含空格）跳过，不影响其余头
        let r = build(&[pair("bad name", "v"), pair("x-ok", "1")], None);
        assert!(r.headers().get("bad name").is_none());
        assert_eq!(r.headers().get("x-ok").unwrap(), "1");
    }
}
