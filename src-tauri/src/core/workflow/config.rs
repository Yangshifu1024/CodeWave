//! #6 项目工作流配置：**唯一 schema 来源**。
//!
//! 写在 `<主目录>/.codewave/workflows.json`，由 workflow 工具向真实用户确认后保存；尚无独立设置页编辑器。
//! 它定义**可用的阶段与边**，不是某个 run 的状态（那是 #5 / #8）。
//!
//! 安全纪律（[docs/tools-and-safety](../../../../docs/tools-and-safety.md)）：
//!
//! - **保存配置不绕执行审批**。危险命令可以**存进配置**（用户可能在组织模板里声明它），
//!   但真正执行时仍走既有审批门；配置文件不是执行许可。
//! - **引用闭包严格**：未知字段 / 未知角色 / 未知模型 / 未知工具一律拒绝，不静默丢弃。
//! - **只有 `repair` 边允许成环**。普通环意味着无限循环，直接拒绝。
//! - **跨项目路径拒绝**。配置里的路径必须落在本项目主目录内。
//! - **CAS 冲突保留草稿**，**磁盘失败不丢原有效版本**，**外部文件修改不自动应用**。

use crate::core::delivery::types::RunIdentity;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 配置 schema 版本。
pub const WORKFLOW_SCHEMA: u32 = 1;

/// 工作流定义（项目级）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    /// schema 版本。
    pub schema: u32,
    /// 定义 id。
    pub id: String,
    /// 修订号（每次保存 +1）。
    #[serde(default)]
    pub revision: u64,
    /// 支持的阶段。
    pub stages: Vec<Stage>,
    /// 支持的边。
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// 角色政策（角色 id → 允许的工具 / 模型）。
    #[serde(default)]
    pub roles: BTreeMap<String, RolePolicy>,
    /// 检查命令模板（**只允许覆盖允许字段**，见 [`validate`]）。
    #[serde(default)]
    pub checks: Vec<CheckCommand>,
    /// 谁写的（真实 actor，模型不能自称）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// 一个阶段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    /// 阶段 id。
    pub id: String,
    /// 展示名。
    pub title: String,
    /// 允许的下一阶段（为空表示终点）。
    #[serde(default)]
    pub next: Vec<String>,
    /// 该阶段必须通过的检查 id。
    #[serde(default)]
    pub required_checks: Vec<String>,
}

/// 一条边。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// 边类别。**只有 `repair` 允许成环**。
    #[serde(default)]
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// 正常推进。
    #[default]
    Advance,
    /// 修复回边（唯一允许成环的类别）。
    Repair,
}

/// 角色政策。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePolicy {
    /// 允许使用的工具（`*` 表示不限）。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 绑定的模型 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// 是否强制独立审查身份（#9 / #10）。
    #[serde(default)]
    pub independent_review: bool,
}

/// 项目自定义检查命令。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckCommand {
    pub id: String,
    pub command_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 危险命令**允许存**，但执行仍需审批（本字段只影响展示提示）。
    #[serde(default)]
    pub dangerous: bool,
}

/// 校验问题。
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// 稳定码。
    pub code: &'static str,
    /// 说明。
    pub message: String,
}

/// 已知角色（首版固定表；配置引用未知角色即拒绝）。
pub const KNOWN_ROLES: &[&str] = &[
    "explore",
    "backend-dev",
    "frontend-dev",
    "app-dev",
    "reviewer",
    "tester",
    "product-manager",
    "code-reviewer",
];

/// 已知工具（用于 `tools` 引用闭包；`*` 除外）。
pub fn known_tool(id: &str) -> bool {
    if id == "*" {
        return true;
    }
    crate::tools::registry::ToolRegistry::all_tool_names()
        .iter()
        .any(|n| n == id)
}

