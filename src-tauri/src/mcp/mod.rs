//! MCP 客户端管理（[docs/p1-plan](../../../docs/p1-plan.md) §4）：官方 rmcp SDK（v3 API）。
//! 传输层：stdio（TokioChildProcess）与 streamable-http（server 不支持时 rmcp
//! 自动协商回退 SSE，覆盖 [docs/p1-plan](../../../docs/p1-plan.md) 的 "sse" 形态）。
//! 函数名映射 `mcp__<server>__<tool>`；全部工具（内置 + MCP）按名排序，保持请求字节稳定。
//!
//! 本模块只做出口与再导出，实现按职责拆到子模块：
//! - [`config`]：mcp.json 的 serde 类型与多层装载合并
//! - [`tools`]：工具命名（`mcp__<server>__<tool>`）与入参 schema 归一化
//! - [`manager`]：连接生命周期（建连 / 列表 / 调用 / 全停）与状态

mod config;
mod manager;
mod tools;

pub use config::{McpServerConfig, McpTransport, load_configs};
pub use manager::{CALL_TIMEOUT, INIT_TIMEOUT, McpManager, McpState};
pub use tools::{McpTool, McpToolDef, normalize_schema, server_function_name};
