//! Fail-closed Office writes shared by tools and manual restore.
//! All source parsing is performed on one bounded snapshot while holding the
//! process-wide target lock. This is not a cross-process compare-and-swap.

use super::backup::{self, BackupReceipt};
use crate::tools::{
    pathutil::{self, WriteRoots},
    writelock,
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const RESTORE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct DocumentWriteError {
    pub code: &'static str,
    pub message: String,
}
impl DocumentWriteError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn outcome(self) -> crate::tools::ToolOutcome {
        crate::tools::ToolOutcome::err(self.code, self.message)
    }
}
type Result<T> = std::result::Result<T, DocumentWriteError>;

/// Owns the entire read/plan/build/backup/commit lock interval. Claims deliberately
/// have a different lifetime and are released only by the runtime drive guard.
pub struct LockedTarget {
    pub path: PathBuf,
    requested: PathBuf,
    roots: WriteRoots,
    _guards: Vec<tokio::sync::OwnedMutexGuard<()>>,
}
impl LockedTarget {
    pub async fn acquire(
        roots: &WriteRoots,
        requested: &Path,
        cancel: Option<&CancellationToken>,
    ) -> Result<Self> {
        let path = pathutil::canonical_best_effort(requested);
        let guards = writelock::acquire_all(std::slice::from_ref(&path), cancel)
            .await
            .map_err(|_| {
                DocumentWriteError::new(
                    "E_CANCELLED",
                    "操作已取消；目标未修改 / Cancelled before writing",
                )
            })?;
        let target = Self {
            path,
            requested: requested.to_path_buf(),
            roots: roots.clone(),
            _guards: guards,
        };
        target.validate()?;
        Ok(target)
    }

    /// Revalidate both the original alias and the canonical destination after
    /// locking and immediately before commit. Never silently follow a new alias.
    fn validate(&self) -> Result<()> {
        for candidate in [&self.requested, &self.path] {
            pathutil::resolve_write(&self.roots, &candidate.to_string_lossy()).map_err(
                |(code, msg)| {
                    DocumentWriteError::new(
                        if code == "E_PATH_OUTSIDE" {
                            "E_PATH_OUTSIDE"
                        } else {
                            "E_DOCUMENT_CHANGED"
                        },
                        msg,
                    )
                },
            )?;
            if pathutil::canonical_best_effort(candidate) != self.path {
                return Err(DocumentWriteError::new(
                    "E_DOCUMENT_CHANGED",
                    "目标路径已改变；请重新读取 / Target path changed; read it again",
                ));
            }
        }
        Ok(())
    }

    pub fn create_new(&self, bytes: &[u8], cancel: &CancellationToken) -> Result<()> {
        #[cfg(test)]
        super::faults::check(&self.path, "before_create")
            .map_err(|e| DocumentWriteError::new("E_IO", e.to_string()))?;
        self.validate()?;
        check_cancel(cancel)?;
        crate::util::atomic::atomic_create_new(&self.path, bytes).map_err(|e| DocumentWriteError::new(
            if e.kind() == std::io::ErrorKind::AlreadyExists { "E_EXISTS" } else { "E_IO" },
            format!("创建失败，未覆盖已有文件 / Creation failed without overwriting an existing file: {e}")))
    }
}

/// Snapshot ownership prevents subsequent parser calls from reopening a mutable
/// target. The temp is removed on error, cancellation, panic, or normal return.
pub struct PreparedDocument {
    pub source_digest: Vec<u8>,
    snapshot: tempfile::NamedTempFile,
}
impl PreparedDocument {
    pub fn staged_path(&self) -> &Path {
        self.snapshot.path()
    }
}

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    // Reject directories/devices/FIFOs before opening (a FIFO open can block).
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other("document exceeds 50 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(std::io::Error::other("document exceeds 50 MiB"));
    }
    Ok(bytes)
}
fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(DocumentWriteError::new(
            "E_CANCELLED",
            "操作已取消；目标未修改 / Cancelled before writing",
        ));
    }
    Ok(())
}

