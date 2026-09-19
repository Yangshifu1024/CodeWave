//! 订阅额度域（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//!
//! 与具体厂商无关的数据模型 + 提供商注册表 + 凭证链（`credentials`）+ opencode 运行目录解析
//! （`opencode_paths`）；各厂商适配器在 `providers/`。分层：本层不依赖 tauri，
//! host 命令只做校验与转调。
//!
//! 设计约束：
//! - 「未配置凭证」不是错误（`Missing` 的提供商不进返回列表）；
//! - 单家失败不影响其他家；每家请求 10s 超时；
//! - 凭证与响应体绝不进日志，错误文案按 token 抹除。

pub mod credentials;
pub mod opencode_paths;
pub(crate) mod providers;
#[cfg(test)]
mod tests;

use crate::core::config::ConfigState;
use chrono::{SecondsFormat, Utc};
use credentials::{Credential, CredentialSpec};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// 单家额度请求超时。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const USER_AGENT: &str = "CodeWave-Quota/1.0";

/// 一行额度事实：百分比行（`remaining_percent`）或数值行（`value_text`），二者必居其一。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaEntry {
    /// 窗口键（rolling/weekly/monthly/five_hour/week/mcp/usage/limit_N/balance_xxx）
    pub key: String,
    /// 厂商自带标签（Kimi/Zhipu 的 limits 有 name/scope 时使用）
    pub label: Option<String>,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub value_text: Option<String>,
    /// 重置时间（RFC3339；数值行恒为 None）
    pub resets_at: Option<String>,
}

impl QuotaEntry {
    /// 百分比行：入参为「已用百分比」，剩余取补（都收敛到 0-100 一位小数）。
    pub(crate) fn percent(
        key: &str,
        label: Option<String>,
        used_percent: f64,
        resets_at: Option<String>,
    ) -> Self {
        let used = clamp_percent(used_percent);
        Self {
            key: key.to_string(),
            label,
            used_percent: Some(used),
            remaining_percent: Some(clamp_percent(100.0 - used)),
            value_text: None,
            resets_at,
        }
    }

    /// 数值行（余额/计数）：前端直接展示文本。
    pub(crate) fn value(key: &str, label: Option<String>, value_text: String) -> Self {
        Self {
            key: key.to_string(),
            label,
            used_percent: None,
            remaining_percent: None,
            value_text: Some(value_text),
            resets_at: None,
        }
    }
}

/// 一位小数取整（前端展示与测试断言都以此为准）。
pub(crate) fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// 百分比收敛到 0-100。
pub(crate) fn clamp_percent(value: f64) -> f64 {
    round1(value.clamp(0.0, 100.0))
}

/// 提供商快照状态：`Invalid` = 凭证形态不可用（不是请求失败，重试无用）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Ok,
    Invalid,
    Error,
}

/// 一家提供商的额度快照。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaSnapshot {
    pub provider_id: String,
    pub display_name: String,
    pub status: QuotaStatus,
    pub entries: Vec<QuotaEntry>,
    pub error: Option<String>,
    /// 凭证来源（`env:NAME` / `opencode.jsonc` / `opencode.json` / `auth.json`）——前端悬浮排障用
    pub credential_source: Option<String>,
    pub fetched_at: String,
}

/// 本期支持的提供商（固定注册序即默认展示序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenCodeGo,
    DeepSeek,
    MiniMaxIntl,
    MiniMaxCn,
    Kimi,
    Zhipu,
    Zai,
}

/// 固定注册序（未命中「当前会话提供商」时的展示顺序）。
pub const ALL: &[ProviderKind] = &[
    ProviderKind::OpenCodeGo,
    ProviderKind::DeepSeek,
    ProviderKind::MiniMaxIntl,
    ProviderKind::MiniMaxCn,
    ProviderKind::Kimi,
    ProviderKind::Zhipu,
    ProviderKind::Zai,
];

