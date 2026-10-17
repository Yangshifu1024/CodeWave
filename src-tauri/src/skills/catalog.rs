//! A single candidate scan feeds execution and management views.
use super::{Skill, SkillMeta, builtin_skills, parse_skill_md};
use crate::resources::{ResourceOrigin, ResourceSource};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SkillCandidate {
    pub meta: SkillMeta,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillAuditEntry {
    pub name: String,
    pub disabled: bool,
    /// Highest precedence first. Disabling the name never selects a lower candidate.
    pub candidates: Vec<SkillCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillIssue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillAudit {
    pub entries: Vec<SkillAuditEntry>,
    pub issues: Vec<SkillIssue>,
}

pub(super) struct SkillCatalogSnapshot {
    pub effective: HashMap<String, Skill>,
    pub audit: SkillAudit,
}

impl SkillCatalogSnapshot {
    pub fn build(
        home: Option<&Path>,
        workspace: &Path,
        data_dir: &Path,
        disabled: &[String],
        project_dir: Option<&Path>,
    ) -> Self {
        let mut skills = Vec::new();
        let mut issues = Vec::new();
        let mut remaining = 16 * 1024 * 1024;
        for (name, text) in builtin_skills() {
            if let Some(mut skill) = parse_skill_md(text, name) {
                skill.meta.origin = "<builtin>".into();
                skill.meta.resource = Some(ResourceOrigin::builtin("skill", name, text.as_bytes()));
                skills.push(skill);
            }
        }
        let mut roots = Vec::new();
        if let Some(home) = home {
            roots.push((home.join(".claude/skills"), ResourceSource::UserClaude));
            roots.push((home.join(".agents/skills"), ResourceSource::UserAgents));
        }
        roots.extend([
            (
                workspace.join(".claude/skills"),
                ResourceSource::WorkspaceClaude,
            ),
            (
                workspace.join(".agents/skills"),
                ResourceSource::WorkspaceAgents,
            ),
            (data_dir.join("skills"), ResourceSource::GlobalManaged),
        ]);
        if let Some(project) = project_dir {
            roots.push((project.join("skills"), ResourceSource::ProjectManaged));
        }
        for (root, source) in roots {
            scan(&root, source, &mut skills, &mut issues, &mut remaining);
        }
        // Stable tie-breaking replaces the filesystem's unspecified read_dir order.
        skills.sort_by_cached_key(|skill| {
            let resource = skill.meta.resource.as_ref().expect("scanned provenance");
            (
                resource.source.priority(),
                skill.meta.origin.replace('\\', "/"),
                skill.meta.origin.clone(),
            )
        });
        // Deduplicate physical files before grouping names, including aliases with fallback names.
        let mut physical = HashSet::new();
        skills.reverse();
        skills.retain(|skill| {
            physical.insert(skill.meta.resource.as_ref().unwrap().identity.clone())
        });
        skills.reverse();
        let mut names: BTreeMap<String, Vec<Skill>> = BTreeMap::new();
        for skill in skills {
            let candidates = names.entry(skill.meta.name.clone()).or_default();
            if candidates.last().is_some_and(|other| {
                other.meta.resource.as_ref().unwrap().source
                    == skill.meta.resource.as_ref().unwrap().source
            }) {
                issues.push(SkillIssue {
                    path: skill.meta.origin.clone(),
                    message: format!("Duplicate skill name '{}': the last normalized path wins at equal precedence", skill.meta.name),
                });
            }
            candidates.push(skill);
        }
        for name in disabled {
            names.entry(name.clone()).or_default();
        }
        let mut effective = HashMap::new();
        let entries = names
            .into_iter()
            .map(|(name, mut candidates)| {
                candidates.reverse();
                let disabled = disabled.contains(&name);
                if !disabled && let Some(winner) = candidates.first() {
                    effective.insert(name.clone(), winner.clone());
                }
                SkillAuditEntry {
                    name,
                    disabled,
                    candidates: candidates
                        .into_iter()
                        .enumerate()
                        .map(|(index, skill)| SkillCandidate {
                            meta: skill.meta,
                            selected: index == 0,
                        })
                        .collect(),
                }
            })
            .collect();
        Self {
            effective,
            audit: SkillAudit { entries, issues },
        }
    }
}

/// Deletion roots are real children of the declared data/project scope, never root links.
pub(super) fn managed_root(root: &Path) -> Option<PathBuf> {
    if std::fs::symlink_metadata(root)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let parent = root.parent()?.canonicalize().ok()?;
    let canonical = root.canonicalize().ok()?;
    (canonical != parent && canonical.starts_with(&parent)).then_some(canonical)
}

fn scan(
    root: &Path,
    source: ResourceSource,
    skills: &mut Vec<Skill>,
    issues: &mut Vec<SkillIssue>,
    remaining: &mut usize,
) {
    let issue = |path: &Path, message: String| SkillIssue {
        path: path.display().to_string(),
        message,
    };
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(root).is_err() =>
        {
            return;
        }
        Err(error) => {
            issues.push(issue(root, error.to_string()));
            return;
        }
    };
    let canonical_root = managed_root(root);
    if source.managed() && canonical_root.is_none() {
        issues.push(issue(
            root,
            "Linked or escaping managed skills root is read-only".into(),
        ));
    }
    for entry in entries {
        if *remaining == 0 {
            issues.push(issue(root, "Skill catalog content budget exhausted".into()));
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                issues.push(issue(root, error.to_string()));
                continue;
            }
        };
        let path = entry.path();
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                issues.push(issue(&path, error.to_string()));
                continue;
            }
        };
        let file = if metadata.is_dir() {
            let candidate = path.join("SKILL.md");
            match std::fs::metadata(&candidate) {
                Ok(metadata) if metadata.is_file() => candidate,
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound
                        && std::fs::symlink_metadata(&candidate).is_err() =>
                {
                    continue;
                }
                Err(error) => {
                    issues.push(issue(&candidate, error.to_string()));
                    continue;
                }
                Ok(_) => {
                    issues.push(issue(&candidate, "SKILL.md is not a regular file".into()));
                    continue;
                }
            }
        } else if metadata.is_file() && path.extension().is_some_and(|extension| extension == "md")
        {
            path.clone()
        } else {
            continue;
        };
        let result = (|| -> Result<Skill, String> {
            let bytes = crate::resources::read_bytes(&file).map_err(|error| error.to_string())?;
            if bytes.len() > *remaining {
                *remaining = 0;
                return Err("Skill catalog exceeds 16 MiB".into());
            }
            *remaining -= bytes.len();
            let text =
                std::str::from_utf8(&bytes).map_err(|_| "Skill is not valid UTF-8".to_string())?;
            let fallback = if file != path {
                entry.file_name().to_string_lossy().into_owned()
            } else {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let mut skill =
                parse_skill_md(text, &fallback).ok_or("Invalid skill frontmatter or name")?;
            let canonical = file.canonicalize().map_err(|error| error.to_string())?;
            skill.meta.origin = file.display().to_string();
            let linked_entry = std::fs::symlink_metadata(&path)
                .is_ok_and(|meta| meta.file_type().is_symlink())
                || std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.file_type().is_symlink());
            skill.meta.deletable = source.managed()
                && !linked_entry
                && canonical_root
                    .as_ref()
                    .is_some_and(|root| canonical.starts_with(root));
            skill.meta.resource = Some(ResourceOrigin::file(source, &canonical, &bytes));
            Ok(skill)
        })();
        match result {
            Ok(skill) => skills.push(skill),
            Err(error) => issues.push(issue(&file, error)),
        }
    }
}
