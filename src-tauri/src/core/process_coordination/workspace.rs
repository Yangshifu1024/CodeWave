//! C0 workspace：canonical 根租约与进程 guardian。
//!
//! ## 为什么租约键必须是 **canonical** 路径
//!
//! 非规范化的键等于没有锁：`/tmp/x`、`/tmp/./x`、`/tmp/y/../x` 是三条不同的键，
//! 两个进程各拿一把，**同时**写同一个目录。用 [`std::fs::canonicalize`] 取真实路径
//! 才让"同一个目录"在跨进程时是同一个键。canonicalize 失败（路径不存在）时
//! 退回词法规范化并**保留原样**，因为此时还不该凭猜测拒绝一个将来会存在的目录。
//!
//! ## guardian 的职责边界
//!
//! guardian **持有物理根锁直到自身管理的子树确认退出**。它不做的事：
//! 按 PID 判活自动回收租约（PID 复用会误杀）、删除用户代码目录、
//! 在持有根锁期间等待模型或网络。

use super::owner::LeaseStore;
use crate::core::coordinator::Coordinator;
use crate::core::coordinator::{AcquireRequest, CoordinateError, Lease};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 词法规范化：消 `.` / `..` / 重复分隔符。`canonicalize` 失败时的兜底。
pub fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for seg in p.components() {
        match seg {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // 只在有可回退的普通段时弹栈；`..` 出现在根部就保留。
                let popped = match out.components().next_back() {
                    Some(std::path::Component::Normal(_)) => out.pop(),
                    _ => false,
                };
                if !popped {
                    out.push(seg);
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 租约键：优先 canonical，失败退回词法规范化。
pub fn resource_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path));
    resolved.to_string_lossy().to_string()
}

/// canonical 根守卫：持有一把根租约，`Drop` 时释放。
///
/// `Drop` 只能做**尽力**释放：释放失败（进程正在被杀、磁盘错误）不会 panic，
/// 残留租约按模块文档的保守规则继续占用，由 guardian 显式处理。
pub struct WorkspaceGuard {
    store: LeaseStore,
    lease: Lease,
    guardian: Option<super::guardian::GuardianClient>,
    settlement: std::sync::Mutex<Option<Result<(), CoordinateError>>>,
}

impl WorkspaceGuard {
    /// Coordinate the physical workspace independently of the caller's profile.
    /// All cooperating desktop processes use this same canonical root.
    pub fn acquire_global(
        session: &str,
        run: &str,
        attempt: u32,
        root: &Path,
    ) -> Result<Self, CoordinateError> {
        let root = std::fs::canonicalize(root).map_err(super::owner::io_err)?;
        let guard = Self::acquire(
            &LeaseStore::new(&root.join(".codewave/coord")),
            session,
            run,
            attempt,
            &root,
        )?;
        #[cfg(not(test))]
        {
            let mut guard = guard;
            guard.guardian = Some(super::guardian::GuardianClient::launch(
                &guard.store,
                &guard.lease,
            )?);
            Ok(guard)
        }
        #[cfg(test)]
        Ok(guard)
    }

    pub fn acquire(
        store: &LeaseStore,
        session_id: &str,
        root_run_id: &str,
        attempt: u32,
        root: &Path,
    ) -> Result<Self, CoordinateError> {
        let key = resource_key(root);
        let lease = store.acquire(&AcquireRequest {
            session_id: session_id.to_string(),
            root_run_id: root_run_id.to_string(),
            resource: key,
            attempt,
            expected_generation: None,
        })?;
        Ok(WorkspaceGuard {
            store: store.clone(),
            lease,
            guardian: None,
            settlement: std::sync::Mutex::new(None),
        })
    }

    /// The command must still be passed through process_wrap so the wrapper owns
    /// its process group/job. Guardian authorizes it before the actual exec.
    pub fn guarded_invocation(
        &self,
        program: &str,
        argv: &[String],
    ) -> Result<(PathBuf, Vec<std::ffi::OsString>), CoordinateError> {
        if let Some(guardian) = &self.guardian {
            guardian.invocation(program, argv)
        } else {
            #[cfg(test)]
            {
                Ok((
                    PathBuf::from(program),
                    argv.iter().map(std::ffi::OsString::from).collect(),
                ))
            }
            #[cfg(not(test))]
            {
                Err(CoordinateError::new(
                    "guardian_unavailable",
                    "Cannot launch a command without the independent guardian",
                ))
            }
        }
    }
    pub fn lease(&self) -> &Lease {
        &self.lease
    }

    /// 显式释放（幂等：已释放不报错）。
    pub fn settle(&self) -> Result<(), CoordinateError> {
        let mut settled = self.settlement.lock().unwrap();
        if let Some(result) = settled.as_ref() {
            if result.is_err()
                && self
                    .guardian
                    .as_ref()
                    .is_some_and(|guardian| guardian.cleanup_proven())
            {
                *settled = Some(Ok(()));
                return Ok(());
            }
            return result.clone();
        }
        let result = if let Some(guardian) = &self.guardian {
            guardian.finish()
        } else {
            self.store.release(&self.lease)
        };
        let result = match result {
            Err(error) if error.code == "no_lease" => Ok(()),
            result => result,
        };
        *settled = Some(result.clone());
        result
    }
    pub fn release(mut self) -> Result<(), CoordinateError> {
        let result = self.settle();
        self.lease.generation = u64::MAX;
        result
    }
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        if self.lease.generation == u64::MAX {
            return;
        }
        if let Err(error) = self.settle() {
            tracing::error!("Guardian cleanup unconfirmed; root lease retained: {error}");
        }
    }
}

