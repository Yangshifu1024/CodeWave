//! #3 验证策略夹具：固定矩阵、平台缺口不冒充通过、lint 顶不掉 build、
//! GUI 保持未验证、项目命令模板不直接执行、锁文件使旧结论失效。

use super::*;
use crate::core::delivery::types::RunIdentity;
use crate::core::workflow::routing::{TaskKind, TaskProfile, to_work_units};

fn unit(rel: &str) -> WorkUnit {
    WorkUnit {
        design_input: Default::default(),
        id: "u1".into(),
        requirement_ids: vec!["r1".into()],
        read_scopes: vec![],
        write_scopes: vec![PathRef {
            root_id: "ws".into(),
            relative_path: rel.into(),
        }],
        depends_on: vec![],
        mutable_resources: vec![],
        role_id: None,
        model_id: None,
        checks: vec![],
        stage_id: None,
    }
}

fn ids(checks: &[CheckSpec]) -> Vec<&str> {
    checks.iter().map(|c| c.id.as_str()).collect()
}

#[test]
fn backend_only_change_suggests_backend_gate() {
    let c = suggest_checks(&unit("src-tauri/src/core/x.rs"));
    assert_eq!(ids(&c), vec!["backend_test"]);
    // 后端检查的 cwd 指向 src-tauri。
    let t = c.iter().find(|c| c.id == "backend_test").unwrap();
    assert_eq!(t.cwd.as_ref().unwrap().relative_path, "src-tauri");
    assert!(t.independent_verifier);
    assert!(
        matches!(t.command.as_ref(),Some(CommandSpec::Requested{command_text,shell_hint:None}) if command_text == "cargo test --workspace")
    );
}

#[test]
fn frontend_change_suggests_test_build_lint_and_manual_gui() {
    let c = suggest_checks(&unit("ui/src/App.tsx"));
    assert_eq!(
        ids(&c),
        vec![
            "frontend_test",
            "frontend_build",
            "frontend_lint",
            "frontend_manual_gui"
        ]
    );
    assert!(c.iter().all(|c| c.required));
}

#[test]
fn docs_change_does_not_pull_in_backend_or_frontend_gates() {
    let c = suggest_checks(&unit("docs/plan-and-approval.md"));
    assert_eq!(ids(&c), vec!["docs_linkcheck"]);
}

#[test]
fn cross_layer_change_takes_the_union() {
    let mut u = unit("src-tauri/src/core/x.rs");
    u.write_scopes.push(PathRef {
        root_id: "ws".into(),
        relative_path: "ui/src/App.tsx".into(),
    });
    let c = suggest_checks(&u);
    let got = ids(&c);
    for want in [
        "backend_test",
        "frontend_test",
        "frontend_build",
        "frontend_lint",
        "frontend_manual_gui",
    ] {
        assert!(got.contains(&want), "跨层改动缺少 {want}");
    }
}

#[test]
fn mechanical_change_reuses_exact_assertion_instead_of_the_whole_suite() {
    let c = suggest_checks(&unit("ui/src/theme/app.css"));
    // CSS 归前端面，仍要跑整套 UI 检查；真正的机械面见下一个用例。
    let c2 = suggest_checks(&unit("VERSION"));
    assert_eq!(ids(&c2), vec!["mechanical_exact"]);
    let _ = c;
}

#[test]
fn unknown_extension_falls_back_to_the_mechanical_path() {
    let c = suggest_checks(&unit("scripts/run.sh"));
    assert_eq!(ids(&c), vec!["mechanical_exact"]);
    let _ = c;
}

#[test]
fn a_missing_platform_keeps_the_check_unverified_not_passed() {
    let t = CheckTemplate {
        id: "win_only".into(),
        surface: ChangeSurface::Backend,
        command_text: "cargo test --target x86_64-pc-windows-msvc".into(),
        cwd: Some("src-tauri".into()),
        manual: false,
        platforms: vec!["windows".into()],
        required: true,
    };
    let mac = runnability(&t, "macos", true, true);
    assert!(matches!(
        mac,
        Runnability::Blocked {
            reason: "unverified",
            ..
        }
    ));
    let win = runnability(&t, "windows", true, true);
    assert_eq!(win, Runnability::Runnable);
}

