//! C0 owner：进程 / 会话 / canonical 根的租约存储。
//!
//! 落盘形态是**一个文件一个租约**，申请走 `create_new`（O_CREAT|O_EXCL，跨平台原子）：
//! 并发申请里**恰好一个**成功，其余拿到 `AlreadyExists` 后读回真实持有者。
//!
//! ## 代次为什么不能"读一下加一"
//!
//! 代次必须跨进程单调，而 `read → +1 → write` 之间有窗口：两个进程能读到同一个值，
//! 都加一、写同一个代次，于是两个持有者拿着**相同 generation**，
//! "释放必须校验 generation"这条不变量当场失效——后者的迟到释放会放掉前者。
//! 所以代次由一把 test-and-set 锁串行分配（[`LeaseStore::next_generation`]）。

use super::atomic_write;
use crate::core::coordinator::{AcquireRequest, CoordinateError, Lease};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 落盘的租约记录。
///
/// 比 [`Lease`] 多两样：进程身份与取得时刻。
/// 它们**不参与**判活（见模块文档第 2 条：崩溃保守 busy），
/// 只用于让另一个进程在错误消息与 guardian 报告里能说清"是谁占着"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseRecord {
    #[serde(flatten)]
    pub lease: Lease,
    /// 持有进程 id。
    pub pid: u32,
    /// Unix 毫秒时间戳。
    pub acquired_at_ms: u64,
}

impl LeaseRecord {
    pub fn from_lease(lease: Lease) -> Self {
        LeaseRecord {
            lease,
            pid: std::process::id(),
            acquired_at_ms: now_ms(),
        }
    }
}

/// 租约存储（`<data_dir>/coord/leases/<digest>.json`）。
#[derive(Debug, Clone)]
pub struct LeaseStore {
    root: PathBuf,
}

impl LeaseStore {
    pub fn new(coord_root: &Path) -> Self {
        LeaseStore {
            root: coord_root.to_path_buf(),
        }
    }

    fn lease_path(&self, resource: &str) -> PathBuf {
        self.root
            .join("leases")
            .join(format!("{}.json", super::key_digest(resource)))
    }

    fn gen_path(&self) -> PathBuf {
        self.root.join("generation")
    }

    fn gen_lock_path(&self) -> PathBuf {
        self.root.join("generation.lock")
    }

