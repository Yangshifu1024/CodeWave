//! `core::quota` 单测：base_url 白名单匹配、行态分类与排序、`quota.json` 状态落盘，
//! 以及 7 家适配器的纯解析。
//!
//! 不发真实网络请求：取数器注入假实现（`FakeFetcher`），状态文件写在临时目录。
//! 例外：`live_probe_*`（默认 `#[ignore]`）走真实网络与真实全局数据目录。

use super::providers::{FetchFailure, deepseek, glm, kimi, minimax, opencode_go};
use super::state::{self, QuotaState};
use super::{
    FetchBox, ProviderKind, QuotaEntry, QuotaStatus, REASON_EMPTY_BASE_URL, REASON_NO_ADAPTER,
    classify_failure, display_name, host_of, key_source_of, kind_for_base_url, sanitize,
    snapshots_with,
};
use crate::core::config::{ConfigState, KEYRING_PLACEHOLDER, ProviderConfig};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------- 测试脚手架 ----------

fn provider(id: &str, name: &str, base_url: &str, keys: &[&str]) -> ProviderConfig {
    ProviderConfig {
        id: id.into(),
        name: name.into(),
        base_url: base_url.into(),
        keys: keys.iter().map(|k| k.to_string()).collect(),
        ..Default::default()
    }
}

fn config(providers: Vec<ProviderConfig>) -> ConfigState {
    ConfigState {
        providers,
        ..Default::default()
    }
}

/// 一个可展示的百分比行（重置时间固定，便于断言）。
fn percent_entry(key: &str, used: f64) -> QuotaEntry {
    QuotaEntry::percent(
        key,
        None,
        used,
        Some("2026-09-21T00:00:00Z".to_string()),
        None,
    )
}

/// 结构化取数失败（`status = None` 表示超时/连接失败一类无响应失败）。
fn failure(status: Option<u16>) -> FetchFailure {
    FetchFailure {
        status,
        message: format!("测试取数失败（status={status:?}）"),
    }
}

/// 假取数器：按「首密钥」回放预设结果，同时记录调用次数与实际收到的 key。
struct FakeFetcher {
    replies: HashMap<String, Result<Vec<QuotaEntry>, FetchFailure>>,
    calls: AtomicUsize,
    keys: Mutex<Vec<String>>,
}

impl FakeFetcher {
    fn new(replies: Vec<(&str, Result<Vec<QuotaEntry>, FetchFailure>)>) -> Self {
        Self {
            replies: replies
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            calls: AtomicUsize::new(0),
            keys: Mutex::new(Vec::new()),
        }
    }

    fn fetcher(&self) -> impl Fn(ProviderKind, String) -> FetchBox + '_ {
        move |_kind, key: String| {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.keys.lock().unwrap().push(key.clone());
            let reply = match self.replies.get(&key) {
                Some(reply) => reply.clone(),
                None => Err(failure(None)),
            };
            Box::pin(async move { reply })
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn keys(&self) -> Vec<String> {
        self.keys.lock().unwrap().clone()
    }
}

fn ids(snapshots: &[super::QuotaSnapshot]) -> Vec<String> {
    snapshots.iter().map(|s| s.provider_id.clone()).collect()
}

// ---------- 适配器匹配 ----------

#[test]
fn kind_for_base_url_matches_whitelisted_hosts_only() {
    assert_eq!(
        kind_for_base_url("https://api.deepseek.com/v1"),
        Some(ProviderKind::DeepSeek)
    );
    assert_eq!(
        kind_for_base_url("https://opencode.ai/zen/go/v1"),
        Some(ProviderKind::OpenCodeGo)
    );
    assert_eq!(
        kind_for_base_url("https://zen.opencode.ai/v1"),
        Some(ProviderKind::OpenCodeGo)
    );
    assert_eq!(
        kind_for_base_url("https://api.minimax.io/v1"),
        Some(ProviderKind::MiniMaxIntl)
    );
    assert_eq!(
        kind_for_base_url("https://api.minimaxi.com/v1/token_plan/remains"),
        Some(ProviderKind::MiniMaxCn)
    );
    assert_eq!(
        kind_for_base_url("https://api.kimi.com/coding/v1"),
        Some(ProviderKind::Kimi)
    );
    assert_eq!(
        kind_for_base_url("https://api.moonshot.cn/v1"),
        Some(ProviderKind::Kimi)
    );
    assert_eq!(
        kind_for_base_url("https://bigmodel.cn/api/monitor"),
        Some(ProviderKind::Zhipu)
    );
    assert_eq!(
        kind_for_base_url("https://api.z.ai/api/monitor"),
        Some(ProviderKind::Zai)
    );
    // 自建/中转网关、空值、后缀伪冒域一律不命中（额度接口不可查）
    assert_eq!(kind_for_base_url("https://my-gateway.example.com/v1"), None);
    assert_eq!(kind_for_base_url("https://evildeepseek.com/v1"), None);
    assert_eq!(kind_for_base_url(""), None);
}

#[test]
fn host_of_handles_ports_userinfo_and_missing_scheme() {
    assert_eq!(
        host_of("https://api.z.ai:8443/api").as_deref(),
        Some("api.z.ai")
    );
    assert_eq!(
        host_of("https://user@api.kimi.com/x").as_deref(),
        Some("api.kimi.com")
    );
    assert_eq!(
        host_of("api.minimax.io/v1").as_deref(),
        Some("api.minimax.io")
    );
    assert_eq!(host_of("").as_deref(), None);
}

#[test]
fn display_name_falls_back_to_base_url_host_then_id() {
    assert_eq!(
        display_name(&provider(
            "p1",
            "  My Kimi  ",
            "https://api.kimi.com/x",
            &[]
        )),
        "My Kimi"
    );
    assert_eq!(
        display_name(&provider("p2", "   ", "https://api.kimi.com/x", &[])),
        "api.kimi.com"
    );
    assert_eq!(display_name(&provider("p3", "", "", &[])), "p3");
}

// ---------- 行态分类 ----------

#[tokio::test]
async fn unsupported_rows_never_send_requests_and_sort_last() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![
        provider("p-empty", "空端点", "", &["sk-empty"]),
        provider(
            "p-ok",
            "DeepSeek",
            "https://api.deepseek.com/v1",
            &["sk-ok"],
        ),
        provider(
            "p-gateway",
            "中转",
            "https://gateway.example.com/v1",
            &["sk-gateway"],
        ),
    ]);
    let fake = FakeFetcher::new(vec![("sk-ok", Ok(vec![percent_entry("balance", 10.0)]))]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;

    // 可查询类在前，unsupported 殿后（均按配置序）
    assert_eq!(ids(&out), vec!["p-ok", "p-empty", "p-gateway"]);
    assert_eq!(out[0].status, QuotaStatus::Ok);
    assert_eq!(out[0].key_source.as_deref(), Some("config"));
    assert_eq!(out[1].status, QuotaStatus::Unsupported);
    assert_eq!(out[1].reason.as_deref(), Some(REASON_EMPTY_BASE_URL));
    assert_eq!(out[1].error, None);
    assert_eq!(out[1].key_source, None);
    assert_eq!(out[2].status, QuotaStatus::Unsupported);
    assert_eq!(out[2].reason.as_deref(), Some(REASON_NO_ADAPTER));
    // 只有真正要查的那一家发了请求，且只取 keys[0]
    assert_eq!(fake.calls(), 1);
    assert_eq!(fake.keys(), vec!["sk-ok".to_string()]);
}

#[tokio::test]
async fn no_key_and_invalid_are_distinguished() {
    let dir = tempfile::tempdir().unwrap();
    // 占位符形态的 account 用随机 uuid（现实中就是 config 里的供应商 id）
    let keyring_id = uuid::Uuid::new_v4().to_string();
    let cfg = config(vec![
        provider("p-nokey", "无 key", "https://api.deepseek.com/v1", &[]),
        provider(
            &keyring_id,
            "坏 key",
            "https://api.kimi.com/coding/v1",
            &[KEYRING_PLACEHOLDER],
        ),
    ]);
    let fake = FakeFetcher::new(vec![]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;

    assert_eq!(ids(&out), vec!["p-nokey".to_string(), keyring_id]);
    assert_eq!(out[0].status, QuotaStatus::NoKey);
    assert_eq!(out[0].error, None);
    assert_eq!(out[0].key_source, None);
    // keyring 回读失败 ≠ 没有 key：判 invalid 并带原因（重试无用）
    assert_eq!(out[1].status, QuotaStatus::Invalid);
    assert!(
        out[1].error.as_deref().is_some_and(|e| !e.is_empty()),
        "invalid 必须带原因：{:?}",
        out[1].error
    );
    assert_eq!(out[1].key_source, None);
    assert_eq!(fake.calls(), 0);
}

#[test]
fn failure_classification_splits_rejected_from_error() {
    // 401/403/404 且从未成功过 → rejected（密钥/套餐被拒，重试无用）
    for code in [401u16, 403, 404] {
        assert_eq!(
            classify_failure(&failure(Some(code)), false),
            QuotaStatus::Rejected,
            "status {code} 且从未成功过"
        );
        // 曾成功过 → 临时失效，可重试
        assert_eq!(
            classify_failure(&failure(Some(code)), true),
            QuotaStatus::Error,
            "status {code} 但曾成功过"
        );
    }
    // 其它 HTTP 失败与无响应失败（超时/连接失败/解析失败）一律 error
    for status in [Some(429u16), Some(500), Some(502), Some(200), None] {
        assert_eq!(
            classify_failure(&failure(status), false),
            QuotaStatus::Error
        );
        assert_eq!(classify_failure(&failure(status), true), QuotaStatus::Error);
    }
}

#[tokio::test]
async fn rejected_first_time_then_error_after_a_success() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![provider(
        "p-ds",
        "DeepSeek",
        "https://api.deepseek.com/v1",
        &["sk-ds"],
    )]);

    // 首轮成功 → 记入 quota.json
    let ok = FakeFetcher::new(vec![("sk-ds", Ok(vec![percent_entry("balance", 0.0)]))]);
    let first = snapshots_with(&cfg, None, dir.path(), &ok.fetcher()).await;
    assert_eq!(first[0].status, QuotaStatus::Ok);
    let saved = QuotaState::load(dir.path());
    assert!(saved.has_succeeded("p-ds"));
    assert_eq!(
        saved.verified[0].last_ok_at, first[0].fetched_at,
        "last_ok_at 应等于成功那次快照的时刻"
    );

    // 次轮 401：曾成功过 → error（不是 rejected）
    let denied = FakeFetcher::new(vec![("sk-ds", Err(failure(Some(401))))]);
    let second = snapshots_with(&cfg, None, dir.path(), &denied.fetcher()).await;
    assert_eq!(second[0].status, QuotaStatus::Error);
    assert_eq!(
        second[0].error.as_deref(),
        Some("测试取数失败（status=Some(401)）")
    );
}

