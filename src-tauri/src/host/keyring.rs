//! keyring 迁移（[docs/p1-plan](../../../docs/p1-plan.md) §7.2）：service 名 `codewave.yangshifu.xyz`，account = provider.id（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md) 起 key 归属 provider）。
//! 明文 key 迁入系统 keyring，config 中以占位符替代；解析时按占位符回读。
//! keyring 不可用（如 Linux 无 Secret Service）时回退明文并在日志中注明。

use crate::core::config::{ConfigState, ModelConfig, KEYRING_PLACEHOLDER};
/// keyring service 名（本应用所有凭据共用）。
pub const SERVICE: &str = "codewave.yangshifu.xyz";

/// 构造 keyring 条目（service + account）；环境不可用时返回可读错误。
fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|e| format!("keyring 不可用：{e}"))
}

/// 读取一个 keyring 账户下存储的全部 key（多 key 以换行分隔存储），空行过滤。
fn read_account(account: &str) -> Result<Vec<String>, String> {
    entry(account)
        .and_then(|e| {
            e.get_password()
                .map_err(|e| format!("读取 keyring 失败：{e}"))
        })
        .map(|stored| {
            stored
                .split('\n')
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
}

/// 启动/保存时迁移：各 provider 的明文 key → keyring；config 中替换为占位符。
/// 写入前过滤占位符与空行（防止占位符字符串污染 keyring）。
/// 返回 (是否有变更, 警告)。
pub fn migrate(cfg: &mut ConfigState) -> (bool, Option<String>) {
    let mut changed = false;
    let mut warning = None;
    for p in cfg.providers.iter_mut() {
        let plain: Vec<String> = p
            .keys
            .iter()
            .filter(|k| !k.is_empty() && k.as_str() != KEYRING_PLACEHOLDER)
            .cloned()
            .collect();
        if plain.is_empty() {
            continue;
        }
        match entry(&p.id).and_then(|e| {
            e.set_password(&plain.join("\n"))
                .map_err(|e| format!("写入 keyring 失败：{e}"))
        }) {
            Ok(()) => {
                p.keys = vec![KEYRING_PLACEHOLDER.to_string()];
                changed = true;
            }
            Err(e) => {
                tracing::warn!("{e}（保留明文存储）");
                warning = Some(e);
            }
        }
    }
    (changed, warning)
}

/// 解析一个模型可用的 keys：
/// - keys 为空 → 空（用户全删即无 key；不回读旧 keyring 值）
/// - 含占位符 → 从 keyring 回读（账户优先级：provider 账户 → legacy 按模型账户），与明文行合并去重
/// - 纯明文 → 原样返回
pub fn resolve_keys(model: &ModelConfig) -> Vec<String> {
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
        match read_account(account) {
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
    if resolved.is_empty() {
        if let Some(e) = last_err {
            tracing::warn!("{e}（模型 {} 的 key 回退为空）", model.id);
        }
    }
    resolved
}

/// 为一次性调用（title / compact 等无池场景）取首个可用 key。
/// 必须先过 resolve_keys 解析 keyring 占位符：直接对 config 原始 keys 做 filter 式挑选
///（provider::keys::pick_key）对 keyring 用户（keys == ["__keyring__"]）会得到 None，
/// 未带认证的请求将 401——[docs/topbar-migration-and-git-identity](../../../docs/topbar-migration-and-git-identity.md)
/// 缺陷的根因；title/compact 现统一走此入口。
pub fn pick_resolved_key(model: &ModelConfig) -> Option<String> {
    resolve_keys(model).into_iter().find(|k| !k.is_empty())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ProviderConfig, ProviderModel};

    /// 涉及真实 keyring 的测试必须串行：keyring 是机器全局资源，
    /// 并发写→回读存在可见性竞态（[docs/topbar-migration-and-git-identity](../../../docs/topbar-migration-and-git-identity.md) 批次实测：两个 migrate
    /// 测试并发时间歇性 assert contains 失败）
    static KEYRING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn migrate_and_resolve_roundtrip() {
        let _serial = KEYRING_LOCK.lock().unwrap();
        let mut cfg = ConfigState::default();
        cfg.providers.push(ProviderConfig {
            keys: vec!["sk-plain".to_string()],
            models: vec![ProviderModel {
                id: "m1".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let provider_id = cfg.providers[0].id.clone();

        let (changed, warning) = migrate(&mut cfg);
        let flat = cfg.find_model("m1").unwrap();
        if warning.is_some() {
            // 无 keyring 环境（CI Linux）：保留明文，解析原样返回
            assert!(!changed);
            assert_eq!(resolve_keys(&flat), vec!["sk-plain".to_string()]);
            return;
        }
        assert!(changed);
        assert_eq!(cfg.providers[0].keys, vec![KEYRING_PLACEHOLDER.to_string()]);
        assert_eq!(resolve_keys(&flat), vec!["sk-plain".to_string()]);
        // 清理 keyring 条目（避免污染真实 keyring）
        let _ = entry(&provider_id).and_then(|e| e.delete_credential().map_err(|e| e.to_string()));
    }

    #[test]
    fn resolve_empty_keys_returns_empty_and_mixed_keeps_plain() {
        // keys 全删 → 空（不回读旧 keyring 值）
        let empty = ModelConfig {
            keys: vec![],
            ..Default::default()
        };
        assert!(resolve_keys(&empty).is_empty());
        // 无 keyring 环境也成立的纯逻辑断言：含占位符但无可回读账户时，明文行仍保留
        let mixed = ModelConfig {
            keys: vec![KEYRING_PLACEHOLDER.into(), "sk-plain".into()],
            keyring_accounts: vec!["nonexistent-account-xyz".into()],
            ..Default::default()
        };
        let keys = resolve_keys(&mixed);
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
        // 占位符形态 + 不存在的 keyring 账户：解析为空 → None（[docs/topbar-migration-and-git-identity](../../../docs/topbar-migration-and-git-identity.md) 根因锚点）
        let placeholder = ModelConfig {
            keys: vec![KEYRING_PLACEHOLDER.into()],
            keyring_accounts: vec!["nonexistent-account-xyz".into()],
            ..Default::default()
        };
        assert_eq!(pick_resolved_key(&placeholder), None);
    }

    #[test]
    fn pick_resolved_key_mixed_takes_plain_line() {
        let mixed = ModelConfig {
            keys: vec![KEYRING_PLACEHOLDER.into(), "sk-plain".into()],
            keyring_accounts: vec!["nonexistent-account-xyz".into()],
            ..Default::default()
        };
        assert_eq!(pick_resolved_key(&mixed).as_deref(), Some("sk-plain"));
    }
}
