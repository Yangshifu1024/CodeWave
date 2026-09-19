//! 一键安装的执行层（TS/Python 走 npm，Rust 走 rustup，Go 走 go install）。
//!
//! **边界**：本模块只负责「拼命令 + 起进程 + 收输出」，审批与安全围栏由 host 层在调用前完成
//! （与 `service` 工具一致：命令仍然过 fence，灾难级照样拦）。Java/Dart 是 Manual，这里返回 None。

use super::discovery;
use super::server_spec;
use super::{InstallKind, Lang};
#[cfg(any(windows, test))]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// 一键安装的最小超时预算。
///
/// ≥10 分钟：npm 装 typescript、rustup 拉工具链都可能跑十几分钟。调用方（`host/commands/lsp.rs`）
/// 本就传 600s，这里再兜一道底，避免将来有人传小了把安装卡死在半途。
pub const MIN_TIMEOUT: Duration = Duration::from_secs(600);

/// 安装命令的执行计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// 可执行程序
    pub program: String,
    /// 参数
    pub args: Vec<String>,
    /// 展示用整条命令
    pub display: String,
}

/// 取某语言的安装计划（Manual/ConfirmEnable 返回 `None`）。
pub fn plan(lang: Lang) -> Option<InstallPlan> {
    let spec = server_spec::spec(lang);
    if spec.install.kind != InstallKind::Installable {
        return None;
    }
    let command = spec.install.command.clone()?;
    let mut words = super::discovery::split_command(&command).into_iter();
    let program = words.next()?;
    Some(InstallPlan {
        program,
        args: words.collect(),
        display: command,
    })
}

/// 解析后的可执行目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProgram {
    /// 真实的绝对路径（绝不是裸名）
    pub path: PathBuf,
    /// 是否必须经 `cmd /C` 执行（Windows 的 `.cmd`/`.bat` 用 `CreateProcess` 跑不了）
    pub shell_wrapped: bool,
}

/// 安装执行的 PATH 语境：新鲜探测优先，失败回落进程快照（两份都试，见 [`resolve_program`]）。
fn install_paths() -> (String, String) {
    let snapshot = std::env::var("PATH").unwrap_or_default();
    let fresh = discovery::fresh_env_path().unwrap_or_default();
    (fresh, snapshot)
}

/// 把裸名解析成真实可执行文件：新鲜 PATH → 快照 PATH（Windows 按 `PATHEXT` 补扩展名）。
///
/// **解析不到直接 `Err`**（文案写明名字），绝不静默地拿裸名去 spawn——Windows 上那必然失败，
/// 而且报的是含糊的「程序不存在」（真实原因是缺 `.cmd` 后缀）。
pub fn resolve_program(
    program: &str,
    fresh_path: &str,
    snapshot_path: &str,
) -> Result<ResolvedProgram, String> {
    let name = program.trim();
    if name.is_empty() {
        return Err("安装命令缺少可执行程序名，无法执行安装".to_string());
    }
    let mut found: Option<PathBuf> = None;
    for path in [fresh_path, snapshot_path] {
        // 带路径分隔符的名字由 `which_in` 按路径处理（PATH 为空也能解析）
        if let Some(p) = discovery::which_in(name, path) {
            found = Some(p);
            break;
        }
    }
    let path = found.ok_or_else(|| format!("未找到 {name}，无法执行安装"))?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Ok(ResolvedProgram {
        path,
        shell_wrapped: cfg!(windows) && matches!(ext.as_str(), "cmd" | "bat"),
    })
}

/// 组装实际执行的命令：`.cmd`/`.bat` 经 `cmd /C`（批处理不能被 `CreateProcess` 直接执行）。
///
/// Windows 分支用 [`cmd_command_line`] **显式拼命令行**再 `raw_arg` 原样追加；不把路径直接交给
/// `Command::arg`，因为它自动加引号时会把内层引号转义成 `\"`——cmd.exe 不认这种转义，会把整条
/// 命令当成一个文件名。外层再套一对引号，配合 `/S` 让 cmd 先剥掉这一层、再解析内层。
fn build_command(resolved: &ResolvedProgram, args: &[String]) -> tokio::process::Command {
    #[cfg(windows)]
    {
        if resolved.shell_wrapped {
            let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
            let mut cmd = tokio::process::Command::new(comspec);
            cmd.arg("/D").arg("/S").arg("/C");
            cmd.raw_arg(format!("\"{}\"", cmd_command_line(&resolved.path, args)));
            return cmd;
        }
    }
    let mut cmd = tokio::process::Command::new(&resolved.path);
    cmd.args(args);
    cmd
}