#[tokio::test]
async fn rejected_when_never_succeeded_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![provider(
        "p-kimi",
        "Kimi",
        "https://api.kimi.com/coding/v1",
        &["sk-kimi"],
    )]);
    let denied = FakeFetcher::new(vec![("sk-kimi", Err(failure(Some(403))))]);
    let out = snapshots_with(&cfg, None, dir.path(), &denied.fetcher()).await;
    assert_eq!(out[0].status, QuotaStatus::Rejected);
    // 无成功、无孤儿 → 不写盘
    assert!(!state::path(dir.path()).exists());
}

#[tokio::test]
async fn error_causes_are_reported_without_leaking_keys() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "sk-secret-abcdefgh12345678";
    let cfg = config(vec![
        provider(
            "p-timeout",
            "超时家",
            "https://api.deepseek.com/v1",
            &[secret],
        ),
        provider(
            "p-5xx",
            "5xx 家",
            "https://api.kimi.com/coding/v1",
            &["sk-50000000000"],
        ),
        provider(
            "p-429",
            "限流家",
            "https://bigmodel.cn/api",
            &["sk-42900000000"],
        ),
        provider(
            "p-parse",
            "解析家",
            "https://api.z.ai/api",
            &["sk-parse0000000"],
        ),
    ]);
    let fake = FakeFetcher::new(vec![
        // 真实路径里超时/连接失败的 message 由 `fetch_json` 用本次密钥脱敏（`sanitize(e, key)`）；
        // 误试器用同一约定构造，验证错误不经快照回显密钥
        (secret, Err(failure(None))),
        ("sk-50000000000", Err(failure(Some(503)))),
        ("sk-42900000000", Err(failure(Some(429)))),
        (
            "sk-parse0000000",
            Err(FetchFailure::message(sanitize(
                &format!("Z.ai API：响应不是合法 JSON（{secret}）"),
                secret,
            ))),
        ),
    ]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;
    assert_eq!(out.len(), 4);
    for snapshot in &out {
        assert_eq!(snapshot.status, QuotaStatus::Error, "{:?}", snapshot.error);
        let error = snapshot.error.as_deref().unwrap();
        assert!(!error.contains(secret), "错误文案泄漏了密钥：{error}");
        assert_eq!(snapshot.entries.len(), 0);
        assert_eq!(snapshot.reason, None);
    }
    // 整个快照序列化结果也不得带密钥（前端悬浮排障文案的来源）
    let wire = serde_json::to_string(&out).unwrap();
    assert!(!wire.contains(secret), "快照 wire 形态泄漏了密钥");
}

// ---------- 排序与字段透出 ----------

#[tokio::test]
async fn active_provider_is_pinned_first_and_missing_id_keeps_config_order() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![
        provider("p-empty", "", "", &[]),
        provider("p-ok1", "A", "https://api.deepseek.com/v1", &["sk-a"]),
        provider("p-nokey", "B", "https://api.kimi.com/coding/v1", &[]),
        provider("p-ok2", "C", "https://bigmodel.cn/api", &["sk-c"]),
    ]);
    let fake = FakeFetcher::new(vec![
        ("sk-a", Ok(vec![percent_entry("balance_cny", 0.0)])),
        ("sk-c", Ok(vec![percent_entry("five_hour", 25.0)])),
    ]);

    // active = 第 3 行 → 置顶，其余保持配置序，unsupported 殿后
    let pinned = snapshots_with(&cfg, Some("p-ok2"), dir.path(), &fake.fetcher()).await;
    assert_eq!(ids(&pinned), vec!["p-ok2", "p-ok1", "p-nokey", "p-empty"]);

    // 未命中当前会话（或者未传）→ 原顺序
    let plain = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;
    assert_eq!(ids(&plain), vec!["p-ok1", "p-nokey", "p-ok2", "p-empty"]);
    let not_found = snapshots_with(&cfg, Some("p-nonexistent"), dir.path(), &fake.fetcher()).await;
    assert_eq!(
        ids(&not_found),
        vec!["p-ok1", "p-nokey", "p-ok2", "p-empty"]
    );
}

#[tokio::test]
async fn entries_carry_window_status_and_all_snapshot_fields_are_filled() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![provider(
        "p-go",
        "OpenCode Go",
        "https://opencode.ai/zen/go/v1",
        &["sk-go"],
    )]);
    let fake = FakeFetcher::new(vec![(
        "sk-go",
        Ok(vec![
            QuotaEntry::percent(
                "weekly",
                None,
                100.0,
                Some("2026-09-21T00:00:00Z".to_string()),
                Some("rate-limited".to_string()),
            ),
            QuotaEntry::value("balance_cny", None, "CNY 12.50".to_string()),
        ]),
    )]);
    let out = snapshots_with(&cfg, Some("p-go"), dir.path(), &fake.fetcher()).await;
    let snapshot = &out[0];
    assert_eq!(snapshot.provider_id, "p-go");
    assert_eq!(snapshot.display_name, "OpenCode Go");
    assert_eq!(snapshot.status, QuotaStatus::Ok);
    assert_eq!(snapshot.reason, None);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.key_source.as_deref(), Some("config"));
    assert!(!snapshot.fetched_at.is_empty());
    // 成功行：历史时间被刷新为本轮时刻
    assert_eq!(
        snapshot.last_ok_at.as_deref(),
        Some(snapshot.fetched_at.as_str())
    );
    // 窗口级上游 status 原样带出；数值行恒为 None
    assert_eq!(snapshot.entries[0].status.as_deref(), Some("rate-limited"));
    assert_eq!(snapshot.entries[0].remaining_percent, Some(0.0));
    assert_eq!(snapshot.entries[1].status, None);
    assert_eq!(snapshot.entries[1].value_text.as_deref(), Some("CNY 12.50"));
}

#[test]
fn quota_status_wire_names_are_snake_case() {
    let wire = |status: QuotaStatus| serde_json::to_string(&status).unwrap();
    assert_eq!(wire(QuotaStatus::Ok), "\"ok\"");
    assert_eq!(wire(QuotaStatus::Error), "\"error\"");
    assert_eq!(wire(QuotaStatus::Invalid), "\"invalid\"");
    assert_eq!(wire(QuotaStatus::Rejected), "\"rejected\"");
    assert_eq!(wire(QuotaStatus::NoKey), "\"no_key\"");
    assert_eq!(wire(QuotaStatus::Unsupported), "\"unsupported\"");
}

// ---------- last_ok_at 透出（上次成功时间） ----------

#[tokio::test]
async fn ok_row_carries_this_round_success_time_not_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    // 预置一条旧记录：ok 行必须刷新成本轮时刻，不能回显旧值
    let stale = "2026-01-01T00:00:00Z";
    let mut seeded = QuotaState::default();
    seeded.record_ok("p-ds", stale);
    assert!(seeded.save(dir.path()));

    let cfg = config(vec![provider(
        "p-ds",
        "DeepSeek",
        "https://api.deepseek.com/v1",
        &["sk-ds"],
    )]);
    let fake = FakeFetcher::new(vec![("sk-ds", Ok(vec![percent_entry("balance_cny", 0.0)]))]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;

    assert_eq!(out[0].status, QuotaStatus::Ok);
    assert_eq!(
        out[0].last_ok_at.as_deref(),
        Some(out[0].fetched_at.as_str()),
        "ok 行带的是本次成功时间"
    );
    assert_ne!(
        out[0].last_ok_at.as_deref(),
        Some(stale),
        "ok 行不得回显上一次的时间"
    );
    // 落盘被同一批刷新覆盖
    assert_eq!(
        QuotaState::load(dir.path()).verified[0].last_ok_at,
        out[0].fetched_at
    );
    // wire 上确实带着这个键（前端不再自建内存 Map 顶替）
    let wire = serde_json::to_value(&out[0]).unwrap();
    assert_eq!(
        wire.get("last_ok_at").and_then(|v| v.as_str()),
        Some(out[0].fetched_at.as_str())
    );
}

#[tokio::test]
async fn denied_row_after_a_success_carries_historical_last_ok_at() {
    let dir = tempfile::tempdir().unwrap();
    let stale = "2026-02-02T00:00:00Z";
    let mut seeded = QuotaState::default();
    seeded.record_ok("p-kimi", stale);
    assert!(seeded.save(dir.path()));

    let cfg = config(vec![provider(
        "p-kimi",
        "Kimi",
        "https://api.kimi.com/coding/v1",
        &["sk-kimi"],
    )]);
    let denied = FakeFetcher::new(vec![("sk-kimi", Err(failure(Some(401))))]);
    let out = snapshots_with(&cfg, None, dir.path(), &denied.fetcher()).await;

    // 分类规则是冻结的：曾成功过的 401 → error（不是 rejected），历史时间是区分点
    assert_eq!(out[0].status, QuotaStatus::Error);
    assert_eq!(
        out[0].last_ok_at.as_deref(),
        Some(stale),
        "失败行必须带出历史的上次成功时间"
    );
    assert_ne!(
        out[0].last_ok_at.as_deref(),
        Some(out[0].fetched_at.as_str())
    );
}

#[tokio::test]
async fn rows_without_history_have_no_last_ok_at() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![
        provider(
            "p-rejected",
            "Kimi",
            "https://api.kimi.com/coding/v1",
            &["sk-denied"],
        ),
        provider("p-nokey", "无 key", "https://api.deepseek.com/v1", &[]),
        provider(
            "p-gateway",
            "中转",
            "https://gateway.example.com/v1",
            &["sk-gw"],
        ),
    ]);
    let fake = FakeFetcher::new(vec![("sk-denied", Err(failure(Some(403))))]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;

    // 从未成功过的 403 → rejected；其余按各自行态（可查询类在前，unsupported 殿后）
    assert_eq!(ids(&out), vec!["p-rejected", "p-nokey", "p-gateway"]);
    assert_eq!(out[0].status, QuotaStatus::Rejected);
    assert_eq!(out[1].status, QuotaStatus::NoKey);
    assert_eq!(out[2].status, QuotaStatus::Unsupported);
    for snapshot in &out {
        assert_eq!(
            snapshot.last_ok_at, None,
            "{} 无历史记录 → last_ok_at 必须是 None",
            snapshot.provider_id
        );
    }
}

