//! `core::quota` 单测：凭证链、opencode 路径、7 家解析与展示序。
//! 全部走注入（假环境变量 / 假文件 / 固定 now），不发网络请求、不读真实凭证。

use super::credentials::{resolve, resolve_with, Credential, CredentialSpec, Resolver};
use super::opencode_paths::{
    auth_candidates, config_candidates, parse_jsonc, provider_api_key, resolve_env_template,
    strip_jsonc, RuntimeEnv,
};
use super::providers::{deepseek, glm, kimi, minimax, opencode_go};
use super::{host_of, order_for, sanitize, ProviderKind, QuotaStatus, ALL};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEEPSEEK: CredentialSpec = CredentialSpec {
    env_vars: &["DEEPSEEK_API_KEY"],
    config_keys: &["deepseek"],
    auth_keys: &["deepseek"],
};

fn runtime(home: &Path, config_dir: &Path, data_dir: &Path) -> RuntimeEnv {
    RuntimeEnv {
        home: Some(home.to_path_buf()),
        xdg_config_home: Some(config_dir.to_path_buf()),
        xdg_data_home: Some(data_dir.to_path_buf()),
        ..Default::default()
    }
}

fn resolver<'a>(
    runtime: RuntimeEnv,
    env: &'a dyn Fn(&str) -> Option<String>,
    read_file: &'a dyn Fn(&Path) -> Option<String>,
) -> Resolver<'a> {
    Resolver {
        runtime,
        lookup_env: env,
        read_file,
    }
}

#[test]
fn credential_chain_prefers_env_then_config_then_auth() {
    let home = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("opencode").join("opencode.jsonc");
    let auth_path = data_dir.path().join("opencode").join("auth.json");

    let config_text =
        r#"{ "provider": { "deepseek": { "options": { "apiKey": "config-key" } } } }"#.to_string();
    let auth_text = r#"{ "deepseek": { "type": "api", "key": "auth-key" } }"#.to_string();

    let runtime = runtime(home.path(), config_dir.path(), data_dir.path());
    let no_env = |_: &str| None;
    let with_env = |name: &str| (name == "DEEPSEEK_API_KEY").then(|| "env-key".to_string());

    // 四个场景各用独立假文件系统（闭包借用规则要求不可边借边改）
    let none_files: HashMap<PathBuf, String> = HashMap::new();
    let read_none = |p: &Path| none_files.get(p).cloned();
    let full_files: HashMap<PathBuf, String> = HashMap::from([
        (config_path.clone(), config_text),
        (auth_path.clone(), auth_text.clone()),
    ]);
    let read_full = |p: &Path| full_files.get(p).cloned();
    let auth_only: HashMap<PathBuf, String> = HashMap::from([(auth_path, auth_text)]);
    let read_auth_only = |p: &Path| auth_only.get(p).cloned();

    // 环境变量优先
    assert_eq!(
        resolve_with(&DEEPSEEK, &resolver(runtime.clone(), &with_env, &read_none)),
        Credential::Found {
            key: "env-key".to_string(),
            source: "env:DEEPSEEK_API_KEY".to_string()
        }
    );
    // 其次全局配置（jsonc）
    assert_eq!(
        resolve_with(&DEEPSEEK, &resolver(runtime.clone(), &no_env, &read_full)),
        Credential::Found {
            key: "config-key".to_string(),
            source: "opencode.jsonc".to_string()
        }
    );
    // 配置文件缺失时回退 auth.json
    assert_eq!(
        resolve_with(&DEEPSEEK, &resolver(runtime.clone(), &no_env, &read_auth_only)),
        Credential::Found {
            key: "auth-key".to_string(),
            source: "auth.json".to_string()
        }
    );
    // 全无 → Missing（不是错误）
    assert_eq!(
        resolve_with(&DEEPSEEK, &resolver(runtime, &no_env, &read_none)),
        Credential::Missing
    );
}

#[test]
fn credential_chain_flags_oauth_entries_as_invalid() {
    let home = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let auth_path = data_dir.path().join("opencode").join("auth.json");
    let mut files = HashMap::new();
    files.insert(
        auth_path,
        r#"{ "deepseek": { "type": "oauth", "access": "x" } }"#.to_string(),
    );
    let runtime = runtime(home.path(), config_dir.path(), data_dir.path());
    let no_env = |_: &str| None;
    let read = |p: &Path| files.get(p).cloned();
    let resolved = resolve_with(&DEEPSEEK, &resolver(runtime, &no_env, &read));
    match resolved {
        Credential::Invalid(reason) => assert!(reason.contains("oauth"), "{reason}"),
        other => panic!("应判为 Invalid，实际 {other:?}"),
    }
}

