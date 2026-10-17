use super::*;
use crate::mcp::Scope;
use serde_json::json;

fn config() -> McpServerConfig {
    serde_json::from_value(json!({"command": "fixture-server", "args": ["--fixture"]})).unwrap()
}

fn key(name: &str) -> PoolKey {
    PoolKey {
        scope: Scope::Project,
        project_id: Some("project-a".into()),
        name: name.into(),
        source: std::path::PathBuf::from("project-a/.codewave/mcp.json"),
    }
}

fn grant(registry: &LaunchRegistry, key: &PoolKey, cfg: &McpServerConfig) -> LaunchPermit {
    registry.grant(key, cfg, &launch_version(cfg)).unwrap();
    registry.connection(key, cfg).unwrap()
}

#[test]
fn grants_bind_source_project_name_scope_and_configuration_until_revoked() {
    let registry = LaunchRegistry::default();
    let key = key("fixture");
    let cfg = config();
    assert!(!registry.status(&key, &cfg).trusted);
    assert!(registry.grant(&key, &cfg, "stale-version").is_err());
    assert!(!registry.status(&key, &cfg).trusted);
    let permit = grant(&registry, &key, &cfg);
    for foreign in [
        PoolKey {
            source: "other/mcp.json".into(),
            ..key.clone()
        },
        PoolKey {
            project_id: Some("project-b".into()),
            ..key.clone()
        },
        PoolKey {
            name: "other".into(),
            ..key.clone()
        },
        PoolKey {
            scope: Scope::Global,
            project_id: None,
            ..key.clone()
        },
    ] {
        assert!(registry.connection(&foreign, &cfg).is_err());
        assert!(permit.check(&foreign, &cfg).is_err());
    }
    let mut changed = cfg.clone();
    changed.args.push("--different".into());
    assert!(registry.connection(&key, &changed).is_err());
    let replacement = grant(&registry, &key, &changed);
    assert!(permit.cancel.is_cancelled());
    assert!(!replacement.cancel.is_cancelled());
    registry
        .grant(&key, &changed, &launch_version(&changed))
        .unwrap();
    assert!(
        !replacement.cancel.is_cancelled(),
        "identical approval must not interrupt live work"
    );
    registry.revoke(&key);
    assert!(replacement.cancel.is_cancelled());
    assert!(registry.connection(&key, &changed).is_err());
    assert!(!LaunchRegistry::default().status(&key, &changed).trusted);
}

#[test]
fn launch_identity_ignores_json_order_and_defaults_but_tracks_effective_changes() {
    let first: McpServerConfig = serde_json::from_str(
        r#"{
        "command":"fixture-server", "env":{"B":"two","A":"one"},
        "custom":{"z":[{"b":2,"a":1}],"a":true}
    }"#,
    )
    .unwrap();
    let reordered: McpServerConfig = serde_json::from_str(
        r#"{
        "custom":{"a":true,"z":[{"a":1,"b":2}]},
        "enabled":true,"read_only":false,"always_allow":false,"args":[],
        "env":{"A":"one","B":"two"},"command":"fixture-server"
    }"#,
    )
    .unwrap();
    let registry = LaunchRegistry::default();
    let key = key("fixture");
    let permit = grant(&registry, &key, &first);
    assert_eq!(launch_version(&first), launch_version(&reordered));
    assert!(registry.connection(&key, &reordered).is_ok());
    let mut changes = Vec::new();
    let mut changed = first.clone();
    changed.command = Some("other-server".into());
    changes.push(changed);
    let mut changed = first.clone();
    changed.env.insert("A".into(), "changed".into());
    changes.push(changed);
    let mut changed = first.clone();
    changed.extra.insert("custom".into(), json!({"a": false}));
    changes.push(changed);
    for changed in changes {
        assert!(registry.connection(&key, &changed).is_err());
        assert!(permit.check(&key, &changed).is_err());
    }
}

#[test]
fn probes_are_single_use_and_do_not_create_or_inherit_connection_grants() {
    let registry = LaunchRegistry::default();
    let key = key("fixture");
    let cfg = config();
    let challenge = registry.prepare_probe(&key, &cfg).unwrap();
    assert_eq!(challenge.version, launch_version(&cfg));
    assert!(!registry.status(&key, &cfg).trusted);
    let lease = registry.confirm_probe(&challenge.id, &key, &cfg).unwrap();
    assert!(lease.permit.check(&key, &cfg).is_ok());
    assert!(registry.connection(&key, &cfg).is_err());
    assert!(registry.confirm_probe(&challenge.id, &key, &cfg).is_err());
    let live = lease.permit.clone();
    drop(lease);
    assert!(live.cancel.is_cancelled());
    let formal = grant(&registry, &key, &cfg);
    assert!(registry.confirm_probe("no-challenge", &key, &cfg).is_err());
    assert!(!formal.cancel.is_cancelled());
}