#[tokio::test]
async fn last_ok_at_is_read_back_from_disk_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![provider(
        "p-ds",
        "DeepSeek",
        "https://api.deepseek.com/v1",
        &["sk-ds"],
    )]);

    // 第一轮成功：该时刻只从这一轮开始存在
    let ok = FakeFetcher::new(vec![("sk-ds", Ok(vec![percent_entry("balance_cny", 0.0)]))]);
    let first = snapshots_with(&cfg, None, dir.path(), &ok.fetcher()).await;
    assert_eq!(
        first[0].last_ok_at.as_deref(),
        Some(first[0].fetched_at.as_str())
    );

    // 模拟重启：内存无残留，只有 quota.json；次轮 500 时必须从盘上读回上一轮的时刻
    let broken = FakeFetcher::new(vec![("sk-ds", Err(failure(Some(500))))]);
    let second = snapshots_with(&cfg, None, dir.path(), &broken.fetcher()).await;
    assert_eq!(second[0].status, QuotaStatus::Error);
    assert_eq!(
        second[0].last_ok_at.as_deref(),
        Some(first[0].fetched_at.as_str()),
        "重启后必须仍能读到同一时间（不能只活在内存里）"
    );
    assert_eq!(
        QuotaState::load(dir.path()).verified[0].last_ok_at,
        first[0].fetched_at
    );
}

#[tokio::test]
async fn historical_record_is_carried_even_for_rows_that_are_not_queried() {
    let dir = tempfile::tempdir().unwrap();
    let stale = "2026-03-03T00:00:00Z";
    let mut seeded = QuotaState::default();
    seeded.record_ok("p-gateway", stale);
    seeded.record_ok("p-nokey", stale);
    assert!(seeded.save(dir.path()));

    // 历史上有过成功（如今配成了不命中白名单的网关 / 没有 key）也不特判抹掉
    let cfg = config(vec![
        provider(
            "p-gateway",
            "中转",
            "https://gateway.example.com/v1",
            &["sk-gw"],
        ),
        provider("p-nokey", "无 key", "https://api.deepseek.com/v1", &[]),
    ]);
    let fake = FakeFetcher::new(vec![]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;

    assert_eq!(ids(&out), vec!["p-nokey", "p-gateway"]);
    assert_eq!(out[0].status, QuotaStatus::NoKey);
    assert_eq!(out[1].status, QuotaStatus::Unsupported);
    for snapshot in &out {
        assert_eq!(
            snapshot.last_ok_at.as_deref(),
            Some(stale),
            "{} 的历史记录应照实带上",
            snapshot.provider_id
        );
    }
    assert_eq!(fake.calls(), 0);
}

// ---------- quota.json 状态落盘 ----------

#[tokio::test]
async fn a_batch_writes_one_state_file_with_every_success() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![
        provider("p-a", "A", "https://api.deepseek.com/v1", &["sk-a"]),
        provider("p-b", "B", "https://api.kimi.com/coding/v1", &["sk-b"]),
    ]);
    let fake = FakeFetcher::new(vec![
        ("sk-a", Ok(vec![percent_entry("balance_cny", 0.0)])),
        ("sk-b", Ok(vec![percent_entry("usage", 10.0)])),
    ]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;
    assert!(out.iter().all(|s| s.status == QuotaStatus::Ok));

    // 一批只落一个文件：两家的成功都被同一次写盘覆盖，目录里没有残留临时文件
    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec![state::FILE_NAME.to_string()]);

    let saved = QuotaState::load(dir.path());
    assert_eq!(saved.version, state::VERSION);
    assert_eq!(saved.verified.len(), 2);
    assert!(saved.has_succeeded("p-a") && saved.has_succeeded("p-b"));
    assert_eq!(saved.verified[0].last_ok_at, out[0].fetched_at);
}

#[test]
fn state_roundtrip_records_once_per_provider_and_drops_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = QuotaState::default();
    state.record_ok("p-a", "2026-01-01T00:00:00Z");
    state.record_ok("p-a", "2026-02-02T00:00:00Z");
    state.record_ok("p-orphan", "2026-03-03T00:00:00Z");
    assert!(state.save(dir.path()));
    // 同 id 覆盖而不重复
    let loaded = QuotaState::load(dir.path());
    assert_eq!(loaded.verified.len(), 2);
    assert_eq!(loaded.verified[0].last_ok_at, "2026-02-02T00:00:00Z");

    // 清孤儿：保留 p-a，移除已删除的供应商
    let mut pruned = loaded.clone();
    assert!(pruned.retain(&["p-a".to_string()]));
    assert_eq!(pruned.verified.len(), 1);
    assert!(pruned.has_succeeded("p-a"));
    assert!(!pruned.has_succeeded("p-orphan"));
    assert!(pruned.save(dir.path()));
    assert_eq!(QuotaState::load(dir.path()), pruned);
    // 已无孤儿时再次 retain 不再报变更
    assert!(!pruned.retain(&["p-a".to_string()]));
}

#[tokio::test]
async fn orphan_entries_are_removed_on_the_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut seeded = QuotaState::default();
    seeded.record_ok("p-keep", "2026-01-01T00:00:00Z");
    seeded.record_ok("p-gone", "2026-01-01T00:00:00Z");
    assert!(seeded.save(dir.path()));

    let cfg = config(vec![provider(
        "p-keep",
        "Keep",
        "https://api.deepseek.com/v1",
        &["sk-keep"],
    )]);
    // p-keep 本轮 403：因为曾成功过 → error（证明读盘后的历史仍生效）；p-gone 成孤儿并被清
    let fake = FakeFetcher::new(vec![("sk-keep", Err(failure(Some(403))))]);
    let out = snapshots_with(&cfg, None, dir.path(), &fake.fetcher()).await;
    assert_eq!(out[0].status, QuotaStatus::Error);

    let saved = QuotaState::load(dir.path());
    assert!(saved.has_succeeded("p-keep"));
    assert!(!saved.has_succeeded("p-gone"));
}

#[test]
fn quota_state_load_never_panics_on_broken_or_partial_files() {
    let dir = tempfile::tempdir().unwrap();
    // 文件不存在 → 默认值
    assert_eq!(QuotaState::load(dir.path()), QuotaState::default());
    // 内容损坏 → 默认值（不 panic、不抛错）
    std::fs::write(state::path(dir.path()), b"{ not json").unwrap();
    assert_eq!(QuotaState::load(dir.path()), QuotaState::default());
    // 旧文件缺字段（无 version、无 last_ok_at）→ serde default 向前兼容
    std::fs::write(
        state::path(dir.path()),
        br#"{"verified":[{"provider_id":"p-legacy"}]}"#,
    )
    .unwrap();
    let loaded = QuotaState::load(dir.path());
    assert_eq!(loaded.version, state::VERSION);
    assert!(loaded.has_succeeded("p-legacy"));
    assert_eq!(loaded.verified[0].last_ok_at, "");
}

// ---------- quota.json 启动期自愈（`state::heal`） ----------

/// 读回文件字节（隔离/保留都要逐字节保真）。
fn bytes_of(file: &std::path::Path) -> Vec<u8> {
    std::fs::read(file).expect("读文件")
}

