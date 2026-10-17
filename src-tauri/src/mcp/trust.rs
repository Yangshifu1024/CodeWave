//! Host-owned launch grants. Configuration files cannot authorize their own execution.
//! Local tickets are not serialized. Remembered permissions come from the
//! independent authority store; probes use bounded, single-use confirmations.

use super::{McpError, McpServerConfig, PoolKey};
use crate::core::trust::{AuthorityGuard, TrustKey, TrustStore};

pub(super) enum AdmissionError {
    Denied,
    Trust(crate::core::trust::TrustError),
    Local(McpError),
}
impl AdmissionError {
    pub fn is_denied(&self) -> bool {
        matches!(
            self,
            Self::Denied | Self::Trust(crate::core::trust::TrustError::Denied)
        )
    }
}
impl From<crate::core::trust::TrustError> for AdmissionError {
    fn from(error: crate::core::trust::TrustError) -> Self {
        Self::Trust(error)
    }
}
impl From<McpError> for AdmissionError {
    fn from(error: McpError) -> Self {
        Self::Local(error)
    }
}
impl From<AdmissionError> for McpError {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Denied => untrusted(),
            AdmissionError::Trust(error) => authority_error(error),
            AdmissionError::Local(error) => error,
        }
    }
}
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub(super) fn authority_key(key: &PoolKey) -> TrustKey {
    TrustKey {
        kind: "mcp".into(),
        purpose: "launch".into(),
        source: key.source.clone(),
        owner: format!(
            "{}:{}",
            key.scope_str(),
            key.project_id.as_deref().unwrap_or("")
        ),
        name: key.name.clone(),
    }
}

pub(super) fn authority_error(error: crate::core::trust::TrustError) -> McpError {
    McpError::config_hint(
        error.to_string(),
        "Review the resource in Settings → MCP. Persistent trust never falls back to a local grant.",
    )
}

#[derive(Clone)]
pub(super) struct AuthorityProof {
    pub store: Arc<TrustStore>,
    pub revision: u64,
}

pub fn launch_version(cfg: &McpServerConfig) -> String {
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    let value = canonical(serde_json::to_value(cfg).expect("MCP configuration is JSON"));
    super::scope_version(&value.to_string())
}

pub(super) fn untrusted() -> McpError {
    McpError::config_hint(
        "MCP launch requires approval for this source and configuration version",
        "Review this server in Settings → MCP and approve once or remember this exact version.",
    )
}

#[derive(Clone)]
pub(super) struct LaunchPermit {
    pub id: String,
    key: PoolKey,
    version: String,
    pub cancel: CancellationToken,
    pub authority: Option<AuthorityProof>,
}

