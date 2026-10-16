//! System credential adapter and resolver. New writes use immutable generation accounts via
//! core::config_commit; unavailable keyrings never fall back to plaintext persistence.

use crate::core::config::{KEYRING_PLACEHOLDER, ModelConfig, ProviderConfig};
/// 模型/API 凭据的 service；信任权威记录使用 host::trust 的独立 service。
pub const SERVICE: &str = "codewave.yangshifu.xyz";

/// 构造 keyring 条目（service + account）；环境不可用时返回可读错误。
fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|_| "keyring 不可用".to_string())
}

/// 读取一个 keyring 账户下存储的全部 key（多 key 以换行分隔存储），空行过滤。
fn read_account(account: &str) -> Result<Vec<String>, String> {
    entry(account)
        .and_then(|e| {
            e.get_password()
                .map_err(|_| "读取 keyring 失败".to_string())
        })
        .map(|stored| {
            stored
                .split('\n')
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
}

/// OS credential adapter. Backend errors are deliberately redacted (never include OS payloads).
pub struct SystemCredentialStore;
impl crate::core::config_commit::CredentialStore for SystemCredentialStore {
    fn read(
        &self,
        account: &str,
    ) -> Result<Vec<String>, crate::core::config_commit::ConfigCommitError> {
        read_account(account).map_err(|_| credential_error("CredentialReadFailed", "Cannot read saved credentials. Unlock the system keyring or explicitly replace the pool"))
    }
    fn write(
        &self,
        account: &str,
        keys: &[String],
    ) -> Result<(), crate::core::config_commit::ConfigCommitError> {
        entry(account).and_then(|e| e.set_password(&keys.join("\n")).map_err(|_| "write failed".into()))
            .map_err(|_| credential_error("CredentialWriteFailed", "Cannot store credentials securely. Unlock or enable the system keyring and retry; no plaintext fallback is used"))
    }
    fn delete(&self, account: &str) -> Result<(), crate::core::config_commit::ConfigCommitError> {
        let entry = entry(account).map_err(|_| {
            credential_error("CredentialCleanupFailed", "Credential cleanup needs retry")
        })?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(credential_error(
                "CredentialCleanupFailed",
                "Credential cleanup needs retry",
            )),
        }
    }
}
fn credential_error(code: &str, message: &str) -> crate::core::config_commit::ConfigCommitError {
    crate::core::config_commit::ConfigCommitError {
        code: code.into(),
        message: message.into(),
    }
}

/// 解析一个模型可用的 keys：
/// - keys 为空 → 空（用户全删即无 key；不回读旧 keyring 值）
/// - 含占位符 → 从 flatten 决议的 generation 或当前 v2 provider 账户回读，与明文行合并去重
/// - 纯明文 → 原样返回
pub fn resolve_keys(model: &ModelConfig) -> Vec<String> {
    resolve_keys_with(&SystemCredentialStore, model)
}
fn resolve_keys_with(
    store: &dyn crate::core::config_commit::CredentialStore,
    model: &ModelConfig,
) -> Vec<String> {
    if model.keys.is_empty() {
        return Vec::new();
    }
    let has_placeholder = model.keys.iter().any(|k| k == KEYRING_PLACEHOLDER);
    let mut resolved: Vec<String> = model
        .keys
        .iter()
        .filter(|k| !k.is_empty() && k.as_str() != KEYRING_PLACEHOLDER)
        .cloned()
        .collect();
    if !has_placeholder {
        return resolved;
    }
    let mut accounts = model.keyring_accounts.clone();
    if accounts.is_empty() {
        accounts.push(model.id.clone());
    }
    let mut last_err = None;
    for account in &accounts {
        match store.read(account) {
            Ok(keys) => {
                for k in keys {
                    if !resolved.contains(&k) {
                        resolved.push(k);
                    }
                }
            }
            Err(e) => {
                tracing::debug!("{e}（账户 {account}）");
                last_err = Some(e);
            }
        }
    }
    if resolved.is_empty()
        && let Some(e) = last_err
    {
        tracing::warn!("{e}（模型 {} 的 key 回退为空）", model.id);
    }
    resolved
}

/// 为一次性调用（title / compact 等无池场景）取首个可用 key。
/// 必须先过 resolve_keys 解析 keyring 占位符：直接对 config 原始 keys 做 filter 式挑选
///（provider::keys::pick_key）对 keyring 用户（keys == ["__keyring__"]）会得到 None，
/// 未带认证的请求将 401——[docs/chat-and-interface](../../../docs/chat-and-interface.md)
/// 缺陷的根因；title/compact 现统一走此入口。
pub fn pick_resolved_key(model: &ModelConfig) -> Option<String> {
    pick_resolved_key_with(&SystemCredentialStore, model)
}
fn pick_resolved_key_with(
    store: &dyn crate::core::config_commit::CredentialStore,
    model: &ModelConfig,
) -> Option<String> {
    resolve_keys_with(store, model)
        .into_iter()
        .find(|k| !k.is_empty())
}

/// provider 级回读（订阅额度用，[docs/chat-and-interface](../../../docs/chat-and-interface.md)）：
/// account = credential_ref，缺省才读当前 v2 provider.id；与模型摊平使用同一引用。
///
/// 与 `resolve_keys` 的差异：**回读失败必须返回 Err**——额度行态要据此区分
/// `invalid`（密钥形态不可用，重试无用）与 `no_key`（用户确实没配 key）。
/// 回读参数不能携带密钥，错误文案来自 keyring 层，不含密钥。
pub fn resolve_provider_keys(p: &ProviderConfig) -> Result<Vec<String>, String> {
    crate::core::config_commit::resolve_provider_with(&SystemCredentialStore, p)
        .map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ProviderConfig;
    struct MissingStore;
    impl crate::core::config_commit::CredentialStore for MissingStore {
        fn read(
            &self,
            _: &str,
        ) -> Result<Vec<String>, crate::core::config_commit::ConfigCommitError> {
            Err(credential_error(
                "CredentialReadFailed",
                "test account unavailable",
            ))
        }
        fn write(
            &self,
            _: &str,
            _: &[String],
        ) -> Result<(), crate::core::config_commit::ConfigCommitError> {
            panic!("read-only resolver")
        }
        fn delete(&self, _: &str) -> Result<(), crate::core::config_commit::ConfigCommitError> {
            panic!("read-only resolver")
        }
    }

    #[test]
    fn resolve_empty_keys_returns_empty_and_mixed_keeps_plain() {
        // keys 全删 → 空（不回读旧 keyring 值）
        let empty = ModelConfig {
            keys: vec![],
            ..Default::default()
        };
        assert!(resolve_keys_with(&MissingStore, &empty).is_empty());
        // 无 keyring 环境也成立的纯逻辑断言：含占位符但无可回读账户时，明文行仍保留
        let mixed = ModelConfig {
            keys: vec![KEYRING_PLACEHOLDER.into(), "sk-plain".into()],
            keyring_accounts: vec!["nonexistent-account-xyz".into()],
            ..Default::default()
        };
        let keys = resolve_keys_with(&MissingStore, &mixed);
        assert_eq!(keys, vec!["sk-plain".to_string()]);
    }

    #[test]
    fn pick_resolved_key_plain_yields_some() {
        let plain = ModelConfig {
            keys: vec!["sk-plain".into()],
            ..Default::default()
        };
        assert_eq!(pick_resolved_key(&plain).as_deref(), Some("sk-plain"));
    }

    #[test]
    fn pick_resolved_key_all_placeholder_without_keyring_yields_none() {
        // 占位符形态 + 不存在的 keyring 账户：解析为空 → None（[docs/chat-and-interface](../../../docs/chat-and-interface.md) 根因锚点）
        let placeholder = ModelConfig {
            keys: vec![KEYRING_PLACEHOLDER.into()],
            keyring_accounts: vec!["nonexistent-account-xyz".into()],
            ..Default::default()
        };
        assert_eq!(pick_resolved_key_with(&MissingStore, &placeholder), None);
    }

    #[test]
    fn pick_resolved_key_mixed_takes_plain_line() {
        let mixed = ModelConfig {
            keys: vec![KEYRING_PLACEHOLDER.into(), "sk-plain".into()],
            keyring_accounts: vec!["nonexistent-account-xyz".into()],
            ..Default::default()
        };
        assert_eq!(
            pick_resolved_key_with(&MissingStore, &mixed).as_deref(),
            Some("sk-plain")
        );
    }

    #[test]
    fn resolve_provider_keys_plain_returns_all_without_keyring() {
        let provider = ProviderConfig {
            id: "provider-plain".into(),
            keys: vec!["sk-plain-a".into(), "sk-plain-b".into()],
            ..Default::default()
        };
        assert_eq!(
            resolve_provider_keys(&provider),
            Ok(vec!["sk-plain-a".to_string(), "sk-plain-b".to_string()])
        );
        let none = ProviderConfig {
            id: "provider-none".into(),
            keys: vec![],
            ..Default::default()
        };
        assert_eq!(resolve_provider_keys(&none), Ok(Vec::new()));
    }
    #[test]
    fn model_title_compact_and_quota_share_immutable_reference() {
        struct GenerationStore;
        impl crate::core::config_commit::CredentialStore for GenerationStore {
            fn read(
                &self,
                account: &str,
            ) -> Result<Vec<String>, crate::core::config_commit::ConfigCommitError> {
                assert_eq!(account, "p1:credential:generation");
                Ok(vec!["versioned-first".into(), "versioned-second".into()])
            }
            fn write(
                &self,
                _: &str,
                _: &[String],
            ) -> Result<(), crate::core::config_commit::ConfigCommitError> {
                panic!("read-only")
            }
            fn delete(&self, _: &str) -> Result<(), crate::core::config_commit::ConfigCommitError> {
                panic!("read-only")
            }
        }
        let mut cfg = crate::core::config::ConfigState::default();
        cfg.providers.push(ProviderConfig {
            id: "p1".into(),
            credential_ref: Some("p1:credential:generation".into()),
            keys: vec![KEYRING_PLACEHOLDER.into()],
            models: vec![crate::core::config::ProviderModel {
                id: "m1".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let model = cfg.find_model("m1").unwrap();
        assert_eq!(model.keyring_accounts, vec!["p1:credential:generation"]);
        assert_eq!(
            resolve_keys_with(&GenerationStore, &model),
            vec!["versioned-first", "versioned-second"]
        );
        assert_eq!(
            pick_resolved_key_with(&GenerationStore, &model).as_deref(),
            Some("versioned-first")
        );
        assert_eq!(
            crate::core::config_commit::resolve_provider_with(&GenerationStore, &cfg.providers[0])
                .unwrap(),
            vec!["versioned-first", "versioned-second"]
        );
    }
}