#[test]
fn mismatched_and_expired_probe_confirmations_are_consumed() {
    let registry = LaunchRegistry::default();
    let key = key("fixture");
    let cfg = config();
    let foreign = PoolKey {
        project_id: Some("other-project".into()),
        ..key.clone()
    };
    let challenge = registry.prepare_probe(&key, &cfg).unwrap();
    assert!(
        registry
            .confirm_probe(&challenge.id, &foreign, &cfg)
            .is_err()
    );
    assert!(registry.confirm_probe(&challenge.id, &key, &cfg).is_err());
    let challenge = registry.prepare_probe(&key, &cfg).unwrap();
    let mut changed = cfg.clone();
    changed.args.push("--changed".into());
    assert!(
        registry
            .confirm_probe(&challenge.id, &key, &changed)
            .is_err()
    );
    assert!(registry.confirm_probe(&challenge.id, &key, &cfg).is_err());
    let challenge = registry.prepare_probe(&key, &cfg).unwrap();
    registry
        .0
        .lock()
        .unwrap()
        .probes
        .get_mut(&challenge.id)
        .unwrap()
        .issued = Instant::now() - Duration::from_secs(301);
    assert!(registry.confirm_probe(&challenge.id, &key, &cfg).is_err());
    assert!(registry.confirm_probe(&challenge.id, &key, &cfg).is_err());
}

#[test]
fn pending_and_active_probe_capacity_recovers_after_expiry_or_completion() {
    let registry = LaunchRegistry::default();
    let key = key("fixture");
    let cfg = config();
    let pending: Vec<_> = (0..64)
        .map(|_| registry.prepare_probe(&key, &cfg).unwrap())
        .collect();
    assert!(registry.prepare_probe(&key, &cfg).is_err());
    let active = registry.confirm_probe(&pending[0].id, &key, &cfg).unwrap();
    assert!(registry.prepare_probe(&key, &cfg).is_err());
    registry
        .0
        .lock()
        .unwrap()
        .probes
        .get_mut(&pending[1].id)
        .unwrap()
        .issued = Instant::now() - Duration::from_secs(301);
    let new = registry.prepare_probe(&key, &cfg).unwrap();
    assert!(registry.confirm_probe(&pending[1].id, &key, &cfg).is_err());
    assert!(registry.confirm_probe(&new.id, &key, &cfg).is_ok());
    drop(active);
    assert!(registry.prepare_probe(&key, &cfg).is_ok());
}

#[test]
fn revoke_cancels_active_and_pending_probes_without_touching_other_entries() {
    let registry = LaunchRegistry::default();
    let key = key("fixture");
    let other = PoolKey {
        name: "other".into(),
        ..key.clone()
    };
    let cfg = config();
    let formal = grant(&registry, &key, &cfg);
    let unrelated = grant(&registry, &other, &cfg);
    let pending = registry.prepare_probe(&key, &cfg).unwrap();
    let challenge = registry.prepare_probe(&key, &cfg).unwrap();
    let lease = registry.confirm_probe(&challenge.id, &key, &cfg).unwrap();
    let other_challenge = registry.prepare_probe(&other, &cfg).unwrap();
    registry.revoke(&key);
    assert!(formal.cancel.is_cancelled());
    assert!(lease.permit.cancel.is_cancelled());
    assert!(registry.confirm_probe(&pending.id, &key, &cfg).is_err());
    assert!(
        registry
            .confirm_probe(&other_challenge.id, &other, &cfg)
            .is_ok()
    );
    assert!(!unrelated.cancel.is_cancelled());
    assert!(registry.connection(&other, &cfg).is_ok());
}

#[test]
fn source_refresh_invalidates_changes_removals_and_disables_but_preserves_other_grants() {
    let registry = LaunchRegistry::default();
    let cfg = config();
    let keys = [key("changed"), key("removed"), key("disabled"), key("kept")];
    let permits: Vec<_> = keys.iter().map(|key| grant(&registry, key, &cfg)).collect();
    let other_source = PoolKey {
        source: "project-b/.codewave/mcp.json".into(),
        ..keys[0].clone()
    };
    let unrelated = grant(&registry, &other_source, &cfg);
    let pending = registry.prepare_probe(&keys[1], &cfg).unwrap();
    let challenge = registry.prepare_probe(&keys[0], &cfg).unwrap();
    let active = registry
        .confirm_probe(&challenge.id, &keys[0], &cfg)
        .unwrap();
    let mut changed = cfg.clone();
    changed.command = Some("replacement".into());
    let mut disabled = cfg.clone();
    disabled.enabled = false;
    registry.invalidate_source(
        &keys[0].source,
        &[
            ("changed".into(), changed),
            ("disabled".into(), disabled),
            ("kept".into(), cfg.clone()),
        ],
    );
    for permit in &permits[..3] {
        assert!(permit.cancel.is_cancelled());
    }
    assert!(active.permit.cancel.is_cancelled());
    assert!(registry.confirm_probe(&pending.id, &keys[1], &cfg).is_err());
    assert!(!permits[3].cancel.is_cancelled());
    assert!(registry.connection(&keys[3], &cfg).is_ok());
    assert!(!unrelated.cancel.is_cancelled());
    assert!(registry.connection(&other_source, &cfg).is_ok());
}