impl LaunchPermit {
    fn new(key: &PoolKey, cfg: &McpServerConfig) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            key: key.clone(),
            version: launch_version(cfg),
            cancel: CancellationToken::new(),
            authority: None,
        }
    }

    pub fn check(&self, key: &PoolKey, cfg: &McpServerConfig) -> Result<(), McpError> {
        if self.key != *key || self.version != launch_version(cfg) || self.cancel.is_cancelled() {
            return Err(untrusted());
        }
        Ok(())
    }

    pub async fn admit(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<Option<AuthorityGuard>, AdmissionError> {
        self.check(key, cfg)?;
        let Some(proof) = &self.authority else {
            return Ok(None);
        };
        let guard = proof
            .store
            .admit(
                &authority_key(key),
                &self.version,
                Some(proof.revision),
                &self.cancel,
            )
            .await?;
        // Cooperating saves take the authority barrier before publishing a new
        // document. External edits are also rejected when observed here.
        Self::check_source(key, cfg)?;
        self.check(key, cfg)?;
        Ok(Some(guard))
    }

    pub fn check_source(key: &PoolKey, cfg: &McpServerConfig) -> Result<(), McpError> {
        let source = super::ScopeRef {
            scope: key.scope,
            path: key.source.clone(),
        };
        let doc = super::load_scope(&source);
        if !doc
            .servers
            .iter()
            .any(|(name, current)| name == &key.name && current == cfg && current.enabled)
            || doc.issues.iter().any(|issue| {
                issue.level == super::IssueLevel::Error
                    && issue.server.as_deref().is_none_or(|name| name == key.name)
            })
        {
            return Err(untrusted());
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct LaunchStatus {
    pub version: String,
    pub trusted: bool,
    pub remembered: bool,
    pub revision: Option<u64>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ProbeChallenge {
    pub id: String,
    pub version: String,
}

struct Probe {
    permit: LaunchPermit,
    issued: Instant,
}

#[derive(Default)]
struct Inner {
    grants: HashMap<PoolKey, LaunchPermit>,
    probes: HashMap<String, Probe>,
    active_probes: HashMap<String, LaunchPermit>,
}

#[derive(Default)]
pub(super) struct LaunchRegistry(Mutex<Inner>);

pub(super) struct ProbeLease<'a> {
    pub permit: LaunchPermit,
    registry: &'a LaunchRegistry,
    id: String,
}

impl Drop for ProbeLease<'_> {
    fn drop(&mut self) {
        if let Some(permit) = self
            .registry
            .0
            .lock()
            .unwrap()
            .active_probes
            .remove(&self.id)
        {
            permit.cancel.cancel();
        }
    }
}

impl LaunchRegistry {
    pub fn status(&self, key: &PoolKey, cfg: &McpServerConfig) -> LaunchStatus {
        LaunchStatus {
            version: launch_version(cfg),
            trusted: self.connection(key, cfg).is_ok(),
            remembered: self
                .connection(key, cfg)
                .ok()
                .is_some_and(|permit| permit.authority.is_some()),
            revision: self
                .connection(key, cfg)
                .ok()
                .and_then(|permit| permit.authority.map(|proof| proof.revision)),
            error: None,
        }
    }

    pub fn connection(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<LaunchPermit, McpError> {
        let inner = self.0.lock().unwrap();
        let permit = inner.grants.get(key).ok_or_else(untrusted)?;
        permit.check(key, cfg)?;
        Ok(permit.clone())
    }

    pub fn grant(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
        expected: &str,
    ) -> Result<(), McpError> {
        if launch_version(cfg) != expected {
            return Err(untrusted());
        }
        let mut inner = self.0.lock().unwrap();
        if let Some(permit) = inner.grants.get_mut(key)
            && permit.check(key, cfg).is_ok()
        {
            // Only an explicit new host approval calls grant; never use this
            // path as a fallback when persistent authority cannot be read.
            permit.authority = None;
            return Ok(());
        }
        if let Some(old) = inner
            .grants
            .insert(key.clone(), LaunchPermit::new(key, cfg))
        {
            old.cancel.cancel();
        }
        Ok(())
    }

    pub fn grant_persistent(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
        proof: AuthorityProof,
    ) -> Result<LaunchPermit, McpError> {
        let mut inner = self.0.lock().unwrap();
        if let Some(permit) = inner.grants.get_mut(key)
            && permit.check(key, cfg).is_ok()
        {
            permit.authority = Some(proof);
            return Ok(permit.clone());
        }
        let mut permit = LaunchPermit::new(key, cfg);
        permit.authority = Some(proof);
        if let Some(old) = inner.grants.insert(key.clone(), permit.clone()) {
            old.cancel.cancel();
        }
        Ok(permit)
    }

    pub fn revoke(&self, key: &PoolKey) {
        let mut inner = self.0.lock().unwrap();
        Self::revoke_locked(&mut inner, key);
    }

    pub fn revoke_if_current(&self, key: &PoolKey, id: &str, revision: Option<u64>) -> bool {
        let mut inner = self.0.lock().unwrap();
        if !inner.grants.get(key).is_some_and(|permit| {
            permit.id == id && permit.authority.as_ref().map(|proof| proof.revision) == revision
        }) {
            return false;
        }
        Self::revoke_locked(&mut inner, key);
        true
    }

    fn revoke_locked(inner: &mut Inner, key: &PoolKey) {
        if let Some(permit) = inner.grants.remove(key) {
            permit.cancel.cancel();
        }
        inner.probes.retain(|_, probe| {
            if probe.permit.key != *key {
                return true;
            }
            probe.permit.cancel.cancel();
            false
        });
        for permit in inner
            .active_probes
            .values()
            .filter(|permit| permit.key == *key)
        {
            permit.cancel.cancel();
        }
    }

    pub fn invalidate_source(
        &self,
        source: &std::path::Path,
        servers: &[(String, McpServerConfig)],
    ) {
        let inner = self.0.lock().unwrap();
        let keys: Vec<_> = inner
            .grants
            .values()
            .chain(inner.probes.values().map(|probe| &probe.permit))
            .chain(inner.active_probes.values())
            .filter(|permit| {
                permit.key.source == source
                    && !servers.iter().any(|(name, cfg)| {
                        *name == permit.key.name
                            && cfg.enabled
                            && permit.check(&permit.key, cfg).is_ok()
                    })
            })
            .map(|permit| permit.key.clone())
            .collect();
        drop(inner);
        for key in keys {
            self.revoke(&key);
        }
    }

    /// A user-approved policy-only patch keeps the original launch identity.
    pub fn rebind_policy(&self, key: &PoolKey, before: &McpServerConfig, after: &McpServerConfig) {
        let mut allowed = before.clone();
        allowed.always_allow = after.always_allow;
        if allowed != *after {
            return;
        }
        let mut inner = self.0.lock().unwrap();
        if let Some(permit) = inner.grants.get_mut(key)
            && permit.check(key, before).is_ok()
        {
            permit.version = launch_version(after);
        }
    }

    pub fn prepare_probe(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<ProbeChallenge, McpError> {
        let mut inner = self.0.lock().unwrap();
        inner
            .probes
            .retain(|_, probe| probe.issued.elapsed() < Duration::from_secs(300));
        if inner.probes.len() + inner.active_probes.len() >= 64 {
            return Err(McpError::config("Too many pending or active MCP probes"));
        }
        let permit = LaunchPermit::new(key, cfg);
        let result = ProbeChallenge {
            id: uuid::Uuid::new_v4().to_string(),
            version: permit.version.clone(),
        };
        inner.probes.insert(
            result.id.clone(),
            Probe {
                permit,
                issued: Instant::now(),
            },
        );
        Ok(result)
    }

    pub fn confirm_probe(
        &self,
        id: &str,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<ProbeLease<'_>, McpError> {
        let mut inner = self.0.lock().unwrap();
        let probe = inner.probes.remove(id).ok_or_else(untrusted)?;
        if probe.issued.elapsed() >= Duration::from_secs(300) {
            return Err(untrusted());
        }
        probe.permit.check(key, cfg)?;
        inner.active_probes.insert(id.into(), probe.permit.clone());
        Ok(ProbeLease {
            permit: probe.permit,
            registry: self,
            id: id.into(),
        })
    }
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
