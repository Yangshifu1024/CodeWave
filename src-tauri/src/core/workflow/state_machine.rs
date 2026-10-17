//! #5 工作流节点状态机。
//!
//! 边只由 [`WorkflowDefinition`](super::config::WorkflowDefinition) 声明的转移决定；
//! 本模块只管**状态**与**转移合法性**，不负责派发（那是 #2）。
//!
//! 纪律：
//!
//! - **依赖未通过不能 running**。`depends_on` 里任一单元不是终态通过，就地挡住。
//! - **重复终态幂等**。同 owner 重复落终态返回既有状态，不改 revision。
//! - **旧 attempt 迟到拒绝**。`attempt` 小于当前的一律拒绝——不能让迟到的 A 覆盖 A2。
//! - **预算内修复受限**。repair 次数用尽即 `Blocked`，不再回边。
//! - **重启不重跑工具**。状态从快照恢复，恢复只改显示，不重新执行。

use crate::core::delivery::types::RunIdentity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 节点状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// 待派发。
    Pending,
    /// 已准入并占用写范围租约（#2）。
    Ready,
    /// 运行中。
    Running,
    /// 通过。
    Passed,
    /// 明确失败。
    Failed,
    /// 阻塞（等用户 / 等环境）。
    Blocked,
    /// 取消。
    Cancelled,
}

impl NodeState {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeState::Pending => "pending",
            NodeState::Ready => "ready",
            NodeState::Running => "running",
            NodeState::Passed => "passed",
            NodeState::Failed => "failed",
            NodeState::Blocked => "blocked",
            NodeState::Cancelled => "cancelled",
        }
    }

    /// 是否终态。终态只由同 owner 的终态消息改变。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeState::Passed | NodeState::Failed | NodeState::Blocked | NodeState::Cancelled
        )
    }
}

/// 一个单元的节点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// 单元 id。
    pub unit_id: String,
    /// 当前状态。
    pub state: NodeState,
    /// 当前 attempt（从 0 起；重试 +1）。
    #[serde(default)]
    pub attempt: u32,
    /// 占用租约的 generation（#2 释放时校验用）。
    #[serde(default)]
    pub lease_generation: u64,
    /// 已用修复轮。
    #[serde(default)]
    pub repair_rounds: u32,
    /// 状态修订（每次真实转移 +1）。
    #[serde(default)]
    pub revision: u64,
    /// 可定位原因。
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// 冻结的工作流运行快照（#8 拥有内容，这里只是宿主视图）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRunSnapshot {
    /// 运行归属。
    pub owner: RunIdentity,
    /// 采用的定义修订（**冻结值**：配置之后改了也不影响本 run）。
    ///
    /// serde default：老快照没有这个字段时按 0 读回（显示未验证），
    /// 不能因为缺字段整份读不出来。
    #[serde(default)]
    pub definition_revision: u64,
    /// 定义 id。
    pub definition_id: String,
    /// 节点集合。
    #[serde(default)]
    pub nodes: BTreeMap<String, Node>,
    /// 依赖图：`unit_id` → 它依赖的单元。
    ///
    /// 单独存一份（而不是每次遍历 nodes）是为了转移时能**反向**查「谁依赖我」——
    /// 依赖闸门必须问的是「我的上游过了吗」，靠正向表查不出来。
    #[serde(default)]
    pub depends_on: BTreeMap<String, Vec<String>>,
    /// 转移修订（每次真实转移 +1）。
    #[serde(default)]
    pub revision: u64,
}

impl WorkflowRunSnapshot {
    /// 新建（definition_revision 冻结在此刻）。
    pub fn new(
        owner: RunIdentity,
        definition_id: impl Into<String>,
        definition_revision: u64,
        units: &[String],
    ) -> Self {
        Self::with_deps(
            owner,
            definition_id,
            definition_revision,
            units,
            &BTreeMap::new(),
        )
    }

    /// 带依赖图构造。
    pub fn with_deps(
        owner: RunIdentity,
        definition_id: impl Into<String>,
        definition_revision: u64,
        units: &[String],
        depends_on: &BTreeMap<String, Vec<String>>,
    ) -> Self {
        let mut nodes = BTreeMap::new();
        for u in units {
            nodes.insert(
                u.clone(),
                Node {
                    unit_id: u.clone(),
                    state: NodeState::Pending,
                    attempt: 0,
                    lease_generation: 0,
                    repair_rounds: 0,
                    revision: 0,
                    reason: None,
                    actor_id: None,
                },
            );
        }
        WorkflowRunSnapshot {
            owner,
            definition_revision,
            definition_id: definition_id.into(),
            nodes,
            depends_on: depends_on.clone(),
            revision: 0,
        }
    }

    /// 反向依赖：直接依赖 `unit` 的那些单元。
    pub fn dependents_of(&self, unit: &str) -> Vec<String> {
        self.depends_on
            .iter()
            .filter(|(_, ds)| ds.iter().any(|d| d == unit))
            .map(|(u, _)| u.clone())
            .collect()
    }

    pub fn node(&self, unit: &str) -> Option<&Node> {
        self.nodes.get(unit)
    }
}

/// 转移拒绝的原因。
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    /// 稳定码。
    pub code: &'static str,
    /// 说明。
    pub message: String,
}

impl Rejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Rejection {
            code,
            message: message.into(),
        }
    }
}

/// 允许的边（来自定义）。
pub fn edge_allowed(def: &super::config::WorkflowDefinition, from: &str, to: &str) -> bool {
    def.edges.iter().any(|e| e.from == from && e.to == to)
}

