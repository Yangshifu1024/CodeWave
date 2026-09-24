//! 工具命名与 schema 归一化：`mcp__<server>__<tool>` 的生成 / 反查，以及入参 schema 兜底。

use serde_json::{Value, json};
use std::collections::HashMap;

/// 一个已发现的 MCP 工具（供工具注册表与模型可见的 schema）。
#[derive(Debug, Clone)]
pub struct McpTool {
    /// 所属 server 名
    pub server: String,
    /// 工具原名（server 侧定义）
    pub name: String,
    /// 工具描述（带 `[server]` 前缀，模型区分同名工具）
    pub description: String,
    /// 归一化后的入参 JSON Schema 字符串
    pub schema_json: String,
}

/// 对模型暴露的 MCP 工具定义（函数名已归一化为 provider 合法字符集）。
#[derive(Debug, Clone)]
pub struct McpToolDef {
    /// 模型可见函数名（`mcp__<server>__<tool>`，两段均归一化）
    pub function_name: String,
    /// 工具描述（带 `[server]` 前缀）
    pub description: String,
    /// 归一化后的入参 JSON Schema 字符串
    pub schema_json: String,
}

/// 把 server / tool 名归一化到 provider 允许的函数名字符集 `[A-Za-z0-9_-]`。
/// 模型可见工具名受 provider 校验（`^[a-zA-Z0-9_-]+$`），而 mcp.json 的 server 名
/// 允许任意字符（如 `PowerShell.MCP`），故拼函数名前必须归一化。
pub(crate) fn sanitize_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// 拼出对模型暴露的函数名：`mcp__<server>__<tool>`（两段均归一化到合法字符集）。
pub fn server_function_name(server: &str, tool: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_component(server),
        sanitize_component(tool)
    )
}

/// 由工具列表构建「模型可见定义 + 函数名反查表」。
/// 归一化后若撞名（不同原名映射到同一函数名）追加 `__N` 后缀去重。
pub(crate) fn build_tool_defs(
    tools: &[McpTool],
) -> (Vec<McpToolDef>, HashMap<String, (String, String)>) {
    let mut sorted: Vec<&McpTool> = tools.iter().collect();
    sorted.sort_by(|a, b| {
        (a.server.as_str(), a.name.as_str()).cmp(&(b.server.as_str(), b.name.as_str()))
    });
    let mut index: HashMap<String, (String, String)> = HashMap::new();
    let mut defs = Vec::with_capacity(sorted.len());
    for t in sorted {
        let base = server_function_name(&t.server, &t.name);
        let mut function_name = base.clone();
        let mut n = 2u32;
        while index.contains_key(&function_name) {
            function_name = format!("{base}__{n}");
            n += 1;
        }
        index.insert(function_name.clone(), (t.server.clone(), t.name.clone()));
        defs.push(McpToolDef {
            function_name,
            description: t.description.clone(),
            schema_json: t.schema_json.clone(),
        });
    }
    (defs, index)
}

/// 拆解 `mcp__<server>__<tool>` 函数名；形态非法返回 None。
/// 仅作 [`crate::mcp::McpManager`] 反查的兜底（注册表未命中时），
/// 正常路径走反查表，避免 server 名含 `__` 时切分错位。
pub(crate) fn parse_function(function: &str) -> Option<(String, String)> {
    let rest = function.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

/// Schema normalization: fills in type:object / required:[] / additionalProperties:false ([docs/p1-plan](../../../docs/p1-plan.md) §4.2).
pub fn normalize_schema(obj: &serde_json::Map<String, Value>) -> String {
    let mut v = Value::Object(obj.clone());
    if v.get("type").is_none() {
        v["type"] = json!("object");
    }
    if v.get("required").is_none() {
        v["required"] = json!([]);
    }
    v["additionalProperties"] = json!(false);
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_name_roundtrip() {
        let f = server_function_name("fs", "read_file");
        assert_eq!(f, "mcp__fs__read_file");
        assert_eq!(parse_function(&f), Some(("fs".into(), "read_file".into())));
        assert!(parse_function("read_file").is_none());
        assert!(parse_function("mcp__fs").is_none());
        assert!(parse_function("mcp____x").is_none());
    }

    /// 回归：server 名里的 `.` / 空格 / 中文必须归一化，否则 provider 以
    /// `Invalid 'tools[N].name' ... ^[a-zA-Z0-9_-]+$` 拒绝整条请求。
    #[test]
    fn function_name_sanitizes_illegal_chars() {
        assert_eq!(
            server_function_name("PowerShell.MCP", "read_file"),
            "mcp__PowerShell_MCP__read_file"
        );
        let f = server_function_name("我的 server", "get");
        assert!(
            f.strip_prefix("mcp__")
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn build_tool_defs_registers_and_dedups() {
        let mk = |server: &str, name: &str| McpTool {
            server: server.into(),
            name: name.into(),
            description: format!("[{server}] {name}"),
            schema_json: "{}".into(),
        };
        // 归一化后两条撞名（`.` → `_`），后者追加 `__2` 去重
        let tools = vec![mk("PowerShell.MCP", "echo"), mk("PowerShell_MCP", "echo")];
        let (defs, index) = build_tool_defs(&tools);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].function_name, "mcp__PowerShell_MCP__echo");
        assert_eq!(defs[1].function_name, "mcp__PowerShell_MCP__echo__2");
        assert_eq!(
            index.get("mcp__PowerShell_MCP__echo"),
            Some(&("PowerShell.MCP".to_string(), "echo".to_string()))
        );
        assert_eq!(
            index.get("mcp__PowerShell_MCP__echo__2"),
            Some(&("PowerShell_MCP".to_string(), "echo".to_string()))
        );
    }

    #[test]
    fn schema_normalization() {
        let mut obj = serde_json::Map::new();
        obj.insert("properties".into(), json!({"a": {"type": "string"}}));
        let s = normalize_schema(&obj);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["type"], "object");
        assert_eq!(v["additionalProperties"], false);
        assert_eq!(v["required"], serde_json::json!([]));
    }
}