/// 构造 `cmd.exe /S /C` 用的命令行：**每个 token 都用双引号包裹**。
///
/// 为什么要自己拼：cmd 只把引号**外**的 `& | < > ^ ( )` 当元字符，路径里含它们时（如
/// `C:\Program Files\nodejs & tools\npm.cmd`）不加引号会被切成两条命令；包裹后它们全部退回字面量。
///
/// 两处不能想当然：
/// 1. `^` **不能**在引号内写成 `^^`——cmd 的 `^` 在引号内不是转义符，写两个反而会把路径里的一个
///    `^` 变成两个（路径直接错）；引号本身已经让 `^` 退回字面量；
/// 2. `%` / `!` 在 cmd 里**引号内外都会被展开**，没有可靠的转义写法 → 遇到只记一条 warn
///    （当前六语言的安装计划都不含这两个字符；真出现了也不静默）。
#[cfg(any(windows, test))]
fn cmd_command_line(path: &Path, args: &[String]) -> String {
    let program = path.to_string_lossy().to_string();
    warn_if_cmd_expands(&program);
    let mut parts: Vec<String> = Vec::with_capacity(args.len() + 1);
    parts.push(format!("\"{program}\""));
    for a in args {
        warn_if_cmd_expands(a);
        parts.push(format!("\"{a}\""));
    }
    parts.join(" ")
}

/// cmd 会展开 `%VAR%` / `!VAR!`（引号挡不住）→ 记一条 warn 而不是静默。
#[cfg(any(windows, test))]
fn warn_if_cmd_expands(token: &str) {
    if token.contains('%') || token.contains('!') {
        tracing::warn!(
            token = %token,
            "安装命令的路径/参数含 cmd 会展开的字符（% / !），无法转义——执行结果可能不符预期"
        );
    }
}

/// 读干一条管道（无管道时返回空）。
async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut p) = pipe {
        let _ = p.read_to_end(&mut buf).await;
    }
    buf
}

