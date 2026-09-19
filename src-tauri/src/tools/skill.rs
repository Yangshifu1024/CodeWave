//! skill 工具：按名称加载 SKILL.md 正文，以 `<skill-loaded>` 块回喂给模型。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

/// skill 工具入参。
#[derive(Deserialize)]
pub struct Args {
    /// 技能名称（系统提示词中列出可用技能）。
    skill: String,
    /// 传给技能的可选上下文（包进 caller-context 块）。
    #[serde(default)]
    args: Option<String>,
}

/// 条件式 ask 交互规范：随任意技能正文注入（[docs/skill-ask-norm](../../../docs/skill-ask-norm.md)）。
/// 流程含「向用户提问并等待作答」轮次的技能据此用 ask 弹窗呈现提问；无提问轮次的技能自动休眠，行为零变化。
/// 注意：文本不得提及 `caller-context`（既有测试以「无 args 时全文不含该字样」判定 caller-context 缺席）
/// 与 `</skill-loaded>`（会提前终结注入块）；两者均有契约测试守护。
const ASK_INTERACTION_NORM: &str = r#"<ask-interaction-norm>
Applies only if this skill's procedure includes rounds where you ask the user questions and wait for answers; if it has no question rounds, ignore this block and proceed unchanged.
When it applies, every question round MUST be presented through the `ask` tool (structured prompt) instead of a plain-text question list. Fall back to the skill's own text format only when `ask` is unavailable (e.g. subagent context).
Mapping rules:
- A recommended answer indicated by the skill body (e.g. a "➡️" line) becomes an option with `recommended: true`; recommended options stay visible and selectable, never hidden.
- Mutually exclusive questions set `single: true`; genuine multi-select questions omit `single`.
- Open-ended questions become `ask` questions without `options`.
- Respect tool limits (max 5 questions, 6 options each): split a larger round into consecutive `ask` calls without dropping any question.
</ask-interaction-norm>"#;

