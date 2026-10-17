//! User-mediated workflow inspection and project definition admission.
use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::workflow::{config, service};
use serde_json::{Value, json};
pub struct WorkflowTool;
#[async_trait::async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &'static str {
        "workflow"
    }
    fn description(&self) -> &'static str {
        "读取任务路由、设计理由与冻结的工作流规则；项目配置修改和分类覆盖必须向用户展示确切内容并确认。配置保存不构成执行许可。"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Interactive
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "action"
  ],
  "properties": {
    "action": {
      "enum": [
        "read",
        "save_config",
        "override_route",
        "plan_units",
        "record_design"
      ]
    },
    "taskId": {
      "type": "string"
    },
    "definition": {
      "type": "object",
      "description": "WorkflowDefinition：schema=1、id、stages[{id,title,next?,required_checks?}]、edges[{from,to,kind:advance|repair}]、roles{角色:{tools,model_id?,independent_review?}}、checks[{id,command_text,cwd?,dangerous?}]"
    },
    "design": {
      "type": "object",
      "additionalProperties": false,
      "required": [
        "required",
        "triggers",
        "reasons",
        "alternatives",
        "selected"
      ],
      "properties": {
        "required": {
          "type": "boolean"
        },
        "triggers": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "reasons": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "alternatives": {
          "type": "array",
          "items": {
            "type": "string"
          }
        },
        "selected": {
          "type": "string"
        },
        "assumptions": {
          "type": "array",
          "items": {
            "type": "string"
          }
        }
      }
    },
    "units": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "id",
          "requirement_ids",
          "write_scopes",
          "checks"
        ],
        "properties": {
          "id": {
            "type": "string"
          },
          "requirement_ids": {
            "type": "array",
            "items": {
              "type": "string"
            }
          },
          "read_scopes": {
            "type": "array",
            "items": {
              "type": "object",
              "additionalProperties": false,
              "required": [
                "root_id",
                "relative_path"
              ],
              "properties": {
                "root_id": {
                  "type": "string"
                },
                "relative_path": {
                  "type": "string"
                }
              }
            }
          },
          "write_scopes": {
            "type": "array",
            "items": {
              "type": "object",
              "additionalProperties": false,
              "required": [
                "root_id",
                "relative_path"
              ],
              "properties": {
                "root_id": {
                  "type": "string"
                },
                "relative_path": {
                  "type": "string"
                }
              }
            }
          },
          "depends_on": {
            "type": "array",
            "items": {
              "type": "string"
            }
          },
          "mutable_resources": {
            "type": "array",
            "items": {
              "type": "string"
            }
          },
          "role_id": {
            "type": [
              "string",
              "null"
            ]
          },
          "model_id": {
            "type": [
              "string",
              "null"
            ]
          },
          "checks": {
            "type": "array",
            "items": {
              "type": "string"
            }
          },
          "stage_id": {
            "type": [
              "string",
              "null"
            ]
          }
        }
      },
      "description": "嵌套字段为snake_case；checks仅本包验收检查，不复制后继review/verify阶段检查。根契约始终全量验收。"
    },
    "expectedRevision": {
      "type": "integer",
      "minimum": 0
    },
    "kind": {
      "enum": [
        "conversation",
        "mechanical",
        "single_surface",
        "serial_surfaces",
        "parallel_surfaces"
      ]
    },
    "reason": {
      "type": "string"
    }
  }
}"#
    }
    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let owner = crate::core::delivery::DeliveryService::identity_of(&ctx.rt);
        let action = args["action"].as_str().unwrap_or("");
        if action == "read" {
            let task = args["taskId"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| ctx.rt.delivery_task_id.lock().unwrap().clone());
            return match task.and_then(|t| service::read(&ctx.rt.data_dir, &owner, &t).ok()) {
                Some(p) => ToolOutcome::ok(json!(p)),
                None => {
                    ToolOutcome::err("E_WORKFLOW_MISSING", "当前任务没有可恢复的冻结工作流快照")
                }
            };
        }
        if !ctx.rt.is_main_session {
            return ToolOutcome::err("E_WORKFLOW_ACTOR", "只有根会话能请求用户调整工作流");
        }
        if action != "save_config"
            && action != "override_route"
            && action != "plan_units"
            && action != "record_design"
        {
            return ToolOutcome::err("E_ARGS", "未知 workflow action");
        }
        let preview = serde_json::to_string_pretty(&args).unwrap_or_default();
        if !ctx
            .rt
            .interactive
            .load(std::sync::atomic::Ordering::Acquire)
        {
            *ctx.rt.interaction_blocked.lock().unwrap() = Some("interaction_unavailable".into());
            ctx.cancel.cancel();
            return ToolOutcome::err(
                "E_INTERACTION_UNAVAILABLE",
                "当前宿主不能请求用户采用工作流调整",
            );
        }
        let ask_id = format!("workflow_{}", uuid::Uuid::new_v4().simple());
        let (tx, rx) = tokio::sync::oneshot::channel();
        ctx.rt.open_ask(&ask_id, tx);
        ctx.core.sink.emit(&ctx.rt.id,"ask:opened",json!({"session":ctx.rt.id,"ask_id":ask_id,"kind":"ask","run_id":owner.actor_run_id,
            "questions":[{"id":"workflow_change","question":format!("确认采用以下工作流调整？保存不授予执行权限，分类覆盖不能绕过范围冲突。\n{preview}"),"single":true,"options":[{"id":"apply_workflow","label":"采用此次调整"},{"id":"keep_workflow","label":"保留现状"}]}]}));
        let answer = tokio::select! {_=ctx.cancel.cancelled()=>None,v=rx=>v.ok()};
        ctx.rt.asks.lock().unwrap().remove(&ask_id);
        ctx.core.sink.emit(
            &ctx.rt.id,
            "ask:closed",
            json!({"session":ctx.rt.id,"ask_id":ask_id}),
        );
        let approved = answer
            .as_ref()
            .and_then(|a| a["answers"]["workflow_change"]["selections"].as_array())
            .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some("apply_workflow"));
        if !approved {
            return ToolOutcome::err("E_WORKFLOW_DECLINED", "用户未确认工作流调整");
        }
        if action == "record_design" {
            let decision = match serde_json::from_value(args["design"].clone()) {
                Ok(v) => v,
                Err(e) => return ToolOutcome::err("E_WORKFLOW_DESIGN", e.to_string()),
            };
            return match service::record_design(
                &ctx.rt.data_dir,
                &owner,
                args["taskId"].as_str().unwrap_or(""),
                args["expectedRevision"].as_u64().unwrap_or(0),
                decision,
            ) {
                Ok(()) => ToolOutcome::ok(json!({"saved":true,"execution_authorized":false})),
                Err(e) => ToolOutcome::err("E_WORKFLOW_DESIGN", e),
            };
        }
        if action == "plan_units" {
            let task = args["taskId"].as_str().unwrap_or("");
            let Some(contract) = ctx.core.delivery.read_contract(&owner.session_id, task) else {
                return ToolOutcome::err("E_WORKFLOW_CONTRACT", "任务契约不存在");
            };
            if contract.revision != args["expectedRevision"].as_u64().unwrap_or(0) {
                return ToolOutcome::err("E_WORKFLOW_REVISION", "契约修订已变更");
            }
            let units = match serde_json::from_value(args["units"].clone()) {
                Ok(v) => v,
                Err(e) => return ToolOutcome::err("E_WORKFLOW_UNITS", e.to_string()),
            };
            let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&ctx.rt);
            return match service::plan_units(&ctx.rt.data_dir, &owner, &contract, units, &roots) {
                Ok(p) => ToolOutcome::ok(json!(p)),
                Err(e) => ToolOutcome::err("E_WORKFLOW_UNITS", e),
            };
        }
        if action == "save_config" {
            if ctx.rt.project_id.is_none() {
                return ToolOutcome::err("E_WORKFLOW_PROJECT", "临时会话不能保存项目工作流");
            }
            let def: config::WorkflowDefinition =
                match serde_json::from_value(args["definition"].clone()) {
                    Ok(v) => v,
                    Err(e) => return ToolOutcome::err("E_WORKFLOW_CONFIG", e.to_string()),
                };
            let models = ctx
                .core
                .cfg
                .read()
                .unwrap()
                .providers
                .iter()
                .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
                .collect();
            let mut user = owner.clone();
            user.actor_id = "user".into();
            let path = ctx.rt.workspace.join(".codewave/workflows.json");
            let expected = args["expectedRevision"].as_u64().unwrap_or(0);
            match config::commit(&path, &def, expected, &ctx.rt.workspace, &models, &user) {
                config::SaveOutcome::Committed { revision } => {
                    let mut committed = def.clone();
                    committed.revision = revision;
                    committed.author = Some(user.actor_id.clone());
                    let bytes = serde_json::to_vec_pretty(&committed).unwrap_or_default();
                    if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
                        return ToolOutcome::err(
                            "E_WORKFLOW_SAVE",
                            "配置提交后发生外部修改，未签发信任摘要",
                        );
                    }
                    // Pin the exact user-approved bytes. External edits are not implicitly trusted.
                    let pin = crate::core::delivery::version::digest_of_bytes(&bytes);
                    if let Err(e) = crate::core::process_coordination::atomic_write(
                        &service::approval_pin(&ctx.rt.data_dir, &ctx.rt.workspace),
                        pin.as_bytes(),
                    ) {
                        return ToolOutcome::err("E_WORKFLOW_SAVE", e.to_string());
                    }
                    ToolOutcome::ok(
                        json!({"revision":revision,"approved_digest":pin,"execution_authorized":false}),
                    )
                }
                other => ToolOutcome::error_with_data(
                    "工作流配置未保存",
                    json!({"result":format!("{other:?}")}),
                ),
            }
        } else {
            let task = args["taskId"].as_str().unwrap_or("");
            let kind = match serde_json::from_value(args["kind"].clone()) {
                Ok(v) => v,
                Err(e) => return ToolOutcome::err("E_ARGS", e.to_string()),
            };
            let reason = args["reason"].as_str().unwrap_or("");
            match service::record_override(
                &ctx.rt.data_dir,
                &owner,
                task,
                args["expectedRevision"].as_u64().unwrap_or(0),
                kind,
                reason,
            ) {
                Ok(v) => ToolOutcome::ok(v),
                Err(e) => ToolOutcome::err("E_WORKFLOW_OVERRIDE", e),
            }
        }
    }
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
