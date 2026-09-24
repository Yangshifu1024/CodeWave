use super::*;
// 本文件全部用例 cfg(unix)：trait 导入需同步 cfg——Windows 下 cfg(unix) 用例整体剔除后报 unused import、
// macOS 上则因缺此导入 E0599（CommandTool.run 不可见）——基线曾因 Windows 端用例剔除而漏检
#[cfg(unix)]
use crate::tools::Tool as _;
use crate::tools::ToolCtx;

#[cfg(unix)]
#[tokio::test]
async fn echo_runs() {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = super::super::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
    // 机制测试显式设 AutoEdit：默认 Plan 档的只读白名单会把 `exit 3` 送进审批超时（审批链路有专门测试）
    rt.set_prefs(mode_prefs(crate::core::prefs::ApprovalMode::AutoEdit));
    let ctx = ToolCtx {
        core: core.clone(),
        rt,
        batch_id: "b".into(),
        call_index: 0,
        call_key: "b:0".into(),
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    let tool = CommandTool;
    let out = tool
        .run(&ctx, serde_json::json!({"command": "echo hello-ws"}))
        .await;
    assert!(out.ok, "{out:?}");
    assert_eq!(out.data["exit_code"], 0);
    assert!(out.data["output"].as_str().unwrap().contains("hello-ws"));

    // 非零退出码
    let out = tool
        .run(&ctx, serde_json::json!({"command": "exit 3"}))
        .await;
    assert!(!out.ok);
    assert_eq!(out.data["exit_code"], 3);
}

#[test]
fn shell_detection_returns_something() {
    assert!(!shell_description(None).is_empty());
}

// ===== shell 全量探测 / 选择解析 / 启动拼接（存在性一律注入，不依赖本机环境）=====

/// shell_meta：全部 SHELL_IDS 的展示名 / kind / limited 契约。
#[test]
fn shell_meta_contract() {
    let expect = [
        ("bash", "bash", "posix", false),
        ("git_bash", "Git Bash", "posix", false),
        ("zsh", "zsh", "posix", false),
        ("fish", "fish", "posix", true),
        ("sh", "sh", "posix", false),
        ("powershell", "PowerShell", "powershell", false),
        ("pwsh", "PowerShell Core (pwsh)", "powershell", false),
        ("cmd", "cmd", "cmd", true),
        ("wsl", "WSL (bash)", "wsl", true),
    ];
    for (id, name, kind, limited) in expect {
        let (n, k, l) = shell_meta(id).unwrap();
        assert_eq!(n, name);
        assert_eq!(k, kind);
        assert_eq!(l, limited);
    }
    assert!(shell_meta("auto").is_none(), "auto 仅为配置内部值");
    assert!(shell_meta("nope").is_none());
}

/// ShellInfo 序列化即 wire 契约（前端按字段名消费）。
#[test]
fn shell_info_serializes_wire_shape() {
    let info = ShellInfo {
        id: "cmd".into(),
        name: "cmd".into(),
        path: Some(r"C:\Windows\System32\cmd.exe".into()),
        kind: "cmd".into(),
        limited: true,
        auto: false,
    };
    let v = serde_json::to_value(&info).unwrap();
    assert_eq!(v["id"], "cmd");
    assert_eq!(v["name"], "cmd");
    assert_eq!(v["kind"], "cmd");
    assert_eq!(v["limited"], true);
    assert!(v["path"].is_string());
    assert!(v["auto"].is_boolean(), "auto 字段进入 wire 契约");
}

/// resolve：显式 id 命中/缺席/未知/auto/空串全覆盖（存在性注入）。
#[test]
fn resolve_from_available_maps_and_falls_back() {
    let fb = Shell::PowerShellDesktop;
    let all = [
        "bash",
        "git_bash",
        "zsh",
        "fish",
        "sh",
        "powershell",
        "pwsh",
        "cmd",
        "wsl",
    ];

    assert_eq!(resolve_from_available("cmd", &all, fb.clone()), Shell::Cmd);
    assert_eq!(resolve_from_available("wsl", &all, fb.clone()), Shell::Wsl);
    assert_eq!(
        resolve_from_available("pwsh", &all, fb.clone()),
        Shell::Pwsh
    );
    assert_eq!(resolve_from_available("zsh", &all, fb.clone()), Shell::Zsh);
    assert_eq!(
        resolve_from_available("fish", &all, fb.clone()),
        Shell::Fish
    );
    assert_eq!(resolve_from_available("sh", &all, fb.clone()), Shell::Sh);
    assert_eq!(
        resolve_from_available("git_bash", &all, fb.clone()),
        Shell::Bash { login: true }
    );

    // 缺席 / 未知 / auto / 空 → 回退
    assert_eq!(resolve_from_available("zsh", &["cmd"], fb.clone()), fb);
    assert_eq!(resolve_from_available("auto", &all, fb.clone()), fb);
    assert_eq!(resolve_from_available("", &all, fb.clone()), fb);
    assert_eq!(resolve_from_available("nope", &all, fb.clone()), fb);
}

/// 探测列表按 SHELL_IDS 固定顺序；Windows 下 powershell/cmd 恒在。
#[cfg(windows)]
#[test]
fn detect_all_shells_stable_order_and_windows_always_present() {
    let shells = detect_all_shells();
    let ids: Vec<&str> = shells.iter().map(|s| s.id.as_str()).collect();
    let order: Vec<usize> = ids
        .iter()
        .map(|id| SHELL_IDS.iter().position(|s| s == id).unwrap())
        .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted, "结果必须按 SHELL_IDS 顺序：{ids:?}");
    assert!(
        ids.contains(&"powershell"),
        "Windows powershell 恒在：{ids:?}"
    );
    assert!(ids.contains(&"cmd"), "Windows cmd 恒在：{ids:?}");
    for s in &shells {
        assert!(!s.name.is_empty());
        assert_eq!(s.limited, matches!(s.id.as_str(), "cmd" | "fish" | "wsl"));
    }
}

