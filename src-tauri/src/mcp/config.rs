//! MCP 配置形状与装载：mcp.json 的 serde 类型与多层合并（同名后者覆盖）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// MCP server 的传输形态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    /// 本地子进程（stdio 管道）
    Stdio,
    /// HTTP 流式端点（rmcp 不支持时自动回退 SSE）
    StreamableHttp,
}

/// 单个 MCP server 的连接配置（mcp.json 中 mcpServers 条目的值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// 传输形态
    pub transport: McpTransport,
    /// stdio 形态的启动命令
    #[serde(default)]
    pub command: Option<String>,
    /// stdio 形态的命令参数
    #[serde(default)]
    pub args: Vec<String>,
    /// stdio 形态的子进程环境变量
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// streamable-http 形态的端点 URL
    #[serde(default)]
    pub url: Option<String>,
}

/// 配置文件形态：{"mcpServers": {"name": {...}}}
#[derive(Default, Deserialize)]
struct McpConfigFile {
    #[serde(default)]
    #[serde(rename = "mcpServers")]
    mcp_servers: HashMap<String, McpServerConfig>,
}

/// MCP 配置装载（优先级低→高，同名后者覆盖）：
/// 用户级 ~/.codewave/mcp.json < 各仓库根 .codewave/mcp.json（兼容）< 项目托管 projects/<id>/mcp.json。
pub fn load_configs(
    data_dir: &std::path::Path,
    workspace: &std::path::Path,
    project_dir: Option<&std::path::Path>,
    extra_roots: &[String],
) -> Vec<(String, McpServerConfig)> {
    let mut out: Vec<(String, McpServerConfig)> = Vec::new();
    let mut paths = vec![data_dir.join("mcp.json")];
    for root in std::iter::once(workspace.to_path_buf())
        .chain(extra_roots.iter().map(std::path::PathBuf::from))
    {
        paths.push(
            root.join(crate::core::config::MANAGED_DIR_NAME)
                .join("mcp.json"),
        );
    }
    if let Some(pd) = project_dir {
        paths.push(pd.join("mcp.json"));
    }
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(cfg) = serde_json::from_str::<McpConfigFile>(&text) else {
            tracing::warn!("MCP 配置解析失败：{}", path.display());
            continue;
        };
        for (name, server) in cfg.mcp_servers {
            out.retain(|(n, _)| n != &name);
            out.push((name, server));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_merging() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            dd.path().join("mcp.json"),
            r#"{"mcpServers":{"fs":{"transport":"stdio","command":"npx","args":["-y","fs-server"]}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(ws.path().join(crate::core::config::MANAGED_DIR_NAME)).unwrap();
        std::fs::write(
            ws.path()
                .join(crate::core::config::MANAGED_DIR_NAME)
                .join("mcp.json"),
            r#"{"mcpServers":{"fs":{"transport":"streamable_http","url":"http://x/mcp"},"extra":{"transport":"stdio","command":"x"}}}"#,
        )
        .unwrap();
        let cfgs = load_configs(dd.path(), ws.path(), None, &[]);
        assert_eq!(cfgs.len(), 2);
        let fs = cfgs.iter().find(|(n, _)| n == "fs").unwrap();
        assert_eq!(
            fs.1.transport,
            McpTransport::StreamableHttp,
            "项目级应覆盖用户级"
        );
        assert!(cfgs.iter().any(|(n, _)| n == "extra"));
    }
}
