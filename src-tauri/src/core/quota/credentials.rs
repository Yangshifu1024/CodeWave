//! 额度凭证链：环境变量 → opencode 全局配置 → `auth.json`（只读，绝不落盘、绝不外传）。
//!
//! 与上游 opencode-quota 的解析顺序一致；每个提供商的键名/环境变量名各自声明，
//! 不复用别家（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。

use super::opencode_paths::{
    AuthEntry, RuntimeEnv, auth_candidates, auth_entry, config_candidates, parse_jsonc,
    provider_api_key, read_text, resolve_env_template,
};
use std::path::Path;

/// 某提供商的凭证来源声明（环境变量 → 配置 provider 键 → auth.json 条目键，各自有序）。
pub struct CredentialSpec {
    pub env_vars: &'static [&'static str],
    pub config_keys: &'static [&'static str],
    pub auth_keys: &'static [&'static str],
}

/// 凭证解析结果。`Missing` 表示「未配置」（不是错误）；`Invalid` 表示配置存在但形态不可用。
#[derive(Debug, Clone, PartialEq)]
pub enum Credential {
    Missing,
    Invalid(String),
    Found { key: String, source: String },
}

/// 注入的解析环境：基目录 + 环境变量查询 + 文件读取（测试替换为临时目录与假数据）。
pub struct Resolver<'a> {
    pub runtime: RuntimeEnv,
    pub lookup_env: &'a dyn Fn(&str) -> Option<String>,
    pub read_file: &'a dyn Fn(&Path) -> Option<String>,
}

fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// 生产解析入口。
pub fn resolve(spec: &CredentialSpec) -> Credential {
    let lookup: fn(&str) -> Option<String> = env_lookup;
    let read: fn(&Path) -> Option<String> = read_text;
    resolve_with(
        spec,
        &Resolver {
            runtime: RuntimeEnv::from_process(),
            lookup_env: &lookup,
            read_file: &read,
        },
    )
}

/// 注入式解析（单测入口）：严格按 环境变量 → 全局配置 → auth.json 的顺序取首个命中。
pub fn resolve_with(spec: &CredentialSpec, resolver: &Resolver<'_>) -> Credential {
    for name in spec.env_vars {
        if let Some(value) = (resolver.lookup_env)(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Credential::Found {
                    key: trimmed.to_string(),
                    source: format!("env:{name}"),
                };
            }
        }
    }

    for candidate in config_candidates(&resolver.runtime) {
        let Some(text) = (resolver.read_file)(&candidate.path) else {
            continue;
        };
        let Some(config) = parse_jsonc(&text) else {
            continue;
        };
        let Some(raw) = provider_api_key(&config, spec.config_keys) else {
            continue;
        };
        let Some(key) = resolve_env_template(&raw, spec.env_vars, &resolver.lookup_env) else {
            continue;
        };
        return Credential::Found {
            key,
            source: if candidate.is_jsonc {
                "opencode.jsonc".to_string()
            } else {
                "opencode.json".to_string()
            },
        };
    }

    for path in auth_candidates(&resolver.runtime) {
        let Some(text) = (resolver.read_file)(&path) else {
            continue;
        };
        let Some(auth) = parse_jsonc(&text) else {
            continue;
        };
        match auth_entry(&auth, spec.auth_keys) {
            AuthEntry::Missing => continue,
            AuthEntry::Invalid(reason) => return Credential::Invalid(reason),
            AuthEntry::Key(key) => {
                return Credential::Found {
                    key,
                    source: "auth.json".to_string(),
                };
            }
        }
    }

    Credential::Missing
}