/// 目录条目名（排序后）：用来断言「不留临时文件 / 不留 .bak / 空目录仍为空」。
fn entries_of(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("读目录")
        .map(|e| {
            e.expect("目录条目")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// 文件 mtime（AC-6）：字节相同区分不了「完全没写」与「写回了同样的内容」，mtime 能。
fn modified_of(file: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(file)
        .expect("读元数据")
        .modified()
        .expect("读 mtime")
}

fn record(provider_id: &str, last_ok_at: &str) -> state::VerifiedProvider {
    state::VerifiedProvider {
        provider_id: provider_id.to_string(),
        last_ok_at: last_ok_at.to_string(),
    }
}

/// 核心不变式（本次修复的要害）：**`heal` 返回后，文件要么不存在（读不到 / 已被隔离留证），
/// 要么 [`QuotaState::load`] 成功、且读出的状态与 `heal` 保留的状态完全一致**——
/// `heal` 绝不留下「自己判健康、`load` 却读不了」的文件。
///
/// 为什么必须这样：`heal` 的分层判定（逐元素宽松、`version` 不参与结构解析）与 `load` 的严格语义
/// 不在一条线上。若写盘判据只看「有没有丢记录」，下面两类形态就会被判健康并原样留在盘上：
/// - `{"version":"abc","verified":[<合法记录>]}`：`version` 收不进 `u32` → `load` 整文件回退默认；
/// - `{"version":1,"verified":null}`：显式 `null` ≠ 字段缺失，`#[serde(default)]` 不生效 → 同样整文件回退。
///
/// 后果不是好看不好看：这两类文件每次启动都告警、且「曾成功过」整批失效 → 那些家下次 401/403/404
/// 被渲染成「查询被拒（可能非订阅账号）」的误导性灰行（正是本批要消灭的结论），而自愈反复跑也修不好
/// （它自己认为没事）。用例按形态遍历，并区分三类：
/// - `untouched`：本来就 load 得回同一状态 → 必须**一字不写**（不 churn、连 mtime 都不动）；
/// - 需修：写完必须 load 得回来、且等于期望状态；
/// - `expected == None`：文件级形态不识别 → 必须**隔离留证**（不得静默删除）。
#[test]
fn heal_always_leaves_a_loadable_file_or_none_at_all() {
    let legal = record("p-a", "2026-01-01T00:00:00Z");
    let ok_records = json!([{"provider_id": "p-a", "last_ok_at": "2026-01-01T00:00:00Z"}]);
    let with_version =
        |version: serde_json::Value| json!({"version": version, "verified": ok_records.clone()});
    let one_legal = || QuotaState {
        version: state::VERSION,
        verified: vec![legal.clone()],
    };
    let none_legal = || QuotaState {
        version: state::VERSION,
        verified: Vec::new(),
    };

    struct Case {
        label: &'static str,
        body: Vec<u8>,
        /// 自愈后该读到的状态；`None` = 期望「文件不存在（已被隔离留证）」
        expected: Option<QuotaState>,
        /// 期望「一字未写」（字节 + mtime 都不变）
        untouched: bool,
    }
    fn case(
        label: &'static str,
        body: serde_json::Value,
        expected: Option<QuotaState>,
        untouched: bool,
    ) -> Case {
        Case {
            label,
            body: serde_json::to_vec(&body).unwrap(),
            expected,
            untouched,
        }
    }

    let cases = vec![
        // ---- 本来就 load 得回来：必须一字不写（含「版本号本身不触发写盘」）----
        case(
            "健康（v1 + 合法记录）",
            with_version(json!(1)),
            Some(one_legal()),
            true,
        ),
        case(
            "version 缺失",
            json!({"verified": ok_records.clone()}),
            Some(one_legal()),
            true,
        ),
        case(
            "version = 0（合法 u32：原样保留、不迁移）",
            with_version(json!(0)),
            Some(QuotaState {
                version: 0,
                verified: vec![legal.clone()],
            }),
            true,
        ),
        case(
            "verified 缺失",
            json!({"version": 1}),
            Some(none_legal()),
            true,
        ),
        case(
            "verified = []",
            json!({"version": 1, "verified": []}),
            Some(none_legal()),
            true,
        ),
        case("空对象 {}", json!({}), Some(none_legal()), true),
        // ---- load 读不回来：必须修成可读形态（缺口两类 + 同族的版本写法）----
        case(
            "version 为字符串 \"abc\"",
            with_version(json!("abc")),
            Some(one_legal()),
            false,
        ),
        case(
            "version 为 null",
            with_version(json!(null)),
            Some(one_legal()),
            false,
        ),
        case(
            "version 为数字字符串 \"1\"",
            with_version(json!("1")),
            Some(one_legal()),
            false,
        ),
        case(
            "version 为浮点 1.0",
            with_version(json!(1.0)),
            Some(one_legal()),
            false,
        ),
        case(
            "version 为负数 -1",
            with_version(json!(-1)),
            Some(one_legal()),
            false,
        ),
        case(
            "verified 为 null",
            json!({"version": 1, "verified": null}),
            Some(none_legal()),
            false,
        ),
        // ---- 元素级坏元素：丢坏元素、保住合法记录 ----
        case(
            "verified = [合法, 空白 provider_id]",
            json!({"version": 1, "verified": [
                {"provider_id": "p-a", "last_ok_at": "2026-01-01T00:00:00Z"},
                {"provider_id": "   ", "last_ok_at": "2026-01-01T00:00:00Z"}
            ]}),
            Some(one_legal()),
            false,
        ),
        case(
            "verified = [合法, null 元素]",
            json!({"version": 1, "verified": [
                {"provider_id": "p-a", "last_ok_at": "2026-01-01T00:00:00Z"},
                null
            ]}),
            Some(one_legal()),
            false,
        ),
        // ---- 文件级形态不识别：隔离留证（`expected == None`）----
        case("顶层数组 []", json!([]), None, false),
        case("verified 非数组", json!({"verified": 5}), None, false),
    ];

    for Case {
        label,
        body,
        expected,
        untouched,
    } in cases
    {
        let dir = tempfile::tempdir().unwrap();
        let file = state::path(dir.path());
        std::fs::write(&file, &body).unwrap();
        let before_mtime = modified_of(&file);

        let outcome = state::heal(dir.path());
        assert!(
            !outcome.write_failed,
            "{label}：这条路径上写盘不该失败：{outcome:?}"
        );

        match &expected {
            Some(expected) => {
                assert!(
                    file.exists(),
                    "{label}：该保留的文件却被移走了：{outcome:?}"
                );
                let bytes = bytes_of(&file);
                assert!(
                    serde_json::from_slice::<QuotaState>(&bytes).is_ok(),
                    "{label}：自愈留下了 load 读不了的文件（正是本次修复的缺口）"
                );
                assert_eq!(
                    &QuotaState::load(dir.path()),
                    expected,
                    "{label}：load 结果与自愈保留的状态不一致"
                );
                assert!(
                    !state::backup_path(dir.path()).exists(),
                    "{label}：形态可用的文件不得产生 .corrupt"
                );
                if untouched {
                    assert_eq!(bytes, body, "{label}：健康形态不该写盘");
                    assert_eq!(
                        modified_of(&file),
                        before_mtime,
                        "{label}：健康形态连 mtime 都不该变"
                    );
                    assert_eq!(
                        outcome,
                        state::HealOutcome::default(),
                        "{label}：{outcome:?}"
                    );
                } else {
                    assert!(
                        outcome.dropped > 0 || outcome.repaired_only,
                        "{label}：确实写了盘，结果里却没体现：{outcome:?}"
                    );
                }
            }
            None => {
                assert!(!file.exists(), "{label}：形态不识别的文件不得留在原位");
                assert!(
                    state::backup_path(dir.path()).exists(),
                    "{label}：文件既不在原位、也没被隔离——不许静默删除"
                );
                assert_eq!(
                    bytes_of(&state::backup_path(dir.path())),
                    body,
                    "{label}：隔离内容须逐字节保真"
                );
                assert!(outcome.quarantined, "{label}：{outcome:?}");
            }
        }
    }
}

/// AC-1/AC-2：损坏文件被移动隔离且逐字节保真、**不重建**；旧备份可覆盖。
#[test]
fn heal_quarantines_a_corrupt_file_and_keeps_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let broken = b"{ not json".to_vec();
    std::fs::write(state::path(dir.path()), &broken).unwrap();

    let outcome = state::heal(dir.path());
    assert!(outcome.quarantined, "损坏文件应被隔离：{outcome:?}");
    assert_eq!(outcome.dropped, 0);
    assert!(
        !outcome.skipped_future && !outcome.write_failed,
        "{outcome:?}"
    );

    // 原文件被移走且**不重建**（维持「无记录 = 文件不存在」的既有不变式）
    assert!(!state::path(dir.path()).exists(), "quota.json 不应被重建");
    assert_eq!(
        bytes_of(&state::backup_path(dir.path())),
        broken,
        "隔离文件内容须与原文件逐字节相同"
    );
    assert_eq!(
        entries_of(dir.path()),
        vec![format!("{}{}", state::FILE_NAME, state::BACKUP_SUFFIX)],
        "目录里只该多出隔离文件"
    );

    // 加载仍是默认值（自愈不改变 load 的语义）
    let loaded = QuotaState::load(dir.path());
    assert_eq!(loaded.version, state::VERSION);
    assert!(loaded.verified.is_empty());

    // 再损坏一次：旧备份被覆盖为新内容（备份可覆盖、只作证据）
    std::fs::write(state::backup_path(dir.path()), b"stale").unwrap();
    let broken_again = b"{ also not json".to_vec();
    std::fs::write(state::path(dir.path()), &broken_again).unwrap();
    assert!(state::heal(dir.path()).quarantined);
    assert_eq!(
        bytes_of(&state::backup_path(dir.path())),
        broken_again,
        "旧备份应被新内容覆盖"
    );
}

/// AC-1：形态不识别（空文件 / 顶层 null / 顶层数组 / `verified` 类型错 / 截断 / 非 UTF-8）
/// 一律隔离，且都不重建。
#[test]
fn heal_quarantines_unrecognised_shapes() {
    let shapes: Vec<(&str, Vec<u8>)> = vec![
        ("0 字节", Vec::new()),
        ("顶层 null", b"null".to_vec()),
        ("顶层数组", b"[]".to_vec()),
        ("顶层数字", b"5".to_vec()),
        ("verified 类型错（数字）", br#"{"verified": 5}"#.to_vec()),
        (
            "verified 类型错（字符串）",
            br#"{"verified": "x"}"#.to_vec(),
        ),
        ("截断的 JSON", br#"{"version": 1, "verified": ["#.to_vec()),
        ("非 UTF-8", vec![0x80, 0xff, 0xfe]),
    ];
    for (label, body) in shapes {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(state::path(dir.path()), &body).unwrap();

        let outcome = state::heal(dir.path());
        assert!(outcome.quarantined, "{label} 应被隔离：{outcome:?}");
        assert!(
            !state::path(dir.path()).exists(),
            "{label}：quota.json 不应被重建"
        );
        assert_eq!(
            bytes_of(&state::backup_path(dir.path())),
            body,
            "{label}：隔离内容须逐字节相同"
        );
    }
}

/// AC-3/AC-4/AC-5：只删「必然不可用」的记录；三种合法时间戳形态（Z / 偏移 / 小数秒）全保留。
#[test]
fn heal_drops_only_records_that_cannot_be_used() {
    let dir = tempfile::tempdir().unwrap();
    let keep = vec![
        record("p-z", "2026-01-01T00:00:00Z"),
        record("p-offset", "2026-02-02T08:00:00+08:00"),
        record("p-frac", "2026-03-03T00:00:00.123Z"),
    ];
    let mut all = keep.clone();
    all.extend([
        record("p-empty", ""),
        record("p-junk", "not-a-time"),
        record("   ", "2026-04-04T00:00:00Z"),
    ]);
    let seed = QuotaState {
        version: state::VERSION,
        verified: all,
    };
    assert!(seed.save(dir.path()));
    let before = bytes_of(&state::path(dir.path()));

    let outcome = state::heal(dir.path());
    assert_eq!(outcome.dropped, 3, "只应清掉必然不可用的 3 条：{outcome:?}");
    assert!(
        !outcome.quarantined && !outcome.skipped_future && !outcome.write_failed,
        "可解析文件不该被隔离、写盘应成功：{outcome:?}"
    );
    assert!(
        !state::backup_path(dir.path()).exists(),
        "能解析的文件不该产生 .corrupt"
    );

    // 合法记录逐字保留（顺序 + 字段值都不变），文件被重写且仍可解析、无残留
    let healed = QuotaState::load(dir.path());
    assert_eq!(healed.version, state::VERSION);
    assert_eq!(healed.verified, keep, "合法记录必须逐字保留");
    assert_ne!(
        bytes_of(&state::path(dir.path())),
        before,
        "有记录被清时应重写文件"
    );
    assert_eq!(
        entries_of(dir.path()),
        vec![state::FILE_NAME.to_string()],
        "不该留临时文件或 .bak"
    );
}

/// AC-6/AC-8：健康文件一字不写，且连续两次自愈第二次仍是 no-op。
#[test]
fn heal_leaves_a_healthy_file_untouched_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut healthy = QuotaState::default();
    healthy.record_ok("p-a", "2026-01-01T00:00:00Z");
    healthy.record_ok("p-b", "2026-02-02T08:00:00+08:00");
    assert!(healthy.save(dir.path()));
    let before = bytes_of(&state::path(dir.path()));
    let before_mtime = modified_of(&state::path(dir.path()));

    assert_eq!(
        state::heal(dir.path()),
        state::HealOutcome::default(),
        "健康文件应 no-op"
    );
    assert_eq!(bytes_of(&state::path(dir.path())), before, "字节不变");
    // AC-6：mtime 不变才证明「完全没写盘」，而不是「写回了同样的内容」
    assert_eq!(
        modified_of(&state::path(dir.path())),
        before_mtime,
        "健康文件连 mtime 都不该变"
    );
    assert!(!state::backup_path(dir.path()).exists());

    // 幂等：第二次仍是 no-op
    assert_eq!(state::heal(dir.path()), state::HealOutcome::default());
    assert_eq!(bytes_of(&state::path(dir.path())), before, "字节仍不变");
    assert_eq!(
        modified_of(&state::path(dir.path())),
        before_mtime,
        "第二次自愈后 mtime 仍不变"
    );
}

/// AC-7：文件不存在时不创建（空目录仍为空）。
#[test]
fn heal_does_not_create_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(state::heal(dir.path()), state::HealOutcome::default());
    assert!(
        entries_of(dir.path()).is_empty(),
        "空目录应仍为空（不凭空创建 quota.json）"
    );
}

/// AC-13：`version` 高于当前 schema 的文件完全不碰（降级运行保护）。
#[test]
fn heal_skips_files_written_by_a_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let body = serde_json::to_vec(&json!({
        "version": state::VERSION + 1,
        "verified": [{"provider_id": "p-a", "last_ok_at": "2026-01-01T00:00:00Z"}],
        "unknown_future_field": {"nested": true},
    }))
    .unwrap();
    std::fs::write(state::path(dir.path()), &body).unwrap();

    let outcome = state::heal(dir.path());
    assert!(outcome.skipped_future, "{outcome:?}");
    assert!(
        !outcome.quarantined && outcome.dropped == 0 && !outcome.write_failed,
        "未来的 schema 只该被跳过：{outcome:?}"
    );
    assert_eq!(bytes_of(&state::path(dir.path())), body, "文件须一字未动");
    assert!(!state::backup_path(dir.path()).exists(), "不得隔离未来版本");
}

/// AC-9：写盘失败（只读目录）只上报不 panic，原文件保留。
#[cfg(unix)]
#[test]
fn heal_reports_a_write_failure_without_panicking() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut dirty = QuotaState::default();
    dirty.record_ok("p-keep", "2026-01-01T00:00:00Z");
    dirty.record_ok("p-empty", "");
    assert!(dirty.save(dir.path()));

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let outcome = state::heal(dir.path());
    // 先恢复权限，否则 TempDir 清理会失败（并影响同进程其它用例）
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(outcome.write_failed, "只读目录应上报写失败：{outcome:?}");
    assert_eq!(outcome.dropped, 1);
    assert!(
        !outcome.quarantined && !outcome.skipped_future,
        "{outcome:?}"
    );
    // 行为退回自愈前：原文件保留（仍含那条待清记录），load 照旧可用
    let loaded = QuotaState::load(dir.path());
    assert!(loaded.has_succeeded("p-keep"));
    assert_eq!(loaded.verified.len(), 2, "写失败时原文件不动");
}