/// run_process 拼接契约：各变体的程序 + 参数（纯字符串断言）。
#[test]
fn shell_invocation_per_variant() {
    let cwd = std::path::Path::new(r"D:\tmp");
    let cmd = "echo hi";

    // Windows 上 Bash 变体的程序是探测到的 Git Bash 完整路径（含 exe），非裸 "bash"
    let (prog, argv) = shell_invocation(&Shell::Bash { login: true }, cmd, cwd);
    if cfg!(windows) {
        assert!(
            prog.to_ascii_lowercase().ends_with("bash.exe"),
            "Git Bash 完整路径：{prog}"
        );
    } else {
        assert_eq!(prog, "bash");
    }
    assert_eq!(argv, vec!["-lc".to_string(), cmd.to_string()]);

    let (prog, argv) = shell_invocation(&Shell::Bash { login: false }, cmd, cwd);
    assert_eq!(argv, vec!["-c".to_string(), cmd.to_string()]);
    let _ = prog;

    for (shell, prog_name) in [
        (Shell::Zsh, "zsh"),
        (Shell::Fish, "fish"),
        (Shell::Sh, "sh"),
    ] {
        let (prog, argv) = shell_invocation(&shell, cmd, cwd);
        assert_eq!(prog, prog_name);
        assert_eq!(argv, vec!["-c".to_string(), cmd.to_string()], "{shell:?}");
    }

    let (prog, argv) = shell_invocation(&Shell::PowerShellDesktop, cmd, cwd);
    assert_eq!(prog, "powershell");
    assert_eq!(
        argv,
        vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            cmd.to_string()
        ]
    );

    let (prog, argv) = shell_invocation(&Shell::Pwsh, cmd, cwd);
    assert_eq!(prog, "pwsh");
    assert_eq!(argv[0], "-NoProfile");
    assert_eq!(argv[1], "-Command");

    let (prog, argv) = shell_invocation(&Shell::Cmd, cmd, cwd);
    assert_eq!(prog, "cmd");
    assert_eq!(argv, vec!["/C".to_string(), cmd.to_string()]);

    // WSL：wsl.exe --cd <windows_cwd> -e bash -lc <command>（cwd 原样传 Windows 路径）
    let (prog, argv) = shell_invocation(&Shell::Wsl, cmd, cwd);
    assert_eq!(prog, "wsl.exe");
    assert_eq!(
        argv,
        vec![
            "--cd".to_string(),
            r"D:\tmp".to_string(),
            "-e".to_string(),
            "bash".to_string(),
            "-lc".to_string(),
            cmd.to_string(),
        ]
    );
}

