use super::*;
use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState, GoalStatus};
use crate::tools::{Tool, ToolCtx, ToolOutcome};
use serde_json::{Value, json};

fn ask_ctx_mode(mode: crate::core::prefs::ApprovalMode) -> ToolCtx {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session("g2", roots.workspace.clone(), None, vec![], None, vec![]);
    rt.set_prefs(crate::core::prefs::SessionPrefs {
        approval_mode: mode,
        model_id: None,
        reasoning_effort: None,
    });
    ToolCtx {
        core,
        rt,
        batch_id: "b".into(),
        call_index: 0,
        call_key: "b:0".into(),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

fn plan_ctx() -> ToolCtx {
    ask_ctx_mode(crate::core::prefs::ApprovalMode::Plan)
}

fn register_todos(ctx: &ToolCtx, titles: &[&str]) {
    *ctx.rt.todos.lock().unwrap() = titles
        .iter()
        .map(|t| crate::tools::plan::Todo {
            title: t.to_string(),
            status: crate::tools::plan::TodoStatus::Pending,
        })
        .collect();
}

#[test]
fn gate_rejects_empty_todos() {
    let ctx = plan_ctx();
    let err = plan_approval_gate(&ctx, false, false).expect("空 todos 应被拒");
    assert!(!err.ok);
    let dbg = format!("{:?}", err.error);
    assert!(dbg.contains("E_PLAN_TODOS_REQUIRED"), "{dbg}");
}

#[test]
fn gate_rejects_without_analysis() {
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    let err = plan_approval_gate(&ctx, false, false).expect("无分析应被拒");
    assert!(format!("{:?}", err.error).contains("E_PLAN_ANALYSIS_REQUIRED"));
    assert_eq!(
        ctx.rt
            .analysis_gate_denials
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[test]
fn gate_passes_with_analysis_marker() {
    let ctx = plan_ctx();
    register_todos(&ctx, &["a", "b"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(plan_approval_gate(&ctx, false, false).is_none());
}

#[test]
fn lightweight_exception_ignores_todo_count() {
    // 轻量豁免不再受条数上限约束：5 条 todos + lightweight 也放行
    let ctx = plan_ctx();
    register_todos(&ctx, &["a", "b", "c", "d", "e"]);
    assert!(plan_approval_gate(&ctx, true, false).is_none());
    // G3 非空硬要求不变：空 todos 仍拒绝
    let empty = plan_ctx();
    let err = plan_approval_gate(&empty, true, false).expect("空 todos 应被拒");
    assert!(format!("{:?}", err.error).contains("E_PLAN_TODOS_REQUIRED"));
}

#[test]
fn skip_phrase_requires_user_message_hit() {
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    // 模型凭空声明 skipAnalysis 而用户从未说过 → 拒绝
    let err = plan_approval_gate(&ctx, false, true).expect("口令未命中应被拒");
    assert!(format!("{:?}", err.error).contains("E_PLAN_ANALYSIS_REQUIRED"));
    // 最近一条用户消息命中口令 → 放行
    ctx.rt
        .history
        .lock()
        .unwrap()
        .push(crate::core::types::Message::user_text(
            "这个 typo 直接改，不用分析",
        ));
    assert!(plan_approval_gate(&ctx, false, true).is_none());
}

#[test]
fn skip_phrase_substring_without_analysis_context_rejected() {
    // Y3：闲聊里出现「直接改」但无分析语义同现 → 口令不认
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .history
        .lock()
        .unwrap()
        .push(crate::core::types::Message::user_text(
            "上次那个函数保持直接改的逻辑就好了",
        ));
    assert!(plan_approval_gate(&ctx, false, true).is_some());
}

#[test]
fn args_deserialize_camel_case_skip_analysis() {
    // R1 回归：schema key skipAnalysis（camelCase）必须反序列化进 skip_analysis 字段
    let args: Args = serde_json::from_value(json!({
        "questions": [{ "id": "q", "question": "?" }],
        "skipAnalysis": true,
        "lightweight": true,
    }))
    .expect("camelCase 反序列化必须成功");
    assert_eq!(args.skip_analysis, Some(true));

    // [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：计划批准形 ask → 完整方案落盘到 .codewave/tasks/plan-*.md
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "plan-file",
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
    let path = save_plan_file(&ctx, "## 改动点\n1. 调整 A\n2. 验证 B").expect("落盘应成功");
    assert!(
        path.contains(crate::core::config::MANAGED_DIR_NAME),
        "{path}"
    );
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("# 计划"), "{content}");
    assert!(content.contains("调整 A"), "{content}");
    // 落盘路径必须仍在工作区内（不可逃逸）
    assert!(path.starts_with(ctx.rt.workspace.to_string_lossy().as_ref()));
    assert_eq!(args.lightweight, Some(true));
}

/// [docs/session-cleanup](../../../../docs/session-cleanup.md)：计划文件落盘后登记归属（kind = plan）；
/// 临时会话（工作区 = 全局数据目录）的**双层路径** `.codewave/.codewave/tasks/` 被正确解析；
/// 右栏「文件」数据源不返回计划文件；会话清理时计划文件被连带删除。
#[test]
fn plan_file_registered_as_plan_artifact_with_double_layer_path() {
    let dd = tempfile::tempdir().unwrap();
    let data_root = std::fs::canonicalize(dd.path()).unwrap();
    // 临时会话：工作区根就是全局数据目录（既有的 workspace == data_dir 场景）
    let roots = crate::tools::pathutil::WriteRoots {
        workspace: data_root.clone(),
        extra: vec![],
        data_dir: data_root.clone(),
    };
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "temp-plan",
        data_root.clone(),
        None,
        vec![data_root.to_string_lossy().into_owned()],
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

    let path = save_plan_file(&ctx, "## 改动点").expect("落盘应成功");
    // 双层路径：工作区根已是全局数据目录，又拼了一层 .codewave —— 从家目录看就是
    // `~/.codewave/.codewave/tasks/plan-*.md`
    let expected_dir = data_root
        .join(crate::core::config::MANAGED_DIR_NAME)
        .join("tasks");
    assert!(
        path.starts_with(expected_dir.to_string_lossy().as_ref()),
        "临时会话计划文件应是数据目录下再套一层 .codewave（真实数据目录 ~/.codewave 时即双层路径），实际：{path}"
    );

    // 归属登记：计划文件进了边车且种类为 plan；右栏「文件」数据源不返回它
    let items = ctx.core.store.load_artifacts("temp-plan");
    assert_eq!(items.len(), 1, "计划文件必须登记归属");
    assert_eq!(items[0].kind, crate::core::sessions::ArtifactKind::Plan);
    assert_eq!(items[0].path, path);
    assert!(ctx.core.store.load_file_artifacts("temp-plan").is_empty());

    // 会话清理连带删除计划文件
    let now = chrono::Utc::now().to_rfc3339();
    let meta = crate::core::sessions::SessionMeta {
        id: "temp-plan".into(),
        title: "t".into(),
        workspace: data_root.to_string_lossy().into_owned(),
        model_id: None,
        created_at: now.clone(),
        updated_at: now,
        message_count: 0,
        project_id: None,
        roots: vec![data_root.to_string_lossy().into_owned()],
        running: false,
        interrupted: None,
        last_opened_at: None,
        history_status: None,
    };
    assert!(crate::core::sessions::cleanup::delete_session_files(
        &ctx.core.store,
        &data_root,
        &meta
    ));
    assert!(
        !std::path::Path::new(&path).exists(),
        "计划文件应随会话清理删除：{path}"
    );
    assert!(!ctx.core.store.artifacts_path("temp-plan").exists());
}

#[test]
fn args_deserialize_camel_case_switch_to_autoedit() {
    let args: Args = serde_json::from_value(json!({
        "questions": [{ "id": "q", "question": "?" }],
        "switchToAutoEdit": true,
    }))
    .expect("camelCase 反序列化必须成功");
    assert_eq!(args.switch_to_autoedit, Some(true));
    // 缺省为 None（不切档）
    let plain: Args = serde_json::from_value(json!({
        "questions": [{ "id": "q", "question": "?" }],
    }))
    .unwrap();
    assert_eq!(plain.switch_to_autoedit, None);
}

#[test]
fn wants_mode_switch_matrix() {
    use crate::core::prefs::ApprovalMode;
    // Plan 档：批准即切换（既有语义；valid_answer 不改变 Plan 协议）
    assert!(wants_mode_switch(
        ApprovalMode::Plan,
        true,
        false,
        false,
        false
    ));
    assert!(wants_mode_switch(
        ApprovalMode::Plan,
        true,
        true,
        true,
        false
    ));
    // ConfirmEach：arch 标记或有效应答（[docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)），任一通道都切；标记通道无需批准命中
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        true,
        true,
        false,
        false
    ));
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        false,
        true,
        false
    ));
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        true,
        false,
        false
    ));
    // 本就放开：无显式选档时不切
    assert!(!wants_mode_switch(
        ApprovalMode::AutoEdit,
        true,
        true,
        true,
        false
    ));
    assert!(!wants_mode_switch(
        ApprovalMode::FullAccess,
        true,
        true,
        true,
        false
    ));
    // 显式选档（选中带 mode 的批准类选项）：跨档允许——含 auto_edit → full_access 升级
    assert!(wants_mode_switch(
        ApprovalMode::AutoEdit,
        false,
        false,
        false,
        true
    ));
    assert!(wants_mode_switch(
        ApprovalMode::FullAccess,
        false,
        false,
        false,
        true
    ));
    // 未批准、无有效应答、无标记、无选档：永不切
    assert!(!wants_mode_switch(
        ApprovalMode::Plan,
        false,
        true,
        false,
        false
    ));
    assert!(!wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        false,
        false,
        false
    ));
}

