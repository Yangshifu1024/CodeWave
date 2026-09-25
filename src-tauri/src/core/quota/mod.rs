//! 订阅额度域（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//!
//! **数据源 = CodeWave 自己的供应商配置**（`config.providers`）：逐家按 `base_url` 主机名
//! 匹配适配器（`providers/`），密钥经 host 层 keyring 解析；不再读 opencode 的配置与
//! auth.json（原凭证链与 opencode 运行目录解析已删除）。
//!
//! 分层：本层不依赖 tauri；host 命令只做校验与转调。数据模型 + 适配器注册表 + 行态分类
//! 都在这里，各厂商协议差异在 `providers/`（与 `core::agent::drive` 调用
//! `host::keyring::resolve_keys` 同一既有模式）。
//!
//! 设计约束：
//! - **按行态分类返回**（`ok` / `error` / `invalid` / `rejected` / `no_key` / `unsupported`），
//!   每一家配置过的供应商都有一行，不再「未配置凭证就整家消失」；
//! - 单家失败不影响其他家；每家请求 10s 超时；需发请求的全并发（`join_all`，不加并发上限）；
//! - 密钥与响应体绝不进日志，错误文案按密钥抹除（`sanitize`）；
//! - 「曾经成功过」记在**全局数据目录**的 `quota.json`（`state`）：一批刷新只写一次盘，
//!   且在批次末的进程内临界区里**重新读盘再合并**（`state::commit`），
//!   并发刷新不会互相覆盖记录。

pub(crate) mod providers;
pub mod state;
#[cfg(test)]
mod tests;

use crate::core::config::{ConfigState, KEYRING_PLACEHOLDER, ProviderConfig};
use chrono::{SecondsFormat, Utc};
use providers::FetchFailure;
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

/// 单家额度请求超时。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const USER_AGENT: &str = "CodeWave-Quota/1.0";

/// `unsupported` 的 reason：base_url 主机名未命中任何适配器。
pub const REASON_NO_ADAPTER: &str = "no_adapter";
/// `unsupported` 的 reason：base_url 为空/空白。
pub const REASON_EMPTY_BASE_URL: &str = "empty_base_url";

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
    /// 窗口级上游状态字面量（目前只有 opencode_go 带，如 `ok` / `rate-limited`；其它家 None）
    pub status: Option<String>,
}

impl QuotaEntry {
    /// 百分比行：入参为「已用百分比」，剩余取补（都收敛到 0-100 一位小数）。
    pub(crate) fn percent(
        key: &str,
        label: Option<String>,
        used_percent: f64,
        resets_at: Option<String>,
        status: Option<String>,
    ) -> Self {
        let used = clamp_percent(used_percent);
        Self {
            key: key.to_string(),
            label,
            used_percent: Some(used),
            remaining_percent: Some(clamp_percent(100.0 - used)),
            value_text: None,
            resets_at,
            status,
        }
    }

