//! Configuration transaction boundary shared by IPC saves and approval policy patches.
use super::{
    CONFIG_COMMIT, ConfigIssue, IssueLevel, McpError, McpServerConfig, PoolKey, Scope, ScopeRef,
    global_scope, load_scope, merge_scopes, parse_scope_text, project_scope, read_scope_text,
    scope_version, source_identity,
};
use crate::core::agent::{AgentCore, SessionRuntime};
use serde::Serialize;

pub fn launch_key(
    core: &AgentCore,
    source: &ScopeRef,
    session_id: Option<&str>,
    name: &str,
) -> Result<PoolKey, McpError> {
    let key = match source.scope {
        Scope::Global => PoolKey::global(name),
        Scope::Project => {
            let rt = session_id
                .and_then(|id| core.session(id))
                .ok_or_else(|| McpError::config("Project session closed"))?;
            if rt.zombie.load(std::sync::atomic::Ordering::SeqCst)
                || !rt.project_dir.as_deref().is_some_and(|path| {
                    source_identity(&project_scope(path).path) == source_identity(&source.path)
                })
            {
                return Err(McpError::config("Project source changed"));
            }
            PoolKey::project(rt.project_id.clone().unwrap_or_default(), name)
        }
    };
    Ok(key.with_source(&source.path))
}

/// A host action, separate from enabled/read_only/always_allow and scope-file contents.
pub async fn set_launch_trust(
    core: &AgentCore,
    source: &ScopeRef,
    session_id: Option<&str>,
    name: &str,
    expected: &str,
    trusted: bool,
) -> Result<super::LaunchStatus, McpError> {
    let _commit = CONFIG_COMMIT.lock().await;
    let key = launch_key(core, source, session_id, name)?;
    let doc = load_scope(source);
    let cfg = doc
        .servers
        .iter()
        .find(|(server, _)| server == name)
        .map(|(_, cfg)| cfg)
        .ok_or_else(|| McpError::config("MCP server no longer exists"))?;
    if doc.issues.iter().any(|issue| {
        issue.level == IssueLevel::Error
            && issue.server.as_deref().is_none_or(|server| server == name)
    }) {
        return Err(McpError::config(
            "MCP configuration must be corrected before launch approval",
        ));
    }
    core.mcp
        .set_launch_trust(&key, cfg, expected, trusted)
        .await?;
    drop(_commit);
    if !trusted {
        let authority = core.mcp.revoke_launch_version(&key, cfg).await?;
        let _commit = CONFIG_COMMIT.lock().await;
        if launch_key(core, source, session_id, name)? != key
            || !load_scope(source)
                .servers
                .iter()
                .any(|(server, current)| server == name && current == cfg)
        {
            return Err(McpError::config(
                "MCP configuration changed during revocation; reload",
            ));
        }
        core.mcp
            .set_launch_trust(&key, cfg, expected, false)
            .await?;
        drop(_commit);
        drop(authority);
    }
    if trusted {
        let sessions: Vec<_> = core
            .sessions
            .iter()
            .filter_map(|entry| {
                let rt = entry.value();
                if rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
                    return None;
                }
                let visible = visible_servers(rt);
                visible
                    .iter()
                    .any(|(visible, _)| *visible == key)
                    .then(|| (rt.id.clone(), visible))
            })
            .collect();
        let http = core.client.read().unwrap().clone();
        for (session, visible) in sessions {
            core.mcp
                .connect_named(&session, visible, &[name.into()], http.clone())
                .await;
        }
    }
    Ok(core.mcp.launch_status(&key, cfg))
}

pub fn visible_servers(rt: &SessionRuntime) -> Vec<(PoolKey, McpServerConfig)> {
    let global = global_scope(&rt.data_dir);
    let project = rt.project_dir.as_deref().map(project_scope);
    let g = load_scope(&global);
    let p = project.as_ref().map(load_scope);
    merge_scopes(&g, p.as_ref())
        .into_iter()
        .filter(|server| {
            server.cfg.enabled && !server.issues.iter().any(|i| i.level == IssueLevel::Error)
        })
        .map(|server| {
            let key = match server.source {
                Scope::Global => PoolKey::global(&server.name).with_source(&global.path),
                Scope::Project => {
                    PoolKey::project(rt.project_id.clone().unwrap_or_default(), &server.name)
                        .with_source(
                            &project
                                .as_ref()
                                .expect("project source requires project scope")
                                .path,
                        )
                }
            };
            (key, server.cfg)
        })
        .collect()
}

#[derive(Serialize)]
pub struct SaveReceipt {
    pub saved: bool,
    pub issues: Vec<ConfigIssue>,
    pub version: String,
    pub apply_revision: u64,
    pub affected_servers: Vec<String>,
}