/// describe 完整环境说明：名称/家族 + 实际调用方式 + ≥3 条要点（全部 8 个变体全覆盖）。
#[test]
fn describe_contains_name_invocation_and_three_points() {
    for (shell, name_anchor, invoke_anchor) in [
        (Shell::Bash { login: true }, "bash", "bash -lc <command>"),
        (Shell::Zsh, "zsh", "zsh -c <command>"),
        (Shell::Fish, "fish", "fish -c <command>"),
        (Shell::Sh, "sh", "sh -c <command>"),
        (
            Shell::PowerShellDesktop,
            "PowerShell",
            "powershell -NoProfile -Command <command>",
        ),
        (Shell::Pwsh, "pwsh", "pwsh -NoProfile -Command <command>"),
        (Shell::Cmd, "cmd", "cmd /C <command>"),
        (
            Shell::Wsl,
            "WSL",
            "wsl.exe --cd <dir> -e bash -lc <command>",
        ),
    ] {
        let d = shell.describe();
        assert!(d.contains(name_anchor), "{shell:?} 描述缺名称：{d}");
        assert!(d.contains(invoke_anchor), "{shell:?} 描述缺调用方式：{d}");
        assert!(
            d.matches("- ").count() >= 3,
            "{shell:?} 描述语法要点不足 3 条：{d}"
        );
    }
    // WSL 额外要素：Linux 内执行 + /mnt 路径映射
    let wsl = Shell::Wsl.describe();
    assert!(wsl.contains("Linux"));
    assert!(wsl.contains("/mnt/"));
}

#[cfg(unix)]
#[tokio::test]
async fn full_output_false_spills_truncated_output() {
    // 28KB 输出：高于模型尾部预算、低于流式落盘阈值 → 命中「凡截断必落盘」盲区修复路径
    let (_ws, roots) = fresh_roots();
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt =
        core.get_or_create_session("spill", roots.workspace.clone(), None, vec![], None, vec![]);
    rt.set_prefs(mode_prefs(crate::core::prefs::ApprovalMode::AutoEdit));
    let ctx = test_ctx(core, rt, tokio_util::sync::CancellationToken::new());
    let out = CommandTool
        .run(
            &ctx,
            serde_json::json!({ "command": "seq 1 5000", "fullOutput": false }),
        )
        .await;
    assert!(out.ok, "{out:?}");
    assert_eq!(out.data["output_truncated"], true);
    let s = out.data["output"].as_str().unwrap();
    assert!(s.contains("输出已截断"), "应有 signal line：{s}");
    assert!(s.contains("共 5000 行"), "signal line 应带行数：{s}");
    assert!(
        s.chars().count() < 2_300,
        "false 时只回短尾部（2000 + signal line）"
    );
    let p = out.data["output_file_path"].as_str().unwrap();
    let full = std::fs::read_to_string(p).unwrap();
    assert!(
        full.starts_with("1\n"),
        "spill 应含头部：{}",
        &full[..20.min(full.len())]
    );
    assert!(full.contains("\n5000"), "spill 应含尾部");
}

#[cfg(unix)]
#[tokio::test]
async fn full_output_true_small_output_no_spill() {
    let (_ws, roots) = fresh_roots();
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt =
        core.get_or_create_session("full", roots.workspace.clone(), None, vec![], None, vec![]);
    rt.set_prefs(mode_prefs(crate::core::prefs::ApprovalMode::AutoEdit));
    let ctx = test_ctx(core, rt, tokio_util::sync::CancellationToken::new());
    let out = CommandTool
        .run(
            &ctx,
            serde_json::json!({ "command": "seq 1 100", "fullOutput": true }),
        )
        .await;
    assert!(out.ok, "{out:?}");
    assert!(
        out.data["output_file_path"].is_null(),
        "未截断不应有 spill 文件"
    );
    let s = out.data["output"].as_str().unwrap();
    assert!(!s.contains("输出已截断"));
    assert!(s.contains("\n100"));
}

// ===== 权限档消费点（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）：预取消 token = 审批立即拒绝，不等 120s =====

fn test_ctx(
    core: std::sync::Arc<crate::core::agent::AgentCore>,
    rt: std::sync::Arc<crate::core::agent::SessionRuntime>,
    cancel: tokio_util::sync::CancellationToken,
) -> ToolCtx {
    ToolCtx {
        core,
        rt,
        batch_id: "b".into(),
        call_index: 0,
        call_key: "b:0".into(),
        cancel,
    }
}

