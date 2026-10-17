use super::*;
use crate::core::agent::test_support::make_core;
use crate::tools::pathutil::WriteRoots;

fn fixture() -> (tempfile::TempDir, ToolCtx) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let data = dir.path().join("profile");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(workspace.join("input.txt"), b"stable").unwrap();
    let roots = WriteRoots {
        workspace: workspace.clone(),
        data_dir: data,
        project_dir: None,
        extra: vec![],
    };
    let core = make_core(&roots);
    let rt = core.get_or_create_session("contract-test", workspace, None, vec![], None, vec![]);
    let mut prefs = rt.prefs();
    prefs.approval_mode = crate::core::prefs::ApprovalMode::FullAccess;
    rt.set_prefs(prefs);
    (
        dir,
        ToolCtx {
            core,
            rt,
            batch_id: "batch".into(),
            call_index: 0,
            call_key: "batch:0".into(),
            provider_call_id: "create".into(),
            cancel: Default::default(),
        },
    )
}
fn draft(kind: &str) -> Value {
    let check = if kind == "command" {
        json!({"id":"check","kind":"command","required":true,
        "command":{"form":"requested","command_text":"echo verified"},
        "expectation":{"kind":"command","allowed_exit_codes":[0]},
        "scope":{"inputs":[{"root_id":"workspace","relative_path":"input.txt"}]}})
    } else {
        json!({
        "id":"check","kind":"manual","required":true,"expectation":{"kind":"manual"},
        "scope":{"inputs":[{"root_id":"workspace","relative_path":"input.txt"}]}})
    };
    json!({"requirements":[{"id":"r1","title":"真实检查","required":true,"check_ids":["check"],"artifact_ids":[]}],
        "artifacts":[],"checks":[check],"review_policy":{"required":true,"independent":true},
        "roots":[{"root_id":"workspace","kind":"workspace","absolute":"ignored-host-overrides"}]})
}
async fn create(ctx: &ToolCtx, args: Value) -> ToolOutcome {
    let rt = ctx.rt.clone();
    let answer = async move {
        let id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(id) = rt.asks.lock().unwrap().keys().next().cloned() {
                    break id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(rt.resolve_ask(
            &id,
            json!({"answers":{"contract_proposal":{"selections":["accept_contract"]}}})
        ));
    };
    let (result, ()) = tokio::join!(DeliveryContractTool.run(ctx, args), answer);
    result
}

#[tokio::test]
async fn create_is_production_bound_and_idempotent_and_changed_draft_rejected() {
    let (_dir, ctx) = fixture();
    let args = json!({"action":"create","draft":draft("manual")});
    let first = create(&ctx, args.clone()).await;
    assert!(first.ok, "{first:?}");
    let task = first.data["contract"]["task_id"].as_str().unwrap();
    assert_eq!(
        ctx.rt.delivery_task_id.lock().unwrap().as_deref(),
        Some(task)
    );
    assert!(
        crate::core::workflow::service::read(
            &ctx.rt.data_dir,
            &DeliveryService::identity_of(&ctx.rt),
            task
        )
        .is_ok()
    );
    let second = create(&ctx, args).await;
    assert!(second.ok, "{second:?}");
    assert_eq!(
        second.data["contract"]["task_id"],
        first.data["contract"]["task_id"]
    );
    let mut changed = draft("manual");
    changed["requirements"][0]["title"] = json!("changed");
    assert!(
        !create(&ctx, json!({"action":"create","draft":changed}))
            .await
            .ok
    );
}
#[cfg(unix)]
#[tokio::test]
async fn registered_command_produces_bound_receipt_and_later_edit_invalidates() {
    let (_dir, mut ctx) = fixture();
    assert!(
        create(&ctx, json!({"action":"create","draft":draft("command")}))
            .await
            .ok
    );
    ctx.provider_call_id = "command".into();
    let output = crate::tools::command::CommandTool
        .run(
            &ctx,
            json!({"command":"echo verified","checkId":"check","fullOutput":false}),
        )
        .await;
    assert!(output.ok, "{output:?}");
    let identity = DeliveryService::identity_of(&ctx.rt);
    let task = ctx.rt.delivery_task_id.lock().unwrap().clone().unwrap();
    let roots = DeliveryService::roots_from_runtime(&ctx.rt);
    let (before, saved) = ctx
        .core
        .delivery
        .evaluate(&identity.session_id, &task, &roots)
        .unwrap();
    assert!(saved);
    assert_eq!(before.overall, AcceptanceStatusKind::Passed);
    std::fs::write(ctx.rt.workspace.join("input.txt"), b"edited").unwrap();
    let (after, _) = ctx
        .core
        .delivery
        .evaluate(&identity.session_id, &task, &roots)
        .unwrap();
    assert_ne!(after.overall, AcceptanceStatusKind::Passed);
    let mismatch = crate::tools::command::CommandTool
        .run(&ctx, json!({"command":"echo fake","checkId":"check"}))
        .await;
    assert!(!mismatch.ok);
}
#[tokio::test]
async fn manual_confirmation_requires_live_answer_and_preserves_mode() {
    let (_dir, ctx) = fixture();
    assert!(
        create(&ctx, json!({"action":"create","draft":draft("manual")}))
            .await
            .ok
    );
    let rt = ctx.rt.clone();
    let mode = rt.prefs().approval_mode;
    let answer = async move {
        let id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(id) = rt.asks.lock().unwrap().keys().next().cloned() {
                    break id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(rt.resolve_ask(
            &id,
            json!({"answers":{"delivery_decision":{"selections":["confirmed"]}}})
        ));
    };
    let (out, ()) = tokio::join!(
        DeliveryContractTool.run(&ctx, json!({"action":"confirm","checkId":"check"})),
        answer
    );
    assert!(out.ok, "{out:?}");
    assert_eq!(ctx.rt.prefs().approval_mode, mode);
    assert_eq!(
        ctx.core
            .delivery
            .ledger(&ctx.rt.id)
            .decisions
            .values()
            .filter(|decision| decision.item_ids.contains(&"check".to_string()))
            .count(),
        1
    );
}
#[tokio::test]
async fn noninteractive_confirmation_fails_without_opening_waiter() {
    let (_dir, ctx) = fixture();
    assert!(
        create(&ctx, json!({"action":"create","draft":draft("manual")}))
            .await
            .ok
    );
    ctx.rt
        .interactive
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let out = DeliveryContractTool
        .run(&ctx, json!({"action":"confirm","checkId":"check"}))
        .await;
    assert!(!out.ok);
    assert!(ctx.rt.asks.lock().unwrap().is_empty());
    assert!(ctx.cancel.is_cancelled());
}

#[tokio::test]
async fn review_requires_real_assignment_independent_actor_and_current_version() {
    let (_dir, mut ctx) = fixture();
    let mut proposal = draft("manual");
    proposal["checks"][0]["kind"] = json!("review");
    proposal["checks"][0]["expectation"] = json!({"kind":"review"});
    assert!(
        create(&ctx, json!({"action":"create","draft":proposal}))
            .await
            .ok
    );
    let dimensions: Vec<_> = crate::core::delivery::types::REVIEW_DIMENSIONS
        .iter()
        .map(|dimension| json!({"dimension":dimension,"state":"checked","review_findings":[]}))
        .collect();
    assert!(
        !DeliveryContractTool
            .run(
                &ctx,
                json!({"action":"submit_review","checkId":"check","dimensions":dimensions})
            )
            .await
            .ok
    );
    ctx.provider_call_id = "assign".into();
    let assigned = DeliveryContractTool
        .run(&ctx, json!({"action":"assign_review","checkId":"check"}))
        .await;
    assert!(assigned.ok, "{assigned:?}");
    let id = assigned.data["assignment"]["review_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut sub = crate::core::agent::SessionRuntime::new_sub(&ctx.rt, "reviewer".into());
    std::sync::Arc::get_mut(&mut sub).unwrap().root_run_id =
        Some(DeliveryService::identity_of(&ctx.rt).root_run_id);
    *sub.delivery_task_id.lock().unwrap() = ctx.rt.delivery_task_id.lock().unwrap().clone();
    *sub.delivery_review_assignment_id.lock().unwrap() = Some(id);
    let subctx = ToolCtx {
        core: ctx.core.clone(),
        rt: sub,
        batch_id: "review".into(),
        call_index: 0,
        call_key: "review:0".into(),
        provider_call_id: "review_result".into(),
        cancel: Default::default(),
    };
    let out = DeliveryContractTool
        .run(
            &subctx,
            json!({"action":"submit_review","checkId":"check","dimensions":dimensions}),
        )
        .await;
    assert!(out.ok, "{out:?}");
    let identity = DeliveryService::identity_of(&ctx.rt);
    let task = ctx.rt.delivery_task_id.lock().unwrap().clone().unwrap();
    let (snapshot, _) = ctx
        .core
        .delivery
        .evaluate(
            &identity.session_id,
            &task,
            &DeliveryService::roots_from_runtime(&ctx.rt),
        )
        .unwrap();
    assert_eq!(snapshot.overall, AcceptanceStatusKind::Passed);
    std::fs::write(ctx.rt.workspace.join("input.txt"), b"edited after review").unwrap();
    let (stale, _) = ctx
        .core
        .delivery
        .evaluate(
            &identity.session_id,
            &task,
            &DeliveryService::roots_from_runtime(&ctx.rt),
        )
        .unwrap();
    assert_ne!(stale.overall, AcceptanceStatusKind::Passed);
    assert!(
        !DeliveryContractTool
            .run(
                &subctx,
                json!({"action":"submit_review","checkId":"check","dimensions":dimensions})
            )
            .await
            .ok
    );
}

async fn with_answer(ctx: &ToolCtx, args: Value, question: &str, choice: &str) -> ToolOutcome {
    let rt = ctx.rt.clone();
    let answer = async {
        let id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(id) = rt.asks.lock().unwrap().keys().next().cloned() {
                    break id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(rt.resolve_ask(&id, json!({"answers":{question:{"selections":[choice]}}})));
    };
    let (out, ()) = tokio::join!(DeliveryContractTool.run(ctx, args), answer);
    out
}

#[tokio::test]
async fn proposal_decline_and_noninteractive_never_publish_contract_or_change_permission() {
    let (_dir, ctx) = fixture();
    let mode = ctx.rt.prefs().approval_mode;
    let out = with_answer(
        &ctx,
        json!({"action":"create","draft":draft("manual")}),
        "contract_proposal",
        "decline_contract",
    )
    .await;
    assert!(!out.ok);
    assert!(ctx.core.delivery.active_contract(&ctx.rt.id).is_none());
    assert_eq!(ctx.rt.prefs().approval_mode, mode);
    assert!(ctx.rt.asks.lock().unwrap().is_empty());
    ctx.rt
        .interactive
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        !DeliveryContractTool
            .run(&ctx, json!({"action":"create","draft":draft("manual")}))
            .await
            .ok
    );
    assert!(ctx.core.delivery.active_contract(&ctx.rt.id).is_none());
}

#[tokio::test]
async fn cancelling_requires_explicit_current_scope_decision() {
    let (_dir, mut ctx) = fixture();
    assert!(
        create(&ctx, json!({"action":"create","draft":draft("manual")}))
            .await
            .ok
    );
    ctx.provider_call_id = "waive".into();
    let waived = with_answer(
        &ctx,
        json!({"action":"confirm","checkId":"check","targetId":"r1"}),
        "delivery_decision",
        "waived",
    )
    .await;
    assert!(waived.ok, "{waived:?}");
    let contract = ctx.core.delivery.active_contract(&ctx.rt.id).unwrap();
    let identity = DeliveryService::identity_of(&ctx.rt);
    let roots = DeliveryService::roots_from_runtime(&ctx.rt);
    let changes = vec![ScopeChange {
        id: "scope".into(),
        target_id: "r1".into(),
        kind: ScopeChangeKind::Cancelled,
        reason: "explicit cancellation".into(),
        user_decision_id: None,
        at: "0".into(),
    }];
    let id = waived.data["write"]["record_id"].as_str().unwrap();
    assert!(
        ctx.core
            .delivery
            .revise_contract_with(
                &identity,
                &contract.task_id,
                contract.revision,
                changes.clone(),
                Some(id),
                "wrong_kind",
                Some(&roots),
                |_| Ok(())
            )
            .is_err()
    );
    std::fs::write(ctx.rt.workspace.join("input.txt"), b"changed").unwrap();
    let mut waiver_changes = changes.clone();
    waiver_changes[0].kind = ScopeChangeKind::Waived;
    assert!(
        ctx.core
            .delivery
            .revise_contract_with(
                &identity,
                &contract.task_id,
                contract.revision,
                waiver_changes,
                Some(id),
                "stale_scope",
                Some(&roots),
                |_| Ok(())
            )
            .is_err()
    );
    ctx.provider_call_id = "cancel".into();
    let cancelled = with_answer(
        &ctx,
        json!({"action":"confirm","checkId":"check","targetId":"r1"}),
        "delivery_decision",
        "cancelled",
    )
    .await;
    assert!(cancelled.ok, "{cancelled:?}");
    ctx.provider_call_id = "revise".into();
    let revised = DeliveryContractTool.run(&ctx, json!({"action":"revise","expectedRevision":contract.revision,"changes":changes,"userDecisionId":cancelled.data["write"]["record_id"]})).await;
    assert!(revised.ok, "{revised:?}");
    let snapshot = ctx
        .core
        .delivery
        .evaluate_current(&identity.session_id, &contract.task_id, &roots)
        .unwrap();
    assert_eq!(snapshot.items[0].status, AcceptanceStatus::Cancelled);
    let before = ctx.core.delivery.store().load_index(&ctx.rt.id).revision;
    ctx.core
        .delivery
        .evaluate_current(&identity.session_id, &contract.task_id, &roots)
        .unwrap();
    assert_eq!(
        before,
        ctx.core.delivery.store().load_index(&ctx.rt.id).revision,
        "readonly projection must not publish snapshots"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn independent_check_needs_host_assigned_verifier_and_declared_cwd() {
    let (_dir, mut ctx) = fixture();
    let mut proposal = draft("command");
    proposal["checks"][0]["independent_verifier"] = json!(true);
    assert!(
        create(&ctx, json!({"action":"create","draft":proposal}))
            .await
            .ok
    );
    let task = ctx.rt.delivery_task_id.lock().unwrap().clone().unwrap();
    let identity = DeliveryService::identity_of(&ctx.rt);
    let roots = DeliveryService::roots_from_runtime(&ctx.rt);
    ctx.provider_call_id = "self_test".into();
    assert!(
        crate::tools::command::CommandTool
            .run(&ctx, json!({"command":"echo verified","checkId":"check"}))
            .await
            .ok
    );
    assert_ne!(
        ctx.core
            .delivery
            .evaluate_current(&identity.session_id, &task, &roots)
            .unwrap()
            .overall,
        AcceptanceStatusKind::Passed
    );
    std::fs::create_dir(ctx.rt.workspace.join("other")).unwrap();
    assert!(
        !crate::tools::command::CommandTool
            .run(
                &ctx,
                json!({"command":"echo verified","checkId":"check","cwd":"other"})
            )
            .await
            .ok
    );
    let mut sub = crate::core::agent::SessionRuntime::new_sub(&ctx.rt, "verifier".into());
    std::sync::Arc::get_mut(&mut sub).unwrap().root_run_id = Some(identity.root_run_id.clone());
    *sub.delivery_task_id.lock().unwrap() = Some(task.clone());
    *sub.delivery_verification_task_id.lock().unwrap() = Some(task.clone());
    let subctx = ToolCtx {
        core: ctx.core.clone(),
        rt: sub,
        batch_id: "verify".into(),
        call_index: 0,
        call_key: "verify:0".into(),
        provider_call_id: "verified".into(),
        cancel: Default::default(),
    };
    assert!(
        !crate::tools::command::CommandTool
            .run(&subctx, json!({"command":"echo verified"}))
            .await
            .ok
    );
    let out = crate::tools::command::CommandTool
        .run(
            &subctx,
            json!({"command":"echo verified","checkId":"check"}),
        )
        .await;
    assert!(out.ok, "{out:?}");
    assert_eq!(
        ctx.core
            .delivery
            .evaluate_current(&identity.session_id, &task, &roots)
            .unwrap()
            .overall,
        AcceptanceStatusKind::Passed
    );
}

#[tokio::test]
async fn same_run_new_contract_and_revision_reject_old_command_and_manual_evidence() {
    for kind in ["command", "manual"] {
        let (_dir, mut ctx) = fixture();
        assert!(
            create(&ctx, json!({"action":"create","draft":draft(kind)}))
                .await
                .ok
        );
        if kind == "command" {
            assert!(
                crate::tools::command::CommandTool
                    .run(&ctx, json!({"command":"echo verified","checkId":"check"}))
                    .await
                    .ok
            );
        } else {
            assert!(
                with_answer(
                    &ctx,
                    json!({"action":"confirm","checkId":"check"}),
                    "delivery_decision",
                    "confirmed"
                )
                .await
                .ok
            );
        }
        let identity = DeliveryService::identity_of(&ctx.rt);
        let old_task = ctx.rt.delivery_task_id.lock().unwrap().clone().unwrap();
        let roots = DeliveryService::roots_from_runtime(&ctx.rt);
        assert_eq!(
            ctx.core
                .delivery
                .evaluate_current(&identity.session_id, &old_task, &roots)
                .unwrap()
                .overall,
            AcceptanceStatusKind::Passed
        );
        ctx.provider_call_id = format!("second-contract-{kind}");
        assert!(
            create(&ctx, json!({"action":"create","draft":draft(kind)}))
                .await
                .ok
        );
        let new_task = ctx.rt.delivery_task_id.lock().unwrap().clone().unwrap();
        assert_ne!(old_task, new_task);
        assert_ne!(
            ctx.core
                .delivery
                .evaluate_current(&identity.session_id, &new_task, &roots)
                .unwrap()
                .overall,
            AcceptanceStatusKind::Passed,
            "same run, check id, command and files must not authorize a different task"
        );
        // The old task's immutable record remains intact and independently readable.
        assert_eq!(
            ctx.core
                .delivery
                .evaluate_current(&identity.session_id, &old_task, &roots)
                .unwrap()
                .overall,
            AcceptanceStatusKind::Passed
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn intermittent_regression_uses_real_red_and_ten_green_receipts() {
    let (_dir, mut ctx) = fixture();
    std::fs::write(ctx.rt.workspace.join("subject.txt"), b"bad").unwrap();
    std::fs::write(ctx.rt.workspace.join("assertion.sh"),b"if [ \"$(cat subject.txt)\" = good ]; then echo passed; else echo 'test result: FAILED'; exit 1; fi\n").unwrap();
    let scope = json!({"inputs":[{"root_id":"workspace","relative_path":"subject.txt"}],"dependency_files":[{"root_id":"workspace","relative_path":"assertion.sh"}]});
    let proposal = json!({"requirements":[{"id":"r1","title":"回归稳定","required":true,"check_ids":["source","regression"],"artifact_ids":[]}],"checks":[
        {"id":"source","kind":"command","required":true,"command":{"form":"requested","command_text":"sh assertion.sh"},"scope":scope,"expectation":{"kind":"command","allowed_exit_codes":[0]}},
        {"id":"regression","kind":"structured","required":true,"scope":scope,"expectation":{"kind":"structured","validator_id":"regression_intermittent","validator_version":"1"}}],"artifacts":[],"roots":[{"root_id":"workspace","kind":"workspace","absolute":"ignored"}],"review_policy":{"required":false,"independent":false}});
    assert!(
        create(&ctx, json!({"action":"create","draft":proposal}))
            .await
            .ok
    );
    ctx.provider_call_id = "red".into();
    let red = crate::tools::command::CommandTool
        .run(
            &ctx,
            json!({"command":"sh assertion.sh","checkId":"source"}),
        )
        .await;
    let red_id = red.data["receipt_id"].as_str().unwrap().to_owned();
    std::fs::write(ctx.rt.workspace.join("subject.txt"), b"good").unwrap();
    let mut green_id = String::new();
    for i in 0..10 {
        ctx.provider_call_id = format!("green-{i}");
        let green = crate::tools::command::CommandTool
            .run(
                &ctx,
                json!({"command":"sh assertion.sh","checkId":"source"}),
            )
            .await;
        assert!(green.ok, "{green:?}");
        green_id = green.data["receipt_id"].as_str().unwrap().into();
        if i == 0 {
            ctx.provider_call_id = "too-few-observations".into();
            let early=DeliveryContractTool.run(&ctx,json!({"action":"verify_regression","checkId":"regression","redReceiptId":red_id,"greenReceiptId":green_id})).await;
            assert!(early.ok, "{early:?}");
            let identity = DeliveryService::identity_of(&ctx.rt);
            let ledger = ctx.core.delivery.ledger(&identity.session_id);
            assert!(
                ledger
                    .validations
                    .values()
                    .all(|v| v.validator_id != "regression_intermittent"
                        || v.outcome != ValidationOutcome::Passed)
            );
        }
    }
    ctx.provider_call_id = "ten-observations".into();
    let verified=DeliveryContractTool.run(&ctx,json!({"action":"verify_regression","checkId":"regression","redReceiptId":red_id,"greenReceiptId":green_id})).await;
    assert!(verified.ok, "{verified:?}");
    let identity = DeliveryService::identity_of(&ctx.rt);
    let task = ctx.rt.delivery_task_id.lock().unwrap().clone().unwrap();
    let roots = DeliveryService::roots_from_runtime(&ctx.rt);
    assert_eq!(
        ctx.core
            .delivery
            .evaluate_current(&identity.session_id, &task, &roots)
            .unwrap()
            .overall,
        AcceptanceStatusKind::Passed
    );
}

#[tokio::test]
async fn review_submission_uses_frozen_policy_for_verdict_and_extra_dimensions() {
    let (_dir, mut ctx) = fixture();
    let mut proposal = draft("manual");
    proposal["checks"][0]["kind"] = json!("review");
    proposal["checks"][0]["expectation"] = json!({"kind":"review","dimensions":["security"]});
    proposal["review_policy"]["dimensions"] = json!(["security", "licensing"]);
    proposal["review_policy"]["blocking_severities"] = json!(["warning"]);
    assert!(
        create(&ctx, json!({"action":"create","draft":proposal}))
            .await
            .ok
    );
    ctx.provider_call_id = "assign-policy".into();
    let assigned = DeliveryContractTool
        .run(&ctx, json!({"action":"assign_review","checkId":"check"}))
        .await;
    assert!(assigned.ok, "{assigned:?}");
    let mut sub = crate::core::agent::SessionRuntime::new_sub(&ctx.rt, "policy-reviewer".into());
    std::sync::Arc::get_mut(&mut sub).unwrap().root_run_id =
        Some(DeliveryService::identity_of(&ctx.rt).root_run_id);
    *sub.delivery_task_id.lock().unwrap() = ctx.rt.delivery_task_id.lock().unwrap().clone();
    *sub.delivery_review_assignment_id.lock().unwrap() = Some(
        assigned.data["assignment"]["review_id"]
            .as_str()
            .unwrap()
            .into(),
    );
    let mut subctx = ToolCtx {
        core: ctx.core.clone(),
        rt: sub,
        batch_id: "review".into(),
        call_index: 0,
        call_key: "review:0".into(),
        provider_call_id: "policy-incomplete".into(),
        cancel: Default::default(),
    };
    let checked = json!({"dimension":"security","state":"checked","review_findings":[]});
    let incomplete = DeliveryContractTool
        .run(
            &subctx,
            json!({"action":"submit_review","checkId":"check","dimensions":[checked]}),
        )
        .await;
    assert!(incomplete.ok, "{incomplete:?}");
    assert_eq!(
        incomplete.data["review"]["verdict"]["approved"],
        json!(false)
    );
    subctx.provider_call_id = "policy-warning".into();
    let warning = json!({"dimension":"licensing","state":"checked","review_findings":[{"stable_id":"license-1","severity":"warning","message":"blocking by contract","evidence":[],"needs_human_check":true}]});
    let blocked = DeliveryContractTool
        .run(
            &subctx,
            json!({"action":"submit_review","checkId":"check","dimensions":[checked, warning]}),
        )
        .await;
    assert!(blocked.ok, "{blocked:?}");
    assert_eq!(blocked.data["review"]["verdict"]["approved"], json!(false));
    assert_eq!(
        blocked.data["review"]["verdict"]["blocking_count"],
        json!(1)
    );
    subctx.provider_call_id = "policy-clean".into();
    let clean = DeliveryContractTool.run(&subctx, json!({"action":"submit_review","checkId":"check","dimensions":[checked,{"dimension":"licensing","state":"checked","review_findings":[]}]})).await;
    assert!(clean.ok, "{clean:?}");
    assert_eq!(clean.data["review"]["verdict"]["approved"], json!(true));
}