#[test]
fn config_api_key_supports_env_templates_and_rejects_unknown_placeholders() {
    let lookup = |name: &str| (name == "DEEPSEEK_API_KEY").then(|| "from-env".to_string());
    assert_eq!(
        resolve_env_template("${DEEPSEEK_API_KEY}", &["DEEPSEEK_API_KEY"], &lookup).unwrap(),
        "from-env"
    );
    assert_eq!(
        resolve_env_template("sk-${DEEPSEEK_API_KEY}-suffix", &["DEEPSEEK_API_KEY"], &lookup).unwrap(),
        "sk-from-env-suffix"
    );
    // 未授权占位符 / 解析为空 → 失败（不静默降级）
    assert!(resolve_env_template("${OTHER_KEY}", &["DEEPSEEK_API_KEY"], &lookup).is_none());
    assert!(resolve_env_template("${DEEPSEEK_API_KEY}", &["DEEPSEEK_API_KEY"], &|_| None).is_none());
}

#[test]
fn jsonc_strips_comments_and_trailing_commas_only_outside_strings() {
    let text = r#"{
  // 行注释
  "a": "http://keep//me",
  /* 块注释 */
  "b": [1, 2,],
}"#;
    let parsed = parse_jsonc(text).expect("jsonc 应可解析");
    assert_eq!(parsed["a"], json!("http://keep//me"));
    assert_eq!(parsed["b"], json!([1, 2]));
    // 字符串里的注释符号必须原样保留
    assert!(strip_jsonc(r#"{"u":"//x"}"#).contains("//x"));
}

#[test]
fn provider_api_key_reads_nested_options_and_skips_empty() {
    let config = json!({
        "provider": {
            "zai": { "options": { "apiKey": "  " } },
            "zai-coding-plan": { "options": { "apiKey": "zai-key" } }
        }
    });
    assert_eq!(
        provider_api_key(&config, &["zai", "zai-coding-plan"]).unwrap(),
        "zai-key"
    );
    assert!(provider_api_key(&json!({}), &["zai"]).is_none());
}

#[test]
fn runtime_path_candidates_include_opencode_dirs() {
    let home = PathBuf::from("C:/home/u");
    let env = RuntimeEnv {
        home: Some(home.clone()),
        app_data: Some(PathBuf::from("C:/home/u/AppData/Roaming")),
        local_app_data: Some(PathBuf::from("C:/home/u/AppData/Local")),
        ..Default::default()
    };
    let configs = config_candidates(&env);
    assert_eq!(
        configs[0].path,
        home.join(".config/opencode/opencode.jsonc")
    );
    assert!(configs[0].is_jsonc);
    assert_eq!(configs[1].path, home.join(".config/opencode/opencode.json"));

    let auths = auth_candidates(&env);
    assert_eq!(auths[0], home.join(".local/share/opencode/auth.json"));
    assert!(auths.contains(&PathBuf::from("C:/home/u/AppData/Local/opencode/auth.json")));
}

#[test]
fn opencode_go_parses_percent_windows_and_skips_broken_ones() {
    let body = json!({
        "usage": {
            "rolling": { "status": "ok", "percent": 12.5, "resetsAt": "2026-09-18T20:00:00Z" },
            "weekly": { "status": "error", "percent": 50.0, "resetsAt": "2026-09-20T00:00:00Z" },
            "monthly": { "status": "ok", "percent": 140.0, "resetsAt": "2026-10-01T00:00:00Z" }
        }
    });
    let entries = opencode_go::parse_usage(&body).unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["rolling", "monthly"]);
    assert_eq!(entries[0].remaining_percent, Some(87.5));
    assert_eq!(entries[0].used_percent, Some(12.5));
    // 越界百分比收敛到 0-100
    assert_eq!(entries[1].remaining_percent, Some(0.0));
    // 全不可用 → 明确报错，而不是返回空列表
    assert!(opencode_go::parse_usage(&json!({ "usage": {} })).is_err());
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
    let texts: Vec<&str> = entries.iter().map(|e| e.value_text.as_deref().unwrap()).collect();
    assert_eq!(texts, vec!["CNY 12.50 · 不可用", "USD 3.25 · 不可用"]);
    assert_eq!(entries[0].key, "balance_cny");
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
    assert_eq!(
        intl[0].resets_at.as_deref(),
        Some("2026-01-21T13:53:20Z")
    );

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
    assert_eq!(entries[0].resets_at.as_deref(), Some("2026-01-21T13:53:20Z"));
}