fn mode_prefs(m: crate::core::prefs::ApprovalMode) -> crate::core::prefs::SessionPrefs {
    crate::core::prefs::SessionPrefs {
        approval_mode: m,
        model_id: None,
        reasoning_effort: None,
    }
}

fn fresh_roots() -> (tempfile::TempDir, crate::tools::pathutil::WriteRoots) {
    let ws = tempfile::tempdir().unwrap();
    let dd = tempfile::tempdir().unwrap();
    let roots = super::super::pathutil::WriteRoots {
        workspace: std::fs::canonicalize(ws.path()).unwrap(),
        extra: vec![],
        data_dir: std::fs::canonicalize(dd.path()).unwrap(),
    };
    (ws, roots)
}

#[cfg(unix)]
#[tokio::test]
async fn approval_modes_gate_inside_writes() {
    use crate::core::prefs::ApprovalMode;
    let (ws, roots) = fresh_roots();
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt =
        core.get_or_create_session("modes", roots.workspace.clone(), None, vec![], None, vec![]);

    // AutoEdit（当前默认）：范围内写直接执行
    rt.set_prefs(mode_prefs(ApprovalMode::AutoEdit));
    let ctx = test_ctx(
        core.clone(),
        rt.clone(),
        tokio_util::sync::CancellationToken::new(),
    );
    let out = CommandTool
        .run(&ctx, serde_json::json!({ "command": "echo x > w1.txt" }))
        .await;
    assert!(out.ok, "{out:?}");
    assert!(ws.path().join("w1.txt").exists());

    // ConfirmEach：范围内写需确认；预取消 token → 拒绝且不落盘
    rt.set_prefs(mode_prefs(ApprovalMode::ConfirmEach));
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let ctx = test_ctx(core.clone(), rt.clone(), cancel);
    let out = CommandTool
        .run(&ctx, serde_json::json!({ "command": "echo x > w2.txt" }))
        .await;
    assert!(!out.ok);
    assert_eq!(out.error.as_ref().unwrap().code, "E_APPROVAL_DENIED");
    assert!(!ws.path().join("w2.txt").exists(), "被拒绝的写不应落盘");

    // FullAccess：跳过确认直接执行
    rt.set_prefs(mode_prefs(ApprovalMode::FullAccess));
    let ctx = test_ctx(
        core.clone(),
        rt.clone(),
        tokio_util::sync::CancellationToken::new(),
    );
    let out = CommandTool
        .run(&ctx, serde_json::json!({ "command": "echo x > w3.txt" }))
        .await;
    assert!(out.ok, "{out:?}");
    assert!(ws.path().join("w3.txt").exists());

    // FullAccess：灾难级仍拦截（dd 不会真实执行）
    let ctx = test_ctx(core, rt, tokio_util::sync::CancellationToken::new());
    let out = CommandTool
        .run(
            &ctx,
            serde_json::json!({ "command": "echo x | dd of=/dev/zero bs=1 count=1" }),
        )
        .await;
    assert!(!out.ok);
    assert_eq!(out.error.as_ref().unwrap().code, "E_COMMAND_BLOCKED");
}

#[cfg(unix)]
#[tokio::test]
async fn full_access_skips_high_risk_confirm() {
    // 高危（非灾难；此处是「变量展开写目标」形态，无需联网）：
    // FullAccess 免确认执行；ConfirmEach 需确认（预取消 = 拒绝）。
    let (ws, roots) = fresh_roots();
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session("fa", roots.workspace.clone(), None, vec![], None, vec![]);
    let probe = ws.path().join("probe.txt");
    // 用范围内的变量展开目标（$PWD 指向 cwd = 工作区）：执行落在临时工作区内
    let cmd = format!(
        "echo hi > $PWD/{}",
        probe.file_name().unwrap().to_string_lossy()
    );

    rt.set_prefs(mode_prefs(crate::core::prefs::ApprovalMode::FullAccess));
    let ctx = test_ctx(
        core.clone(),
        rt.clone(),
        tokio_util::sync::CancellationToken::new(),
    );
    let out = CommandTool
        .run(&ctx, serde_json::json!({ "command": cmd }))
        .await;
    assert!(out.ok, "{out:?}");
    assert!(probe.exists(), "免确认执行应落盘");

    rt.set_prefs(mode_prefs(crate::core::prefs::ApprovalMode::ConfirmEach));
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let ctx = test_ctx(core, rt, cancel);
    let out = CommandTool
        .run(&ctx, serde_json::json!({ "command": cmd }))
        .await;
    assert!(!out.ok);
    assert_eq!(out.error.as_ref().unwrap().code, "E_APPROVAL_DENIED");
}

