//! Version-bound host authority. Disk files coordinate transactions, never grant trust.
//! Native keyring calls run in the blocking pool; no runtime/config lock belongs here.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
pub struct TrustKey {
    pub kind: String,
    pub purpose: String,
    pub source: PathBuf,
    pub owner: String,
    pub name: String,
}

impl TrustKey {
    pub fn digest(&self) -> Result<String, TrustError> {
        if self.kind.is_empty()
            || self.purpose.is_empty()
            || self.owner.is_empty()
            || self.name.is_empty()
            || !self.source.is_absolute()
        {
            return Err(TrustError::Invalid);
        }
        let canonical = Self {
            source: crate::tools::pathutil::canonical_best_effort(&self.source),
            ..self.clone()
        };
        let bytes = serde_json::to_vec(&canonical).map_err(|_| TrustError::Invalid)?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Grant,
    Revoke,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRecord {
    schema: u8,
    key: String,
    pub version: String,
    pub decision: Decision,
    pub revision: u64,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    #[error("TrustInputInvalid: review the current resource again")]
    Invalid,
    #[error("TrustUnavailable: unlock or enable the system keyring; no disk fallback is used")]
    Unavailable,
    #[error("TrustCorrupt: the authority record is invalid; no permission was granted")]
    Corrupt,
    #[error("TrustConflict: trust changed; review the resource again")]
    Conflict,
    #[error("TrustDenied: this resource version has no current permission")]
    Denied,
    #[error("TrustBusy: another trust transaction did not finish in time")]
    Busy,
    #[error("TrustCancelled: the operation was cancelled")]
    Cancelled,
    #[error("TrustOutcomeUnknown: refresh the operation receipt before retrying")]
    OutcomeUnknown,
}

/// Implementations must distinguish a missing entry from an unavailable store.
/// Accounts are resource digests in a separate service, never provider accounts.
pub trait TrustBackend: Send + Sync {
    fn read(&self, account: &str) -> Result<Option<String>, TrustError>;
    fn write(&self, account: &str, value: &str) -> Result<(), TrustError>;
}

pub struct TrustStore {
    backend: Arc<dyn TrustBackend>,
    lock_path: PathBuf,
    namespace: PathBuf,
    local: Arc<Mutex<()>>,
    budget: Duration,
}

/// Keeps read/admission ordered against cooperating revocations until the caller
/// has registered its admission. Drop before waiting on a process or network.
pub struct AuthorityGuard {
    _file: File,
    _local: OwnedMutexGuard<()>,
    pub record: Option<TrustRecord>,
}

impl TrustStore {
    pub fn new(data_dir: PathBuf, backend: Arc<dyn TrustBackend>) -> Self {
        let data_dir = crate::tools::pathutil::canonical_best_effort(&data_dir);
        Self {
            backend,
            lock_path: data_dir.join("trust/authority.lock"),
            namespace: data_dir,
            local: Arc::new(Mutex::new(())),
            budget: WAIT,
        }
    }

    fn account(&self, key: &TrustKey) -> Result<String, TrustError> {
        // Separate profiles must not share an authority record while using
        // different advisory locks. Canonical aliases of one profile do share it.
        let bytes = serde_json::to_vec(&(&self.namespace, key.digest()?))
            .map_err(|_| TrustError::Invalid)?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    async fn transact<T: Send + 'static>(
        &self,
        cancel: &CancellationToken,
        mutation: bool,
        operation: impl FnOnce(File, OwnedMutexGuard<()>, Instant) -> Result<T, TrustError>
        + Send
        + 'static,
    ) -> Result<T, TrustError> {
        let deadline = Instant::now() + self.budget;
        let local = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(TrustError::Cancelled),
            result = tokio::time::timeout(self.budget, self.local.clone().lock_owned()) =>
                result.map_err(|_| TrustError::Busy)?,
        };
        let path = self.lock_path.clone();
        let worker_stop = cancel.child_token();
        let stopped = worker_stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let parent = path.parent().ok_or(TrustError::Invalid)?;
            std::fs::create_dir_all(parent).map_err(|_| TrustError::Unavailable)?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(|_| TrustError::Unavailable)?;
            loop {
                if stopped.is_cancelled() {
                    return Err(TrustError::Cancelled);
                }
                match file.try_lock() {
                    Ok(()) => break,
                    Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(TryLockError::WouldBlock) => return Err(TrustError::Busy),
                    Err(TryLockError::Error(_)) => return Err(TrustError::Unavailable),
                }
            }
            if stopped.is_cancelled() {
                return Err(TrustError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(TrustError::Busy);
            }
            operation(file, local, deadline)
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(if mutation { TrustError::OutcomeUnknown } else { TrustError::Cancelled }),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                worker_stop.cancel();
                Err(if mutation { TrustError::OutcomeUnknown } else { TrustError::Busy })
            },
            result = task => result.map_err(|_| if mutation { TrustError::OutcomeUnknown } else { TrustError::Unavailable })?,
        }
        // A native keyring call cannot be interrupted after entry. A detached
        // worker retains both locks until it finishes; cancellation never frees
        // a transaction lock while that worker may still commit.
    }