/// AC-9 变体：`dir` 指向普通文件（不是目录）→ 静默 no-op、不 panic、不碰该文件。
#[test]
fn heal_never_panics_when_the_directory_path_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();

    assert_eq!(state::heal(&file), state::HealOutcome::default());
    assert_eq!(bytes_of(&file), b"x".to_vec(), "该文件不该被动");
}

/// AC-1 变体：**缺** `verified` 属「空数组」（`#[serde(default)]` 同语义），不是形态错——
/// 隔离它只会把「无记录」误报成「文件损坏」，反而误导排障；`load` 也读得回来，故连写盘都不该有。
#[test]
fn heal_treats_missing_verified_as_an_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let body = br#"{"version":1}"#;
    std::fs::write(state::path(dir.path()), body).unwrap();
    let before_mtime = modified_of(&state::path(dir.path()));

    let outcome = state::heal(dir.path());
    assert_eq!(
        outcome,
        state::HealOutcome::default(),
        "缺 verified 应 no-op（无记录可清、不隔离）：{outcome:?}"
    );
    assert_eq!(bytes_of(&state::path(dir.path())), body, "字节不变");
    assert_eq!(
        modified_of(&state::path(dir.path())),
        before_mtime,
        "未写盘"
    );
    assert!(!state::backup_path(dir.path()).exists(), "不得隔离");
    assert_eq!(
        QuotaState::load(dir.path()),
        QuotaState {
            version: state::VERSION,
            verified: Vec::new()
        },
        "load 该读回「无记录」"
    );
}

/// 本次修复的缺口之一：`verified: null` 在**值上**同样是「没有记录」，但显式 `null` ≠ 字段缺失，
/// `#[serde(default)]` 不生效 → `load` 会**整文件**回退默认。旧行为把 `null` 当空数组就完事，
/// 于是这种文件每次启动反复告警，且（若同文件还有别的记录）那些家的「曾成功过」全部失效
/// → 下次 401/403/404 被渲染成「查询被拒」的误导性灰行，正是本批要消灭的结论。
/// 修法是写成 `[]`：不动任何记录，只让文件重新可读。
#[test]
fn heal_repairs_explicit_null_verified_into_an_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let body = br#"{"version":1,"verified":null}"#;
    assert!(
        serde_json::from_slice::<QuotaState>(body).is_err(),
        "前提：显式 null 在 load 侧是整文件失败（不是「空数组」）"
    );
    std::fs::write(state::path(dir.path()), body).unwrap();

    let outcome = state::heal(dir.path());
    assert_eq!(
        outcome,
        state::HealOutcome {
            repaired_only: true,
            ..state::HealOutcome::default()
        },
        "只该修可读性，不删记录：{outcome:?}"
    );
    assert_eq!(outcome.dropped, 0, "没有记录可删");
    assert!(
        !state::backup_path(dir.path()).exists(),
        "不是形态错，不得隔离"
    );
    assert_eq!(
        QuotaState::load(dir.path()),
        QuotaState {
            version: state::VERSION,
            verified: Vec::new()
        },
        "修完必须 load 得回来且是「无记录」"
    );

    // 幂等：修完即转 no-op
    let after = bytes_of(&state::path(dir.path()));
    let after_mtime = modified_of(&state::path(dir.path()));
    assert_eq!(state::heal(dir.path()), state::HealOutcome::default());
    assert_eq!(bytes_of(&state::path(dir.path())), after, "字节不变");
    assert_eq!(
        modified_of(&state::path(dir.path())),
        after_mtime,
        "mtime 不变"
    );
}

/// 本批的核心边界（R1）：**元素级类型错不得把整文件判死**。
///
/// 手改出的 `"last_ok_at": null` / `"provider_id": 123` / 元素不是对象，只能丢这些元素；
/// 同一文件里合法记录的「曾成功过」事实必须保住（否则那些家下次 401/403/404 会被渲染成
/// 「查询被拒（可能非订阅账号）」，正是要消灭的误导性结论）。
#[test]
fn heal_keeps_valid_records_when_some_elements_are_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let body = serde_json::to_vec(&json!({
        "version": state::VERSION,
        "verified": [
            {"provider_id": "p-keep", "last_ok_at": "2026-01-01T00:00:00Z"},
            {"provider_id": "p-null", "last_ok_at": null},
            {"provider_id": 123, "last_ok_at": "2026-02-02T00:00:00Z"},
            "not-an-object",
            {"provider_id": "p-keep2", "last_ok_at": "2026-02-02T08:00:00+08:00"}
        ]
    }))
    .unwrap();
    std::fs::write(state::path(dir.path()), &body).unwrap();

    let outcome = state::heal(dir.path());
    assert!(
        !outcome.quarantined,
        "元素级类型错不该隔离整文件：{outcome:?}"
    );
    assert_eq!(outcome.dropped, 3, "只该丢三个坏元素：{outcome:?}");
    assert!(
        !outcome.skipped_future && !outcome.write_failed,
        "{outcome:?}"
    );
    assert!(
        !state::backup_path(dir.path()).exists(),
        "能逐元素读出来的文件不得产生 .corrupt"
    );

    // 合法记录逐字保留（顺序 + 字段值），且写回的文件能被严格解析（否则下次启动又退回默认）
    let healed = QuotaState::load(dir.path());
    assert_eq!(healed.version, state::VERSION);
    assert_eq!(
        healed.verified,
        vec![
            record("p-keep", "2026-01-01T00:00:00Z"),
            record("p-keep2", "2026-02-02T08:00:00+08:00"),
        ],
        "合法记录的「曾成功过」事实必须保住"
    );
    assert!(healed.has_succeeded("p-keep") && healed.has_succeeded("p-keep2"));
    assert_eq!(
        entries_of(dir.path()),
        vec![state::FILE_NAME.to_string()],
        "不该留临时文件或 .bak"
    );
}

/// AC-13：`version` 的**数值形态**（浮点 / 数字字符串 / 超 `u64` 大整数）也要判为未来版本。
///
/// 漏判的后果不是好看不好看：未来版本可能以 `2.0` 这类形式落盘，隔离它就等于在降级运行时
/// 挪走新版数据。
#[test]
fn heal_skips_numeric_versions_beyond_the_current_schema() {
    let versions: Vec<(&str, serde_json::Value)> = vec![
        ("整数", json!(state::VERSION + 1)),
        ("浮点 2.0", json!(2.0)),
        ("浮点 2.5", json!(2.5)),
        ("数字字符串", json!("2")),
        (
            "超 u64 大整数",
            serde_json::from_str("99999999999999999999").unwrap(),
        ),
    ];
    for (label, version) in versions {
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::to_vec(&json!({
            "version": version,
            "verified": [{"provider_id": "p-a", "last_ok_at": "2026-01-01T00:00:00Z"}],
            "unknown_future_field": {"nested": true},
        }))
        .unwrap();
        std::fs::write(state::path(dir.path()), &body).unwrap();

        let outcome = state::heal(dir.path());
        assert!(outcome.skipped_future, "{label} 应判未来版本：{outcome:?}");
        assert!(
            !outcome.quarantined && outcome.dropped == 0 && !outcome.write_failed,
            "{label} 只该被跳过：{outcome:?}"
        );
        assert_eq!(
            bytes_of(&state::path(dir.path())),
            body,
            "{label}：文件须一字未动"
        );
        assert!(
            !state::backup_path(dir.path()).exists(),
            "{label}：不得隔离未来版本"
        );
    }
}

