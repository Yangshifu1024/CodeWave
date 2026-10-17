//! Trust authority has its own service; provider account references cannot read it.
use crate::core::trust::{TrustBackend, TrustError};

pub const SERVICE: &str = "codewave.yangshifu.xyz.trust.v1";

pub struct SystemTrustBackend;

impl TrustBackend for SystemTrustBackend {
    fn read(&self, account: &str) -> Result<Option<String>, TrustError> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(|_| TrustError::Unavailable)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(TrustError::Unavailable),
        }
    }

    fn write(&self, account: &str, value: &str) -> Result<(), TrustError> {
        keyring::Entry::new(SERVICE, account)
            .and_then(|entry| entry.set_password(value))
            .map_err(|_| TrustError::Unavailable)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::trust::{Decision, TrustKey, TrustStore};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    /// Only accounts written by this unique disposable profile are cleaned up.
    #[derive(Default)]
    pub(crate) struct NativeFixture {
        written: Mutex<HashSet<String>>,
    }
    impl NativeFixture {
        pub(crate) fn cleanup(&self) -> Result<(), String> {
            let accounts = self.written.lock().unwrap().clone();
            for account in accounts {
                let entry =
                    keyring::Entry::new(SERVICE, &account).map_err(|error| error.to_string())?;
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(error) => return Err(error.to_string()),
                }
                if !matches!(entry.get_password(), Err(keyring::Error::NoEntry)) {
                    return Err("Synthetic trust record removal could not be verified".into());
                }
                self.written.lock().unwrap().remove(&account);
            }
            Ok(())
        }
    }
    impl Drop for NativeFixture {
        fn drop(&mut self) {
            if self.cleanup().is_err() {
                eprintln!(
                    "Synthetic trust account cleanup failed; native verification is incomplete"
                );
            }
        }
    }
    impl TrustBackend for NativeFixture {
        fn read(&self, account: &str) -> Result<Option<String>, TrustError> {
            SystemTrustBackend.read(account)
        }
        fn write(&self, account: &str, value: &str) -> Result<(), TrustError> {
            self.written.lock().unwrap().insert(account.into());
            SystemTrustBackend.write(account, value)
        }
    }

    fn fixture_key(profile: &std::path::Path, owner: String) -> TrustKey {
        TrustKey {
            kind: "native-fixture".into(),
            purpose: "test-only".into(),
            source: profile.join("synthetic-resource"),
            owner,
            name: "native-roundtrip".into(),
        }
    }

    async fn native_child(profile: &std::path::Path, owner: &str, phase: &str) {
        let stdout_path = profile.join(format!("native-{phase}.stdout"));
        let stderr_path = profile.join(format!("native-{phase}.stderr"));
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "host::trust::tests::native_trust_roundtrip",
                "--ignored",
                "--nocapture",
            ])
            .env("CODEWAVE_NATIVE_TRUST_CHILD", phase)
            .env("CODEWAVE_NATIVE_TRUST_PROFILE", profile)
            .env("CODEWAVE_NATIVE_TRUST_OWNER", owner)
            .stdin(std::process::Stdio::null())
            .stdout(std::fs::File::create(&stdout_path).unwrap())
            .stderr(std::fs::File::create(&stderr_path).unwrap())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let status = match tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
            .await
        {
            Ok(status) => status.unwrap(),
            Err(_) => {
                // Reap before unwinding: parent cleanup must not race a live
                // child's late keyring mutation. kill_on_drop alone only signals.
                let killed = child.kill().await;
                let reaped = child.wait().await;
                panic!("native child exceeded its test budget; kill={killed:?}, wait={reaped:?}");
            }
        };
        assert!(
            status.success(),
            "native child {phase} failed: {} {}",
            std::fs::read_to_string(stdout_path).unwrap(),
            std::fs::read_to_string(stderr_path).unwrap()
        );
        let receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(profile.join(format!("native-{phase}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            receipt["phase"], phase,
            "child test must actually execute, not match zero tests"
        );
        assert_eq!(receipt["owner"], owner);
    }

    #[tokio::test]
    #[ignore = "Real system keyring and child processes: explicitly run native_trust_roundtrip with --ignored; creates and removes synthetic trust records"]
    async fn native_trust_roundtrip() {
        // Child uses the real backend but never owns cleanup. The parent already
        // recorded this unique account and retains cleanup through both children.
        if let Ok(phase) = std::env::var("CODEWAVE_NATIVE_TRUST_CHILD") {
            let profile = std::path::PathBuf::from(
                std::env::var_os("CODEWAVE_NATIVE_TRUST_PROFILE").unwrap(),
            );
            let owner = std::env::var("CODEWAVE_NATIVE_TRUST_OWNER").unwrap();
            let key = fixture_key(&profile, owner);
            let store = TrustStore::new(profile.clone(), Arc::new(SystemTrustBackend));
            let cancel = CancellationToken::new();
            match phase.as_str() {
                "locked" => assert!(matches!(
                    store.read_guard(&key, &cancel).await,
                    Err(TrustError::Busy)
                )),
                "revoke" => {
                    drop(
                        store
                            .admit(&key, &"a".repeat(64), Some(1), &cancel)
                            .await
                            .unwrap(),
                    );
                    let receipt = store
                        .compare_exchange(
                            &key,
                            Some(1),
                            &"a".repeat(64),
                            Decision::Revoke,
                            &uuid::Uuid::new_v4().to_string(),
                            &cancel,
                        )
                        .await
                        .unwrap();
                    assert_eq!(receipt.revision, 2);
                }
                _ => panic!("invalid native fixture child phase"),
            }
            std::fs::write(
                profile.join(format!("native-{phase}.json")),
                serde_json::to_vec(&serde_json::json!({"phase":phase,"owner":key.owner})).unwrap(),
            )
            .unwrap();
            return;
        }
        let profile = tempfile::tempdir().unwrap();
        let backend = Arc::new(NativeFixture::default());
        let key = fixture_key(profile.path(), uuid::Uuid::new_v4().to_string());
        let cancel = CancellationToken::new();
        let version = "a".repeat(64);
        let first = TrustStore::new(profile.path().into(), backend.clone());
        assert!(
            first
                .read_guard(&key, &cancel)
                .await
                .unwrap()
                .record
                .is_none()
        );
        let grant = first
            .compare_exchange(
                &key,
                None,
                &version,
                Decision::Grant,
                &uuid::Uuid::new_v4().to_string(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(grant.revision, 1);
        drop(first);
        let reopened = TrustStore::new(profile.path().into(), backend.clone());
        drop(
            reopened
                .admit(&key, &version, Some(1), &cancel)
                .await
                .unwrap(),
        );
        assert!(matches!(
            reopened.admit(&key, &"b".repeat(64), None, &cancel).await,
            Err(TrustError::Denied)
        ));
        // Intentionally hold admission through a competing child's lock deadline.
        // This tests OS-level exclusion, not only two instances' local mutexes.
        let held = reopened.read_guard(&key, &cancel).await.unwrap();
        native_child(profile.path(), &key.owner, "locked").await;
        drop(held);
        native_child(profile.path(), &key.owner, "revoke").await;
        drop(reopened);
        let after_revoke = TrustStore::new(profile.path().into(), backend.clone());
        let guard = after_revoke.read_guard(&key, &cancel).await.unwrap();
        assert_eq!(guard.record.as_ref().unwrap().revision, 2);
        assert_eq!(guard.record.as_ref().unwrap().decision, Decision::Revoke);
        drop(guard);
        assert!(matches!(
            after_revoke.admit(&key, &version, None, &cancel).await,
            Err(TrustError::Denied)
        ));
        assert!(matches!(
            after_revoke
                .compare_exchange(
                    &key,
                    Some(1),
                    &version,
                    Decision::Grant,
                    &uuid::Uuid::new_v4().to_string(),
                    &cancel
                )
                .await,
            Err(TrustError::Conflict)
        ));
        backend
            .cleanup()
            .expect("synthetic native trust records must be removed");
        assert!(
            after_revoke
                .read_guard(&key, &cancel)
                .await
                .unwrap()
                .record
                .is_none()
        );
    }
}