    pub async fn read_guard(
        &self,
        key: &TrustKey,
        cancel: &CancellationToken,
    ) -> Result<AuthorityGuard, TrustError> {
        let digest = self.account(key)?;
        let backend = self.backend.clone();
        self.transact(cancel, false, move |file, local, _deadline| {
            let record = read(backend.as_ref(), &digest)?;
            Ok(AuthorityGuard {
                _file: file,
                _local: local,
                record,
            })
        })
        .await
    }

    pub async fn admit(
        &self,
        key: &TrustKey,
        version: &str,
        revision: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<AuthorityGuard, TrustError> {
        let version = normalize_version(version)?;
        let guard = self.read_guard(key, cancel).await?;
        if guard.record.as_ref().is_some_and(|record| {
            record.decision == Decision::Grant
                && record.version == version
                && revision.is_none_or(|revision| record.revision == revision)
        }) {
            Ok(guard)
        } else {
            Err(TrustError::Denied)
        }
    }

    pub async fn compare_exchange(
        &self,
        key: &TrustKey,
        expected: Option<u64>,
        version: &str,
        decision: Decision,
        operation_id: &str,
        cancel: &CancellationToken,
    ) -> Result<TrustRecord, TrustError> {
        let digest = self.account(key)?;
        let version = normalize_version(version)?;
        let operation_id = uuid::Uuid::parse_str(operation_id)
            .ok()
            .filter(|id| !id.is_nil())
            .ok_or(TrustError::Invalid)?
            .to_string();
        let backend = self.backend.clone();
        let stopped = cancel.clone();
        self.transact(cancel, true, move |_file, _local, deadline| {
            let current = read(backend.as_ref(), &digest)?;
            if let Some(record) = &current
                && record.operation_id == operation_id
            {
                return if record.version == version
                    && record.decision == decision
                    && expected.unwrap_or(0).checked_add(1) == Some(record.revision)
                {
                    Ok(record.clone())
                } else {
                    Err(TrustError::Conflict)
                };
            }
            if current.as_ref().map(|record| record.revision) != expected {
                return Err(TrustError::Conflict);
            }
            let revision = expected
                .unwrap_or(0)
                .checked_add(1)
                .ok_or(TrustError::Corrupt)?;
            let next = TrustRecord {
                schema: 1,
                key: digest.clone(),
                version,
                decision,
                revision,
                operation_id,
            };
            let value = serde_json::to_string(&next).map_err(|_| TrustError::Invalid)?;
            if stopped.is_cancelled() {
                return Err(TrustError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(TrustError::Busy);
            }
            // A backend may report failure after committing. Readback, rather
            // than set_password's return alone, determines a confirmed outcome.
            let written = backend.write(&digest, &value);
            match read(backend.as_ref(), &digest) {
                Ok(Some(actual)) if actual == next => Ok(actual),
                Ok(actual) if actual == current && written.is_err() => Err(TrustError::Unavailable),
                _ => Err(TrustError::OutcomeUnknown),
            }
        })
        .await
    }
}

fn valid_version(version: &str) -> bool {
    version.len() == 64
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// MCP uses bare digests; resource provenance uses `sha256:`. Normalize here,
/// once, so all admission and transaction callers bind the same byte version.
fn normalize_version(version: &str) -> Result<String, TrustError> {
    let digest = version.strip_prefix("sha256:").unwrap_or(version);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TrustError::Invalid);
    }
    Ok(digest.to_ascii_lowercase())
}

fn read(backend: &dyn TrustBackend, key: &str) -> Result<Option<TrustRecord>, TrustError> {
    let Some(value) = backend.read(key)? else {
        return Ok(None);
    };
    if value.len() > 2048 {
        return Err(TrustError::Corrupt);
    }
    let record: TrustRecord = serde_json::from_str(&value).map_err(|_| TrustError::Corrupt)?;
    if record.schema != 1
        || record.key != key
        || record.revision == 0
        || !valid_version(&record.version)
        || uuid::Uuid::parse_str(&record.operation_id)
            .ok()
            .is_none_or(|id| id.is_nil())
    {
        return Err(TrustError::Corrupt);
    }
    Ok(Some(record))
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
