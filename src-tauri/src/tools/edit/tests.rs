    use super::*;
    use crate::tools::{Tool, ToolCtx};

    /// [docs/arithmetic-audit](../../../../docs/arithmetic-audit.md)#9：min_indent 必须按字符计数（与 strip_indent 的 chars().skip(n) 口径一致）——
    /// 按字节统计会把 U+00A0 等多字节空白过度剥除伤及内容。
    #[test]
    fn indent_units_chars_consistent_for_multibyte_space() {
        let block = "\u{00a0}\u{00a0}hello\n\u{00a0}\u{00a0}world";
        let stripped = strip_indent(block, min_indent(block));
        assert_eq!(stripped, "hello\nworld");
    }

    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：编辑成功即登记产物（Edit op）；子代理 runtime 归属主会话。
    #[tokio::test]
    async fn edit_registers_artifact_with_root_owner() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
        // 模拟子代理 runtime：root_session_id 指向主会话
        let sub = crate::core::agent::SessionRuntime::new_sub(&rt, "sub-1".to_string());
        assert_eq!(sub.root_session_id.as_deref(), Some("t"));
        let ctx = ToolCtx {
            core: core.clone(),
            rt: sub,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let f = ws.path().join("doc.md");
        std::fs::write(&f, "hello\n").unwrap();
        let tool = EditTool;
        let args = serde_json::json!({"files":[{"path":"doc.md","changes":[{"oldText":"hello","newText":"world"}]}]});
        let out = tool.run(&ctx, args).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "world\n");
        // 子代理写入归属主会话 "t"，而非 "sub-1"
        let items = core.store.load_artifacts("t");
        assert_eq!(items.len(), 1);
        assert!(items[0].path.ends_with("doc.md"));
        assert_eq!(items[0].first_op, crate::core::sessions::ArtifactOp::Edit);
        assert_eq!(items[0].last_op, crate::core::sessions::ArtifactOp::Edit);
        assert_eq!(items[0].count, 1);
        assert!(core.store.load_artifacts("sub-1").is_empty());
        // 再次经绝对路径编辑同一文件：规范化后仍去重为一条，count=2
        let abs = f.to_string_lossy().into_owned();
        let args2 = serde_json::json!({"files":[{"path": abs,"changes":[{"oldText":"world","newText":"hello"}]}]});
        let out2 = tool.run(&ctx, args2).await;
        assert!(out2.ok, "{out2:?}");
        let items = core.store.load_artifacts("t");
        assert_eq!(items.len(), 1, "相对/绝对路径写同一文件必须去重为一条");
        assert_eq!(items[0].count, 2);
    }

    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：task runtime（is_task_runtime）跳过产物登记，无边车泄漏。
    #[tokio::test]
    async fn task_runtime_skips_artifact_registration() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = crate::core::agent::SessionRuntime::new_task(
            "task_t1".into(),
            roots.data_dir.clone(),
            roots.workspace.clone(),
            roots.data_dir.clone(),
        );
        assert!(rt.is_task_runtime);
        let ctx = ToolCtx {
            core: core.clone(),
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let f = ws.path().join("doc.md");
        std::fs::write(&f, "hello\n").unwrap();
        let tool = EditTool;
        let args = serde_json::json!({"files":[{"path":"doc.md","changes":[{"oldText":"hello","newText":"world"}]}]});
        let out = tool.run(&ctx, args).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "world\n");
        assert!(core.store.load_artifacts("task_t1").is_empty());
        assert!(!dd.path().join("sessions/task_t1.artifacts.json").exists());
    }

    #[test]
    fn unique_oldtext_replaces() {
        let s = "fn a() {}\nfn b() {}\n";
        let ch = Change {
            old_text: Some("fn b() {}".into()),
            line_range: None,
            new_text: Some("fn c() {}".into()),
        };
        let (out, w) = apply_changes(s, &[ch]).unwrap();
        assert_eq!(out, "fn a() {}\nfn c() {}\n");
        assert!(w.is_empty(), "精确命中不应产生 warnings");
    }

    #[test]
    fn ambiguous_rejected() {
        let s = "x\nx\n";
        let ch = Change {
            old_text: Some("x".into()),
            line_range: None,
            new_text: Some("y".into()),
        };
        assert!(apply_changes(s, &[ch]).is_err());
    }

    #[test]
    fn line_range_replaces_block() {
        let s = "1\n2\n3\n4\n";
        let ch = Change {
            old_text: None,
            line_range: Some("2-3".into()),
            new_text: Some("a\nb\n".into()),
        };
        assert_eq!(apply_changes(s, &[ch]).unwrap().0, "1\na\nb\n4\n");
    }

    #[test]
    fn version_detects_change() {
        let v1 = version_of(b"hello");
        let v2 = version_of(b"hello!");
        assert_ne!(v1, v2);
        assert_eq!(v1.len(), 6);
    }

    #[tokio::test]
    async fn full_edit_flow_with_rollback() {
        let ws = tempfile::tempdir().unwrap();
        let p = ws.path().join("f.txt");
        std::fs::write(&p, b"one\ntwo\n").unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
        let ctx = ToolCtx {
            core: core.clone(),
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let tool = EditTool;
        let v = version_of(b"one\ntwo\n");
        let args = serde_json::json!({
            "files": [{"path": "f.txt", "version": v, "changes": [{"oldText": "two", "newText": "deux"}]}]
        });
        let out = tool.run(&ctx, args).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read(&p).unwrap(), b"one\ndeux\n");

        // stale version（仅过期但可唯一命中 → 警告放行）
        let args = serde_json::json!({
            "files": [{"path": "f.txt", "version": "AAAAAA", "changes": [{"oldText": "one", "newText": "x"}]}]
        });
        let out = tool.run(&ctx, args).await;
        assert!(out.ok, "stale 但可唯一命中应成功: {out:?}");
        assert!(out
            .warnings
            .iter()
            .any(|w| w.contains("version 令牌已过期")));
        assert_eq!(std::fs::read(&p).unwrap(), b"x\ndeux\n");

        // stale version + oldText 也无法命中（真实内容冲突）→ 此时才要求重新 read
        let args = serde_json::json!({
            "files": [{"path": "f.txt", "version": "AAAAAA", "changes": [{"oldText": "不存在的文本", "newText": "y"}]}]
        });
        let out = tool.run(&ctx, args).await;
        assert_eq!(out.error.unwrap().code, "E_VERSION_STALE");
    }

    // ===== EOL 归一 + 两级模糊匹配 + 杂项检查（[docs/tools-optimization-and-gap-fill-plan](../../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 1）=====

    fn test_ctx(
        core: std::sync::Arc<crate::core::agent::AgentCore>,
        rt: std::sync::Arc<crate::core::agent::SessionRuntime>,
    ) -> ToolCtx {
        ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn fresh_fixture(name: &str) -> (tempfile::TempDir, std::sync::Arc<crate::core::agent::AgentCore>, std::sync::Arc<crate::core::agent::SessionRuntime>, crate::tools::pathutil::WriteRoots) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(name, roots.workspace.clone(), None, vec![], None, vec![]);
        (ws, core, rt, roots)
    }

    /// CRLF 文件 + 多行 LF oldText：归一后命中，写回保持 CRLF。
    #[tokio::test]
    async fn crlf_file_edited_with_lf_oldtext_roundtrip() {
        let (ws, core, rt, _roots) = fresh_fixture("crlf");
        let ctx = test_ctx(core, rt);
        let f = ws.path().join("c.txt");
        std::fs::write(&f, b"one\r\ntwo\r\n").unwrap();
        let out = EditTool
            .run(&ctx, serde_json::json!({"files":[{"path":"c.txt","changes":[
                {"oldText":"one\ntwo","newText":"uno\ndos"}]}]}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read(&f).unwrap(), b"uno\r\ndos\r\n", "写回必须保持原 CRLF");
    }

    /// 缩进平移：oldText 块处于不同嵌套层级 → 缩进平移归一唯一命中 + 警告。
    #[test]
    fn fuzzy_indent_shift_matches_uniquely() {
        let content = "fn main() {\n    if x {\n        a();\n    }\n}\nrest\n";
        let ch = Change {
            old_text: Some("if x {\n    a();\n}".into()),
            line_range: None,
            new_text: Some("if x {\n    b();\n}".into()),
        };
        let (out, w) = apply_changes(content, &[ch]).unwrap();
        assert_eq!(out, "fn main() {\nif x {\n    b();\n}\n}\nrest\n");
        assert!(w.iter().any(|x| x.contains("缩进平移归一")), "{w:?}");
    }

    /// 行尾空白漂移：缩进平移归一不中（块内空白不同），行首尾空白归一命中 + 警告。
    #[test]
    fn fuzzy_line_trailing_whitespace_matches() {
        let content = "start\nbeta \ngamma\nend\n";
        let ch = Change {
            old_text: Some("beta\ngamma".into()),
            line_range: None,
            new_text: Some("BETA\nGAMMA".into()),
        };
        let (out, w) = apply_changes(content, &[ch]).unwrap();
        assert_eq!(out, "start\nBETA\nGAMMA\nend\n");
        assert!(w.iter().any(|x| x.contains("行首尾空白归一")), "{w:?}");
    }

    /// 模糊匹配多处命中：报错要求加长上下文（绝不瞎猜）。
    #[test]
    fn fuzzy_ambiguous_rejected() {
        let content = " a\n a\n";
        // "a " 精确不命中（内容中 a 后无空格），行首尾空白归一后命中两处 → 拒绝
        let ch = Change { old_text: Some("a ".into()), line_range: None, new_text: Some("b".into()) };
        let err = apply_changes(content, &[ch]).unwrap_err();
        assert!(err.contains("模糊匹配到 2 处"), "{err}");
    }

    /// oldText 与 newText 相同：拒绝（对齐 opencode 的 "No changes to apply"）。
    #[test]
    fn oldtext_equal_newtext_rejected() {
        let ch = Change { old_text: Some("x".into()), line_range: None, new_text: Some("x".into()) };
        let err = apply_changes("x\n", &[ch]).unwrap_err();
        assert!(err.contains("oldText 与 newText 相同"), "{err}");
    }

    /// lineRange 整段替换第一行时，UTF-8 BOM 交接给新首行（防止 BOM 丢失）。
    #[test]
    fn bom_transferred_on_line1_line_range() {
        let content = "\u{FEFF}hello\nworld\n";
        let ch = Change { old_text: None, line_range: Some("1-1".into()), new_text: Some("first\n".into()) };
        let (out, _) = apply_changes(content, &[ch]).unwrap();
        assert_eq!(out, "\u{FEFF}first\nworld\n");
    }

    /// 模糊命中端到端：警告回传模型。
    #[tokio::test]
    async fn fuzzy_match_warning_surfaces_in_outcome() {
        let (ws, core, rt, _roots) = fresh_fixture("fuzzy");
        let ctx = test_ctx(core, rt);
        let f = ws.path().join("fz.txt");
        std::fs::write(&f, b"fn main() {\n    if x {\n        a();\n    }\n}\n").unwrap();
        let out = EditTool
            .run(&ctx, serde_json::json!({"files":[{"path":"fz.txt","changes":[
                {"oldText":"if x {\n    a();\n}","newText":"if x {\n    b();\n}"}]}]}))
            .await;
        assert!(out.ok, "{out:?}");
        assert!(out.warnings.iter().any(|w| w.contains("缩进平移归一")), "{:?}", out.warnings);
        let after = std::fs::read_to_string(&f).unwrap();
        assert!(after.contains("b();"), "{after}");
    }

    // ===== 审批 diff 预览（[docs/tools-optimization-and-gap-fill-plan](../../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 3）=====

    #[test]
    fn approval_detail_renders_unified_diff() {
        let (ws, _core, _rt, roots) = fresh_fixture("apd");
        std::fs::write(ws.path().join("f.txt"), "alpha\nbeta\n").unwrap();
        let files = vec![FileEdit {
            path: "f.txt".into(),
            version: None,
            changes: vec![Change {
                old_text: Some("beta".into()),
                line_range: None,
                new_text: Some("gamma".into()),
            }],
        }];
        let detail = edit_approval_detail(&roots, &files).unwrap();
        assert!(detail.contains("### f.txt"), "{detail}");
        assert!(detail.contains("-beta"), "{detail}");
        assert!(detail.contains("+gamma"), "{detail}");
        assert!(detail.contains(" alpha"), "{detail}"); // 上下文行
    }

    #[test]
    fn approval_detail_none_when_apply_fails() {
        let (ws, _core, _rt, roots) = fresh_fixture("apd2");
        std::fs::write(ws.path().join("f.txt"), "alpha\n").unwrap();
        let files = vec![FileEdit {
            path: "f.txt".into(),
            version: None,
            changes: vec![Change {
                old_text: Some("不存在的文本".into()),
                line_range: None,
                new_text: Some("x".into()),
            }],
        }];
        assert!(edit_approval_detail(&roots, &files).is_none());
        // 文件不存在同样返回 None
        let files2 = vec![FileEdit {
            path: "nope.txt".into(),
            version: None,
            changes: vec![Change {
                old_text: Some("x".into()),
                line_range: None,
                new_text: Some("y".into()),
            }],
        }];
        assert!(edit_approval_detail(&roots, &files2).is_none());
    }

    // ===== 进程级写互斥（[docs/tools-optimization-and-gap-fill-plan](../../../../docs/tools-optimization-and-gap-fill-plan.md) 工作项 2）=====

    /// 两个 runtime（主 + 子代理）并发编辑同一文件的不同片段：锁内重读保证两处变更都落地。
    /// 若无进程级锁：双方基于同一初始内容试运行，后写覆盖先写，丢失一处变更。
    #[tokio::test]
    async fn concurrent_runtimes_editing_same_file_serialize() {
        let (ws, core, rt, _roots) = fresh_fixture("race");
        std::fs::write(ws.path().join("r.txt"), b"aaa\nbbb\n").unwrap();
        let sub = crate::core::agent::SessionRuntime::new_sub(&rt, "sub-race".to_string());
        let ctx1 = test_ctx(core.clone(), rt);
        let ctx2 = test_ctx(core, sub);
        let t1 = tokio::spawn(async move {
            EditTool
                .run(&ctx1, serde_json::json!({"files":[{"path":"r.txt","changes":[
                    {"oldText":"aaa","newText":"AAA"}]}]}))
                .await
        });
        let t2 = tokio::spawn(async move {
            EditTool
                .run(&ctx2, serde_json::json!({"files":[{"path":"r.txt","changes":[
                    {"oldText":"bbb","newText":"BBB"}]}]}))
                .await
        });
        let (o1, o2) = (t1.await.unwrap(), t2.await.unwrap());
        assert!(o1.ok, "{o1:?}");
        assert!(o2.ok, "{o2:?}");
        assert_eq!(
            std::fs::read(ws.path().join("r.txt")).unwrap(),
            b"AAA\nBBB\n",
            "并发编辑两处变更都必须生效（锁内重读串行化）"
        );
    }

    // ===== 用法信息自纠 + 无歧义错位形态 salvage（会话卡死修复：GLM 把 path 提到
    // files 外层 / 漏在元素内，E_ARGS 只回显 serde 原文导致原样重发死循环）=====

    /// 模型可见三层之一/二：schema 的 files/path/changes 节点与 description() 必须带结构说明。
    #[test]
    fn schema_and_description_document_files_shape() {
        let schema: serde_json::Value = serde_json::from_str(EditTool.schema()).unwrap();
        let files = &schema["properties"]["files"];
        let files_desc = files["description"].as_str().unwrap_or_default();
        assert!(
            files_desc.contains("\"path\"") && files_desc.contains("changes"),
            "files 节点 description 必须内嵌单元素结构示例，实际：{files_desc}"
        );
        assert!(
            !files["items"]["properties"]["path"]["description"].is_null(),
            "path 字段必须有 description（此前是裸 string，GLM 据此错位）"
        );
        let d = EditTool.description();
        assert!(d.contains("files 是数组") && d.contains("path"), "description 必须含形状句：{d}");
    }

    /// salvage 只碰两种可唯一还原的形态；正常形态与歧义形态（path 与 files 并存）一律不动。
    #[test]
    fn salvage_args_only_touches_unambiguous_shapes() {
        // 正常形态：不触碰
        let normal = serde_json::json!({"files":[{"path":"a","changes":[{"oldText":"x","newText":"y"}]}]});
        assert!(salvage_args(&normal).is_none());
        // 形态 1：顶层扁平单文件（path + changes、无 files）
        let flat = serde_json::json!({"path":"a","changes":[{"oldText":"x","newText":"y"}]});
        let (fixed, note) = salvage_args(&flat).expect("扁平形态必须被还原");
        assert_eq!(fixed["files"].as_array().unwrap().len(), 1);
        assert_eq!(fixed["files"][0]["path"], "a");
        assert!(note.contains("files 数组"), "{note}");
        // 顶层只有 path 没有 changes：无法确认是 edit 形态，不动
        assert!(salvage_args(&serde_json::json!({"path":"a"})).is_none());
        // 形态 2：files 是对象而非数组
        let obj = serde_json::json!({"files":{"path":"a","changes":[{"oldText":"x","newText":"y"}]}});
        let (fixed2, _) = salvage_args(&obj).expect("files 为对象必须被还原");
        assert_eq!(fixed2["files"].as_array().unwrap().len(), 1);
        // 歧义：path 与 files 并存——不得猜测，交给 E_ARGS 示例报错
        assert!(salvage_args(&serde_json::json!({"path":"a","files":[]})).is_none());
    }

    /// 模型可见三层之三：E_ARGS 报错必须附期望结构示例（可自纠），歧义形态不 salvage。
    #[tokio::test]
    async fn e_args_error_includes_expected_structure_example() {
        let (ws, core, rt, _roots) = fresh_fixture("eargs");
        let ctx = test_ctx(core, rt);
        std::fs::write(ws.path().join("doc.md"), "hello\n").unwrap();
        let tool = EditTool;
        // 元素内缺 path：serde missing field，报错须带示例
        let out = tool
            .run(&ctx, serde_json::json!({"files":[
                {"version":"aaaaaa","changes":[{"oldText":"hello","newText":"world"}]}]}))
            .await;
        assert!(!out.ok, "{out:?}");
        let err = out.error.expect("must be error");
        assert_eq!(err.code, "E_ARGS");
        assert!(err.message.contains("期望结构"), "{:?}", err.message);
        assert!(err.message.contains("files 数组每个元素内"), "{:?}", err.message);
        assert!(err.message.contains("\"path\""), "示例必须含 path 字段样例");
        // 歧义形态（顶层 path 与 files 并存）：不 salvage，走示例报错
        let out2 = tool
            .run(&ctx, serde_json::json!({"path":"doc.md","files":[
                {"changes":[{"oldText":"hello","newText":"world"}]}]}))
            .await;
        assert_eq!(out2.error.expect("must be error").code, "E_ARGS");
    }

    /// 形态 1 端到端：顶层扁平单文件直接被还原执行（不再 E_ARGS 循环），且模型通道
    /// （extra_model_content）与前端（warnings）都收到修正提示。
    #[tokio::test]
    async fn flat_single_file_args_salvaged_and_applied() {
        let (ws, core, rt, _roots) = fresh_fixture("salvage_flat");
        let ctx = test_ctx(core, rt);
        let f = ws.path().join("doc.md");
        std::fs::write(&f, "hello\n").unwrap();
        let out = EditTool
            .run(&ctx, serde_json::json!({"path":"doc.md","changes":[
                {"oldText":"hello","newText":"world"}]}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "world\n");
        assert!(
            out.warnings.iter().any(|w| w.contains("自动包装")),
            "前端 warnings 须含修正提示：{:?}",
            out.warnings
        );
        assert!(
            out.extra_model_content.iter().any(|c| matches!(
                c,
                crate::core::types::Content::Text { text } if text.contains("自动包装")
            )),
            "模型通道须含修正提示：{:?}",
            out.extra_model_content
        );
    }

    /// 形态 2 端到端：files 写成对象时包装为单元素数组并正常应用。
    #[tokio::test]
    async fn files_as_object_salvaged_and_applied() {
        let (ws, core, rt, _roots) = fresh_fixture("salvage_obj");
        let ctx = test_ctx(core, rt);
        let f = ws.path().join("doc.md");
        std::fs::write(&f, "world\n").unwrap();
        let out = EditTool
            .run(&ctx, serde_json::json!({"files":{"path":"doc.md","changes":[
                {"oldText":"world","newText":"hello"}]}}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello\n");
        assert!(out.warnings.iter().any(|w| w.contains("自动包装")), "{out:?}");
    }