/// 校验工作流定义。
///
/// 已知模型 id 只能由调用方传入（`core` 不该在此处反查可变配置）。
pub fn validate(
    def: &WorkflowDefinition,
    project_dir: &std::path::Path,
    known_model_ids: &BTreeSet<String>,
) -> Result<(), Vec<Issue>> {
    let mut issues = Vec::new();
    if def.stages.len() > 100
        || def.edges.len() > 400
        || def.checks.len() > 100
        || def.roles.len() > 50
    {
        issues.push(Issue {
            code: "definition_too_large",
            message: "工作流定义超过阶段/边/检查/角色上限".into(),
        });
        return Err(issues);
    }
    if def.stages.is_empty() {
        issues.push(Issue {
            code: "empty_stages",
            message: "工作流至少需要一个阶段".into(),
        });
    }
    if def.schema != WORKFLOW_SCHEMA {
        issues.push(Issue {
            code: "unknown_schema",
            message: format!("schema {} 不是 {WORKFLOW_SCHEMA}", def.schema),
        });
    }
    if def.id.trim().is_empty() {
        issues.push(Issue {
            code: "empty_id",
            message: "定义 id 不能为空".into(),
        });
    }
    let mut stage_ids: Vec<&str> = Vec::new();
    for s in &def.stages {
        if stage_ids.contains(&s.id.as_str()) {
            issues.push(Issue {
                code: "duplicate_stage",
                message: format!("阶段 id 重复：`{}`", s.id),
            });
        }
        if s.id.trim().is_empty() {
            issues.push(Issue {
                code: "empty_stage_id",
                message: "阶段id不能为空".into(),
            });
        }
        stage_ids.push(&s.id);
    }
    // 边引用闭包
    for e in &def.edges {
        if !stage_ids.contains(&e.from.as_str()) {
            issues.push(Issue {
                code: "edge_unknown_from",
                message: format!("边引用未知阶段 `{}`", e.from),
            });
        }
        if !stage_ids.contains(&e.to.as_str()) {
            issues.push(Issue {
                code: "edge_unknown_to",
                message: format!("边引用未知阶段 `{}`", e.to),
            });
        }
    }
    // 只有**含 repair 边**的环才允许。
    //
    // 判据是「含至少一条 repair 边」而不是「整环都是 repair 边」：有界修复环的返回路径
    // 必然由普通推进边构成，若要求整环皆 repair，则任何修复环都无法成立——那条能力就废了。
    // 没有 repair 边的环 = 普通环 = 无限循环，直接拒绝。
    // Removing repair edges must leave a DAG. Kahn's algorithm is bounded by V+E;
    // enumerating every cycle was exponential for dense user-provided graphs.
    let mut indegree: BTreeMap<&str, usize> = stage_ids.iter().map(|id| (*id, 0)).collect();
    for e in def.edges.iter().filter(|e| e.kind == EdgeKind::Advance) {
        if let Some(n) = indegree.get_mut(e.to.as_str()) {
            *n += 1;
        }
    }
    let mut queue: Vec<&str> = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut visited = 0;
    while let Some(id) = queue.pop() {
        visited += 1;
        for e in def
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Advance && e.from == id)
        {
            if let Some(n) = indegree.get_mut(e.to.as_str()) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    queue.push(&e.to);
                }
            }
        }
    }
    if visited != stage_ids.len() {
        issues.push(Issue {
            code: "ordinary_cycle_rejected",
            message: "普通推进边成环，只有显式有界 repair 回边允许重试".into(),
        });
    }
    for stage in &def.stages {
        for next in &stage.next {
            if !stage_ids.contains(&next.as_str())
                || !def
                    .edges
                    .iter()
                    .any(|e| e.from == stage.id && &e.to == next)
            {
                issues.push(Issue {
                    code: "stage_next_mismatch",
                    message: format!("阶段 {} 的next必须对应一条已声明边", stage.id),
                });
            }
        }
    }
    // 角色 / 工具 / 模型引用闭包
    for (role, policy) in &def.roles {
        if !KNOWN_ROLES.contains(&role.as_str()) {
            issues.push(Issue {
                code: "unknown_role",
                message: format!("未知角色 `{role}`"),
            });
        }
        for t in &policy.tools {
            if !known_tool(t) {
                issues.push(Issue {
                    code: "unknown_tool",
                    message: format!("角色 `{role}` 引用未知工具 `{t}`"),
                });
            }
        }
        if let Some(m) = policy
            .model_id
            .as_ref()
            .filter(|m| !known_model_ids.contains(*m))
        {
            issues.push(Issue {
                code: "unknown_model",
                message: format!("角色 `{role}` 引用未知模型 `{m}`"),
            });
        }
    }
    // 检查引用闭包 + 跨项目路径
    let check_ids: Vec<&str> = def.checks.iter().map(|c| c.id.as_str()).collect();
    for s in &def.stages {
        for c in &s.required_checks {
            if !check_ids.contains(&c.as_str()) {
                issues.push(Issue {
                    code: "unknown_check",
                    message: format!("阶段 `{}` 引用未知检查 `{c}`", s.id),
                });
            }
        }
    }
    for c in &def.checks {
        if c.command_text.trim().is_empty() {
            issues.push(Issue {
                code: "empty_command",
                message: format!("检查 `{}` 命令为空", c.id),
            });
        }
        if let Some(cwd) = c.cwd.as_ref().filter(|d| !inside_project(project_dir, d)) {
            issues.push(Issue {
                code: "cross_project_path",
                message: format!("检查 `{}` 的 cwd 越出项目主目录：`{cwd}`", c.id),
            });
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

fn inside_project(project_dir: &std::path::Path, rel: &str) -> bool {
    if rel.starts_with(['/', '\\'])
        || rel.contains(':')
        || rel.split(['/', '\\']).any(|part| part == "..")
    {
        return false;
    }
    let base = crate::tools::pathutil::canonical_best_effort(project_dir);
    crate::tools::pathutil::canonical_best_effort(&project_dir.join(rel)).starts_with(base)
}

/// 保存草稿的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SaveOutcome {
    /// 已提交。
    Committed { revision: u64 },
    /// CAS 冲突：草稿保留，**当前有效版本不动**。
    Conflict { current_revision: u64 },
    /// 磁盘失败：原有效版本不丢。
    Failed { message: String },
}

/// 提交草稿。
///
/// `expected_revision` 与当前不符即冲突；冲突与失败都**保留草稿**，
/// 且**不修改**当前有效版本（外部文件被改同样只体现为 revision 不符）。
#[allow(clippy::too_many_arguments)]
pub fn commit(
    path: &std::path::Path,
    draft: &WorkflowDefinition,
    expected_revision: u64,
    project_dir: &std::path::Path,
    known_model_ids: &BTreeSet<String>,
    author: &RunIdentity,
) -> SaveOutcome {
    let expected_path = project_dir.join(".codewave/workflows.json");
    if crate::tools::pathutil::canonical_best_effort(path)
        != crate::tools::pathutil::canonical_best_effort(&expected_path)
    {
        return SaveOutcome::Failed {
            message: "工作流配置只能保存到本项目 .codewave/workflows.json".into(),
        };
    }
    // Hold the cross-process lease across read, CAS and atomic replacement.
    let coordinator =
        crate::core::process_coordination::LeaseStore::new(&project_dir.join(".codewave/coord"));
    let lease = match coordinator.acquire(&crate::core::coordinator::AcquireRequest {
        session_id: author.session_id.clone(),
        root_run_id: author.root_run_id.clone(),
        resource: format!(
            "workflow:{}",
            crate::tools::pathutil::canonical_best_effort(path).display()
        ),
        attempt: 0,
        expected_generation: None,
    }) {
        Ok(v) => v,
        Err(e) => {
            return SaveOutcome::Failed {
                message: e.to_string(),
            };
        }
    };
    struct Release<'a>(
        &'a crate::core::process_coordination::LeaseStore,
        crate::core::coordinator::Lease,
    );
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            let _ = self.0.release(&self.1);
        }
    }
    let _guard = Release(&coordinator, lease);
    // **以磁盘上的为准**，不信调用方转述的 current：文件可能已被外部改过。
    // 读不出来（有内容但解析失败）同样按冲突处理，绝不静默覆盖。
    let on_disk: Option<WorkflowDefinition> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let current_rev = on_disk.as_ref().map(|c| c.revision).unwrap_or(0);
    if path.exists() && on_disk.is_none() {
        return SaveOutcome::Failed {
            message: "磁盘上的工作流定义无法解析：拒绝覆盖，原文件保持不变".into(),
        };
    }
    if current_rev != expected_revision {
        return SaveOutcome::Conflict {
            current_revision: current_rev,
        };
    }
    if let Err(issues) = validate(draft, project_dir, known_model_ids) {
        return SaveOutcome::Failed {
            message: issues
                .iter()
                .map(|i| format!("[{}] {}", i.code, i.message))
                .collect::<Vec<_>>()
                .join("；"),
        };
    }
    let mut next = draft.clone();
    next.revision = current_rev + 1;
    next.author = Some(author.actor_id.clone());
    match crate::core::process_coordination::atomic_write(
        path,
        &serde_json::to_vec_pretty(&next).unwrap_or_default(),
    ) {
        Ok(()) => SaveOutcome::Committed {
            revision: next.revision,
        },
        Err(e) => SaveOutcome::Failed {
            message: format!("写入失败：{e}"),
        },
    }
}

/// 危险命令是否需要额外确认（**存得下 ≠ 跑得了**）。
///
/// 保存配置不构成执行许可：返回 `true` 时执行路径仍要过一次既有审批门。
pub fn requires_execution_approval(def: &WorkflowDefinition, check_id: &str) -> bool {
    def.checks
        .iter()
        .find(|c| c.id == check_id)
        .map(|c| c.dangerous)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
