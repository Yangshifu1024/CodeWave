//! 写入后检查命令（[docs/post-write-check-plan](../../../docs/post-write-check-plan.md)）：
//! 取代 LSP 写后语义校验。create / edit 成功后执行用户配置的一条命令（如
//! `npx eslint {file}`），在会话工作区根目录运行，把输出尾部整理成 `check` 结构
//! 放进工具结果的 `outcome.data`——**这是缺陷 B 的修复要点**：结论必须走 `data`
//! 通道，而不是只到前端的 `warnings`。
//!
//! 与 aider `--lint-cmd` 同思路：命令由用户按项目配置、执行环境即项目环境，
//! 因此不需要写前基准 / 差集 / 就绪证据这一整套判据。用户自己配置的命令视为已授权：
//! 只过安全围栏（Block 才拦下），Confirm 不逐次弹审批。
//!
//! 纪律：**没真的跑过，文案里绝不出现「通过」**。`skipped` 携带未执行/未完成的原因。

use super::ToolCtx;
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 单次命令捕获的输出字节上限（防长命令撑爆内存；模型只要尾部，故滚动保留尾部）。
const MAX_CAPTURE_BYTES: usize = 1 << 20;

/// 检查命令未执行 / 未完成的原因（wire 字符串固定：前端与模型都读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// 开关关闭
    Disabled,
    /// 未配置命令
    EmptyCommand,
    /// 临时会话无项目目录
    NoProject,
    /// 被安全围栏拦下
    BlockedByFence,
    /// 命令超时（已终止进程组）
    Timeout,
    /// 用户取消运行（已终止进程组）
    Cancelled,
}

/// 单个文件的写入后检查结论（放进 `outcome.data` 的 `check` 对象；字段名即 wire 契约）。
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// 是否真的启动过命令（`false` 时 `ok` 无意义）
    pub ran: bool,
    /// 命令退出码是否为零（`ran=false` 时无意义）
    pub ok: bool,
    /// 实际执行的命令（`{file}` 已替换；内置 JSON 解析为 `builtin:json-parse`）
    pub command: String,
    /// 合并后的输出尾部（最多 `tail_chars` 个字符）
    pub output: String,
    /// 未执行 / 未完成的原因（`null` 表示有完整结论）
    pub skipped: Option<SkipReason>,
}

impl CheckResult {
    /// 未执行的跳过结果（`ran=false`、`output` 可携带如实原因）。
    pub fn skipped(reason: SkipReason, command: impl Into<String>, output: impl Into<String>) -> Self {
        CheckResult {
            ran: false,
            ok: false,
            command: command.into(),
            output: output.into(),
            skipped: Some(reason),
        }
    }

    /// 内置 JSON 解析的结论（`ran=true`；`ok` 即解析是否成功）。
    pub fn builtin_json(ok: bool, output: impl Into<String>) -> Self {
        CheckResult {
            ran: true,
            ok,
            command: "builtin:json-parse".into(),
            output: output.into(),
            skipped: None,
        }
    }
}

/// 命令是否含 `{file}` 占位符（决定「每文件执行一次」还是「整个调用执行一次」）。
pub fn has_file_placeholder(command: &str) -> bool {
    command.contains("{file}")
}

/// 按 shell 家族引用一个路径（**防注入**：路径由模型提供，绝不能裸拼进 shell 字符串）。
/// - POSIX（bash/zsh/sh/wsl 内层 bash）：单引号包裹，内部 `'` 用 `'\''` 转义；
/// - fish：单引号包裹，内部 `\` 与 `'` 反斜杠转义；
/// - PowerShell：单引号包裹，内部 `'` 双写；
/// - cmd：双引号包裹（Windows 路径不允许 `"`，无需再转义）。
pub fn quote_path(shell: &crate::tools::command::Shell, rel: &str) -> String {
    use crate::tools::command::Shell;
    match shell {
        Shell::Bash { .. } | Shell::Zsh | Shell::Sh | Shell::Wsl => {
            format!("'{}'", rel.replace('\'', "'\\''"))
        }
        Shell::Fish => format!("'{}'", rel.replace('\\', "\\\\").replace('\'', "\\'")),
        Shell::PowerShellDesktop | Shell::Pwsh => format!("'{}'", rel.replace('\'', "''")),
        Shell::Cmd => format!("\"{}\"", rel.replace('"', "\"\"")),
    }
}

