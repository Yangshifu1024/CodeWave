//! scheduled_task 工具（[docs/p2-plan](../../../docs/p2-plan.md) §3.1）：create / list / delete 三种操作。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

/// scheduled_task 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 操作类型：create / list / delete。
    action: String,
    /// create：任务名称。
    #[serde(default)]
    name: Option<String>,
    /// create：每次运行时要执行的指令。
    #[serde(default)]
    instruction: Option<String>,
    /// create：计划表达式（`cron:<5 字段>` / `every:<n> <m|h|d>` / `once:<RFC3339>`）。
    #[serde(default)]
    schedule: Option<String>,
    /// delete：目标任务 id。
    #[serde(default)]
    id: Option<String>,
}

/// scheduled_task 工具：管理「稍后在隔离上下文中运行 agent」的定时任务。
/// 入参为 action 三选一（create 需 name/instruction/schedule，delete 需 id）；
/// Meta 分级（管理调度器状态而非工作区）。任务按调度器语法校验并计算首次 next_run；
/// 挂项目的任务持久化可跨重启恢复，自由会话任务为进程本地。
pub struct ScheduledTaskTool;

#[async_trait::async_trait]
impl Tool for ScheduledTaskTool {
    fn name(&self) -> &'static str {
        "scheduled_task"
    }
    fn description(&self) -> &'static str {
        "管理「稍后在隔离上下文中运行你（agent）」的定时任务。action=create {name, instruction, schedule}；action=list；action=delete {id}。计划语法：'cron:<5 字段>'（如 cron:0 9 * * *）、'every:<n> <m|h|d>'（如 every:30 m）或 'once:<RFC3339>'。任务为进程本地（应用重启后清除）且串行运行。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["action"],
  "properties": {
    "action": {"type": "string", "enum": ["create", "list", "delete"]},
    "name": {"type": "string"},
    "instruction": {"type": "string", "description": "每次运行时要做什么"},
    "schedule": {"type": "string", "description": "cron:0 9 * * * | every:30 m | once:RFC3339"},
    "id": {"type": "string"}
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
        match args.action.as_str() {
            "create" => {
                let Some(name) = args.name.filter(|s| !s.trim().is_empty()) else {
                    return ToolOutcome::err("E_ARGS", "create 需要 name");
                };
                let Some(instruction) = args.instruction.filter(|s| !s.trim().is_empty()) else {
                    return ToolOutcome::err("E_ARGS", "create 需要 instruction");
                };
                let Some(schedule) = args.schedule.filter(|s| !s.trim().is_empty()) else {
                    return ToolOutcome::err(
                        "E_ARGS",
                        "create 需要 schedule（cron:/every:/once:）",
                    );
                };
                // 语法校验 + 初始 next_run
                let next = match crate::core::scheduler::initial_next(&schedule) {
                    Ok(n) => n,
                    Err(e) => return ToolOutcome::err("E_SCHEDULE", e),
                };
                let Some(next) = next else {
                    return ToolOutcome::err("E_SCHEDULE", "once 时间已过去");
                };
                if crate::core::scheduler::parse_schedule(&schedule).is_err() {
                    return ToolOutcome::err("E_SCHEDULE", "计划语法无效");
                }
                let task = crate::core::scheduler::ScheduledTask {
                    id: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
                    name,
                    instruction,
                    schedule,
                    next_run: Some(next),
                    last_status: None,
                    last_summary: None,
                    project_id: ctx.rt.project_id.clone(),
                    enabled: true,
                    runs: Vec::new(),
                };
                ctx.core.tasks.upsert(task.clone()).await;
                ToolOutcome::ok(json!({ "created": task }))
            }
            "list" => {
                let list = ctx.core.tasks.list().await;
                ToolOutcome::ok(json!({ "tasks": list, "count": list.len(),
                    "note": "挂项目的任务已持久化（重启恢复）；自由会话任务仍为进程本地" }))
            }
            "delete" => {
                let Some(id) = args.id else {
                    return ToolOutcome::err("E_ARGS", "delete 需要 id");
                };
                match ctx.core.tasks.remove(&id).await {
                    Some(_) => ToolOutcome::ok(json!({ "deleted": id })),
                    None => ToolOutcome::err("E_NOT_FOUND", format!("任务不存在：{id}")),
                }
            }
            other => ToolOutcome::err("E_ARGS", format!("未知 action：{other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scheduler::{initial_next, parse_schedule};

    #[test]
    fn schedule_grammar() {
        assert!(parse_schedule("cron:0 9 * * *").is_ok());
        assert!(parse_schedule("cron:*/5 * * * *").is_ok());
        assert!(parse_schedule("cron:bad").is_err());
        assert!(parse_schedule("every:30 m").is_ok());
        assert!(parse_schedule("every:2 h").is_ok());
        assert!(parse_schedule("every:1 d").is_ok());
        assert!(parse_schedule("every:x m").is_err());
        assert!(parse_schedule("once:2030-01-01T09:00:00+08:00").is_ok());
        assert!(parse_schedule("once:not-a-time").is_err());
        assert!(parse_schedule("weekly").is_err());
    }

    #[test]
    fn schedule_grammar_boundaries() {
        // every：零 / 负数 / 超上限的间隔拒绝；大但在上限内接受（30 天上限，[docs/arithmetic-audit](../../../docs/arithmetic-audit.md)#4）
        assert!(parse_schedule("every:0 m").is_err());
        assert!(parse_schedule("every:-1 m").is_err());
        assert!(parse_schedule("every:30 d").is_ok());
        assert!(parse_schedule("every:100000 d").is_err());
        // 缺单位 / 未知单位 / 无空白分隔
        assert!(parse_schedule("every:30").is_err());
        assert!(parse_schedule("every:30 x").is_err());
        assert!(parse_schedule("every:30m").is_err());
        // 空前缀
        assert!(parse_schedule("cron:").is_err());
        assert!(parse_schedule("once:").is_err());
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("   ").is_err());
    }

    #[test]
    fn initial_next_boundaries() {
        // 未来 once → Some；过去 once → None（工具层映射为 E_SCHEDULE）
        let future = initial_next("once:2030-01-01T00:00:00Z").unwrap();
        assert!(future.is_some());
        let past = initial_next("once:2020-01-01T00:00:00Z").unwrap();
        assert!(past.is_none());
        // 周期计划总有下一次运行
        assert!(initial_next("every:30 m").unwrap().is_some());
        assert!(initial_next("cron:0 9 * * *").unwrap().is_some());
        // 坏语法以 Err 透出
        assert!(initial_next("nonsense").is_err());
    }

    /// 走共享任务表的工具级 create/list/delete 流程。
    #[tokio::test]
    async fn tool_create_list_delete_flow() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "sched-test",
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
        let tool = ScheduledTaskTool;

        // create 需要 name / instruction / schedule
        let out = tool.run(&ctx, json!({"action": "create"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        let out = tool
            .run(&ctx, json!({"action": "create", "name": "  ", "instruction": "x", "schedule": "every:30 m"}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        let out = tool
            .run(&ctx, json!({"action": "create", "name": "n"}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");

        // 坏语法 → E_SCHEDULE
        let out = tool
            .run(
                &ctx,
                json!({"action": "create", "name": "n", "instruction": "i", "schedule": "weekly"}),
            )
            .await;
        assert_eq!(out.error.unwrap().code, "E_SCHEDULE");
        // 过去的 once 时间 → E_SCHEDULE
        let out = tool
            .run(&ctx, json!({"action": "create", "name": "n", "instruction": "i", "schedule": "once:2020-01-01T00:00:00Z"}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_SCHEDULE");

        // 正常路径：创建成功且 next_run 在未来
        let out = tool
            .run(&ctx, json!({"action": "create", "name": "nightly", "instruction": "run checks", "schedule": "every:1 d"}))
            .await;
        assert!(out.ok, "{out:?}");
        let created = out.data["created"].clone();
        assert_eq!(created["name"], "nightly");
        assert_eq!(created["schedule"], "every:1 d");
        assert!(created["next_run"].is_string());
        let id = created["id"].as_str().unwrap().to_string();

        // list 能看到任务
        let out = tool.run(&ctx, json!({"action": "list"})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["count"], 1);
        assert_eq!(out.data["tasks"][0]["id"], id.as_str());

        // 按 id 删除；未知 id → E_NOT_FOUND
        let out = tool.run(&ctx, json!({"action": "delete", "id": id})).await;
        assert!(out.ok, "{out:?}");
        let out = tool
            .run(&ctx, json!({"action": "delete", "id": "ghost"}))
            .await;
        assert_eq!(out.error.unwrap().code, "E_NOT_FOUND");

        // 未知 action
        let out = tool.run(&ctx, json!({"action": "pause"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }
}
