//! C0 transaction：跨文件的**全有或全无**共享写。
//!
//! 单文件 `rename` 是原子的，多文件不是。所以顺序必须是：
//!
//! ```text
//! 1. 校验全部写（任一不合法 → 一条都不做）
//! 2. 全部落到同目录临时文件并 sync
//! 3. 写**意向日志**（temp → final 映射）并 sync ← 这一刻起事务"已决定"
//! 4. 逐个 rename
//! 5. 删日志
//! ```
//!
//! 崩在第 4 步中间时，磁盘上确实只生效了一部分——但日志还在，恢复时**向前滚动**
//! 把剩下的补完。之所以选滚动而不是回滚：回滚需要每个目标的旧值副本，
//! 而共享写可能被**另一个进程**合法地改过，按旧值回滚会覆盖别人的新数据。
//! 向前滚动只会把本事务自己写的内容落到位，不会踩到别人的写入。
//!
//! 没有日志却留下临时文件 = 事务从未"被决定"，直接丢弃临时文件。

use super::owner::io_err;
use crate::core::coordinator::CoordinateError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一条待落地的写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteIntent {
    /// 目标绝对路径。
    pub path: String,
    /// 临时文件绝对路径（与目标同目录）。
    pub temp: String,
}

/// 落盘的事务意向日志。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// 事务 id（也用于日志文件名）。
    pub tx_id: String,
    /// 关联租约代次。
    pub generation: u64,
    pub intents: Vec<WriteIntent>,
}

/// 日志目录管理器。
#[derive(Debug, Clone)]
pub struct JournalStore {
    root: PathBuf,
}

/// 向前滚动的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollForward {
    /// 本次补完的 rename 数。
    pub renamed: usize,
    /// 崩溃前已 rename 过的条数。
    pub already_applied: usize,
}

/// 校验并准备写集。**任一条不合法即整体拒绝**，且不产生任何临时文件。
pub fn plan_writes(writes: &[(String, String)]) -> Result<Vec<WriteIntent>, CoordinateError> {
    if writes.is_empty() {
        return Err(CoordinateError::new(
            "empty_transaction",
            "事务没有任何写入：整体不生效",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(writes.len());
    for (path, _) in writes {
        let p = Path::new(path);
        if path.trim().is_empty() {
            return Err(CoordinateError::new(
                "invalid_write",
                "共享写路径为空：事务整体不生效",
            ));
        }
        if !p.is_absolute() {
            return Err(CoordinateError::new(
                "invalid_write",
                format!("共享写路径必须是绝对路径：{path}"),
            ));
        }
        if !seen.insert(p) {
            return Err(CoordinateError::new(
                "duplicate_write",
                format!("同一路径在事务里出现两次：{path}"),
            ));
        }
        out.push(WriteIntent {
            path: path.clone(),
            temp: super::tmp_sibling(p).to_string_lossy().to_string(),
        });
    }
    Ok(out)
}

/// 事务引擎。
#[derive(Debug, Clone)]
pub struct TransactionEngine {
    journal: JournalStore,
}

/// 暂存阶段的"待提交"标记。
///
/// 必要性：暂存临时文件**必须**与目标同目录（rename 才不跨文件系统），
/// 于是它们散落在目标目录里，只靠扫协调根是找不到的。解法是 stage 之前
/// 先把意图列表登记进 `<coord>/pending/`——没有 pending 也没有 journal 的事务，
/// 就是**从未被决定**的，恢复时可以把它的临时文件一并丢掉。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub tx_id: String,
    pub intents: Vec<WriteIntent>,
}

impl TransactionEngine {
    pub fn new(coord_root: &Path) -> Self {
        TransactionEngine {
            journal: JournalStore::new(coord_root),
        }
    }

    fn pending_path(&self, tx_id: &str) -> PathBuf {
        self.journal
            .root()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("pending")
            .join(format!("{}.json", super::key_digest(tx_id)))
    }

    fn write_pending(&self, tx_id: &str, intents: &[WriteIntent]) -> Result<(), CoordinateError> {
        let p = self.pending_path(tx_id);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let bytes = serde_json::to_vec(&Pending {
            tx_id: tx_id.to_string(),
            intents: intents.to_vec(),
        })
        .map_err(|e| {
            CoordinateError::new(
                "pending_encode_failed",
                format!("待提交标记序列化失败：{e}"),
            )
        })?;
        super::atomic_write(&p, &bytes).map_err(io_err)
    }

    fn clear_pending(&self, tx_id: &str) {
        let _ = std::fs::remove_file(self.pending_path(tx_id));
    }

    fn list_pending(&self) -> Vec<Pending> {
        let dir = self
            .pending_path("")
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            if let Ok(bytes) = std::fs::read(e.path())
                && let Ok(p) = serde_json::from_slice::<Pending>(&bytes)
            {
                out.push(p);
            }
        }
        out.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));
        out
    }
}