/// 把 `{file}` 替换为**按当前 shell 引用过**的相对项目根路径。
pub fn substitute_file(command: &str, rel: &str, shell: &crate::tools::command::Shell) -> String {
    command.replace("{file}", &quote_path(shell, rel))
}

/// 执行一条检查命令，返回结论。
///
/// - `file`：`Some(rel)` 时把命令里的 `{file}` 替换为按 shell 引用过的相对路径（每文件执行一次）；
///   `None` 时不替换（整个工具调用执行一次）；
/// - 执行前过安全围栏：`Block` 才拦下（写进 `output` 如实告知），`Confirm` 视为已授权直接执行；
/// - 工作区根为 cwd；独立进程组整树终止；超时 / 取消都杀进程组并收尸，绝不留孤儿；
/// - stdout/stderr 并发读取合并；输出取尾部 `tail_chars` 个字符。
pub async fn run_command(
    ctx: &ToolCtx,
    command: &str,
    file: Option<&str>,
    timeout_seconds: u64,
    tail_chars: usize,
) -> CheckResult {
    let selection = ctx.core.cfg.read().unwrap().shell.selection.clone();
    let shell = crate::tools::command::resolve_shell(selection.as_deref());
    let concrete = match file {
        Some(rel) => substitute_file(command, rel, &shell),
        None => command.to_string(),
    };
    let command = concrete.as_str();

    // 安全围栏：用户自己配置的命令视为已授权，只有 Block 才拦下
    let verdict = crate::safety::fence::check_command_policy(
        command,
        &ctx.rt.workspace,
        &ctx.write_roots(),
        ctx.fence_policy(),
    );
    if let crate::safety::fence::Verdict::Block { message, .. } = verdict {
        return CheckResult::skipped(SkipReason::BlockedByFence, command, message);
    }

    let cwd = ctx.rt.workspace.clone();
    let (program, argv) = crate::tools::command::shell_invocation(&shell, command, &cwd);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&argv)
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // 任务被 abort 时子进程随之回收；正常超时 / 取消路径仍走 terminate_tree 主动杀整树
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0); // 独立进程组：可整树终止
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // 命令没真的跑起来：ran=false，输出如实带上原因（绝不让「没跑」读成「跑完有错」）
            return CheckResult {
                ran: false,
                ok: false,
                command: command.to_string(),
                output: format!("进程启动失败：{e}"),
                skipped: None,
            };
        }
    };
    let Some(pid) = child.id() else {
        return CheckResult {
            ran: true,
            ok: false,
            command: command.to_string(),
            output: "进程启动后立即退出".into(),
            skipped: None,
        };
    };

    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let t1 = stdout.map(|s| tokio::spawn(read_stream(s, buf.clone())));
    let t2 = stderr.map(|s| tokio::spawn(read_stream(s, buf.clone())));

    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let status = tokio::select! {
        s = child.wait() => match s {
            Ok(st) => st,
            Err(e) => {
                terminate_headless(ctx, pid, &mut child, t1, t2).await;
                return CheckResult {
                    ran: true,
                    ok: false,
                    command: command.to_string(),
                    output: format!("等待进程失败：{e}"),
                    skipped: None,
                };
            }
        },
        _ = tokio::time::sleep(timeout) => {
            terminate_headless(ctx, pid, &mut child, t1, t2).await;
            return CheckResult {
                ran: true,
                ok: false,
                command: command.to_string(),
                output: tail_text(&buf, tail_chars),
                skipped: Some(SkipReason::Timeout),
            };
        }
        _ = ctx.cancel.cancelled() => {
            terminate_headless(ctx, pid, &mut child, t1, t2).await;
            return CheckResult {
                ran: true,
                ok: false,
                command: command.to_string(),
                output: tail_text(&buf, tail_chars),
                skipped: Some(SkipReason::Cancelled),
            };
        }
    };
    let _ = wait_pump(t1).await;
    let _ = wait_pump(t2).await;
    let code = status.code().unwrap_or(-1);
    CheckResult {
        ran: true,
        ok: code == 0,
        command: command.to_string(),
        output: tail_text(&buf, tail_chars),
        skipped: None,
    }
}

