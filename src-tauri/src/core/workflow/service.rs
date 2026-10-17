//! Production composition for routing, verification policy, design and frozen workflow state.
use super::{config::*, design, routing::*, snapshot, state_machine::*};
use crate::core::delivery::{types::*, version};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedWorkflow {
    #[serde(default)]
    pub resumed_from: Option<(String, String)>,
    #[serde(default)]
    pub resume_budget: Option<crate::core::delivery::completion::RepairBudget>,
    pub task_id: String,
    pub profile: TaskProfile,
    pub units: Vec<WorkUnit>,
    pub design: design::DesignDecision,
    #[serde(default)]
    pub unit_designs: BTreeMap<String, design::DesignDecision>,
    pub snapshot: snapshot::FrozenSnapshot,
    pub state: WorkflowRunSnapshot,
}
impl PreparedWorkflow {
    pub fn validate_models(&self, cfg: &crate::core::config::ConfigState) -> Result<(), String> {
        let def = self
            .snapshot
            .definition
            .as_ref()
            .ok_or("Frozen definition missing")?;
        for policy in &def.model_policies {
            if !cfg.providers.iter().any(|p| {
                p.id == policy.provider_id && p.models.iter().any(|m| m.id == policy.model_entry_id)
            }) {
                return Err("Frozen provider/model policy is unavailable".into());
            }
        }
        Ok(())
    }