impl TransactionEngine {
    /// 第二阶段：登记 pending 标记 → 全部写临时文件。任一失败则**清掉已写的临时文件**并整体失败。
    ///
    /// pending 标记写在这里而不是 `commit` 里：暂存过的临时文件**必须**是可找回的，
    /// 否则「stage 之后、journal 之前」崩掉就只剩一堆没人认领的垃圾。
    pub fn stage(
        &self,
        tx_id: &str,
        intents: &[WriteIntent],
        contents: &[String],
    ) -> Result<(), CoordinateError> {
        use std::io::Write;
        if intents.len() != contents.len() {
            return Err(CoordinateError::new(
                "invalid_write",
                "写集与内容条数不一致：事务整体不生效",
            ));
        }
        self.write_pending(tx_id, intents)?;
        let mut staged: Vec<PathBuf> = Vec::new();
        for (intent, body) in intents.iter().zip(contents) {
            let temp = PathBuf::from(&intent.temp);
            if let Some(parent) = temp.parent() {
                std::fs::create_dir_all(parent).map_err(io_err)?;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .and_then(|mut f| f.write_all(body.as_bytes()).and_then(|()| f.sync_all()))
            {
                Ok(()) => staged.push(temp),
                Err(e) => {
                    discard(&staged);
                    self.clear_pending(tx_id);
                    return Err(io_err(e));
                }
            }
        }
        Ok(())
    }

    /// 第三阶段：写意向日志（**持久化决定**）。
    pub fn record_intent(
        &self,
        tx_id: &str,
        generation: u64,
        intents: &[WriteIntent],
    ) -> Result<(), CoordinateError> {
        self.journal.write(&Journal {
            tx_id: tx_id.to_string(),
            generation,
            intents: intents.to_vec(),
        })
    }

    /// 第四阶段：逐个 rename。**允许部分完成**——日志已持久化，恢复会补完。
    pub fn apply(&self, intents: &[WriteIntent]) -> Result<RollForward, CoordinateError> {
        let mut out = RollForward {
            renamed: 0,
            already_applied: 0,
        };
        for i in intents {
            let temp = PathBuf::from(&i.temp);
            let target = PathBuf::from(&i.path);
            if !temp.exists() {
                if target.exists() {
                    out.already_applied += 1;
                    continue;
                }
                return Err(CoordinateError::new(
                    "transaction_data_lost",
                    format!("临时文件与目标都不存在，无法判定事务是否生效：{}", i.path),
                ));
            }
            std::fs::rename(&temp, &target).map_err(io_err)?;
            out.renamed += 1;
        }
        Ok(out)
    }

    /// 第五阶段：删日志。日志消失 = 事务已完整生效。
    pub fn seal(&self, tx_id: &str) -> Result<(), CoordinateError> {
        self.journal.remove(tx_id)
    }

    /// 端到端提交：staged(pending) → 日志 → apply → seal。返回已生效条数。
    pub fn commit(
        &self,
        tx_id: &str,
        generation: u64,
        writes: &[(String, String)],
    ) -> Result<RollForward, CoordinateError> {
        let _lock = super::DiskLock::acquire(
            &self
                .journal
                .root
                .parent()
                .unwrap()
                .join("transactions.lock"),
        )
        .map_err(io_err)?;
        let intents = plan_writes(writes)?;
        let contents: Vec<String> = writes.iter().map(|(_, b)| b.clone()).collect();
        self.stage(tx_id, &intents, &contents)?;
        if let Err(e) = self.record_intent(tx_id, generation, &intents) {
            // 日志没落盘 = 事务从未被决定：临时文件必须丢，不能留着等人误读。
            discard(
                &intents
                    .iter()
                    .map(|i| PathBuf::from(&i.temp))
                    .collect::<Vec<_>>(),
            );
            self.clear_pending(tx_id);
            return Err(e);
        }
        // 日志已持久化 → 事务已被决定，pending 的使命完成（崩溃时由 journal 接手）。
        self.clear_pending(tx_id);
        let r = self.apply(&intents)?;
        self.seal(tx_id)?;
        Ok(r)
    }

    /// 恢复：把已决定但未完成的事务**向前滚动**，丢弃从未被决定的暂存。
    ///
    /// **只恢复状态，不执行任何工具、不重放副作用**——这是 #36 的核心不变量。
    pub fn recover(&self) -> Result<Vec<RollForward>, CoordinateError> {
        let _lock = super::DiskLock::acquire(
            &self
                .journal
                .root
                .parent()
                .unwrap()
                .join("transactions.lock"),
        )
        .map_err(io_err)?;
        let decided: std::collections::BTreeSet<String> =
            self.journal.list()?.into_iter().map(|j| j.tx_id).collect();
        let mut done = Vec::new();
        for j in self.journal.list()? {
            let r = self.apply(&j.intents)?;
            self.journal.remove(&j.tx_id)?;
            done.push(r);
        }
        // 没有对应日志的 pending = 从未被决定：连同它的临时文件一起丢。
        for p in self.list_pending() {
            if decided.contains(&p.tx_id) {
                continue;
            }
            for i in &p.intents {
                let tmp = PathBuf::from(&i.temp);
                if let Err(e) = std::fs::remove_file(&tmp)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!("丢弃未决定事务的临时文件失败 {}：{e}", tmp.display());
                }
            }
            self.clear_pending(&p.tx_id);
        }
        self.journal.discard_orphan_temps()?;
        Ok(done)
    }

