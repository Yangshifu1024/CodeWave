//! MCP 客户端管理（[docs/technical-design](../../../docs/technical-design.md)）：官方 rmcp SDK。
//! 传输层：stdio（子进程，**进程树由 Job Object / 进程组回收**）与 streamable-http。
//! 函数名映射 `mcp__<server>__<tool>`；全部工具（内置 + MCP）按名排序，保持请求字节稳定。
//!
//! # 模块结构
//!
//! - [`config`]：mcp.json 形状定稿、两层作用域装载（全局 + 项目）、逐条目容错、
//!   未知键透传、来源诊断
//! - [`error`]：结构化错误分类（配置错 / spawn / 握手 / 调用 / 取消）
//! - [`process`]：stdio 子进程 spawn 参数与进程树回收
//! - [`tools`]：工具命名（`mcp__<server>__<tool>`）、schema 归一化、工具过滤
//! - [`manager`]：**会话可见集** + 连接池（引用计数 + 上限 LRU）+ 调用分发
//!
//! # 会话隔离（本次重构的核心）
//!
//! 连接按 `(作用域, 项目 id, server 名, 配置路径身份)` 作键，每个会话登记自己的可见集，
//! 工具注入与调用都先过可见集校验 —— 项目 A 的 MCP 工具不会再出现在项目 B 的请求里。

mod config;
mod error;
mod manager;
mod process;
mod review_service;
mod service;
mod tools;
mod trust;

pub use config::{
    CONFIG_COMMIT, ConfigIssue, DEFAULT_TIMEOUT_MS, IssueLevel, McpServerConfig, McpTransport,
    MergedServer, Scope, ScopeDoc, ScopeRef, ToolFilter, ToolFilterMode, default_doc_text,
    global_scope, load_scope, merge_scopes, parse_scope_text, project_scope, read_scope_text,
    scope_version, source_identity, validate_server,
};
pub use error::{McpError, McpErrorKind};
pub use manager::{
    DEFAULT_MAX_SERVERS, INIT_TIMEOUT, LaunchReview, LaunchReviewReceipt, McpCallOutput,
    McpManager, McpState, McpStatusPayload, PoolKey, ReviewAction, StatusSink,
};
pub use tools::{
    McpTool, McpToolDef, normalize_schema, server_function_name, to_provider_tool_defs,
};

pub(crate) use process::wrap_process_tree;

pub use review_service::{confirm_launch_review, launch_operation, prepare_launch_review};
pub use service::{SaveReceipt, launch_key, save_configuration, set_launch_trust, visible_servers};
pub use trust::{LaunchStatus, ProbeChallenge, launch_version};