impl ProviderKind {
    pub fn id(&self) -> &'static str {
        match self {
            Self::OpenCodeGo => "opencode-go",
            Self::DeepSeek => "deepseek",
            Self::MiniMaxIntl => "minimax-coding-plan",
            Self::MiniMaxCn => "minimax-china-coding-plan",
            Self::Kimi => "kimi-code",
            Self::Zhipu => "zhipu-coding-plan",
            Self::Zai => "zai-coding-plan",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenCodeGo => "OpenCode Go",
            Self::DeepSeek => "DeepSeek",
            Self::MiniMaxIntl => "MiniMax Token Plan",
            Self::MiniMaxCn => "MiniMax Token Plan (CN)",
            Self::Kimi => "Kimi Code",
            Self::Zhipu => "Zhipu Coding Plan",
            Self::Zai => "Z.ai Coding Plan",
        }
    }

    /// 凭证来源声明（环境变量 → 配置 provider 键 → auth.json 条目键）。
    pub fn credential_spec(&self) -> CredentialSpec {
        match self {
            Self::OpenCodeGo => CredentialSpec {
                env_vars: &["OPENCODE_API_KEY"],
                config_keys: &["opencode-go", "opencode"],
                auth_keys: &["opencode-go", "opencode"],
            },
            Self::DeepSeek => CredentialSpec {
                env_vars: &["DEEPSEEK_API_KEY"],
                config_keys: &["deepseek"],
                auth_keys: &["deepseek"],
            },
            Self::MiniMaxIntl => CredentialSpec {
                env_vars: &["MINIMAX_CODING_PLAN_API_KEY", "MINIMAX_API_KEY"],
                config_keys: &["minimax-coding-plan", "minimax"],
                auth_keys: &["minimax-coding-plan"],
            },
            Self::MiniMaxCn => CredentialSpec {
                env_vars: &["MINIMAX_CHINA_CODING_PLAN_API_KEY"],
                config_keys: &[
                    "minimax-china-coding-plan",
                    "minimax-cn-coding-plan",
                    "minimax-cn",
                ],
                auth_keys: &["minimax-china-coding-plan", "minimax-cn-coding-plan"],
            },
            Self::Kimi => CredentialSpec {
                env_vars: &["KIMI_API_KEY", "KIMI_CODE_API_KEY"],
                config_keys: &["kimi-for-coding", "kimi-code", "kimi"],
                auth_keys: &["kimi-for-coding", "kimi-code", "kimi"],
            },
            Self::Zhipu => CredentialSpec {
                env_vars: &["ZHIPU_API_KEY", "ZHIPU_CODING_PLAN_API_KEY"],
                config_keys: &[
                    "zhipu",
                    "zhipu-coding-plan",
                    "zhipuai-coding-plan",
                    "glm-coding-plan",
                ],
                auth_keys: &["zhipu-coding-plan", "zhipuai-coding-plan"],
            },
            Self::Zai => CredentialSpec {
                env_vars: &["ZAI_API_KEY", "ZAI_CODING_PLAN_API_KEY"],
                config_keys: &["zai", "zai-coding-plan", "glm"],
                auth_keys: &["zai-coding-plan"],
            },
        }
    }

    /// 该提供商的 base_url 域名（「当前会话提供商置顶」按域名判定，**不用模型 id 前缀**：
    /// OpenCode Go 上也跑 DeepSeek 模型，前缀匹配会误判）。
    pub fn base_url_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::OpenCodeGo => &["opencode.ai"],
            Self::DeepSeek => &["api.deepseek.com", "deepseek.com"],
            Self::MiniMaxIntl => &["api.minimax.io", "minimax.io"],
            Self::MiniMaxCn => &["api.minimaxi.com", "minimaxi.com"],
            Self::Kimi => &["api.kimi.com", "kimi.com", "api.moonshot.cn", "moonshot.cn"],
            Self::Zhipu => &["bigmodel.cn"],
            Self::Zai => &["api.z.ai", "z.ai"],
        }
    }
}

