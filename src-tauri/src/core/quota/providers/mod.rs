//! 各厂商额度适配器：协议与解析差异不出本模块
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//!
//! 每个适配器拆成「取数（`fetch`）+ 纯解析（`parse_*`）」两半：解析收 serde_json 值与
//! 注入的 `now`，因此单测不需要网络、也不需要时钟。

pub(crate) mod deepseek;
pub(crate) mod glm;
pub(crate) mod kimi;
pub(crate) mod minimax;
pub(crate) mod opencode_go;

use super::{ProviderKind, QuotaEntry};

/// 按提供商分发取数（凭证已解析完成）。
pub(crate) async fn fetch(
    kind: ProviderKind,
    key: &str,
    client: &reqwest::Client,
) -> Result<Vec<QuotaEntry>, String> {
    match kind {
        ProviderKind::OpenCodeGo => opencode_go::fetch(key, client).await,
        ProviderKind::DeepSeek => deepseek::fetch(key, client).await,
        ProviderKind::MiniMaxIntl => {
            minimax::fetch(minimax::Endpoint::International, key, client).await
        }
        ProviderKind::MiniMaxCn => minimax::fetch(minimax::Endpoint::China, key, client).await,
        ProviderKind::Kimi => kimi::fetch(key, client).await,
        ProviderKind::Zhipu => glm::fetch(glm::Flavor::Zhipu, key, client).await,
        ProviderKind::Zai => glm::fetch(glm::Flavor::Zai, key, client).await,
    }
}

/// 数值取值（数字或数字字符串都接受）。
pub(crate) fn number(value: &serde_json::Value, key: &str) -> Option<f64> {
    let raw = value.get(key)?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// 非空字符串取值。
pub(crate) fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    let raw = value.get(key)?.as_str()?.trim();
    (!raw.is_empty()).then(|| raw.to_string())
}
