pub mod anthropic;
pub mod dto;
pub mod keys;
pub mod openai_chat;
pub mod openai_responses;
pub mod proxy;
pub mod retry;
pub mod sse;
#[cfg(test)]
mod tests_integration;

pub use dto::{ProviderError, RunUsage, StreamDelta, StreamRequest, ToolDef};

use crate::core::config::{ApiFormat, ModelConfig};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 协议分发入口：按 `ModelConfig.api_format` 路由到对应协议适配器（trait 语义见
/// [docs/technical-design](../../../docs/technical-design.md) §4.2.2，P0 以函数分派实现、接口语义不变）。
/// 这里也是 provider 层的统一观测点：重试决策由上层 agent 主循环负责，
/// 故本层 Err 统一记 warn（可能瞬时），最终失败才由主循环记 error。
pub async fn stream_model(
    client: &reqwest::Client,
    model: &ModelConfig,
    key: Option<String>,
    req: StreamRequest,
    tx: mpsc::Sender<StreamDelta>,
    cancel: CancellationToken,
) -> Result<RunUsage, ProviderError> {
    let started = std::time::Instant::now();
    let result = match model.api_format {
        ApiFormat::OpenAiChat => openai_chat::stream(client, model, key, req, tx, cancel).await,
        ApiFormat::AnthropicMessages => {
            anthropic::stream(client, model, key, req, tx, cancel).await
        }
        ApiFormat::OpenAiResponses => {
            openai_responses::stream(client, model, key, req, tx, cancel).await
        }
    };
    match &result {
        Ok(u) => tracing::debug!(
            model = %model.name,
            ms = started.elapsed().as_millis() as u64,
            input = u.input,
            output = u.output,
            "LLM 请求完成"
        ),
        // 用户取消属正常路径，记 debug 避免噪音
        Err(ProviderError::Cancelled) => {
            tracing::debug!(model = %model.name, "LLM 请求被取消")
        }
        Err(e) => tracing::warn!(
            model = %model.name,
            ms = started.elapsed().as_millis() as u64,
            error = %e,
            "LLM 请求失败"
        ),
    }
    result
}