/// 从 base_url 取小写主机名（去协议、去端口、去路径）。
pub(crate) fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url.split("://").nth(1).unwrap_or(base_url);
    let host = rest.split(['/', '?', '#']).next()?.split('@').next_back()?;
    let host = host.split(':').next()?.trim().to_lowercase();
    (!host.is_empty()).then_some(host)
}

fn host_matches(host: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|c| host == *c || host.ends_with(&format!(".{c}")))
}

/// 展示顺序：当前会话模型所属 provider 的 base_url 命中 → 置顶；其余按固定注册序。
pub fn order_for(active_base_url: Option<&str>) -> Vec<ProviderKind> {
    let matched = active_base_url.and_then(host_of).and_then(|host| {
        ALL.iter()
            .copied()
            .find(|k| host_matches(&host, k.base_url_hosts()))
    });
    let mut ordered = Vec::with_capacity(ALL.len());
    if let Some(first) = matched {
        ordered.push(first);
    }
    ordered.extend(ALL.iter().copied().filter(|k| Some(*k) != matched));
    ordered
}

/// 全部已配置提供商的快照（未配置凭证的不出现在结果里）。
pub async fn snapshots(cfg: &ConfigState, active_base_url: Option<&str>) -> Vec<QuotaSnapshot> {
    let client = crate::provider::proxy::build_client(cfg);
    let futures = order_for(active_base_url)
        .into_iter()
        .map(|kind| snapshot_one(&client, kind));
    futures::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect()
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn snapshot_one(client: &reqwest::Client, kind: ProviderKind) -> Option<QuotaSnapshot> {
    let spec = kind.credential_spec();
    let (key, source) = match credentials::resolve(&spec) {
        Credential::Missing => return None,
        Credential::Invalid(reason) => {
            return Some(QuotaSnapshot {
                provider_id: kind.id().to_string(),
                display_name: kind.display_name().to_string(),
                status: QuotaStatus::Invalid,
                entries: Vec::new(),
                error: Some(reason),
                credential_source: Some("auth.json".to_string()),
                fetched_at: now_rfc3339(),
            });
        }
        Credential::Found { key, source } => (key, source),
    };

    let (status, entries, error) = match providers::fetch(kind, &key, client).await {
        Ok(entries) => (QuotaStatus::Ok, entries, None),
        Err(message) => (QuotaStatus::Error, Vec::new(), Some(message)),
    };
    Some(QuotaSnapshot {
        provider_id: kind.id().to_string(),
        display_name: kind.display_name().to_string(),
        status,
        entries,
        error,
        credential_source: Some(source),
        fetched_at: now_rfc3339(),
    })
}

/// Authorization 头风格：多数厂商 `Bearer <key>`，GLM 系（Zhipu/Z.ai）用裸 key。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AuthStyle {
    Bearer,
    Raw,
}

/// 抹除文本里的密钥（长度 >= 8 才处理，避免把普通词替换掉），并压缩空白 + 截断。
pub(crate) fn sanitize(text: &str, secret: &str) -> String {
    let redacted = if secret.len() >= 8 {
        text.replace(secret, "[redacted]")
    } else {
        text.to_string()
    };
    let squeezed = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    squeezed.chars().take(160).collect()
}

/// 统一的 GET + JSON 响应处理：非 2xx 与解析失败都返回面向用户的错误文案。
pub(crate) async fn fetch_json(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    style: AuthStyle,
    label: &str,
) -> Result<Value, String> {
    let auth = match style {
        AuthStyle::Bearer => format!("Bearer {key}"),
        AuthStyle::Raw => key.to_string(),
    };
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, auth)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("{label}：{}", sanitize(&e.to_string(), key)))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("{label}：{}", sanitize(&e.to_string(), key)))?;
    if !status.is_success() {
        return Err(format!("{label} {status}：{}", sanitize(body.trim(), key)));
    }
    serde_json::from_str::<Value>(&body).map_err(|_| format!("{label}：响应不是合法 JSON"))
}
