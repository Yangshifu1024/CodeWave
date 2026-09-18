//! core 层聚合：agent 编排主循环、config、context（token 统计/自动压缩）、prompt、
//! projects（目录式项目注册表）、scheduler、sessions、stats、session_log 等。
//! 分层规则：本层不依赖 tauri，事件经 EventSink trait 由 host 层注入（docs/02-technical-design.md）。

pub mod agent;
pub mod config;
pub mod context;
pub mod logging;
pub mod openers;
pub mod prefs;
pub mod projects;
pub mod quota;
pub mod prompt;
pub mod scheduler;
pub mod session_log;
pub mod sessions;
pub mod stats;
pub mod title;
pub mod types;
pub mod ui_state;
