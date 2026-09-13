//! 多 key 池与 failover（[docs/p1-plan](../../../docs/p1-plan.md) §3.2）：
//! 认证失败冷却 30min；瞬时失败冷却 10s×2^(n-1)、封顶 30min；整池冷却时取最早恢复者。

use super::dto::ProviderError;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 认证失败冷却时长（30 分钟）：换 key 也救不了错 key，长冷却逼 failover。
pub const AUTH_COOLDOWN: Duration = Duration::from_secs(30 * 60);
/// 瞬时失败冷却基数（10 秒），按连续失败次数指数退避。
pub const TRANSIENT_BASE: Duration = Duration::from_secs(10);
/// 瞬时失败冷却上限（30 分钟），与认证冷却对齐。
pub const TRANSIENT_MAX: Duration = Duration::from_secs(30 * 60);

/// 单个 key 的健康状态。
#[derive(Debug, Clone, PartialEq)]
#[derive(Default)]
struct KeyHealth {
    /// 冷却截止时刻；None = 可用
    cool_until: Option<Instant>,
    /// 连续瞬时失败次数（指数退避依据；认证失败时清零——冷却时长固定）
    consecutive: u32,
    /// 最近一次错误文案（排障用）
    last_error: Option<String>,
}

/// key 池：按 pool_key 分组的健康表，负责多 key 择优与失败冷却。
#[derive(Default)]
pub struct KeyPool {
    // pool_key（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md) 起为 provider id）→ 与 keys 平行的健康表；同 provider 的模型共享冷却
    health: Mutex<HashMap<String, Vec<KeyHealth>>>,
}

/// 简单取 key：取第一个非空、非占位符的 key（无需池化能力的场景用）。
pub fn pick_key(keys: &[String]) -> Option<String> {
    keys.iter()
        .find(|k| !k.is_empty() && k.as_str() != crate::core::config::KEYRING_PLACEHOLDER)
        .cloned()
}

impl KeyPool {
    /// 选 key：返回 (下标, key)。优先取首个未冷却者；全在冷却则取最早恢复者。
    pub fn pick(&self, pool_key: &str, keys: &[String]) -> Option<(usize, String)> {
        let usable: Vec<usize> = keys
            .iter()
            .enumerate()
            .filter(|(_, k)| {
                !k.is_empty() && k.as_str() != crate::core::config::KEYRING_PLACEHOLDER
            })
            .map(|(i, _)| i)
            .collect();
        if usable.is_empty() {
            return None;
        }
        let mut health = self.health.lock().unwrap();
        let table = health.entry(pool_key.to_string()).or_default();
        // 对齐健康表长度
        if table.len() < keys.len() {
            table.resize(keys.len(), KeyHealth::default());
        }
        let now = Instant::now();
        // 首个未冷却者
        for &i in &usable {
            if table[i].cool_until.map(|t| t <= now).unwrap_or(true) {
                return Some((i, keys[i].clone()));
            }
        }
        // 全在冷却：取最早恢复者
        let best = usable
            .into_iter()
            .min_by_key(|&i| table[i].cool_until.unwrap_or(now))
            .unwrap();
        Some((best, keys[best].clone()))
    }

    /// 上报一次结果：None = 成功（重置健康态）；Some = 失败并按错误类型计算冷却。
    pub fn report(&self, pool_key: &str, keys: &[String], idx: usize, err: Option<&ProviderError>) {
        let mut health = self.health.lock().unwrap();
        let table = health.entry(pool_key.to_string()).or_default();
        if table.len() < keys.len() {
            table.resize(keys.len(), KeyHealth::default());
        }
        let Some(h) = table.get_mut(idx) else { return };
        match err {
            None => {
                h.cool_until = None;
                h.consecutive = 0;
                h.last_error = None;
            }
            Some(e) => {
                h.last_error = Some(e.to_string());
                if e.is_auth() {
                    h.cool_until = Some(Instant::now() + AUTH_COOLDOWN);
                    h.consecutive = 0;
                } else {
                    h.consecutive += 1;
                    let mult = 2u32.saturating_pow(h.consecutive.saturating_sub(1).min(16));
                    let step = TRANSIENT_BASE.saturating_mul(mult).min(TRANSIENT_MAX);
                    h.cool_until = Some(Instant::now() + step);
                }
            }
        }
    }