/// AC-14：版本号本身不触发写盘——**合法 `u32` 且 ≤ [`state::VERSION`]** 的写法（缺失 / `0` / `1`）
/// 一律不迁移、不重写、不隔离。这三种形态在 `load` 侧都读得回来（`0` 就是要被原样保留的旧版本号），
/// 所以「版本号与当前不同」本身不构成写盘理由。
#[test]
fn heal_does_not_write_for_the_version_field_alone() {
    let cases: Vec<(&str, &[u8], u32)> = vec![
        (
            "version 缺失",
            br#"{"verified":[{"provider_id":"p-a","last_ok_at":"2026-01-01T00:00:00Z"}]}"#,
            state::VERSION,
        ),
        (
            "version = 0（合法 u32，原样保留）",
            br#"{"version":0,"verified":[{"provider_id":"p-a","last_ok_at":"2026-01-01T00:00:00Z"}]}"#,
            0,
        ),
        (
            "version = 1（当前版本）",
            br#"{"version":1,"verified":[{"provider_id":"p-a","last_ok_at":"2026-01-01T00:00:00Z"}]}"#,
            state::VERSION,
        ),
    ];
    for (label, body, expected_version) in cases {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(state::path(dir.path()), body).unwrap();
        let before_mtime = modified_of(&state::path(dir.path()));

        assert_eq!(
            state::heal(dir.path()),
            state::HealOutcome::default(),
            "{label}：版本号不该触发写盘"
        );
        assert_eq!(
            bytes_of(&state::path(dir.path())),
            body,
            "{label}：字节不变"
        );
        assert_eq!(
            modified_of(&state::path(dir.path())),
            before_mtime,
            "{label}：mtime 不变"
        );
        assert!(
            !state::backup_path(dir.path()).exists(),
            "{label}：不得隔离"
        );
        assert_eq!(
            QuotaState::load(dir.path()).version,
            expected_version,
            "{label}：load 该读回盘上的版本号（`0` 不迁移、不被 canonical 成当前版本）"
        );
    }
}

/// 与上一条对照：`version` 读不成 `u32` 的写法**必须被修好**——`version` 不参与结构解析
/// （宽松侧看不出问题），但 `load` 收不进 `u32` → **整文件**回退默认：「曾成功过」整批失效
/// → 那些家下次 401/403/404 被渲染成「查询被拒（可能非订阅账号）」的误导性灰行，
/// 且每次启动都告警、永不收敛。修法是重写成可读形态（`version` 落当前值），**不删任何记录**，
/// 修完再自愈即转 no-op（不 churn）。
#[test]
fn heal_repairs_versions_that_load_cannot_read() {
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("非数值字符串", json!("abc")),
        ("null", json!(null)),
        ("数字字符串", json!("1")),
        ("浮点 1.0", json!(1.0)),
        ("负数 -1", json!(-1)),
    ];
    for (label, version) in cases {
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::to_vec(&json!({
            "version": version,
            "verified": [{"provider_id": "p-a", "last_ok_at": "2026-01-01T00:00:00Z"}],
        }))
        .unwrap();
        // 前提：这类写法确实 load 读不回来（否则本用例就测不到缺口）
        assert!(
            serde_json::from_slice::<QuotaState>(&body).is_err(),
            "fixture 需重挑：version={label} 本应 load 失败"
        );
        std::fs::write(state::path(dir.path()), &body).unwrap();

        let outcome = state::heal(dir.path());
        assert_eq!(
            outcome,
            state::HealOutcome {
                repaired_only: true,
                ..state::HealOutcome::default()
            },
            "{label}：应只做「修可读性」的写盘：{outcome:?}"
        );
        assert_eq!(outcome.dropped, 0, "{label}：一条记录都不许删");
        assert!(
            !state::backup_path(dir.path()).exists(),
            "{label}：形态可用只是 load 读不回，不该隔离"
        );

        let healed = QuotaState::load(dir.path());
        assert_eq!(
            healed.version,
            state::VERSION,
            "{label}：写回统一落当前版本"
        );
        assert_eq!(
            healed.verified,
            vec![record("p-a", "2026-01-01T00:00:00Z")],
            "{label}：合法记录的「曾成功过」事实必须保住"
        );

        // 幂等：修完即转 no-op（字节 + mtime 都不动）
        let after = bytes_of(&state::path(dir.path()));
        let after_mtime = modified_of(&state::path(dir.path()));
        assert_eq!(
            state::heal(dir.path()),
            state::HealOutcome::default(),
            "{label}：修完应转 no-op"
        );
        assert_eq!(
            bytes_of(&state::path(dir.path())),
            after,
            "{label}：字节不变"
        );
        assert_eq!(
            modified_of(&state::path(dir.path())),
            after_mtime,
            "{label}：mtime 不变"
        );
    }
}

/// AC-14 补：当前版本号的非整数写法（`1.0` / `"1"`）不算未来版本，照常走清洗流程
/// （有坏记录就写回，并统一落当前整数 `VERSION`）。
#[test]
fn heal_accepts_numeric_current_version_and_sanitizes() {
    for (label, version) in [("浮点 1.0", json!(1.0)), ("数字字符串 1", json!("1"))] {
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::to_vec(&json!({
            "version": version,
            "verified": [
                {"provider_id": "p-keep", "last_ok_at": "2026-01-01T00:00:00Z"},
                {"provider_id": "p-junk", "last_ok_at": ""}
            ]
        }))
        .unwrap();
        std::fs::write(state::path(dir.path()), &body).unwrap();

        let outcome = state::heal(dir.path());
        assert!(
            !outcome.skipped_future && !outcome.quarantined && !outcome.write_failed,
            "{label}：当前版本应走清洗流程：{outcome:?}"
        );
        assert_eq!(outcome.dropped, 1, "{label}");
        let healed = QuotaState::load(dir.path());
        assert_eq!(
            healed.version,
            state::VERSION,
            "{label}：写回统一落当前版本"
        );
        assert_eq!(
            healed.verified,
            vec![record("p-keep", "2026-01-01T00:00:00Z")]
        );
    }
}

