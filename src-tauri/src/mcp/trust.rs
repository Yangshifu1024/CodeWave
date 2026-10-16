//! Host-owned launch grants. Configuration files cannot authorize their own execution.
//! Grants last for this process; probes use bounded, single-use confirmations.

use super::{McpError, McpServerConfig, PoolKey};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

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
        "Review this server in Settings → MCP and approve its launch. Grants expire when CodeWave exits.",
    )
}

#[derive(Clone)]
pub(super) struct LaunchPermit {
    key: PoolKey,
    version: String,
    pub cancel: CancellationToken,
}

impl LaunchPermit {
    fn new(key: &PoolKey, cfg: &McpServerConfig) -> Self {
        Self {
            key: key.clone(),
            version: launch_version(cfg),
            cancel: CancellationToken::new(),
        }
    }

    pub fn check(&self, key: &PoolKey, cfg: &McpServerConfig) -> Result<(), McpError> {
        if self.key != *key || self.version != launch_version(cfg) || self.cancel.is_cancelled() {
            return Err(untrusted());
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct LaunchStatus {
    pub version: String,
    pub trusted: bool,
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
        if inner
            .grants
            .get(key)
            .is_some_and(|permit| permit.check(key, cfg).is_ok())
        {
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

    pub fn revoke(&self, key: &PoolKey) {
        let mut inner = self.0.lock().unwrap();
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
