//! Host-only version review. Model tools and permission modes cannot grant trust.
use super::*;
use crate::core::trust::{Decision, TrustError};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Once,
    Remember,
    Revoke,
}

#[derive(Clone)]
pub(super) struct PendingReview {
    scope: ResourceScope,
    resource: InstructionResource,
    action: Action,
    revision: Option<u64>,
    issued: Instant,
    consumed: bool,
    cancelled: bool,
    applied: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceStatus {
    pub trusted: bool,
    pub remembered: bool,
    pub revision: Option<u64>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ResourceView {
    pub available: bool,
    pub id: String,
    pub name: String,
    pub kind: InstructionKind,
    pub origin: ResourceOrigin,
    pub status: ResourceStatus,
    pub active_versions: Vec<ResourceUse>,
}

#[derive(Serialize)]
pub struct Inspection {
    pub resources: Vec<ResourceView>,
    pub issues: Vec<String>,
}

#[derive(Serialize)]
pub struct Challenge {
    pub id: String,
    pub resource_id: String,
    pub name: String,
    pub origin: ResourceOrigin,
    pub text: String,
    pub action: Action,
}

#[derive(Serialize)]
pub struct Receipt {
    pub operation_id: String,
    pub confirmed: bool,
    pub resolved: bool,
    pub error: Option<String>,
}

fn resource(
    core: &AgentCore,
    scope: &ResourceScope,
    id: &str,
    version: &str,
) -> Result<InstructionResource, String> {
    collect(core, scope)
        .resources
        .into_iter()
        .find(|resource| {
            resource.id == id && resource.origin.version == version && !resource.builtin()
        })
        .ok_or_else(|| "Resource source, version or enabled state changed; review again".into())
}

fn valid(review: &PendingReview) -> bool {
    !review.cancelled && review.issued.elapsed() < Duration::from_secs(300)
}

impl InstructionTrust {
    fn revoke_local(&self, id: &str) {
        self.local
            .lock()
            .unwrap()
            .retain(|(_, resource), _| resource != id);
        for review in self.reviews.lock().unwrap().values_mut().filter(|review| {
            review.resource.id == id && !review.applied && review.action != Action::Revoke
        }) {
            review.cancelled = true;
        }
    }
}

pub async fn inspect(core: &AgentCore, session: Option<&str>) -> Result<Inspection, String> {
    let scope = ResourceScope::host(core, session)?;
    let snapshot = collect(core, &scope);
    let statuses = core
        .instructions
        .statuses(&scope, &snapshot.resources, &CancellationToken::new())
        .await;
    let mut resources = Vec::new();
    let active: Vec<_> = core
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .chain(core.subs.iter().map(|entry| entry.value().clone()))
        .filter(|rt| rt.running.load(std::sync::atomic::Ordering::SeqCst))
        .flat_map(|rt| rt.instruction_usage.lock().unwrap().clone())
        .filter(|usage| usage.scope == scope.authority_owner())
        .collect();
    for (resource, status) in snapshot.resources.into_iter().zip(statuses) {
        let mut active_versions = Vec::new();
        active_versions.extend(
            active
                .iter()
                .filter(|usage| usage.id == resource.id)
                .cloned(),
        );
        active_versions.sort_by(|a, b| a.version.cmp(&b.version));
        active_versions.dedup_by(|a, b| a.version == b.version);
        resources.push(ResourceView {
            available: true,
            status,
            id: resource.id,
            name: resource.name,
            kind: resource.kind,
            origin: resource.origin,
            active_versions,
        });
    }
    for usage in active {
        if let Some(resource) = resources
            .iter_mut()
            .find(|resource| resource.id == usage.id)
        {
            if !resource.available
                && !resource
                    .active_versions
                    .iter()
                    .any(|version| version.version == usage.version)
            {
                resource.active_versions.push(usage);
            }
            continue;
        }
        resources.push(ResourceView {
            available: false,
            id: usage.id.clone(),
            name: usage.name.clone(),
            kind: usage.kind,
            origin: usage.origin.clone(),
            status: ResourceStatus {
                trusted: false,
                remembered: false,
                revision: None,
                error: Some(
                    "Source is absent or disabled; an active run still has the captured version"
                        .into(),
                ),
            },
            active_versions: vec![usage],
        });
    }
    Ok(Inspection {
        resources,
        issues: snapshot.issues,
    })
}

pub async fn prepare(
    core: &AgentCore,
    session: Option<&str>,
    id: &str,
    version: &str,
    action: Action,
) -> Result<Challenge, String> {
    if action == Action::Once && session.is_none() {
        return Err("Select a session before allowing a temporary instruction version".into());
    }
    let scope = ResourceScope::host(core, session)?;
    let resource = resource(core, &scope, id, version)?;
    let revision = if action == Action::Once {
        None
    } else {
        let read = match &core.instructions.store {
            Some(store) => {
                store
                    .read_guard(&resource.key, &CancellationToken::new())
                    .await
            }
            None => Err(TrustError::Unavailable),
        };
        match read {
            Ok(guard) => guard.record.as_ref().map(|record| record.revision),
            // Preparing is still read-only. Confirmation can withdraw local
            // permission even when persistent revocation must report failure.
            Err(_) if action == Action::Revoke => None,
            Err(error) => return Err(error.to_string()),
        }
    };
    if ResourceScope::host(core, session)? != scope {
        return Err("Resource scope changed".into());
    }
    let mut reviews = core.instructions.reviews.lock().unwrap();
    reviews.retain(|_, review| review.issued.elapsed() < Duration::from_secs(300));
    if reviews.len() >= 64 {
        return Err("Too many resource reviews; close an existing review".into());
    }
    let ticket = uuid::Uuid::new_v4().to_string();
    reviews.insert(
        ticket.clone(),
        PendingReview {
            scope,
            resource: resource.clone(),
            action,
            revision,
            issued: Instant::now(),
            consumed: false,
            cancelled: false,
            applied: false,
        },
    );
    Ok(Challenge {
        id: ticket,
        resource_id: resource.id,
        name: resource.name,
        origin: resource.origin,
        text: resource.text,
        action,
    })
}

pub async fn confirm(core: &AgentCore, session: Option<&str>, id: &str) -> Result<Receipt, String> {
    // Consume before scope validation so a failed or misplaced confirmation
    // cannot become a reusable authorization token.
    let review = {
        let mut reviews = core.instructions.reviews.lock().unwrap();
        let review = reviews.get_mut(id).ok_or("Resource review missing")?;
        if review.consumed || !valid(review) {
            return Err("Resource review expired or already used".into());
        }
        review.consumed = true;
        review.clone()
    };
    let scope = ResourceScope::host(core, session)?;
    if scope != review.scope {
        return Err("Resource scope changed".into());
    }
    resource(
        core,
        &scope,
        &review.resource.id,
        &review.resource.origin.version,
    )?;
    if review.action == Action::Revoke {
        let _gate = core.instructions.gate.lock().await;
        core.instructions.revoke_local(&review.resource.id);
    }
    let authority = if review.action == Action::Once {
        None
    } else {
        let store = core
            .instructions
            .store
            .as_ref()
            .ok_or("Trust authority unavailable")?;
        match store
            .compare_exchange_guard(
                &review.resource.key,
                review.revision,
                &review.resource.origin.version,
                if review.action == Action::Revoke {
                    Decision::Revoke
                } else {
                    Decision::Grant
                },
                id,
                &CancellationToken::new(),
            )
            .await
        {
            Ok(guard) => Some(guard),
            Err(TrustError::OutcomeUnknown) => {
                return Ok(Receipt {
                    operation_id: id.into(),
                    confirmed: false,
                    resolved: false,
                    error: Some(TrustError::OutcomeUnknown.to_string()),
                });
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    let gate = core.instructions.gate.lock().await;
    let current = core
        .instructions
        .reviews
        .lock()
        .unwrap()
        .get(id)
        .is_some_and(valid);
    let unchanged = current
        && ResourceScope::host(core, session).is_ok_and(|current| current == scope)
        && resource(
            core,
            &scope,
            &review.resource.id,
            &review.resource.origin.version,
        )
        .is_ok();
    if !unchanged {
        let revision = authority
            .as_ref()
            .and_then(|guard| guard.record.as_ref())
            .map(|record| record.revision);
        drop(gate);
        drop(authority);
        if review.action == Action::Remember {
            let result = core
                .instructions
                .store
                .as_ref()
                .ok_or("Trust authority unavailable")?
                .compare_exchange(
                    &review.resource.key,
                    revision,
                    &review.resource.origin.version,
                    Decision::Revoke,
                    &uuid::Uuid::new_v4().to_string(),
                    &CancellationToken::new(),
                )
                .await;
            if let Err(error) = result
                && error != TrustError::Conflict
            {
                return Err(error.to_string());
            }
        }
        return Err("Resource changed or review was revoked during confirmation".into());
    }
    match review.action {
        Action::Once => {
            core.instructions.local.lock().unwrap().insert(
                (scope.owner, review.resource.id.clone()),
                review.resource.origin.version,
            );
        }
        Action::Remember => {
            core.instructions
                .local
                .lock()
                .unwrap()
                .remove(&(scope.owner, review.resource.id.clone()));
        }
        Action::Revoke => core.instructions.revoke_local(&review.resource.id),
    }
    if let Some(review) = core.instructions.reviews.lock().unwrap().get_mut(id) {
        review.applied = true;
    }
    Ok(Receipt {
        operation_id: id.into(),
        confirmed: true,
        resolved: true,
        error: None,
    })
}

pub async fn operation(
    core: &AgentCore,
    session: Option<&str>,
    id: &str,
) -> Result<Receipt, String> {
    let scope = ResourceScope::host(core, session)?;
    let review = core
        .instructions
        .reviews
        .lock()
        .unwrap()
        .get(id)
        .filter(|review| {
            review.consumed
                && review.scope == scope
                && review.issued.elapsed() < Duration::from_secs(300)
        })
        .cloned()
        .ok_or("Resource operation expired or scope changed")?;
    if review.applied {
        return Ok(Receipt {
            operation_id: id.into(),
            confirmed: true,
            resolved: true,
            error: None,
        });
    }
    let store = core
        .instructions
        .store
        .as_ref()
        .ok_or("Trust authority unavailable")?;
    let authority = store
        .read_guard(&review.resource.key, &CancellationToken::new())
        .await
        .map_err(|error| error.to_string())?;
    let matches = authority.record.as_ref().is_some_and(|record| {
        record.operation_id == id
            && record.version == review.resource.origin.version.trim_start_matches("sha256:")
            && record.decision
                == if review.action == Action::Revoke {
                    Decision::Revoke
                } else {
                    Decision::Grant
                }
            && review.revision.unwrap_or(0).checked_add(1) == Some(record.revision)
    });
    if !matches {
        return Ok(Receipt {
            operation_id: id.into(),
            confirmed: false,
            resolved: true,
            error: Some(
                "Operation was not confirmed or was superseded; nothing was retried".into(),
            ),
        });
    }
    let _gate = core.instructions.gate.lock().await;
    if ResourceScope::host(core, session)? != scope {
        return Err("Resource scope changed".into());
    }
    resource(
        core,
        &scope,
        &review.resource.id,
        &review.resource.origin.version,
    )?;
    let applied = core
        .instructions
        .reviews
        .lock()
        .unwrap()
        .get(id)
        .is_some_and(|review| review.applied);
    if !applied {
        if review.action == Action::Revoke {
            core.instructions.revoke_local(&review.resource.id);
        } else if !core
            .instructions
            .reviews
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(valid)
        {
            return Err("Resource review was revoked".into());
        }
        if let Some(review) = core.instructions.reviews.lock().unwrap().get_mut(id) {
            review.applied = true;
        }
    }
    Ok(Receipt {
        operation_id: id.into(),
        confirmed: true,
        resolved: true,
        error: None,
    })
}

pub fn discard(core: &AgentCore, id: &str) {
    let mut reviews = core.instructions.reviews.lock().unwrap();
    if reviews
        .get(id)
        .is_some_and(|review| !review.consumed || review.applied)
    {
        reviews.remove(id);
    }
}