    /// 读当前代次（缺失 = 0）。
    fn read_generation(&self) -> u64 {
        std::fs::read_to_string(self.gen_path())
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// 跨进程单调地取下一个代次。
    ///
    /// test-and-set：`generation.lock` 的 `create_new` 成功者独占递增，
    /// 读-改-写全程在临界区内。别进程自旋重试。取不到就报争用。
    pub fn next_generation(&self) -> Result<u64, CoordinateError> {
        std::fs::create_dir_all(&self.root).map_err(io_err)?;
        let _lock = super::DiskLock::acquire(&self.gen_lock_path()).map_err(io_err)?;
        let next = self
            .read_generation()
            .checked_add(1)
            .ok_or_else(|| CoordinateError::new("generation_exhausted", "租约代次耗尽"))?;
        atomic_write(&self.gen_path(), next.to_string().as_bytes()).map_err(io_err)?;
        Ok(next)
    }

    /// 申请租约。
    ///
    /// - 空闲：取新代次并 `create_new` 落盘。
    /// - 同持有者重复申请：**幂等**返回现有租约（重试不该换掉自己的代次）。
    /// - 他人持有：`resource_held`。**不做**基于时间的自动回收。
    pub fn acquire(&self, req: &AcquireRequest) -> Result<Lease, CoordinateError> {
        let _gate = super::DiskLock::acquire(
            &self
                .root
                .join("owner-locks")
                .join(format!("{}.lock", super::key_digest(&req.resource))),
        )
        .map_err(io_err)?;
        let path = self.lease_path(&req.resource);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(io_err)?;

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                // 抢到了：此时必须把代次**写进去**，否则文件存在但内容不可解析。
                let generation = match self.next_generation() {
                    Ok(g) => g,
                    Err(e) => {
                        drop(f);
                        let _ = std::fs::remove_file(&path);
                        return Err(e);
                    }
                };
                let lease = Lease {
                    session_id: req.session_id.clone(),
                    root_run_id: req.root_run_id.clone(),
                    resource: req.resource.clone(),
                    attempt: req.attempt,
                    generation,
                };
                let rec = LeaseRecord::from_lease(lease.clone());
                let bytes = serde_json::to_vec(&rec).map_err(|e| {
                    CoordinateError::new("lease_encode_failed", format!("租约序列化失败：{e}"))
                })?;
                use std::io::Write;
                if let Err(e) = f.write_all(&bytes).and_then(|()| f.sync_all()) {
                    // 写坏了必须把占位撤掉，否则资源被一个空文件永久占死。
                    drop(f);
                    let _ = std::fs::remove_file(&path);
                    return Err(io_err(e));
                }
                return Ok(lease);
            }
            // 已被占用：往下读真实持有者。
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(e)),
        }

        // 读不回来 ≠ 没锁。放行等于把别人的资源当空闲抢走。
        let current = self.read(&req.resource).ok_or_else(|| {
            CoordinateError::new(
                "lease_unreadable",
                format!("租约文件 {} 存在但读不出来：保守按占用处理", path.display()),
            )
        })?;
        let same_holder = current.pid == std::process::id()
            && current.lease.root_run_id == req.root_run_id
            && current.lease.attempt == req.attempt
            && current.lease.session_id == req.session_id;
        if same_holder {
            return Ok(current.lease);
        }
        if let Some(exp) = req.expected_generation
            && exp != current.lease.generation
        {
            return Err(CoordinateError::new(
                "stale_generation",
                format!(
                    "申请基于代次 {exp}，磁盘上为 {}：租约已被后继持有者取得",
                    current.lease.generation
                ),
            ));
        }
        Err(CoordinateError::new(
            "resource_held",
            format!(
                "资源 `{}` 已由会话 {} / run {}（attempt {}，generation {}，pid {}）持有",
                req.resource,
                current.lease.session_id,
                current.lease.root_run_id,
                current.lease.attempt,
                current.lease.generation,
                current.pid
            ),
        ))
    }

    /// 读当前租约。
    pub fn read(&self, resource: &str) -> Option<LeaseRecord> {
        let bytes = std::fs::read(self.lease_path(resource)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 释放租约（generation 必须匹配）。
    ///
    /// 「读回校验 → 删除」之间不存在窗口：文件还在，别人**申请不到**新代次。
    pub fn release(&self, lease: &Lease) -> Result<(), CoordinateError> {
        let _gate = super::DiskLock::acquire(
            &self
                .root
                .join("owner-locks")
                .join(format!("{}.lock", super::key_digest(&lease.resource))),
        )
        .map_err(io_err)?;
        match self.read(&lease.resource) {
            Some(cur) if cur.lease.generation == lease.generation => {
                std::fs::remove_file(self.lease_path(&lease.resource)).map_err(io_err)?;
                Ok(())
            }
            Some(cur) => Err(CoordinateError::new(
                "generation_mismatch",
                format!(
                    "租约代次为 {}，收到 {}：迟到完成不得释放后继持有者的租约",
                    cur.lease.generation, lease.generation
                ),
            )),
            None => Err(CoordinateError::new("no_lease", "租约不存在或已被释放")),
        }
    }

    /// 该会话仍持有的全部租约（**恢复用**：只报告状态，不执行任何工具）。
    pub fn leases_of(&self, session_id: &str) -> Vec<LeaseRecord> {
        let dir = self.root.join("leases");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let Ok(bytes) = std::fs::read(e.path()) else {
                continue;
            };
            if let Ok(rec) = serde_json::from_slice::<LeaseRecord>(&bytes)
                && rec.lease.session_id == session_id
            {
                out.push(rec);
            }
        }
        // 按代次排序：恢复报告要有稳定顺序，不能靠目录遍历顺序。
        out.sort_by_key(|r| r.lease.generation);
        out
    }

    /// 全部租约（guardian 巡检用）。
    pub fn all_leases(&self) -> Vec<LeaseRecord> {
        let dir = self.root.join("leases");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            if let Ok(bytes) = std::fs::read(e.path())
                && let Ok(rec) = serde_json::from_slice::<LeaseRecord>(&bytes)
            {
                out.push(rec);
            }
        }
        out.sort_by_key(|r| r.lease.generation);
        out
    }

    /// guardian 显式回收：调用方**必须**已确认持有者确实退出。
    ///
    /// 这里刻意不按 PID 判活自动调用——PID 复用会把别人的租约误判成陈旧的。
    pub fn force_release(&self, resource: &str, reason: &str) -> Result<Lease, CoordinateError> {
        let _gate = super::DiskLock::acquire(
            &self
                .root
                .join("owner-locks")
                .join(format!("{}.lock", super::key_digest(resource))),
        )
        .map_err(io_err)?;
        match self.read(resource) {
            Some(cur) => {
                std::fs::remove_file(self.lease_path(resource)).map_err(io_err)?;
                tracing::warn!(
                    "guardian 回收租约 `{}`（原 pid {}，generation {}）：{reason}",
                    resource,
                    cur.pid,
                    cur.lease.generation
                );
                Ok(cur.lease)
            }
            None => Err(CoordinateError::new("no_lease", "租约不存在或已被释放")),
        }
    }

    pub fn coord_root(&self) -> &Path {
        &self.root
    }
}

pub(crate) fn io_err(e: std::io::Error) -> CoordinateError {
    CoordinateError::new("coord_io", format!("协调 IO 失败：{e}"))
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
