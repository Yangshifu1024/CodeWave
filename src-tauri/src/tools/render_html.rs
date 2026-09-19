//! render_html 工具：最多 5 万字符的沙箱小组件（由前端在沙箱 iframe 中渲染）。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

/// HTML 字符数上限。
const MAX_HTML: usize = 50_000;

/// render_html 工具入参。
#[derive(Deserialize)]
pub struct Args {
    /// 小组件标题（缺省 "Widget"）。
    #[serde(default)]
    title: Option<String>,
    /// 自包含的 HTML 片段（1–50000 字符）。
    html: String,
}

/// render_html 工具：把小型自包含 HTML 渲染为面向用户的交互式小组件。
/// 入参为 html（必填）+ 可选 title；Meta 分级（不触工作区与网络）。
/// 前端在无网络、无同源权限的沙箱 iframe 中展示，适合快速可视化 / 原型演示。
pub struct RenderHtmlTool;

#[async_trait::async_trait]
impl Tool for RenderHtmlTool {
    fn name(&self) -> &'static str {
        "render_html"
    }
    fn description(&self) -> &'static str {
        "把小型自包含 HTML 片段（最多 5 万字符）渲染为交互式小组件，在沙箱 iframe（无网络、无同源权限）中展示给用户。适用于快速可视化/原型演示。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["html"],
  "properties": {
    "title": {"type": "string", "description": "小组件标题"},
    "html": {"type": "string"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    async fn run(&self, _ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let n = args.html.chars().count();
        if n == 0 {
            return ToolOutcome::err("E_ARGS", "html 为空");
        }
        if n > MAX_HTML {
            return ToolOutcome::err("E_TOO_LARGE", format!("html {n} 字符超过 {MAX_HTML} 上限"));
        }
        ToolOutcome::ok(json!({
            "title": args.title.unwrap_or_else(|| "Widget".into()),
            "html": args.html,
            "chars": n,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolCtx {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "render-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn echoes_html_with_title_and_char_count() {
        let ctx = ctx();
        let out = RenderHtmlTool
            .run(&ctx, json!({"title": "Demo", "html": "<b>hi</b>"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["title"], "Demo");
        assert_eq!(out.data["html"], "<b>hi</b>");
        assert_eq!(out.data["chars"], 9);
    }

    #[tokio::test]
    async fn title_defaults_to_widget() {
        let ctx = ctx();
        let out = RenderHtmlTool.run(&ctx, json!({"html": "x"})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["title"], "Widget");
    }

    #[tokio::test]
    async fn char_count_is_unicode_aware() {
        let ctx = ctx();
        // 6 个 unicode 字符（增补平面 emoji 也按单字符计），而非字节
        let out = RenderHtmlTool.run(&ctx, json!({"html": "héllo🚀"})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["chars"], 6);
    }

    #[tokio::test]
    async fn empty_html_rejected() {
        let ctx = ctx();
        let out = RenderHtmlTool.run(&ctx, json!({"html": ""})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }

    #[tokio::test]
    async fn over_limit_rejected_at_exact_boundary() {
        let ctx = ctx();
        // 恰好 50_000 字符可通过
        let out = RenderHtmlTool
            .run(&ctx, json!({"html": "a".repeat(50_000)}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["chars"], 50_000);
        // 多一个字符 → E_TOO_LARGE
        let out = RenderHtmlTool
            .run(&ctx, json!({"html": "a".repeat(50_001)}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_TOO_LARGE");
    }

    #[tokio::test]
    async fn missing_html_field_maps_to_e_args() {
        let ctx = ctx();
        let out = RenderHtmlTool.run(&ctx, json!({"title": "no html"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }
}
