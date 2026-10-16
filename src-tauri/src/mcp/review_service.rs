//! Review/commit boundary. No config or pool mutex is held during keyring I/O.
use super::*;
use crate::core::agent::AgentCore;
use crate::core::trust::{Decision, TrustError};
use crate::mcp::trust::{authority_error, authority_key};
use tokio_util::sync::CancellationToken;

fn candidate(
    core: &AgentCore,
    source: &ScopeRef,
    session: Option<&str>,
    name: &str,
) -> Result<(PoolKey, McpServerConfig), McpError> {
    let key = launch_key(core, source, session, name)?;
    let doc = load_scope(source);
    if doc.issues.iter().any(|issue| {
        issue.level == IssueLevel::Error
            && issue.server.as_deref().is_none_or(|server| server == name)
    }) {
        return Err(McpError::config(
            "Correct the MCP configuration before reviewing launch permission",
        ));
    }
    let cfg = doc
        .servers
        .into_iter()
        .find(|(server, _)| server == name)
        .map(|(_, cfg)| cfg)
        .ok_or_else(|| McpError::config("MCP server no longer exists"))?;
    Ok((key, cfg))
}

pub async fn prepare_launch_review(
    core: &AgentCore,
    source: &ScopeRef,
    session: Option<&str>,
    name: &str,
    version: &str,
    action: ReviewAction,
) -> Result<LaunchReview, McpError> {
    let (key, cfg) = {
        let _commit = CONFIG_COMMIT.lock().await;
        candidate(core, source, session, name)?
    };
    core.mcp
        .prepare_launch_review(&key, &cfg, version, action)
        .await
}

async fn connect_approved(core: &AgentCore, key: &PoolKey) {
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
                .any(|(visible, _)| visible == key)
                .then(|| (rt.id.clone(), visible))
        })
        .collect();
    let http = core.client.read().unwrap().clone();
    for (session, visible) in sessions {
        core.mcp
            .connect_named(
                &session,
                visible,
                std::slice::from_ref(&key.name),
                http.clone(),
            )
            .await;
    }
}

pub async fn confirm_launch_review(
    core: &AgentCore,
    source: &ScopeRef,
    session: Option<&str>,
    name: &str,
    id: &str,
) -> Result<LaunchReviewReceipt, McpError> {
    let review = core.mcp.consume_launch_review(id)?;
    let (key, cfg, review) = {
        let _commit = CONFIG_COMMIT.lock().await;
        let (key, cfg) = candidate(core, source, session, name)?;
        if review.key != key || review.cfg != cfg {
            return Err(McpError::config(
                "Launch review source or configuration changed",
            ));
        }
        (key, cfg, review)
    };
    let version = launch_version(&cfg);
    if review.action == ReviewAction::Revoke {
        // Stop this instance even if the subsequent OS transaction is unknown.
        core.mcp
            .set_launch_trust(&key, &cfg, &version, false)
            .await?;
    }
    let authority = if review.action == ReviewAction::Once {
        None
    } else {
        let store = core
            .mcp
            .authority_store()
            .ok_or_else(|| McpError::config("Persistent trust authority is unavailable"))?;
        let decision = if review.action == ReviewAction::Revoke {
            Decision::Revoke
        } else {
            Decision::Grant
        };
        match store
            .compare_exchange_guard(
                &authority_key(&key),
                review.revision,
                &version,
                decision,
                id,
                &CancellationToken::new(),
            )
            .await
        {
            Ok(guard) => Some(guard),
            Err(TrustError::OutcomeUnknown) => {
                return Ok(LaunchReviewReceipt {
                    operation_id: id.into(),
                    confirmed: false,
                    resolved: false,
                    status: None,
                    error: Some(TrustError::OutcomeUnknown.to_string()),
                });
            }
            Err(error) => return Err(authority_error(error)),
        }
    };
    let _commit = CONFIG_COMMIT.lock().await;
    let unchanged = core.mcp.validate_launch_review(id).is_ok()
        && candidate(core, source, session, name)
            .is_ok_and(|(current, config)| current == key && config == cfg);
    let applied = if !unchanged {
        Err(McpError::config(
            "MCP source or configuration changed during confirmation; review again",
        ))
    } else {
        match review.action {
            ReviewAction::Once => core.mcp.approve_launch_review(id, &key, &cfg, None).await,
            ReviewAction::Remember => {
                let revision = authority
                    .as_ref()
                    .and_then(|guard| guard.record.as_ref())
                    .ok_or_else(|| McpError::config("Trust receipt missing"))?
                    .revision;
                core.mcp
                    .approve_launch_review(id, &key, &cfg, Some(revision))
                    .await
            }
            ReviewAction::Revoke => core.mcp.set_launch_trust(&key, &cfg, &version, false).await,
        }
    };
    if let Err(error) = applied {
        let revision = authority
            .as_ref()
            .and_then(|guard| guard.record.as_ref())
            .map(|record| record.revision);
        drop(_commit);
        drop(authority);
        if review.action == ReviewAction::Remember {
            // Withdraw only this failed review's native grant. A newer grant
            // wins its CAS rather than being revoked by stale cleanup.
            let store = core
                .mcp
                .authority_store()
                .ok_or_else(|| McpError::config("Trust authority unavailable"))?;
            match store
                .compare_exchange(
                    &authority_key(&key),
                    revision,
                    &version,
                    Decision::Revoke,
                    &uuid::Uuid::new_v4().to_string(),
                    &CancellationToken::new(),
                )
                .await
            {
                Ok(_) | Err(TrustError::Conflict) => {}
                Err(error) => return Err(authority_error(error)),
            }
        }
        return Err(error);
    }
    core.mcp.finish_launch_review(id);
    let status = core.mcp.launch_status(&key, &cfg);
    drop(_commit);
    drop(authority);
    if review.action != ReviewAction::Revoke {
        connect_approved(core, &key).await;
    }
    Ok(LaunchReviewReceipt {
        operation_id: id.into(),
        confirmed: true,
        resolved: true,
        status: Some(status),
        error: None,
    })
}