    pub fn journal(&self) -> &JournalStore {
        &self.journal
    }
}

/// 删除临时文件。回滚路径上的清理失败只记日志：清理失败不该把整体变成失败
/// （内容还没落地，失败结论是明确的），但必须留下痕迹。
fn discard(paths: &[PathBuf]) {
    for p in paths {
        if let Err(e) = std::fs::remove_file(p) {
            tracing::warn!("回滚清理临时文件失败 {}：{e}", p.display());
        }
    }
}

impl JournalStore {
    pub fn new(coord_root: &Path) -> Self {
        JournalStore {
            root: coord_root.join("journal"),
        }
    }

    fn path_for(&self, tx_id: &str) -> PathBuf {
        self.root.join(format!("{}.json", super::key_digest(tx_id)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, j: &Journal) -> Result<(), CoordinateError> {
        let bytes = serde_json::to_vec(j).map_err(|e| {
            CoordinateError::new("journal_encode_failed", format!("事务日志序列化失败：{e}"))
        })?;
        // 必须"日志先落盘，再开始 rename"：顺序反了滚动就无从谈起。
        super::atomic_write(&self.path_for(&j.tx_id), &bytes).map_err(io_err)
    }

    pub fn read(&self, tx_id: &str) -> Option<Journal> {
        let bytes = std::fs::read(self.path_for(tx_id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn remove(&self, tx_id: &str) -> Result<(), CoordinateError> {
        match std::fs::remove_file(self.path_for(tx_id)) {
            Ok(()) => Ok(()),
            // 已被前一次恢复清掉：幂等。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    pub fn list(&self) -> Result<Vec<Journal>, CoordinateError> {
        let rd = match std::fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(e)),
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let Ok(bytes) = std::fs::read(e.path()) else {
                // 读不出来的日志**不能**当不存在：它可能正对应一次已决定的事务。
                return Err(CoordinateError::new(
                    "journal_unreadable",
                    format!("事务日志不可读：{}（保守不推进）", e.path().display()),
                ));
            };
            let j: Journal = serde_json::from_slice(&bytes).map_err(|err| {
                CoordinateError::new(
                    "journal_unreadable",
                    format!("事务日志解析失败：{}（{err}）", e.path().display()),
                )
            })?;
            out.push(j);
        }
        out.sort_by(|a, b| {
            a.generation
                .cmp(&b.generation)
                .then_with(|| a.tx_id.cmp(&b.tx_id))
        });
        Ok(out)
    }

    /// 清理不属于任何日志的 `.cw-tmp-*`（事务从未被决定）。
    ///
    /// 只扫协调根下我们自己建的子树，**绝不做目录树通配删除**。
    pub fn discard_orphan_temps(&self) -> Result<(), CoordinateError> {
        self.discard_orphan_temps_in(&self.root.clone())
    }

    fn discard_orphan_temps_in(&self, dir: &Path) -> Result<(), CoordinateError> {
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err(e)),
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.discard_orphan_temps_in(&p)?;
                continue;
            }
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with(".cw-tmp-")
                && let Err(err) = std::fs::remove_file(&p)
            {
                tracing::warn!("清理孤儿临时文件失败 {}：{err}", p.display());
            }
        }
        Ok(())
    }
}