/// 超时 / 取消路径：杀进程组 → 收尸 → 等输出泵结束。
async fn terminate_headless(
    _ctx: &ToolCtx,
    pid: u32,
    child: &mut tokio::process::Child,
    t1: Option<tokio::task::JoinHandle<()>>,
    t2: Option<tokio::task::JoinHandle<()>>,
) {
    crate::tools::command::terminate_tree(pid);
    let _ = child.wait().await;
    let _ = wait_pump(t1).await;
    let _ = wait_pump(t2).await;
}

/// 等待输出泵任务收尾（pump panic 不影响主流程）。
async fn wait_pump(t: Option<tokio::task::JoinHandle<()>>) -> Result<(), ()> {
    match t {
        Some(h) => h.await.map_err(|_| ()),
        None => Ok(()),
    }
}

/// 并发读取一条流到共享缓冲（滚动保留尾部上限，防内存膨胀）。
async fn read_stream<R: tokio::io::AsyncRead + Unpin>(mut s: R, buf: Arc<Mutex<Vec<u8>>>) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut b = buf.lock().unwrap();
                b.extend_from_slice(&chunk[..n]);
                if b.len() > MAX_CAPTURE_BYTES {
                    let overflow = b.len() - MAX_CAPTURE_BYTES;
                    b.drain(..overflow);
                }
            }
        }
    }
}

