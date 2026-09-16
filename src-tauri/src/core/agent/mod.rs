//! Agent 编排核心（G4，[docs/p0-plan](../../../../docs/p0-plan.md) §5）：主循环、注入、取消、检查点、重试。
//! 本模块不依赖 tauri —— 事件一律经 EventSink trait 流出，由 host 层注入实现。
//!
//! [docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构：自原单一 agent.rs 纯代码搬移而来 ——
//! runtime.rs = Frame / EventSink / SessionRuntime / AgentCore，drive.rs = 运行主循环与参数，
//! stream.rs = 流组装与刷新循环，guards.rs = panic/压缩守卫，测试模块各自独立成文件。
//! 下方 re-export 保证所有外部路径（`crate::core::agent::Xxx`）逐字节不变。

mod drive;
mod guards;
mod runtime;
mod stream;
mod supervise;

#[cfg(test)]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use drive::{
    DriveParams, NormalizedCall, drive_agent, main_drive_params,
    run_task_agent,
};
pub use runtime::{AgentCore, EventSink, Frame, SessionRuntime};
pub use supervise::CallSig;
pub(crate) use guards::{CompactingGuard, lock_ok};
/// `<report>` 标记剥离（子代理 / 任务运行收尾消费）
///（[docs/subagent-text-turn-premature-exit]）。
pub(crate) use drive::split_report;