    /// 数值行（余额/计数）：前端直接展示文本；无上游窗口状态。
    pub(crate) fn value(key: &str, label: Option<String>, value_text: String) -> Self {
        Self {
            key: key.to_string(),
            label,
            used_percent: None,
            remaining_percent: None,
            value_text: Some(value_text),
            resets_at: None,
            status: None,
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

/// 提供商快照行态（wire 名 snake_case，前端按此渲染）：
/// - `Ok` 取数成功；`Error` 其它失败（超时/连接失败/5xx/429/解析失败，或曾成功过的 401/403/404）；
/// - `Invalid` 密钥形态不可用（keyring 回读失败，重试无用）；
/// - `Rejected` 401/403/404 且该家从未成功过（密钥/套餐被拒）；
/// - `NoKey` 命中适配器但没有解析出任何密钥；`Unsupported` 无法查询（无适配器 / 无 base_url）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Ok,
    Error,
    Invalid,
    Rejected,
    NoKey,
    Unsupported,
}

impl QuotaStatus {
    /// 是否属于「可查询类」（排序时排在 `unsupported` 之前）。
    fn is_queryable(&self) -> bool {
        !matches!(self, Self::Unsupported)
    }
}

/// 一家提供商的额度快照。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaSnapshot {
    /// CodeWave 供应商 id（`config.providers[].id`）
    pub provider_id: String,
    /// `provider.name`；空则回落 base_url 主机名（再无则回落 id）
    pub display_name: String,
    pub status: QuotaStatus,
    /// 仅 `unsupported`：`no_adapter` | `empty_base_url`
    pub reason: Option<String>,
    pub entries: Vec<QuotaEntry>,
    /// `error` / `invalid` / `rejected` 的原因（已脱敏）
    pub error: Option<String>,
    /// 密钥来源（`keyring` | `config`）；无密钥行为 None
    pub key_source: Option<String>,
    pub fetched_at: String,
    /// **上次成功取数的时刻**（RFC3339 秒；数据源 = 全局数据目录 `quota.json`）：
    /// 本轮成功即本轮时刻（`record_ok` 之后的值），本轮失败则带历史记录；
    /// 从未成功过（如首次即 401/403/404 的 `rejected`）恒为 None。
    /// 用途：区分「刚才还好好的」与「从来没成功过」。
    pub last_ok_at: Option<String>,
}

/// 本期支持的提供商（固定注册序即适配器匹配序）。
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

/// 适配器注册表（白名单：只有这些域的 base_url 才能查询额度）。
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
    /// 该提供商支持的 base_url 域名白名单（子域也算命中）。
    pub fn base_url_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::OpenCodeGo => &["opencode.ai"],
            Self::DeepSeek => &["api.deepseek.com", "deepseek.com"],
            Self::MiniMaxIntl => &["api.minimax.io", "minimax.io"],
            Self::MiniMaxCn => &["api.minimax.cn", "minimax.cn", "www.minimax.cn"],
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

/// 该 base_url 命中的适配器（主机名白名单判定；**不用模型 id 前缀**：
/// OpenCode Go 上也跑 DeepSeek 模型，前缀匹配会误判）。
pub(crate) fn kind_for_base_url(base_url: &str) -> Option<ProviderKind> {
    let host = host_of(base_url)?;
    ALL.iter()
        .copied()
        .find(|k| host_matches(&host, k.base_url_hosts()))
}

/// 展示名：`provider.name` 优先，空则回落 base_url 主机名，再无则回落 id。
pub(crate) fn display_name(provider: &ProviderConfig) -> String {
    let name = provider.name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    host_of(&provider.base_url).unwrap_or_else(|| provider.id.clone())
}

/// 全部配置过的供应商的额度快照（结果顺序即渲染顺序）：
/// ① 可查询类在前——`active_provider_id` 命中者置顶，其余按 `config.providers` 顺序；
/// ② `unsupported` 类殿后，按配置顺序。
///
/// `data_dir` = **全局数据目录**（`~/.codewave`）：额度是账号级事实，状态文件随全局走。
pub async fn snapshots(
    cfg: &ConfigState,
    active_provider_id: Option<&str>,
    data_dir: &Path,
) -> Vec<QuotaSnapshot> {
    let client = crate::provider::proxy::build_client(cfg);
    // 真实取数：client 移进闭包（reqwest::Client 内部 Arc，clone 廉价），
    // 使返回的 Future 不借用局部变量，可与其它请求统一 join_all。
    let fetcher = move |kind: ProviderKind, key: String| -> FetchBox {
        let client = client.clone();
        Box::pin(async move { providers::fetch(kind, &key, &client).await })
    };
    snapshots_with(cfg, active_provider_id, data_dir, &fetcher).await
}

/// 取数 Future（统一装箱：生产走 `providers::fetch`，单测注入假实现）。
pub(crate) type FetchBox =
    Pin<Box<dyn Future<Output = Result<Vec<QuotaEntry>, FetchFailure>> + Send>>;

/// 取数器：按「适配器种类 + 首密钥」返回取数 Future。
pub(crate) type Fetcher<'a> = &'a (dyn Fn(ProviderKind, String) -> FetchBox + Send + Sync);

/// 待发请求的一条（供应商下标 + 适配器 + 首密钥 + 密钥来源）。
struct Pending {
    index: usize,
    kind: ProviderKind,
    key: String,
    key_source: String,
}

/// 实际取用的那枚密钥（`resolve_provider_keys` 结果的 `[0]`）的出处：
/// 配置文件明文 → `config`；keyring 回读 → `keyring`。
///
/// 判据是**这枚 key 的实际出处**，不是「池里有没有占位符」：混合池
/// `["sk-plain", "__keyring__"]` 实际取用的是明文那枚（`resolve_provider_keys`
/// 把配置明文排在 keyring 回读之前）→ `config`，否则提示会说「密钥来源：系统钥匙串」
/// 而实际用的是配置文件里的明文。首项为空串被过滤后取到后续 key 时，同样按
/// 实际取用者的出处报。
fn key_source_of(provider: &ProviderConfig, used_key: &str) -> String {
    let from_config = provider
        .keys
        .iter()
        .any(|k| !k.is_empty() && k.as_str() != KEYRING_PLACEHOLDER && k.as_str() == used_key);
    if from_config { "config" } else { "keyring" }.to_string()
}

/// 快照主流程的可注入形态（单测传假取数器与临时目录）。
pub(crate) async fn snapshots_with(
    cfg: &ConfigState,
    active_provider_id: Option<&str>,
    data_dir: &Path,
    fetcher: Fetcher<'_>,
) -> Vec<QuotaSnapshot> {
    let fetched_at = now_rfc3339();
    let ids: Vec<String> = cfg.providers.iter().map(|p| p.id.clone()).collect();
    // 取数前读一份状态：只供本批行展示（历史 `last_ok_at`、401/403/404 的「曾成功过」判据）。
    // 真正的「读—改—写」在批次末的临界区里重做（`state::commit`）——那个才落盘。
    let state = state::QuotaState::load(data_dir);

    let mut rows: Vec<Option<QuotaSnapshot>> = vec![None; cfg.providers.len()];
    let mut pending: Vec<Pending> = Vec::new();

    for (index, provider) in cfg.providers.iter().enumerate() {
        // ① 不命中白名单 / base_url 为空 → unsupported（不发请求）
        if provider.base_url.trim().is_empty() {
            rows[index] = Some(plain_snapshot(
                provider,
                QuotaStatus::Unsupported,
                Some(REASON_EMPTY_BASE_URL.to_string()),
                None,
                None,
                &state,
                &fetched_at,
            ));
            continue;
        }
        let Some(kind) = kind_for_base_url(&provider.base_url) else {
            rows[index] = Some(plain_snapshot(
                provider,
                QuotaStatus::Unsupported,
                Some(REASON_NO_ADAPTER.to_string()),
                None,
                None,
                &state,
                &fetched_at,
            ));
            continue;
        };

        // ② 密钥解析：回读失败 → invalid；解析为空 → no_key
        match crate::host::keyring::resolve_provider_keys(provider) {
            Err(reason) => {
                rows[index] = Some(plain_snapshot(
                    provider,
                    QuotaStatus::Invalid,
                    None,
                    Some(sanitize(&reason, "")),
                    None,
                    &state,
                    &fetched_at,
                ));
            }
            Ok(keys) if keys.is_empty() => {
                rows[index] = Some(plain_snapshot(
                    provider,
                    QuotaStatus::NoKey,
                    None,
                    None,
                    None,
                    &state,
                    &fetched_at,
                ));
            }
            // ③ 只取 keys[0]（不轮换、不多 key 各一行）
            Ok(keys) => pending.push(Pending {
                index,
                kind,
                key: keys[0].clone(),
                key_source: key_source_of(provider, &keys[0]),
            }),
        }
    }

    // ④ 需发请求的全并发（保持 join_all，不加并发上限）
    let results = futures::future::join_all(
        pending
            .iter()
            .map(|item| (fetcher)(item.kind, item.key.clone())),
    )
    .await;

    let mut ok_ids: Vec<String> = Vec::new();
    for (item, result) in pending.iter().zip(results) {
        let provider = &cfg.providers[item.index];
        match result {
            Ok(entries) => {
                ok_ids.push(provider.id.clone());
                rows[item.index] = Some(QuotaSnapshot {
                    provider_id: provider.id.clone(),
                    display_name: display_name(provider),
                    status: QuotaStatus::Ok,
                    reason: None,
                    entries,
                    error: None,
                    key_source: Some(item.key_source.clone()),
                    fetched_at: fetched_at.clone(),
                    // 本轮成功：带本次时刻（record_ok 刚写入的值），不回显旧值
                    last_ok_at: Some(fetched_at.clone()),
                });
            }
            Err(failure) => {
                let status = classify_failure(&failure, state.has_succeeded(&provider.id));
                rows[item.index] = Some(plain_snapshot(
                    provider,
                    status,
                    None,
                    Some(failure.message),
                    Some(item.key_source.clone()),
                    &state,
                    &fetched_at,
                ));
            }
        }
    }

    // ⑤ 排序：可查询类在前（active 置顶，其余保持配置序），unsupported 殿后（配置序）
    let ordered = order(rows.into_iter().flatten().collect(), active_provider_id);

    // 批次末的原子提交：临界区内「重新读盘 → 清孤儿 → 记本批成功者 → 写盘一次」。
    // 取数（await）与落盘（同步文件 IO）彻底分离：持锁期不跨 await，既不阻塞别家
    // 请求也无死锁风险；重新读盘保证并发批次互相不覆盖记录。无成功且无孤儿时不写盘。
    state::commit(data_dir, &ids, &ok_ids, &fetched_at);

    ordered
}

/// 排序：稳定排序保证同组内保持配置顺序。
fn order(snapshots: Vec<QuotaSnapshot>, active_provider_id: Option<&str>) -> Vec<QuotaSnapshot> {
    let mut queryable: Vec<QuotaSnapshot> = Vec::new();
    let mut unsupported: Vec<QuotaSnapshot> = Vec::new();
    for snapshot in snapshots {
        if snapshot.status.is_queryable() {
            queryable.push(snapshot);
        } else {
            unsupported.push(snapshot);
        }
    }
    queryable.sort_by_key(|s| usize::from(Some(s.provider_id.as_str()) != active_provider_id));
    queryable.extend(unsupported);
    queryable
}

/// 失败归类：401/403/404 且**从未成功过** → rejected（密钥/套餐被拒，重试无用）；
/// 其余（超时 / 连接失败 / 5xx / 429 / 解析失败 / 曾成功过的 401/403/404）→ error。
fn classify_failure(failure: &FetchFailure, succeeded_before: bool) -> QuotaStatus {
    let rejected_status = matches!(failure.status, Some(401 | 403 | 404));
    if rejected_status && !succeeded_before {
        QuotaStatus::Rejected
    } else {
        QuotaStatus::Error
    }
}

/// 无额度明细的快照行（unsupported / invalid / no_key / rejected / error 共用）。
///
/// `last_ok_at` 按**行态无关**的统一规则取历史记录（`state`）：不因 `unsupported` /
/// `no_key` 就特判抹掉——历史上有过成功就照实带上。
fn plain_snapshot(
    provider: &ProviderConfig,
    status: QuotaStatus,
    reason: Option<String>,
    error: Option<String>,
    key_source: Option<String>,
    state: &state::QuotaState,
    fetched_at: &str,
) -> QuotaSnapshot {
    QuotaSnapshot {
        provider_id: provider.id.clone(),
        display_name: display_name(provider),
        status,
        reason,
        entries: Vec::new(),
        error,
        key_source,
        fetched_at: fetched_at.to_string(),
        last_ok_at: last_ok_at_of(state, &provider.id),
    }
}

/// 历史「上次成功时间」：`quota.json` 里该家**有记录**才带出。
///
/// 记录存在但值为空串（旧文件缺字段的兼容形态，`VerifiedProvider.last_ok_at` 默认空串）
/// 视同「无可用时间」→ None：空串不是合法 RFC3339，wire 上给前端只会渲染出空白。
/// 「是否曾成功过」仍由行态（`rejected` / `error`）表达，不依赖本字段。
fn last_ok_at_of(state: &state::QuotaState, provider_id: &str) -> Option<String> {
    state
        .verified
        .iter()
        .find(|v| v.provider_id == provider_id)
        .map(|v| v.last_ok_at.clone())
        .filter(|at| !at.is_empty())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
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
/// 有 HTTP 响应时带上状态码（供上层判 401/403/404 → rejected）；无响应/解析失败 status 为 None。
pub(crate) async fn fetch_json(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    style: AuthStyle,
    label: &str,
) -> Result<Value, FetchFailure> {
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
        .map_err(|e| {
            FetchFailure::message(format!("{label}：{}", sanitize(&e.to_string(), key)))
        })?;

    let status = response.status();
    let body = response.text().await.map_err(|e| {
        FetchFailure::message(format!("{label}：{}", sanitize(&e.to_string(), key)))
    })?;
    if !status.is_success() {
        return Err(FetchFailure {
            status: Some(status.as_u16()),
            message: format!("{label} {status}：{}", sanitize(body.trim(), key)),
        });
    }
    serde_json::from_str::<Value>(&body)
        .map_err(|_| FetchFailure::message(format!("{label}：响应不是合法 JSON")))
}