#[test]
fn wants_mode_switch_valid_answer_docs35() {
    use crate::core::prefs::ApprovalMode;
    // [docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)：ConfirmEach 有任一有效应答即切换（无需标记/批准形）；任意应答不改变 Plan 协议
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        false,
        true,
        false
    ));
    assert!(!wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        false,
        false,
        false
    ));
    assert!(!wants_mode_switch(
        ApprovalMode::Plan,
        false,
        false,
        true,
        false
    ));
}

#[test]
fn has_valid_answer_three_states() {
    let qs = vec![
        Question {
            id: "q1".into(),
            question: "?".into(),
            options: vec![],
            single: false,
        },
        Question {
            id: "q2".into(),
            question: "?".into(),
            options: vec![],
            single: false,
        },
    ];
    // 全空（含忽略路径清空的载荷）→ 无效应答
    assert!(!has_valid_answer(
        &qs,
        &json!({"answers": {
            "q1": {"selections": [], "note": ""},
            "q2": {"selections": [], "note": ""}
        }})
    ));
    // 至少一题有 selections → 有效
    assert!(has_valid_answer(
        &qs,
        &json!({"answers": {"q1": {"selections": ["a"], "note": ""}}})
    ));
    // 至少一题 note 非空 → 有效；纯空白 note 不算
    assert!(has_valid_answer(
        &qs,
        &json!({"answers": {"q2": {"selections": [], "note": "用 B 方案"}}})
    ));
    assert!(!has_valid_answer(
        &qs,
        &json!({"answers": {"q1": {"selections": [], "note": "   "}}})
    ));
}

#[test]
fn plan_text_joins_all_questions() {
    let qs = vec![
        Question {
            id: "a".into(),
            question: "第一段".into(),
            options: vec![],
            single: false,
        },
        Question {
            id: "b".into(),
            question: "第二段".into(),
            options: vec![],
            single: false,
        },
    ];
    assert_eq!(plan_text(&qs), "第一段\n\n第二段");
}

#[test]
fn plan_body_prefers_explicit_plan_field() {
    // 修复回归：显式 plan 字段（trim 后非空）优先于题干拼接——此前模型把方案
    // 写在聊天消息、题干只放短问题时，计划文件只剩短题干（用户实测缺陷）
    let qs = vec![Question {
        id: "approve_plan".into(),
        question: "是否批准执行？".into(),
        options: vec![],
        single: true,
    }];
    let body = plan_body(Some("# 完整方案\n1. 改 A\n2. 验证 B"), &qs);
    assert!(body.contains("完整方案"), "{body}");
    assert!(!body.contains("是否批准"), "{body}");
}

#[test]
fn plan_body_falls_back_to_questions_when_plan_blank() {
    // 历史行为兼容：plan 缺失 / 纯空白（trim 后为空不采信，防占位符清空正文）→ 回退题干拼接
    let qs = vec![Question {
        id: "q".into(),
        question: "题干正文".into(),
        options: vec![],
        single: false,
    }];
    assert_eq!(plan_body(None, &qs), "题干正文");
    assert_eq!(plan_body(Some("   \n  "), &qs), "题干正文");
}

#[test]
fn args_deserialize_camel_case_plan() {
    // plan 字段（单词，无大小写歧义）：携带时反序列化进 Args.plan；缺省为 None
    let with_plan: Args = serde_json::from_value(json!({
        "questions": [{ "id": "q", "question": "?" }],
        "plan": "# 方案\n改动点...",
    }))
    .expect("plan 字段反序列化必须成功");
    assert_eq!(with_plan.plan.as_deref(), Some("# 方案\n改动点..."));

    let plain: Args = serde_json::from_value(json!({
        "questions": [{ "id": "q", "question": "?" }],
    }))
    .unwrap();
    assert_eq!(plain.plan, None);
}
#[test]
fn arch_gate_shape_requires_single_question_with_approve_option() {
    let q = |id: &str| Question {
        id: id.into(),
        question: "?".into(),
        options: vec![],
        single: false,
    };
    let mut approve_q = q("approve_plan");
    approve_q.options = vec![
        Option2 {
            id: "approve".into(),
            label: "批准开发".into(),
            description: None,
            recommended: true,
            mode: None,
        },
        Option2 {
            id: "revise".into(),
            label: "补充意见".into(),
            description: None,
            recommended: false,
            mode: None,
        },
    ];
    assert!(arch_gate_shape(&[approve_q.clone()]));
    // 多题 / 无 approve 选项 / 无选项 → 都不算批准闸形态
    assert!(!arch_gate_shape(&[approve_q.clone(), q("extra")]));
    let plain = q("info");
    assert!(!arch_gate_shape(&[plain]));
    let mut no_approve = q("pick");
    no_approve.options = vec![Option2 {
        id: "a".into(),
        label: "甲".into(),
        description: None,
        recommended: false,
        mode: None,
    }];
    assert!(!arch_gate_shape(&[no_approve]));
}

// ---------- [docs/ask-ignore-not-answered-fix](../../../../docs/ask-ignore-not-answered-fix.md)：忽略（全空应答）显式失败回归 ----------
// ---------- [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：批准形宽松检测（单一事实源） ----------

fn opt(id: &str, label: &str) -> Option2 {
    Option2 {
        id: id.into(),
        label: label.into(),
        description: None,
        recommended: false,
        mode: None,
    }
}

/// 批准类选项（C1）：带 mode 声明「选中后切到哪档」。
fn opt_mode(id: &str, label: &str, mode: crate::core::prefs::ApprovalMode) -> Option2 {
    Option2 {
        id: id.into(),
        label: label.into(),
        description: None,
        recommended: false,
        mode: Some(mode),
    }
}

#[test]
fn approve_option_lenient_matching() {
    assert!(is_approve_option(&opt("approve", "执行方案")));
    // 截图案例：label 合规、id 自拟（此前 approved_hit 只匹配
    // id 子串 → 批准静默失效）
    assert!(is_approve_option(&opt("execute", "执行方案")));
    assert!(is_approve_option(&opt("plan_ok", "Approve plan")));
    assert!(is_approve_option(&opt("x", "批准方案 A")));
    assert!(!is_approve_option(&opt("revise", "补充意见")));
    // label 匹配用完整短语，「执行测试」不会被误判为批准
    assert!(!is_approve_option(&opt("run_tests", "执行测试")));
}

#[test]
fn approval_shape_lenient_but_gate_strict() {
    let q = Question {
        id: "q".into(),
        question: "?".into(),
        options: vec![opt("execute", "执行方案"), opt("feedback", "补充意见")],
        single: false,
    };
    // 宽松形态：单题 + label 合规 → 批准形（前端渲染单选 + 直提）
    assert!(approval_shape(std::slice::from_ref(&q)));
    assert_eq!(
        approve_option_id(std::slice::from_ref(&q)).as_deref(),
        Some("execute")
    );
    // 安全边界：arch 闸严格形态不放宽——label 合规但
    // id ≠ approve 仍不算 arch 闸形态
    assert!(!arch_gate_shape(std::slice::from_ref(&q)));
    // 多题永不算批准形；严格形态（id=approve）两者皆满足
    let strict = Question {
        id: "q".into(),
        question: "?".into(),
        options: vec![opt("approve", "执行方案")],
        single: false,
    };
    assert!(!approval_shape(&[q, strict.clone()]));
    assert!(arch_gate_shape(std::slice::from_ref(&strict)));
    assert!(approval_shape(&[strict]));
}

/// 前端「忽略」动作提交的空载荷形态（AskPanel.ignoreCurrent → submitWith 清空 values）。
fn empty_answers_payload(qids: &[&str]) -> Value {
    let mut answers = serde_json::Map::new();
    for qid in qids {
        answers.insert((*qid).to_string(), json!({ "selections": [], "note": "" }));
    }
    json!({ "answers": answers })
}