/// 取合并输出的尾部 `tail_chars` 个字符（UTF-8 宽松解码，不把多字节字符切出替换符）。
fn tail_text(buf: &Arc<Mutex<Vec<u8>>>, tail_chars: usize) -> String {
    let b = buf.lock().unwrap();
    if b.is_empty() || tail_chars == 0 {
        return String::new();
    }
    String::from_utf8_lossy(&b)
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 文件是否为 JSON（JSON 走内置解析，不依赖检查命令开关）。
pub fn is_json(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolCtx;

    #[test]
    fn placeholder_detection_and_substitution_quotes_path() {
        use crate::tools::command::Shell;
        assert!(has_file_placeholder("npx eslint {file}"));
        assert!(!has_file_placeholder("cargo check"));
        // POSIX：单引号包裹（含空格也不裂开）
        assert_eq!(
            substitute_file("eslint {file}", "ui/src/x.ts", &Shell::Sh),
            "eslint 'ui/src/x.ts'"
        );
        // 无占位符：原样
        assert_eq!(substitute_file("cargo check", "x.rs", &Shell::Sh), "cargo check");
        // 防注入：路径里的 shell 元字符被引号封住，不会成为命令分隔符
        assert_eq!(
            substitute_file("eslint {file}", "a; rm -rf ~", &Shell::Sh),
            "eslint 'a; rm -rf ~'"
        );
        // 路径含单引号：POSIX 用 '\'' 转义（仍是单个安全实参）
        assert_eq!(
            substitute_file("eslint {file}", "a'b.ts", &Shell::Sh),
            "eslint 'a'\\''b.ts'"
        );
        // PowerShell：单引号双写
        assert_eq!(
            substitute_file("eslint {file}", "a'b.ts", &Shell::PowerShellDesktop),
            "eslint 'a''b.ts'"
        );
        // cmd：双引号包裹
        assert_eq!(
            substitute_file("eslint {file}", "a b.ts", &Shell::Cmd),
            "eslint \"a b.ts\""
        );
    }

    #[test]
    fn is_json_by_extension() {
        assert!(is_json(Path::new("a.json")));
        assert!(is_json(Path::new("A.JSON")));
        assert!(!is_json(Path::new("a.ts")));
    }

    /// 项目会话 + AutoEdit 档（默认 Plan 档的只读白名单会把检查命令送进审批/Block）。
    #[cfg(unix)]
    fn project_ctx(name: &str) -> (ToolCtx, tempfile::TempDir, tempfile::TempDir) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        // 固定 sh（存在性稳定，避免 login bash 探测拖慢用例）
        core.cfg.write().unwrap().shell.selection = Some("sh".into());
        let rt = core.get_or_create_session(
            name,
            roots.workspace.clone(),
            Some("proj".into()),
            vec![],
            None,
            vec![],
        );
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::AutoEdit,
            model_id: None,
            reasoning_effort: None,
        });
        let ctx = ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        (ctx, ws, dd)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_exit_reports_output_and_ok_false() {
        let (ctx, _ws, _dd) = project_ctx("pc-nonzero");
        let r = run_command(&ctx, "echo boom; exit 3", None, 10, 3000).await;
        assert!(r.ran);
        assert!(!r.ok);
        assert_eq!(r.skipped, None);
        assert!(r.output.contains("boom"), "{:?}", r.output);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn zero_exit_is_ok() {
        let (ctx, _ws, _dd) = project_ctx("pc-zero");
        let r = run_command(&ctx, "echo fine", None, 10, 3000).await;
        assert!(r.ran && r.ok && r.skipped.is_none(), "{r:?}");
        assert!(r.output.contains("fine"));
    }

    /// §7.1 尾部截断：只把尾部 `tail_chars` 个字符交给模型。
    #[cfg(unix)]
    #[tokio::test]
    async fn output_keeps_only_tail() {
        let (ctx, _ws, _dd) = project_ctx("pc-tail");
        let r = run_command(&ctx, "printf '0123456789ABCDEFGHIJ'", None, 10, 5).await;
        assert!(r.ran && r.ok);
        assert_eq!(r.output.chars().count(), 5);
        assert_eq!(r.output, "FGHIJ");
    }

    /// §7.2.3 输出分片安全：多字节字符跨读取块不产生替换符。
    #[cfg(unix)]
    #[tokio::test]
    async fn multibyte_output_is_not_corrupted() {
        let (ctx, _ws, _dd) = project_ctx("pc-utf8");
        let r = run_command(
            &ctx,
            "printf '前缀-你好世界-后缀'",
            None,
            10,
            3000,
        )
        .await;
        assert!(r.output.contains("你好世界"), "{:?}", r.output);
        assert!(!r.output.contains('\u{FFFD}'), "不得出现替换符：{:?}", r.output);
    }

    /// §4.3 超时：终止整个进程组，如实带回已有部分输出，skipped=timeout。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_kills_and_reports_partial() {
        let (ctx, _ws, _dd) = project_ctx("pc-timeout");
        let r = run_command(&ctx, "echo before-timeout; sleep 30", None, 1, 3000).await;
        assert!(r.ran, "命令确实启动过");
        assert_eq!(r.skipped, Some(SkipReason::Timeout));
        assert!(r.output.contains("before-timeout"), "{:?}", r.output);
    }

    /// §4.3 取消：终止整个进程组，skipped=cancelled。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_kills_child() {
        let (ctx, _ws, _dd) = project_ctx("pc-cancel");
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let r = run_command(&ctx, "sleep 30", None, 30, 3000).await;
        assert_eq!(r.skipped, Some(SkipReason::Cancelled));
    }

    /// §7.2.1 子进程不留孤儿：超时杀整树后，命令行里 sleep 的进程也随进程组一起消失。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_leaves_no_orphan() {
        let (ctx, _ws, _dd) = project_ctx("pc-orphan");
        // 把子进程（sleep）的 pid 打到文件；超时后该 pid 必须已不存在
        let pidfile = ctx.rt.workspace.join("orphan.pid");
        let cmd = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let r = run_command(&ctx, &cmd, None, 1, 3000).await;
        assert_eq!(r.skipped, Some(SkipReason::Timeout));
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("pid 文件应已写入")
            .trim()
            .parse()
            .expect("pid 应为数字");
        // 给终止流程一点收尸时间
        tokio::time::sleep(Duration::from_millis(500)).await;
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "超时后子进程 {pid} 不应仍存活（孤儿进程）");
    }

    /// §4.3 围栏 Block：灾难级命令不执行，skipped=blocked-by-fence（用户配置不豁免拦下）。
    #[cfg(unix)]
    #[tokio::test]
    async fn fence_block_skips_command() {
        let (ctx, _ws, _dd) = project_ctx("pc-fence");
        let r = run_command(&ctx, "rm -rf /tmp/codewave-no-such", None, 10, 3000).await;
        assert!(!r.ran, "被围栏拦下的命令不得执行");
        assert_eq!(r.skipped, Some(SkipReason::BlockedByFence));
        assert!(!r.output.is_empty(), "围栏原因应如实带上");
    }
}
