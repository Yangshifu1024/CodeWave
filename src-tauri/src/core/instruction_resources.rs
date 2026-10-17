//! Collect once, authorize that version, then render the captured text without file reads.
use super::agent::{AgentCore, SessionRuntime};
use super::trust::{TrustKey, TrustStore};
use crate::resources::{ResourceOrigin, ResourceSource};
use crate::skills::Skill;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub mod review;
#[cfg(test)]
mod tests;

const MAX_TOTAL: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceScope {
    pub workspace: PathBuf,
    pub data_dir: PathBuf,
    pub project_dir: Option<PathBuf>,
    pub project_id: Option<String>,
    /// Temporary permission belongs to this session, including its children.
    pub owner: String,
}

impl ResourceScope {
    pub fn authority_owner(&self) -> String {
        format!(
            "{}:{}",
            self.project_id.as_deref().unwrap_or("temporary"),
            self.workspace.display()
        )
    }
    pub fn runtime(rt: &SessionRuntime) -> Self {
        Self {
            workspace: crate::tools::pathutil::canonical_best_effort(&rt.workspace),
            data_dir: rt.data_dir.clone(),
            project_dir: rt.project_dir.clone(),
            project_id: rt.project_id.clone(),
            owner: if rt.is_task_runtime {
                format!("task:{}", rt.id)
            } else {
                format!(
                    "session:{}",
                    rt.root_session_id.as_deref().unwrap_or(&rt.id)
                )
            },
        }
    }