/// 驱动完整 run() 流程：挂起后从 rt.asks 注册表取 ask_id，按前端 resolveAsk 的
/// 形态回注应答，再等待工具结果（spawn 侧重建 ToolCtx 共享 Arc 字段以绕开借用生命周期）。
async fn drive_ask_with_answer(ctx: &ToolCtx, args: Value, answer: Value) -> ToolOutcome {
    let tool_ctx = ToolCtx {
        core: ctx.core.clone(),
        rt: ctx.rt.clone(),
        batch_id: ctx.batch_id.clone(),
        call_index: ctx.call_index,
        call_key: ctx.call_key.clone(),
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    let handle = tokio::spawn(async move { AskTool.run(&tool_ctx, args).await });
    let mut ask_id = None;
    for _ in 0..500 {
        if let Some(k) = ctx.rt.asks.lock().unwrap().keys().next().cloned() {
            ask_id = Some(k);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let ask_id = ask_id.expect("ask 未在超时内注册");
    assert!(ctx.rt.resolve_ask(&ask_id, answer), "应答回注应命中挂起");
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("ask 工具超时未返回")
        .expect("ask 任务 panic")
}

#[tokio::test]
async fn ignored_single_question_returns_err() {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::AutoEdit);
    let args = json!({
        "questions": [{ "id": "q1", "question": "选择方案", "options": [
            { "id": "a", "label": "方案 A" },
            { "id": "b", "label": "方案 B" }
        ]}]
    });
    let outcome = drive_ask_with_answer(&ctx, args, empty_answers_payload(&["q1"])).await;
    assert!(!outcome.ok);
    let err = outcome.error.expect("忽略应返回错误");
    assert_eq!(err.code, "E_ASK_NOT_ANSWERED");
    assert!(err.message.contains("忽略"), "{}", err.message);
    assert!(err.message.contains("不要继续执行"), "{}", err.message);
}

#[tokio::test]
async fn ignored_all_empty_multi_question_returns_err() {
    // 多题末页忽略：全部题清空 → 与单题同走显式失败路径
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::AutoEdit);
    let args = json!({
        "questions": [
            { "id": "q1", "question": "第一问" },
            { "id": "q2", "question": "第二问" }
        ]
    });
    let outcome = drive_ask_with_answer(&ctx, args, empty_answers_payload(&["q1", "q2"])).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.expect("忽略应返回错误").code,
        "E_ASK_NOT_ANSWERED"
    );
}

#[tokio::test]
async fn partial_answer_still_ok_with_unanswered_tail() {
    // 部分应答行为不变：返回 ok，未答题保留「（未回答）」尾注
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::AutoEdit);
    let args = json!({
        "questions": [
            { "id": "q1", "question": "第一问" },
            { "id": "q2", "question": "第二问" }
        ]
    });
    let answer = json!({ "answers": {
        "q1": { "selections": ["a"], "note": "" },
        "q2": { "selections": [], "note": "" }
    }});
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "部分应答不应失败");
    let summary = outcome.data["summary"].as_str().expect("summary 应为文本");
    assert!(summary.contains("选定：a"), "{summary}");
    assert!(summary.contains("（未回答）"), "{summary}");
}

#[tokio::test]
async fn whitespace_note_counts_as_ignored() {
    // 纯空白 note 不算有效应答（与 has_valid_answer 的 trim 语义一致）
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::AutoEdit);
    let args = json!({ "questions": [{ "id": "q1", "question": "第一问" }] });
    let answer = json!({ "answers": { "q1": { "selections": [], "note": "   " } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.expect("空白 note 应按忽略失败").code,
        "E_ASK_NOT_ANSWERED"
    );
}

#[tokio::test]
async fn ignored_answer_keeps_confirm_each_mode() {
    // 忽略永不切档：ConfirmEach 下的空应答 → 失败且权限档不变
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    let args = json!({ "questions": [{ "id": "q1", "question": "第一问" }] });
    let outcome = drive_ask_with_answer(&ctx, args, empty_answers_payload(&["q1"])).await;
    assert!(!outcome.ok);
    let mode = ctx.rt.prefs().approval_mode;
    assert!(matches!(
        mode,
        crate::core::prefs::ApprovalMode::ConfirmEach
    ));
}

#[test]
fn question_single_field_serde_default_and_passthrough() {
    // 旧载荷（无 single）→ 默认 false（向前兼容）；显式 single 解析并回序列化
    let q: Question = serde_json::from_value(json!({
        "id": "q1", "question": "保留序号前缀还是彻底去掉？",
        "options": [ { "id": "keep", "label": "保留" }, { "id": "strip", "label": "去掉" } ]
    }))
    .expect("无 single 字段的旧载荷应可反序列化");
    assert!(!q.single);
    let q2: Question = serde_json::from_value(json!({
        "id": "q2", "question": "?", "single": true, "options": []
    }))
    .expect("single=true 应可反序列化");
    assert!(q2.single);
    // ask:opened 载荷透传：questions 整体序列化。注意 Serialize 没有
    // skip_serializing_if，旧载荷（无 single）经后端再发会带显式
    // "single": false——无害：前端按 === true 比较，false ≡ 缺省渲染。
    let back = serde_json::to_value(&q2).unwrap();
    assert_eq!(back["single"], json!(true));
}

#[test]
fn single_true_does_not_override_approval_shape() {
    // 批准优先：批准形问题上的 single=true 不应改变形态
    // 判定（批准形带切档副作用，其协议优先于 UI 声明）
    let approval_q = Question {
        id: "q".into(),
        question: "?".into(),
        options: vec![opt("execute", "执行方案"), opt("feedback", "补充意见")],
        single: true,
    };
    assert!(approval_shape(std::slice::from_ref(&approval_q)));
    // 主批准项：无 recommended 标记时按声明顺序取首个（推荐项优先另有专门用例）
    assert_eq!(approve_option_id(&[approval_q]).as_deref(), Some("execute"));
    // 非批准问题标 single 仍是非批准（该标记从不制造批准语义）
    let plain_single = Question {
        id: "p".into(),
        question: "?".into(),
        options: vec![opt("keep", "保留"), opt("strip", "去掉")],
        single: true,
    };
    assert!(!approval_shape(&[plain_single]));
}

#[tokio::test]
async fn label_matched_approve_still_switches_plan() {
    // [docs/ask-approval-shape-note-nav](../../../../docs/ask-approval-shape-note-nav.md)：id 自拟（execute）但 label 合规 → 批准命中不再静默失效，
    // Plan 档仍过门 → 冻结基线 → 切 AutoEdit → plan_approved
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{ "id": "q1", "question": "是否按此方案执行？", "options": [
            { "id": "execute", "label": "执行方案" },
            { "id": "feedback", "label": "补充意见" }
        ]}]
    });
    let answer = json!({ "answers": { "q1": { "selections": ["execute"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok);
    assert_eq!(outcome.data["plan_approved"], json!(true));
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::AutoEdit
    ));
}

// ---------- 批准门选档确认（C1-C6）：mode 结构化字段取代子串猜测 + 硬编码 AutoEdit ----------

/// 批准门标准形态（三选项）：两个批准类（带 mode）+ 补充意见；id="approve" 锚点保留。
fn gate_question() -> Question {
    use crate::core::prefs::ApprovalMode;
    Question {
        id: "approve_plan".into(),
        question: "是否按此方案执行？".into(),
        options: vec![
            Option2 {
                recommended: true,
                ..opt_mode("approve", "以自动编辑档执行", ApprovalMode::AutoEdit)
            },
            opt_mode("approve_full", "以完全访问档执行", ApprovalMode::FullAccess),
            opt("revise", "补充意见"),
        ],
        single: true,
    }
}

/// 批准门 ask 入参（JSON 形态，走完整 run() 流程）：switch_flag 控制 switchToAutoEdit。
fn approval_gate_args(switch_flag: bool) -> Value {
    json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按此方案执行？", "single": true,
            "options": [
                {
                    "id": "approve", "label": "以自动编辑档执行",
                    "mode": "auto_edit", "recommended": true
                },
                { "id": "approve_full", "label": "以完全访问档执行", "mode": "full_access" },
                { "id": "revise", "label": "补充意见" }
            ]
        }],
        "switchToAutoEdit": switch_flag,
    })
}

/// 批准门应答载荷（选中指定选项）。
fn gate_answer(id: &str) -> Value {
    json!({ "answers": { "approve_plan": { "selections": [id], "note": "" } } })
}

#[test]
fn option_mode_serde_default_and_wire_value() {
    // C1 向前兼容：旧载荷（无 mode）反序列化为 None
    let raw = json!({ "id": "approve", "label": "批准开发" });
    let legacy: Option2 = serde_json::from_value(raw).expect("无 mode 的旧载荷应可反序列化");
    assert_eq!(legacy.mode, None);
    // wire 值即 ApprovalMode 的 snake_case；回序列化保持同值
    let raw = json!({ "id": "approve_full", "label": "完全访问", "mode": "full_access" });
    let full: Option2 = serde_json::from_value(raw).expect("mode 应可反序列化");
    assert_eq!(
        full.mode,
        Some(crate::core::prefs::ApprovalMode::FullAccess)
    );
    let back = serde_json::to_value(&full).unwrap();
    assert_eq!(back["mode"], json!("full_access"));
    // 未知档位值不静默降级（serde 拒绝）
    let raw = json!({ "id": "x", "label": "x", "mode": "nope" });
    assert!(serde_json::from_value::<Option2>(raw).is_err());
}

/// 🟡6（修 1）：`mode: None` 不得序列化出 `"mode": null`——前端声明是 `mode?: ApprovalMode`，
/// 带 null 会让「mode !== undefined」这类判定误判（当前前端判定都是 truthy，行为对但类型在说谎）。
#[test]
fn option_mode_key_absent_when_none() {
    use crate::core::prefs::ApprovalMode;
    let plain = opt("revise", "补充意见");
    let v = serde_json::to_value(&plain).unwrap();
    assert!(v.get("mode").is_none(), "非批准类选项不得带 mode 键：{v}");
    assert!(!serde_json::to_string(&v).unwrap().contains("\"mode\""));
    // 批准类选项仍带 wire 值（ApprovalMode 的 snake_case）
    for (id, mode, wire) in [
        ("approve", ApprovalMode::AutoEdit, "auto_edit"),
        ("approve_full", ApprovalMode::FullAccess, "full_access"),
    ] {
        let v = serde_json::to_value(opt_mode(id, "x", mode)).unwrap();
        assert_eq!(v["mode"], json!(wire), "{id} 应带 mode");
    }
    // 忽略字段不破坏反序列化向后兼容
    let back: Option2 = serde_json::from_value(serde_json::to_value(&plain).unwrap()).unwrap();
    assert_eq!(back.mode, None);
}

