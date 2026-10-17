use super::*;
use std::collections::HashMap;
use std::sync::Mutex as SyncMutex;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Default)]
struct Backend {
    entries: SyncMutex<HashMap<String, String>>,
    fault: AtomicU8,
}

impl TrustBackend for Backend {
    fn read(&self, account: &str) -> Result<Option<String>, TrustError> {
        if self.fault.load(Ordering::SeqCst) == 3 {
            return Err(TrustError::Unavailable);
        }
        Ok(self.entries.lock().unwrap().get(account).cloned())
    }

    fn write(&self, account: &str, value: &str) -> Result<(), TrustError> {
        let fault = self.fault.load(Ordering::SeqCst);
        if fault == 1 {
            return Err(TrustError::Unavailable);
        }
        self.entries
            .lock()
            .unwrap()
            .insert(account.into(), value.into());
        if fault == 2 {
            return Err(TrustError::Unavailable);
        }
        if fault == 4 {
            self.fault.store(3, Ordering::SeqCst);
        }
        Ok(())
    }
}

fn key() -> TrustKey {
    TrustKey {
        kind: "mcp".into(),
        purpose: "launch".into(),
        source: std::env::temp_dir().join("project/.codewave/mcp.json"),
        owner: "project-a".into(),
        name: "fixture".into(),
    }
}

fn op() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::test]
async fn independent_stores_serialize_cas_and_preserve_revocation_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let a = TrustStore::new(dir.path().to_path_buf(), backend.clone());
    let b = TrustStore::new(dir.path().to_path_buf(), backend.clone());
    let cancel = CancellationToken::new();
    let key = key();
    let version = "a".repeat(64);
    assert!(matches!(
        a.admit(&key, &version, None, &cancel).await,
        Err(TrustError::Denied)
    ));
    let (op_a, op_b) = (op(), op());
    let (a_result, b_result) = tokio::join!(
        a.compare_exchange(&key, None, &version, Decision::Grant, &op_a, &cancel),
        b.compare_exchange(&key, None, &version, Decision::Grant, &op_b, &cancel),
    );
    assert_ne!(a_result.is_ok(), b_result.is_ok());
    assert_eq!(
        a_result.err().or(b_result.err()),
        Some(TrustError::Conflict)
    );
    let admitted = a.admit(&key, &version, Some(1), &cancel).await.unwrap();
    assert_eq!(admitted.record.as_ref().unwrap().revision, 1);
    drop(admitted);
    assert!(matches!(
        a.admit(&key, &"b".repeat(64), None, &cancel).await,
        Err(TrustError::Denied)
    ));
    assert!(matches!(
        a.admit(&key, &version, Some(2), &cancel).await,
        Err(TrustError::Denied)
    ));
    let revoke = b
        .compare_exchange(&key, Some(1), &version, Decision::Revoke, &op(), &cancel)
        .await
        .unwrap();
    assert_eq!(revoke.revision, 2);
    let restarted = TrustStore::new(dir.path().to_path_buf(), backend);
    assert!(matches!(
        restarted.admit(&key, &version, None, &cancel).await,
        Err(TrustError::Denied)
    ));
    assert_eq!(
        restarted.read_guard(&key, &cancel).await.unwrap().record,
        Some(revoke)
    );
    assert_eq!(
        restarted
            .compare_exchange(&key, None, &version, Decision::Grant, &op_a, &cancel)
            .await,
        Err(TrustError::Conflict)
    );
}

#[tokio::test]
async fn authority_guard_orders_transactions_and_wait_is_cancellable() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let a = TrustStore::new(dir.path().to_path_buf(), backend.clone());
    let b = TrustStore::new(dir.path().to_path_buf(), backend);
    let held = a
        .read_guard(&key(), &CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let resource = key();
    let waiting = b.read_guard(&resource, &cancel);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut waiting)
            .await
            .is_err()
    );
    cancel.cancel();
    assert!(matches!(waiting.await, Err(TrustError::Cancelled)));
    drop(held);
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            b.read_guard(&key(), &CancellationToken::new())
        )
        .await
        .unwrap()
        .is_ok()
    );
}

