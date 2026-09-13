//! 回合级重试策略（[docs/p0-plan](../../../docs/p0-plan.md) §4.5）：指数退避 500ms×2ⁿ 封顶 10s，最多重试 6 次；
//! Auth/Billing 立即失败；BadRequest 允许 sanitize 后免费重试一次（由 agent 主循环控制）。

use std::time::Duration;

/// 最大重试次数（不含首次请求）。
pub const MAX_RETRIES: u32 = 6;
/// 退避基数：第 n 次重试延迟 BASE_DELAY × 2ⁿ。
pub const BASE_DELAY: Duration = Duration::from_millis(500);
/// 单次退避延迟上限。
pub const MAX_DELAY: Duration = Duration::from_millis(10_000);

/// 第 attempt 次重试（从 0 计）的退避延迟：BASE_DELAY × 2^attempt，封顶 MAX_DELAY。
pub fn delay_for_attempt(attempt: u32) -> Duration {
    let step = BASE_DELAY.saturating_mul(2u32.saturating_pow(attempt.min(16)));
    step.min(MAX_DELAY)
}

/// 判断一个 ProviderError 是否值得重试（Auth 不重试；Cancelled 不重试）。
pub fn should_retry(err: &super::dto::ProviderError, attempt: u32) -> bool {
    use super::dto::ProviderError as E;
    if attempt >= MAX_RETRIES {
        return false;
    }
    match err {
        E::Auth(_) | E::Billing(_) | E::Cancelled => false,
        E::RateLimited(_) | E::Server(_) | E::Network(_) => true,
        E::BadRequest { .. } => false, // BadRequest 的 sanitize 免费重试由 agent 主循环控制，不在此计数
        E::Protocol(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::super::dto::ProviderError;
    use super::*;

    #[test]
    fn backoff_curve() {
        assert_eq!(delay_for_attempt(0), Duration::from_millis(500));
        assert_eq!(delay_for_attempt(1), Duration::from_millis(1000));
        assert_eq!(delay_for_attempt(2), Duration::from_millis(2000));
        assert_eq!(delay_for_attempt(5), Duration::from_millis(10_000)); // 16s → 封顶 10s
        assert_eq!(delay_for_attempt(9), Duration::from_millis(10_000));
    }

    #[test]
    fn retry_matrix() {
        assert!(!should_retry(&ProviderError::Auth("x".into()), 0));
        assert!(!should_retry(
            &ProviderError::Billing("HTTP 402: Insufficient Balance".into()),
            0
        ));
        assert!(!should_retry(&ProviderError::Cancelled, 0));
        assert!(!should_retry(
            &ProviderError::BadRequest {
                message: "x".into(),
                sanitize_hint: true
            },
            0
        ));
        assert!(should_retry(&ProviderError::RateLimited("x".into()), 0));
        // attempt 从 0 计已完成的重试次数：MAX_RETRIES-1 给到第 6 次（最后一次）重试，
        // MAX_RETRIES 及以后拒绝（「最多重试 6 次」，[docs/p0-plan](../../../docs/p0-plan.md) §4.5）
        assert!(should_retry(
            &ProviderError::Server("x".into()),
            MAX_RETRIES - 1
        ));
        assert!(!should_retry(
            &ProviderError::Server("x".into()),
            MAX_RETRIES
        ));
    }
}