/// AC-10 日志卫生：摘要只描述形态（类别 / 行列号），**不回显文件内容**。
///
/// serde 的 `Error::to_string()` 对 `Unexpected::Str` 会打印**完整字符串不截断**，
/// 于是 `{"verified": "<很长内容>"}` 这类文件会把整段内容写进 warn，而日志常被用户分享。
#[test]
fn quarantine_and_load_summaries_never_echo_file_content() {
    let long = "x".repeat(200);

    // 1) 形态错（verified 是长字符串）：摘要只描述形态
    let shape = format!(r#"{{"verified": "{long}"}}"#);
    let failure =
        state::parse_state(shape.as_bytes()).expect_err("verified 不是数组应判形态不识别");
    assert_eq!(failure, state::ShapeFailure::VerifiedNotAnArray);
    assert!(
        !failure.summary().contains(&long),
        "摘要回显了文件内容：{}",
        failure.summary()
    );

    // 2) 语法错（截断）：摘要只用「类别 + 行列号」
    let truncated = format!(r#"{{"version": "{long}""#);
    let failure = state::parse_state(truncated.as_bytes()).expect_err("截断应判形态不识别");
    let summary = failure.summary();
    assert!(summary.contains("第 1 行"), "摘要该带行列号：{summary}");
    assert!(!summary.contains(&long), "摘要回显了文件内容：{summary}");

    // 3) 对照：serde 自身的 to_string() 确实会回显整段内容（证明上面的防护不是空转）
    let data_body = format!(r#"{{"version": "{long}"}}"#);
    let raw = serde_json::from_slice::<QuotaState>(data_body.as_bytes()).unwrap_err();
    assert!(
        raw.to_string().contains(&long),
        "serde 的 to_string() 会回显内容（这正是要绕开的）：{raw}"
    );
    let structured = state::ShapeFailure::from_error(&raw).summary();
    assert!(structured.contains("字段类型或结构不符"), "{structured}");
    assert!(
        !structured.contains(&long),
        "摘要回显了文件内容：{structured}"
    );

    // 4) 真实路径：这类文件仍不 panic 地隔离、内容逐字节留证
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(state::path(dir.path()), shape.as_bytes()).unwrap();
    let outcome = state::heal(dir.path());
    assert!(outcome.quarantined, "{outcome:?}");
    assert!(!state::path(dir.path()).exists(), "隔离后不重建");
    assert_eq!(
        bytes_of(&state::backup_path(dir.path())),
        shape.as_bytes(),
        "隔离内容须逐字节相同"
    );
}

/// 自愈与提交并发：两者共用同一把 `lock_io()`，谁都不该把对方的记录覆盖掉。
///
/// 与 `concurrent_batches_do_not_lose_each_others_success_records` 同一风险面，但覆盖的是
/// `heal` 的「读—改—写」：自愈会重写文件（清不可用记录），若它不参与串行化，提交刚写下的
/// 成功记录就会被自愈用旧快照覆盖回去。
///
/// 预置一条「必然不可用」记录（id 合法、时间戳非法）让自愈**真的走写盘分支**；
/// 两条断言都与线程交错顺序无关（成功记录的非法时间戳只可能被能读到旧文件的提交补上）。
#[test]
fn heal_and_commit_do_not_lose_each_others_records() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    let ids = vec!["p-a".to_string(), "p-b".to_string()];
    std::fs::write(
        state::path(&dir_path),
        br#"{"version":1,"verified":[{"provider_id":"p-a","last_ok_at":"2026-01-01T00:00:00Z"},{"provider_id":"p-b","last_ok_at":"junk"}]}"#,
    )
    .unwrap();

    let healer = {
        let dir_path = dir_path.clone();
        std::thread::spawn(move || {
            for _ in 0..100 {
                let _ = state::heal(&dir_path);
            }
        })
    };
    for _ in 0..100 {
        assert!(state::commit(
            &dir_path,
            &ids,
            &["p-a".to_string()],
            "2026-01-01T00:00:00Z"
        ));
        assert!(state::commit(
            &dir_path,
            &ids,
            &["p-b".to_string()],
            "2026-02-02T00:00:00Z"
        ));
    }
    healer.join().expect("自愈线程不得 panic");

    let saved = QuotaState::load(&dir_path);
    assert!(
        saved.has_succeeded("p-a") && saved.has_succeeded("p-b"),
        "并发下丢了成功记录：{saved:?}"
    );
    assert_eq!(saved.verified.len(), 2);
    assert_eq!(
        saved.verified[0].last_ok_at, "2026-01-01T00:00:00Z",
        "合法记录的时间戳不该被自愈改写"
    );
    assert!(
        !state::backup_path(&dir_path).exists(),
        "始终形态完好的文件不该被隔离"
    );
}

// ---------- 密钥来源（按实际取用的那枚 key 判定） ----------

/// 混合池偏乐观的反例：池里出现占位符就报 `keyring`，而实际取用的是排在前面的明文
/// （`resolve_provider_keys` 把配置明文排在 keyring 回读之前）。
#[test]
fn key_source_follows_the_key_actually_used_not_placeholder_presence() {
    let base = "https://api.kimi.com/coding/v1";
    // 纯明文池：不变
    let plain = provider("p-plain", "纯明文", base, &["sk-plain"]);
    assert_eq!(key_source_of(&plain, "sk-plain"), "config");
    // 混合池（明文 + 占位符）：实际取用明文 → config，不能说成 keyring
    let mixed = provider("p-mixed", "混合", base, &["sk-plain", KEYRING_PLACEHOLDER]);
    assert_eq!(key_source_of(&mixed, "sk-plain"), "config");
    // 首项空串被过滤后取到后续 key：来源跟随实际取用者
    let leading_empty = provider("p-empty-head", "空首项", base, &["", "sk-later"]);
    assert_eq!(key_source_of(&leading_empty, "sk-later"), "config");
    // 纯占位符池（回读成功）：实际取用的是回读出来的那枚 → keyring
    let pure = provider("p-keyring", "纯钥匙串", base, &[KEYRING_PLACEHOLDER]);
    assert_eq!(key_source_of(&pure, "sk-from-keyring"), "keyring");
    // 占位符在前、明文在后：取到的仍是明文（空串/占位符被过滤，明文先入队）
    let placeholder_first = provider(
        "p-ph-first",
        "占位符在前",
        base,
        &[KEYRING_PLACEHOLDER, "sk-plain"],
    );
    assert_eq!(key_source_of(&placeholder_first, "sk-plain"), "config");
}

// ---------- 并发刷新：quota.json 的读—改—写串行化 ----------

/// 并发两批刷新的「丢失更新」复现：第二批在**第一批落盘之前**就读到了旧状态。
///
/// 真并发下必然发生（两批同时 load）；这里用 `tokio::spawn` 把时序钉死：
/// 第二批先读到空状态并挂在取数上，第一批（主任务）完整跑完并落盘，然后才放行
/// 第二批。旧实现下第二批会把自己那份旧内存状态整体写回（其中没有 p-a 的记录），
/// 覆盖掉第一批的成功；现实现第二批在批次末的临界区**重新读盘再合并**。
///
/// 盘上丢记录的后果不是好看不好看：下一轮 401/403/404 时会被当成「从未成功过」→
/// `rejected` → 灰行（额度静默消失）。
///
/// 两批共用同一份 config（生产形态：同一份供应商配置被定时器与手动刷新各跑一轮），
/// 否则清孤儿语义会把对方家的记录当作已删除供应商清掉。
#[tokio::test]
async fn concurrent_batches_do_not_lose_each_others_success_records() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(vec![
        provider("p-a", "A", "https://api.deepseek.com/v1", &["sk-a"]),
        provider("p-b", "B", "https://api.kimi.com/coding/v1", &["sk-b"]),
    ]);

    // 第二批的取数器：p-b 一路成功但进入后先挂住，制造「已 load、未 commit」的窗口；
    // p-a 取数失败（第二批不该靠自己的结果保住 p-a，只能靠临界区重新读盘）
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let slow = {
        let entered = entered.clone();
        let release = release.clone();
        move |_kind: ProviderKind, key: String| -> FetchBox {
            let entered = entered.clone();
            let release = release.clone();
            Box::pin(async move {
                if key == "sk-b" {
                    entered.notify_one();
                    release.notified().await;
                    return Ok(vec![percent_entry("balance_cny", 0.0)]);
                }
                Err(failure(Some(500)))
            })
        }
    };
    let dir_b = dir.path().to_path_buf();
    let second = tokio::spawn({
        let cfg = cfg.clone();
        async move { snapshots_with(&cfg, None, &dir_b, &slow).await }
    });

    // 第二批已读到旧状态（文件还不存在）并开始取数
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .expect("第二批取数未启动");

    // 第一批（主任务）完整跑完并落盘：p-a 成功，p-b 失败
    let fast = FakeFetcher::new(vec![
        ("sk-a", Ok(vec![percent_entry("balance_cny", 0.0)])),
        ("sk-b", Err(failure(Some(500)))),
    ]);
    let first = snapshots_with(&cfg, None, dir.path(), &fast.fetcher()).await;
    assert_eq!(first[0].status, QuotaStatus::Ok);
    assert!(QuotaState::load(dir.path()).has_succeeded("p-a"));

    // 放行第二批：它必须重新读盘再合并，而不是把旧状态写回
    release.notify_one();
    let second = tokio::time::timeout(std::time::Duration::from_secs(10), second)
        .await
        .expect("第二批未结束")
        .unwrap();
    assert!(
        second
            .iter()
            .any(|s| s.provider_id == "p-b" && s.status == QuotaStatus::Ok)
    );

    let saved = QuotaState::load(dir.path());
    assert!(
        saved.has_succeeded("p-a") && saved.has_succeeded("p-b"),
        "并发两批的成功记录都必须在盘上，实际：{saved:?}"
    );
    assert_eq!(saved.verified.len(), 2);
}

/// 同上语义的确定性版本（不依赖真并发）：提交 = 临界区内重新读盘 + 清孤儿 +
/// 记本批成功者 + 写盘一次；无成功且无孤儿则不写盘。
#[test]
fn commit_reloads_before_merging_so_other_batches_records_survive() {
    let dir = tempfile::tempdir().unwrap();
    let ids = vec!["p-a".to_string(), "p-b".to_string()];
    // 另一批（并发刷新）先记上 p-a
    assert!(state::commit(
        dir.path(),
        &ids,
        &["p-a".to_string()],
        "2026-01-01T00:00:00Z"
    ));
    // 本批成功的是 p-b：提交时必须重新读盘，不能把 p-a 覆盖掉
    assert!(state::commit(
        dir.path(),
        &ids,
        &["p-b".to_string()],
        "2026-02-02T00:00:00Z"
    ));
    let saved = QuotaState::load(dir.path());
    assert!(saved.has_succeeded("p-a") && saved.has_succeeded("p-b"));
    assert_eq!(saved.verified.len(), 2);
    assert_eq!(saved.verified[0].last_ok_at, "2026-01-01T00:00:00Z");
    assert_eq!(saved.verified[1].last_ok_at, "2026-02-02T00:00:00Z");

    // 无成功者且无孤儿 → 不写盘（目录里不出现文件）
    let empty = tempfile::tempdir().unwrap();
    assert!(!state::commit(
        empty.path(),
        &ids,
        &[],
        "2026-03-03T00:00:00Z"
    ));
    assert!(!state::path(empty.path()).exists());
}

// ---------- 7 家适配器解析 ----------

#[test]
fn opencode_go_keeps_windows_whose_status_is_not_ok() {
    // 实调样例（2026-09-20）：weekly 用满 → status "rate-limited"；
    // status 不再作过滤条件，否则「本周剩余 0%」会整窗静默消失。
    let body = json!({
        "usage": {
            "rolling": { "status": "ok", "percent": 12.5, "resetsAt": "2026-09-18T20:00:00Z" },
            "weekly": { "status": "rate-limited", "percent": 100, "resetsAt": "2026-09-21T00:00:00Z" },
            "monthly": { "status": "ok", "percent": 140.0, "resetsAt": "2026-10-01T00:00:00Z" }
        }
    });
    let entries = opencode_go::parse_usage(&body).unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["rolling", "weekly", "monthly"]);
    assert_eq!(entries[0].remaining_percent, Some(87.5));
    assert_eq!(entries[0].used_percent, Some(12.5));
    // 被限流的周窗口：剩余 0% + 重置时间照常带出，窗口级 status 原样透出
    assert_eq!(entries[1].used_percent, Some(100.0));
    assert_eq!(entries[1].remaining_percent, Some(0.0));
    assert_eq!(entries[1].status.as_deref(), Some("rate-limited"));
    assert_eq!(
        entries[1].resets_at.as_deref(),
        Some("2026-09-21T00:00:00Z")
    );
    // 正常窗口也带自己的 status
    assert_eq!(entries[0].status.as_deref(), Some("ok"));
    // 越界百分比收敛到 0-100
    assert_eq!(entries[2].remaining_percent, Some(0.0));
}

#[test]
fn opencode_go_accepts_numeric_string_percent_and_skips_unusable_windows() {
    let body = json!({
        "usage": {
            "rolling": { "status": "ok", "percent": "31" },
            "weekly": { "status": "rate-limited" },
            "monthly": { "status": "ok", "percent": null }
        }
    });
    let entries = opencode_go::parse_usage(&body).unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["rolling"]);
    assert_eq!(entries[0].used_percent, Some(31.0));
    // 没有 resetsAt 不算异常，只是没有重置倒计时
    assert_eq!(entries[0].resets_at, None);
    // 全不可用 → 明确报错，而不是返回空列表
    assert!(opencode_go::parse_usage(&json!({ "usage": {} })).is_err());
    assert!(
        opencode_go::parse_usage(&json!({ "usage": { "weekly": { "status": "rate-limited" } } }))
            .is_err()
    );
    assert!(opencode_go::parse_usage(&json!({})).is_err());
}

#[test]
fn deepseek_keeps_supported_currencies_and_marks_unavailable() {
    let body = json!({
        "is_available": false,
        "balance_infos": [
            { "currency": "CNY", "total_balance": "12.5", "granted_balance": "2.5", "topped_up_balance": "10" },
            { "currency": "EUR", "total_balance": "3" },
            { "currency": "USD", "granted_balance": "1", "topped_up_balance": "2.25" }
        ]
    });
    let entries = deepseek::parse_balance(&body).unwrap();
    let texts: Vec<&str> = entries
        .iter()
        .map(|e| e.value_text.as_deref().unwrap())
        .collect();
    assert_eq!(texts, vec!["CNY 12.50 · 不可用", "USD 3.25 · 不可用"]);
    assert_eq!(entries[0].key, "balance_cny");
    // 数值行没有窗口状态
    assert_eq!(entries[0].status, None);
    // 无可用数据 → 报错
    assert!(deepseek::parse_balance(&json!({ "balance_infos": [] })).is_err());
}

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_769_000_000, 0).unwrap()
}