#[tokio::test]
async fn retry_is_idempotent_and_unknown_commit_can_be_recovered_without_reauthorizing() {
    for fault in [0, 1, 2, 4] {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(Backend::default());
        let store = TrustStore::new(dir.path().to_path_buf(), backend.clone());
        let operation = op();
        let version = "b".repeat(64);
        let cancel = CancellationToken::new();
        backend.fault.store(fault, Ordering::SeqCst);
        let result = store
            .compare_exchange(&key(), None, &version, Decision::Grant, &operation, &cancel)
            .await;
        match fault {
            1 => assert_eq!(result, Err(TrustError::Unavailable)),
            4 => assert_eq!(result, Err(TrustError::OutcomeUnknown)),
            _ => assert_eq!(result.unwrap().revision, 1),
        }
        backend.fault.store(0, Ordering::SeqCst);
        let retry = store
            .compare_exchange(&key(), None, &version, Decision::Grant, &operation, &cancel)
            .await
            .unwrap();
        assert_eq!(retry.revision, 1);
        assert_eq!(
            store
                .compare_exchange(
                    &key(),
                    Some(1),
                    &version,
                    Decision::Grant,
                    &operation,
                    &cancel
                )
                .await,
            Err(TrustError::Conflict)
        );
        let resource_version = format!("sha256:{}", version.to_ascii_uppercase());
        assert_eq!(
            store
                .admit(&key(), &resource_version, Some(1), &cancel)
                .await
                .unwrap()
                .record
                .unwrap()
                .version,
            version
        );
        assert_eq!(
            store
                .compare_exchange(
                    &key(),
                    Some(1),
                    &version,
                    Decision::Revoke,
                    &operation,
                    &cancel
                )
                .await,
            Err(TrustError::Conflict)
        );
        assert_eq!(
            store
                .compare_exchange(
                    &key(),
                    None,
                    &"c".repeat(64),
                    Decision::Grant,
                    &operation,
                    &cancel
                )
                .await,
            Err(TrustError::Conflict)
        );
    }
}

#[tokio::test]
async fn missing_corrupt_unavailable_and_forged_disk_authority_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let store = TrustStore::new(dir.path().to_path_buf(), backend.clone());
    let cancel = CancellationToken::new();
    std::fs::create_dir_all(dir.path().join("trust")).unwrap();
    std::fs::write(dir.path().join("trust/trusted.json"), r#"{"trusted":true}"#).unwrap();
    assert!(
        store
            .read_guard(&key(), &cancel)
            .await
            .unwrap()
            .record
            .is_none()
    );
    backend
        .entries
        .lock()
        .unwrap()
        .insert(store.account(&key()).unwrap(), "{broken".into());
    assert!(matches!(
        store.read_guard(&key(), &cancel).await,
        Err(TrustError::Corrupt)
    ));
    assert_eq!(
        store
            .compare_exchange(
                &key(),
                None,
                &"d".repeat(64),
                Decision::Grant,
                &op(),
                &cancel
            )
            .await,
        Err(TrustError::Corrupt)
    );
    backend.fault.store(3, Ordering::SeqCst);
    assert!(matches!(
        store.read_guard(&key(), &cancel).await,
        Err(TrustError::Unavailable)
    ));
}

