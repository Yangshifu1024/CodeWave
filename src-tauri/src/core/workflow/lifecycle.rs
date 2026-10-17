//! Recovery is display-only. Continuing explicitly starts a new root attempt.
use super::{
    service,
    state_machine::{NodeState, WorkflowRunSnapshot},
};
use crate::core::{
    agent::{AgentCore, SessionRuntime},
    delivery::{DeliveryService, types::*},
};
use std::{path::Path, sync::Arc};

pub fn validate_restore(
    core: &AgentCore,
    rt: &SessionRuntime,
    p: &service::PreparedWorkflow,
) -> Result<(), String> {
    let current = DeliveryService::roots_from_runtime(rt);
    if p.snapshot.root_locations.is_empty() {
        return Err(
            "Frozen root identities missing; re-adopt the task instead of inferring paths".into(),
        );
    }
    for (id, location) in &p.snapshot.root_locations {
        if current.get(id).is_none_or(|path| {
            crate::tools::pathutil::canonical_best_effort(path).to_string_lossy()
                != location.as_str()
        }) {
            return Err("Frozen root identity changed".into());
        }
    }
    let cfg = core.cfg.read().unwrap();
    super::snapshot::restore(
        &p.snapshot,
        &rt.id,
        rt.project_id.as_deref().unwrap_or("temporary"),
        &crate::agents::delegable()
            .iter()
            .map(|r| r.name.clone())
            .collect(),
        &cfg.providers
            .iter()
            .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
            .collect(),
    )
    .map_err(|e| e.message)?;
    let def = p
        .snapshot
        .definition
        .as_ref()
        .ok_or("Frozen definition is missing")?;
    if p.snapshot.definition_digest
        != super::super::delivery::version::digest_of_bytes(
            &serde_json::to_vec(def).map_err(|e| e.to_string())?,
        )
    {
        return Err(
            "Frozen definition hash mismatch; current configuration cannot replace it".into(),
        );
    }
    for policy in &def.model_policies {
        if !cfg.providers.iter().any(|provider| {
            provider.id == policy.provider_id
                && provider
                    .models
                    .iter()
                    .any(|m| m.id == policy.model_entry_id)
        }) {
            return Err("Frozen provider/model was removed".into());
        }
    }
    Ok(())
}
/// Journal is the commit point; the mutable snapshot is a replaceable projection.
pub fn save_state(
    data: &Path,
    task: &str,
    contract_revision: u64,
    state: &WorkflowRunSnapshot,
    source: &str,
) -> Result<(), String> {
    let dir = data
        .join("sessions")
        .join(format!("{}.workflow", state.owner.session_id));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!(
        "{task}.transition-r{contract_revision}-{:020}.json",
        state.revision
    ));
    let record = serde_json::to_vec(&serde_json::json!({"source":source,"operation_id":format!("{}:{}:{}",state.owner.root_run_id,contract_revision,state.revision),"state":state}))
        .map_err(|e| e.to_string())?;
    if path.exists() {
        let old = std::fs::read(&path).map_err(|e| e.to_string())?;
        if old != record {
            return Err("Transition revision conflict".into());
        }
    } else {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        f.write_all(&record)
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    crate::core::process_coordination::atomic_write(
        &dir.join(format!("{task}.state-r{contract_revision}.json")),
        &serde_json::to_vec(state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
pub fn replay(
    data: &Path,
    task: &str,
    revision: u64,
    state: &mut WorkflowRunSnapshot,
) -> Result<(), String> {
    let dir = data
        .join("sessions")
        .join(format!("{}.workflow", state.owner.session_id));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{task}.transition-r{revision}-"))
        })
        .map(|e| e.path())
        .collect();
    paths.sort();
    for path in paths {
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let next: WorkflowRunSnapshot =
            serde_json::from_value(record["state"].clone()).map_err(|e| e.to_string())?;
        if next.owner != state.owner {
            return Err("Transition owner mismatch".into());
        }
        if next.revision > state.revision {
            *state = next;
        }
    }
    Ok(())
}
pub fn interrupt(core: &AgentCore, rt: &SessionRuntime) -> Result<(), String> {
    if rt.running.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(());
    }
    let leases = crate::core::process_coordination::LeaseStore::new(&rt.data_dir.join("coord"));
    if !leases.leases_of(&rt.id).is_empty() {
        let _ = core.store.recover_session_owner_if_proven(&rt.id);
        if !leases.leases_of(&rt.id).is_empty() {
            return Err("Workflow owner/cleanup is not proven stopped".into());
        }
    }
    let Some(contract) = core.delivery.active_contract(&rt.id) else {
        return Ok(());
    };
    let key = crate::core::delivery::version::digest_of_bytes(
        format!("workflow_state:{}:{}", rt.id, contract.task_id).as_bytes(),
    );
    let _lock = crate::core::process_coordination::DiskLock::acquire(
        &rt.data_dir
            .join("coord/workflow-state")
            .join(format!("{key}.lock")),
    )
    .map_err(|e| e.to_string())?;
    let mut p = service::read(&rt.data_dir, &contract.owner, &contract.task_id)?;
    validate_restore(core, rt, &p)?;
    let mut changed = false;
    for node in p.state.nodes.values_mut() {
        if matches!(node.state, NodeState::Running | NodeState::Ready) {
            node.state = NodeState::Interrupted;
            node.reason = Some("Host stopped; explicit new attempt required".into());
            node.revision += 1;
            changed = true;
        }
    }
    if changed {
        p.state.revision += 1;
        save_state(
            &rt.data_dir,
            &contract.task_id,
            contract.revision,
            &p.state,
            "recovery",
        )?;
    }
    Ok(())
}
pub fn continue_task(
    core: &Arc<AgentCore>,
    rt: Arc<SessionRuntime>,
    task: &str,
    expected_revision: u64,
) -> Result<String, String> {
    let start_guard = core.start_gate.lock().unwrap();
    if rt.running.load(std::sync::atomic::Ordering::Acquire) {
        return Err("Session is already running".into());
    }
    if core
        .delivery
        .active_contract(&rt.id)
        .is_none_or(|c| c.task_id != task)
    {
        return Err("Workflow was superseded by another attempt".into());
    }
    let old = core
        .delivery
        .read_contract(&rt.id, task)
        .ok_or("Contract is missing")?;
    if old.revision != expected_revision {
        return Err("Contract revision changed".into());
    }
    let mut p = service::read(&rt.data_dir, &old.owner, task)?;
    validate_restore(core, &rt, &p)?;
    if p.state.nodes.values().all(|n| n.state == NodeState::Passed) {
        return Err("Completed workflow cannot be resumed".into());
    }
    let draft = ContractDraft {
        delivery_goal: old.delivery_goal,
        requirements: old.requirements.clone(),
        artifacts: old.artifacts.clone(),
        checks: old.checks.clone(),
        review_policy: old.review_policy.clone(),
        roots: DeliveryService::roots_from_runtime(&rt)
            .into_iter()
            .map(|(root_id, path)| RootIdentity {
                root_id: root_id.clone(),
                absolute: path.to_string_lossy().into_owned(),
                kind: match root_id.as_str() {
                    "workspace" => RootKind::Workspace,
                    "data_dir" => RootKind::DataDir,
                    "project_dir" => RootKind::ProjectDir,
                    _ => RootKind::Extra,
                },
            })
            .collect(),
        scope_changes: vec![],
    };
    p.resumed_from = Some((task.into(), old.owner.root_run_id.clone()));
    p.resume_budget = Some(core.delivery.load_budget(&rt.id, task));
    *rt.pending_contract.lock().unwrap() = Some(draft);
    *rt.pending_workflow.lock().unwrap() = Some(p);
    // Continuing never restores a former FullAccess approval or responder.
    core.set_session_mode_and_persist(&rt, crate::core::prefs::ApprovalMode::Plan);
    let result=core.start_chat_locked(rt.clone(),format!("Continue workflow {task} as a new attempt using the frozen definition. Re-evaluate all evidence and request current approval before writes."),vec![],None,&start_guard).map_err(|e|e.to_string());
    if result.is_err() {
        rt.pending_contract.lock().unwrap().take();
        rt.pending_workflow.lock().unwrap().take();
    }
    result
}

/// Re-adoption records host routing provenance, never former user waivers or approvals.
pub fn record_resume_provenance(
    p: &service::PreparedWorkflow,
    contract: &mut TaskContract,
) -> Result<(), String> {
    if !contract.review_policy.required
        && contract.review_policy.independent
        && contract
            .checks
            .iter()
            .all(|check| check.kind != CheckKind::Review)
        && contract.scope_changes.is_empty()
    {
        let (task, run) = p.resumed_from.as_ref().ok_or("Resume provenance missing")?;
        let target = contract
            .requirements
            .first()
            .ok_or("Resume requirement missing")?;
        contract.scope_changes.push(ScopeChange {
            id: "host-resumed-routing".into(), target_id: target.id.clone(),
            kind: ScopeChangeKind::Added,
            reason: format!("Host re-adopted frozen {:?} routing from task {task}, root {run}; no former waiver or approval restored", p.profile.kind),
            user_decision_id: None,
            at: chrono::Utc::now().to_rfc3339(),
        });
    }
    Ok(())
}
