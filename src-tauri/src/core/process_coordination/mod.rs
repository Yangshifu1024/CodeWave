//! #36-C0 跨进程共享协调（owner / transaction / workspace / process guardian）。
//!
//! 规格要求明确：**C0 未实接前只允许隔离测试数据根，不开放真实共享写**。
//! 本模块是那份"实接"：真跨进程语义，靠 `create_new`（O_CREAT|O_EXCL，跨平台原子）
//! 与临时文件 + rename 落地，不依赖 advisory 文件锁——advisory 锁只保护合作实例，
//! 对"不遵守锁的旧进程"无能为力，而那种进程必须被**检测到**而不是被假装挡住。
//!
//! ## 锁序（写错就是死锁，且死锁在跨进程下表现为"两边都卡住"）
//!
//! ```text
//! 短生命周期锁 → owner try
//! 保存路径：owner → save_lock → 磁盘事务 → index_lock
//! ```
//!
//! - `config` / `MCP` / `stats` 的事务**不与 history 事务嵌套**。
//! - **不持锁等模型 / 用户 / 网络**。本模块所有临界区都是纯文件 IO，没有 await。
//!
//! ## 三条不许被"优化"掉的语义
//!
//! 1. **租约带 generation，释放必须校验**。迟到完成不得释放后继持有者的租约。
//! 2. **崩溃后保守 busy**。崩溃 / PID 复用 / 删除中断一律视为仍被占用——
//!    按 PID 判活再自动回收，会在 PID 复用时把**别人的**租约当成陈旧的抢走。
//!    回收只能由 guardian 在确认持有者确实退出后显式发起。
//! 3. **事务要么整体生效要么整体不生效**。两阶段 + 持久化意向日志 + **向前滚动**：
//!    崩在 rename 中途时，恢复把事务**补完**，而不是留下写一半的共享状态。

use std::path::{Path, PathBuf};

pub mod guardian;
pub mod owner;
pub mod transaction;
pub mod workspace;

pub use owner::LeaseStore;
pub use workspace::{FileCoordinator, ProcessGuardian, WorkspaceGuard};

/// 进程启动时**向前滚动**未完成的共享事务。
///
/// 崩在 rename 中途的共享写必须被补完，而不是留一半让下一个进程读到自相矛盾的共享状态。
/// 这是桌面启动和共享存储 调用的**唯一**入口——写成同一个函数而不是
/// 各处各调一次 `FileCoordinator::new(..).recover_transactions()`，正是为了避免两边日后
/// 对「协调根是数据根还是协调根」产生分歧（那次分歧曾让每一次提交都变成 `stale_lease`）。
///
/// 参数是**数据根**（不是协调根）：[`FileCoordinator::new`] 会自己拼 `coord/`。
pub fn recover_shared_transactions(
    data_dir: &Path,
) -> Result<Vec<transaction::RollForward>, crate::core::coordinator::CoordinateError> {
    FileCoordinator::new(data_dir).recover_transactions()
}

/// 原子写：临时文件 → rename。
///
/// 不用 `fs::write` 直写目标：直写在断电/崩溃下会留下半截文件，
/// 而共享状态被读到半截文件比读到旧值糟糕得多。
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = tmp_sibling(path);
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// 与目标同目录的临时文件名——同目录才能保证 rename 不跨文件系统。
pub(crate) fn tmp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "out".into());
    let unique = next_local_tag();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".cw-tmp-{}-{unique}-{}-{name}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// 进程内单调标签，用于让同一进程内的临时名互不相同。
///
/// **不参与跨进程语义**——跨进程唯一性由 `create_new` 提供，这里只是避免自己和自己撞名。
pub(crate) fn next_local_tag() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static T: AtomicU64 = AtomicU64::new(0);
    let n = T.fetch_add(1, Ordering::SeqCst);
    // 掺入 pid：同 pid 不同进程不共享这个静态量，掺入后便于事后辨认归属。
    n
}

/// 资源键 → 稳定的短文件名。
///
/// 路径可能很长且含分隔符，不能直接拼文件名。用内容哈希（`sha2` 已是既有依赖），
/// 与图片 blob 目录同样的取法（前 32 位十六进制）。
pub(crate) fn key_digest(resource: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(resource.as_bytes());
    let out = h.finalize();
    out.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Cross-process short critical section. The OS releases the lock on process death;
/// the lock file itself is never unlinked (unlinking permits two inode owners).
pub struct DiskLock(std::fs::File);
impl DiskLock {
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(Self(file))
    }
    pub fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}
impl Drop for DiskLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
