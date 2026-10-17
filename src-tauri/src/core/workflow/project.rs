//! One user-approved project commit service, shared by desktop IPC and the workflow tool.
use super::config::{self, WorkflowDefinition};
use crate::core::{config::ConfigState, delivery::types::RunIdentity};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub text: String,
    pub revision: u64,
    pub digest: String,
    pub approved: bool,
    pub issues: Vec<config::Issue>,
}
fn digest(bytes: &[u8]) -> String {
    crate::core::delivery::version::digest_of_bytes(bytes)
}
pub fn validate(text: &str, project: &Path, cfg: &ConfigState) -> Vec<config::Issue> {
    if text.len() > 256 * 1024 {
        return vec![config::Issue {
            pointer: "/".into(),
            code: "definition_too_large",
            message: "Workflow text exceeds 256 KiB".into(),
        }];
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        let issues = super::definition::json_issues(&value);
        if !issues.is_empty() {
            return issues;
        }
    }
    let def: WorkflowDefinition = match serde_json::from_str(text) {
        Ok(d) => d,
        Err(e) => {
            return vec![config::Issue {
                pointer: "/".into(),
                code: "invalid_definition",
                message: e.to_string(),
            }];
        }
    };
    let models: BTreeSet<_> = cfg
        .providers
        .iter()
        .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
        .collect();
    let mut issues = config::validate_template(&def, project, &models)
        .err()
        .unwrap_or_default();
    for (i, p) in def.model_policies.iter().enumerate() {
        if !cfg.providers.iter().any(|provider| {
            provider.id == p.provider_id && provider.models.iter().any(|m| m.id == p.model_entry_id)
        }) {
            issues.push(config::Issue {
                pointer: format!("/model_policies/{i}"),
                code: "provider_model_mismatch",
                message: "Model entry does not belong to this provider".into(),
            });
        }
    }
    issues
}
pub fn read(data: &Path, project: &Path, cfg: &ConfigState) -> Result<ProjectView, String> {
    recover(data, project)?;
    read_current(data, project, cfg)
}
fn read_current(data: &Path, project: &Path, cfg: &ConfigState) -> Result<ProjectView, String> {
    let path = managed_path(project)?;
    if path.exists() && std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 256 * 1024 {
        return Err("Workflow exceeds 256 KiB; original file retained".into());
    }
    let bytes = if path.exists() {
        std::fs::read(path).map_err(|e| e.to_string())?
    } else {
        vec![]
    };
    let text = String::from_utf8(bytes.clone()).map_err(|e| e.to_string())?;
    let revision = serde_json::from_slice::<WorkflowDefinition>(&bytes)
        .map(|d| d.revision)
        .unwrap_or(0);
    let hash = digest(&bytes);
    let approved = std::fs::read_to_string(super::service::approval_pin(data, project))
        .is_ok_and(|pin| pin == hash);
    let issues = if bytes.is_empty() {
        vec![]
    } else {
        validate(&text, project, cfg)
    };
    Ok(ProjectView {
        text,
        revision,
        digest: hash,
        approved,
        issues,
    })
}
// The explicit CAS tuple is shared verbatim by IPC and tool callers.
#[allow(clippy::too_many_arguments)]
pub fn save(
    data: &Path,
    project: &Path,
    cfg: &ConfigState,
    text: &str,
    revision: u64,
    expected_digest: &str,
    operation: &str,
    owner: &RunIdentity,
) -> Result<ProjectView, String> {
    validate_operation(operation)?;
    managed_path(project)?;
    let _guard = crate::core::process_coordination::DiskLock::acquire(
        &project.join(".codewave/coord/workflow-admission.lock"),
    )
    .map_err(|e| e.to_string())?;
    recover_locked(data, project)?;
    let receipt = receipt_path(data, project, operation);
    let request = digest(format!("{revision}:{expected_digest}:{text}").as_bytes());
    if receipt.exists() {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if v["request"].as_str() != Some(&request) {
            return Err("Operation ID was reused with a different draft".into());
        }
        return read_current(data, project, cfg);
    }
    let original_exists = project.join(".codewave/workflows.json").exists();
    let old = read_current(data, project, cfg)?;
    if old.revision != revision || old.digest != expected_digest {
        return Err("Workflow conflict: retain your draft and reload the current version".into());
    }
    let issues = validate(text, project, cfg);
    if !issues.is_empty() {
        return Err(serde_json::to_string(&issues).unwrap_or_default());
    }
    let draft: WorkflowDefinition = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let path = project.join(".codewave/workflows.json");
    let mut committed = draft.clone();
    committed.revision = revision + 1;
    committed.author = Some(owner.actor_id.clone());
    let committed_bytes = serde_json::to_vec_pretty(&committed).map_err(|e| e.to_string())?;
    // Write-ahead record retains the last approved bytes before replacing the project file.
    let pending = PendingCommit {
        old: old.text.into_bytes(),
        old_exists: original_exists,
        new: committed_bytes.clone(),
        operation: operation.into(),
        request: request.clone(),
        revision: revision + 1,
    };
    crate::core::process_coordination::atomic_write(
        &pending_path(data, project),
        &serde_json::to_vec(&pending).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    match config::commit(
        &path,
        &draft,
        revision,
        project,
        &cfg.providers
            .iter()
            .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
            .collect(),
        owner,
    ) {
        config::SaveOutcome::Committed { .. } => {}
        other => {
            recover_locked(data, project)?;
            return Err(format!("{other:?}"));
        }
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes != committed_bytes {
        return Err("Configuration changed before approval; not activated".into());
    }
    let pin = super::service::approval_pin(data, project);
    if let Err(error) =
        crate::core::process_coordination::atomic_write(&pin, digest(&bytes).as_bytes())
    {
        recover_locked(data, project)?;
        return Err(format!("Approval not saved: {error}"));
    }
    crate::core::process_coordination::atomic_write(
        &receipt,
        &serde_json::to_vec(
            &serde_json::json!({"request":request,"revision":revision+1,"digest":digest(&bytes)}),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("Saved; operation receipt uncertain: {e}. Reload before retrying"))?;
    std::fs::remove_file(pending_path(data, project)).map_err(|e| e.to_string())?;
    read_current(data, project, cfg)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingCommit {
    old: Vec<u8>,
    old_exists: bool,
    new: Vec<u8>,
    operation: String,
    request: String,
    revision: u64,
}
fn validate_operation(operation: &str) -> Result<(), String> {
    if operation.is_empty()
        || operation.len() > 100
        || !operation
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid operation ID".into());
    }
    Ok(())
}
fn receipt_path(data: &Path, project: &Path, operation: &str) -> std::path::PathBuf {
    data.join("workflow_operations").join(format!(
        "{}-{operation}.json",
        digest(project.to_string_lossy().as_bytes())
    ))
}
fn pending_path(data: &Path, project: &Path) -> std::path::PathBuf {
    data.join("workflow_pending").join(format!(
        "{}.json",
        digest(project.to_string_lossy().as_bytes())
    ))
}
/// Recovery never adopts unapproved bytes and never overwrites an unrelated external edit.
pub fn recover(data: &Path, project: &Path) -> Result<(), String> {
    managed_path(project)?;
    let _guard = crate::core::process_coordination::DiskLock::acquire(
        &project.join(".codewave/coord/workflow-admission.lock"),
    )
    .map_err(|e| e.to_string())?;
    recover_locked(data, project)
}
fn recover_locked(data: &Path, project: &Path) -> Result<(), String> {
    let journal = pending_path(data, project);
    if !journal.exists() {
        return Ok(());
    }
    let pending: PendingCommit =
        serde_json::from_slice(&std::fs::read(&journal).map_err(|e| e.to_string())?)
            .map_err(|e| format!("Workflow transaction recovery requires attention: {e}"))?;
    validate_operation(&pending.operation)?;
    let receipt = receipt_path(data, project, &pending.operation);
    let path = managed_path(project)?;
    let current = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(error) => return Err(error.to_string()),
    };
    if current == pending.new {
        let hash = digest(&current);
        if std::fs::read_to_string(super::service::approval_pin(data, project))
            .is_ok_and(|pin| pin == hash)
        {
            crate::core::process_coordination::atomic_write(&receipt,
                &serde_json::to_vec(&serde_json::json!({"request":pending.request,"revision":pending.revision,"digest":hash})).map_err(|e|e.to_string())?
            ).map_err(|e|e.to_string())?;
        } else if pending.old_exists {
            crate::core::process_coordination::atomic_write(&path, &pending.old)
                .map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
    } else if current != pending.old {
        return Err("Workflow changed outside the interrupted transaction; original bytes retained in recovery journal".into());
    }
    std::fs::remove_file(journal).map_err(|e| e.to_string())
}

pub fn managed_path(project: &Path) -> Result<std::path::PathBuf, String> {
    let base = crate::tools::pathutil::canonical_best_effort(project);
    let path = project.join(".codewave/workflows.json");
    if !crate::tools::pathutil::canonical_best_effort(&path).starts_with(base) {
        return Err("Project workflow path escapes its project".into());
    }
    Ok(path)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn interrupted_replace_recovers_old_approved_bytes_or_finishes_approved_commit() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let path = managed_path(project.path()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let pending = PendingCommit {
            old: b"old approved bytes".to_vec(),
            old_exists: true,
            new: b"new approved bytes".to_vec(),
            operation: "recovery-test".into(),
            request: "request-hash".into(),
            revision: 2,
        };
        let journal = pending_path(data.path(), project.path());
        let pin = super::super::service::approval_pin(data.path(), project.path());
        crate::core::process_coordination::atomic_write(&pin, digest(&pending.old).as_bytes())
            .unwrap();
        crate::core::process_coordination::atomic_write(
            &journal,
            &serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        std::fs::write(&path, &pending.new).unwrap();
        recover(data.path(), project.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), pending.old);
        assert!(!journal.exists());
        assert!(!receipt_path(data.path(), project.path(), &pending.operation).exists());
        // Pin is the approval commit point. A crash before the receipt is reconciled idempotently.
        std::fs::write(&path, &pending.new).unwrap();
        crate::core::process_coordination::atomic_write(
            &journal,
            &serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        crate::core::process_coordination::atomic_write(&pin, digest(&pending.new).as_bytes())
            .unwrap();
        recover(data.path(), project.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), pending.new);
        assert!(receipt_path(data.path(), project.path(), &pending.operation).exists());
        assert!(!journal.exists());
    }
    #[test]
    fn interrupted_transaction_preserves_external_edits_and_recovery_copy() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let path = managed_path(project.path()).unwrap();
        let pending = PendingCommit {
            old: b"old".to_vec(),
            old_exists: true,
            new: b"new".to_vec(),
            operation: "recovery-test".into(),
            request: "hash".into(),
            revision: 2,
        };
        let journal = pending_path(data.path(), project.path());
        crate::core::process_coordination::atomic_write(
            &journal,
            &serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"external").unwrap();
        assert!(recover(data.path(), project.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"external");
        assert!(journal.exists());
    }
}

#[cfg(test)]
mod journal_security_tests {
    use super::*;
    #[test]
    fn journal_cannot_redirect_receipt_writes() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let outside = data.path().join("outside.json");
        std::fs::write(&outside, "sentinel").unwrap();
        let journal = pending_path(data.path(), project.path());
        let record = serde_json::json!({"old":[],"old_exists":false,"new":[],"receipt":outside,"operation":"safe","request":"hash","revision":1});
        crate::core::process_coordination::atomic_write(
            &journal,
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(recover(data.path(), project.path()).is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "sentinel");
        assert!(journal.exists());
    }
}