/// 按 server 的工具过滤策略过滤工具列表，返回（保留集，被过滤数）。
///
/// 被过滤数要透出到状态表（「6（已过滤 2）」），否则用户看不到自己的过滤规则生效了。
pub(crate) fn filter_tools(
    filter: &super::config::ToolFilter,
    tools: &[McpTool],
) -> (Vec<McpTool>, usize) {
    let kept: Vec<McpTool> = tools
        .iter()
        .filter(|t| filter.allows(&t.name))
        .cloned()
        .collect();
    let filtered = tools.len() - kept.len();
    (kept, filtered)
}

/// 把 MCP 工具定义转成 provider 侧工具定义（字段 1:1）。
///
/// 放在这里而不是接线层：`provider::ToolDef` 与 [`McpToolDef`] 字段完全对应，
/// 让 `core/agent/stream.rs` 的注入只多一行。
pub fn to_provider_tool_defs(defs: &[McpToolDef]) -> Vec<crate::provider::ToolDef> {
    defs.iter()
        .map(|d| crate::provider::ToolDef {
            name: d.function_name.clone(),
            description: d.description.clone(),
            schema_json: d.schema_json.clone(),
        })
        .collect()
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::mcp::config::{ToolFilter, ToolFilterMode};

    fn tool(name: &str) -> McpTool {
        McpTool {
            server: "s".into(),
            name: name.into(),
            description: format!("[s] {name}"),
            schema_json: "{}".into(),
        }
    }

    #[test]
    fn filter_all_keeps_everything() {
        let tools = vec![tool("a"), tool("b")];
        let (kept, filtered) = filter_tools(&ToolFilter::default(), &tools);
        assert_eq!(kept.len(), 2);
        assert_eq!(filtered, 0);
    }

    #[test]
    fn filter_allow_list_keeps_only_matches_and_counts() {
        let tools = vec![tool("read_file"), tool("write_file"), tool("read_dir")];
        let f = ToolFilter {
            mode: ToolFilterMode::Allow,
            list: vec!["read_*".into()],
        };
        let (kept, filtered) = filter_tools(&f, &tools);
        let names: Vec<&str> = kept.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["read_file", "read_dir"]);
        assert_eq!(filtered, 1);
    }

    #[test]
    fn filter_deny_list_drops_matches() {
        let tools = vec![tool("read_file"), tool("write_file")];
        let f = ToolFilter {
            mode: ToolFilterMode::Deny,
            list: vec!["write_*".into()],
        };
        let (kept, filtered) = filter_tools(&f, &tools);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "read_file");
        assert_eq!(filtered, 1);
    }

    #[test]
    fn provider_defs_map_one_to_one() {
        let defs = vec![McpToolDef {
            function_name: "mcp__s__a".into(),
            description: "desc".into(),
            schema_json: "{\"type\":\"object\"}".into(),
        }];
        let out = to_provider_tool_defs(&defs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "mcp__s__a");
        assert_eq!(out[0].description, "desc");
    }
}
