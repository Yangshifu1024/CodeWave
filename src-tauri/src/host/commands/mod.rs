//! 全部 IPC 命令（[docs/technical-design](../../../../docs/technical-design.md) 的 IPC 边界）。只做参数校验 + 转调 core，不放业务逻辑。
//! 各领域子模块持有命令 fn；本枢纽 re-export 它们，使 lib.rs `generate_handler!` 注册的
//! `host::commands::xxx` 路径保持稳定。

mod activity;
mod agents;
mod delivery;
mod git;
mod instruction_resources;
mod logs;
mod mcp;
mod openers;
mod preview;
mod project;
mod quota;
mod scheduler;
mod session;
mod skills;
mod stats;
mod system;
mod ui_state;
mod util;
mod workflow;
mod workspace;

pub use activity::*;
pub use agents::*;
pub use delivery::*;
pub use git::*;
pub use instruction_resources::*;
pub use logs::*;
pub use mcp::*;
pub use openers::*;
pub use preview::*;
pub use project::*;
pub use quota::*;
pub use scheduler::*;
pub use session::*;
pub use skills::*;
pub use stats::*;
pub use system::*;
pub use ui_state::*;
pub use workflow::*;
pub use workspace::*;
