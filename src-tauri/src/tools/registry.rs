//! 工具注册表：单一分发入口背后的数据面；schema 顺序固定（缓存优先）。

use super::*;
use std::collections::HashMap;
use std::sync::Arc;

/// 内置工具注册表：name → 工具实例的静态映射。所有内置工具在 `default_tools` 一次性注册；
/// `tool_defs` 供 provider 组装 tools 列表，`schemas_token_estimate` 供上下文预算统计 schema token 开销。
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// 注册全部内置工具（P0 基础 8 个 + P1-A 扩展若干）。新增工具必须在此登记，否则模型不可见。
    pub fn default_tools() -> Self {
        let mut tools: HashMap<&'static str, Arc<dyn Tool>> = HashMap::new();
        macro_rules! reg {
            ($t:expr_2021) => {
                let t: Arc<dyn Tool> = Arc::new($t);
                tools.insert(t.name(), t);
            };
        }
        reg!(crate::tools::read::ReadTool);
        reg!(crate::tools::edit::EditTool);
        reg!(crate::tools::create::CreateTool);
        reg!(crate::tools::delete::DeleteTool);
        reg!(crate::tools::list_files::ListFilesTool);
        reg!(crate::tools::command::CommandTool);
        reg!(crate::tools::grep::GrepTool);
        reg!(crate::tools::ask::AskTool);
        // P1-A 批次新增的工具
        reg!(crate::tools::web_fetch::WebFetchTool);
        reg!(crate::tools::http_request::HttpRequestTool);
        reg!(crate::tools::service::ServiceTool);
        reg!(crate::tools::wait::WaitTool);
        reg!(crate::tools::suggest::SuggestTool);
        reg!(crate::tools::plan::PlanTool);
        reg!(crate::tools::batch_read::BatchReadTool);
        reg!(crate::tools::calculate::CalculateTool);
        reg!(crate::tools::render_html::RenderHtmlTool);
        reg!(crate::tools::skill::SkillTool);
        reg!(crate::tools::subagent::SubagentTool);
        reg!(crate::tools::scheduled_task::ScheduledTaskTool);
        ToolRegistry { tools }
    }

    /// 仅测试用：空注册表。
    #[cfg(test)]
    pub fn empty() -> Self {
        ToolRegistry {
            tools: HashMap::new(),
        }
    }

    /// 按名取工具，返回 Arc 克隆（调用方持有独立引用计数）。
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 组装发给 provider 的工具定义列表；按名字排序以保证请求稳定（利于 provider 侧 prompt 缓存命中）。
    pub fn tool_defs(&self) -> Vec<crate::provider::ToolDef> {
        let mut defs: Vec<crate::provider::ToolDef> = self
            .tools
            .values()
            .map(|t| crate::provider::ToolDef {
                name: t.name().to_string(),
                description: t.description().to_string(),
                schema_json: t.schema().to_string(),
            })
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// 估算全部工具 schema + description 占用的 token 总量，供自动压缩阈值判断参考。
    pub fn schemas_token_estimate(&self) -> u64 {
        self.tool_defs()
            .iter()
            .map(|d| {
                crate::util::token_est::est_tokens_text(&d.schema_json)
                    + crate::util::token_est::est_tokens_text(&d.description)
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tools_sorted_and_strict() {
        let reg = ToolRegistry::default_tools();
        let defs = reg.tool_defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "ask",
                "batch_read",
                "calculate",
                "command",
                "create",
                "delete",
                "edit",
                "grep",
                "http_request",
                "list_files",
                "plan",
                "read",
                "render_html",
                "scheduled_task",
                "service",
                "skill",
                "subagent",
                "suggest",
                "wait",
                "web_fetch"
            ]
        );
        for d in &defs {
            let s: serde_json::Value = serde_json::from_str(&d.schema_json).unwrap();
            assert_eq!(
                s["additionalProperties"],
                serde_json::json!(false),
                "{} schema 必须严格",
                d.name
            );
            assert_eq!(s["type"], "object");
        }
    }

    #[test]
    fn get_resolves_registered_tools_and_none_for_unknown() {
        let reg = ToolRegistry::default_tools();
        let read = reg.get("read").expect("read must be registered");
        assert_eq!(read.name(), "read");
        assert!(
            reg.get("read").is_some(),
            "repeated get returns a fresh Arc clone"
        );
        assert!(reg.get("no_such_tool").is_none());
    }

    #[test]
    fn schema_token_estimate_positive_and_stable() {
        let reg = ToolRegistry::default_tools();
        let a = reg.schemas_token_estimate();
        let b = reg.schemas_token_estimate();
        assert!(a > 0, "20 built-in tools yield a positive estimate");
        assert_eq!(a, b, "estimate must be deterministic");
        assert_eq!(ToolRegistry::empty().schemas_token_estimate(), 0);
    }

    #[test]
    fn empty_registry_has_no_defs() {
        let reg = ToolRegistry::empty();
        assert!(reg.tool_defs().is_empty());
        assert!(reg.get("read").is_none());
    }
}
