//! Production task-contract, review and human-decision entry point.
use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::delivery::{DeliveryService, responsibility, types::*, version};
use serde_json::{Value, json};

pub struct DeliveryContractTool;

fn error(error: impl std::fmt::Display) -> ToolOutcome {
    ToolOutcome::err("E_DELIVERY_INVALID", error.to_string())
}

fn capture(roots: &version::RootResolver, scope: &VerificationScope) -> Result<String, String> {
    let value =
        version::capture_scope(roots, scope, &Default::default()).map_err(|e| format!("{e:?}"))?;
    if !value.is_valid() {
        return Err("声明范围未能完整采集".into());
    }
    Ok(value.digest)
}

#[async_trait::async_trait]
impl Tool for DeliveryContractTool {
    fn name(&self) -> &'static str {
        "delivery_contract"
    }
    fn description(&self) -> &'static str {
        "任务交付契约：开始开发前 create 登记 requirements/artifacts/checks/review_policy/roots；read 查看；evaluate 重算；assign_review 签发独立审查；审查子代理 submit_review；confirm 通过真实用户询问记录人工确认/豁免。所有身份与证据版本均由宿主补齐，不接受自签passed。command检查用 command.checkId 运行。"
    }
    fn schema(&self) -> &'static str {
        include_str!("delivery_contract.schema.json")
    }
    // confirm is exclusive and may wait for a real answer. Other actions are also exclusive.
    fn kind(&self) -> ToolKind {
        ToolKind::Interactive
    }
    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let identity = DeliveryService::identity_of(&ctx.rt);
        let roots = DeliveryService::roots_from_runtime(&ctx.rt);
        let svc = &ctx.core.delivery;
        let operation_id = format!(
            "delivery_{}_{}_{}",
            identity.root_run_id, identity.actor_id, ctx.provider_call_id
        );
        let action = args["action"].as_str().unwrap_or("");
        if action == "create" {
            if identity.sub_id.is_some() {
                return error("只有根任务能创建契约");
            }
            let draft: ContractDraft = match serde_json::from_value(args["draft"].clone()) {
                Ok(d) => d,
                Err(e) => return error(e),
            };
            if !ctx
                .rt
                .interactive
                .load(std::sync::atomic::Ordering::Acquire)
            {
                *ctx.rt.interaction_blocked.lock().unwrap() =
                    Some("interaction_unavailable".into());
                ctx.cancel.cancel();
                return ToolOutcome::err(
                    "E_INTERACTION_UNAVAILABLE",
                    "模型提出的契约需要用户确认；无交互宿主须通过可信宿主输入提供契约",
                );
            }
            let ask_id = format!("contract_{}", uuid::Uuid::new_v4().simple());
            let (tx, rx) = tokio::sync::oneshot::channel();
            ctx.rt.open_ask(&ask_id, tx);
            ctx.core.sink.emit(&ctx.rt.id,"ask:opened",json!({"session":ctx.rt.id,"ask_id":ask_id,"kind":"ask","run_id":identity.actor_run_id,
                "questions":[{"id":"contract_proposal","question":format!("请确认采用以下任务验收契约。确认只登记要求，不切换权限档位。\n{}",serde_json::to_string_pretty(&draft).unwrap_or_default()),"single":true,
                "options":[{"id":"accept_contract","label":"采用此契约"},{"id":"decline_contract","label":"暂不采用"}]}]}));
            let answer = tokio::select! {_=ctx.cancel.cancelled()=>None,answer=rx=>answer.ok()};
            ctx.rt.asks.lock().unwrap().remove(&ask_id);
            ctx.core.sink.emit(
                &ctx.rt.id,
                "ask:closed",
                json!({"session":ctx.rt.id,"ask_id":ask_id}),
            );
            let approved = answer
                .as_ref()
                .and_then(|v| v["answers"]["contract_proposal"]["selections"].as_array())
                .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some("accept_contract"));
            if !approved {
                return ToolOutcome::err("E_CONTRACT_NOT_APPROVED", "用户未采用该草稿，契约未创建");
            }
            let models = ctx
                .core
                .cfg
                .read()
                .unwrap()
                .providers
                .iter()
                .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
                .collect();
            let result =
                svc.create_contract_with(&identity, &draft, &roots, &operation_id, |contract| {
                    crate::core::workflow::service::prepare(
                        contract,
                        &roots,
                        &ctx.rt.data_dir,
                        ctx.rt.project_id.as_deref().unwrap_or("temporary"),
                        ctx.rt
                            .project_id
                            .as_ref()
                            .map(|_| ctx.rt.workspace.as_path()),
                        &models,
                    )
                    .and_then(|prepared| {
                        prepared.validate_models(&ctx.core.cfg.read().unwrap())?;
                        crate::core::workflow::service::persist(&ctx.rt.data_dir, &prepared)
                    })
                    .map_err(|message| crate::core::delivery::DeliveryError::Save { message })?;
                    let approval = UserDecision {
                        binding: Some(crate::core::delivery::types::ContractBinding::of(contract)),
                        id: crate::core::delivery::store::host_id(
                            &format!("adopt_{operation_id}"),
                            "user_decision",
                        ),
                        owner: identity.clone(),
                        item_ids: vec![contract.task_id.clone()],
                        decision: UserDecisionKind::Confirmed,
                        scope_version: None,
                        prompt_id: ask_id.clone(),
                        answered_at: crate::core::delivery::store::now_iso(),
                    };
                    svc.record_user_decision(&identity, approval, &format!("adopt_{operation_id}"))
                        .map(|_| ())
                });
            return match result {
                Ok((contract, receipt)) => {
                    *ctx.rt.delivery_task_id.lock().unwrap() = Some(contract.task_id.clone());
                    ToolOutcome::ok(json!({"contract":contract,"write":receipt}))
                }
                Err(e) => error(e),
            };
        }
        let task_id = args["taskId"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| ctx.rt.delivery_task_id.lock().unwrap().clone());
        let contract = task_id
            .as_deref()
            .and_then(|id| svc.read_contract(&identity.session_id, id))
            .or_else(|| svc.active_contract(&identity.session_id));
        let Some(contract) = contract else {
            return error("无有效任务契约；先 create");
        };
        if contract.owner.root_run_id != identity.root_run_id {
            return error("契约属于其他 run，不能沿用旧任务证据");
        }
        if action == "read" {
            return ToolOutcome::ok(json!({"contract":contract}));
        }
        if action == "evaluate" {
            return match svc.evaluate(&identity.session_id, &contract.task_id, &roots) {
                Ok((snapshot, persisted)) => {
                    ToolOutcome::ok(json!({"acceptance":snapshot,"persisted":persisted}))
                }
                Err(e) => error(e),
            };
        }
        if action == "revise" {
            if identity.sub_id.is_some() {
                return error("子代理不能修订根契约");
            }
            let changes = match serde_json::from_value(args["changes"].clone()) {
                Ok(v) => v,
                Err(e) => return error(e),
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
            return match svc.revise_contract_with(
                &identity,
                &contract.task_id,
                args["expectedRevision"].as_u64().unwrap_or(0),
                changes,
                args["userDecisionId"].as_str(),
                &operation_id,
                Some(&roots),
                |contract| {
                    crate::core::workflow::service::prepare(
                        contract,
                        &roots,
                        &ctx.rt.data_dir,
                        ctx.rt.project_id.as_deref().unwrap_or("temporary"),
                        ctx.rt
                            .project_id
                            .as_ref()
                            .map(|_| ctx.rt.workspace.as_path()),
                        &models,
                    )
                    .and_then(|prepared| {
                        prepared.validate_models(&ctx.core.cfg.read().unwrap())?;
                        crate::core::workflow::service::persist(&ctx.rt.data_dir, &prepared)
                    })
                    .map_err(|message| crate::core::delivery::DeliveryError::Save { message })
                },
            ) {
                Ok((c, w)) => ToolOutcome::ok(json!({"contract":c,"write":w})),
                Err(e) => error(e),
            };
        }
        let Some(check) = args["checkId"].as_str().and_then(|id| contract.check(id)) else {
            return error("checkId 必须命中本契约检查");
        };
        let target = args["targetId"].as_str().unwrap_or(&check.id);
        let decision_scope = if action == "confirm" {
            match crate::core::delivery::service::target_scope(&contract, target) {
                Some(scope) => scope,
                None => return error("人工决定目标不在契约内"),
            }
        } else {
            check.scope.clone()
        };
        let before = match capture(&roots, &decision_scope) {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        if action == "verify_regression" {
            let CheckExpectation::Structured { validator_id, .. } = &check.expectation else {
                return error("复现配对检查必须是 structured/regression");
            };
            if validator_id != "regression" && validator_id != "regression_intermittent" {
                return error("检查未登记 regression 校验器");
            }
            let ledger = svc.ledger(&identity.session_id);
            let red = args["redReceiptId"]
                .as_str()
                .and_then(|id| ledger.receipts.get(id))
                .filter(|r| {
                    r.binding
                        .as_ref()
                        .is_some_and(|binding| binding.matches(&contract))
                });
            let green = args["greenReceiptId"]
                .as_str()
                .and_then(|id| ledger.receipts.get(id))
                .filter(|r| {
                    r.binding
                        .as_ref()
                        .is_some_and(|binding| binding.matches(&contract))
                });
            let source_id = red.and_then(|r| r.check_id.as_deref()).unwrap_or("");
            let Some(source) = contract.check(source_id) else {
                return error("RED 必须绑定已登记命令检查");
            };
            if source.kind != CheckKind::Command || source.scope.dependency_files.is_empty() {
                return error(
                    "复现命令必须事先声明非空 dependency_files，冻结实际测试/断言定义；空依赖不能证明断言未被删除",
                );
            }
            // Failure codes are fixed by the host policy, never chosen after observing RED.
            let failure_codes = vec![1, 101];
            let red_output = red
                .and_then(|red| {
                    svc.store()
                        .read_execution_output(&identity.session_id, &red.id)
                })
                .map(|bytes| String::from_utf8_lossy(&bytes).to_lowercase())
                .unwrap_or_default();
            let assertion_failed = [
                "assertion",
                "test result: failed",
                "failed tests",
                "tests failed",
            ]
            .iter()
            .any(|marker| red_output.contains(marker));
            let pair = crate::core::delivery::regression::RegressionPair {
                issue_id: check.id.clone(),
                check_id: source_id.into(),
                red_receipt_id: red.map(|r| r.id.clone()),
                green_receipt_id: green.map(|r| r.id.clone()),
                reproduction_status:
                    crate::core::delivery::regression::ReproductionStatus::Unproven,
                reason: None,
                red_version: red.and_then(|r| r.after.as_ref()).map(|v| v.digest.clone()),
                green_version: green
                    .and_then(|r| r.after.as_ref())
                    .map(|v| v.digest.clone()),
                check_spec_revision: contract.revision,
                nondeterminism: if validator_id == "regression_intermittent" {
                    let observations: Vec<_> = ledger
                        .receipts
                        .values()
                        .filter(|r| {
                            r.check_id.as_deref() == Some(source_id)
                                && r.binding
                                    .as_ref()
                                    .is_some_and(|binding| binding.matches(&contract))
                                && r.owner.session_id == identity.session_id
                                && r.owner.root_run_id == identity.root_run_id
                                && red.is_some_and(|red| r.started_at >= red.finished_at)
                                && r.after.as_ref().is_some_and(|after| {
                                    after.is_valid()
                                        && green
                                            .and_then(|g| g.after.as_ref())
                                            .is_some_and(|g| g.digest == after.digest)
                                })
                        })
                        .collect();
                    Some(crate::core::delivery::regression::NondeterminismRecord {
                        runs: observations.len() as u32,
                        failures: observations
                            .iter()
                            .filter(|r| {
                                r.outcome != ExecutionOutcome::Exited
                                    || r.exit_code != Some(0)
                                    || r.signal.is_some()
                                    || green.is_none_or(|g| {
                                        r.requested != g.requested
                                            || r.resolved != g.resolved
                                            || r.cwd != g.cwd
                                    })
                            })
                            .count() as u32,
                        environment: green
                            .and_then(|g| g.after.as_ref())
                            .map(|v| {
                                v.toolchain
                                    .iter()
                                    .map(|f| format!("{}={}", f.key, f.value))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                } else {
                    None
                },
            };
            let (status, detail) = crate::core::delivery::regression::judge(
                &pair,
                red,
                green,
                &identity,
                &failure_codes,
                &roots,
            );
            let unchanged_assertions = source.scope.dependency_files.iter().all(|path| {
                let a = red
                    .and_then(|r| r.after.as_ref())
                    .and_then(|v| v.entry(&path.root_id, &path.relative_path));
                let b = green
                    .and_then(|r| r.after.as_ref())
                    .and_then(|v| v.entry(&path.root_id, &path.relative_path));
                a.is_some() && a == b
            });
            let current_green = green.and_then(|r| r.after.as_ref()).is_some_and(|after| {
                version::capture_scope(&roots, &source.scope, &Default::default())
                    .ok()
                    .is_some_and(|v| version::compare_scope(after, &v).is_fresh())
            });
            let passed = status == crate::core::delivery::regression::ReproductionStatus::Fixed
                && unchanged_assertions
                && current_green
                && assertion_failed;
            let record = BuiltinValidationRecord {
                binding: Some(crate::core::delivery::types::ContractBinding::of(&contract)),
                id: crate::core::delivery::store::host_id(&operation_id, "validation"),
                owner: identity.clone(),
                validator_id: validator_id.clone(),
                validator_version: "1".into(),
                check_id: Some(check.id.clone()),
                input_refs: vec![
                    pair.red_receipt_id.clone().unwrap_or_default(),
                    pair.green_receipt_id.clone().unwrap_or_default(),
                ],
                scope_version: Some(before),
                started_at: crate::core::delivery::store::now_iso(),
                finished_at: crate::core::delivery::store::now_iso(),
                outcome: if passed {
                    ValidationOutcome::Passed
                } else {
                    ValidationOutcome::Failed
                },
                findings: if passed {
                    vec![]
                } else {
                    vec![ValidationFinding {
                        severity: FindingSeverity::Error,
                        location: None,
                        message: format!(
                            "{:?}；{}；断言未变={}；GREEN新鲜={}",
                            status,
                            detail.unwrap_or_default(),
                            unchanged_assertions,
                            current_green
                        ),
                    }]
                },
                output_ref: None,
            };
            return match svc.record_validation(&identity, record.clone(), &operation_id) {
                Ok(write) => {
                    ToolOutcome::ok(json!({"pair":pair,"validation":record,"write":write}))
                }
                Err(e) => error(e),
            };
        }
        if action == "assign_review" {
            if identity.sub_id.is_some() || check.kind != CheckKind::Review {
                return error("只有根任务能分派 review 检查");
            }
            let ledger = svc.ledger(&identity.session_id);
            let mut implementers = vec![contract.owner.actor_id.clone()];
            implementers.extend(
                ledger
                    .deliveries
                    .values()
                    .filter(|d| d.task_id == contract.task_id)
                    .map(|d| d.owner.actor_id.clone()),
            );
            implementers.sort();
            implementers.dedup();
            let assignment = responsibility::ReviewAssignment {
                review_id: format!("review_{}", uuid::Uuid::new_v4().simple()),
                implementer_actor_ids: implementers,
                target_scope_version: Some(before),
                policy_revision: contract.revision,
            };
            return match svc.record_review_assignment(&identity, assignment.clone(), &operation_id)
            {
                Ok(write) => ToolOutcome::ok(
                    json!({"assignment":assignment,"write":write,"task_id":contract.task_id,"check_id":check.id}),
                ),
                Err(e) => error(e),
            };
        }
        if action == "submit_review" {
            if check.kind != CheckKind::Review {
                return error("需要review检查");
            }
            let assignment_id = ctx.rt.delivery_review_assignment_id.lock().unwrap().clone();
            let Some(assignment) = assignment_id
                .as_deref()
                .and_then(|id| svc.read_review_assignment(&identity.session_id, id))
            else {
                return error("审查必须绑定宿主分派，不能自称审查者");
            };
            if let Err(e) = responsibility::validate_review_submission(
                &assignment,
                &identity,
                &contract.owner.root_run_id,
                &contract.review_policy,
            ) {
                return error(e.join("；"));
            }
            if assignment.policy_revision != contract.revision
                || assignment.target_scope_version.as_deref() != Some(before.as_str())
            {
                return error("审查目标或契约已变，需重新分派");
            }
            let dimensions: Vec<ReviewDimensionResult> =
                match serde_json::from_value(args["dimensions"].clone()) {
                    Ok(v) => v,
                    Err(e) => return error(e),
                };
            if dimensions.iter().any(|d| {
                d.state.is_none()
                    || !d.findings.is_empty()
                    || d.review_findings
                        .iter()
                        .any(|f| f.stable_id.trim().is_empty())
                    || (d.state == Some(responsibility::DimensionState::NotApplicable)
                        && d.reason.as_deref().is_none_or(|r| r.trim().is_empty()))
            }) {
                return error(
                    "逐维结论必须包含 state(checked/not_applicable/unverified)，不适用须给 reason，发现使用 review_findings 并保留 stable_id",
                );
            }
            let blocking = dimensions
                .iter()
                .flat_map(|d| &d.review_findings)
                .filter(|f| contract.review_policy.is_blocking(f.severity))
                .count();
            let check_dimensions = match &check.expectation {
                CheckExpectation::Review { dimensions } => dimensions.as_deref(),
                _ => None,
            };
            let required = contract.review_policy.required_dimensions(check_dimensions);
            let dimensions_complete = required.iter().all(|required| {
                dimensions.iter().any(|d| {
                    &d.dimension == required
                        && match d.state {
                            Some(responsibility::DimensionState::Checked) => true,
                            Some(responsibility::DimensionState::NotApplicable) => {
                                d.reason.as_deref().is_some_and(|r| !r.trim().is_empty())
                            }
                            _ => false,
                        }
                })
            });
            let review = ReviewResult {
                schema: SCHEMA,
                id: crate::core::delivery::store::host_id(&operation_id, "review"),
                owner: identity.clone(),
                reviewer: identity.clone(),
                task_id: contract.task_id.clone(),
                check_id: Some(check.id.clone()),
                contract_revision: contract.revision,
                dimensions,
                verdict: ReviewVerdict {
                    approved: blocking == 0 && dimensions_complete,
                    blocking_count: blocking,
                },
                scope_version: Some(before),
                reviewed_at: crate::core::delivery::store::now_iso(),
            };
            return match svc.record_review(&identity, review.clone(), &operation_id) {
                Ok(write) => ToolOutcome::ok(json!({"review":review,"write":write})),
                Err(e) => error(e),
            };
        }
        if action == "confirm" {
            if !ctx
                .rt
                .interactive
                .load(std::sync::atomic::Ordering::Acquire)
            {
                *ctx.rt.interaction_blocked.lock().unwrap() =
                    Some("interaction_unavailable".into());
                ctx.cancel.cancel();
                return ToolOutcome::err("E_INTERACTION_UNAVAILABLE", "当前宿主不支持人工验收询问");
            }
            let ask_id = format!("delivery_{}", uuid::Uuid::new_v4().simple());
            let (tx, rx) = tokio::sync::oneshot::channel();
            ctx.rt.open_ask(&ask_id, tx);
            ctx.core.sink.emit(&ctx.rt.id,"ask:opened",json!({"session":ctx.rt.id,"ask_id":ask_id,"kind":"ask",
                "run_id":identity.actor_run_id,"questions":[{"id":"delivery_decision","question":format!("人工验收目标 {}（检查 {}）：请在实际验证后确认，或明确豁免/取消该目标。",target,check.id),"single":true,
                "options":[{"id":"confirmed","label":"我已实际验证通过"},{"id":"waived","label":"明确豁免此项"},{"id":"declined","label":"尚未通过"},{"id":"cancelled","label":"明确取消该目标，不再要求完成"}]}]}));
            let answer = tokio::select! { _=ctx.cancel.cancelled()=>None, answer=rx=>answer.ok() };
            ctx.rt.asks.lock().unwrap().remove(&ask_id);
            ctx.core.sink.emit(
                &ctx.rt.id,
                "ask:closed",
                json!({"session":ctx.rt.id,"ask_id":ask_id}),
            );
            let Some(answer) = answer else {
                return ToolOutcome::err("E_ASK_CANCELLED", "人工验收取消");
            };
            let selections = answer["answers"]["delivery_decision"]["selections"].as_array();
            let decision = match selections
                .filter(|v| v.len() == 1)
                .and_then(|v| v[0].as_str())
            {
                Some("confirmed") if check.kind == CheckKind::Manual && target == check.id => {
                    UserDecisionKind::Confirmed
                }
                Some("waived") => UserDecisionKind::Waived,
                Some("cancelled") => UserDecisionKind::Cancelled,
                _ => return ToolOutcome::err("E_NOT_CONFIRMED", "用户未确认或豁免"),
            };
            if capture(&roots, &decision_scope).ok().as_deref() != Some(before.as_str()) {
                return error("等待人工确认期间目标发生变化，原决定不适用于当前版本");
            }
            let decision = UserDecision {
                binding: Some(crate::core::delivery::types::ContractBinding::of(&contract)),
                id: crate::core::delivery::store::host_id(&operation_id, "user_decision"),
                owner: identity.clone(),
                item_ids: vec![target.to_owned()],
                decision,
                scope_version: Some(before),
                prompt_id: ask_id,
                answered_at: crate::core::delivery::store::now_iso(),
            };
            return match svc.record_user_decision(&identity, decision.clone(), &operation_id) {
                Ok(write) => ToolOutcome::ok(json!({"decision":decision,"write":write})),
                Err(e) => error(e),
            };
        }
        error("未知action")
    }
}

#[cfg(test)]
#[path = "delivery_contract_tests.rs"]
mod tests;