    /// 供测试/展示用：当前各 key 的冷却状态（是否冷却, 连续失败次数）。
    pub fn debug_state(&self, pool_key: &str) -> Vec<(bool, u32)> {
        let health = self.health.lock().unwrap();
        health
            .get(pool_key)
            .map(|v| {
                v.iter()
                    .map(|h| (h.cool_until.is_some(), h.consecutive))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：清空全部冷却状态（配置保存后调用），
    /// 让修好的 key 下次尝试即生效，而不是被残留的认证冷却跳过。
    pub fn reset_all(&self) {
        self.health.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_usable_skipping_placeholders() {
        assert_eq!(pick_key(&[]), None);
        assert_eq!(
            pick_key(&["".into(), "__keyring__".into(), "k".into()]),
            Some("k".into())
        );
    }

    #[test]
    fn auth_failure_cools_and_failover() {
        let pool = KeyPool::default();
        let keys = vec!["k0".into(), "k1".into(), "k2".into()];
        // 初始选中 k0
        let (i, k) = pool.pick("m", &keys).unwrap();
        assert_eq!((i, k.as_str()), (0, "k0"));
        // k0 认证失败 → 30min 冷却 → 下次选中 k1
        pool.report("m", &keys, 0, Some(&ProviderError::Auth("401".into())));
        let (i, _) = pool.pick("m", &keys).unwrap();
        assert_eq!(i, 1);
        let state = pool.debug_state("m");
        assert!(state[0].0, "k0 应在冷却");
    }

    #[test]
    fn transient_backoff_then_success_resets() {
        let pool = KeyPool::default();
        let keys = vec!["k0".into()];
        pool.report(
            "m",
            &keys,
            0,
            Some(&ProviderError::RateLimited("429".into())),
        );
        let state = pool.debug_state("m");
        assert_eq!(state[0].1, 1);
        // 成功上报后重置
        pool.report("m", &keys, 0, None);
        let state = pool.debug_state("m");
        assert_eq!(state[0], (false, 0));
    }

    #[test]
    fn all_cooldown_picks_earliest_recovery() {
        let pool = KeyPool::default();
        let keys = vec!["a".into(), "b".into()];
        // a 认证冷却 30min；b 瞬时冷却 10s → 最早恢复者是 b
        pool.report("m", &keys, 0, Some(&ProviderError::Auth("x".into())));
        pool.report("m", &keys, 1, Some(&ProviderError::RateLimited("y".into())));
        let (i, _) = pool.pick("m", &keys).unwrap();
        assert_eq!(i, 1, "应选冷却更短的 b");
    }

    /// [docs/auth-error-guidance](../../../docs/auth-error-guidance.md)：reset_all（配置保存）清空所有冷却，重新回到首个可用 key。
    #[test]
    fn reset_all_clears_cooldowns() {
        let pool = KeyPool::default();
        let keys = vec!["a".into(), "b".into()];
        pool.report("m", &keys, 0, Some(&ProviderError::Auth("x".into())));
        pool.report("m", &keys, 1, Some(&ProviderError::RateLimited("y".into())));
        assert!(pool.debug_state("m").iter().any(|(cooling, _)| *cooling));
        pool.reset_all();
        assert!(pool.debug_state("m").iter().all(|(cooling, _)| !*cooling));
        let (i, _) = pool.pick("m", &keys).unwrap();
        assert_eq!(i, 0, "冷却清零后回到首个可用 key");
    }
}
