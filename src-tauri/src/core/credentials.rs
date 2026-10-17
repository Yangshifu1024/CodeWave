//! Pure credential resolution shared by every host and provider consumer.
use crate::core::config::{KEYRING_PLACEHOLDER, ModelConfig};
pub use crate::core::config_commit::{ConfigCommitError as CredentialError, CredentialStore};

/// Safe test/default adapter; a production bootstrap must inject its host store.
#[derive(Default)]
pub struct UnavailableCredentialStore;
impl CredentialStore for UnavailableCredentialStore {
    fn read(&self, _: &str) -> Result<Vec<String>, CredentialError> {
        Err(unavailable())
    }
    fn write(&self, _: &str, _: &[String]) -> Result<(), CredentialError> {
        Err(unavailable())
    }
    fn delete(&self, _: &str) -> Result<(), CredentialError> {
        Err(unavailable())
    }
}
fn unavailable() -> CredentialError {
    CredentialError {
        code: "CredentialUnavailable".into(),
        message: "Host credential storage was not provided; no plaintext fallback is permitted"
            .into(),
    }
}

pub fn resolve_keys(
    store: &dyn crate::core::config_commit::CredentialStore,
    model: &ModelConfig,
) -> Vec<String> {
    if model.keys.is_empty() {
        return Vec::new();
    }
    let has_placeholder = model.keys.iter().any(|k| k == KEYRING_PLACEHOLDER);
    let mut resolved: Vec<String> = model
        .keys
        .iter()
        .filter(|k| !k.is_empty() && k.as_str() != KEYRING_PLACEHOLDER)
        .cloned()
        .collect();
    if !has_placeholder {
        return resolved;
    }
    let mut accounts = model.keyring_accounts.clone();
    if accounts.is_empty() {
        accounts.push(model.id.clone());
    }
    let mut last_err = None;
    for account in &accounts {
        match store.read(account) {
            Ok(keys) => {
                for k in keys {
                    if !resolved.contains(&k) {
                        resolved.push(k);
                    }
                }
            }
            Err(e) => {
                tracing::debug!("{e}（账户 {account}）");
                last_err = Some(e);
            }
        }
    }
    if resolved.is_empty()
        && let Some(e) = last_err
    {
        tracing::warn!("{e}（模型 {} 的 key 回退为空）", model.id);
    }
    resolved
}

pub fn pick_resolved_key(store: &dyn CredentialStore, model: &ModelConfig) -> Option<String> {
    resolve_keys(store, model)
        .into_iter()
        .find(|key| !key.is_empty())
}
pub fn resolve_provider_keys(
    store: &dyn CredentialStore,
    provider: &crate::core::config::ProviderConfig,
) -> Result<Vec<String>, String> {
    crate::core::config_commit::resolve_provider_with(store, provider).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct MemoryCredentials(AtomicUsize);
    impl CredentialStore for MemoryCredentials {
        fn read(&self, account: &str) -> Result<Vec<String>, CredentialError> {
            assert_eq!(account, "generation.fixture");
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec!["isolated-fixture-secret".into()])
        }
        fn write(&self, _: &str, _: &[String]) -> Result<(), CredentialError> {
            panic!("read-only fixture")
        }
        fn delete(&self, _: &str) -> Result<(), CredentialError> {
            panic!("read-only fixture")
        }
    }

    #[tokio::test]
    async fn actual_drive_resolves_saved_reference_through_injected_host_credentials() {
        let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
        let address = listener.local_addr().unwrap();
        let server_listener = listener.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = server_listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "incomplete request");
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&request);
            assert!(
                text.to_ascii_lowercase()
                    .contains("authorization: bearer isolated-fixture-secret")
            );
            assert!(
                !text.contains("__keyring__"),
                "credential references must not be sent as keys"
            );
            let body = "data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let workspace = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut config = crate::core::config::ConfigState::default();
        config.providers.push(crate::core::config::ProviderConfig {
            id: "provider".into(),
            api_format: crate::core::config::ApiFormat::OpenAiChat,
            base_url: format!("http://{address}/v1"),
            keys: vec![KEYRING_PLACEHOLDER.into()],
            credential_ref: Some("generation.fixture".into()),
            models: vec![crate::core::config::ProviderModel {
                id: "model".into(),
                model: "fixture".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        config.active_model_id = Some("model".into());
        let credentials = Arc::new(MemoryCredentials(AtomicUsize::new(0)));
        let store = Arc::new(crate::core::sessions::SessionStore::new(
            data.path().to_path_buf(),
        ));
        let core = Arc::new(crate::core::agent::AgentCore::new_with_credentials(
            config,
            Arc::new(crate::core::agent::test_support::NoopSink),
            store,
            reqwest::Client::new(),
            data.path().to_path_buf(),
            credentials.clone(),
        ));
        let runtime = core.get_or_create_session(
            "credential-fixture",
            workspace.path().to_path_buf(),
            None,
            vec![],
            None,
            vec![],
        );
        runtime
            .history
            .lock()
            .unwrap()
            .push(crate::core::types::Message::user_text("hello"));
        let (result, usage, _) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::core::agent::drive_agent(&core, &runtime, Default::default(), "credential-run"),
        )
        .await
        .unwrap();
        assert!(result.is_ok(), "actual drive failed: {result:?}");
        assert_eq!(usage.output, 2);
        assert_eq!(credentials.0.load(Ordering::SeqCst), 1);
        server.await.unwrap();
    }
}
