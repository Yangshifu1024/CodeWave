//! OpenCode Go：官方额度接口 `GET https://opencode.ai/zen/go/v1/usage`（Bearer）。
//! 响应形如 `{"usage":{"rolling":{"status":"ok","percent":12.5,"resetsAt":"…"},…}}`，
//! `percent` 是「已用」百分比。

use super::super::{AuthStyle, QuotaEntry, fetch_json};
use serde_json::Value;

pub(crate) const URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// 窗口顺序即展示顺序。
const WINDOWS: [&str; 3] = ["rolling", "weekly", "monthly"];

pub(crate) async fn fetch(key: &str, client: &reqwest::Client) -> Result<Vec<QuotaEntry>, String> {
    let body = fetch_json(client, URL, key, AuthStyle::Bearer, "OpenCode Go API").await?;
    parse_usage(&body)
}

/// 解析官方 usage 响应：单窗口形态异常只跳过该窗口，全部不可用才判失败。
pub(crate) fn parse_usage(body: &Value) -> Result<Vec<QuotaEntry>, String> {
    let usage = body
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(|| "OpenCode Go API 返回缺少 usage 字段".to_string())?;

    let mut entries = Vec::new();
    for window in WINDOWS {
        let Some(entry) = usage.get(window) else {
            continue;
        };
        if entry.get("status").and_then(Value::as_str) != Some("ok") {
            continue;
        }
        let Some(percent) = entry.get("percent").and_then(Value::as_f64) else {
            continue;
        };
        let resets_at = entry
            .get("resetsAt")
            .and_then(Value::as_str)
            .map(str::to_string);
        entries.push(QuotaEntry::percent(window, None, percent, resets_at));
    }

    if entries.is_empty() {
        return Err("OpenCode Go API 返回中未找到可展示的额度窗口".to_string());
    }
    Ok(entries)
}