#[test]
fn glm_maps_units_to_windows_and_handles_zai_error_envelope() {
    let payload = json!({
        "data": { "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "percentage": 25.0, "nextResetTime": 1_769_000_000_000i64 },
            { "type": "TOKENS_LIMIT", "unit": 6, "percentage": 60.0 },
            { "type": "TIME_LIMIT", "unit": 5, "percentage": 10.0 },
            { "type": "UNKNOWN", "unit": 3, "percentage": 99.0 }
        ]}
    });
    let entries = glm::parse_limits(&payload, glm::Flavor::Zhipu).unwrap();
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["five_hour", "weekly", "mcp"]);
    assert_eq!(entries[0].remaining_percent, Some(75.0));
    assert_eq!(entries[0].resets_at.as_deref(), Some("2026-01-21T12:53:20Z"));

    // Z.ai 顶层 limits 兜底 + 错误体
    let zai_fallback = json!({ "limits": [{ "type": "CREDIT_LIMIT", "unit": 6, "percentage": 5.0 }] });
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
    assert_eq!(entries[0].resets_at.as_deref(), Some("2026-09-20T00:00:00Z"));
    // reset_in（秒）
    assert_eq!(entries[1].remaining_percent, Some(75.0));
    assert_eq!(entries[1].resets_at.as_deref(), Some("2026-01-21T14:53:20Z"));
    // window.duration（秒）+ 时长标签
    assert_eq!(entries[2].label.as_deref(), Some("5h"));
    assert_eq!(entries[2].remaining_percent, Some(50.0));
    assert_eq!(entries[2].resets_at.as_deref(), Some("2026-01-21T12:58:20Z"));

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
fn order_for_puts_current_session_provider_first() {
    assert_eq!(order_for(None), ALL.to_vec());
    // 域名命中（含子域）→ 置顶，其余保持固定序
    let ordered = order_for(Some("https://api.deepseek.com/v1"));
    assert_eq!(ordered[0], ProviderKind::DeepSeek);
    assert_eq!(ordered.len(), ALL.len());
    assert_eq!(*ordered.last().unwrap(), ProviderKind::Zai);
    // 用户当前的 opencode 网关：命中 OpenCode Go（本就首位）
    assert_eq!(
        order_for(Some("https://opencode.ai/zen/go/v1"))[0],
        ProviderKind::OpenCodeGo
    );
    // 匹配不到 → 原序
    assert_eq!(order_for(Some("https://example.com/v1")), ALL.to_vec());
}

#[test]
fn host_of_handles_ports_userinfo_and_missing_scheme() {
    assert_eq!(host_of("https://api.z.ai:8443/api").as_deref(), Some("api.z.ai"));
    assert_eq!(host_of("https://user@api.kimi.com/x").as_deref(), Some("api.kimi.com"));
    assert_eq!(host_of("api.minimax.io/v1").as_deref(), Some("api.minimax.io"));
    assert_eq!(host_of("").as_deref(), None);
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

#[test]
fn quota_status_wire_names_are_snake_case() {
    assert_eq!(serde_json::to_string(&QuotaStatus::Ok).unwrap(), "\"ok\"");
    assert_eq!(
        serde_json::to_string(&QuotaStatus::Invalid).unwrap(),
        "\"invalid\""
    );
    assert_eq!(
        serde_json::to_string(&QuotaStatus::Error).unwrap(),
        "\"error\""
    );
}

/// 真实接口探针（默认不跑）：`cargo test --lib -- --ignored live_probe`。
/// 用本机 opencode 凭证实调一次官方 usage 接口，用于校对字段漂移；
/// 凭证只用于请求头，不外传、不落盘、不进日志。
#[tokio::test]
#[ignore = "真实网络请求：需本机 OpenCode Go 凭证，手动运行 cargo test --lib -- --ignored live_probe"]
async fn live_probe_opencode_go_usage() {
    let spec = ProviderKind::OpenCodeGo.credential_spec();
    let Credential::Found { key, source } = resolve(&spec) else {
        panic!("本机未检测到 OpenCode Go 凭证（查 OPENCODE_API_KEY / opencode 配置 / auth.json）");
    };
    let cfg = crate::core::config::ConfigState::default();
    let client = crate::provider::proxy::build_client(&cfg);
    let entries = opencode_go::fetch(&key, &client)
        .await
        .unwrap_or_else(|e| panic!("OpenCode Go 实调失败（凭证来源 {source}）：{e}"));
    assert!(!entries.is_empty(), "实调返回空窗口列表");
    for entry in &entries {
        assert!(
            entry.remaining_percent.is_some() && entry.resets_at.is_some(),
            "窗口 {} 缺少剩余百分比或重置时间：{entry:?}",
            entry.key
        );
    }
    println!("凭证来源 {source}；窗口：{entries:?}");
}