// ===== 目标档执行期：账本闸门 / 免确认 / 高危硬拦（不依赖 shell 实际执行）=====

/// 目标档 ctx 夹具：指定档位 + 目标阶段 + 账本（路径/程序由调用方给定）。
fn goal_ctx(
    mode: crate::core::prefs::ApprovalMode,
    status: crate::core::agent::goal::GoalStatus,
    ledger_paths: Vec<String>,
    ledger_programs: Vec<String>,
) -> (tempfile::TempDir, ToolCtx) {
    use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState};
    let (ws, roots) = fresh_roots();
    let core = crate::core::agent::test_support::make_core(&roots);
    let rt = core.get_or_create_session(
        "goalcmd",
        roots.workspace.clone(),
        None,
        vec![],
        None,
        vec![],
    );
    rt.set_prefs(mode_prefs(mode));
    rt.set_goal(Some(GoalState {
        text: "把 X 改成 Y".into(),
        criteria: vec![GoalCriterion {
            title: "改完 X".into(),
            done: false,
        }],
        ledger: GoalLedger {
            paths: ledger_paths,
            programs: ledger_programs,
        },
        status,
        decisions: Vec::new(),
        pending: Vec::new(),
        blocked: Vec::new(),
        rounds: 0,
        stall_streak: 0,
        ledger_denials: 0,
    }));
    let ctx = test_ctx(core, rt, tokio_util::sync::CancellationToken::new());
    (ws, ctx)
}

/// ⑤ 目标档执行期 L3 高危：硬拦 + `blocked` 记录 + 请求硬停（不弹审批）。
#[tokio::test]
async fn goal_execute_hard_blocks_high_risk_command() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    use crate::tools::Tool as _;
    let (_ws, ctx) = goal_ctx(
        ApprovalMode::Goal,
        GoalStatus::Executing,
        vec![],
        vec!["git".into()],
    );
    let out = CommandTool
        .run(
            &ctx,
            serde_json::json!({"command": "git push --force origin main"}),
        )
        .await;
    assert!(!out.ok);
    assert_eq!(out.error.as_ref().unwrap().code, "E_COMMAND_BLOCKED");
    let g = ctx.rt.goal_snapshot().unwrap();
    assert!(
        g.blocked.iter().any(|b| b.contains("git push --force")),
        "L3 硬拦必须记入 blocked：{:?}",
        g.blocked
    );
    assert!(ctx.rt.take_goal_abort(), "L3 硬拦必须请求硬停");
}

