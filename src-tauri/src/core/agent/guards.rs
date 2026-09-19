use super::runtime::SessionRuntime;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// panic unwind 兜底守卫：drive_agent 主路径提前解除武装（armed=false）后按序收尾；
/// 一旦中途 panic，Drop 兜底停掉流式刷新 ticker 并清掉活跃 cancel token，
/// 避免泄漏后台循环与悬挂的取消句柄。
pub(super) struct DriveUnwindGuard {
    pub(super) rt: Arc<SessionRuntime>,
    pub(super) flush_stop: CancellationToken,
    /// false = 主路径已按序收尾，Drop 不再重复动作
    pub(super) armed: bool,
}
impl Drop for DriveUnwindGuard {
    fn drop(&mut self) {
        if self.armed {
            self.flush_stop.cancel();
            *lock_ok(&self.rt.active_cancel) = None;
        }
    }
}

/// 中毒锁安全的加锁助手：若 panic 已让锁中毒，直接 unwrap 会在收尾路径二次 panic、
/// 打断 run:error 清理；改用 into_inner 保住数据继续收尾。
pub(crate) fn lock_ok<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// 压缩互斥 RAII（[docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md)）：acquire 以 swap(true) 抢占槽位，失败返回 None
/// （其余路径一律不得手工构造——错误构造的 Drop 会清掉别人的槽位）。Drop 恒复位 compacting：
/// 若 compact_history panic unwind 后不复位，start_chat 将永远拒绝新 run，会话直接变砖。
pub(crate) struct CompactingGuard {
    pub(super) rt: Arc<SessionRuntime>,
}
impl CompactingGuard {
    /// 抢占压缩槽位；已被占（swap 返回 true）则返回 None，绝不构造错误守卫。
    pub(crate) fn acquire(rt: &Arc<SessionRuntime>) -> Option<Self> {
        match rt.compacting.swap(true, Ordering::SeqCst) {
            false => Some(Self { rt: rt.clone() }),
            true => None,
        }
    }
}
impl Drop for CompactingGuard {
    fn drop(&mut self) {
        self.rt.compacting.store(false, Ordering::SeqCst);
    }
}