#[test]
fn mode_bearing_option_is_approve_class() {
    use crate::core::prefs::ApprovalMode;
    let full = opt_mode("approve_full", "以完全访问档执行", ApprovalMode::FullAccess);
    assert!(is_approve_class(&full));
    // label 未用协议措辞也认（结构化字段优先）
    assert!(is_approve_option(&full));
    // 无 mode 的修订项仍非批准项
    assert!(!is_approve_class(&opt("revise", "补充意见")));
    assert!(!is_approve_option(&opt("revise", "补充意见")));
}

#[test]
fn approval_gate_shape_keeps_arch_anchor_and_primary_id() {
    let q = gate_question();
    // 批准形（前端渲染单选 + 批准项直提）；arch 闸形态靠保留的 id="approve" 锚点仍成立
    assert!(approval_shape(std::slice::from_ref(&q)));
    assert!(arch_gate_shape(std::slice::from_ref(&q)));
    // 主批准项 = 推荐项优先
    assert_eq!(
        approve_option_id(std::slice::from_ref(&q)).as_deref(),
        Some("approve")
    );
    assert_eq!(
        mode_label(crate::core::prefs::ApprovalMode::FullAccess),
        "完全访问"
    );
    assert_eq!(
        mode_label(crate::core::prefs::ApprovalMode::AutoEdit),
        "自动编辑"
    );
}

#[test]
fn approve_option_id_prefers_recommended_over_declaration_order() {
    use crate::core::prefs::ApprovalMode;
    // 完全访问档声明在前、自动编辑档（推荐）在后 → 主批准项仍是推荐项
    let q = Question {
        id: "approve_plan".into(),
        question: "?".into(),
        options: vec![
            opt_mode("approve_full", "以完全访问档执行", ApprovalMode::FullAccess),
            Option2 {
                recommended: true,
                ..opt_mode("approve", "以自动编辑档执行", ApprovalMode::AutoEdit)
            },
            opt("revise", "补充意见"),
        ],
        single: true,
    };
    assert_eq!(
        approve_option_id(std::slice::from_ref(&q)).as_deref(),
        Some("approve")
    );
}

#[test]
fn selected_target_mode_reads_selected_option_mode() {
    use crate::core::prefs::ApprovalMode;
    let q = gate_question();
    assert_eq!(
        selected_target_mode(std::slice::from_ref(&q), &gate_answer("approve_full")),
        Some(ApprovalMode::FullAccess)
    );
    assert_eq!(
        selected_target_mode(std::slice::from_ref(&q), &gate_answer("approve")),
        Some(ApprovalMode::AutoEdit)
    );
    // 修订项无 mode → None（调用点回落 AutoEdit）
    assert_eq!(
        selected_target_mode(std::slice::from_ref(&q), &gate_answer("revise")),
        None
    );
    // 旧形态（id=approve 无 mode）→ None：行为与改造前一致
    let legacy = Question {
        id: "q".into(),
        question: "?".into(),
        options: vec![opt("approve", "批准开发")],
        single: false,
    };
    let legacy_answer = json!({ "answers": { "q": { "selections": ["approve"], "note": "" } } });
    assert_eq!(selected_target_mode(&[legacy], &legacy_answer), None);
}

#[tokio::test]
async fn full_access_choice_switches_to_full_access_in_plan_mode() {
    // C3：目标档位按选中项声明（不再硬编码 AutoEdit）
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = approval_gate_args(true);
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve_full")).await;
    assert!(outcome.ok);
    assert_eq!(outcome.data["plan_approved"], json!(true));
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::FullAccess
    ));
    let summary = outcome.data["summary"].as_str().expect("summary 应为文本");
    assert!(summary.contains("会话已切换到完全访问模式"), "{summary}");
    assert!(summary.contains("请立即按方案执行"), "{summary}");
}

#[tokio::test]
async fn auto_edit_choice_switches_to_auto_edit() {
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = approval_gate_args(true);
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve")).await;
    assert!(outcome.ok);
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::AutoEdit
    ));
    let summary = outcome.data["summary"].as_str().expect("summary 应为文本");
    assert!(summary.contains("会话已切换到自动编辑模式"), "{summary}");
}

#[tokio::test]
async fn cross_tier_confirm_each_full_access_choice_switches() {
    // C5：ConfirmEach 档选中带 mode 的批准类选项 → 一次跳两档（Light 路径：不冻结基线）
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    let args = approval_gate_args(false);
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve_full")).await;
    assert!(outcome.ok);
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::FullAccess
    ));
    assert!(
        ctx.rt.approved_plan.lock().unwrap().is_none(),
        "Light 路径不冻结基线"
    );
}

#[tokio::test]
async fn confirm_each_arch_gate_full_access_freezes_baseline() {
    // arch 闸形态 + switchToAutoEdit → 完整路径：过 G2/G3 门、冻结基线、切到所选档
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    register_todos(&ctx, &["a", "b"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = approval_gate_args(true);
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve_full")).await;
    assert!(outcome.ok);
    assert_eq!(outcome.data["plan_approved"], json!(true));
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::FullAccess
    ));
    assert_eq!(
        ctx.rt
            .approved_plan
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| v.len()),
        Some(2)
    );
}

#[tokio::test]
async fn full_access_choice_still_blocked_by_empty_todos() {
    // C6：选完全访问档也仍要过批准门（G3 todos 非空）
    let ctx = plan_ctx();
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = approval_gate_args(true);
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve_full")).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.expect("空 todos 应被拒").code,
        "E_PLAN_TODOS_REQUIRED"
    );
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    ));
}

#[tokio::test]
async fn full_access_choice_still_blocked_without_analysis() {
    // C6：选完全访问档也仍要过 G2 分析产物门
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    let args = approval_gate_args(true);
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve_full")).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.expect("无分析应被拒").code,
        "E_PLAN_ANALYSIS_REQUIRED"
    );
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    ));
}

#[tokio::test]
async fn legacy_approve_without_mode_still_switches_to_auto_edit() {
    // 回归保护：旧形态（无 mode 字段）行为与改造前一致 → AutoEdit
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按此方案执行？",
            "options": [
                { "id": "approve", "label": "批准开发", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }]
    });
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve")).await;
    assert!(outcome.ok);
    assert_eq!(outcome.data["plan_approved"], json!(true));
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::AutoEdit
    ));
}

// ---------- 目标档批准分支（goal mode）：澄清期的唯一批准点 ----------

/// 目标状态（一条未达成的验收标准 + 非空账本）。
fn goal_state(status: GoalStatus) -> GoalState {
    GoalState {
        text: "把 X 改成 Y".into(),
        criteria: vec![GoalCriterion {
            title: "改完 X".into(),
            done: false,
        }],
        ledger: GoalLedger {
            paths: vec!["/work/proj/src".into()],
            programs: vec!["cargo".into()],
        },
        status,
        decisions: Vec::new(),
        pending: Vec::new(),
        blocked: Vec::new(),
        rounds: 0,
        stall_streak: 0,
        ledger_denials: 0,
    }
}

/// 目标档 ctx：登记一个目标（默认澄清期）。
fn goal_ctx(status: GoalStatus) -> ToolCtx {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::Goal);
    ctx.rt.set_goal(Some(goal_state(status)));
    ctx
}

/// 目标档批准形 ask（批准项声明 mode="goal"）。
fn goal_approval_args() -> Value {
    json!({
        "questions": [{
            "id": "approve_plan", "question": "目标与验收标准是否确认？", "single": true,
            "options": [
                { "id": "approve", "label": "确认并开始执行", "mode": "goal", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }]
    })
}

#[test]
fn goal_approval_verdict_matrix() {
    use crate::core::prefs::ApprovalMode;
    // 打开时已是目标档：批准才触发
    assert!(goal_approval(ApprovalMode::Goal, ApprovalMode::Goal, true));
    assert!(!goal_approval(
        ApprovalMode::Goal,
        ApprovalMode::Goal,
        false
    ));
    // 从其它档位一次切进目标档：同样走目标档分支
    assert!(goal_approval(ApprovalMode::Plan, ApprovalMode::Goal, true));
    assert!(goal_approval(
        ApprovalMode::ConfirmEach,
        ApprovalMode::Goal,
        true
    ));
    // 非目标档的批准不受影响（走各自的既有路径）
    assert!(!goal_approval(
        ApprovalMode::Plan,
        ApprovalMode::AutoEdit,
        true
    ));
    assert!(!goal_approval(
        ApprovalMode::AutoEdit,
        ApprovalMode::FullAccess,
        true
    ));
}

#[tokio::test]
async fn goal_approval_rejected_without_registered_goal() {
    // 没有合同的执行一律拒绝：未登记目标 → 批准不生效
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::Goal);
    let outcome = drive_ask_with_answer(&ctx, goal_approval_args(), gate_answer("approve")).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.expect("未登记目标应被拒").code,
        "E_GOAL_NOT_REGISTERED"
    );
    assert!(ctx.rt.goal_snapshot().is_none(), "拒绝不得登记目标");
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Goal
    ));
    assert!(ctx.rt.approved_plan.lock().unwrap().is_none());
}