/// 依赖是否全部通过。
///
/// **依赖未通过不能 running**；一次失败**不使依赖自动 ready**（`partial` / `failed`
/// 都不算通过，必须显式完成或豁免）。
pub fn deps_satisfied(snap: &WorkflowRunSnapshot, deps: &[String]) -> bool {
    deps.iter().all(|d| {
        snap.node(d)
            .map(|n| n.state == NodeState::Passed)
            .unwrap_or(false)
    })
}

/// 申请把某单元推进到 `Running`。
#[allow(clippy::too_many_arguments)]
pub fn transition(
    snap: &mut WorkflowRunSnapshot,
    def: &super::config::WorkflowDefinition,
    unit_id: &str,
    to: NodeState,
    attempt: u32,
    lease_generation: u64,
    max_repair_rounds: u32,
    actor: &RunIdentity,
) -> Result<Node, Rejection> {
    let Some(node) = snap.nodes.get(unit_id).cloned() else {
        return Err(Rejection::new(
            "unknown_unit",
            format!("未知单元 `{unit_id}`"),
        ));
    };
    // 身份校验：只有同一 root run 的 actor 能推进。
    if actor.session_id != snap.owner.session_id || actor.root_run_id != snap.owner.root_run_id {
        return Err(Rejection::new(
            "identity_mismatch",
            "推进者与快照归属不是同一会话 / root run",
        ));
    }
    if (node.state == NodeState::Running || to != NodeState::Running)
        && node
            .actor_id
            .as_deref()
            .is_some_and(|id| id != actor.actor_id)
    {
        return Err(Rejection::new("actor_mismatch", "节点已绑定其他执行者"));
    }
    // 迟到结果：旧 attempt 不得覆盖后继。
    if attempt < node.attempt {
        return Err(Rejection::new(
            "stale_attempt",
            format!(
                "attempt {attempt} 早于当前 {}：迟到结果不得覆盖后继尝试",
                node.attempt
            ),
        ));
    }
    // 重复终态幂等：同终态重复上报返回既有状态，不进位。
    if node.state == to && node.attempt == attempt && node.lease_generation == lease_generation {
        return Ok(node);
    }
    // 修复复活：`Failed` 可以沿 repair 边重试（预算内），这正是有界修复的意义。
    // 但 `Passed` / `Cancelled` 是**真终态**，永不复活。
    let repair_targets_me = def
        .edges
        .iter()
        .any(|e| e.kind == super::config::EdgeKind::Repair && e.to == unit_id);
    let is_revival = node.state == NodeState::Failed
        && to == NodeState::Running
        && repair_targets_me
        && attempt > node.attempt;
    // 终态只能由同 owner 的终态消息改变；除修复复活外，已终态不得被改写。
    if node.state.is_terminal() && to != node.state && !is_revival {
        return Err(Rejection::new(
            "terminal_immutable",
            format!(
                "`{unit_id}` 已是终态 {}，不得改判为 {}",
                node.state.as_str(),
                to.as_str()
            ),
        ));
    }
    let legal = matches!(
        (node.state, to),
        (
            NodeState::Pending,
            NodeState::Ready | NodeState::Running | NodeState::Cancelled | NodeState::Blocked
        ) | (
            NodeState::Ready,
            NodeState::Running | NodeState::Cancelled | NodeState::Blocked
        ) | (
            NodeState::Running,
            NodeState::Passed | NodeState::Failed | NodeState::Cancelled | NodeState::Blocked
        )
    );
    if !legal && !is_revival {
        return Err(Rejection::new(
            "illegal_transition",
            "状态不能跳过执行直接通过，或跨越未声明的重试边",
        ));
    }
    if !is_revival && attempt != node.attempt {
        return Err(Rejection::new(
            "attempt_mismatch",
            "新 attempt 只能通过有界 repair 边启动",
        ));
    }
    if node.state == NodeState::Running && lease_generation != node.lease_generation {
        return Err(Rejection::new(
            "lease_generation_mismatch",
            "迟到租约不能写入当前节点",
        ));
    }
    // 依赖闸门。
    if to == NodeState::Running {
        // 闸门问的是「**我的**上游过了吗」——不是「谁依赖我」。
        // 方向搞反会把整张图锁死：a 要等 b、b 要等 a，谁都起不来。
        let deps = snap.depends_on.get(unit_id).cloned().unwrap_or_default();
        if !deps_satisfied(snap, &deps) {
            return Err(Rejection::new(
                "deps_not_passed",
                format!("依赖未通过，不能进入 running：{deps:?}"),
            ));
        }
    }
    // 修复预算：repair 次数用尽即阻塞，不再回边。
    // 预算只拦**重新进入 running**：上报失败 / 通过 / 取消永远不受预算影响，
    // 否则一个已用尽预算的单元连"如实报告失败"都做不到。
    let repair = repair_targets_me && to == NodeState::Running;
    if repair && node.repair_rounds >= max_repair_rounds {
        return Err(Rejection::new(
            "repair_budget_exhausted",
            format!(
                "`{unit_id}` 已用 {}/{max_repair_rounds} 轮修复：不再回边",
                node.repair_rounds
            ),
        ));
    }
    let next = Node {
        unit_id: unit_id.to_string(),
        state: to,
        attempt,
        lease_generation,
        // 预算计的是**重试**：从 Failed 复活才消耗一轮，首次启动不消耗。
        repair_rounds: if is_revival {
            node.repair_rounds + 1
        } else {
            node.repair_rounds
        },
        revision: node.revision + 1,
        reason: None,
        actor_id: if to == NodeState::Running {
            Some(actor.actor_id.clone())
        } else {
            node.actor_id.clone()
        },
    };
    snap.nodes.insert(unit_id.to_string(), next.clone());
    snap.revision += 1;
    Ok(next)
}

#[cfg(test)]
#[path = "state_machine_tests.rs"]
mod tests;
