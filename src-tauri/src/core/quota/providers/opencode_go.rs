//! OpenCode Go：官方额度接口 `GET https://opencode.ai/zen/go/v1/usage`（Bearer）。
//! 响应形如 `{"usage":{"rolling":{"status":"ok","percent":12.5,"resetsAt":"…"},…}}`，
//! `percent` 是「已用」百分比。
//!
//! `status` 不止 `ok`：窗口用满时上游给的是 **`rate-limited`**（本机实调样例：
//! `{"weekly":{"status":"rate-limited","percent":100,"resetsAt":"…"}}`），
//! 所以它只作诊断信息，**绝不作为过滤条件**（见 `parse_usage`）。

use super::super::{AuthStyle, QuotaEntry, fetch_json};
use super::{FetchFailure, number};
use serde_json::Value;

pub(crate) const URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// 窗口顺序即展示顺序。
const WINDOWS: [&str; 3] = ["rolling", "weekly", "monthly"];

pub(crate) async fn fetch(
    key: &str,
    client: &reqwest::Client,
) -> Result<Vec<QuotaEntry>, FetchFailure> {
    let body = fetch_json(client, URL, key, AuthStyle::Bearer, "OpenCode Go API").await?;
    parse_usage(&body).map_err(FetchFailure::message)
}

/// 解析官方 usage 响应：展示条件只有「窗口存在 + `percent` 可用」，`status` 仍**不参与过滤**，
/// 但原样带进 `QuotaEntry.status`（上游字面量，例如用满时的 `rate-limited`），
/// 供前端区分「用满被限流」与普通高占用。
/// 反例（本机实调 2026-09-20）：weekly 用满 →
/// `status: "rate-limited", percent: 100`；按 `status == "ok"` 整窗过滤会让
/// 「本周剩余 0%」——最该被看到的一档——静默消失。
/// `percent` 缺失/非数字（数字字符串仍接受）才跳过该窗口；全部不可用才判失败。
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
        let Some(percent) = number(entry, "percent") else {
            continue;
        };
        let resets_at = entry
            .get("resetsAt")
            .and_then(Value::as_str)
            .map(str::to_string);
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string);
        entries.push(QuotaEntry::percent(window, None, percent, resets_at, status));
    }

    if entries.is_empty() {
        return Err("OpenCode Go API 返回中未找到可展示的额度窗口".to_string());
    }
    Ok(entries)
}
