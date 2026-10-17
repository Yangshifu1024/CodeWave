//! #4 选择性设计：不是每件事都要先设计。
//!
//! 判据全部落在**变更形状**与**用户意图**上，不用「任务看起来複杂吗」这种主观描述。
//!
//! | 情形 | 设计 |
//! |---|---|
//! | 机械明确改名 / 格式化 / 版本号 | 不需要 |
//! | 存储格式、序列化、公共 API、迁移 | **需要** |
//! | 并发、权限、密钥、删除路径 | **需要**（不可由模型自行豁免） |
//! | 用户明确要求完整覆盖 | 需要（默认选择被用户改写） |
//!
//! 关键纪律：**跳设计不等于跳批准**。设计可以省，验收门照走（#16）；
//! 预览方案不冻结基线；批准后范围没扩就不重启设计；范围一扩，旧决定保留为版本。

use crate::core::delivery::types::PathRef;
use crate::core::workflow::routing::{TaskKind, TaskProfile};
use serde::{Deserialize, Serialize};

/// 需要设计的理由（稳定码，便于界面与测试对账）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignTrigger {
    /// 机械明确改动。
    Mechanical,
    /// 存储格式 / 序列化 / 迁移。
    StorageFormat,
    /// 公共 API / 导出入口。
    PublicApi,
    /// 并发 / 所有权 / 原子性。
    Concurrency,
    /// 权限 / 密钥 / 受保护路径。
    Security,
    /// 删除路径 / 不可逆操作。
    Destructive,
    /// 用户明确要求完整覆盖。
    UserRequested,
}

/// 设计裁决。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignDecision {
    /// 是否需要设计。
    pub required: bool,
    /// 触发的理由（可多条）。
    pub triggers: Vec<DesignTrigger>,
    /// 人读说明。
    pub reasons: Vec<String>,
    #[serde(default)]
    pub alternatives: Vec<String>,
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub user_override: Option<super::routing::ProfileOverride>,
}

impl DesignDecision {
    fn no(trigger: DesignTrigger, why: &str) -> Self {
        DesignDecision {
            required: false,
            triggers: vec![trigger],
            reasons: vec![why.to_string()],
            alternatives: vec![],
            selected: None,
            assumptions: vec![],
            user_override: None,
        }
    }
}

/// 高危路径片段（相对路径任一段命中即触发）。
const SECURITY_MARKERS: &[&str] = &[".env", "secrets", "keyring", "credential", "token"];
const DESTRUCTIVE_MARKERS: &[&str] = &["delete", "remove", "drop", "purge", "reset"];
const STORAGE_MARKERS: &[&str] = &["schema", "migration", "serialize", "persist", "snapshot"];
const PUBLIC_MARKERS: &[&str] = &["mod.rs", "index.ts", "lib.rs", "types.rs", "api"];
const CONCURRENCY_MARKERS: &[&str] = &["lock", "atomic", "race", "concurren", "parallel"];

