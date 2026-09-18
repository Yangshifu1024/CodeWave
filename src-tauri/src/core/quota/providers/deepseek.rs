//! DeepSeek：余额接口 `GET https://api.deepseek.com/user/balance`（Bearer）。
//! 响应含 `is_available` 与 `balance_infos[]`（currency/total_balance/granted_balance/topped_up_balance）。
//! 这是**数值行**（余额）而非百分比行。

use super::super::{fetch_json, AuthStyle, QuotaEntry};
use serde_json::Value;

pub(crate) const URL: &str = "https://api.deepseek.com/user/balance";

/// 只认官方支持的两种币种（与上游一致，其余币种一律忽略）。
const CURRENCIES: [&str; 2] = ["CNY", "USD"];

pub(crate) async fn fetch(key: &str, client: &reqwest::Client) -> Result<Vec<QuotaEntry>, String> {
    let body = fetch_json(client, URL, key, AuthStyle::Bearer, "DeepSeek API").await?;
    parse_balance(&body)
}

fn amount(value: Option<&Value>) -> Option<f64> {
    let raw = value?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// 解析余额响应：按币种出一行数值行；总额缺失时退化为「赠送 + 充值」之和。
pub(crate) fn parse_balance(body: &Value) -> Result<Vec<QuotaEntry>, String> {
    let available = body
        .get("is_available")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let empty = Vec::new();
    let infos = body
        .get("balance_infos")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut entries = Vec::new();
    for info in infos {
        let Some(currency) = info
            .get("currency")
            .and_then(Value::as_str)
            .map(|c| c.trim().to_ascii_uppercase())
        else {
            continue;
        };
        if !CURRENCIES.contains(&currency.as_str()) {
            continue;
        }
        let total = amount(info.get("total_balance")).or_else(|| {
            let granted = amount(info.get("granted_balance"));
            let topped = amount(info.get("topped_up_balance"));
            match (granted, topped) {
                (Some(g), Some(t)) => Some(g + t),
                (Some(g), None) => Some(g),
                (None, Some(t)) => Some(t),
                (None, None) => None,
            }
        });
        let Some(total) = total else { continue };
        let suffix = if available { "" } else { " · 不可用" };
        entries.push(QuotaEntry::value(
            &format!("balance_{}", currency.to_ascii_lowercase()),
            None,
            format!("{currency} {total:.2}{suffix}"),
        ));
    }

    if entries.is_empty() {
        return Err("DeepSeek API 返回中未找到可展示的余额".to_string());
    }
    Ok(entries)
}
