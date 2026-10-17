//! 任务编排域（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）。
//!
//! 交付域回答「**凭什么算完成**」，编排域回答「**这件事该怎么拆、谁来做、按什么顺序**」。
//! 两者的分工是硬的：编排域**不判定验收**，交付域**不调度单元**。
//!
//! | 模块 | 归属 | 职责 |
//! |---|---|---|
//! | [`routing`] | #1 | 任务分类、工作面与工作单元所有权 |
//! | [`verification_policy`] | #3 | 改动面 → 建议检查 |
//! | [`design`] | #4 | 选择性设计（不是每件事都要设计） |
//! | [`state_machine`] | #5 | 单元状态机与转移 |
//! | [`config`] | #6 | 工作流定义 schema（唯一来源） |
//! | [`snapshot`] | #8 | 冻结的运行快照 |
//! | [`dispatch`] | #2 | 原子准入与写范围租约（消费 #1 的工作单元） |
//!
//! 跨层纪律：`core` 不依赖 tauri；宿主接线一律经 `host/commands/`；
//! 模型只能**建议**分类与计划，宿主按结构化图与真实范围核验后生效。

pub mod config;
pub mod definition;
pub mod design;
pub mod dispatch;
pub mod environment;
pub mod lifecycle;
pub mod project;
pub mod projection;
pub mod routing;
pub mod service;
pub mod snapshot;
pub mod state_machine;
pub mod verification_policy;

#[allow(unused_imports)]
pub use config::{
    WorkflowDefinition, commit as commit_definition, validate as validate_definition,
};
#[allow(unused_imports)]
pub use dispatch::{DispatchLease, SideEffectScope, admission, release};
#[allow(unused_imports)]
pub use routing::{RouteDecision, TaskKind, TaskProfile, WorkUnit, route};
#[allow(unused_imports)]
pub use snapshot::{FrozenSnapshot, freeze, restore};
#[allow(unused_imports)]
pub use state_machine::{NodeState, WorkflowRunSnapshot, transition};
