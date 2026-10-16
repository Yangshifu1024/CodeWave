//! Persistent authority is checked without holding the pool/config mutexes.
use super::*;
use crate::core::trust::{AuthorityGuard, TrustStore};
use crate::mcp::trust::{AdmissionError, AuthorityProof, authority_error, authority_key};

impl McpManager {
    pub fn with_authority(max_servers: usize, store: Arc<TrustStore>) -> Self {
        let mut manager = Self::new(max_servers);
        manager.authority = Some(store);
        manager
    }

    pub fn authority_store(&self) -> Option<&Arc<TrustStore>> {
        self.authority.as_ref()
    }

    pub(in crate::mcp) async fn install_persistent(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
        revision: u64,
    ) -> Result<LaunchPermit, McpError> {
        let store = self.authority.as_ref().ok_or_else(untrusted)?.clone();
        let mut inner = self.inner.lock().await;
        let permit = self
            .launch
            .grant_persistent(key, cfg, AuthorityProof { store, revision })?;
        if let Some(entry) = inner.entries.get_mut(key)
            && entry.cfg == *cfg
        {
            entry.launch = Some(permit.clone());
        }
        Ok(permit)
    }

    pub(super) async fn connection_admission(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<(LaunchPermit, Option<AuthorityGuard>), AdmissionError> {
        if let Ok(permit) = self.launch.connection(key, cfg) {
            let guard = permit.admit(key, cfg).await?;
            return Ok((permit, guard));
        }
        let store = self.authority.as_ref().ok_or(AdmissionError::Denied)?;
        let guard = store
            .admit(
                &authority_key(key),
                &super::super::launch_version(cfg),
                None,
                &CancellationToken::new(),
            )
            .await?;
        LaunchPermit::check_source(key, cfg)?;
        let revision = guard.record.as_ref().ok_or_else(untrusted)?.revision;
        let permit = self.install_persistent(key, cfg, revision).await?;
        Ok((permit, Some(guard)))
    }

    pub(super) async fn entry_admission(
        &self,
        key: &PoolKey,
        generation: Option<u64>,
    ) -> Result<Option<AuthorityGuard>, McpError> {
        let (permit, cfg) = {
            let inner = self.inner.lock().await;
            let entry = inner.entries.get(key).ok_or_else(changed_error)?;
            if generation.is_some_and(|generation| generation != entry.generation) {
                return Err(changed_error());
            }
            (
                entry.launch.clone().ok_or_else(untrusted)?,
                entry.cfg.clone(),
            )
        };
        permit.admit(key, &cfg).await.map_err(Into::into)
    }

    pub(super) async fn revalidate_authority(&self, session: &str) {
        let candidates: Vec<_> = {
            let inner = self.inner.lock().await;
            inner
                .sessions
                .get(session)
                .into_iter()
                .flatten()
                .filter_map(|key| {
                    let entry = inner.entries.get(key)?;
                    let permit = entry
                        .launch
                        .as_ref()
                        .filter(|permit| permit.authority.is_some())?;
                    Some((key.clone(), entry.cfg.clone(), permit.clone()))
                })
                .collect()
        };
        for (key, cfg, permit) in candidates {
            let failed = permit.admit(&key, &cfg).await.is_err();
            if !failed {
                continue;
            }
            let retired = {
                let mut inner = self.inner.lock().await;
                let Some(entry) = inner.entries.get_mut(&key) else {
                    continue;
                };
                if !entry.launch.as_ref().is_some_and(|current| {
                    current.id == permit.id
                        && current.authority.as_ref().map(|proof| proof.revision)
                            == permit.authority.as_ref().map(|proof| proof.revision)
                }) {
                    continue;
                }
                self.retire_launch(
                    &key,
                    &permit.id,
                    permit.authority.as_ref().map(|proof| proof.revision),
                );
                let service = entry.service.take();
                clear_connection(entry);
                entry.state = McpState::Stopped;
                entry.error = None;
                entry.note = Some("Persistent launch permission changed or is unavailable".into());
                (service, payload_of(&key, entry))
            };
            if let Some(service) = retired.0 {
                tokio::spawn(close_service(service));
            }
            self.emit_key(&key, session, &retired.1).await;
        }
    }

    pub async fn revalidate_all_authority(&self) {
        let sessions: Vec<_> = self.inner.lock().await.sessions.keys().cloned().collect();
        futures::future::join_all(
            sessions
                .iter()
                .map(|session| self.revalidate_authority(session)),
        )
        .await;
    }

    pub async fn current_launch_status(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> LaunchStatus {
        match self.connection_admission(key, cfg).await {
            Ok((_permit, _guard)) => self.launch.status(key, cfg),
            Err(error) => {
                let mut status = self.launch.status(key, cfg);
                status.trusted = false;
                status.error = if error.is_denied() {
                    None
                } else {
                    Some(McpError::from(error).message)
                };
                if status.error.is_none() {
                    status.remembered = false;
                    status.revision = None;
                }
                status
            }
        }
    }

    pub(super) async fn authorized_entries(
        &self,
        session: &str,
    ) -> Vec<(CallBinding, Vec<McpTool>)> {
        let candidates: Vec<_> = {
            let inner = self.inner.lock().await;
            inner
                .sessions
                .get(session)
                .into_iter()
                .flatten()
                .filter_map(|key| {
                    let entry = inner.entries.get(key)?;
                    Some((
                        key.clone(),
                        entry.cfg.clone(),
                        entry.generation,
                        entry.launch.clone()?,
                    ))
                })
                .collect()
        };
        let mut captured = Vec::new();
        for (key, cfg, generation, permit) in candidates {
            let Ok(admission) = permit.admit(&key, &cfg).await else {
                continue;
            };
            let mut inner = self.inner.lock().await;
            if inner
                .sessions
                .get(session)
                .is_some_and(|keys| keys.contains(&key))
                && let Some(entry) = inner.entries.get_mut(&key)
                && entry_ready(&key, entry)
                && entry.generation == generation
                && entry.cfg == cfg
                && entry.launch.as_ref().is_some_and(|current| {
                    current.id == permit.id
                        && current.authority.as_ref().map(|proof| proof.revision)
                            == permit.authority.as_ref().map(|proof| proof.revision)
                })
            {
                entry.last_used = self.tick();
                captured.push((
                    CallBinding {
                        function: String::new(),
                        tool: String::new(),
                        key: key.clone(),
                        generation,
                        config_revision: entry.config_revision,
                        policy_revision: entry.policy_revision,
                        tool_schema_revision: entry.tool_schema_revision,
                        cfg: entry.cfg.clone(),
                    },
                    entry.tools.clone(),
                ));
            }
            drop(inner);
            drop(admission);
        }
        captured
    }

    pub async fn invalidate_persistent(
        &self,
        keys: &[PoolKey],
    ) -> Result<Option<AuthorityGuard>, McpError> {
        if keys.is_empty() {
            return Ok(None);
        }
        let Some(store) = &self.authority else {
            return Ok(None);
        };
        store
            .revoke_batch(
                &keys.iter().map(authority_key).collect::<Vec<_>>(),
                &CancellationToken::new(),
            )
            .await
            .map(Some)
            .map_err(authority_error)
    }

    pub async fn revoke_launch_version(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<Option<AuthorityGuard>, McpError> {
        let Some(store) = &self.authority else {
            return Ok(None);
        };
        let resource = authority_key(key);
        let guard = store
            .read_guard(&resource, &CancellationToken::new())
            .await
            .map_err(authority_error)?;
        let version = super::super::launch_version(cfg);
        if guard.record.as_ref().is_some_and(|record| {
            record.decision == crate::core::trust::Decision::Grant && record.version != version
        }) {
            return Err(changed_error());
        }
        let revision = guard.record.as_ref().map(|record| record.revision);
        drop(guard);
        store
            .compare_exchange_guard(
                &resource,
                revision,
                &version,
                crate::core::trust::Decision::Revoke,
                &uuid::Uuid::new_v4().to_string(),
                &CancellationToken::new(),
            )
            .await
            .map(Some)
            .map_err(authority_error)
    }
}