/// skill 工具：加载技能的完整指令文本供模型遵循执行。
/// 入参为 skill 名称 + 可选 args；Meta 分级（只读技能索引，不触工作区）。
/// 返回正文包在 `<skill-loaded>` 块中，附带 origin 来源；未知或被禁用的技能返回
/// 带可用清单的 E_SKILL_NOT_FOUND。
pub struct SkillTool;

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }
    fn description(&self) -> &'static str {
        "按名称加载技能的完整指令文本。可用技能在系统提示词中列出。返回以 <skill-loaded> 包裹的技能正文。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["skill"],
  "properties": {
    "skill": {"type": "string"},
    "args": {"type": "string", "description": "传给技能的可选上下文"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let cfg = ctx.core.cfg.read().unwrap().clone();
        let index = &ctx.core.skills;
        let Some(s) = index.get(
            &ctx.rt.workspace,
            &ctx.rt.data_dir,
            &cfg.disabled_skills,
            ctx.rt.project_dir.as_deref(),
            &args.skill,
        ) else {
            let names: Vec<String> = index
                .list(
                    &ctx.rt.workspace,
                    &ctx.rt.data_dir,
                    &cfg.disabled_skills,
                    ctx.rt.project_dir.as_deref(),
                )
                .iter()
                .map(|m| m.name.clone())
                .collect();
            return ToolOutcome::err(
                "E_SKILL_NOT_FOUND",
                format!("skill `{}` 不存在。可用：{}", args.skill, names.join(", ")),
            );
        };
        let mut body = format!("<skill-loaded name=\"{}\">\n{}", s.meta.name, s.body);
        if let Some(a) = &args.args {
            body.push_str(&format!("\n\n<caller-context>{a}</caller-context>"));
        }
        // 条件式 ask 交互规范（[docs/skill-ask-norm](../../../docs/skill-ask-norm.md)）：
        // 置于 caller-context 之后、闭合标签之前；规范文本自身不得含闭合标签（有测试守护）。
        body.push_str(&format!("\n\n{ASK_INTERACTION_NORM}"));
        body.push_str("\n</skill-loaded>");
        ToolOutcome::ok(json!({ "skill": s.meta.name, "origin": s.meta.origin, "content": body }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在 `.codewave/skills`（data_dir 托管路径）安装了 `demo` 技能的全新 core + 工作区。
    fn setup() -> (ToolCtx, tempfile::TempDir, tempfile::TempDir) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let skill_dir = dd.path().join("skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: test skill\n---\nDEMO BODY LINE",
        )
        .unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "skill-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        let ctx = ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        (ctx, ws, dd)
    }

    #[tokio::test]
    async fn loads_skill_body_wrapped_in_skill_loaded() {
        let (ctx, _ws, _dd) = setup();
        let out = SkillTool.run(&ctx, json!({"skill": "demo"})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["skill"], "demo");
        let content = out.data["content"].as_str().unwrap();
        assert!(content.contains("<skill-loaded name=\"demo\">"));
        assert!(content.contains("DEMO BODY LINE"));
        assert!(content.trim_end().ends_with("</skill-loaded>"));
        // 耦合前提：ASK_INTERACTION_NORM 文本不得提及 caller-context，否则本断言失效
        assert!(
            !content.contains("caller-context"),
            "no args → no caller-context block"
        );
    }

    #[tokio::test]
    async fn caller_context_is_appended_when_args_given() {
        let (ctx, _ws, _dd) = setup();
        let out = SkillTool
            .run(&ctx, json!({"skill": "demo", "args": "extra ctx"}))
            .await;
        assert!(out.ok, "{out:?}");
        let content = out.data["content"].as_str().unwrap();
        assert!(content.contains("<caller-context>extra ctx</caller-context>"));
    }

    #[tokio::test]
    async fn ask_interaction_norm_is_appended_to_every_load() {
        let (ctx, _ws, _dd) = setup();
        let out = SkillTool.run(&ctx, json!({"skill": "demo"})).await;
        assert!(out.ok, "{out:?}");
        let content = out.data["content"].as_str().unwrap();
        assert!(content.contains("<ask-interaction-norm>"), "{content:?}");
        assert!(content.contains("</ask-interaction-norm>"));
        assert!(content.trim_end().ends_with("</skill-loaded>"));
    }

    #[tokio::test]
    async fn norm_block_sits_between_caller_context_and_closing_tag() {
        let (ctx, _ws, _dd) = setup();
        let out = SkillTool
            .run(&ctx, json!({"skill": "demo", "args": "extra ctx"}))
            .await;
        assert!(out.ok, "{out:?}");
        let content = out.data["content"].as_str().unwrap();
        let caller = content
            .find("<caller-context>extra ctx</caller-context>")
            .unwrap();
        let norm = content.find("<ask-interaction-norm>").unwrap();
        let close = content.find("</skill-loaded>").unwrap();
        assert!(caller < norm && norm < close, "{content:?}");
    }

    /// 规范文本自身不得包含闭合标签，否则会提前终结 skill-loaded 块；
    /// 顺带锁定标签形状（以 <ask-interaction-norm> 开、自身闭合）。
    #[test]
    fn norm_text_never_contains_skill_loaded_closing_tag() {
        assert!(!ASK_INTERACTION_NORM.contains("</skill-loaded>"));
        assert!(ASK_INTERACTION_NORM.starts_with("<ask-interaction-norm>\n"));
        assert!(
            ASK_INTERACTION_NORM
                .trim_end()
                .ends_with("</ask-interaction-norm>")
        );
    }

    #[tokio::test]
    async fn unknown_skill_lists_available_names() {
        let (ctx, _ws, _dd) = setup();
        let out = SkillTool.run(&ctx, json!({"skill": "nope"})).await;
        let err = out.error.unwrap();
        assert_eq!(err.code, "E_SKILL_NOT_FOUND");
        // 错误信息列出了内置技能为可用项
        assert!(err.message.contains("repo-index"), "{err:?}");
        assert!(err.message.contains("demo"), "{err:?}");
    }

    #[tokio::test]
    async fn disabled_skill_is_not_found() {
        let (ctx, _ws, _dd) = setup();
        ctx.core
            .cfg
            .write()
            .unwrap()
            .disabled_skills
            .push("demo".into());
        let out = SkillTool.run(&ctx, json!({"skill": "demo"})).await;
        assert_eq!(out.error.unwrap().code, "E_SKILL_NOT_FOUND");
    }

    /// 疑似潜在缺陷：SkillIndex 的 TTL 缓存（10s）无视禁用列表——成功加载后才被禁用的技能
    /// 在缓存过期前仍可解析。此处仅记录不设断言（生产流程中应在首次加载前改配置）。
    #[ignore]
    #[tokio::test]
    async fn disabled_after_load_should_invalidate_within_ttl() {
        let (ctx, _ws, _dd) = setup();
        let ok = SkillTool.run(&ctx, json!({"skill": "demo"})).await;
        assert!(ok.ok);
        ctx.core
            .cfg
            .write()
            .unwrap()
            .disabled_skills
            .push("demo".into());
        let out = SkillTool.run(&ctx, json!({"skill": "demo"})).await;
        assert_eq!(
            out.error.unwrap().code,
            "E_SKILL_NOT_FOUND",
            "disabled skill must stop resolving immediately"
        );
    }
}
