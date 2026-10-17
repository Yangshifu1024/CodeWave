//! #8 冻结的工作流运行快照。
//!
//! 一次 run 在开始时**冻结**它采用的工作流定义，此后配置怎么改都不影响它：
//! 新 run 采用新版，运行中的旧 run 保持原样。
//!
//! 四条纪律（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：
//!
//! 1. **快照缺失不能拿当前定义补齐**。补齐等于让旧 run 悄悄换掉规则——它跑的是旧规则，
//!    补出来的结论却按新规则解释。读不到就是读不到，显示「缺少运行快照」。
//! 2. **重启不恢复高权限**。快照里**不存**权限档位；恢复时按当时的既有 step 语义
//!    重新解析，撤销的权限不会被快照偷偷带回来。
//! 3. **跨项目引用拒绝**。快照记录项目标识，恢复时项目不匹配即拒绝。
//! 4. **模型被删阻塞**。快照引用的模型 / 角色在新配置里不存在时，阻塞而不是静默换人。

use crate::core::delivery::types::{PathRef, RunIdentity};
use crate::core::workflow::config::WorkflowDefinition;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 冻结快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenSnapshot {
    /// 归属。
    pub owner: RunIdentity,
    /// 项目标识（恢复时必须匹配）。
    pub project_id: String,
    /// 冻结的定义内容（**内嵌**，不靠 id 回查当前配置）。
    #[serde(default)]
    pub definition: Option<WorkflowDefinition>,
    /// 冻结时的单元 → 角色 / 模型。
    #[serde(default)]
    pub unit_bindings: BTreeMap<String, UnitBinding>,
    /// 冻结时的写范围（供恢复后核对；不回查可变注册表替代历史根）。
    #[serde(default)]
    pub roots: BTreeMap<String, Vec<PathRef>>,
    /// 时刻。
    pub at: String,
    #[serde(default)]
    pub contract_revision: u64,
    #[serde(default)]
    pub definition_digest: String,
    #[serde(default)]
    pub scope_version: Option<crate::core::delivery::version::ScopeVersion>,
    #[serde(default)]
    pub resource_hashes: BTreeMap<String, String>,
}

/// 单元绑定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnitBinding {
    /// 角色 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    /// 模型 id（**快照值**：配置后续删掉该模型不影响已冻结的 run）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

/// 冻结：把定义与绑定复制进快照。
pub fn freeze(
    owner: RunIdentity,
    project_id: impl Into<String>,
    definition: &WorkflowDefinition,
    unit_bindings: BTreeMap<String, UnitBinding>,
    roots: BTreeMap<String, Vec<PathRef>>,
    at: impl Into<String>,
) -> FrozenSnapshot {
    FrozenSnapshot {
        owner,
        project_id: project_id.into(),
        // **内嵌定义**：只存 id 再回查当前配置的话，运行中改配置就会改掉旧 run 的规则。
        definition: Some(definition.clone()),
        unit_bindings,
        roots,
        at: at.into(),
        contract_revision: 0,
        definition_digest: crate::core::delivery::version::digest_of_bytes(
            &serde_json::to_vec(definition).unwrap_or_default(),
        ),
        scope_version: None,
        resource_hashes: BTreeMap::new(),
    }
}

/// 恢复拒绝。
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreRejection {
    pub code: &'static str,
    pub message: String,
}

/// 从快照恢复绑定（**不重新解析当前配置**）。
pub fn restore(
    snap: &FrozenSnapshot,
    session_id: &str,
    project_id: &str,
    available_roles: &BTreeSet<String>,
    available_models: &BTreeSet<String>,
) -> Result<BTreeMap<String, UnitBinding>, RestoreRejection> {
    if snap.owner.session_id != session_id {
        return Err(RestoreRejection {
            code: "cross_session",
            message: "快照不属于本会话".into(),
        });
    }
    if snap.project_id != project_id {
        return Err(RestoreRejection {
            code: "cross_project",
            message: format!(
                "快照来自项目 `{}`，当前项目是 `{project_id}`：跨项目引用拒绝",
                snap.project_id
            ),
        });
    }
    // 快照缺失 → 不得拿当前定义补齐。
    if snap.definition.is_none() {
        return Err(RestoreRejection {
            code: "snapshot_missing_definition",
            message: "快照里没有冻结的定义：不拿当前定义补齐（旧 run 跑的是旧规则，补齐会让结论按新规则解释）"
                .into(),
        });
    }
    // 模型 / 角色被删 → 阻塞，不静默换人。
    let mut out = BTreeMap::new();
    for (unit, b) in &snap.unit_bindings {
        if let Some(r) = b.role_id.as_ref().filter(|r| !available_roles.contains(*r)) {
            return Err(RestoreRejection {
                code: "role_removed",
                message: format!("单元 `{unit}` 绑定的角色 `{r}` 已被删除：阻塞，不静默换人"),
            });
        }
        if let Some(m) = b
            .model_id
            .as_ref()
            .filter(|m| !available_models.contains(*m))
        {
            return Err(RestoreRejection {
                code: "model_removed",
                message: format!("单元 `{unit}` 绑定的模型 `{m}` 已被删除：阻塞，不静默换人"),
            });
        }
        out.insert(unit.clone(), b.clone());
    }
    Ok(out)
}

/// 权限档位**不在快照里**——恢复时按当时的既有 step 语义重新解析。
///
/// 返回 `None` 表示"快照不携带权限"，调用方必须走正常档位解析路径。
/// 之所以显式声明而不是"忘了存"：把权限冻进快照就等于让一次撤销失效的授权复活。
pub fn snapshot_permission(snap: &FrozenSnapshot) -> Option<&serde_json::Value> {
    let _ = snap;
    None
}

/// 供 IPC / 界面使用的只读摘要。
pub fn summary(snap: &FrozenSnapshot) -> serde_json::Value {
    serde_json::json!({
        "project_id": snap.project_id,
        "at": snap.at,
        "definition_revision": snap.definition.as_ref().map(|d| d.revision),
        "definition_id": snap.definition.as_ref().map(|d| d.id.clone()),
        "units": snap.unit_bindings.keys().collect::<Vec<_>>(),
        "carries_permission": false,
    })
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