/// 进程 guardian：持有一组根租约直到自身退出。
///
/// 用途是"这个进程在管这棵子树"的**可证明事实**：另一个进程想写同一根时，
/// `acquire` 会拿到 `resource_held`，并从消息里看到占用者的 pid 与代次。
pub struct ProcessGuardian {
    store: LeaseStore,
    held: std::sync::Mutex<Vec<Lease>>,
    pid: u32,
}

impl ProcessGuardian {
    pub fn new(store: &LeaseStore) -> Arc<Self> {
        Arc::new(ProcessGuardian {
            store: store.clone(),
            held: std::sync::Mutex::new(Vec::new()),
            pid: std::process::id(),
        })
    }

    /// 认领一棵子树的根。
    pub fn guard_root(
        &self,
        session_id: &str,
        root_run_id: &str,
        attempt: u32,
        root: &Path,
    ) -> Result<Lease, CoordinateError> {
        let lease = self.store.acquire(&AcquireRequest {
            session_id: session_id.to_string(),
            root_run_id: root_run_id.to_string(),
            resource: resource_key(root),
            attempt,
            expected_generation: None,
        })?;
        if let Ok(mut h) = self.held.lock() {
            h.push(lease.clone());
        }
        Ok(lease)
    }

    pub fn held(&self) -> Vec<Lease> {
        self.held.lock().map(|h| h.clone()).unwrap_or_default()
    }

    /// 释放全部（进程退出前调用）。
    pub fn release_all(&self) -> Vec<CoordinateError> {
        let taken: Vec<Lease> = match self.held.lock() {
            Ok(mut h) => std::mem::take(&mut *h),
            Err(_) => return Vec::new(),
        };
        taken
            .into_iter()
            .filter_map(|l| match self.store.release(&l) {
                Ok(()) => None,
                // 已被别人回收：不是错误。
                Err(e) if e.code == "no_lease" => None,
                Err(e) => Some(e),
            })
            .collect()
    }

    /// 巡检：报告当前仍被占用的资源。
    pub fn busy_roots(&self) -> Vec<super::owner::LeaseRecord> {
        self.store.all_leases()
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

/// 真跨进程的 [`Coordinator`]：租约落盘、事务两阶段提交。
///
/// 这是 C0 的生产实现；[`crate::core::coordinator::InProcessCoordinator`]
/// 仍只用于隔离夹具。
#[derive(Debug, Clone)]
pub struct FileCoordinator {
    store: LeaseStore,
    engine: super::transaction::TransactionEngine,
}

impl FileCoordinator {
    /// 从**数据根**装配（协调根 = `<data_dir>/coord`）。
    ///
    /// 注意与 [`LeaseStore::new`] 的参数含义差一层：那个直接收**协调根**。
    /// 两处曾因此混用，导致子进程在 `root` 取租约、协调器在 `root/coord` 找租约，
    /// 提交一律 `stale_lease`。已经由 `协调根约定_两种入口不混用` 钉死。
    pub fn new(data_dir: &Path) -> Arc<Self> {
        Self::from_coord_root(&data_dir.join("coord"))
    }

    /// 从**协调根**装配。给已经持有 coord 根的调用方（如双进程夹具两侧）。
    pub fn from_coord_root(coord_root: &Path) -> Arc<Self> {
        Arc::new(FileCoordinator {
            store: LeaseStore::new(coord_root),
            engine: super::transaction::TransactionEngine::new(coord_root),
        })
    }

    pub fn store(&self) -> &LeaseStore {
        &self.store
    }

    pub fn engine(&self) -> &super::transaction::TransactionEngine {
        &self.engine
    }

    /// 启动恢复：向前滚动未完成事务。**不执行任何工具**。
    pub fn recover_transactions(
        &self,
    ) -> Result<Vec<super::transaction::RollForward>, CoordinateError> {
        self.engine.recover()
    }
}

impl Coordinator for FileCoordinator {
    fn acquire(&self, req: &AcquireRequest) -> Result<Lease, CoordinateError> {
        self.store.acquire(req)
    }

    fn release(&self, lease: &Lease) -> Result<(), CoordinateError> {
        self.store.release(lease)
    }

    fn commit(
        &self,
        tx: &crate::core::coordinator::SharedTransaction,
    ) -> Result<crate::core::coordinator::TxReceipt, CoordinateError> {
        // 租约仍有效才提交：提交期间被抢走则整体失败。
        match self.store.read(&tx.lease.resource) {
            Some(cur) if cur.lease.generation == tx.lease.generation => {}
            _ => {
                return Err(CoordinateError::new(
                    "stale_lease",
                    format!(
                        "提交时租约代次 {} 已失效：事务整体不生效",
                        tx.lease.generation
                    ),
                ));
            }
        }
        let writes: Vec<(String, String)> = tx
            .writes
            .iter()
            .map(|w| (w.path.clone(), w.contents.clone()))
            .collect();
        // tx_id 由资源 + 代次确定：同一租约的重复提交是同一笔事务，
        // 重放时不会因为 id 抖动而绕过日志。
        let tx_id = format!("{}@{}", tx.lease.resource, tx.lease.generation);
        let r = self.engine.commit(&tx_id, tx.lease.generation, &writes)?;
        Ok(crate::core::coordinator::TxReceipt {
            generation: tx.lease.generation,
            applied: r.renamed + r.already_applied,
        })
    }

    fn recover(&self, session_id: &str) -> Result<Vec<Lease>, CoordinateError> {
        Ok(self
            .store
            .leases_of(session_id)
            .into_iter()
            .map(|r| r.lease)
            .collect())
    }
}
