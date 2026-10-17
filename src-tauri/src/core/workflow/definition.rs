//! The project node schema is compiled into the existing stage scheduler.
use super::config::{Issue, WorkflowDefinition};
use crate::core::delivery::types::TaskContract;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Design,
    #[default]
    Implement,
    Review,
    Verify,
    Integrate,
    Complete,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRule {
    pub id: String,
    pub from: String,
    pub to: String,
    pub max_rounds: u32,
    pub fingerprint_budget: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPolicy {
    pub node_id: String,
    pub provider_id: String,
    pub model_entry_id: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn issue(pointer: String, code: &'static str, message: impl Into<String>) -> Issue {
    Issue {
        pointer,
        code,
        message: message.into(),
    }
}
pub fn issues(def: &WorkflowDefinition, models: &BTreeSet<String>) -> Vec<Issue> {
    let mut out = vec![];
    for (ptr, id) in std::iter::once(("/id".into(), def.id.as_str()))
        .chain(
            def.stages
                .iter()
                .enumerate()
                .map(|(i, n)| (format!("/nodes/{i}/id"), n.id.as_str())),
        )
        .chain(
            def.checks
                .iter()
                .enumerate()
                .map(|(i, c)| (format!("/checks/{i}/id"), c.id.as_str())),
        )
    {
        if !valid_id(id) {
            out.push(issue(
                ptr,
                "invalid_id",
                "ID must contain 1–64 ASCII letters, digits, underscore or hyphen",
            ));
        }
    }
    let checks: BTreeSet<_> = def.checks.iter().map(|c| &c.id).collect();
    if checks.len() != def.checks.len() {
        out.push(issue(
            "/checks".into(),
            "duplicate_check",
            "Check IDs must be unique",
        ));
    }
    for (i, n) in def.stages.iter().enumerate() {
        if n.role_id
            .as_ref()
            .is_some_and(|r| !super::config::KNOWN_ROLES.contains(&r.as_str()))
        {
            out.push(issue(
                format!("/nodes/{i}/role_id"),
                "unknown_role",
                "Unknown role",
            ));
        }
        for (j, t) in n.tool_allowlist.iter().enumerate() {
            if t == "*" || !super::config::known_tool(t) {
                out.push(issue(
                    format!("/nodes/{i}/tool_allowlist/{j}"),
                    "unknown_tool",
                    "Node tools must name a known tool explicitly",
                ));
            }
        }
        if matches!(n.kind, NodeKind::Review | NodeKind::Verify) && n.required_checks.is_empty() {
            out.push(issue(
                format!("/nodes/{i}/required_checks"),
                "evidence_required",
                "Review and verify nodes need explicit checks",
            ));
        }
        if n.kind == NodeKind::Complete
            && (!n.tool_allowlist.is_empty()
                || !def
                    .edges
                    .iter()
                    .any(|e| e.to == n.id && e.kind == super::config::EdgeKind::Advance))
        {
            out.push(issue(
                format!("/nodes/{i}"),
                "completion_join_required",
                "Complete must be an evidence join without tools",
            ));
        }
    }
    let mut rule_ids = BTreeSet::new();
    for (i, r) in def.repair_rules.iter().enumerate() {
        if !valid_id(&r.id)
            || !rule_ids.insert(&r.id)
            || r.max_rounds == 0
            || r.max_rounds > crate::core::delivery::completion::MAX_REPAIR_ROUNDS
            || r.fingerprint_budget == 0
            || r.fingerprint_budget > 2
            || !def.stages.iter().any(|n| n.id == r.from)
            || !def.stages.iter().any(|n| n.id == r.to)
        {
            out.push(issue(
                format!("/repair_rules/{i}"),
                "invalid_repair_rule",
                "Repair references and budgets must fit the host budget",
            ));
        }
    }
    for (i, e) in def
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == super::config::EdgeKind::Repair)
    {
        if !def
            .repair_rules
            .iter()
            .any(|r| r.from == e.from && r.to == e.to)
        {
            out.push(issue(
                format!("/edges/{i}"),
                "repair_rule_required",
                "Repair edges need a bounded rule",
            ));
        }
    }
    let mut policy_nodes = BTreeSet::new();
    for (i, p) in def.model_policies.iter().enumerate() {
        if p.reasoning_effort.as_deref().is_some_and(|effort| {
            effort != "default" && crate::core::prefs::EffortLevel::parse(effort).is_none()
        }) {
            out.push(issue(
                format!("/model_policies/{i}/reasoning_effort"),
                "unknown_reasoning_effort",
                "Unsupported reasoning effort",
            ));
        }
        if !policy_nodes.insert(&p.node_id) {
            out.push(issue(
                format!("/model_policies/{i}/node_id"),
                "duplicate_model_policy",
                "A node has one model policy",
            ));
        }
    }
    for (i, p) in def.model_policies.iter().enumerate() {
        if !def.stages.iter().any(|n| n.id == p.node_id)
            || !models.contains(&p.model_entry_id)
            || p.provider_id.is_empty()
        {
            out.push(issue(
                format!("/model_policies/{i}"),
                "unknown_model_policy",
                "Node/provider/model reference is unavailable",
            ));
        }
    }
    for (role, p) in &def.roles {
        for (i, k) in p.max_tool_kinds.iter().enumerate() {
            if !["read_only", "file_write", "network", "interactive", "meta"].contains(&k.as_str())
            {
                out.push(issue(
                    format!("/role_policies/{role}/max_tool_kinds/{i}"),
                    "unknown_tool_kind",
                    "Unknown capability kind",
                ));
            }
        }
    }
    out
}
pub fn validate_bound_definition(
    def: &WorkflowDefinition,
    contract: &TaskContract,
) -> Result<(), Vec<Issue>> {
    let mut errors = vec![];
    for (i, n) in def.stages.iter().enumerate() {
        for (j, r) in n.requirement_ids.iter().enumerate() {
            if r != "*" && contract.requirement(r).is_none() {
                errors.push(issue(
                    format!("/nodes/{i}/requirement_ids/{j}"),
                    "requirement_not_found",
                    "Requirement is not in this contract",
                ));
            }
        }
        for (j, id) in n.required_checks.iter().enumerate() {
            let check = contract
                .check(id)
                .or_else(|| contract.check(&format!("project_{id}")));
            if let Some(check) = check
                && (n.kind == NodeKind::Review
                    && check.kind != crate::core::delivery::types::CheckKind::Review
                    || n.kind == NodeKind::Verify
                        && !check.independent_verifier
                        && check.kind != crate::core::delivery::types::CheckKind::Manual)
            {
                errors.push(issue(
                    format!("/nodes/{i}/required_checks/{j}"),
                    "check_duty_mismatch",
                    "Check does not satisfy this node's duty",
                ));
            }
            if check.is_none() {
                errors.push(issue(
                    format!("/nodes/{i}/required_checks/{j}"),
                    "check_not_found",
                    "Check is not bound to this contract",
                ));
            }
        }
        if n.kind == NodeKind::Complete {
            let incoming: BTreeSet<_> = def
                .edges
                .iter()
                .filter(|e| e.to == n.id && e.kind == super::config::EdgeKind::Advance)
                .map(|e| e.from.as_str())
                .collect();
            if !def.stages.iter().any(|s| {
                incoming.contains(s.id.as_str())
                    && matches!(s.kind, NodeKind::Verify | NodeKind::Review)
            }) {
                errors.push(issue(
                    format!("/nodes/{i}"),
                    "completion_join_required",
                    "Complete needs a review/verification predecessor",
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Locate external JSON shape errors before typed deserialization; persisted defaults
/// remain independent from the external template boundary.
pub fn json_issues(value: &serde_json::Value) -> Vec<Issue> {
    let mut out = vec![];
    fn object(value: &serde_json::Value, pointer: &str, allowed: &[&str], out: &mut Vec<Issue>) {
        if let Some(map) = value.as_object() {
            for key in map.keys() {
                if !allowed.contains(&key.as_str()) {
                    out.push(issue(
                        format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1")),
                        "unknown_field",
                        "Unknown field",
                    ));
                }
            }
        }
    }
    object(
        value,
        "",
        &[
            "schema",
            "id",
            "revision",
            "nodes",
            "edges",
            "repair_rules",
            "role_policies",
            "model_policies",
            "checks",
            "author",
        ],
        &mut out,
    );
    for (name, allowed) in [
        (
            "nodes",
            vec![
                "id",
                "title",
                "kind",
                "requirement_ids",
                "role_id",
                "tool_allowlist",
                "next",
                "required_checks",
            ],
        ),
        ("edges", vec!["from", "to", "kind"]),
        (
            "repair_rules",
            vec!["id", "from", "to", "max_rounds", "fingerprint_budget"],
        ),
        (
            "model_policies",
            vec![
                "node_id",
                "provider_id",
                "model_entry_id",
                "reasoning_effort",
            ],
        ),
        (
            "checks",
            vec![
                "id",
                "command_text",
                "cwd",
                "dangerous",
                "platforms",
                "prerequisites",
            ],
        ),
    ] {
        if let Some(items) = value[name].as_array() {
            for (index, item) in items.iter().enumerate() {
                object(item, &format!("/{name}/{index}"), &allowed, &mut out);
            }
        }
    }
    if let Some(roles) = value["role_policies"].as_object() {
        for (role, policy) in roles {
            object(
                policy,
                &format!("/role_policies/{role}"),
                &["tools", "model_id", "independent_review", "max_tool_kinds"],
                &mut out,
            );
        }
    }
    out
}
