use super::*;
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
    assert!(wants_mode_switch(ApprovalMode::Plan, true, false, false));
    assert!(wants_mode_switch(ApprovalMode::Plan, true, true, true));
    // ConfirmEach：arch 标记或有效应答（[docs/notification-click-reveal](../../../../docs/notification-click-reveal.md)），任一通道都切；标记通道无需批准命中
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        true,
        true,
        false
    ));
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        false,
        true
    ));
    assert!(wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        true,
        false
    ));
    // 本就放开：不切
    assert!(!wants_mode_switch(ApprovalMode::AutoEdit, true, true, true));
    assert!(!wants_mode_switch(
        ApprovalMode::FullAccess,
        true,
        true,
        true
    ));
    // 未批准、无有效应答、无标记：永不切
    assert!(!wants_mode_switch(ApprovalMode::Plan, false, true, false));
    assert!(!wants_mode_switch(
        ApprovalMode::ConfirmEach,
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
        true
    ));
    assert!(!wants_mode_switch(
        ApprovalMode::ConfirmEach,
        false,
        false,
        false
    ));
    assert!(!wants_mode_switch(ApprovalMode::Plan, false, false, true));
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
        },
        Option2 {
            id: "revise".into(),
            label: "补充意见".into(),
            description: None,
            recommended: false,
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