/// 执行安装计划（解析可执行文件 + 超时 + 抓输出）。**不做围栏/审批**——调用方必须先过审批。
///
/// 三条硬纪律：
/// 1. 执行前必须把裸名解析成真实文件（Windows 上 `npm` 是 `npm.cmd`）+  `.cmd`/`.bat` 经 `cmd /C`；
/// 2. **stdout / stderr 必须并发读**（`tokio::join!`）——串行读时 npm/rustup 往 stderr 写满
///    64KB 管道就会卡死到超时（与 `client.rs` 的 stderr 独立泵同源教训）；
/// 3. 超时分支杀完进程组还要 `wait()` 回收，否则留僵尸进程。
pub async fn execute(plan: &InstallPlan, timeout: Duration) -> Result<String, String> {
    let (fresh, snapshot) = install_paths();
    let resolved = resolve_program(&plan.program, &fresh, &snapshot)?;
    let timeout = timeout.max(MIN_TIMEOUT);
    tracing::debug!(
        command = %plan.display,
        resolved = %resolved.path.display(),
        shell_wrapped = resolved.shell_wrapped,
        "登录安装命令的可执行文件"
    );
    let mut cmd = build_command(&resolved, &plan.args);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        // tokio 的 `Command` 自带 `creation_flags`（不必再引 std 的 `CommandExt`）
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法执行安装命令 {}：{e}", plan.display))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // 两路并发读：两个 future 各自持有自己的管道，互不阻塞
    let collect = async move {
        let (out, err) = tokio::join!(read_all(stdout), read_all(stderr));
        (out, err)
    };
    let (buf, status) = match tokio::time::timeout(timeout, collect).await {
        Ok((mut out, err)) => {
            out.extend_from_slice(&err);
            let status = child.wait().await;
            (out, status)
        }
        Err(_) => {
            let _ = super::client::kill_process_group(child.id(), &plan.program);
            // 收尸：kill 只发信号，不等就留僵尸（也避免句柄泄漏）
            let _ = child.wait().await;
            return Err(format!("安装命令超时（{}s）", timeout.as_secs()));
        }
    };
    let text = String::from_utf8_lossy(&buf).into_owned();
    let trimmed = if text.chars().count() > 4000 {
        text.chars().take(4000).collect::<String>()
    } else {
        text
    };
    match status {
        Ok(s) if s.success() => Ok(trimmed),
        Ok(s) => Err(format!("安装命令退出码 {s}：{trimmed}")),
        Err(e) => Err(format!("等待安装命令失败：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installable_languages_have_a_plan() {
        for lang in [Lang::TypeScript, Lang::Rust, Lang::Python, Lang::Go] {
            let p = plan(lang).unwrap_or_else(|| panic!("{} 应有一键安装计划", lang.id()));
            assert!(!p.program.is_empty());
            assert_eq!(p.display, server_spec::spec(lang).install.command.unwrap());
        }
    }

    #[test]
    fn manual_languages_have_no_plan() {
        assert!(plan(Lang::Java).is_none());
        assert!(plan(Lang::Dart).is_none());
    }

    #[test]
    fn plan_splits_arguments() {
        let p = plan(Lang::Go).expect("go 计划");
        assert_eq!(p.program, "go");
        assert!(p.args.contains(&"install".to_string()));
        assert!(p.args.iter().any(|a| a.contains("gopls@latest")));
    }

    /// 🔴-1：四条一键安装命令在本机必须解析到**存在的绝对路径**，或者给出明确 `Err`；
    /// 绝不允许把裸名原样拿给 `spawn`（Windows 上 `npm` 是 `npm.cmd`，必失败）。
    #[test]
    fn install_commands_resolve_to_real_executables_or_clear_error() {
        let (fresh, snapshot) = install_paths();
        for lang in [Lang::TypeScript, Lang::Python, Lang::Go, Lang::Rust] {
            let p = plan(lang).expect("可一键安装的语言必须有计划");
            match resolve_program(&p.program, &fresh, &snapshot) {
                Ok(r) => {
                    assert!(
                        r.path.is_absolute(),
                        "{} 必须解析到绝对路径，实得 {r:?}",
                        p.program
                    );
                    assert!(
                        r.path.is_file(),
                        "{} 解析到的文件必须存在，实得 {r:?}",
                        p.program
                    );
                    // 平台差异（本用例两端必须分别断言，否则 Unix 上必红）：
                    // - Windows：`which_in` 按 `PATHEXT` 补扩展名，裸名不是可执行文件（`npm` → `npm.cmd`），
                    //   故「不得回吐裸名」+ 扩展名/`shell_wrapped` 一并校验；
                    // - Unix：`which_in` **不补扩展名**（只认原名），`file_name == 裸名` 是**正常**结论，
                    //   判据改为「解析到绝对路径，且其父目录来自 PATH 中的某个目录」。
                    let file_name = r
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    #[cfg(windows)]
                    {
                        assert_ne!(
                            file_name, p.program,
                            "Windows 上不得回吐裸名（npm 不是可执行文件）"
                        );
                        let ext = r
                            .path
                            .extension()
                            .map(|e| e.to_string_lossy().to_ascii_lowercase())
                            .unwrap_or_default();
                        assert!(
                            matches!(ext.as_str(), "exe" | "cmd" | "bat" | "com"),
                            "Windows 必须解析到可执行扩展名，实得 {r:?}"
                        );
                        assert_eq!(
                            r.shell_wrapped,
                            matches!(ext.as_str(), "cmd" | "bat"),
                            ".cmd/.bat 必须走 cmd /C：{r:?}"
                        );
                    }
                    #[cfg(not(windows))]
                    {
                        // Unix 不补扩展名：`file_name == 裸名` **允许**（绝不是失败信号）
                        assert_eq!(
                            file_name, p.program,
                            "Unix 不补扩展名，file_name 就应是裸名：{r:?}"
                        );
                        assert!(!r.shell_wrapped, "cmd /C 包装只属于 Windows：{r:?}");
                        let parent = r.path.parent().expect("绝对路径必有父目录");
                        let parent = trim_dir_sep(parent);
                        let from_path = std::env::split_paths(&fresh)
                            .chain(std::env::split_paths(&snapshot))
                            .filter(|d| !d.as_os_str().is_empty())
                            .any(|d| trim_dir_sep(&d) == parent);
                        assert!(
                            from_path,
                            "{} 必须解析自 PATH 中的某个目录（父目录 {parent}；PATH: {fresh} | {snapshot}）",
                            p.program
                        );
                    }
                }
                Err(e) => assert!(
                    e.contains(&p.program) && e.contains("未找到"),
                    "解析失败必须写明「未找到 <name>」：{e}"
                ),
            }
        }
    }

    #[test]
    fn empty_program_name_is_a_clear_error() {
        assert!(resolve_program("   ", "", "").is_err());
        let e = resolve_program("definitely-missing-installer-xyz", "", "").unwrap_err();
        assert!(e.contains("未找到") && e.contains("无法执行安装"), "{e}");
    }

    /// 🔴-1 第 3 条：`.cmd`/`.bat` 在 Windows 上必须经 `cmd /C`（CreateProcess 跑不了批处理）。
    #[test]
    fn batch_programs_are_shell_wrapped_on_windows() {
        let tmp = tempfile::tempdir().unwrap();
        let bat = tmp.path().join("fake-install.cmd");
        std::fs::write(&bat, "@echo off\r\n").unwrap();
        let resolved = resolve_program(&bat.to_string_lossy(), "", "").expect("按路径必须解析得到");
        assert_eq!(resolved.path, bat);
        assert_eq!(
            resolved.shell_wrapped,
            cfg!(windows),
            ".cmd 只在 Windows 需要 cmd /C"
        );

        let mut cmd = build_command(&resolved, &["i".to_string(), "-g".to_string()]);
        let std_cmd = cmd.as_std_mut();
        let program = std_cmd.get_program().to_string_lossy().to_ascii_lowercase();
        let args: Vec<String> = std_cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        if cfg!(windows) {
            assert!(
                program == "cmd" || program.ends_with("cmd.exe"),
                "必须走 cmd：{program} {args:?}"
            );
            // `/D`（跳过 AutoRun）`/S` + `/C`：命令行本体由 `cmd_command_line` 拼成后经
            // `raw_arg` **原样**追加（见该函数与 `cmd_command_line_*` 单测），这里只看开关。
            assert_eq!(
                args.iter().take(3).map(String::as_str).collect::<Vec<_>>(),
                vec!["/D", "/S", "/C"],
                "{args:?}"
            );
        } else {
            assert!(program.ends_with("fake-install.cmd"), "{program}");
            assert_eq!(
                args,
                vec!["i".to_string(), "-g".to_string()],
                "Unix 不包 cmd /C：{args:?}"
            );
        }
    }

    /// 🟡-2：cmd 命令行构造——程序路径 / 参数含空格与 `&` 时，仍能还原出原本的 program 与 args。
    ///
    /// （`&^|<>` 在引号外会被 cmd 切成第二条命令；`^` 在引号内不是转义符，写成 `^^` 反而会改坏
    /// 路径里的 `^`——两处都靠这条用例钉死。）
    #[test]
    fn cmd_command_line_quotes_spaces_and_metacharacters() {
        let path = PathBuf::from(r"C:\Program Files\nodejs & tools\npm.cmd");
        let args = vec![
            "i".to_string(),
            "-g".to_string(),
            "pkg with space".to_string(),
        ];
        let line = cmd_command_line(&path, &args);
        // 每个 token 都在引号内：`&` 退回字面量，不会被当成命令分隔符
        assert!(line.starts_with(r#""#), "{line}");
        assert!(
            line.starts_with(&format!(r#""{}""#, path.to_string_lossy())),
            "程序路径必须整段包在引号里：{line}"
        );
        let (program, parsed) = split_cmd_line(&line);
        assert_eq!(program, path.to_string_lossy(), "{line}");
        assert_eq!(parsed, args, "{line}");

        // `^` 保持单个（不得写成 `^^`）
        let caret = cmd_command_line(PathBuf::from(r"C:\a^b\npm.cmd").as_path(), &[]);
        assert_eq!(split_cmd_line(&caret).0, r"C:\a^b\npm.cmd", "{caret}");

        // `%` / `!` 在 cmd 里无法转义：只保证「原样放入 + 记 warn」，不得静默改字符
        let risky = cmd_command_line(PathBuf::from(r"C:\a%TEMP%b\npm.cmd").as_path(), &[]);
        assert_eq!(split_cmd_line(&risky).0, r"C:\a%TEMP%b\npm.cmd", "{risky}");
    }

    /// 极简 cmd 命令行解析（只处理双引号包裹；只用于验证构造结果可还原）。
    fn split_cmd_line(line: &str) -> (String, Vec<String>) {
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut in_quotes = false;
        for ch in line.chars() {
            match ch {
                '"' => in_quotes = !in_quotes,
                ' ' if !in_quotes => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(ch),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        let program = out.remove(0);
        (program, out)
    }

    /// 🟡-2 端到端：目录名同时含**空格与 `&`** 的 `.cmd` 必须真的跑起来（不是「看起来加了引号」）。
    /// 这是 Windows 专属：Unix 不走 `cmd /C`。
    #[cfg(windows)]
    #[tokio::test]
    async fn batch_program_under_metacharacter_dir_still_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a & b");
        std::fs::create_dir_all(&dir).unwrap();
        let bat = dir.join("noisy name.cmd");
        std::fs::write(&bat, "@echo off\r\necho ran-ok\r\n").unwrap();
        let resolved = resolve_program(&bat.to_string_lossy(), "", "").expect("按路径必须解析得到");
        assert!(resolved.shell_wrapped, "`.cmd` 必须走 cmd /C：{resolved:?}");
        let mut cmd = build_command(&resolved, &["ignored-arg".to_string()]);
        cmd.stdin(std::process::Stdio::null());
        let out = tokio::time::timeout(Duration::from_secs(30), cmd.output())
            .await
            .expect("cmd /C 不得挂死")
            .expect("必须能启动 cmd");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            text.contains("ran-ok"),
            "含空格与 `&` 的路径必须跑得起来：{text}"
        );
    }

    /// 去尾部斜杠（Unix 侧校验「父目录来自 PATH 中的某个目录」时用）。
    #[cfg(not(windows))]
    fn trim_dir_sep(p: &Path) -> String {
        p.to_string_lossy()
            .trim_end_matches('/')
            .trim_end_matches('\\')
            .to_string()
    }

    /// 🟡-4：双管道死锁回归——子进程往 stderr 写爆 64KB 管道时，串行读（先 stdout 后 stderr）
    /// 会卡到超时；并发读必须在几十秒内正常收尾。
    #[cfg(windows)]
    #[tokio::test]
    async fn execute_drains_stdout_and_stderr_concurrently() {
        let tmp = tempfile::tempdir().unwrap();
        let bat = tmp.path().join("noisy.cmd");
        std::fs::write(
            &bat,
            "@echo off\r\nfor /L %%i in (1,1,20000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx>&2\r\necho done\r\n",
        )
        .unwrap();
        let p = InstallPlan {
            program: bat.to_string_lossy().to_string(),
            args: Vec::new(),
            display: bat.to_string_lossy().to_string(),
        };
        let out = tokio::time::timeout(Duration::from_secs(120), execute(&p, MIN_TIMEOUT))
            .await
            .expect("并发读两路必须不卡死（120s 内返回）")
            .expect("噪声脚本应正常退出");
        assert!(out.contains("done"), "stdout 必须被收走：{out}");
        assert!(
            out.contains("xxxx"),
            "stderr 也必须被收走（不截断到 stdout）：{out}"
        );
    }
}