#[tokio::test]
async fn identity_fields_and_record_binding_prevent_cross_resource_authorization() {
    let original = key();
    for changed in [
        TrustKey {
            kind: "skill".into(),
            ..original.clone()
        },
        TrustKey {
            purpose: "prompt".into(),
            ..original.clone()
        },
        TrustKey {
            source: original.source.with_file_name("other.json"),
            ..original.clone()
        },
        TrustKey {
            owner: "project-b".into(),
            ..original.clone()
        },
        TrustKey {
            name: "other".into(),
            ..original.clone()
        },
    ] {
        assert_ne!(original.digest().unwrap(), changed.digest().unwrap());
    }
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::default());
    let store = TrustStore::new(dir.path().to_path_buf(), backend.clone());
    let cancel = CancellationToken::new();
    let record = store
        .compare_exchange(
            &original,
            None,
            &"e".repeat(64),
            Decision::Grant,
            &op(),
            &cancel,
        )
        .await
        .unwrap();
    let other_profile = tempfile::tempdir().unwrap();
    let isolated = TrustStore::new(other_profile.path().to_path_buf(), backend.clone());
    assert_ne!(
        store.account(&original).unwrap(),
        isolated.account(&original).unwrap()
    );
    assert!(matches!(
        isolated
            .admit(&original, &"e".repeat(64), None, &cancel)
            .await,
        Err(TrustError::Denied)
    ));
    #[cfg(unix)]
    {
        let alias = other_profile.path().join("profile-alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        let same = TrustStore::new(alias, backend.clone());
        assert_eq!(
            store.account(&original).unwrap(),
            same.account(&original).unwrap()
        );
        assert!(
            same.admit(&original, &"e".repeat(64), Some(1), &cancel)
                .await
                .is_ok()
        );
        let source = dir.path().join("mcp.json");
        std::fs::write(&source, "{}").unwrap();
        let alias = other_profile.path().join("source-alias");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert_eq!(
            TrustKey {
                source,
                ..original.clone()
            }
            .digest()
            .unwrap(),
            TrustKey {
                source: alias,
                ..original.clone()
            }
            .digest()
            .unwrap()
        );
    }
    let other = TrustKey {
        name: "other".into(),
        ..original
    };
    backend.entries.lock().unwrap().insert(
        store.account(&other).unwrap(),
        serde_json::to_string(&record).unwrap(),
    );
    assert!(matches!(
        store.read_guard(&other, &cancel).await,
        Err(TrustError::Corrupt)
    ));
}

#[tokio::test]
async fn native_timeout_retains_locks_and_unknown_write_replays_exactly_once() {
    struct Blocked {
        base: Backend,
        writing: bool,
        entered: SyncMutex<Option<tokio::sync::oneshot::Sender<()>>>,
        released: SyncMutex<bool>,
        wake: std::sync::Condvar,
    }
    impl Blocked {
        fn pause(&self, writing: bool) {
            if self.writing != writing {
                return;
            }
            let Some(entered) = self.entered.lock().unwrap().take() else {
                return;
            };
            let _ = entered.send(());
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }
    impl TrustBackend for Blocked {
        fn read(&self, account: &str) -> Result<Option<String>, TrustError> {
            self.pause(false);
            self.base.read(account)
        }
        fn write(&self, account: &str, value: &str) -> Result<(), TrustError> {
            self.pause(true);
            self.base.write(account, value)
        }
    }
    // Also release on assertion failure, so a failed test cannot hang the
    // runtime's blocking-pool shutdown while a fixture owns an OS lock.
    struct Release(Arc<Blocked>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    for (writing, cancelled) in [(false, false), (true, false), (false, true), (true, true)] {
        let dir = tempfile::tempdir().unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let backend = Arc::new(Blocked {
            base: Backend::default(),
            writing,
            entered: SyncMutex::new(Some(entered)),
            released: SyncMutex::new(false),
            wake: std::sync::Condvar::new(),
        });
        let _release = Release(backend.clone());
        let mut store = TrustStore::new(dir.path().to_path_buf(), backend.clone());
        store.budget = Duration::from_millis(250);
        let store = Arc::new(store);
        let operation = op();
        let version = "f".repeat(64);
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let store = store.clone();
            let operation = operation.clone();
            let version = version.clone();
            let cancel = cancel.clone();
            async move {
                if writing {
                    store
                        .compare_exchange(
                            &key(),
                            None,
                            &version,
                            Decision::Grant,
                            &operation,
                            &cancel,
                        )
                        .await
                        .map(|_| ())
                } else {
                    store.read_guard(&key(), &cancel).await.map(|_| ())
                }
            }
        });
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        if cancelled {
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&store.lock_path)
            .unwrap();
        let still_locked = matches!(file.try_lock(), Err(TryLockError::WouldBlock));
        drop(file);
        let local_held = store.local.try_lock().is_err();
        backend.release();
        assert_eq!(
            result,
            Err(if writing {
                TrustError::OutcomeUnknown
            } else if cancelled {
                TrustError::Cancelled
            } else {
                TrustError::Busy
            })
        );
        assert!(still_locked && local_held);
        if writing {
            let receipt = store
                .compare_exchange(
                    &key(),
                    None,
                    &version,
                    Decision::Grant,
                    &operation,
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(receipt.revision, 1);
        } else {
            assert!(
                store
                    .read_guard(&key(), &CancellationToken::new())
                    .await
                    .unwrap()
                    .record
                    .is_none()
            );
        }
    }
}