#[tokio::test]
async fn goal_approval_rejected_without_criteria() {
    // 验收标准为空 = 没有可判定的合同 → 同样拒绝
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::Goal);
    let mut g = goal_state(GoalStatus::Clarify);
    g.criteria.clear();
    ctx.rt.set_goal(Some(g));
    let outcome = drive_ask_with_answer(&ctx, goal_approval_args(), gate_answer("approve")).await;
    assert!(!outcome.ok);
    assert_eq!(
        outcome.error.expect("无验收标准应被拒").code,
        "E_GOAL_NOT_REGISTERED"
    );
    assert_eq!(
        ctx.rt.goal_snapshot().unwrap().status,
        GoalStatus::Clarify,
        "拒绝不得推进阶段"
    );
}

#[tokio::test]
async fn goal_approval_enters_execute_without_plan_baseline() {
    let ctx = goal_ctx(GoalStatus::Clarify);
    // 登记 todos + 分析产物：若误入 plan 档批准协议，基线就会被冻结（G3 前置成立）
    register_todos(&ctx, &["a", "b"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = drive_ask_with_answer(&ctx, goal_approval_args(), gate_answer("approve")).await;
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(outcome.data["plan_approved"], json!(true));
    // 澄清期 → 执行期（内存 + 边车）
    assert_eq!(
        ctx.rt.goal_snapshot().unwrap().status,
        GoalStatus::Executing
    );
    assert_eq!(
        ctx.core.store.load_goal(&ctx.rt.id).unwrap().status,
        GoalStatus::Executing,
        "阶段推进必须落边车"
    );
    // G3 前置不成立：目标档批准不冻结计划基线（范围确认在目标档保持关闭）
    assert!(
        ctx.rt.approved_plan.lock().unwrap().is_none(),
        "目标档批准不得冻结 approved_plan 基线"
    );
    // 档位保持在用户所选的目标档
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Goal
    ));
    // 注入文案是目标模式语义（账本 + 勾标准），不是 plan 档原文
    let summary = outcome.data["summary"].as_str().expect("summary 应为文本");
    assert!(summary.contains("[system] 方案已批准"), "{summary}");
    assert!(summary.contains("执行期"), "{summary}");
    assert!(summary.contains("账本"), "{summary}");
    assert!(summary.contains("goal 工具"), "{summary}");
}

#[tokio::test]
async fn paused_goal_can_revise_ledger_then_reapprove() {
    let ctx = goal_ctx(GoalStatus::Paused);
    ctx.core
        .reopen_goal(&ctx.rt)
        .expect("暂停目标应能重开澄清期");
    assert_eq!(ctx.rt.goal_snapshot().unwrap().status, GoalStatus::Clarify);

    let revised = crate::tools::goal::GoalTool
        .run(
            &ctx,
            json!({"ledger": {"paths": ["/work/proj/src"], "programs": ["cargo", "mkdir"]}}),
        )
        .await;
    assert!(revised.ok, "{:?}", revised.error);
    assert_eq!(ctx.rt.goal_snapshot().unwrap().status, GoalStatus::Clarify);

    let approved = drive_ask_with_answer(&ctx, goal_approval_args(), gate_answer("approve")).await;
    assert!(approved.ok, "{:?}", approved.error);
    let goal = ctx.rt.goal_snapshot().unwrap();
    assert_eq!(goal.status, GoalStatus::Executing);
    assert!(
        goal.ledger
            .programs
            .iter()
            .any(|program| program == "mkdir")
    );
    assert_eq!(ctx.core.store.load_goal(&ctx.rt.id).unwrap(), goal);
}

#[tokio::test]
async fn goal_approval_with_arch_shape_skips_baseline_and_gate() {
    // 目标档下的批准形询问即便满足 arch 闸形态（单题 + id="approve" + switchToAutoEdit），
    // 也走目标档独立分支。不置 analysis_done：若误入 plan 档批准协议，G2 门会以
    // E_PLAN_ANALYSIS_REQUIRED 拦下（本用例断言批准成功 = 未走计划门）。
    let ctx = goal_ctx(GoalStatus::Clarify);
    register_todos(&ctx, &["a", "b"]);
    let args = json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按此方案执行？", "single": true,
            "options": [
                { "id": "approve", "label": "以自动编辑档执行", "mode": "auto_edit", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }],
        "switchToAutoEdit": true,
    });
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve")).await;
    assert!(outcome.ok, "{:?}", outcome.error);
    // 最终档位不是目标档（用户选了自动编辑档 = 退出目标模式）：状态**不迁移**，
    // 目标留在澄清期（既无执行授权，也不会被账本闸门误伤）。
    assert_eq!(
        ctx.rt.goal_snapshot().unwrap().status,
        GoalStatus::Clarify,
        "最终档位非目标档时不得推进阶段"
    );
    assert!(
        !matches!(
            ctx.core.store.load_goal(&ctx.rt.id),
            Some(crate::core::agent::goal::GoalState {
                status: GoalStatus::Executing,
                ..
            })
        ),
        "边车同样不得出现 executing"
    );
    let summary = outcome.data["summary"].as_str().expect("summary 应为文本");
    assert!(
        !summary.contains("已进入执行期"),
        "未进入执行期时不得声称已进入执行期：{summary}"
    );
    assert!(
        ctx.rt.approved_plan.lock().unwrap().is_none(),
        "G3 前置不成立：目标档批准不得冻结基线"
    );
    // 档位按用户所选切（mode 是结构化单一事实源）
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::AutoEdit
    ));
}

/// 🟡3：状态迁移只看**最终档位**——打开时是目标档且模型漏声明 mode 时会话没被切走，
/// 最终档位仍是目标档 → 批准必须推进执行期（这就是分支判据保留「打开时已是目标档」的意义）。
#[tokio::test]
async fn goal_approval_enters_execute_when_final_mode_stays_goal() {
    let ctx = goal_ctx(GoalStatus::Clarify);
    // 选项未声明 mode（模型漏写）：selected_mode 为空 → switch=false，档位保持目标档
    let args = json!({
        "questions": [{
            "id": "approve_plan", "question": "目标与验收标准是否确认？", "single": true,
            "options": [
                { "id": "approve", "label": "确认并开始执行", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }]
    });
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve")).await;
    assert!(outcome.ok, "{:?}", outcome.error);
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Goal
    ));
    assert_eq!(
        ctx.rt.goal_snapshot().unwrap().status,
        GoalStatus::Executing,
        "最终档位是目标档 → 必须推进执行期"
    );
}

#[tokio::test]
async fn plan_ask_choosing_goal_mode_enters_goal_execute() {
    // 从 plan 档一次切进目标档：同样走目标档分支（不冻结基线，目标推进执行期）
    let ctx = plan_ctx();
    register_todos(&ctx, &["a", "b"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    ctx.rt.set_goal(Some(goal_state(GoalStatus::Clarify)));
    let args = json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按此方案执行？", "single": true,
            "options": [
                { "id": "approve", "label": "以目标模式执行", "mode": "goal", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }]
    });
    let outcome = drive_ask_with_answer(&ctx, args, gate_answer("approve")).await;
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        ctx.rt.goal_snapshot().unwrap().status,
        GoalStatus::Executing
    );
    assert!(ctx.rt.approved_plan.lock().unwrap().is_none());
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Goal
    ));
}