/// Validates without temporary files, checks the editor's content version, then publishes a local delta.
pub async fn save_configuration(
    core: &AgentCore,
    source: &ScopeRef,
    text: &str,
    expected_version: &str,
) -> Result<SaveReceipt, McpError> {
    let _commit = CONFIG_COMMIT.lock().await;
    if source.scope == Scope::Project
        && !core.sessions.iter().any(|entry| {
            let rt = entry.value();
            !rt.zombie.load(std::sync::atomic::Ordering::SeqCst)
                && rt.project_dir.as_deref().is_some_and(|path| {
                    source_identity(&project_scope(path).path) == source_identity(&source.path)
                })
        })
    {
        return Err(McpError::config("Project context closed before MCP save"));
    }

    let parsed = parse_scope_text(source, text);
    let old = read_scope_text(source)?;
    let version = scope_version(&old);
    if version != expected_version {
        return Err(McpError::config(
            "MCP configuration changed in another editor; reload before saving (配置冲突)",
        ));
    }
    if parsed
        .issues
        .iter()
        .any(|issue| issue.level == IssueLevel::Error)
    {
        return Ok(SaveReceipt {
            saved: false,
            issues: parsed.issues,
            version,
            apply_revision: 0,
            affected_servers: vec![],
        });
    }
    let identity_before = source_identity(&source.path);
    let previous = parse_scope_text(source, &old);
    let changed: Vec<_> = previous
        .servers
        .iter()
        .filter(|(name, cfg)| {
            !parsed
                .servers
                .iter()
                .any(|(current, next)| current == name && next == cfg && next.enabled)
        })
        .map(|(name, _)| name.clone())
        .collect();
    let owners: Vec<_> = if source.scope == Scope::Global {
        vec![None]
    } else {
        core.sessions
            .iter()
            .filter_map(|entry| {
                let rt = entry.value();
                rt.project_dir
                    .as_deref()
                    .filter(|path| source_identity(&project_scope(path).path) == identity_before)
                    .map(|_| Some(rt.project_id.clone().unwrap_or_default()))
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    };
    let keys: Vec<_> = owners
        .iter()
        .flat_map(|owner| {
            changed.iter().map(|name| {
                match owner {
                    Some(id) => PoolKey::project(id, name),
                    None => PoolKey::global(name),
                }
                .with_source(&identity_before)
            })
        })
        .collect();
    drop(_commit);
    let authority = core.mcp.invalidate_persistent(&keys).await?;
    let _commit = CONFIG_COMMIT.lock().await;
    if source_identity(&source.path) != identity_before || read_scope_text(source)? != old {
        return Err(McpError::config(
            "MCP configuration changed during trust invalidation; reload before saving",
        ));
    }
    if source.scope == Scope::Project
        && !core.sessions.iter().any(|entry| {
            let rt = entry.value();
            !rt.zombie.load(std::sync::atomic::Ordering::SeqCst)
                && rt.project_dir.as_deref().is_some_and(|path| {
                    source_identity(&project_scope(path).path) == identity_before
                })
        })
    {
        return Err(McpError::config("Project context closed before MCP save"));
    }
    if let Some(parent) = source.path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| McpError::config("MCP configuration directory could not be created"))?;
    }
    crate::util::atomic::atomic_write(&source.path, text.as_bytes()).map_err(|_| {
        McpError::config("MCP configuration could not be saved; existing connections unchanged")
    })?;
    core.mcp
        .invalidate_launch_source(&source_identity(&source.path), &parsed.servers);
    drop(_commit);
    drop(authority);
    // Only runtimes actually reading this file are affected. The canonical path distinguishes relocated projects.
    let identity = source_identity(&source.path);
    let sessions = core
        .sessions
        .iter()
        .filter_map(|entry| {
            let rt = entry.value();
            let reads_source = source.scope == Scope::Global
                || rt
                    .project_dir
                    .as_deref()
                    .is_some_and(|path| source_identity(&project_scope(path).path) == identity);
            reads_source.then(|| (rt.id.clone(), visible_servers(rt)))
        })
        .collect();
    let http = core.client.read().unwrap().clone();
    let affected_servers = core.mcp.reconcile(sessions, http).await;
    Ok(SaveReceipt {
        saved: true,
        issues: parsed.issues,
        version: scope_version(text),
        apply_revision: core.mcp.next_revision(),
        affected_servers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::test_support::make_core;
    use crate::tools::pathutil::WriteRoots;

    #[tokio::test]
    async fn launch_approval_rereads_disk_and_saves_revoke_old_versions() {
        let dir = tempfile::tempdir().unwrap();
        let core = make_core(&WriteRoots {
            project_dir: None,
            workspace: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
            extra: vec![],
        });
        let source = global_scope(dir.path());
        let original = r#"{"mcpServers":{"saved":{"command":"fixture","read_only":true,"always_allow":true}}}"#;
        std::fs::write(&source.path, original).unwrap();
        let cfg = load_scope(&source).servers.remove(0).1;
        let key = launch_key(&core, &source, None, "saved").unwrap();
        let version = super::super::launch_version(&cfg);
        assert!(!core.mcp.launch_status(&key, &cfg).trusted);
        set_launch_trust(&core, &source, None, "saved", &version, true)
            .await
            .unwrap();
        assert!(core.mcp.launch_status(&key, &cfg).trusted);
        assert!(
            core.mcp.all_status().await.is_empty(),
            "approval without a visible session must not start a process"
        );
        let changed = r#"{"mcpServers":{"saved":{"command":"replacement"}}}"#;
        std::fs::write(&source.path, changed).unwrap();
        assert!(
            set_launch_trust(&core, &source, None, "saved", &version, true)
                .await
                .is_err()
        );
        let changed_cfg = load_scope(&source).servers.remove(0).1;
        assert!(!core.mcp.launch_status(&key, &changed_cfg).trusted);
        set_launch_trust(
            &core,
            &source,
            None,
            "saved",
            &super::super::launch_version(&changed_cfg),
            true,
        )
        .await
        .unwrap();
        save_configuration(&core, &source, original, &scope_version(changed))
            .await
            .unwrap();
        assert!(!core.mcp.launch_status(&key, &changed_cfg).trusted);
        assert!(!core.mcp.launch_status(&key, &cfg).trusted);
    }

    #[tokio::test]
    async fn invalid_or_conflicting_save_never_changes_disk_or_creates_validation_files() {
        let dir = tempfile::tempdir().unwrap();
        let core = make_core(&WriteRoots {
            project_dir: None,
            workspace: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
            extra: vec![],
        });
        let source = global_scope(dir.path());
        let original = r#"{"mcpServers":{"saved":{"command":"working"}}}"#;
        std::fs::write(&source.path, original).unwrap();
        let bad = save_configuration(
            &core,
            &source,
            r#"{"mcpServers":{"draft":{"args":"secret-value"}}}"#,
            &scope_version(original),
        )
        .await
        .unwrap();
        assert!(!bad.saved);
        assert!(
            !serde_json::to_string(&bad)
                .unwrap()
                .contains("secret-value")
        );
        assert_eq!(std::fs::read_to_string(&source.path).unwrap(), original);
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().contains("validate"))
                .count(),
            0
        );
        let next = r#"{"mcpServers":{"saved":{"command":"next"}}}"#;
        let receipt = save_configuration(&core, &source, next, &scope_version(original))
            .await
            .unwrap();
        assert!(receipt.saved);
        assert_eq!(receipt.version, scope_version(next));
        assert!(
            save_configuration(&core, &source, original, &scope_version(original))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&source.path).unwrap(), next);
    }

    #[tokio::test]
    async fn disk_write_failure_does_not_mutate_manager() {
        let dir = tempfile::tempdir().unwrap();
        let core = make_core(&WriteRoots {
            project_dir: None,
            workspace: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
            extra: vec![],
        });
        let source = global_scope(dir.path());
        std::fs::create_dir(&source.path).unwrap();
        let before = core.mcp.all_status().await;
        assert!(
            save_configuration(&core, &source, r#"{"mcpServers":{}}"#, "irrelevant")
                .await
                .is_err()
        );
        assert_eq!(core.mcp.all_status().await.len(), before.len());
        assert!(source.path.is_dir());
    }

    #[tokio::test]
    async fn concurrent_editors_have_exactly_one_successful_commit() {
        let dir = tempfile::tempdir().unwrap();
        let core = make_core(&WriteRoots {
            project_dir: None,
            workspace: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
            extra: vec![],
        });
        let source = global_scope(dir.path());
        let version = scope_version(&super::super::default_doc_text());
        let (one, two) = tokio::join!(
            save_configuration(
                &core,
                &source,
                r#"{"mcpServers":{"one":{"command":"a"}}}"#,
                &version
            ),
            save_configuration(
                &core,
                &source,
                r#"{"mcpServers":{"two":{"command":"b"}}}"#,
                &version
            ),
        );
        assert_ne!(one.is_ok(), two.is_ok());
        assert_eq!(load_scope(&source).servers.len(), 1);
    }
}