    pub fn host(core: &AgentCore, session: Option<&str>) -> Result<Self, String> {
        if let Some(id) = session {
            let rt = core.session(id).ok_or("Resource session closed")?;
            if rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("Resource session closed".into());
            }
            return Ok(Self::runtime(&rt));
        }
        Ok(Self {
            workspace: core.data_dir.clone(),
            data_dir: core.data_dir.clone(),
            project_dir: None,
            project_id: None,
            owner: "global-settings".into(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionKind {
    Instruction,
    Context,
    Skill,
    Memory,
}

#[derive(Clone)]
pub struct InstructionResource {
    pub id: String,
    pub key: TrustKey,
    pub origin: ResourceOrigin,
    pub name: String,
    pub kind: InstructionKind,
    /// Exact projection used by the renderer; version hashes full raw bytes.
    pub text: String,
    pub skill: Option<Skill>,
    pub memory: Option<crate::memory::MemoryMeta>,
}

impl InstructionResource {
    fn new(
        scope: &ResourceScope,
        name: String,
        kind: InstructionKind,
        origin: ResourceOrigin,
        text: String,
    ) -> Self {
        let source = origin
            .identity
            .strip_prefix("file:")
            .map(PathBuf::from)
            .unwrap_or_else(|| scope.workspace.join(".codewave-builtin"));
        let key = TrustKey {
            kind: format!("instruction:{kind:?}"),
            purpose: "prompt".into(),
            source,
            owner: scope.authority_owner(),
            name: name.clone(),
        };
        let id = key.digest().expect("absolute resource scope");
        Self {
            id,
            key,
            origin,
            name,
            kind,
            text,
            skill: None,
            memory: None,
        }
    }

    pub fn builtin(&self) -> bool {
        self.origin.source == ResourceSource::Builtin
    }
}

pub struct ResourceSnapshot {
    pub resources: Vec<InstructionResource>,
    pub issues: Vec<String>,
}

fn read_file(path: &Path) -> Result<(PathBuf, Vec<u8>), String> {
    let bytes = crate::resources::read_bytes(path).map_err(|error| error.to_string())?;
    Ok((
        path.canonicalize().map_err(|error| error.to_string())?,
        bytes,
    ))
}

pub fn collect_project(scope: &ResourceScope) -> ResourceSnapshot {
    let mut snapshot = ResourceSnapshot {
        resources: Vec::new(),
        issues: Vec::new(),
    };
    let mut physical = std::collections::HashSet::new();
    let mut add_file =
        |path: PathBuf, label: &str, kind: InstructionKind, limit: usize| match read_file(&path) {
            Ok((canonical, bytes)) => {
                if !physical.insert(canonical.clone()) {
                    return;
                }
                if !canonical.starts_with(crate::tools::pathutil::canonical_best_effort(
                    &scope.workspace,
                )) && !scope.project_dir.as_ref().is_some_and(|dir| {
                    canonical.starts_with(crate::tools::pathutil::canonical_best_effort(dir))
                }) {
                    snapshot.issues.push(format!(
                        "{}: linked instruction source is outside its project roots",
                        path.display()
                    ));
                    return;
                }
                let text: String = String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(limit)
                    .collect();
                snapshot.resources.push(InstructionResource::new(
                    scope,
                    label.into(),
                    kind,
                    ResourceOrigin::file(
                        if scope
                            .project_dir
                            .as_ref()
                            .is_some_and(|dir| path.starts_with(dir))
                        {
                            ResourceSource::ProjectManaged
                        } else {
                            ResourceSource::WorkspaceFile
                        },
                        &canonical,
                        &bytes,
                    ),
                    text,
                ));
            }
            Err(error) if path.exists() || std::fs::symlink_metadata(&path).is_ok() => {
                // Rejected sources still have a physical identity. Case-insensitive
                // filesystems and links can expose the same oversized/non-regular
                // source under multiple instruction names; report it only once.
                let identity = path.canonicalize().unwrap_or_else(|_| path.clone());
                if !physical.insert(identity) {
                    return;
                }
                snapshot.issues.push(format!("{}: {error}", path.display()))
            }
            Err(_) => {}
        };
    for name in ["CODEWAVE.md", "AGENTS.md", "CLAUDE.md", "claude.md"] {
        add_file(
            scope.workspace.join(name),
            name,
            InstructionKind::Instruction,
            24_000,
        );
    }
    add_file(
        scope.workspace.join("CODEGRAPH.md"),
        "code-graph",
        InstructionKind::Context,
        48_000,
    );
    let lessons = scope
        .project_dir
        .as_ref()
        .map(|path| path.join("lessons.md"))
        .filter(|path| path.exists())
        .unwrap_or_else(|| {
            scope
                .workspace
                .join(crate::core::config::MANAGED_DIR_NAME)
                .join("lessons.md")
        });
    add_file(lessons, "lessons", InstructionKind::Context, 48_000);
    snapshot
}

pub fn collect(core: &AgentCore, scope: &ResourceScope) -> ResourceSnapshot {
    let mut snapshot = collect_project(scope);
    let disabled = core.cfg.read().unwrap().disabled_skills.clone();
    // No TTL here: a changed winning source must never reuse an old authorization.
    let mut skills: Vec<_> = core
        .skills
        .fresh(
            &scope.workspace,
            &scope.data_dir,
            &disabled,
            scope.project_dir.as_deref(),
        )
        .into_values()
        .collect();
    skills.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
    for skill in skills {
        let Some(origin) = skill.meta.resource.clone() else {
            continue;
        };
        let mut resource = InstructionResource::new(
            scope,
            skill.meta.name.clone(),
            InstructionKind::Skill,
            origin,
            format!(
                "{}\n\n{}",
                crate::skills::prompt_listing(std::slice::from_ref(&skill.meta)),
                skill.body
            ),
        );
        resource.skill = Some(skill);
        snapshot.resources.push(resource);
    }
    // Read the effective memory once and derive both metadata and raw version
    // from those bytes. A stale scan description is never paired with a new hash.
    let mut size: usize = snapshot
        .resources
        .iter()
        .map(|resource| resource.text.len())
        .sum();
    for meta in crate::memory::scan(&scope.data_dir, scope.project_dir.as_deref()) {
        let path = Path::new(&meta.path);
        match read_file(path) {
            Ok((canonical, bytes)) => {
                if size + bytes.len() > MAX_TOTAL {
                    snapshot
                        .issues
                        .push("Memory resources exceed the 16 MiB snapshot limit".into());
                    break;
                }
                size += bytes.len();
                let text = String::from_utf8_lossy(&bytes);
                let Some(parsed) = crate::skills::parse_skill_md(
                    &text,
                    path.file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("memory"),
                ) else {
                    continue;
                };
                let memory = crate::memory::MemoryMeta {
                    name: parsed.meta.name,
                    description: if parsed.meta.description.is_empty() {
                        parsed
                            .body
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(80)
                            .collect()
                    } else {
                        parsed.meta.description
                    },
                    path: canonical.display().to_string(),
                };
                let source = if scope
                    .project_dir
                    .as_ref()
                    .is_some_and(|dir| path.starts_with(dir))
                {
                    ResourceSource::ProjectManaged
                } else {
                    ResourceSource::GlobalManaged
                };
                let mut resource = InstructionResource::new(
                    scope,
                    memory.name.clone(),
                    InstructionKind::Memory,
                    ResourceOrigin::file(source, &canonical, &bytes),
                    text.into_owned(),
                );
                resource.memory = Some(memory);
                snapshot.resources.push(resource);
            }
            Err(error) => snapshot.issues.push(format!("{}: {error}", path.display())),
        }
    }
    let mut used = 0;
    snapshot.resources.retain(|resource| {
        used += resource.text.len();
        if used > MAX_TOTAL {
            snapshot.issues.push(format!(
                "{}: resource snapshot exceeds 16 MiB",
                resource.name
            ));
            false
        } else {
            true
        }
    });
    snapshot
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceUse {
    pub id: String,
    pub version: String,
    pub name: String,
    pub kind: InstructionKind,
    pub origin: ResourceOrigin,
    #[serde(skip)]
    pub scope: String,
}

#[derive(Default)]
pub struct InstructionTrust {
    pub store: Option<Arc<TrustStore>>,
    local: Mutex<HashMap<(String, String), String>>,
    reviews: Mutex<HashMap<String, review::PendingReview>>,
    gate: tokio::sync::Mutex<()>,
}

impl InstructionTrust {
    pub fn with_store(store: Arc<TrustStore>) -> Self {
        Self {
            store: Some(store),
            ..Self::default()
        }
    }

    fn local_status(
        &self,
        scope: &ResourceScope,
        resource: &InstructionResource,
    ) -> review::ResourceStatus {
        let mut status = review::ResourceStatus {
            trusted: resource.builtin(),
            remembered: false,
            revision: None,
            error: None,
        };
        if status.trusted {
            return status;
        }
        if self
            .local
            .lock()
            .unwrap()
            .get(&(scope.owner.clone(), resource.id.clone()))
            == Some(&resource.origin.version)
        {
            status.trusted = true;
            return status;
        }
        status
    }

    pub async fn status_with_cancel(
        &self,
        scope: &ResourceScope,
        resource: &InstructionResource,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> review::ResourceStatus {
        let mut status = self.local_status(scope, resource);
        if status.trusted {
            return status;
        }
        if let Some(store) = &self.store {
            match store
                .admit(&resource.key, &resource.origin.version, None, cancel)
                .await
            {
                Ok(guard) => {
                    status.trusted = true;
                    status.remembered = true;
                    status.revision = guard.record.as_ref().map(|record| record.revision);
                }
                Err(super::trust::TrustError::Denied) => {}
                Err(error) => status.error = Some(error.to_string()),
            }
        }
        status
    }

    /// A native failure stops further native reads for this snapshot. Local
    /// approvals and built-ins remain usable; the whole wait budget is 5s.
    pub async fn statuses(
        &self,
        scope: &ResourceScope,
        resources: &[InstructionResource],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Vec<review::ResourceStatus> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut native_error: Option<String> = None;
        let mut statuses = Vec::new();
        for resource in resources {
            let mut status = self.local_status(scope, resource);
            if !status.trusted {
                if cancel.is_cancelled() {
                    status.error = Some(super::trust::TrustError::Cancelled.to_string());
                } else if let Some(error) = &native_error {
                    status.error = Some(error.clone());
                } else {
                    status = match tokio::time::timeout_at(
                        deadline,
                        self.status_with_cancel(scope, resource, cancel),
                    )
                    .await
                    {
                        Ok(status) => status,
                        Err(_) => review::ResourceStatus {
                            error: Some(super::trust::TrustError::Busy.to_string()),
                            ..status
                        },
                    };
                    if status.error.is_some() {
                        native_error = status.error.clone();
                    }
                }
            }
            statuses.push(status);
        }
        statuses
    }
}

pub struct AdmittedResources {
    pub resources: Vec<InstructionResource>,
    pub issues: Vec<String>,
}

pub async fn admit(core: &AgentCore, scope: &ResourceScope) -> AdmittedResources {
    admit_with_cancel(core, scope, &tokio_util::sync::CancellationToken::new()).await
}

pub async fn admit_with_cancel(
    core: &AgentCore,
    scope: &ResourceScope,
    cancel: &tokio_util::sync::CancellationToken,
) -> AdmittedResources {
    if cancel.is_cancelled() {
        return AdmittedResources {
            resources: Vec::new(),
            issues: Vec::new(),
        };
    }
    let snapshot = collect(core, scope);
    let mut resources = Vec::new();
    let statuses = core
        .instructions
        .statuses(scope, &snapshot.resources, cancel)
        .await;
    for (resource, status) in snapshot.resources.into_iter().zip(statuses) {
        if status.trusted && !cancel.is_cancelled() {
            resources.push(resource);
        }
    }
    let current = collect(core, scope);
    resources.retain(|resource| {
        current
            .resources
            .iter()
            .any(|entry| entry.id == resource.id && entry.origin == resource.origin)
    });
    AdmittedResources {
        resources,
        issues: snapshot.issues,
    }
}

impl AdmittedResources {
    pub fn usage(&self) -> Vec<ResourceUse> {
        self.resources
            .iter()
            .filter(|resource| !resource.builtin())
            .map(|resource| ResourceUse {
                id: resource.id.clone(),
                version: resource.origin.version.clone(),
                name: resource.name.clone(),
                kind: resource.kind,
                origin: resource.origin.clone(),
                scope: resource.key.owner.clone(),
            })
            .collect()
    }

    pub fn listings(&self) -> (String, String, String) {
        let mut tail = String::new();
        let mut instructions = String::new();
        let mut context = String::new();
        let mut skills = Vec::new();
        let mut memories = Vec::new();
        for resource in &self.resources {
            match resource.kind {
                InstructionKind::Instruction => instructions.push_str(&format!(
                    "<!-- From: {} -->\n{}\n\n",
                    resource.key.source.display(),
                    resource.text
                )),
                InstructionKind::Context => context.push_str(&format!(
                    "<!-- {}: {} -->\n{}\n\n",
                    resource.name,
                    resource.key.source.display(),
                    resource.text
                )),
                InstructionKind::Skill => {
                    if let Some(skill) = &resource.skill {
                        skills.push(skill.meta.clone());
                    }
                }
                InstructionKind::Memory => {
                    if let Some(memory) = &resource.memory {
                        memories.push(memory.clone());
                    }
                }
            }
        }
        if !instructions.is_empty() {
            tail.push_str(&format!("<project-instructions priority=\"lower-than-core\">\n{}\n</project-instructions>\n", instructions.trim_end()));
        }
        if !context.is_empty() {
            tail.push_str(&format!(
                "<project-context>\n{}\n</project-context>\n",
                context.trim_end()
            ));
        }
        (
            crate::skills::prompt_listing(&skills),
            crate::memory::prompt_listing(&memories),
            tail,
        )
    }
}

/// Shared by wire generation and idle estimates. This never grants permission.
pub async fn system(
    core: &AgentCore,
    rt: &SessionRuntime,
    cfg: &super::config::ConfigState,
) -> (String, Vec<ResourceUse>) {
    let cancel = rt.active_cancel.lock().unwrap().clone().unwrap_or_default();
    let admitted = admit_with_cancel(core, &ResourceScope::runtime(rt), &cancel).await;
    let (skills, memories, tail) = admitted.listings();
    let project = rt.project_id.as_ref().map(|id| {
        let name = super::projects::find(&core.data_dir, id).map(|project| project.name).unwrap_or_else(|| id.clone());
        let cwd = rt.project_dir.as_deref().unwrap_or(&rt.workspace);
        format!("\n<project>\n- Project: {name}\n- Project directory: {}\n- Command working directory: {}\n</project>\n", rt.workspace.display(), cwd.display())
    });
    let text = super::prompt::assemble(
        &rt.workspace,
        cfg,
        &rt.extra_roots.lock().unwrap().clone(),
        &crate::tools::command::shell_description(cfg.shell.selection.as_deref()),
        &skills,
        &memories,
        project.as_deref(),
        &tail,
    );
    (text, admitted.usage())
}
