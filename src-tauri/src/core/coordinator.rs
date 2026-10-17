//! #36 跨进程协调契约（L0：trait 与身份规则；C0 具体实现另计）。
//!
//! 本模块只定义**接口与不变量**，不实现共享写。规格要求明确：C0 未实接前
//! 只允许隔离测试数据根，**不开放真实共享目录**——所以这里没有生产实现。
//!
//! 不变量：
//!
//! 1. **租约有 generation**。释放必须校验 generation：迟到完成不得释放后继持有者的租约。
//! 2. **事务要么整体生效要么整体不生效**。部分生效的共享写无法回滚，等于没有隔离。
//! 3. **恢复不重跑工具**。重启只恢复显示与中断状态，不执行历史工具、不恢复旧审批。
//! 4. **撤销立即生效**。权限撤销按既有 step 语义在下一次检查点生效，不等当前 run 结束。

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 协调器（跨进程共享资源的所有权与事务）。
///
/// trait 对象里**不含 Tauri 类型**，也不允许模型 / 普通工具构造替换。
pub trait Coordinator: Send + Sync {
    /// 申请租约。
    fn acquire(&self, req: &AcquireRequest) -> Result<Lease, CoordinateError>;

    /// 释放租约（generation 必须匹配）。
    fn release(&self, lease: &Lease) -> Result<(), CoordinateError>;

    /// 提交事务（整体生效或整体不生效）。
    fn commit(&self, tx: &SharedTransaction) -> Result<TxReceipt, CoordinateError>;

    /// 恢复：返回当前仍有效的租约（**不执行任何工具**）。
    fn recover(&self, session_id: &str) -> Result<Vec<Lease>, CoordinateError>;
}

/// 协调错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinateError {
    /// 稳定码。
    pub code: String,
    /// 说明。
    pub message: String,
}

impl std::fmt::Display for CoordinateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for CoordinateError {}

impl CoordinateError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        CoordinateError {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// 租约申请。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcquireRequest {
    /// 会话。
    pub session_id: String,
    /// root run。
    pub root_run_id: String,
    /// 资源键（规范化后的真实路径或服务标识）。
    pub resource: String,
    /// attempt。
    pub attempt: u32,
    /// 客户端自报的期望代次（`None` = 不校验，仅首次申请）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<u64>,
}

/// 租约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub session_id: String,
    pub root_run_id: String,
    pub resource: String,
    pub attempt: u32,
    pub generation: u64,
}

/// 共享事务。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedTransaction {
    pub lease: Lease,
    /// 要写入的内容（**已由既有审批门放行**；本层不替代审批）。
    pub writes: Vec<SharedWrite>,
}

/// 一条共享写。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedWrite {
    /// 规范化后的绝对路径。
    pub path: String,
    pub contents: String,
}

/// 事务回执。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TxReceipt {
    pub generation: u64,
    /// 已生效的写入数（与申请一致；部分生效即视为失败，不会返回这种回执）。
    pub applied: usize,
}

/// 单进程内存适配（**仅测试与隔离夹具**）。
///
/// 明确不具备跨进程语义：C0 未实接前它**不得**用于真实共享目录。
#[derive(Default)]
pub struct InProcessCoordinator {
    inner: std::sync::Mutex<Inner>,
    next_generation: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct Inner {
    by_resource: std::collections::BTreeMap<String, Lease>,
    committed: Vec<TxReceipt>,
}

impl InProcessCoordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 已提交事务数（恢复测试用）。
    pub fn committed_count(&self) -> usize {
        self.inner.lock().unwrap().committed.len()
    }
}

impl Coordinator for InProcessCoordinator {
    fn acquire(&self, req: &AcquireRequest) -> Result<Lease, CoordinateError> {
        let mut t = self.inner.lock().unwrap();
        if let Some(existing) = t.by_resource.get(&req.resource) {
            let same_holder =
                existing.root_run_id == req.root_run_id && existing.attempt == req.attempt;
            if !same_holder {
                return Err(CoordinateError::new(
                    "resource_held",
                    format!(
                        "资源 `{}` 已由 {}（attempt {}，generation {}）持有",
                        req.resource, existing.root_run_id, existing.attempt, existing.generation
                    ),
                ));
            }
            // 同持有者重复申请幂等。
            return Ok(existing.clone());
        }
        if let Some(exp) = req.expected_generation {
            let cur = self
                .next_generation
                .load(std::sync::atomic::Ordering::SeqCst);
            if cur != exp {
                return Err(CoordinateError::new(
                    "stale_generation",
                    format!("申请基于代次 {exp}，当前为 {cur}"),
                ));
            }
        }
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let lease = Lease {
            session_id: req.session_id.clone(),
            root_run_id: req.root_run_id.clone(),
            resource: req.resource.clone(),
            attempt: req.attempt,
            generation,
        };
        t.by_resource.insert(req.resource.clone(), lease.clone());
        Ok(lease)
    }

    fn release(&self, lease: &Lease) -> Result<(), CoordinateError> {
        let mut t = self.inner.lock().unwrap();
        match t.by_resource.get(&lease.resource) {
            Some(cur) if cur.generation == lease.generation => {
                t.by_resource.remove(&lease.resource);
                Ok(())
            }
            Some(cur) => Err(CoordinateError::new(
                "generation_mismatch",
                format!(
                    "租约代次为 {}，收到 {}：迟到完成不得释放后继持有者的租约",
                    cur.generation, lease.generation
                ),
            )),
            None => Err(CoordinateError::new("no_lease", "租约不存在或已被释放")),
        }
    }

    fn commit(&self, tx: &SharedTransaction) -> Result<TxReceipt, CoordinateError> {
        let mut t = self.inner.lock().unwrap();
        // 校验租约仍然有效（提交期间被抢走则整体失败）。
        match t.by_resource.get(&tx.lease.resource) {
            Some(cur) if cur.generation == tx.lease.generation => {}
            _ => {
                return Err(CoordinateError::new(
                    "stale_lease",
                    "提交时租约已失效：事务整体不生效",
                ));
            }
        }
        // 全查后全写：任一条不合法则**一条都不写**（部分生效无法回滚）。
        for w in &tx.writes {
            if w.path.trim().is_empty() {
                return Err(CoordinateError::new(
                    "invalid_write",
                    "共享写路径为空：事务整体不生效",
                ));
            }
        }
        let receipt = TxReceipt {
            generation: tx.lease.generation,
            applied: tx.writes.len(),
        };
        t.committed.push(receipt.clone());
        Ok(receipt)
    }

    fn recover(&self, session_id: &str) -> Result<Vec<Lease>, CoordinateError> {
        let t = self.inner.lock().unwrap();
        Ok(t.by_resource
            .values()
            .filter(|l| l.session_id == session_id)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
