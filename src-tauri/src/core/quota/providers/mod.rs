//! 各厂商额度适配器：协议与解析差异不出本模块
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//!
//! 每个适配器拆成「取数（`fetch`）+ 纯解析（`parse_*`）」两半：解析收 serde_json 值与
//! 注入的 `now`，因此单测不需要网络、也不需要时钟。
//!
//! 失败统一为 `FetchFailure`：**带 HTTP 状态的失败**（4xx/5xx）与「无状态失败」
//!（超时/连接失败/解析失败）分开表达，上层据此判 `rejected` 与 `error`。

pub(crate) mod deepseek;
pub(crate) mod glm;
pub(crate) mod kimi;
pub(crate) mod minimax;
pub(crate) mod opencode_go;

use super::{ProviderKind, QuotaEntry};

/// 一次取数失败：`status` 是有 HTTP 响应时的状态码，无响应（超时/连接失败/解析失败）为 None。
/// `message` 已完成脱敏，可直接进快照与日志。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FetchFailure {
    pub(crate) status: Option<u16>,
    pub(crate) message: String,
}

impl FetchFailure {
    /// 无 HTTP 状态的失败（超时/连接失败/解析失败）。
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
        }
    }
}

/// 按提供商分发取数（密钥已解析完成）。
pub(crate) async fn fetch(
    kind: ProviderKind,
    key: &str,
    client: &reqwest::Client,
) -> Result<Vec<QuotaEntry>, FetchFailure> {
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