#[test]
fn missing_toolchain_or_approval_blocks_as_unverified() {
    let t = &builtin_templates()[0];
    assert!(matches!(
        runnability(t, "macos", false, true),
        Runnability::Blocked {
            reason: "unverified",
            ..
        }
    ));
    assert!(matches!(
        runnability(t, "macos", true, false),
        Runnability::Blocked {
            reason: "unverified",
            ..
        }
    ));
    assert_eq!(runnability(t, "macos", true, true), Runnability::Runnable);
}

#[test]
fn frontend_lint_passing_never_substitutes_for_build() {
    // lint 与 build 是**两条独立必跑检查**：lint 绿不构成 build 绿。
    let c = suggest_checks(&unit("ui/src/App.tsx"));
    let lint = c.iter().find(|c| c.id == "frontend_lint").unwrap();
    let build = c.iter().find(|c| c.id == "frontend_build").unwrap();
    assert_ne!(lint.id, build.id);
    assert!(lint.required && build.required);
}

#[test]
fn gui_check_stays_manual_and_is_never_auto_run() {
    let c = suggest_checks(&unit("ui/src/App.tsx"));
    let gui = c.iter().find(|c| c.id == "frontend_manual_gui").unwrap();
    assert_eq!(gui.kind, CheckKind::Manual);
    assert!(gui.command.is_none(), "人工走查不得被宿主代跑");
    assert!(gui.required);
}

#[test]
fn lock_files_are_in_scope_so_stale_detection_has_an_anchor() {
    let mut u = unit("src-tauri/src/core/x.rs");
    u.write_scopes.push(PathRef {
        root_id: "ws".into(),
        relative_path: "ui/src/App.tsx".into(),
    });
    let c = suggest_checks(&u);
    let deps: Vec<String> = c[0]
        .scope
        .dependency_files
        .iter()
        .map(|p| p.relative_path.clone())
        .collect();
    assert!(deps.iter().any(|d| d.contains("Cargo.lock")));
    assert!(deps.iter().any(|d| d.contains("pnpm-lock")));
}

#[test]
fn command_templates_come_only_from_the_builtin_table() {
    // 模板表是固定字面量；没有任何入口接受「从 README 抄来的命令」当模板。
    for t in builtin_templates() {
        assert!(
            t.command_text.starts_with("cargo ")
                || t.command_text.starts_with("pnpm ")
                || t.command_text.starts_with("git diff")
                || t.manual,
            "模板 `{}` 不是宿主内置形态：{}",
            t.id,
            t.command_text
        );
    }
}

#[test]
fn pr_gate_is_a_separate_check_not_a_replacement_for_the_suite() {
    let c = suggest_checks(&unit("src-tauri/src/core/x.rs"));
    assert!(!ids(&c).contains(&"prepr_full_gate"));
    assert!(
        builtin_templates()
            .iter()
            .any(|t| t.id == "prepr_full_gate")
    );
    assert!(ids(&c).contains(&"backend_test"));
}

#[test]
fn suggestion_summary_lands_in_data_and_lists_check_ids() {
    let c = suggest_checks(&unit("src-tauri/src/core/x.rs"));
    let s = suggestion_summary(&c);
    assert_eq!(s["checks"][0]["id"], "backend_test");
    assert!(
        s.get("warnings").is_none(),
        "结论必须在 data，warnings 模型收不到"
    );
}

#[test]
fn conversation_tasks_generate_no_write_units_or_checks() {
    let profile = TaskProfile {
        kind: TaskKind::Conversation,
        ..Default::default()
    };
    let units = to_work_units(&profile, &RunIdentity::root("s", "r"), &["r1".into()]);
    assert!(units.is_empty());
    assert!(units.iter().all(|u| suggest_checks(u).is_empty()));
}