pub fn prepare_existing(
    target: &LockedTarget,
    data_dir: &Path,
    cancel: &CancellationToken,
) -> Result<PreparedDocument> {
    let started = std::time::Instant::now();
    check_cancel(cancel)?;
    target.validate()?;
    #[cfg(test)]
    super::faults::check(&target.path, "source_read").map_err(|e| {
        DocumentWriteError::new(
            "E_SOURCE_READ",
            format!("读取原件失败；目标未修改 / Source read failed: {e}"),
        )
    })?;
    let bytes = read_bounded(&target.path).map_err(|e| {
        DocumentWriteError::new(
            "E_SOURCE_READ",
            format!("读取原件失败；目标未修改 / Source read failed: {e}"),
        )
    })?;
    let source_digest = Sha256::digest(&bytes).to_vec();
    let stage = || -> std::io::Result<tempfile::NamedTempFile> {
        #[cfg(test)]
        super::faults::check(&target.path, "snapshot")?;
        let dir = data_dir.join("tmp").join("document-stage");
        std::fs::create_dir_all(&dir)?;
        let mut snapshot = tempfile::NamedTempFile::new_in(dir)?;
        snapshot.write_all(&bytes)?;
        snapshot.flush()?;
        snapshot.as_file().sync_all()?;
        Ok(snapshot)
    };
    let snapshot = stage().map_err(|e| {
        DocumentWriteError::new(
            "E_IO",
            format!("创建源快照失败；目标未修改 / Snapshot failed: {e}"),
        )
    })?;
    tracing::debug!(
        bytes = bytes.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "document source snapshot prepared"
    );
    Ok(PreparedDocument {
        source_digest,
        snapshot,
    })
}

pub struct CommitReceipt {
    pub backup: BackupReceipt,
    pub warnings: Vec<String>,
}

pub fn commit_prepared(
    target: &LockedTarget,
    prepared: PreparedDocument,
    data_dir: &Path,
    bytes: &[u8],
    cancel: &CancellationToken,
    protected: &[&Path],
) -> Result<CommitReceipt> {
    let started = std::time::Instant::now();
    check_cancel(cancel)?;
    target.validate()?;
    let original = read_bounded(prepared.staged_path()).map_err(|e| {
        DocumentWriteError::new(
            "E_SOURCE_READ",
            format!("读取源快照失败；目标未修改 / Snapshot read failed: {e}"),
        )
    })?;
    if Sha256::digest(&original).to_vec() != prepared.source_digest {
        return Err(DocumentWriteError::new(
            "E_SOURCE_READ",
            "源快照校验失败；目标未修改 / Snapshot verification failed",
        ));
    }
    let backup = backup::save_verified(data_dir, &target.path, &original).map_err(|e| {
        DocumentWriteError::new(
            "E_BACKUP_FAILED",
            format!("备份失败；目标未修改 / Backup failed; target was not modified: {e}"),
        )
    })?;
    #[cfg(test)]
    super::faults::check(&target.path, "before_compare")
        .map_err(|e| DocumentWriteError::new("E_IO", e.to_string()))?;
    target.validate()?;
    let current = read_bounded(&target.path).map_err(|e| {
        DocumentWriteError::new(
            "E_SOURCE_READ",
            format!(
                "提交前无法读取目标；未写入，备份位于 {} / Cannot verify target: {e}",
                backup.path().display()
            ),
        )
    })?;
    if Sha256::digest(&current).to_vec() != prepared.source_digest {
        return Err(DocumentWriteError::new(
            "E_DOCUMENT_CHANGED",
            format!(
                "目标已被其它操作修改；本次未写入，请重新读取。备份位于 {} / Document changed; read it again",
                backup.path().display()
            ),
        ));
    }
    backup.verify().map_err(|e| {
        DocumentWriteError::new(
            "E_BACKUP_FAILED",
            format!("备份校验失败；目标未修改 / Backup verification failed: {e}"),
        )
    })?;
    check_cancel(cancel)?;
    // No await/cancellation point in the commit critical section. Once replaced,
    // success means committed, even if cancellation arrives immediately after.
    let commit = || -> std::io::Result<()> {
        #[cfg(test)]
        super::faults::check(&target.path, "commit")?;
        crate::util::atomic::atomic_write(&target.path, bytes)
    };
    if let Err(e) = commit() {
        let unchanged = read_bounded(&target.path)
            .is_ok_and(|b| Sha256::digest(&b).to_vec() == prepared.source_digest);
        return Err(DocumentWriteError::new(
            "E_IO",
            format!(
                "写入失败；{}。完整备份：{} / Commit failed: {e}",
                if unchanged {
                    "已验证目标未修改 / verified unchanged"
                } else {
                    "目标当前状态未确认，请检查 / target state not verified"
                },
                backup.path().display()
            ),
        ));
    }
    let mut pinned = protected.to_vec();
    pinned.push(backup.path());
    let warnings = backup::prune(data_dir, &target.path, &pinned)
        .err()
        .map(|e| format!("文件已写入，但旧备份清理失败 / Committed; backup cleanup failed: {e}"))
        .into_iter()
        .collect();
    tracing::debug!(
        bytes = bytes.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "document backup and commit completed"
    );
    Ok(CommitReceipt { backup, warnings })
}