/// Inspect the original native receipt and finish its local revocation once.
/// Never writes or retries a native authorization.
pub async fn launch_operation(
    core: &AgentCore,
    source: &ScopeRef,
    session: Option<&str>,
    name: &str,
    id: &str,
) -> Result<LaunchReviewReceipt, McpError> {
    let (key, cfg, review) = {
        let _commit = CONFIG_COMMIT.lock().await;
        let (key, cfg) = candidate(core, source, session, name)?;
        let review = core.mcp.launch_operation(id, &key, &cfg)?;
        (key, cfg, review)
    };
    if !review.applied {
        let store = core
            .mcp
            .authority_store()
            .ok_or_else(|| McpError::config("Persistent trust authority is unavailable"))?;
        let guard = store
            .read_guard(&authority_key(&key), &CancellationToken::new())
            .await
            .map_err(authority_error)?;
        let decision = if review.action == ReviewAction::Revoke {
            Decision::Revoke
        } else {
            Decision::Grant
        };
        if !guard.record.as_ref().is_some_and(|record| {
            record.operation_id == id
                && record.version == launch_version(&cfg)
                && record.decision == decision
                && review.revision.unwrap_or(0).checked_add(1) == Some(record.revision)
        }) {
            return Ok(LaunchReviewReceipt {
                operation_id: id.into(),
                confirmed: false,
                resolved: true,
                status: None,
                error: Some(
                    "Operation is unresolved or superseded; no authorization was retried".into(),
                ),
            });
        }
        let _commit = CONFIG_COMMIT.lock().await;
        let (current_key, current_cfg) = candidate(core, source, session, name)?;
        if current_key != key || current_cfg != cfg {
            return Err(McpError::config(
                "Launch operation source or configuration changed",
            ));
        }
        // Another inspector may have completed while this reader waited for
        // the authority lock. Never apply the same local revoke a second time.
        let current = core.mcp.launch_operation(id, &key, &cfg)?;
        if !current.applied {
            if current.action == ReviewAction::Revoke {
                core.mcp
                    .set_launch_trust(&key, &cfg, &launch_version(&cfg), false)
                    .await?;
            } else {
                core.mcp.validate_launch_review(id)?;
            }
            core.mcp.finish_launch_review(id);
        }
        drop(_commit);
        drop(guard);
    }
    let status = core.mcp.current_launch_status(&key, &cfg).await;
    Ok(LaunchReviewReceipt {
        operation_id: id.into(),
        confirmed: true,
        resolved: true,
        status: Some(status),
        error: None,
    })
}
