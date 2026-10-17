//! Read-only workflow view shared by desktop panels and activity snapshots.
use super::{project, service};
use crate::core::agent::{AgentCore, SessionRuntime};
use serde::Serialize;
#[derive(Debug, Serialize)]
pub struct Projection {
    pub task_id: String,
    pub contract_revision: u64,
    pub root_run_id: String,
    pub route: super::routing::TaskProfile,
    pub units: Vec<super::routing::WorkUnit>,
    pub nodes: Vec<super::state_machine::Node>,
    pub checks: Vec<crate::core::delivery::types::CheckSpec>,
    pub leases: Vec<super::dispatch::DispatchLease>,
    pub frozen_revision: u64,
    pub frozen_digest: String,
    pub source_digest: Option<String>,
    pub candidate: Option<project::ProjectView>,
    pub resume_allowed: bool,
    pub recovery_error: Option<String>,
    pub design: super::design::DesignDecision,
}
pub fn read(core: &AgentCore, rt: &SessionRuntime) -> Result<Option<Projection>, String> {
    let Some(contract) = core.delivery.active_contract(&rt.id) else {
        return Ok(None);
    };
    let p = service::read(&rt.data_dir, &contract.owner, &contract.task_id)?;
    let recovery_error = super::lifecycle::validate_restore(core, rt, &p).err();
    let candidate = if rt.project_id.is_some() {
        Some(project::read(
            &rt.data_dir,
            &rt.workspace,
            &core.cfg.read().unwrap(),
        )?)
    } else {
        None
    };
    Ok(Some(Projection {
        task_id: contract.task_id,
        contract_revision: contract.revision,
        root_run_id: contract.owner.root_run_id.clone(),
        route: p.profile,
        units: p.units,
        nodes: p.state.nodes.into_values().collect(),
        checks: contract.checks,
        leases: super::dispatch::active_leases()
            .into_iter()
            .filter(|l| l.root_run_id == contract.owner.root_run_id)
            .collect(),
        frozen_revision: p
            .snapshot
            .definition
            .as_ref()
            .map(|d| d.revision)
            .unwrap_or(0),
        source_digest: p.snapshot.resource_hashes.get("project_workflow").cloned(),
        frozen_digest: p.snapshot.definition_digest,
        candidate,
        resume_allowed: recovery_error.is_none()
            && !rt.running.load(std::sync::atomic::Ordering::Acquire),
        recovery_error,
        design: p.design,
    }))
}
pub fn override_route(
    _core: &AgentCore,
    rt: &SessionRuntime,
    task: &str,
    revision: u64,
    kind: super::routing::TaskKind,
    reason: &str,
) -> Result<serde_json::Value, String> {
    let owner = crate::core::delivery::DeliveryService::identity_of(rt);
    service::record_override(&rt.data_dir, &owner, task, revision, kind, reason)
}

/// Decorate the bounded process overview using the same persisted stage state as the right bar.
pub fn activity_snapshot(core: &AgentCore) -> crate::core::activity::ActivitySnapshot {
    let mut snapshot = core.activity.snapshot();
    for session in &mut snapshot.sessions {
        if let Some(contract) = core.delivery.active_contract(&session.session_id)
            && contract.owner.root_run_id == session.run_id
        {
            session.workflow = match service::read(
                &core.data_dir,
                &contract.owner,
                &contract.task_id,
            ) {
                Ok(p) => Some(
                    serde_json::json!({"task_id":contract.task_id,"revision":p.state.revision,"nodes":p.state.nodes.values().map(|n|serde_json::json!({"id":n.unit_id,"state":n.state,"attempt":n.attempt,"reason":n.reason})).collect::<Vec<_>>()}),
                ),
                Err(e) => Some(serde_json::json!({"blocked":e})),
            };
        }
    }
    snapshot
}
