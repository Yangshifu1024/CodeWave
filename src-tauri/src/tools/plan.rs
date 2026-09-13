//! plan 工具：todo 状态机（同一时刻至多一个 in_progress）；plan:update 事件 + 会话持久化。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// todo 状态：待办 / 进行中 / 已完成。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// 待办。
    Pending,
    /// 进行中（同一时刻至多一项）。
    InProgress,
    /// 已完成。
    Completed,
}

/// 单条计划项：标题 + 状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    /// 标题（非空）。
    pub title: String,
    /// 当前状态。
    pub status: TodoStatus,
}

/// plan 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 省略 = 读取当前计划；提供 = 全量替换。
    #[serde(default)]
    todos: Option<Vec<TodoIn>>,
}

/// 单条 todo 入参（wire 形态：状态为字符串）。
#[derive(Deserialize)]
pub struct TodoIn {
    /// 标题。
    title: String,
    /// `pending | in_progress | completed`。
    status: String,
}

/// plan 工具：读写当前会话的计划（todo 列表）。
/// 入参为可选 todos 数组，省略即读取、提供即全量替换；Meta 分级（操作 agent 自身状态）。
/// 批准基线联动：批准后替换计划时按标题集合 diff，新增标题视为计划外步骤并置范围扩张标记，
/// 供批次层在执行前要求用户确认。
pub struct PlanTool;

/// 状态机校验：≤100 项、标题非空、同一时刻至多一个 in_progress。
pub fn validate_todos(todos: &[Todo]) -> Result<(), String> {
    if todos.len() > 100 {
        return Err("todo 列表超过 100 项".into());
    }
    let mut in_progress = 0;
    for t in todos {
        if t.title.trim().is_empty() {
            return Err("todo title 不能为空".into());
        }
        if t.status == TodoStatus::InProgress {
            in_progress += 1;
            if in_progress > 1 {
                return Err("同一时刻最多一个 in_progress 任务".into());
            }
        }
    }
    Ok(())
}

/// 渲染为模型可读文本（tool_result 内容）。
pub fn render_todos(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "（计划为空）".into();
    }
    todos
        .iter()
        .map(|t| {
            let mark = match t.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Completed => "[x]",
            };
            format!("{mark} {}", t.title)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait::async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &'static str {
        "plan"
    }
    fn description(&self) -> &'static str {
        "读取或替换当前计划（todo 列表）。传入 {\"todos\":[{\"title\":..., \"status\":\"pending|in_progress|completed\"}]} 即全量更新；传 {} 即读取。同一时刻至多一个任务可处于 in_progress。任何实现类请求（无论大小）都应在开始时调用本工具建立跟踪 todo 列表，并随工作推进保持状态最新；当前计划会自动注入每轮新的用户消息。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "todos": {
      "type": "array",
      "maxItems": 100,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "status"],
        "properties": {
          "title": {"type": "string", "minLength": 1},
          "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
        }
      }
    }
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
        match args.todos {
            None => {
                let todos = ctx.rt.todos.lock().unwrap().clone();
                ToolOutcome::ok(json!({ "todos": todos, "rendered": render_todos(&todos) }))
            }
            Some(ins) => {
                let mut todos = Vec::with_capacity(ins.len());
                for t in ins {
                    let status = match t.status.as_str() {
                        "pending" => TodoStatus::Pending,
                        "in_progress" => TodoStatus::InProgress,
                        "completed" => TodoStatus::Completed,
                        other => return ToolOutcome::err("E_ARGS", format!("非法状态：{other}")),
                    };
                    todos.push(Todo {
                        title: t.title.trim().to_string(),
                        status,
                    });
                }
                if let Err(e) = validate_todos(&todos) {
                    return ToolOutcome::err("E_PLAN_INVALID", e);
                }
                // G3（[docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md) §7）：存在批准后基线时，全量替换按标题集合 diff；新增标题 = 计划外步骤。
                // 纯函数判定在 diff_new_todos（含测试）；重命名按「删 + 增」处理，只对新增告警。
                {
                    let baseline = ctx.rt.approved_plan.lock().unwrap().clone();
                    let current: Vec<String> = todos.iter().map(|t| t.title.clone()).collect();
                    if let Some(baseline) = baseline {
                        if !diff_new_todos(&baseline, &current).is_empty()
                            && !ctx
                                .rt
                                .scope_allowed
                                .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            ctx.rt
                                .scope_expanded
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
                *ctx.rt.todos.lock().unwrap() = todos.clone();
                // 事件 + 持久化（payload 带 session：RightBar 与任务面板按会话路由，与其他事件一致）
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "plan:update",
                    json!({ "session": ctx.rt.id, "todos": todos }),
                );
                let _ = ctx.core.store.save_todos(&ctx.rt.id, &todos);
                ToolOutcome::ok(json!({ "todos": todos, "rendered": render_todos(&todos) }))
            }
        }
    }
}

/// G3：比较基线与新增 todo 标题，返回新增标题（不在基线中，保持原序，去重）。
/// plan.rs 的告警与 batch.rs 的确认弹窗复用；纯函数便于测试。
pub fn diff_new_todos(baseline: &[String], current: &[String]) -> Vec<String> {
    current
        .iter()
        .filter(|c| !baseline.iter().any(|b| b == *c))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_rules() {
        let ok = vec![
            Todo {
                title: "a".into(),
                status: TodoStatus::Completed,
            },
            Todo {
                title: "b".into(),
                status: TodoStatus::InProgress,
            },
            Todo {
                title: "c".into(),
                status: TodoStatus::Pending,
            },
        ];
        assert!(validate_todos(&ok).is_ok());

        let two_ip = vec![
            Todo {
                title: "a".into(),
                status: TodoStatus::InProgress,
            },
            Todo {
                title: "b".into(),
                status: TodoStatus::InProgress,
            },
        ];
        assert_eq!(
            validate_todos(&two_ip).unwrap_err(),
            "同一时刻最多一个 in_progress 任务"
        );

        let empty_title = vec![Todo {
            title: "  ".into(),
            status: TodoStatus::Pending,
        }];
        assert!(validate_todos(&empty_title).is_err());
    }

    #[test]
    fn rendered_marks() {
        let todos = vec![
            Todo {
                title: "done".into(),
                status: TodoStatus::Completed,
            },
            Todo {
                title: "now".into(),
                status: TodoStatus::InProgress,
            },
            Todo {
                title: "later".into(),
                status: TodoStatus::Pending,
            },
        ];
        let r = render_todos(&todos);
        assert!(r.contains("[x] done"));
        assert!(r.contains("[~] now"));
        assert!(r.contains("[ ] later"));
    }

    #[test]
    fn diff_new_todos_finds_additions_only() {
        // G3：检测新增；保留/完成/删除的标题不告警；重命名计为删+增
        let baseline = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            diff_new_todos(&baseline, &["a".into(), "b".into()]),
            Vec::<String>::new()
        );
        assert_eq!(
            diff_new_todos(&baseline, &["a".into()]),
            Vec::<String>::new()
        ); // 删除不告警
        assert_eq!(
            diff_new_todos(&baseline, &["a".into(), "b".into(), "c".into()]),
            vec!["c".to_string()]
        );
        assert_eq!(
            diff_new_todos(&baseline, &["a".into(), "x".into()]),
            vec!["x".to_string()]
        ); // 重命名 = 删 + 增
        assert_eq!(diff_new_todos(&[], &["a".into()]), vec!["a".to_string()]); // 空基线：一切都是新增
    }
}