/// Manual restore has its own bounded wait, unrelated to any model run's cancel
/// token. Selected bytes are captured under the target lock before new backups.
pub async fn restore(
    data_dir: &Path,
    roots: &WriteRoots,
    requested: &Path,
    selected: &Path,
) -> Result<serde_json::Value> {
    let target = tokio::time::timeout(
        RESTORE_LOCK_TIMEOUT,
        LockedTarget::acquire(roots, requested, None),
    )
    .await
    .map_err(|_| {
        DocumentWriteError::new(
            "E_FILE_BUSY",
            "文件正被写入，请稍后重试 / File is busy; retry shortly",
        )
    })??;
    let dir = std::fs::canonicalize(backup::dir(data_dir)).map_err(|e| {
        DocumentWriteError::new(
            "E_BACKUP_FAILED",
            format!("备份目录不可读 / Backup directory unreadable: {e}"),
        )
    })?;
    let selected_canonical = std::fs::canonicalize(selected).map_err(|e| {
        DocumentWriteError::new(
            "E_BACKUP_FAILED",
            format!("备份不可读 / Backup unreadable: {e}"),
        )
    })?;
    let belongs = backup::list_for(data_dir, &target.path)
        .iter()
        .any(|p| p == selected);
    // Reject symlink backups, including aliases to other backups in the same dir.
    let regular = std::fs::symlink_metadata(selected).is_ok_and(|m| m.is_file());
    if !selected_canonical.starts_with(&dir) || !regular || !belongs {
        return Err(DocumentWriteError::new(
            "E_BACKUP_INVALID",
            "备份不属于目标文档或不在受限备份目录里 / Backup does not belong to this target",
        ));
    }
    let bytes = read_bounded(&selected_canonical).map_err(|e| {
        DocumentWriteError::new(
            "E_BACKUP_FAILED",
            format!("读取备份失败 / Backup read failed: {e}"),
        )
    })?;
    let cancel = CancellationToken::new();
    let (current_backup, status, warnings) = match std::fs::metadata(&target.path) {
        Ok(_) => {
            let prepared = prepare_existing(&target, data_dir, &cancel)?;
            let receipt =
                commit_prepared(&target, prepared, data_dir, &bytes, &cancel, &[selected])?;
            (
                Some(receipt.backup.path().to_string_lossy().into_owned()),
                "saved",
                receipt.warnings,
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Dangling symlinks are existing filesystem entries, not missing files.
            if std::fs::symlink_metadata(&target.path).is_ok() {
                return Err(DocumentWriteError::new(
                    "E_DOCUMENT_CHANGED",
                    "目标是悬空链接 / Target is a dangling symlink",
                ));
            }
            target.create_new(&bytes, &cancel)?;
            (None, "not_needed_missing_target", Vec::new())
        }
        Err(e) => {
            return Err(DocumentWriteError::new(
                "E_SOURCE_READ",
                format!("读取目标失败；未修改 / Target unreadable: {e}"),
            ));
        }
    };
    Ok(serde_json::json!({
        "path": target.path.to_string_lossy(), "restoredFrom": selected_canonical.to_string_lossy(),
        "currentBackup": current_backup, "backupStatus": status, "size": bytes.len(), "warnings": warnings,
    }))
}
