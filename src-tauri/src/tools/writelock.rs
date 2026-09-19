//! 进程级文件写互斥（[docs/tools-optimization-and-gap-fill-plan](../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 2）：当对同一文件的并发写来自不同
//! runtime（并行子代理 / 定时任务 / 多会话）时，把「读 → 试运行 → 写」整段串行化，消除试运行与写入之间
//! 文件被改动的 TOCTOU，以及「后写覆盖先写」。批次层的每-runtime `file_ops` 只覆盖会话内顺序，与本模块
//! 是正交的两层。锁按规范路径排序去重后获取（防死锁）；
//! 同批次同路径写入已被批次层 `E_WRITE_BATCH_CONFLICT` 拒绝，单个 runtime 不会对同一路径二次加锁。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// 全局锁表：规范路径 → 共享 tokio Mutex。进程内唯一，惰性初始化。
static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();

/// 锁表访问入口。
fn locks() -> &'static Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> {
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 锁 key：尽力规范化路径，使相对/绝对、符号链接等别名落到同一把锁上。
fn key(p: &Path) -> PathBuf {
    crate::tools::pathutil::canonical_best_effort(p)
}

/// 按排序去重后的顺序获取所有路径的写锁；返回的 guard 生命周期即锁生命周期（drop 即释放）。
/// cancel 为 Some 时逐锁监听取消：取消立即返回 Err（已获锁的 guard 随 guards drop 释放），
/// 避免等待被其它 runtime 永久持有的锁时停止按钮失效（批次取消盲区修复）。
pub async fn acquire_all(
    paths: &[PathBuf],
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Vec<tokio::sync::OwnedMutexGuard<()>>, tokio_util::sync::CancellationToken> {
    let mut keys: Vec<PathBuf> = paths.iter().map(|p| key(p)).collect();
    keys.sort();
    keys.dedup();
    let mut guards = Vec::with_capacity(keys.len());
    for k in keys {
        let lock = {
            let mut g = locks().lock().unwrap();
            g.entry(k)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = match cancel {
            Some(c) => {
                // 预检查：已取消的 token 直接拒绝。不能只靠 select 竞速——锁无竞争时两个
                // 分支同时就绪，select 默认随机选分支，会偶发拿到锁（pre_cancelled 抖动根因）
                if c.is_cancelled() {
                    return Err(c.clone());
                }
                tokio::select! {
                    g = lock.lock_owned() => g,
                    _ = c.cancelled() => return Err(c.clone()),
                }
            }
            None => lock.lock_owned().await,
        };
        guards.push(guard);
    }
    Ok(guards)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 并发获取不同路径不得互相阻塞（死锁回归）。
    #[tokio::test]
    async fn different_paths_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        let _ga = acquire_all(std::slice::from_ref(&a), None).await.unwrap();
        let gb =
            tokio::time::timeout(std::time::Duration::from_secs(2), acquire_all(&[b], None)).await;
        assert!(gb.is_ok(), "不同路径不应被彼此阻塞");
    }

    /// 同一路径的二次获取必须等待第一个 guard 释放。
    #[tokio::test]
    async fn same_path_second_acquire_waits() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("s.txt");
        std::fs::write(&a, "").unwrap();
        let _g1 = acquire_all(std::slice::from_ref(&a), None).await.unwrap();
        let try_second = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            acquire_all(&[a], None),
        )
        .await;
        assert!(
            try_second.is_err(),
            "同路径持锁期间二次获取应等待而非立即返回"
        );
    }

    /// 取消盲区修复：等待被持有锁期间 cancel 置位 → 立即返回 Err，不再无限挂起。
    #[tokio::test]
    async fn cancelled_while_waiting_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("c.txt");
        std::fs::write(&a, "").unwrap();
        let _g1 = acquire_all(std::slice::from_ref(&a), None).await.unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        let t = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            t.cancel();
        });
        let started = std::time::Instant::now();
        let res = acquire_all(std::slice::from_ref(&a), Some(&token)).await;
        assert!(res.is_err(), "取消后应返回 Err 而非拿到锁");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "取消应立即返回而非等待锁释放"
        );
    }

    /// cancel 已置位时同样立即拒绝。
    #[tokio::test]
    async fn pre_cancelled_returns_err_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("d.txt");
        std::fs::write(&a, "").unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let res = acquire_all(std::slice::from_ref(&a), Some(&token)).await;
        assert!(res.is_err(), "已取消的 token 应直接拒绝获取");
    }
}