/// 判定是否需要设计。
///
/// `user_requested_full` 是**用户**的明确要求（经真实交互记录），不是模型自述。
pub fn decide(
    profile: &TaskProfile,
    write_scopes: &[PathRef],
    user_requested_full: bool,
) -> DesignDecision {
    let mut triggers = Vec::new();
    let mut reasons = Vec::new();

    if user_requested_full {
        triggers.push(DesignTrigger::UserRequested);
        reasons.push("用户明确要求完整覆盖：默认选择被用户改写".into());
    }

    for p in write_scopes {
        let lower = p.relative_path.to_ascii_lowercase();
        if SECURITY_MARKERS.iter().any(|m| lower.contains(m)) {
            triggers.push(DesignTrigger::Security);
            reasons.push(format!("触及权限/密钥路径 `{}`", p.relative_path));
        }
        if DESTRUCTIVE_MARKERS.iter().any(|m| lower.contains(m)) {
            triggers.push(DesignTrigger::Destructive);
            reasons.push(format!("触及删除/不可逆路径 `{}`", p.relative_path));
        }
        if STORAGE_MARKERS.iter().any(|m| lower.contains(m)) {
            triggers.push(DesignTrigger::StorageFormat);
            reasons.push(format!("触及存储格式 `{}`", p.relative_path));
        }
        if PUBLIC_MARKERS.iter().any(|m| lower.ends_with(m)) {
            triggers.push(DesignTrigger::PublicApi);
            reasons.push(format!("触及公共导出入口 `{}`", p.relative_path));
        }
        if CONCURRENCY_MARKERS.iter().any(|m| lower.contains(m)) {
            triggers.push(DesignTrigger::Concurrency);
            reasons.push(format!("触及并发/所有权 `{}`", p.relative_path));
        }
    }

    // 机械改动且没有任何高危触发 → 不需要设计。
    if triggers.is_empty() {
        if matches!(profile.kind, TaskKind::Mechanical) {
            return DesignDecision::no(
                DesignTrigger::Mechanical,
                "机械改动（单一明确步骤、无高危面）：不需要设计",
            );
        }
        return DesignDecision {
            required: true,
            triggers: vec![],
            reasons: vec!["非机械改动且未命中任何豁免：按需要设计处理".into()],
            alternatives: vec![],
            selected: None,
            assumptions: vec![],
            user_override: None,
        };
    }

    triggers.sort();
    triggers.dedup();
    reasons.dedup();
    DesignDecision {
        required: true,
        triggers,
        reasons,
        alternatives: vec![],
        selected: None,
        assumptions: vec![],
        user_override: None,
    }
}

/// 设计决定版本（范围一扩，旧决定**保留**而不是被覆盖）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignRecord {
    /// 决定 id。
    pub id: String,
    /// 版本号，从 1 起。
    pub version: u32,
    /// 适用契约修订。
    pub contract_revision: u64,
    /// 该版本覆盖的写范围（**快照**，后来的范围变更不会篡改它）。
    pub scope: Vec<PathRef>,
    /// 裁决。
    pub decision: DesignDecision,
    /// 是否已冻结基线（预览**不**冻结；批准才冻结）。
    pub baseline_frozen: bool,
    /// 时刻。
    pub at: String,
}

/// 范围扩张后的版本推进。
///
/// 返回**新版本**记录；旧版本原样保留（可查「当时为什么这么定」）。
/// 范围没变则返回 `None` —— 不因为多问一次就重启设计。
pub fn revise_on_scope_change(
    previous: &[DesignRecord],
    contract_revision: u64,
    new_scope: &[PathRef],
    decision: DesignDecision,
    at: &str,
) -> Option<DesignRecord> {
    let last = previous.last()?;
    // 契约修订没变 + 范围没变 → 不重启设计。
    if last.contract_revision == contract_revision && scope_eq(&last.scope, new_scope) {
        return None;
    }
    Some(DesignRecord {
        id: last.id.clone(),
        version: last.version + 1,
        contract_revision,
        scope: new_scope.to_vec(),
        decision,
        // 范围扩张**不**继承旧冻结：新范围尚未批准。
        baseline_frozen: false,
        at: at.to_string(),
    })
}

fn scope_eq(a: &[PathRef], b: &[PathRef]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().all(|x| {
        b.iter()
            .any(|y| y.root_id == x.root_id && y.relative_path == x.relative_path)
    })
}

#[cfg(test)]
#[path = "design_tests.rs"]
mod tests;

/// User-confirmed semantic inputs are part of the frozen work package.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignInput {
    #[serde(default)]
    pub documentation_only: bool,
    #[serde(default)]
    pub reuse_task: Option<String>,
    #[serde(default)]
    pub reuse_revision: Option<u64>,
    #[serde(default)]
    pub new_tradeoffs: Vec<DesignTrigger>,
    #[serde(default)]
    pub user_requested: bool,
}