/// ⑥ 目标档执行期：程序名过账本（越界即拒），且不再走审批弹窗（预取消 token 不被消费）。
/// 必须 multi_thread：(c) 段真会执行命令并被预取消的 token 终止进程树，而 `terminate_tree`
/// 走 `block_in_place`（current_thread 运行时会直接 panic）。
#[tokio::test(flavor = "multi_thread")]
async fn goal_command_gate_and_confirm_skip() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    use crate::tools::Tool as _;
    // (a) 账本外程序：执行期直接拒绝，不问人（`ls` 过 fence 为 Allow，命中的是账本闸门）
    let (_ws, ctx) = goal_ctx(
        ApprovalMode::Goal,
        GoalStatus::Executing,
        vec![],
        vec!["cargo".into()],
    );
    let out = CommandTool
        .run(&ctx, serde_json::json!({"command": "ls -la"}))
        .await;
    assert_eq!(
        out.error.as_ref().map(|e| e.code.as_str()),
        Some("E_GOAL_OUTSIDE_LEDGER"),
        "{:?}",
        out.error
    );
    assert_eq!(ctx.rt.goal_snapshot().unwrap().ledger_denials, 1);

    // (b) 同一命令（工作区外新建，默认配置下 fence 判 Confirm）在 AutoEdit 下预取消 = 拒绝
    let outside = tempfile::tempdir().unwrap();
    let cmd = format!("mkdir {}", outside.path().join("probe").display());
    let (_ws_auto, ctx_auto) = goal_ctx(
        ApprovalMode::AutoEdit,
        GoalStatus::Executing,
        vec![],
        vec![],
    );
    ctx_auto.cancel.cancel();
    let out = CommandTool
        .run(&ctx_auto, serde_json::json!({"command": cmd}))
        .await;
    assert_eq!(
        out.error.as_ref().map(|e| e.code.as_str()),
        Some("E_APPROVAL_DENIED"),
        "前提：AutoEdit 下该命令需确认：{:?}",
        out.error
    );

    // (c) 目标档执行期 + 账本内：免确认（预取消 token 不被消费 → 绝不是审批拒绝）
    let outside2 = tempfile::tempdir().unwrap();
    let cmd2 = format!("mkdir {}", outside2.path().join("probe").display());
    let (_ws_goal, ctx_goal) = goal_ctx(
        ApprovalMode::Goal,
        GoalStatus::Executing,
        vec![outside2.path().to_string_lossy().into_owned()],
        vec!["mkdir".into()],
    );
    ctx_goal.cancel.cancel();
    let out = CommandTool
        .run(&ctx_goal, serde_json::json!({"command": cmd2}))
        .await;
    assert_ne!(
        out.error.as_ref().map(|e| e.code.as_str()),
        Some("E_APPROVAL_DENIED"),
        "目标档执行期不得产生审批请求：{:?}",
        out.error
    );
    assert!(
        ctx_goal.rt.goal_snapshot().unwrap().blocked.is_empty(),
        "账本内命令不得被硬拦"
    );
    assert!(!ctx_goal.rt.take_goal_abort());
    assert_eq!(ctx_goal.rt.goal_snapshot().unwrap().ledger_denials, 0);
}

#[tokio::test]
async fn goal_command_rejects_redirection_and_second_program() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    use crate::tools::Tool as _;
    let (ws, ctx) = goal_ctx(
        ApprovalMode::Goal,
        GoalStatus::Executing,
        vec![],
        vec!["echo".into()],
    );
    let outside = ws.path().join("outside.txt");
    for command in [
        format!("echo x > {}", outside.display()),
        "echo ok; cargo build".into(),
    ] {
        let out = CommandTool
            .run(&ctx, serde_json::json!({"command": command}))
            .await;
        assert_eq!(
            out.error.as_ref().map(|e| e.code.as_str()),
            Some("E_GOAL_COMMAND_SHAPE")
        );
    }
    assert!(!outside.exists());
}

#[tokio::test]
async fn goal_command_checks_inside_write_even_without_user_confirmation() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    use crate::tools::Tool as _;
    let (ws, ctx) = goal_ctx(
        ApprovalMode::Goal,
        GoalStatus::Executing,
        vec![],
        vec!["touch".into()],
    );
    let outside = ws.path().join("outside.txt");
    let out = CommandTool
        .run(
            &ctx,
            serde_json::json!({"command": format!("touch {}", outside.display())}),
        )
        .await;
    assert_eq!(
        out.error.as_ref().map(|e| e.code.as_str()),
        Some("E_GOAL_OUTSIDE_LEDGER")
    );
    assert!(!outside.exists());
}

/// ③（回归红线）非目标档 / 澄清期：命令执行完全不受账本闸门影响。
#[tokio::test]
async fn goal_command_gate_inert_outside_goal_execute() {
    use crate::core::agent::goal::GoalStatus;
    use crate::core::prefs::ApprovalMode;
    use crate::tools::Tool as _;
    for (mode, status) in [
        (ApprovalMode::AutoEdit, GoalStatus::Executing),
        (ApprovalMode::Goal, GoalStatus::Clarify),
    ] {
        let (_ws, ctx) = goal_ctx(mode, status, vec![], vec![]);
        let out = CommandTool
            .run(&ctx, serde_json::json!({"command": "ls -la"}))
            .await;
        assert_ne!(
            out.error.as_ref().map(|e| e.code.as_str()),
            Some("E_GOAL_OUTSIDE_LEDGER"),
            "{mode:?}/{status:?} 被账本闸门误伤：{:?}",
            out.error
        );
        assert_eq!(
            ctx.rt.goal_snapshot().unwrap().ledger_denials,
            0,
            "{mode:?}/{status:?}"
        );
        assert!(!ctx.rt.take_goal_abort());
    }
}
