//! MCP 客户端管理（[docs/p1-plan](../../../docs/p1-plan.md) §4）：官方 rmcp SDK。
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
//! 连接按 `(作用域, 项目 id, server 名)` 作键，每个会话登记自己的可见集，
//! 工具注入与调用都先过可见集校验 —— 项目 A 的 MCP 工具不会再出现在项目 B 的请求里。

mod config;
mod error;
mod manager;
mod process;
mod tools;

pub use config::{
    ConfigIssue, DEFAULT_TIMEOUT_MS, IssueLevel, McpServerConfig, McpTransport, MergedServer,
    Scope, ScopeDoc, ScopeRef, ToolFilter, ToolFilterMode, default_doc_text, global_scope,
    load_scope, merge_scopes, project_scope, set_always_allow,
};
pub use error::{McpError, McpErrorKind};
pub use manager::{
    DEFAULT_MAX_SERVERS, INIT_TIMEOUT, McpCallOutput, McpManager, McpState, McpStatusPayload,
    PoolKey, StatusSink,
};
pub use tools::{
    McpTool, McpToolDef, normalize_schema, server_function_name, to_provider_tool_defs,
};