    pub fn rebind(&self, contract: &TaskContract) -> Self {
        let mut next = self.clone();
        next.task_id = contract.task_id.clone();
        next.snapshot.owner = contract.owner.clone();
        next.snapshot.contract_revision = contract.revision;
        next.state.owner = contract.owner.clone();
        for node in next.state.nodes.values_mut() {
            node.state = NodeState::Pending;
            node.attempt += 1;
            node.actor_id = None;
            node.lease_generation = 0;
            node.reason = None;
        }
        next.state.revision += 1;
        if let Some(root) = next.state.nodes.get_mut("implementation") {
            root.state = NodeState::Running;
            root.actor_id = Some(contract.owner.actor_id.clone());
        }
        next
    }
}
fn file(data_dir: &Path, session: &str, task: &str) -> Result<PathBuf, String> {
    if [session, task].iter().any(|id| {
        id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }) {
        return Err("无效的会话/任务身份".into());
    }
    Ok(data_dir
        .join("sessions")
        .join(format!("{session}.workflow"))
        .join(format!("{task}.json")))
}
pub fn persist(data_dir: &Path, prepared: &PreparedWorkflow) -> Result<(), String> {
    let path = file(
        data_dir,
        &prepared.snapshot.owner.session_id,
        &prepared.task_id,
    )?
    .with_extension(format!("r{}.json", prepared.snapshot.contract_revision));
    if path.exists() {
        let existing: PreparedWorkflow =
            serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if existing.task_id == prepared.task_id
            && existing.snapshot.owner == prepared.snapshot.owner
            && existing.snapshot.contract_revision == prepared.snapshot.contract_revision
            && existing.snapshot.definition_digest == prepared.snapshot.definition_digest
            && existing.units == prepared.units
        {
            return Ok(());
        }
        return Err("冻结快照已存在且内容不一致，不允许覆盖历史运行规则".into());
    }
    let bytes = serde_json::to_vec_pretty(prepared).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    use std::io::Write;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    output
        .write_all(&bytes)
        .and_then(|_| output.sync_all())
        .map_err(|e| e.to_string())
}
pub fn read(data_dir: &Path, owner: &RunIdentity, task: &str) -> Result<PreparedWorkflow, String> {
    let base = file(data_dir, &owner.session_id, task)?;
    // Only a published delivery revision is executable. Prepared snapshots may remain
    // after a failed publication and must never supersede the live contract.
    let contract = crate::core::delivery::DeliveryService::from_data_root(data_dir.to_path_buf())
        .read_contract(&owner.session_id, task)
        .ok_or("工作流缺少已发布任务契约")?;
    if contract.owner.session_id != owner.session_id
        || contract.owner.root_run_id != owner.root_run_id
    {
        return Err("工作流契约归属与当前运行不匹配".into());
    }
    let bytes = std::fs::read(base.with_extension(format!("r{}.json", contract.revision)))
        .map_err(|e| e.to_string())?;
    let p: PreparedWorkflow = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if p.snapshot.contract_revision != contract.revision
        || p.snapshot.owner.session_id != owner.session_id
        || p.snapshot.owner.root_run_id != owner.root_run_id
    {
        return Err("工作流快照归属与当前运行不匹配".into());
    }
    let mut p = p;
    let plan_path = file(data_dir, &owner.session_id, task)?
        .with_extension(format!("plan-r{}.json", p.snapshot.contract_revision));
    if plan_path.exists() {
        let plan: PreparedWorkflow =
            serde_json::from_slice(&std::fs::read(plan_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if plan.snapshot.owner != p.snapshot.owner
            || plan.snapshot.contract_revision != p.snapshot.contract_revision
        {
            return Err("工作单元计划归属错误".into());
        }
        p = plan;
    }
    let state_path = file(data_dir, &owner.session_id, task)?
        .with_extension(format!("state-r{}.json", p.snapshot.contract_revision));
    if state_path.exists() {
        let state: WorkflowRunSnapshot =
            serde_json::from_slice(&std::fs::read(state_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if state.owner != p.state.owner {
            return Err("工作流状态归属错误".into());
        }
        p.state = state;
    }
    super::lifecycle::replay(data_dir, task, contract.revision, &mut p.state)?;
    let mut overrides = Vec::<ProfileOverride>::new();
    if let Ok(entries) =
        std::fs::read_dir(file(data_dir, &owner.session_id, task)?.parent().unwrap())
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{task}-")) && name.ends_with(".override.json") {
                let record: ProfileOverride = serde_json::from_slice(
                    &std::fs::read(entry.path()).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if record.contract_revision == p.snapshot.contract_revision
                    && record.actor_id == "user"
                {
                    overrides.push(record);
                }
            }
        }
    }
    overrides.sort_by(|a, b| a.at.cmp(&b.at));
    if let Some(record) = overrides.pop() {
        let mut decision = RouteDecision {
            allows_parallel: p.profile.kind.may_parallel(),
            profile: p.profile.clone(),
            downgraded_because: None,
        };
        super::routing::apply_override(&mut decision, record, &contract, owner)?;
        p.profile = decision.profile;
    }
    let design_path = file(data_dir, &owner.session_id, task)?
        .with_extension(format!("design-r{}.json", p.snapshot.contract_revision));
    if design_path.exists() {
        p.design = serde_json::from_slice(&std::fs::read(design_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    for (unit, decision) in &mut p.unit_designs {
        let path = base.with_extension(format!("design-{unit}-r{}.json", contract.revision));
        if path.exists() {
            *decision = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
    }
    // A real user may choose the mechanical path, but host facts still retain
    // design gates for shared effects, multiple units, and risky paths.
    if p.profile.kind == TaskKind::Mechanical
        && p.profile
            .override_record
            .as_ref()
            .is_some_and(|r| r.actor_id == "user")
        && p.units.len() == 1
        && p.profile.surfaces.len() == 1
        && p.units[0].read_scopes.is_empty()
        && p.units[0].depends_on.is_empty()
        && p.profile.shared_resources.is_empty()
        && p.units[0].mutable_resources.is_empty()
    {
        let scopes: Vec<_> = p
            .units
            .iter()
            .flat_map(|u| u.write_scopes.iter().cloned())
            .collect();
        let derived = design::decide(&p.profile, &scopes, false);
        if !derived.required
            && !p
                .design
                .triggers
                .iter()
                .any(|t| *t != design::DesignTrigger::Mechanical)
        {
            p.design = derived;
        }
        p.design.user_override = p.profile.override_record.clone();
    }
    Ok(p)
}
/// Called before contract admission. Never auto-load executable project configuration.
/// A project definition must be explicitly approved via the host before being selected.
pub fn prepare(
    contract: &mut TaskContract,
    roots: &BTreeMap<String, PathBuf>,
    _data_dir: &Path,
    project_id: &str,
    _project_dir: Option<&Path>,
    _known_models: &BTreeSet<String>,
) -> Result<PreparedWorkflow, String> {
    let mut writes: Vec<PathRef> = contract.artifacts.iter().map(|a| a.path.clone()).collect();
    for p in contract.checks.iter().flat_map(|c| c.scope.outputs.iter()) {
        if !writes.contains(p) {
            writes.push(p.clone());
        }
    }
    let input = RouteInput {
        planned_writes: writes.clone(),
        ..Default::default()
    };
    let decision = route(&input);
    let mut unit = WorkUnit {
        design_input: Default::default(),
        id: "implementation".into(),
        requirement_ids: contract.requirements.iter().map(|r| r.id.clone()).collect(),
        read_scopes: vec![],
        write_scopes: writes.clone(),
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: None,
    };
    if !writes.is_empty() || contract.delivery_goal != DeliveryGoal::Development {
        for mut check in
            super::verification_policy::suggest_checks_for_goal(&unit, contract.delivery_goal)
        {
            if contract
                .check(&check.id)
                .is_some_and(|existing| existing != &check)
            {
                check.id = format!("host_{}", check.id);
            }
            if contract
                .check(&check.id)
                .is_some_and(|existing| existing != &check)
            {
                return Err("检查 id 与宿主政策冲突".into());
            }
            for req in &mut contract.requirements {
                if !req.check_ids.contains(&check.id) {
                    req.check_ids.push(check.id.clone());
                }
            }
            unit.checks.push(check.id.clone());
            if contract.check(&check.id).is_none() {
                contract.checks.push(check);
            }
        }
    }
    if !writes.is_empty() && !matches!(decision.profile.kind, TaskKind::Mechanical) {
        contract.review_policy.required = true;
        contract.review_policy.independent = true;
        let check = CheckSpec {
            platforms: vec![],
            prerequisites: vec![],
            id: "host_independent_review".into(),
            cwd: None,
            independent_verifier: false,
            kind: CheckKind::Review,
            command: None,
            scope: VerificationScope {
                inputs: writes.clone(),
                ..Default::default()
            },
            required: true,
            expectation: CheckExpectation::Review { dimensions: None },
        };
        if let Some(existing) = contract.check(&check.id) {
            if existing != &check {
                return Err("独立审查宿主检查被覆盖".into());
            }
        } else {
            contract.checks.push(check.clone());
        }
        for r in &mut contract.requirements {
            if !r.check_ids.contains(&check.id) {
                r.check_ids.push(check.id.clone());
            }
        }
        unit.checks.push(check.id);
    }
    let builtin = WorkflowDefinition {
        schema: WORKFLOW_SCHEMA,
        id: "builtin".into(),
        revision: 1,
        stages: vec![Stage {
            id: "implementation".into(),
            title: "实施与验收".into(),
            next: vec![],
            required_checks: unit.checks.clone(),

            ..Default::default()
        }],
        edges: vec![],
        roles: BTreeMap::new(),
        checks: vec![],
        author: None,

        ..Default::default()
    };
    let mut definition = builtin;
    let mut resource_hashes = BTreeMap::new();
    if let Some(project) = _project_dir {
        super::project::recover(_data_dir, project)?;
        let path = super::project::managed_path(project)?;
        if path.exists() {
            if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 {
                return Err("Project workflow exceeds 256 KiB".into());
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let digest = version::digest_of_bytes(&bytes);
            let approved =
                std::fs::read_to_string(approval_pin(_data_dir, project)).unwrap_or_default();
            if approved != digest {
                return Err(
                    "项目工作流配置未经用户确认或已被外部修改，请先通过 workflow save_config 审核"
                        .into(),
                );
            }
            definition = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            validate(&definition, project, _known_models)
                .map_err(|v| format!("项目工作流定义无效: {v:?}"))?;
            resource_hashes.insert("project_workflow".into(), digest);
        }
    }
    if definition.roles.values().any(|p| p.independent_review)
        || definition.stages.iter().any(|n| {
            n.kind == NodeKind::Review
                || n.required_checks
                    .iter()
                    .any(|id| id == "host_independent_review")
        })
    {
        contract.review_policy.required = true;
        contract.review_policy.independent = true;
        let review = CheckSpec {
            platforms: vec![],
            prerequisites: vec![],
            id: "host_independent_review".into(),
            cwd: None,
            independent_verifier: false,
            kind: CheckKind::Review,
            command: None,
            scope: VerificationScope {
                inputs: writes.clone(),
                ..Default::default()
            },
            required: true,
            expectation: CheckExpectation::Review { dimensions: None },
        };
        if contract.check(&review.id).is_none() {
            contract.checks.push(review.clone());
        } else if contract.check(&review.id) != Some(&review) {
            return Err("Reserved independent review check differs".into());
        }
        for req in &mut contract.requirements {
            if !req.check_ids.contains(&review.id) {
                req.check_ids.push(review.id.clone());
            }
        }
        if !unit.checks.contains(&review.id) {
            unit.checks.push(review.id);
        }
    }
    for rule in definition.repair_rules.clone() {
        if !definition
            .edges
            .iter()
            .any(|e| e.from == rule.from && e.to == rule.to && e.kind == EdgeKind::Repair)
        {
            definition.edges.push(Edge {
                from: rule.from,
                to: rule.to,
                kind: EdgeKind::Repair,
            });
        }
    }
    // Approved project templates contribute real contract checks; approval never runs them.
    for template in &definition.checks {
        let id = format!("project_{}", template.id);
        let text = template.command_text.clone();
        let check = CheckSpec {
            platforms: template.platforms.clone(),
            prerequisites: template.prerequisites.clone(),
            id: id.clone(),
            cwd: Some(PathRef {
                root_id: "workspace".into(),
                relative_path: template.cwd.clone().unwrap_or_else(|| ".".into()),
            }),
            independent_verifier: true,
            kind: CheckKind::Command,
            command: Some(CommandSpec::Requested {
                command_text: text,
                shell_hint: None,
            }),
            scope: VerificationScope {
                inputs: writes.clone(),
                ..Default::default()
            },
            required: true,
            expectation: CheckExpectation::Command {
                allowed_exit_codes: vec![0],
            },
        };
        if let Some(old) = contract.check(&id) {
            if old != &check {
                return Err("项目检查 id 与既有契约冲突".into());
            }
        } else {
            contract.checks.push(check);
        }
        for req in &mut contract.requirements {
            if !req.check_ids.contains(&id) {
                req.check_ids.push(id.clone());
            }
        }
        unit.checks.push(id);
    }
    super::definition::validate_bound_definition(&definition, contract)
        .map_err(|issues| format!("{issues:?}"))?;
    let bindings = BTreeMap::from([(
        "implementation".into(),
        snapshot::UnitBinding {
            role_id: None,
            model_id: None,
        },
    )]);
    let mut snap = snapshot::freeze(
        contract.owner.clone(),
        project_id,
        &definition,
        bindings,
        BTreeMap::from([("implementation".into(), writes.clone())]),
        chrono::Utc::now().to_rfc3339(),
    );
    snap.contract_revision = contract.revision;
    snap.resource_hashes = resource_hashes;
    snap.root_locations = roots
        .iter()
        .map(|(id, path)| {
            (
                id.clone(),
                crate::tools::pathutil::canonical_best_effort(path)
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    let scope = VerificationScope {
        inputs: writes.clone(),
        ..Default::default()
    };
    snap.scope_version = Some(
        version::capture_scope(roots, &scope, &Default::default())
            .map_err(|e| format!("范围采集失败: {e:?}"))?,
    );
    let mut stage_ids: Vec<_> = definition
        .stages
        .iter()
        .filter(|s| s.id != "implementation")
        .map(|s| format!("stage:{}", s.id))
        .collect();
    stage_ids.push("implementation".into());
    let mut stage_deps = BTreeMap::<String, Vec<String>>::new();
    for edge in definition
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Advance)
    {
        stage_deps
            .entry(format!("stage:{}", edge.to))
            .or_default()
            .push(format!("stage:{}", edge.from));
    }
    let mut state = WorkflowRunSnapshot::with_deps(
        contract.owner.clone(),
        definition.id.clone(),
        definition.revision,
        &stage_ids,
        &stage_deps,
    );
    // Contract admission happens inside an already-started root run.
    transition(
        &mut state,
        &definition,
        "implementation",
        NodeState::Running,
        0,
        0,
        2,
        &contract.owner,
    )
    .map_err(|e| e.message)?;
    Ok(PreparedWorkflow {
        resumed_from: None,
        resume_budget: None,
        unit_designs: BTreeMap::new(),
        task_id: contract.task_id.clone(),
        profile: decision.profile.clone(),
        units: vec![unit],
        design: design::decide(&decision.profile, &writes, false),
        snapshot: snap,
        state,
    })
}

/// User decisions are append-only and cannot grant dispatch or file permissions.
pub fn record_override(
    data_dir: &Path,
    owner: &RunIdentity,
    task: &str,
    expected_revision: u64,
    kind: TaskKind,
    reason: &str,
) -> Result<serde_json::Value, String> {
    let prepared = read(data_dir, owner, task)?;
    if prepared.snapshot.contract_revision != expected_revision || reason.trim().is_empty() {
        return Err("契约修订不符或缺少覆盖理由".into());
    }
    let record = ProfileOverride {
        kind,
        actor_id: "user".into(),
        contract_revision: expected_revision,
        reason: reason.into(),
        at: chrono::Utc::now().to_rfc3339(),
    };
    let path = file(data_dir, &owner.session_id, task)?.with_file_name(format!(
        "{}-{}.override.json",
        task,
        uuid::Uuid::new_v4()
    ));
    crate::core::process_coordination::atomic_write(
        &path,
        &serde_json::to_vec(&record).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"override":record,"dispatch_authorized":false}))
}

pub fn approval_pin(data_dir: &Path, project: &Path) -> PathBuf {
    let canonical = crate::tools::pathutil::canonical_best_effort(project);
    data_dir.join("workflow_trust").join(format!(
        "{}.sha256",
        version::digest_of_bytes(canonical.to_string_lossy().as_bytes())
    ))
}

/// Only the host completion gate can settle a workflow using an evaluated ledger snapshot.
pub fn settle(
    data_dir: &Path,
    owner: &RunIdentity,
    snapshot: &AcceptanceSnapshot,
) -> Result<(), String> {
    let mut prepared = read(data_dir, owner, &snapshot.task_id)?;
    if snapshot.overall == AcceptanceStatusKind::Passed {
        advance_automatic_nodes(data_dir, owner, &snapshot.task_id, true)?;
        prepared = read(data_dir, owner, &snapshot.task_id)?;
    }
    if prepared.snapshot.contract_revision != snapshot.contract_revision {
        return Err("完成状态基于旧契约修订".into());
    }
    let target = match snapshot.overall {
        AcceptanceStatusKind::Passed => NodeState::Passed,
        AcceptanceStatusKind::Failed => NodeState::Failed,
        AcceptanceStatusKind::Cancelled => NodeState::Cancelled,
        AcceptanceStatusKind::Blocked => NodeState::Blocked,
        _ => return Ok(()),
    };
    if target == NodeState::Passed
        && prepared
            .units
            .iter()
            .filter(|u| u.id != "implementation")
            .any(|u| {
                prepared
                    .state
                    .node(&u.id)
                    .is_none_or(|n| n.state != NodeState::Passed)
            })
    {
        return Err("工作单元尚未全部由宿主验收通过，不能标记工作流完成".into());
    }
    if target == NodeState::Passed
        && prepared
            .state
            .nodes
            .iter()
            .any(|(id, n)| id.starts_with("stage:") && n.state != NodeState::Passed)
    {
        return Err("自定义工作流阶段尚未完成，不能跳过阶段图完成任务".into());
    }
    if target == NodeState::Passed {
        design_ready(data_dir, owner, &snapshot.task_id)?;
    }
    unit_transition(
        data_dir,
        owner,
        &snapshot.task_id,
        "implementation",
        target,
        0,
    )
}

pub fn exists(data_dir: &Path, owner: &RunIdentity, task: &str) -> bool {
    let Ok(base) = file(data_dir, &owner.session_id, task) else {
        return false;
    };
    if base.exists() {
        return true;
    }
    std::fs::read_dir(base.parent().unwrap())
        .ok()
        .is_some_and(|entries| {
            entries.flatten().any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{task}.r"))
            })
        })
}

/// Adopt explicit work packages after the host has obtained a real user confirmation.
pub fn plan_units(
    data_dir: &Path,
    owner: &RunIdentity,
    contract: &TaskContract,
    mut units: Vec<WorkUnit>,
    roots: &BTreeMap<String, PathBuf>,
) -> Result<PreparedWorkflow, String> {
    let mut p = read(data_dir, owner, &contract.task_id)?;
    if p.snapshot.contract_revision != contract.revision || units.is_empty() || units.len() > 100 {
        return Err("计划修订无效或单元数量超限".into());
    }
    let ids: BTreeSet<_> = units.iter().map(|u| u.id.clone()).collect();
    if ids.len() != units.len()
        || ids.contains("")
        || ids.contains("implementation")
        || ids.iter().any(|id| {
            !id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
    {
        return Err("单元 id 必须唯一且非空".into());
    }
    let declared: Vec<_> = contract
        .artifacts
        .iter()
        .map(|a| a.path.clone())
        .chain(
            contract
                .checks
                .iter()
                .flat_map(|c| c.scope.outputs.iter().cloned()),
        )
        .collect();
    for u in &units {
        if u.role_id
            .as_ref()
            .is_some_and(|r| !super::config::KNOWN_ROLES.contains(&r.as_str()))
            || u.model_id.as_ref().is_some_and(|m| m.trim().is_empty())
            || u.checks.iter().any(|id| contract.check(id).is_none())
        {
            return Err("工作单元引用未知角色、模型或检查".into());
        }
        if u.mutable_resources.len() > 100
            || u.mutable_resources
                .iter()
                .any(|r| r.trim().is_empty() || r.len() > 1024)
        {
            return Err("共享资源声明为空或超限".into());
        }
        for path in &u.read_scopes {
            if !roots.contains_key(&path.root_id)
                || std::path::Path::new(&path.relative_path).is_absolute()
                || path.relative_path.split(['/', '\\']).any(|c| c == "..")
            {
                return Err("只读范围引用未知根或越界路径".into());
            }
        }
        if u.requirement_ids.is_empty()
            || u.requirement_ids
                .iter()
                .any(|r| contract.requirement(r).is_none())
        {
            return Err("工作单元必须映射契约需求".into());
        }
        if u.depends_on.iter().any(|d| !ids.contains(d) || d == &u.id) {
            return Err("工作单元依赖引用无效".into());
        }
        for path in &u.write_scopes {
            let root = roots.get(&path.root_id).ok_or("工作单元引用未知根")?;
            if std::path::Path::new(&path.relative_path).is_absolute()
                || path.relative_path.split(['/', '\\']).any(|c| c == "..")
            {
                return Err("工作单元路径越界".into());
            }
            let absolute =
                crate::tools::pathutil::canonical_best_effort(&root.join(&path.relative_path));
            if !declared.iter().any(|d| {
                roots.get(&d.root_id).is_some_and(|r| {
                    absolute.starts_with(crate::tools::pathutil::canonical_best_effort(
                        &r.join(&d.relative_path),
                    ))
                })
            }) {
                return Err("工作单元超出契约已声明写范围，需要先修订契约".into());
            }
        }
    }
    let input = RouteInput {
        planned_writes: units
            .iter()
            .flat_map(|u| u.write_scopes.iter().cloned())
            .collect(),
        planned_reads: units
            .iter()
            .flat_map(|u| u.read_scopes.iter().cloned())
            .collect(),
        unit_depends_on: units
            .iter()
            .filter(|u| !u.depends_on.is_empty())
            .map(|u| (u.id.clone(), u.depends_on.clone()))
            .collect(),
        shared_resources: units
            .iter()
            .flat_map(|u| u.mutable_resources.iter().cloned())
            .collect(),
        ..Default::default()
    };
    let mut decision = route(&input);
    let physical_read_conflict=units.iter().enumerate().any(|(i,a)|units.iter().skip(i+1).any(|b| {
        [(a,b),(b,a)].iter().any(|(writer,reader)|writer.write_scopes.iter().any(|w|reader.read_scopes.iter().any(|r|{
            let wp=roots.get(&w.root_id).map(|root|crate::tools::pathutil::canonical_best_effort(&root.join(&w.relative_path)));
            let rp=roots.get(&r.root_id).map(|root|crate::tools::pathutil::canonical_best_effort(&root.join(&r.relative_path)));
            matches!((wp,rp),(Some(wp),Some(rp)) if wp.starts_with(&rp)||rp.starts_with(&wp))
        })))
    }));
    if physical_read_conflict {
        decision.allows_parallel = false;
        decision.profile.kind = TaskKind::SerialSurfaces;
        decision
            .profile
            .reasons
            .push("实际读写范围形成依赖（含symlink/root别名），串行派发".into());
    }
    if !decision.allows_parallel() {
        for i in 1..units.len() {
            let previous = units[i - 1].id.clone();
            if !units[i].depends_on.contains(&previous) {
                units[i].depends_on.push(previous);
            }
        }
    }
    let definition = p.snapshot.definition.as_ref().ok_or("缺少工作流定义")?;
    for u in &mut units {
        if u.stage_id.is_none() {
            u.stage_id = definition.stages.first().map(|s| s.id.clone());
        }
        if u.stage_id
            .as_ref()
            .is_none_or(|id| !definition.stages.iter().any(|s| &s.id == id))
        {
            return Err("工作单元引用未知阶段".into());
        }
    }
    for u in &mut units {
        let node = definition
            .stages
            .iter()
            .find(|n| Some(&n.id) == u.stage_id.as_ref())
            .ok_or("Unknown node")?;
        if matches!(node.kind, NodeKind::Design | NodeKind::Complete) {
            return Err(
                "Automatic design/complete nodes cannot own dispatched work packages".into(),
            );
        }
        if let Some(role) = &node.role_id {
            if u.role_id.as_ref().is_some_and(|r| r != role) {
                return Err("Node role differs from work unit".into());
            }
            u.role_id = Some(role.clone());
        }
        if let Some(policy) = definition
            .model_policies
            .iter()
            .find(|p| Some(&p.node_id) == u.stage_id.as_ref())
        {
            if u.model_id
                .as_ref()
                .is_some_and(|id| id != &policy.model_entry_id)
            {
                return Err("Node model differs from work unit".into());
            }
            u.model_id = Some(policy.model_entry_id.clone());
        }
        if !node.requirement_ids.is_empty() {
            let required: Vec<_> = if node.requirement_ids.iter().any(|r| r == "*") {
                contract.requirements.iter().map(|r| r.id.clone()).collect()
            } else {
                node.requirement_ids.clone()
            };
            if u.requirement_ids.iter().any(|r| !required.contains(r)) {
                return Err("Work unit exceeds node requirements".into());
            }
        }
    }
    if definition.id != "builtin" {
        for stage in definition
            .stages
            .iter()
            .filter(|n| n.kind != NodeKind::Complete && n.kind != NodeKind::Design)
        {
            let stage_units: Vec<_> = units
                .iter()
                .filter(|u| u.stage_id.as_ref() == Some(&stage.id))
                .collect();
            if stage_units.is_empty() {
                return Err(format!("阶段 {} 缺少可执行工作单元", stage.id));
            }
            for required in &stage.required_checks {
                let check = if definition.checks.iter().any(|c| &c.id == required) {
                    format!("project_{required}")
                } else {
                    required.clone()
                };
                if !stage_units.iter().any(|u| u.checks.contains(&check)) {
                    return Err(format!("阶段 {} 必检项 {} 未绑定工作单元", stage.id, check));
                }
            }
        }
    }
    let mut done = BTreeSet::new();
    while done.len() < units.len() {
        let before = done.len();
        for u in &units {
            if u.depends_on.iter().all(|d| done.contains(d)) {
                done.insert(u.id.clone());
            }
        }
        if done.len() == before {
            return Err("工作单元依赖成环".into());
        }
    }
    let mut deps: BTreeMap<_, _> = units
        .iter()
        .map(|u| (u.id.clone(), u.depends_on.clone()))
        .collect();
    for edge in definition
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Advance)
    {
        deps.entry(format!("stage:{}", edge.to))
            .or_default()
            .push(format!("stage:{}", edge.from));
    }
    deps.insert("implementation".into(), vec![]);
    let mut names: Vec<_> = units.iter().map(|u| u.id.clone()).collect();
    names.push("implementation".into());
    names.extend(
        definition
            .stages
            .iter()
            .filter(|s| s.id != "implementation")
            .map(|s| format!("stage:{}", s.id)),
    );
    let mut state = WorkflowRunSnapshot::with_deps(
        owner.clone(),
        p.state.definition_id.clone(),
        p.state.definition_revision,
        &names,
        &deps,
    );
    let def = p.snapshot.definition.as_ref().ok_or("缺少冻结定义")?;
    transition(
        &mut state,
        def,
        "implementation",
        NodeState::Running,
        0,
        0,
        2,
        owner,
    )
    .map_err(|e| e.message)?;
    for u in &units {
        let profile = route(&RouteInput {
            planned_writes: u.write_scopes.clone(),
            ..Default::default()
        })
        .profile;
        let mut decision = design::decide(&profile, &u.write_scopes, u.design_input.user_requested);
        if u.design_input.documentation_only {
            if u.write_scopes.iter().any(|p| {
                !["md", "txt", "rst"]
                    .iter()
                    .any(|ext| p.relative_path.ends_with(&format!(".{ext}")))
            }) {
                return Err("Documentation-only intent requires documentation paths".into());
            }
            decision.triggers.retain(|t| {
                !matches!(
                    t,
                    design::DesignTrigger::PublicApi | design::DesignTrigger::Mechanical
                )
            });
            if decision.triggers.is_empty() && !u.design_input.user_requested {
                decision.required = false;
                decision.reasons = vec![
                    "User-confirmed documentation-only package without semantic tradeoffs".into(),
                ];
            }
        }
        for trigger in &u.design_input.new_tradeoffs {
            if !decision.triggers.contains(trigger) {
                decision.triggers.push(*trigger);
            }
        }
        if !u.design_input.new_tradeoffs.is_empty() {
            decision.required = true;
            decision
                .reasons
                .push("Explicit semantic tradeoffs require design".into());
        }
        if let (Some(task), Some(revision)) =
            (&u.design_input.reuse_task, u.design_input.reuse_revision)
        {
            let prior_contract =
                crate::core::delivery::DeliveryService::from_data_root(data_dir.to_path_buf())
                    .read_contract(&owner.session_id, task)
                    .ok_or("Referenced design contract missing")?;
            let prior = read(data_dir, &prior_contract.owner, task)?;
            if prior.snapshot.contract_revision != revision
                || prior.snapshot.project_id != p.snapshot.project_id
                || prior
                    .design
                    .selected
                    .as_ref()
                    .is_none_or(|s| !prior.design.alternatives.contains(s))
            {
                return Err("Referenced design was not approved for this project/revision".into());
            }
            if u.design_input.new_tradeoffs.is_empty()
                && !u.design_input.user_requested
                && decision.triggers.is_empty()
            {
                decision.required = false;
                decision
                    .reasons
                    .push(format!("Reuses approved design {task}@{revision}"));
            }
        }
        p.unit_designs.insert(u.id.clone(), decision);
    }
    p.profile = decision.profile;
    p.units = units;
    p.state = state;
    p.snapshot.unit_bindings = p
        .units
        .iter()
        .map(|u| {
            (
                u.id.clone(),
                snapshot::UnitBinding {
                    role_id: u.role_id.clone(),
                    model_id: u.model_id.clone().or_else(|| {
                        u.role_id
                            .as_ref()
                            .and_then(|r| definition.roles.get(r))
                            .and_then(|r| r.model_id.clone())
                    }),
                },
            )
        })
        .collect();
    p.snapshot.roots = p
        .units
        .iter()
        .map(|u| (u.id.clone(), u.write_scopes.clone()))
        .collect();
    let path = file(data_dir, &owner.session_id, &contract.task_id)?
        .with_extension(format!("plan-r{}.json", contract.revision));
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("计划已冻结或无法保存: {e}"))?;
    f.write_all(&serde_json::to_vec(&p).map_err(|e| e.to_string())?)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    advance_automatic_nodes(data_dir, owner, &contract.task_id, false)?;
    Ok(p)
}

/// CAS state mutation under one cross-process OS transaction lock.
pub fn unit_transition(
    data_dir: &Path,
    owner: &RunIdentity,
    task: &str,
    unit: &str,
    to: NodeState,
    generation: u64,
) -> Result<(), String> {
    // An ownership lease is deliberately idempotent for one root run, so it
    // cannot serialize two actors in that run. A short OS file lock is acquired
    // through a fresh file description on every mutation and spans read/modify/
    // atomic write; process death releases it without unlinking its inode.
    let lock_key =
        version::digest_of_bytes(format!("workflow_state:{}:{task}", owner.session_id).as_bytes());
    let _transaction = crate::core::process_coordination::DiskLock::acquire(
        &data_dir
            .join("coord/workflow-state")
            .join(format!("{lock_key}.lock")),
    )
    .map_err(|error| error.to_string())?;
    let mut p = read(data_dir, owner, task)?;
    let mut effective_def = p.snapshot.definition.clone().ok_or("缺少冻结定义")?;
    let phase = p
        .units
        .iter()
        .find(|u| u.id == unit)
        .and_then(|u| u.stage_id.as_ref())
        .filter(|id| *id != "implementation")
        .map(|id| format!("stage:{id}"));
    if let Some(phase) = &phase {
        let stage = phase.trim_start_matches("stage:");
        if effective_def
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Repair && e.to == stage)
        {
            effective_def.edges.push(Edge {
                from: phase.clone(),
                to: unit.into(),
                kind: EdgeKind::Repair,
            });
            effective_def.edges.push(Edge {
                from: phase.clone(),
                to: phase.clone(),
                kind: EdgeKind::Repair,
            });
        }
    }
    let def = &effective_def;
    let existing = p.state.node(unit).ok_or("Unknown work package")?.clone();
    let retry = to == NodeState::Running
        && matches!(
            existing.state,
            NodeState::Failed | NodeState::Blocked | NodeState::Interrupted | NodeState::Passed
        );
    if retry {
        let target = p
            .units
            .iter()
            .find(|u| u.id == unit)
            .and_then(|u| u.stage_id.as_deref())
            .unwrap_or(unit);
        let rule = def
            .repair_rules
            .iter()
            .find(|r| r.to == target)
            .ok_or("No explicit repair rule")?;
        let source = format!("stage:{}", rule.from);
        if existing.state == NodeState::Passed
            && p.state
                .node(&source)
                .is_none_or(|n| n.state != NodeState::Failed)
        {
            return Err(
                "A passed attempt is immutable until its declared review/verify failure".into(),
            );
        }
        let mut descendants = std::collections::BTreeSet::from([target.to_string()]);
        loop {
            let before = descendants.len();
            for edge in def.edges.iter().filter(|e| e.kind == EdgeKind::Advance) {
                if descendants.contains(&edge.from) {
                    descendants.insert(edge.to.clone());
                }
            }
            if descendants.len() == before {
                break;
            }
        }
        let affected: Vec<_> = p
            .units
            .iter()
            .filter(|u| {
                u.stage_id
                    .as_ref()
                    .is_some_and(|stage| descendants.contains(stage))
            })
            .map(|u| u.id.clone())
            .collect();
        let affected_actors: std::collections::BTreeSet<_> = affected
            .iter()
            .filter_map(|id| p.state.node(id).and_then(|n| n.actor_id.as_ref()))
            .collect();
        if super::dispatch::active_leases().iter().any(|lease| {
            lease.root_run_id == owner.root_run_id
                && lease.unit_id != owner.actor_id
                && (affected_actors.contains(&lease.unit_id) || affected.contains(&lease.unit_id))
        }) {
            return Err("Repair waits for affected actor cleanup".into());
        }
        let check_ids = def
            .stages
            .iter()
            .find(|node| node.id == rule.from)
            .map(|node| node.required_checks.join(","))
            .unwrap_or_default();
        let locations = p
            .units
            .iter()
            .filter(|u| affected.contains(&u.id))
            .flat_map(|u| u.write_scopes.iter())
            .map(|path| format!("{}/{}", path.root_id, path.relative_path))
            .collect::<Vec<_>>()
            .join(" ");
        let gap = crate::core::delivery::completion::Gap {
            requirement_id: unit.into(),
            check_id: (!check_ids.is_empty()).then_some(check_ids.clone()),
            reason_code: "node_failed".into(),
            message: existing.reason.clone().unwrap_or_default(),
            fingerprint: crate::core::delivery::completion::fingerprint(
                Some(&check_ids),
                "node_failed",
                &locations,
                target,
            ),
            needs_user_or_env: false,
        };
        crate::core::delivery::DeliveryService::from_data_root(data_dir.to_path_buf())
            .reserve_repair(
                owner,
                task,
                &format!("workflow_{}_{}_{}", rule.id, unit, existing.attempt + 1),
                &[gap],
                Some((&rule.id, rule.max_rounds, rule.fingerprint_budget)),
            )
            .map_err(|e| e.to_string())?;
        for (id, node) in &mut p.state.nodes {
            if (id == unit
                || affected.contains(id)
                || id
                    .strip_prefix("stage:")
                    .is_some_and(|stage| descendants.contains(stage)))
                && node.state != NodeState::Pending
            {
                node.state = NodeState::Pending;
                node.attempt += 1;
                node.actor_id = None;
                node.lease_generation = 0;
                node.revision += 1;
                if id == unit {
                    node.repair_rounds += 1;
                }
            }
        }
        p.state.revision += 1;
    }
    if to == NodeState::Running
        && let Some(phase) = &phase
    {
        let state = p.state.node(phase).ok_or("Stage node missing")?.clone();
        if state.state == NodeState::Pending {
            transition(
                &mut p.state,
                def,
                phase,
                NodeState::Running,
                state.attempt,
                0,
                0,
                &p.snapshot.owner,
            )
            .map_err(|e| e.message)?;
        } else if state.state != NodeState::Running {
            return Err("Stage is terminal".into());
        }
    }
    let attempt = p.state.node(unit).ok_or("Unknown work package")?.attempt;
    transition(
        &mut p.state,
        def,
        unit,
        to,
        attempt,
        generation,
        def.repair_rules
            .iter()
            .filter(|r| r.to == unit || phase.as_deref() == Some(&format!("stage:{}", r.to)))
            .map(|r| r.max_rounds)
            .max()
            .unwrap_or(0),
        owner,
    )
    .map_err(|e| e.message)?;
    if to == NodeState::Passed
        && let Some(phase) = &phase
    {
        let stage = phase.trim_start_matches("stage:");
        if p.units
            .iter()
            .filter(|u| u.stage_id.as_deref() == Some(stage))
            .all(|u| {
                p.state
                    .node(&u.id)
                    .is_some_and(|n| n.state == NodeState::Passed)
            })
        {
            let phase_attempt = p.state.node(phase).unwrap().attempt;
            transition(
                &mut p.state,
                def,
                phase,
                NodeState::Passed,
                phase_attempt,
                0,
                2,
                &p.snapshot.owner,
            )
            .map_err(|e| e.message)?;
        }
    }
    if matches!(
        to,
        NodeState::Failed | NodeState::Blocked | NodeState::Cancelled
    ) && let Some(phase) = &phase
    {
        let phase_node = p.state.node(phase).ok_or("阶段不存在")?.clone();
        if phase_node.state == NodeState::Running {
            transition(
                &mut p.state,
                def,
                phase,
                to,
                phase_node.attempt,
                0,
                2,
                &p.snapshot.owner,
            )
            .map_err(|e| e.message)?;
        }
    }
    super::lifecycle::save_state(
        data_dir,
        task,
        p.snapshot.contract_revision,
        &p.state,
        "host_transition",
    )
}

pub fn record_design(
    data_dir: &Path,
    owner: &RunIdentity,
    task: &str,
    revision: u64,
    mut decision: design::DesignDecision,
) -> Result<(), String> {
    let p = read(data_dir, owner, task)?;
    if p.snapshot.contract_revision != revision {
        return Err("设计所依据的契约修订已变".into());
    }
    if p.design.required
        && (!decision.required
            || decision
                .selected
                .as_ref()
                .is_none_or(|s| s.trim().is_empty() || !decision.alternatives.contains(s))
            || decision.reasons.is_empty())
    {
        return Err("本任务需要设计：请列出备选、选择其中一个并说明依据，不能自称跳过".into());
    }
    decision.triggers = p.design.triggers;
    let path =
        file(data_dir, &owner.session_id, task)?.with_extension(format!("design-r{revision}.json"));
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(&serde_json::to_vec(&decision).map_err(|e| e.to_string())?)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    advance_automatic_nodes(data_dir, owner, task, false)
}
pub fn design_ready(data_dir: &Path, owner: &RunIdentity, task: &str) -> Result<(), String> {
    let p = read(data_dir, owner, task)?;
    if !p.unit_designs.is_empty() {
        if p.unit_designs.values().any(|d| {
            d.required
                && d.selected
                    .as_ref()
                    .is_none_or(|s| !d.alternatives.contains(s))
        }) {
            return Err("Work packages still need approved design decisions".into());
        }
        return Ok(());
    }
    if p.design.required
        && p.design
            .selected
            .as_ref()
            .is_none_or(|s| !p.design.alternatives.contains(s))
    {
        return Err(
            "需要先通过 workflow record_design 保存备选、选择与依据；设计确认不替代既有计划批准"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

/// Freeze one work package's design without forcing unrelated packages to design.
pub fn record_unit_design(
    data: &Path,
    owner: &RunIdentity,
    task: &str,
    revision: u64,
    unit: &str,
    mut decision: design::DesignDecision,
) -> Result<(), String> {
    let p = read(data, owner, task)?;
    if p.snapshot.contract_revision != revision {
        return Err("Contract revision changed".into());
    }
    let required = p.unit_designs.get(unit).ok_or("Unknown work package")?;
    if required.required
        && (!decision.required
            || decision.reasons.is_empty()
            || decision
                .selected
                .as_ref()
                .is_none_or(|s| !decision.alternatives.contains(s)))
    {
        return Err("Design alternatives, selection and reasons are required".into());
    }
    decision.triggers = required.triggers.clone();
    let path = file(data, &owner.session_id, task)?
        .with_extension(format!("design-{unit}-r{revision}.json"));
    use std::io::Write;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    out.write_all(&serde_json::to_vec(&decision).map_err(|e| e.to_string())?)
        .and_then(|_| out.sync_all())
        .map_err(|e| e.to_string())?;
    advance_automatic_nodes(data, owner, task, false)
}

pub fn advance_automatic_nodes(
    data: &Path,
    owner: &RunIdentity,
    task: &str,
    accepted: bool,
) -> Result<(), String> {
    let initial = read(data, owner, task)?;
    let def = initial
        .snapshot
        .definition
        .as_ref()
        .ok_or("Frozen definition missing")?;
    for _ in 0..def.stages.len() {
        let mut changed = false;
        for node in &def.stages {
            if !matches!(node.kind, NodeKind::Complete | NodeKind::Design) {
                continue;
            }
            if node.kind == NodeKind::Complete && !accepted {
                continue;
            }
            if node.kind == NodeKind::Design && design_ready(data, owner, task).is_err() {
                continue;
            }
            let p = read(data, owner, task)?;
            let id = format!("stage:{}", node.id);
            let Some(state) = p.state.node(&id) else {
                continue;
            };
            if state.state == NodeState::Pending
                && super::state_machine::deps_satisfied(
                    &p.state,
                    p.state
                        .depends_on
                        .get(&id)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            {
                unit_transition(data, owner, task, &id, NodeState::Running, 0)?;
                unit_transition(data, owner, task, &id, NodeState::Passed, 0)?;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

pub fn unit_design_ready(
    data: &Path,
    owner: &RunIdentity,
    task: &str,
    unit: &str,
) -> Result<(), String> {
    let p = read(data, owner, task)?;
    if let Some(d) = p.unit_designs.get(unit) {
        if d.required
            && d.selected
                .as_ref()
                .is_none_or(|s| !d.alternatives.contains(s))
        {
            return Err("This work package requires an approved design".into());
        }
        return Ok(());
    }
    design_ready(data, owner, task)
}
