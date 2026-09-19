//! 全部 IPC 命令（[docs/technical-design](../../../../docs/technical-design.md) §5.1 的 P0 清单）。只做参数校验 + 转调 core，不放业务逻辑。
//! 各领域子模块持有命令 fn；本枢纽 re-export 它们，使 lib.rs `generate_handler!` 注册的
//! `host::commands::xxx` 路径保持稳定。

mod agents;
mod git;
mod logs;
mod mcp;
mod openers;
mod project;
mod quota;
mod scheduler;
mod session;
mod skills;
mod stats;
mod system;
mod ui_state;
mod util;
mod workspace;

pub use agents::*;
pub use git::*;
pub use logs::*;
pub use mcp::*;
pub use openers::*;
pub use project::*;
pub use quota::*;
pub use scheduler::*;
pub use session::*;
pub use skills::*;
pub use stats::*;
pub use system::*;
pub use ui_state::*;
pub use workspace::*;