#[tokio::test]
async fn goal_clarify_plain_question_keeps_status() {
    // 澄清期的普通提问（非批准类选项）完全不受影响
    let ctx = goal_ctx(GoalStatus::Clarify);
    let args = json!({
        "questions": [{ "id": "q1", "question": "用哪种方案？", "options": [
            { "id": "a", "label": "方案 A" },
            { "id": "b", "label": "方案 B" }
        ]}]
    });
    let answer = json!({ "answers": { "q1": { "selections": ["a"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "{:?}", outcome.error);
    assert_eq!(
        ctx.rt.goal_snapshot().unwrap().status,
        GoalStatus::Clarify,
        "普通提问不得推进阶段"
    );
    assert!(ctx.rt.approved_plan.lock().unwrap().is_none());
    assert!(outcome.data["plan_approved"].is_null(), "普通提问不是批准");
    let summary = outcome.data["summary"].as_str().expect("summary 应为文本");
    assert!(
        !summary.contains("[system]"),
        "普通提问不注入批准文案：{summary}"
    );
}

// ---------- [docs/preview-skill](../../../../docs/preview-skill.md)：批准门第三选项「先看预览」 ----------

/// 批准门三选项（[docs/preview-skill](../../../../docs/preview-skill.md)）：批准 / 补充意见 / 先看预览。
fn preview_gate_question() -> Question {
    Question {
        id: "approve_plan".into(),
        question: "是否按上述计划执行？".into(),
        options: vec![
            opt("approve", "执行方案"),
            opt("revise", "补充意见"),
            opt("preview", "先看预览"),
        ],
        single: true,
    }
}

#[test]
fn preview_option_is_not_an_approve_option() {
    // 预览项不得被宽松批准匹配命中（命中即静默批准 + 切档）
    assert!(!is_approve_option(&opt("preview", "先看预览")));
    assert!(!is_approve_option(&opt("preview", "Preview first")));
    // 三选项仍是批准形 / 批准闸形态（arch_gate_shape 只要求含 id="approve"）
    assert!(approval_shape(&[preview_gate_question()]));
    assert!(arch_gate_shape(&[preview_gate_question()]));
}

#[test]
fn preview_only_detection_excludes_other_answers() {
    let qs = vec![preview_gate_question()];
    let preview_answer =
        json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "" } } });
    assert!(has_valid_answer(&qs, &preview_answer)); // 通用判定仍是「有应答」
    assert!(preview_only(&qs, &preview_answer)); // 但它是「只看预览」

    // 选了别的项 / 预览+别的项：都不是「只看预览」→ 按普通有效应答处理
    for other in [
        json!({ "answers": { "approve_plan": { "selections": ["revise"], "note": "" } } }),
        json!({ "answers": { "approve_plan": { "selections": ["preview", "revise"], "note": "" } } }),
    ] {
        assert!(!preview_only(&qs, &other));
    }
    // 审查 R1：预览 + 补充说明仍是「只看预览」（补充说明不是表态；否则 ConfirmEach 批准门会静默批准）
    let preview_with_note =
        json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "顺便看看" } } });
    assert!(preview_only(&qs, &preview_with_note));

    // 非批准形询问里的同名选项不受影响（普通澄清问题不会被当成预览项）
    let plain = Question {
        id: "pick".into(),
        question: "选一个".into(),
        options: vec![opt("preview", "先看预览")],
        single: false,
    };
    let plain_answer = json!({ "answers": { "pick": { "selections": ["preview"], "note": "" } } });
    assert!(!preview_only(&[plain], &plain_answer));
}

/// 完整流水线批准门（ConfirmEach + switchToAutoEdit=true）里选「先看预览」：两条切档通道
///（arch 闸标记 / 有效应答）都不得生效——否则用户点预览会被静默批准并按方案开工。
#[tokio::test]
async fn preview_option_does_not_switch_mode_in_confirm_each_gate() {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{ "id": "approve_plan", "question": "是否按上述计划执行？", "options": [
            { "id": "approve", "label": "执行方案", "recommended": true },
            { "id": "revise", "label": "补充意见" },
            { "id": "preview", "label": "先看预览" }
        ]}],
        "switchToAutoEdit": true
    });
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null); // 不是批准
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::ConfirmEach
    ));
    // 模型侧文本如实记录「选定了 preview」，模型据此去加载 preview 技能
    let summary = outcome.data["summary"].as_str().unwrap_or("");
    assert!(summary.contains("preview"), "{summary}");
}

/// 对照：同一形状下选「执行方案」照旧切档（守门不得把正常批准路径改坏）。
#[tokio::test]
async fn approve_option_still_switches_mode_in_confirm_each_gate() {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{ "id": "approve_plan", "question": "是否按上述计划执行？", "options": [
            { "id": "approve", "label": "执行方案", "recommended": true },
            { "id": "revise", "label": "补充意见" },
            { "id": "preview", "label": "先看预览" }
        ]}],
        "switchToAutoEdit": true
    });
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["approve"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], json!(true));
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::AutoEdit
    ));
}

/// 批准门三选项的入参（标准工作流 S5 的形状：单题 + 含 approve 选项 + switchToAutoEdit）。
fn preview_gate_args() -> Value {
    json!({
        "questions": [{ "id": "approve_plan", "question": "是否按上述计划执行？", "options": [
            { "id": "approve", "label": "执行方案", "recommended": true },
            { "id": "revise", "label": "补充意见" },
            { "id": "preview", "label": "先看预览" }
        ]}],
        "switchToAutoEdit": true
    })
}

/// Plan 档：选「先看预览」不是批准（不切档、不 plan_approved、不注入）。
#[tokio::test]
async fn preview_option_is_not_approved_in_plan_mode() {
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, preview_gate_args(), answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null);
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    ));
}

/// 审查 R1 回归：预览项 + 补充说明仍是「只看预览」——ConfirmEach 批准门不得静默批准与切档。
#[tokio::test]
async fn preview_with_note_does_not_switch_mode_in_confirm_each_gate() {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let answer = json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "顺便看下界面" } } });
    let outcome = drive_ask_with_answer(&ctx, preview_gate_args(), answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null);
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::ConfirmEach
    ));
}

/// 批准候选剔除专项：预览项即使 label 含「执行方案」（宽松批准匹配会命中），
/// 也不得被当成批准项（既不能下发成 `approve_option_id`，也不能让 Plan 档静默批准）。
#[tokio::test]
async fn preview_option_with_approve_like_label_is_not_treated_as_approval() {
    // 前提：这份 label 确实会被 is_approve_option 命中（所以必须靠剔除，而非靠措辞）
    assert!(is_approve_option(&opt("preview", "执行方案预览")));
    let mut q = preview_gate_question();
    q.options = vec![
        opt("preview", "执行方案预览"), // 排在前面：不剔除就会被选为「首个批准项」
        opt("approve", "执行方案"),
        opt("revise", "补充意见"),
    ];
    assert_eq!(
        approve_option_id(std::slice::from_ref(&q)).as_deref(),
        Some("approve"),
        "预览项不得被选为批准候选"
    );

    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{ "id": "approve_plan", "question": "是否按上述计划执行？", "options": [
            { "id": "preview", "label": "执行方案预览" },
            { "id": "approve", "label": "执行方案", "recommended": true },
            { "id": "revise", "label": "补充意见" }
        ]}],
        "switchToAutoEdit": true
    });
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null);
    assert!(matches!(
        ctx.rt.prefs().approval_mode,
        crate::core::prefs::ApprovalMode::Plan
    ));
}

/// 预览项识别：id 大小写不敏感，且模型自拟 id 时按 label（「先看预览」/「Preview first」）兜底。
#[test]
fn preview_option_recognition_is_case_insensitive_and_label_backed() {
    let mut q = preview_gate_question();
    q.options = vec![opt("approve", "执行方案"), opt("Preview", "先看预览")];
    let qs = vec![q];
    assert!(preview_option_ids(&qs).contains("Preview"));
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["Preview"], "note": "" } } });
    assert!(preview_only(&qs, &answer));

    // 自拟 id + 合规 label：仍认得出（否则会落回有效应答 → 静默切档）
    let mut q2 = preview_gate_question();
    q2.options = vec![
        opt("approve", "执行方案"),
        opt("preview_plan", "先看预览"),
        opt("preview_en", "Preview first"),
    ];
    let qs2 = vec![q2];
    let ids = preview_option_ids(&qs2);
    assert!(
        ids.contains("preview_plan") && ids.contains("preview_en"),
        "{ids:?}"
    );
    let a2 =
        json!({ "answers": { "approve_plan": { "selections": ["preview_plan"], "note": "" } } });
    assert!(preview_only(&qs2, &a2));
}

/// 相邻修复（[docs/preview-skill](../../../../docs/preview-skill.md) §3.4）：批准闸（单题 + id="approve"）上只有真选中批准项
/// 才动档位——选「补充意见」既不批准（无 plan_approved / 无「立即执行」注入 / 不冻结基线），也不切档。
#[tokio::test]
async fn revise_option_does_not_switch_mode_in_confirm_each_gate() {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let answer = json!({ "answers": { "approve_plan": { "selections": ["revise"], "note": "风险那节写细一点" } } });
    let outcome = drive_ask_with_answer(&ctx, preview_gate_args(), answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null); // 不是批准
    let summary = outcome.data["summary"].as_str().unwrap_or("");
    assert!(!summary.contains("方案已批准"), "{summary}");
    assert!(
        ctx.rt.approved_plan.lock().unwrap().is_none(),
        "不得冻结 todos 基线"
    );
    assert!(
        matches!(
            ctx.rt.prefs().approval_mode,
            crate::core::prefs::ApprovalMode::ConfirmEach
        ),
        "批准闸上选「补充意见」不得动档位"
    );
}

// ---------- 融合后的批准门四选项（main 显式选档 × 本分支 preview 第四项）：交叉用例 ----------
// 合并前两侧各自只测过三选项形态（main：approve / approve_full / revise；本分支：approve / revise / preview），
// 融合后四选项并存——本节补「preview 与带 mode 的档位选项互不干扰」的交叉验证。

/// 融合后的批准门标准形态（四选项）：approve(auto_edit, recommended) / approve_full(full_access) / revise / preview。
fn fused_gate_question() -> Question {
    use crate::core::prefs::ApprovalMode;
    Question {
        id: "approve_plan".into(),
        question: "是否按上述计划执行？".into(),
        options: vec![
            Option2 {
                recommended: true,
                ..opt_mode("approve", "以自动编辑档执行", ApprovalMode::AutoEdit)
            },
            opt_mode("approve_full", "以完全访问档执行", ApprovalMode::FullAccess),
            opt("revise", "补充意见"),
            opt("preview", "先看预览"),
        ],
        single: true,
    }
}

