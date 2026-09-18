//! Zhipu / Z.ai Coding Plan：`GET https://bigmodel.cn/api/monitor/usage/quota/limit`
//! 与 `GET https://api.z.ai/api/monitor/usage/quota/limit`。
//!
//! 两家的鉴权头是**裸 key**（不加 `Bearer ` 前缀，与上游一致），响应为
//! `data.limits[]`（Z.ai 另有 `limits` 顶层兜底），每项含
//! `type`/`unit`/`percentage`（已用）/`nextResetTime`（毫秒时间戳）。

use super::super::{fetch_json, AuthStyle, QuotaEntry};
use chrono::{DateTime, SecondsFormat};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    Zhipu,
    Zai,
}

impl Flavor {
    fn url(self) -> &'static str {
        match self {
            Self::Zhipu => "https://bigmodel.cn/api/monitor/usage/quota/limit",
            Self::Zai => "https://api.z.ai/api/monitor/usage/quota/limit",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Zhipu => "Zhipu API",
            Self::Zai => "Z.ai API",
        }
    }
}

pub(crate) async fn fetch(
    flavor: Flavor,
    key: &str,
    client: &reqwest::Client,
) -> Result<Vec<QuotaEntry>, String> {
    let body = fetch_json(client, flavor.url(), key, AuthStyle::Raw, flavor.label()).await?;
    parse_limits(&body, flavor)
}

/// 解析 limits 数组：TOKENS_LIMIT 的 unit 3/6 → 5 小时/每周；TIME_LIMIT → MCP 时长限制。
pub(crate) fn parse_limits(body: &Value, flavor: Flavor) -> Result<Vec<QuotaEntry>, String> {
    if flavor == Flavor::Zai {
        let code_failed = body
            .get("code")
            .and_then(Value::as_i64)
            .map(|code| code >= 400)
            .unwrap_or(false);
        if body.get("success").and_then(Value::as_bool) == Some(false) || code_failed {
            let message = body
                .get("msg")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| match body.get("code").and_then(Value::as_i64) {
                    Some(code) => format!("Z.ai API 错误 {code}"),
                    None => "Z.ai API 错误".to_string(),
                });
            return Err(message);
        }
    }

    let limits = match flavor {
        Flavor::Zai => body
            .pointer("/data/limits")
            .or_else(|| body.get("limits")),
        Flavor::Zhipu => body.pointer("/data/limits"),
    }
    .and_then(Value::as_array)
    .ok_or_else(|| format!("{} 返回缺少 limits 数组", flavor.label()))?;

    let mut entries = Vec::new();
    for limit in limits {
        let Some(percentage) = limit.get("percentage").and_then(Value::as_f64) else {
            continue;
        };
        let kind = limit.get("type").and_then(Value::as_str).unwrap_or_default();
        let unit = limit.get("unit").and_then(Value::as_i64);
        let is_quota = kind == "TOKENS_LIMIT"
            || (flavor == Flavor::Zai && kind == "CREDIT_LIMIT");
        let key = if is_quota && unit == Some(3) {
            "five_hour"
        } else if is_quota && unit == Some(6) {
            "weekly"
        } else if kind == "TIME_LIMIT" {
            "mcp"
        } else {
            continue;
        };
        let resets_at = limit
            .get("nextResetTime")
            .and_then(Value::as_f64)
            .filter(|ms| *ms > 0.0)
            .and_then(|ms| DateTime::from_timestamp_millis(ms as i64))
            .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true));
        entries.push(QuotaEntry::percent(key, None, percentage, resets_at));
    }

    if entries.is_empty() {
        return Err(format!(
            "{} 返回中未找到可展示的额度窗口",
            flavor.label()
        ));
    }
    Ok(entries)
}
