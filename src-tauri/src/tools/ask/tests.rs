    use super::*;
    use crate::tools::{Tool, ToolCtx, ToolOutcome};
    use serde_json::{json, Value};

    fn ask_ctx_mode(mode: crate::core::prefs::ApprovalMode) -> ToolCtx {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("g2", roots.workspace.clone(), None, vec![], None, vec![]);
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
        assert_eq!(approve_option_id(std::slice::from_ref(&q)).as_deref(), Some("execute"));
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
        let outcome =
            drive_ask_with_answer(&ctx, args, empty_answers_payload(&["q1", "q2"])).await;
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