/// 四选项批准门的 ask 入参（JSON 形态，走完整 run() 流程）。
fn fused_gate_args(switch_flag: bool) -> Value {
    json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按上述计划执行？", "single": true,
            "options": [
                { "id": "approve", "label": "以自动编辑档执行", "mode": "auto_edit", "recommended": true },
                { "id": "approve_full", "label": "以完全访问档执行", "mode": "full_access" },
                { "id": "revise", "label": "补充意见" },
                { "id": "preview", "label": "先看预览" }
            ]
        }],
        "switchToAutoEdit": switch_flag,
    })
}

/// 四选项批准门里的 preview 应答载荷。
fn fused_preview_answer() -> Value {
    json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "" } } })
}

/// 交叉用例 1：四选项门里选 preview —— **两个起点档位各测一次**：既不批准（无 plan_approved、不冻结基线），
/// 也不切档（档位与选前一致）。既有 preview 用例只覆盖三选项形态（preview_gate_args 里没有带 mode 的选项）。
#[tokio::test]
async fn preview_choice_neither_approves_nor_switches_from_either_mode() {
    use crate::core::prefs::ApprovalMode;
    for start in [ApprovalMode::ConfirmEach, ApprovalMode::Plan] {
        let ctx = ask_ctx_mode(start);
        // 门内条件齐备（todos + 分析产物）：若 preview 被误判成批准，会真的走到批准路径而不是被 G2/G3 拦住——
        // 让「误批准」可观测（否则会被门拒掩盖成假绿）。
        register_todos(&ctx, &["a"]);
        ctx.rt
            .analysis_done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome =
            drive_ask_with_answer(&ctx, fused_gate_args(true), fused_preview_answer()).await;
        assert!(outcome.ok, "{start:?}: {outcome:?}");
        assert_eq!(
            outcome.data["plan_approved"],
            Value::Null,
            "{start:?} 选 preview 不得批准"
        );
        assert!(
            ctx.rt.approved_plan.lock().unwrap().is_none(),
            "{start:?} 选 preview 不得冻结 todos 基线"
        );
        assert_eq!(
            ctx.rt.prefs().approval_mode,
            start,
            "{start:?} 选 preview 不得动档位"
        );
        // 模型侧文本如实记录「选定了 preview」，模型据此去加载 preview 技能
        let summary = outcome.data["summary"].as_str().unwrap_or("");
        assert!(summary.contains("preview"), "{start:?}: {summary}");
    }
}

/// 交叉用例 2a：四选项门里 preview 不进批准候选——主批准项是带 mode 的推荐项 `approve`。
#[test]
fn fused_gate_primary_approve_id_skips_preview() {
    let q = fused_gate_question();
    assert!(approval_shape(std::slice::from_ref(&q)));
    assert!(arch_gate_shape(std::slice::from_ref(&q)));
    assert_eq!(
        approve_option_id(std::slice::from_ref(&q)).as_deref(),
        Some("approve"),
        "四选项门的主批准项应是带 mode 的推荐项，而不是 preview"
    );
}

/// 交叉用例 2b（异常形态）：把 preview 的 label 写成批准类措辞（宽松匹配会命中）并标 recommended、
/// 且声明在首位——它仍不得被选为批准候选，也不得让 `approved` 为真。
#[tokio::test]
async fn fused_gate_preview_with_approve_like_label_never_becomes_approval() {
    use crate::core::prefs::ApprovalMode;
    // 前提：这份 label 确实会被宽松批准匹配命中（所以必须靠剔除，而非靠措辞）
    assert!(is_approve_option(&opt("preview", "执行方案预览")));
    let mut q = fused_gate_question();
    q.options = vec![
        // preview 声明在首位 + 也标 recommended：不剔除就会被 min_by_key 选为主批准项
        Option2 {
            recommended: true,
            ..opt("preview", "执行方案预览")
        },
        Option2 {
            recommended: true,
            ..opt_mode("approve", "以自动编辑档执行", ApprovalMode::AutoEdit)
        },
        opt_mode("approve_full", "以完全访问档执行", ApprovalMode::FullAccess),
        opt("revise", "补充意见"),
    ];
    assert_eq!(
        approve_option_id(std::slice::from_ref(&q)).as_deref(),
        Some("approve"),
        "preview 即便 label 批准样 + recommended + 声明在首位，也不得成为批准候选"
    );
    // 选中它不产生批准意图（无 mode 声明 → selected_target_mode 为 None）
    assert_eq!(
        selected_target_mode(std::slice::from_ref(&q), &fused_preview_answer()),
        None
    );

    // 完整流程复核（Plan 档，门内条件齐备）：选 preview 不批准、不切档、不冻结基线
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按上述计划执行？", "single": true,
            "options": [
                { "id": "preview", "label": "执行方案预览", "recommended": true },
                { "id": "approve", "label": "以自动编辑档执行", "mode": "auto_edit", "recommended": true },
                { "id": "approve_full", "label": "以完全访问档执行", "mode": "full_access" },
                { "id": "revise", "label": "补充意见" }
            ]
        }],
        "switchToAutoEdit": true
    });
    let outcome = drive_ask_with_answer(&ctx, args, fused_preview_answer()).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null);
    assert!(
        ctx.rt.approved_plan.lock().unwrap().is_none(),
        "不得冻结 todos 基线"
    );
    assert_eq!(ctx.rt.prefs().approval_mode, ApprovalMode::Plan);
}

/// 交叉用例 3：preview 的识别仍限批准形询问——非批准形询问里的同名选项不被认作预览项。
/// 单题普通询问已由 `preview_only_detection_excludes_other_answers`（本文件末尾之前一处）覆盖，
/// 这里补多题形态（多题永不算批准形）。
#[test]
fn preview_recognition_stays_limited_to_approval_shape() {
    let multi = vec![
        Question {
            id: "q1".into(),
            question: "选一个".into(),
            options: vec![opt("preview", "先看预览"), opt("a", "甲")],
            single: false,
        },
        Question {
            id: "q2".into(),
            question: "再选一个".into(),
            options: vec![opt("b", "乙")],
            single: false,
        },
    ];
    assert!(!approval_shape(&multi));
    assert!(preview_option_ids(&multi).is_empty());
    let answer = json!({ "answers": {
        "q1": { "selections": ["preview"], "note": "" },
        "q2": { "selections": ["b"], "note": "" }
    }});
    assert!(!preview_only(&multi, &answer));
}

/// 交叉用例 4：四选项门下 approve / approve_full / revise 三条对照——选带 mode 的项即批准且切到
/// 该项声明的档位；选 revise 不批准、档位不变。
#[tokio::test]
async fn fused_gate_choices_match_declared_modes() {
    use crate::core::prefs::ApprovalMode;
    // (选中项, 期望档位, 是否批准)
    for (choice, expect_mode, expect_approved) in [
        ("approve", ApprovalMode::AutoEdit, true),
        ("approve_full", ApprovalMode::FullAccess, true),
        ("revise", ApprovalMode::Plan, false),
    ] {
        let ctx = plan_ctx();
        register_todos(&ctx, &["a"]);
        ctx.rt
            .analysis_done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome = drive_ask_with_answer(&ctx, fused_gate_args(true), gate_answer(choice)).await;
        assert!(outcome.ok, "{choice}: {outcome:?}");
        if expect_approved {
            assert_eq!(outcome.data["plan_approved"], json!(true), "{choice}");
            assert!(
                ctx.rt.approved_plan.lock().unwrap().is_some(),
                "{choice} 应冻结 todos 基线"
            );
        } else {
            assert_eq!(outcome.data["plan_approved"], Value::Null, "{choice}");
            assert!(
                ctx.rt.approved_plan.lock().unwrap().is_none(),
                "{choice} 不得冻结 todos 基线"
            );
        }
        assert_eq!(ctx.rt.prefs().approval_mode, expect_mode, "{choice}");
    }
}