#[test]
fn minimax_endpoint_semantics_differ_for_the_same_counts() {
    let payload = json!({
        "base_resp": { "status_code": 0 },
        "model_remains": [{
            "model_name": "minimax-m2",
            "remains_time": 3_600_000,
            "current_interval_total_count": 100,
            "current_interval_usage_count": 80,
            "current_weekly_total_count": 1000,
            "current_weekly_usage_count": 500,
            "weekly_remains_time": 86_400_000
        }]
    });

    // 国际：计数 = 剩余 → 80/100 = 80% 剩余
    let intl = minimax::parse_usage(&payload, minimax::Endpoint::International, now()).unwrap();
    assert_eq!(intl[0].key, "five_hour");
    assert_eq!(intl[0].remaining_percent, Some(80.0));
    assert_eq!(intl[1].key, "week");
    assert_eq!(intl[1].remaining_percent, Some(50.0));
    assert_eq!(intl[0].resets_at.as_deref(), Some("2026-01-21T13:53:20Z"));

    // 中国：计数 = 已用 → 20% 剩余（语义相反，防误改）
    let cn = minimax::parse_usage(&payload, minimax::Endpoint::China, now()).unwrap();
    assert_eq!(cn[0].remaining_percent, Some(20.0));
}

#[test]
fn minimax_prefers_wildcard_model_and_reports_api_errors() {
    let payload = json!({
        "base_resp": { "status_code": 0 },
        "model_remains": [
            { "model_name": "minimax-m2", "remains_time": 1000, "current_interval_total_count": 100, "current_interval_usage_count": 10 },
            { "model_name": "minimax-m*", "remains_time": 1000, "current_interval_total_count": 100, "current_interval_usage_count": 90 }
        ]
    });
    let entries = minimax::parse_usage(&payload, minimax::Endpoint::International, now()).unwrap();
    assert_eq!(entries[0].remaining_percent, Some(90.0));

    let err = minimax::parse_usage(
        &json!({ "base_resp": { "status_code": 1004, "status_msg": "invalid api key" } }),
        minimax::Endpoint::International,
        now(),
    )
    .unwrap_err();
    assert!(err.contains("invalid api key"), "{err}");
}

#[test]
fn minimax_keeps_models_that_only_expose_the_weekly_reset() {
    // 中国端点可能只回 weekly_remains_time；硬卡 remains_time 会把整家模型滤光 → 误报无数据
    let payload = json!({
        "base_resp": { "status_code": 0 },
        "model_remains": [{
            "model_name": "minimax-m2",
            "weekly_remains_time": 3_600_000,
            "current_weekly_total_count": 100,
            "current_weekly_usage_count": 80
        }]
    });
    let entries = minimax::parse_usage(&payload, minimax::Endpoint::China, now()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "week");
    // 中国端点计数 = 已用 → 80/100 已用 = 20% 剩余
    assert_eq!(entries[0].remaining_percent, Some(20.0));
    assert_eq!(
        entries[0].resets_at.as_deref(),
        Some("2026-01-21T13:53:20Z")
    );
}

#[test]
fn glm_maps_units_to_windows_and_handles_zai_error_envelope() {
    let payload = json!({
        "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "percentage": 25.0, "nextResetTime": 1_769_000_000_000i64 },
            { "type": "TOKENS_LIMIT", "unit": 6, "percentage": 60.0 },
            { "type": "TIME_LIMIT", "unit": 5, "percentage": 10.0 },
            { "type": "UNKNOWN", "unit": 3, "percentage": 99.0 }
        ] }
    });
    let entries = glm::parse_limits(&payload, glm::Flavor::Zhipu).unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["five_hour", "weekly", "mcp"]);
    assert_eq!(entries[0].remaining_percent, Some(75.0));
    assert_eq!(
        entries[0].resets_at.as_deref(),
        Some("2026-01-21T12:53:20Z")
    );

    // Z.ai 顶层 limits 兜底 + 错误体
    let zai_fallback =
        json!({ "limits": [{ "type": "CREDIT_LIMIT", "unit": 6, "percentage": 5.0 }] });
    let entries = glm::parse_limits(&zai_fallback, glm::Flavor::Zai).unwrap();
    assert_eq!(entries[0].remaining_percent, Some(95.0));

    let err = glm::parse_limits(
        &json!({ "success": false, "code": 401, "msg": "unauthorized" }),
        glm::Flavor::Zai,
    )
    .unwrap_err();
    assert_eq!(err, "unauthorized");
    // 缺 limits 数组 → 明确报错
    assert!(glm::parse_limits(&json!({ "data": {} }), glm::Flavor::Zhipu).is_err());
}

#[test]
fn kimi_parses_usage_and_limits_with_all_reset_sources() {
    let payload = json!({
        "data": {
            "usage": { "limit": 100, "used": 30, "name": "Weekly limit", "reset_at": "2026-09-20T00:00:00Z" },
            "limits": [
                { "detail": { "limit": 40, "used": 10 }, "reset_in": 7200 },
                { "detail": { "limit": 50, "remaining": 25 }, "window": { "duration": 300, "timeUnit": "MINUTE" } }
            ]
        }
    });
    let entries = kimi::parse_usage(&payload, now()).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].key, "usage");
    assert_eq!(entries[0].label.as_deref(), Some("Weekly limit"));
    assert_eq!(entries[0].remaining_percent, Some(70.0));
    assert_eq!(
        entries[0].resets_at.as_deref(),
        Some("2026-09-20T00:00:00Z")
    );
    // reset_in（秒）
    assert_eq!(entries[1].remaining_percent, Some(75.0));
    assert_eq!(
        entries[1].resets_at.as_deref(),
        Some("2026-01-21T14:53:20Z")
    );
    // window.duration（秒）+ 时长标签
    assert_eq!(entries[2].label.as_deref(), Some("5h"));
    assert_eq!(entries[2].remaining_percent, Some(50.0));
    assert_eq!(
        entries[2].resets_at.as_deref(),
        Some("2026-01-21T12:58:20Z")
    );

    // 顶层形态也受理；无可用行 → 报错
    let flat = json!({ "usage": { "limit": 10, "used": 1 } });
    assert_eq!(kimi::parse_usage(&flat, now()).unwrap().len(), 1);
    assert!(kimi::parse_usage(&json!({ "data": {} }), now()).is_err());
}

#[test]
fn kimi_skips_rows_without_a_usable_limit_instead_of_faking_zero() {
    // 只有 used、没有 limit 的行不能当成 0% 剩余（会误报红色告警）
    let payload = json!({
        "limits": [
            { "detail": { "used": 5 } },
            { "detail": { "used": 5, "remaining": 5 } },
            { "detail": { "limit": 10, "used": 1 } }
        ]
    });
    let entries = kimi::parse_usage(&payload, now()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "limit_3");
    assert_eq!(entries[0].remaining_percent, Some(90.0));
}

#[test]
fn sanitize_redacts_secret_and_collapses_whitespace() {
    let secret = "sk-abcdefgh12345678";
    let out = sanitize(&format!("boom {secret}\n next line"), secret);
    assert!(!out.contains(secret));
    assert!(out.contains("[redacted]"));
    assert!(!out.contains('\n'));
    // 短串不当密钥处理（避免误伤普通词）
    assert_eq!(sanitize("error abc", "abc"), "error abc");
}

// ---------- 真实接口探针（默认不跑） ----------

/// 真实接口探针（默认不跑）：`cargo test --lib -- --ignored live_probe --nocapture`。
///
/// 走完整 `snapshots` 流程：数据源 = 环境变量 `OPENCODE_API_KEY`（优先）或本机 CodeWave
/// 配置里 base_url 命中 opencode.ai 的供应商（占位符形态走真实 keyring 回读）。
/// **本用例允许真实写 `~/.codewave/quota.json`**（额度是账号级事实）。
/// 密钥只用于请求头：不打印、不外传、不落盘。
#[tokio::test]
#[ignore = "真实网络请求：需本机 opencode.ai 供应商或 OPENCODE_API_KEY，手动运行 cargo test --lib -- --ignored live_probe"]
async fn live_probe_opencode_go_usage() {
    let source = probe_provider();
    let cfg = ConfigState {
        providers: vec![source.clone()],
        ..Default::default()
    };
    let data_dir = crate::core::config::data_dir();
    let snapshots = super::snapshots(&cfg, Some(&source.id), &data_dir).await;
    let snapshot = snapshots
        .into_iter()
        .find(|s| s.provider_id == source.id)
        .expect("供应商应有一行快照");

    assert_eq!(
        snapshot.status,
        QuotaStatus::Ok,
        "实调未成功：error={:?} reason={:?} key_source={:?}",
        snapshot.error,
        snapshot.reason,
        snapshot.key_source
    );
    assert!(!snapshot.entries.is_empty(), "实调返回空窗口列表");
    for entry in &snapshot.entries {
        assert!(
            entry.remaining_percent.is_some(),
            "窗口 {} 缺少剩余百分比：{entry:?}",
            entry.key
        );
        println!(
            "窗口 {}: 已用 {:?}% 剩余 {:?}% 重置 {:?} status={:?}",
            entry.key, entry.used_percent, entry.remaining_percent, entry.resets_at, entry.status
        );
    }
    println!(
        "供应商 {}（{}）密钥来源 {:?}；状态 {:?}；落盘目录 {}",
        snapshot.display_name,
        snapshot.provider_id,
        snapshot.key_source,
        snapshot.status,
        data_dir.display()
    );
}

/// 探针数据源：环境变量优先，其次本机配置里命中 opencode.ai 的供应商。
fn probe_provider() -> ProviderConfig {
    if let Ok(key) = std::env::var("OPENCODE_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return provider(
                "live-probe-env",
                "OpenCode Go (env)",
                "https://opencode.ai/zen/go/v1",
                &[key],
            );
        }
    }
    ConfigState::load()
        .providers
        .into_iter()
        .find(|p| kind_for_base_url(&p.base_url) == Some(ProviderKind::OpenCodeGo))
        .expect("本机未找到 opencode.ai 供应商（可设 OPENCODE_API_KEY 后重跑）")
}
