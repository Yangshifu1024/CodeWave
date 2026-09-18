//! MiniMax Token Plan（国际 / 中国两套端点，同一份解析逻辑）。
//! - 国际：`GET https://api.minimax.io/v1/api/openplatform/coding_plan/remains`
//! - 中国：`GET https://api.minimaxi.com/v1/token_plan/remains`
//!
//! 两者的**计数语义相反**：国际响应里的 `current_*_usage_count` 实为「剩余」，
//! 中国响应里是「已用」（与上游实现一致，勿按字段名直觉修改）。

use super::super::{fetch_json, AuthStyle, QuotaEntry};
use super::{number, text};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::Value;
use std::cmp::Ordering;

const INTERNATIONAL_URL: &str = "https://api.minimax.io/v1/api/openplatform/coding_plan/remains";
const CHINA_URL: &str = "https://api.minimaxi.com/v1/token_plan/remains";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    International,
    China,
}

impl Endpoint {
    fn url(self) -> &'static str {
        match self {
            Self::International => INTERNATIONAL_URL,
            Self::China => CHINA_URL,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::International => "MiniMax API",
            // 两家共用一份解析，错误文案必须能区分是哪套端点（中国端点用 minimaxi.com）
            Self::China => "MiniMax API (CN)",
        }
    }

    /// 计数是「剩余」（国际）还是「已用」（中国）
    fn count_means_remaining(self) -> bool {
        self == Self::International
    }
}

/// 窗口字段表：(窗口键, 总量字段, 计数字段, 百分比字段, 重置偏移字段)
const WINDOWS: [(&str, &str, &str, &str, &str); 2] = [
    (
        "five_hour",
        "current_interval_total_count",
        "current_interval_usage_count",
        "current_interval_remaining_percent",
        "remains_time",
    ),
    (
        "week",
        "current_weekly_total_count",
        "current_weekly_usage_count",
        "current_weekly_remaining_percent",
        "weekly_remains_time",
    ),
];

pub(crate) async fn fetch(
    endpoint: Endpoint,
    key: &str,
    client: &reqwest::Client,
) -> Result<Vec<QuotaEntry>, String> {
    let body = fetch_json(client, endpoint.url(), key, AuthStyle::Bearer, endpoint.label()).await?;
    parse_usage(&body, endpoint, Utc::now())
}

fn model_name(model: &Value) -> String {
    text(model, "model_name").unwrap_or_default().to_ascii_lowercase()
}

/// 模型必须至少带一个重置提示，否则无法给出“多久后恢复”这一必要信息。
/// 不能只看 `remains_time`：中国端点那一族字段不一定回它，硬卡会把所有模型滤掉→整家误报失败。
fn has_reset_hint(model: &Value) -> bool {
    number(model, "remains_time").is_some() || number(model, "weekly_remains_time").is_some()
}

/// 只统计编码类模型（`minimax-m*` 与通用桶 `general`）。
fn is_coding_model(model: &Value) -> bool {
    let name = model_name(model);
    name.starts_with("minimax-m") || name == "general"
}

/// 单窗口的剩余百分比（供模型择优使用）。
fn window_percent(model: &Value, endpoint: Endpoint, window: usize) -> Option<f64> {
    let (_, total_field, count_field, percent_field, _) = WINDOWS[window];
    let total = number(model, total_field);
    let count = number(model, count_field);
    if let (Some(total), Some(count)) = (total, count) {
        if total > 0.0 {
            return Some(entry_percent(total, count, endpoint));
        }
    }
    number(model, percent_field).map(|p| p.clamp(0.0, 100.0))
}

/// 由计数换算剩余百分比（国际=剩余语义，中国=已用语义）。
fn entry_percent(total: f64, count: f64, endpoint: Endpoint) -> f64 {
    if endpoint.count_means_remaining() {
        (count.min(total) / total) * 100.0
    } else {
        let used = count.max(0.0);
        ((total - used) / total) * 100.0
    }
}

fn worst_percent(model: &Value, endpoint: Endpoint) -> f64 {
    let mut worst = f64::INFINITY;
    for window in 0..WINDOWS.len() {
        if let Some(percent) = window_percent(model, endpoint, window) {
            worst = worst.min(percent);
        }
    }
    worst
}

fn build_entries(model: &Value, endpoint: Endpoint, now: DateTime<Utc>) -> Vec<QuotaEntry> {
    let mut entries = Vec::new();
    for (window, (key, _total, _count, _percent, reset_field)) in WINDOWS.iter().enumerate() {
        let Some(remaining_percent) = window_percent(model, endpoint, window) else {
            continue;
        };
        let resets_at = number(model, reset_field)
            .filter(|ms| *ms > 0.0)
            .and_then(|ms| now.checked_add_signed(Duration::milliseconds(ms as i64)))
            .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true));
        entries.push(QuotaEntry::percent(key, None, 100.0 - remaining_percent, resets_at));
    }
    entries
}

/// 解析用量响应：`base_resp.status_code != 0` 判失败；模型择优（通配优先，否则剩余最差者）。
pub(crate) fn parse_usage(
    body: &Value,
    endpoint: Endpoint,
    now: DateTime<Utc>,
) -> Result<Vec<QuotaEntry>, String> {
    if let Some(base) = body.get("base_resp") {
        let code = base.get("status_code").and_then(Value::as_i64).unwrap_or(0);
        if code != 0 {
            let message = text(base, "status_msg").unwrap_or_else(|| "unknown".to_string());
            return Err(format!("{} 错误：{message}", endpoint.label()));
        }
    }

    let empty = Vec::new();
    let models: Vec<&Value> = body
        .get("model_remains")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|m| is_coding_model(m) && has_reset_hint(m))
        .collect();
    if models.is_empty() {
        return Err("MiniMax API 返回中未找到可展示的额度".to_string());
    }

    let wildcard = models
        .iter()
        .find(|m| model_name(m) == "minimax-m*")
        .copied();
    let chosen = wildcard.unwrap_or_else(|| {
        models
            .iter()
            .copied()
            .min_by(|a, b| {
                worst_percent(a, endpoint)
                    .partial_cmp(&worst_percent(b, endpoint))
                    .unwrap_or(Ordering::Equal)
            })
            .expect("models 非空")
    });

    let entries = build_entries(chosen, endpoint, now);
    if entries.is_empty() {
        return Err("MiniMax API 返回中未找到可展示的额度".to_string());
    }
    Ok(entries)
}
