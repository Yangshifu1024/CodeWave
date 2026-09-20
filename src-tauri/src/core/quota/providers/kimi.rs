//! Kimi Code：`GET https://api.kimi.com/coding/v1/usages`（Bearer）。
//!
//! 响应可能是 `{usage, limits}`，也可能包一层 `{data:{usage, limits}}`；
//! 每个限额项的明细可能在 `detail` 子对象里，重置时间有「字符串时间戳 / 秒数 / 窗口时长」
//! 三种来源（与上游解析保持一致）。

use super::super::{AuthStyle, QuotaEntry, fetch_json};
use super::{FetchFailure, number, text};
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use serde_json::Value;

pub(crate) const URL: &str = "https://api.kimi.com/coding/v1/usages";

/// 重置时间字符串字段候选。
const RESET_TEXT_FIELDS: [&str; 4] = ["reset_at", "resetAt", "reset_time", "resetTime"];
/// 重置「剩余秒数」字段候选。
const RESET_SECONDS_FIELDS: [&str; 3] = ["reset_in", "resetIn", "ttl"];

pub(crate) async fn fetch(
    key: &str,
    client: &reqwest::Client,
) -> Result<Vec<QuotaEntry>, FetchFailure> {
    let body = fetch_json(client, URL, key, AuthStyle::Bearer, "Kimi API").await?;
    parse_usage(&body, Utc::now()).map_err(FetchFailure::message)
}

fn as_object(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| v.is_object())
}

/// 解析用量响应：顶层 `usage` 一行 + `limits[]` 每项一行。
pub(crate) fn parse_usage(body: &Value, now: DateTime<Utc>) -> Result<Vec<QuotaEntry>, String> {
    let data = as_object(body.get("data"));
    let usage =
        as_object(data.and_then(|d| d.get("usage"))).or_else(|| as_object(body.get("usage")));
    let limits = data
        .and_then(|d| d.get("limits"))
        .or_else(|| body.get("limits"))
        .and_then(Value::as_array);

    let mut entries = Vec::new();
    if let Some(usage) = usage {
        if let Some(entry) = row(usage, usage, None, "usage", now) {
            entries.push(entry);
        }
    }
    if let Some(limits) = limits {
        for (index, item) in limits.iter().enumerate() {
            if !item.is_object() {
                continue;
            }
            let detail = as_object(item.get("detail")).unwrap_or(item);
            let window = as_object(item.get("window"));
            let label = label_of(item, detail, window);
            if let Some(entry) = row(detail, item, label, &format!("limit_{}", index + 1), now) {
                entries.push(entry);
            }
        }
    }

    if entries.is_empty() {
        return Err("Kimi API 返回中未找到可展示的额度窗口".to_string());
    }
    Ok(entries)
}

/// 一行额度：需要可用的 `limit` 才能算百分比。
/// 只有 `used` 而没有 `limit` 时**不能**按 0 处理——那会伪造出「剩余 0%」的红色告警，
/// 比缺数据更糟；此类行直接跳过（全部跳过时上层报「未找到可展示的额度窗口」）。
fn row(
    detail: &Value,
    item: &Value,
    label: Option<String>,
    key: &str,
    now: DateTime<Utc>,
) -> Option<QuotaEntry> {
    let limit = number(detail, "limit").filter(|l| *l > 0.0)?;
    // 同时要有 used 或 remaining：只有 limit 时算不出已用，不能乐观地当成 100% 剩余
    let used = number(detail, "used")
        .or_else(|| number(detail, "remaining").map(|remaining| limit - remaining))?
        .max(0.0);
    let remaining_percent = ((limit - used) / limit) * 100.0;
    let label = label.or_else(|| text(detail, "name").or_else(|| text(item, "name")));
    let resets_at = reset_at(detail, now).or_else(|| reset_at(item, now));
    Some(QuotaEntry::percent(
        key,
        label,
        100.0 - remaining_percent,
        resets_at,
        None,
    ))
}

/// 标签来源：显式名称优先，其次按窗口时长生成（`5h` / `7d`）。
fn label_of(item: &Value, detail: &Value, window: Option<&Value>) -> Option<String> {
    for source in [item, detail] {
        for field in ["name", "title", "scope"] {
            if let Some(value) = text(source, field) {
                return Some(value);
            }
        }
    }
    let duration = window
        .and_then(|w| number(w, "duration"))
        .or_else(|| number(item, "duration"))
        .or_else(|| number(detail, "duration"))?;
    if duration <= 0.0 {
        return None;
    }
    let unit = window
        .and_then(|w| text(w, "timeUnit"))
        .or_else(|| text(item, "timeUnit"))
        .or_else(|| text(detail, "timeUnit"))
        .unwrap_or_default()
        .to_ascii_uppercase();
    let label = if unit.contains("MINUTE") {
        if duration >= 60.0 && (duration % 60.0) == 0.0 {
            format!("{}h", duration / 60.0)
        } else {
            format!("{duration}m")
        }
    } else if unit.contains("HOUR") {
        format!("{duration}h")
    } else if unit.contains("DAY") {
        format!("{duration}d")
    } else {
        format!("{duration}s")
    };
    Some(label)
}

/// 重置时间：字符串时间戳 → 剩余秒数 → 窗口时长，三级来源。
fn reset_at(value: &Value, now: DateTime<Utc>) -> Option<String> {
    for field in RESET_TEXT_FIELDS {
        if let Some(raw) = text(value, field) {
            if let Some(parsed) = parse_timestamp(&raw) {
                return Some(parsed);
            }
        }
    }
    for field in RESET_SECONDS_FIELDS {
        if let Some(seconds) = number(value, field).filter(|s| *s > 0.0) {
            return now
                .checked_add_signed(Duration::seconds(seconds as i64))
                .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true));
        }
    }
    let duration = value
        .get("window")
        .and_then(|w| number(w, "duration"))
        .filter(|s| *s > 0.0)?;
    now.checked_add_signed(Duration::seconds(duration as i64))
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// 时间戳解析：RFC3339 优先，其次无时区 ISO/空格分隔（按 UTC 处理）。
fn parse_timestamp(raw: &str) -> Option<String> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Some(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        );
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            return Some(naive.and_utc().to_rfc3339_opts(SecondsFormat::Secs, true));
        }
    }
    None
}