/// 疑点钉死：`approval_shape`（tool.rs:451-453）是宽松渲染标记，注释明确「无安全语义」，且**不剔除 preview**——
/// preview 自身的 id/label 命中 `is_approve_option`（如 label 含「执行方案」）时，`approval` 标记会被它点亮。
/// 这里把安全边界钉死：即便如此，preview 也 ① 不进批准候选（approve_id = None）、
/// ② 不让 approved 为真（选它不批准、不切档、不冻结基线）。
/// 注意：**不得**把 approval_shape 收紧成「剔除 preview」——`preview_option_ids`（tool.rs:491-494）以它为前置，
/// 收紧会让「preview 是唯一批准样选项」这一形态失去预览识别 → preview_only 变假 →
/// ConfirmEach 的 valid_answer 通道（tool.rs:318-326）会把档位静默抬到 AutoEdit。
#[tokio::test]
async fn preview_only_approve_like_option_never_becomes_approval() {
    use crate::core::prefs::ApprovalMode;
    // 前提：这份 label 确实会被宽松批准匹配命中
    assert!(is_approve_option(&opt("preview", "执行方案预览")));
    let q = Question {
        id: "approve_plan".into(),
        question: "是否按上述计划执行？".into(),
        options: vec![
            Option2 {
                recommended: true,
                ..opt("preview", "执行方案预览")
            },
            opt("revise", "补充意见"),
        ],
        single: true,
    };
    // 宽松渲染标记：单题 + 有批准样选项 → 批准形（前端据此渲染单选 + 直提；无安全语义）
    assert!(approval_shape(std::slice::from_ref(&q)));
    // 但 preview 不进批准候选 → 前端拿到的 approve_id 为 None（不会把 preview 当批准项直提）
    assert_eq!(approve_option_id(std::slice::from_ref(&q)).as_deref(), None);
    // 选中它也不产生批准意图
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["preview"], "note": "" } } });
    assert_eq!(
        selected_target_mode(std::slice::from_ref(&q), &answer),
        None
    );
    assert!(preview_only(std::slice::from_ref(&q), &answer));

    // 完整流程复核（Plan 档，门内条件齐备）：选 preview 不批准、不切档、不冻结基线
    let ctx = plan_ctx();
    register_todos(&ctx, &["a"]);
    ctx.rt
        .analysis_done
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let args = json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按上述计划执行？", "single": true,
            "options": [
                { "id": "preview", "label": "执行方案预览", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }],
        "switchToAutoEdit": true
    });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null);
    assert!(
        ctx.rt.approved_plan.lock().unwrap().is_none(),
        "不得冻结 todos 基线"
    );
    assert_eq!(ctx.rt.prefs().approval_mode, ApprovalMode::Plan);
}

/// 非闸形状的普通 ConfirmEach 询问：任一有效应答照旧轻量切档（既有语义不得被上一处的收紧带坏）。
#[tokio::test]
async fn non_gate_ask_still_light_switches_in_confirm_each() {
    let ctx = ask_ctx_mode(crate::core::prefs::ApprovalMode::ConfirmEach);
    let args = json!({
        "questions": [{ "id": "pick", "question": "选一个", "options": [
            { "id": "a", "label": "甲" },
            { "id": "b", "label": "乙" }
        ]}]
    });
    let answer = json!({ "answers": { "pick": { "selections": ["b"], "note": "" } } });
    let outcome = drive_ask_with_answer(&ctx, args, answer).await;
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(outcome.data["plan_approved"], Value::Null);
    assert!(
        matches!(
            ctx.rt.prefs().approval_mode,
            crate::core::prefs::ApprovalMode::AutoEdit
        ),
        "非闸形状询问的有效应答仍应轻量切档"
    );
}

// ---------- 宽松批准通道的预览剔除（安全：单边静默提权）----------
// 批准判定有两条通道：结构化通道（选中项声明 mode → selected_target_mode）与宽松兜底通道
// （选中项 id 子串匹配 approve/执行 + 批准候选 id）。`approve_ids` 已剔除预览项，宽松子串通道
// 此前**没有**：模型把「先看预览」的 id 自拟成含 approve 的形态（如 `preview_approve`）时，
// 「选先看预览」会被宽松通道算成批准，与 preview 语义（不批准 / 不切档 / 不冻结基线）直接冲突；
// 前端的宽松匹配已剔除预览项，后端不剔除就是「前端不切档、后端切档」的单边静默提权。

/// 自拟 id 的预览项批准门（label 守约「先看预览」，id 含 `approve` 子串 → 宽松子串匹配会命中）。
fn preview_approve_like_gate_args(single: bool) -> Value {
    json!({
        "questions": [{
            "id": "approve_plan", "question": "是否按上述计划执行？", "single": single,
            "options": [
                { "id": "preview_approve", "label": "先看预览" },
                { "id": "approve", "label": "以自动编辑档执行", "mode": "auto_edit", "recommended": true },
                { "id": "revise", "label": "补充意见" }
            ]
        }],
        "switchToAutoEdit": true
    })
}

/// 同上形态的 Question 视图（纯函数断言用）。
fn preview_approve_like_question(single: bool) -> Question {
    use crate::core::prefs::ApprovalMode;
    Question {
        id: "approve_plan".into(),
        question: "是否按上述计划执行？".into(),
        options: vec![
            opt("preview_approve", "先看预览"),
            opt_mode("approve", "以自动编辑档执行", ApprovalMode::AutoEdit),
            opt("revise", "补充意见"),
        ],
        single,
    }
}

/// 自拟 id 的预览项：选中它不批准、不切档、不冻结基线（逐项确认档 / 计划档两个起点各一次）。
#[tokio::test]
async fn preview_approve_like_id_choice_never_approves_from_either_mode() {
    use crate::core::prefs::ApprovalMode;
    // 前提：这份 id 会被宽松子串匹配命中（含 approve），label 兜底仍认它是预览项
    assert!(is_approve_option(&opt("preview_approve", "先看预览")));
    let q = preview_approve_like_question(true);
    assert!(preview_option_ids(std::slice::from_ref(&q)).contains("preview_approve"));
    let answer =
        json!({ "answers": { "approve_plan": { "selections": ["preview_approve"], "note": "" } } });
    assert!(preview_only(std::slice::from_ref(&q), &answer));
    // 结构化通道天然不批准（预览项不带 mode）
    assert_eq!(
        selected_target_mode(std::slice::from_ref(&q), &answer),
        None
    );

    for start in [ApprovalMode::ConfirmEach, ApprovalMode::Plan] {
        let ctx = ask_ctx_mode(start);
        // 门内条件齐备：万一被误判成批准会真的走到批准路径，而不是被 G2/G3 拦住掩盖成假绿
        register_todos(&ctx, &["a"]);
        ctx.rt
            .analysis_done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome =
            drive_ask_with_answer(&ctx, preview_approve_like_gate_args(true), answer.clone()).await;
        assert!(outcome.ok, "{start:?}: {outcome:?}");
        assert_eq!(
            outcome.data["plan_approved"],
            Value::Null,
            "{start:?} 选先看预览不得批准"
        );
        assert!(
            ctx.rt.approved_plan.lock().unwrap().is_none(),
            "{start:?} 不得冻结 todos 基线"
        );
        assert_eq!(ctx.rt.prefs().approval_mode, start, "{start:?} 不得动档位");
        let summary = outcome.data["summary"].as_str().unwrap_or("");
        assert!(summary.contains("preview_approve"), "{start:?}: {summary}");
    }
}

/// **判别力用例**：预览项与别的项同时被选中 → `preview_only` 不成立，
/// 此时唯一拦住「选先看预览被算成批准」的就是宽松通道里的预览剔除。
/// 去掉剔除逻辑：ConfirmEach 起点会被轻量切到自动编辑档、Plan 起点会直接批准并冻结基线（本用例变红）。
#[tokio::test]
async fn preview_approve_like_id_with_other_selection_never_approves_from_either_mode() {
    use crate::core::prefs::ApprovalMode;
    let q = preview_approve_like_question(false);
    let answer = json!({ "answers": { "approve_plan": { "selections": ["preview_approve", "revise"], "note": "" } } });
    // 前提：多选载荷不算「只看预览」——preview_only 这道双保险在这里不生效
    assert!(!preview_only(std::slice::from_ref(&q), &answer));
    assert!(preview_only(
        std::slice::from_ref(&q),
        &json!({ "answers": { "approve_plan": { "selections": ["preview_approve"], "note": "" } } })
    ));

    for start in [ApprovalMode::ConfirmEach, ApprovalMode::Plan] {
        let ctx = ask_ctx_mode(start);
        register_todos(&ctx, &["a"]);
        ctx.rt
            .analysis_done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome =
            drive_ask_with_answer(&ctx, preview_approve_like_gate_args(false), answer.clone())
                .await;
        assert!(outcome.ok, "{start:?}: {outcome:?}");
        assert_eq!(
            outcome.data["plan_approved"],
            Value::Null,
            "{start:?}：预览项不得借宽松子串通道被算成批准"
        );
        assert!(
            ctx.rt.approved_plan.lock().unwrap().is_none(),
            "{start:?} 不得冻结 todos 基线"
        );
        assert_eq!(ctx.rt.prefs().approval_mode, start, "{start:?} 不得动档位");
    }
}

/// 对照（防过度剔除）：同一个门里选真正的批准项（带 mode 的 `approve`）仍照常批准 + 切到 auto_edit。
#[tokio::test]
async fn real_approve_option_still_approves_next_to_preview_approve_like_id() {
    use crate::core::prefs::ApprovalMode;
    for start in [ApprovalMode::ConfirmEach, ApprovalMode::Plan] {
        let ctx = ask_ctx_mode(start);
        register_todos(&ctx, &["a"]);
        ctx.rt
            .analysis_done
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let answer =
            json!({ "answers": { "approve_plan": { "selections": ["approve"], "note": "" } } });
        let outcome =
            drive_ask_with_answer(&ctx, preview_approve_like_gate_args(true), answer).await;
        assert!(outcome.ok, "{start:?}: {outcome:?}");
        assert_eq!(
            outcome.data["plan_approved"],
            json!(true),
            "{start:?} 真批准项仍应批准"
        );
        assert!(
            ctx.rt.approved_plan.lock().unwrap().is_some(),
            "{start:?} 应冻结 todos 基线"
        );
        assert_eq!(
            ctx.rt.prefs().approval_mode,
            ApprovalMode::AutoEdit,
            "{start:?} 应切到所选档位"
        );
    }
}
