//! 内置工具层：所有工具的实现与注册。纯函数化、不依赖 tauri；
//! 工具分级（ToolKind）与审批/fence 语义见 `tool.rs` 与 docs/33。
pub mod ask;
pub mod batch;
pub mod batch_read;
pub mod calculate;
pub mod claims;
pub mod command;
pub mod compact;
pub mod create;
pub mod delete;
pub mod document;
pub mod edit;
pub mod encoding;
#[cfg(test)]
mod fuzz;
pub mod grep;
pub mod http_request;
pub mod list_files;
pub mod net;
pub mod pathutil;
pub mod plan;
pub mod postcheck;
pub mod read;
pub mod registry;
pub mod render_html;
pub mod sanitize;
pub mod scheduled_task;
pub mod service;
pub mod skill;
pub mod subagent;
pub mod suggest;
pub mod validation;
pub mod wait;
pub mod web_fetch;
pub mod writelock;

mod tool;
pub use tool::*;
