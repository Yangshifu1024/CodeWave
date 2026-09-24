use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 命令默认超时（秒）。
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// 命令超时上限（秒）。
pub const MAX_TIMEOUT_SECS: u64 = 600;
/// 内存缓冲阈值：超过即把已有缓冲整体落盘，后续输出直写 spill 文件。
const SPILL_THRESHOLD: usize = 96 * 1024;
/// full_output=false 时给模型保留的尾部字符数（ally 移植：输出不是答案时只留尾部 + 信号行）
pub const MODEL_TAIL_CHARS: usize = 2_000;
/// full_output=true 时给模型保留的尾部字符数（原始行为）
pub const FULL_TAIL_CHARS: usize = 16_000;

/// command 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 要执行的完整命令文本（交由选定/探测的 shell 解释）。
    command: String,
    /// 工作区相对的执行目录；缺省为项目 temps/ 目录（临时会话为工作区根）。
    #[serde(default)]
    cwd: Option<String>,
    /// 超时秒数，默认 120、上限 600。
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// 必填（ally 733b581）：让模型预判「输出本身是不是答案」；缺省 false 以安全降级（兼容旧载荷）
    #[serde(default)]
    full_output: bool,
}

/// command 工具：在工作区内执行 shell 命令。
/// 入参为 command（必填）+ cwd / timeoutSeconds / fullOutput；声明为 ReadOnly 分级，但执行前
/// 一律过 fence：写目标与危险模式由 L1/L2/L3 判定，Confirm 类命令走审批弹窗（FullAccess 档跳过
/// 确认但灾难级仍拦截）。「始终允许本项目」白名单命中可直接放行（灾难级除外）。
/// 输出经瘦身回传：凡截断必落盘，信号行携带退出码与 spill 文件路径。
pub struct CommandTool;

// ---------- shell 探测与选择 ----------

/// 进程级缓存的 shell 自动探测结果（selection 为 None/"auto" 时使用）。
static SHELL: OnceLock<Shell> = OnceLock::new();
/// resolve_shell 结果缓存：key = selection（None 归一 "auto"），selection 变化即重解析。
static RESOLVED: RwLock<Option<(String, Shell)>> = RwLock::new(None);
/// detect_all_shells 进程内缓存：探测含 PATH 扫描与 wsl --status 有界探测，只做一次。
static PROBED: OnceLock<Vec<ShellInfo>> = OnceLock::new();

/// 可选 shell id 的固定顺序（探测结果按此排序；`auto` 仅为配置内部值，不入探测列表）。
pub const SHELL_IDS: &[&str] = &[
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

/// 探测到的单个 shell 条目（设置页选择列表条目；字段名即 wire 契约）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShellInfo {
    /// 稳定 id：bash / git_bash / zsh / fish / sh / powershell / pwsh / cmd / wsl
    pub id: String,
    /// 展示名
    pub name: String,
    /// 探测到的可执行文件路径（恒在型 shell 缺文件时为 None）
    pub path: Option<String>,
    /// 语法家族：posix | powershell | cmd | wsl
    pub kind: String,
    /// 受限 shell（cmd / fish / wsl）：语法子集或非本机原生环境
    pub limited: bool,
    /// 该项是否即「自动」探测的选中结果（与 detect_shell 同源判定，供前端标注默认项）
    pub auto: bool,
}

/// 本机可用的 shell 类型。
#[derive(Debug, Clone, PartialEq)]
pub enum Shell {
    /// bash -lc/-c（macOS / Linux / Windows Git Bash）
    Bash {
        /// 是否以 login shell 运行（login 会加载用户 PATH，如 homebrew）
        login: bool,
    },
    /// zsh（macOS 默认）
    Zsh,
    /// fish（语法非 POSIX 兼容）
    Fish,
    /// sh（最小 POSIX，可能是 dash/ash）
    Sh,
    /// Windows PowerShell 5.1（System32 恒在；版次命名与 Pwsh 的 Core 对称）
    // 不叫 WindowsPowerShell：clippy enum_variant_names 认为它仍以枚举名 Shell 结尾
    PowerShellDesktop,
    /// PowerShell 7+（pwsh）
    Pwsh,
    /// Windows 命令提示符（受限语法）
    Cmd,
    /// WSL 内 bash（命令在 Linux 环境执行；不做写盘探测，执行时才走 wsl.exe）
    Wsl,
}

/// shell id 静态元数据（展示名 / kind / limited）：探测与测试共用的单一事实源；`auto` 与未知 id 返回 None。
pub fn shell_meta(id: &str) -> Option<(&'static str, &'static str, bool)> {
    match id {
        "bash" => Some(("bash", "posix", false)),
        "git_bash" => Some(("Git Bash", "posix", false)),
        "zsh" => Some(("zsh", "posix", false)),
        "fish" => Some(("fish", "posix", true)),
        "sh" => Some(("sh", "posix", false)),
        "powershell" => Some(("PowerShell", "powershell", false)),
        "pwsh" => Some(("PowerShell Core (pwsh)", "powershell", false)),
        "cmd" => Some(("cmd", "cmd", true)),
        "wsl" => Some(("WSL (bash)", "wsl", true)),
        _ => None,
    }
}

impl Shell {
    /// 供 system prompt `<environment>` 段与工具结果的完整环境说明：shell 名称与家族、
    /// 实际调用方式、≥3 条该家族的语法要点；WSL 变体额外说明 Linux 路径映射。
    pub fn describe(&self) -> String {
        match self {
            Shell::Bash { login } => {
                let flag = if *login { "-lc" } else { "-c" };
                let login_note = if *login {
                    "，登录 shell（-l 加载 profile，PATH 来自用户环境）"
                } else {
                    ""
                };
                format!(
                    "bash（POSIX shell 家族{login_note}）。调用方式：`bash {flag} <command>`。语法要点：\n\
                     - 路径分隔符 `/`；Windows Git Bash 可用 `/c/...` 形式访问 `C:\\...`\n\
                     - 环境变量 `$VAR`/`${{VAR}}`，导出用 `export VAR=value`；命令替换 `$(...)`\n\
                     - 单引号内不展开、双引号内展开变量；重定向 `>` `>>` `2>` `<`；管道 `|`；命令链 `&&` `||` `;`"
                )
            }
            Shell::Zsh => "zsh（POSIX 兼容 shell，macOS 默认）。调用方式：`zsh -c <command>`。语法要点：\n\
                 - 路径分隔符 `/`；环境变量 `$VAR`/`${VAR}`，导出用 `export VAR=value`；命令替换 `$(...)`\n\
                 - 数组下标从 1 开始（与 bash 的 0 不同）；glob 更激进（`**` 递归、未定义变量引用直接报错）\n\
                 - 单引号内不展开、双引号内展开；重定向/管道/`&&` `||` `;` 与 POSIX 一致"
                .to_string(),
            Shell::Fish => "fish（friendly interactive shell；语法非 POSIX 兼容）。调用方式：`fish -c <command>`。语法要点：\n\
                 - 变量赋值用 `set VAR value`、引用用 `$VAR`；不支持 `VAR=value cmd` 前缀语法（改用 `env VAR=value cmd`）\n\
                 - 命令替换用 `(...)`；命令链用 `; and` / `; or`（3.0+ 也支持 `&&` `||`）\n\
                 - 路径分隔符 `/`；单/双引号语义与 POSIX 相同；重定向 `>` `>>` `2>` 与管道 `|` 可用"
                .to_string(),
            Shell::Sh => "sh（最小 POSIX shell；实现可能是 dash/ash 等）。调用方式：`sh -c <command>`。语法要点：\n\
                 - 环境变量 `$VAR`、导出用 `export VAR=value`；命令替换 `$(...)`\n\
                 - 避免 bashism：数组、`[[ ]]`、`==` 比较等不可用\n\
                 - 路径分隔符 `/`；单引号内不展开、双引号内展开；重定向/管道/`&&` `||` `;` 同 POSIX"
                .to_string(),
            Shell::PowerShellDesktop => "PowerShell（Windows PowerShell 5.1，.NET 对象管道 shell）。调用方式：`powershell -NoProfile -Command <command>`。语法要点：\n\
                 - 变量 `$var = value`（无需 export）；路径分隔符 `\\`（多数场景兼容 `/`）\n\
                 - 双引号内展开变量、单引号内字面；转义符是反引号；命令替换用 `$(...)`\n\
                 - 管道传递对象而非文本；`&&`/`||` 链 5.1 不支持（用 `;` 顺序执行并检查 `$LASTEXITCODE`）\n\
                 - 常用别名（ls/cat/rm）指向 .NET cmdlet，参数语义与 POSIX 工具不同"
                .to_string(),
            Shell::Pwsh => "PowerShell Core（pwsh，PowerShell 7+ 跨平台 .NET shell）。调用方式：`pwsh -NoProfile -Command <command>`。语法要点：\n\
                 - 变量 `$var = value`（无需 export）；路径分隔符 Windows 下 `\\`（多数场景兼容 `/`）\n\
                 - 双引号内展开变量、单引号内字面；管道传递对象而非文本\n\
                 - `&&`/`||` 管道链 7+ 支持；常用别名（ls/cat/rm）指向 .NET cmdlet，参数语义与 POSIX 工具不同"
                .to_string(),
            Shell::Cmd => "cmd（Windows 命令提示符，受限语法）。调用方式：`cmd /C <command>`。语法要点：\n\
                 - 路径分隔符 `\\`；环境变量 `%VAR%`，赋值用 `set VAR=value`（`=` 两侧不要有空格）\n\
                 - 只有双引号（无单引号）；转义符 `^`；重定向 `>` `>>` `2>` `<`；命令链 `&` `&&` `||`\n\
                 - 内建命令（dir/copy/del/ren）非 POSIX 工具；`*` 通配仅部分命令展开；无 `$(...)` 命令替换"
                .to_string(),
            Shell::Wsl => "WSL (bash)（命令经 wsl.exe 在 Linux 环境内执行）。调用方式：`wsl.exe --cd <dir> -e bash -lc <command>`。语法要点：\n\
                 - 命令在 WSL Linux 内以 bash 登录 shell 运行，工具链与环境变量均为 Linux 侧（PATH、apt 等）\n\
                 - 文件路径需用 `/mnt/<盘符>/...` 形式（如 `D:\\Work` → `/mnt/d/Work`）\n\
                 - 执行 cwd 由系统自动映射（`--cd` 直接接收 Windows 路径，无需手工换算）"
                .to_string(),
        }
    }
}

/// 全局 shell 描述（system prompt 环境段消费）：按配置 selection 解析实际 shell 后取完整描述。
pub fn shell_description(selection: Option<&str>) -> String {
    resolve_shell(selection).describe()
}

/// 检测并缓存自动 shell：Unix 恒为 bash（探测 login 可用性）；
/// Windows 先找 Git Bash，找不到回退 PowerShell。
fn detect_shell() -> &'static Shell {
    SHELL.get_or_init(|| {
        #[cfg(unix)]
        {
            Shell::Bash {
                login: probe_bash_login(),
            }
        }
        #[cfg(not(unix))]
        {
            if find_windows_bash().is_some() {
                Shell::Bash { login: false }
            } else {
                Shell::PowerShellDesktop
            }
        }
    })
}

/// 自动探测选中的 shell id（供 ShellInfo.auto 标注；语义与 detect_shell 同源）：
/// Unix 恒 bash；Windows 先 Git Bash 回退 powershell。
#[cfg(not(unix))]
fn auto_shell_id() -> &'static str {
    if find_windows_bash().is_some() {
        "git_bash"
    } else {
        "powershell"
    }
}

/// Unix 版（恒 bash；见 detect_shell）。
#[cfg(unix)]
fn auto_shell_id() -> &'static str {
    "bash"
}

/// 全量探测本机可用 shell（首次调用后进程内缓存；结果按 `SHELL_IDS` 固定顺序，
/// 缺席 id 不出现）。供设置页选择列表与 resolve_shell 可用性判定消费；
/// 纯探测、有界超时、不 panic；`auto` 字段标注即自动探测默认项。
pub fn detect_all_shells() -> Vec<ShellInfo> {
    PROBED
        .get_or_init(|| {
            SHELL_IDS
                .iter()
                .filter_map(|id| {
                    let (name, kind, limited) = shell_meta(id)?;
                    let path = probe_shell_path(id);
                    Some(ShellInfo {
                        id: (*id).to_string(),
                        name: name.to_string(),
                        path: path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                        kind: kind.to_string(),
                        limited,
                        auto: *id == auto_shell_id(),
                    })
                })
                .collect()
        })
        .clone()
}

/// 按用户选择解析实际 Shell：
/// - None / "auto" → 自动探测（`detect_shell`：Unix bash；Windows 先 Git Bash 回退 PowerShell）；
/// - 显式 id → 对应变体，但该 id 不在本机探测列表时回退自动探测。
///
/// 结果按 selection 值缓存（selection 变化立即生效；重复调用零探测开销）。
pub fn resolve_shell(selection: Option<&str>) -> Shell {
    let key = selection.unwrap_or("auto").trim().to_string();
    if let Some((k, shell)) = RESOLVED.read().unwrap().as_ref() {
        if *k == key {
            return shell.clone();
        }
    }
    let available: Vec<String> = detect_all_shells().into_iter().map(|s| s.id).collect();
    let available: Vec<&str> = available.iter().map(String::as_str).collect();
    let shell = resolve_from_available(&key, &available, detect_shell().clone());
    *RESOLVED.write().unwrap() = Some((key, shell.clone()));
    shell
}

/// 纯解析逻辑（存在性以 `available` 注入，测试不依赖本机环境）：
/// 空/"auto" → fallback；未知 id 或探测列表缺席 → fallback；命中 → 对应变体。
pub fn resolve_from_available(selection: &str, available: &[&str], fallback: Shell) -> Shell {
    if selection.is_empty() || selection == "auto" {
        return fallback;
    }
    match selection_to_shell(selection) {
        Some(shell) if available.contains(&selection) => shell,
        _ => fallback,
    }
}

/// selection id → Shell 变体的纯映射（存在性不在本函数判定；`auto`/未知 → None）。
/// bash 在 Unix 上按既有语义探测 login；git_bash 恒 login（Git Bash 以 `-lc` 加载用户 PATH）。
pub fn selection_to_shell(selection: &str) -> Option<Shell> {
    match selection {
        "bash" => Some(Shell::Bash {
            login: bash_login_for_selection(),
        }),
        "git_bash" => Some(Shell::Bash { login: true }),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "sh" => Some(Shell::Sh),
        "powershell" => Some(Shell::PowerShellDesktop),
        "pwsh" => Some(Shell::Pwsh),
        "cmd" => Some(Shell::Cmd),
        "wsl" => Some(Shell::Wsl),
        _ => None,
    }
}

/// 显式选择 bash 时的 login 探测（Unix 沿用 probe_bash_login；Windows 恒 false）。
fn bash_login_for_selection() -> bool {
    #[cfg(unix)]
    {
        probe_bash_login()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// 启动拼接（command / service 工具共用的单一事实源）：程序路径 + 解释器参数。
/// WSL 变体的 cwd 传原始 Windows 路径（`--cd` 由系统自动映射到 `/mnt/<盘符>/...`）。
pub fn shell_invocation(
    shell: &Shell,
    command: &str,
    cwd: &std::path::Path,
) -> (String, Vec<String>) {
    (shell_program(shell), shell_args(shell, command, cwd))
}

/// Shell 对应的可执行程序（Windows bash 用探测到的 Git Bash 完整路径，缺失回退 powershell）。
fn shell_program(shell: &Shell) -> String {
    match shell {
        Shell::Bash { .. } => {
            #[cfg(unix)]
            {
                "bash".to_string()
            }
            #[cfg(not(unix))]
            {
                find_windows_bash()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "powershell".into())
            }
        }
        Shell::Zsh => "zsh".to_string(),
        Shell::Fish => "fish".to_string(),
        Shell::Sh => "sh".to_string(),
        Shell::PowerShellDesktop => "powershell".to_string(),
        Shell::Pwsh => "pwsh".to_string(),
        Shell::Cmd => "cmd".to_string(),
        Shell::Wsl => "wsl.exe".to_string(),
    }
}

/// 按 Shell 变体拼接解释器参数（纯函数，测试直接断言字符串）。
pub fn shell_args(shell: &Shell, command: &str, cwd: &std::path::Path) -> Vec<String> {
    match shell {
        Shell::Bash { login } => {
            vec![
                if *login { "-lc".into() } else { "-c".into() },
                command.into(),
            ]
        }
        Shell::Zsh | Shell::Fish | Shell::Sh => vec!["-c".into(), command.into()],
        Shell::PowerShellDesktop | Shell::Pwsh => {
            vec!["-NoProfile".into(), "-Command".into(), command.into()]
        }
        Shell::Cmd => vec!["/C".into(), command.into()],
        Shell::Wsl => vec![
            "--cd".into(),
            cwd.to_string_lossy().into_owned(),
            "-e".into(),
            "bash".into(),
            "-lc".into(),
            command.into(),
        ],
    }
}

/// 单个 shell id 的本机探测（返回可执行路径；None = 本机不可用）。
fn probe_shell_path(id: &str) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        windows_probe(id)
    }
    #[cfg(unix)]
    {
        unix_probe(id)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = id;
        None
    }
}

#[cfg(unix)]
fn probe_bash_login() -> bool {
    // login shell 能捕获用户 PATH（homebrew 等）；以 5s 预算探测一次（有界轮询——
    // 卡死的 shell profile 不能让 shell 检测永远阻塞），结果缓存
    match std::process::Command::new("bash")
        .arg("-lc")
        .arg("echo ok")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => wait_with_timeout(child, Duration::from_secs(5)),
        Err(_) => false,
    }
}

/// 有界等待子进程退出（spawn + poll try_wait 轮询；超时 kill 并回收）。
/// 不引入新依赖；探测类子进程绝不无限阻塞。
fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false, // try_wait 本身失败（如 kill 后的竞态）：视为探测失败
        }
    }
}

/// 在常见安装位置与 PATH 的 Git 目录中查找 Windows Git Bash
/// （探测与执行共用 `windows_probe("git_bash")` 单一事实源，避免两处漂移）。
#[cfg(not(unix))]
fn find_windows_bash() -> Option<std::path::PathBuf> {
    windows_probe("git_bash")
}

/// PATH 目录列表（Windows `;`、Unix `:`；空项忽略）。
fn path_dirs() -> Vec<std::path::PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|d| !d.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 在目录列表中查找可执行文件（Windows 自动优先补 .exe；PATHEXT 语义简化为 .exe）。
fn find_exe_in(dirs: &[std::path::PathBuf], exe: &str) -> Option<std::path::PathBuf> {
    let mut names: Vec<String> = Vec::with_capacity(2);
    if cfg!(windows) && !exe.to_ascii_lowercase().ends_with(".exe") {
        names.push(format!("{exe}.exe"));
    }
    names.push(exe.to_string());
    dirs.iter().find_map(|d| {
        names.iter().find_map(|n| {
            let p = d.join(n);
            p.is_file().then_some(p)
        })
    })
}

/// 探测固定候选路径中第一个存在的（Unix /bin、/usr/bin 布局）。
#[cfg(unix)]
fn first_existing(paths: &[&str]) -> Option<std::path::PathBuf> {
    paths
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file())
}

/// Unix 侧探测：/bin、/usr/bin、/usr/local/bin 优先，其次 PATH。
#[cfg(unix)]
fn unix_probe(id: &str) -> Option<std::path::PathBuf> {
    match id {
        "bash" => first_existing(&["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"])
            .or_else(|| find_exe_in(&path_dirs(), "bash")),
        "zsh" => first_existing(&["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"])
            .or_else(|| find_exe_in(&path_dirs(), "zsh")),
        "fish" => first_existing(&["/bin/fish", "/usr/bin/fish", "/usr/local/bin/fish"])
            .or_else(|| find_exe_in(&path_dirs(), "fish")),
        "sh" => first_existing(&["/bin/sh", "/usr/bin/sh"]),
        _ => None,
    }
}

/// Windows 侧探测：PATH 扫描 + 常见安装位置；powershell/cmd 恒在（System32）；
/// wsl 需 `wsl.exe --status` 在 3s 预算内成功才列入。
#[cfg(windows)]
fn windows_probe(id: &str) -> Option<std::path::PathBuf> {
    match id {
        // System32 的 bash.exe 是 WSL stub（走 wsl id）；Git 目录下的 bash 走 git_bash id
        "bash" => {
            let dirs: Vec<std::path::PathBuf> = path_dirs()
                .into_iter()
                .filter(|d| {
                    let s = d.to_string_lossy().to_ascii_lowercase();
                    !s.contains("system32") && !s.contains("git")
                })
                .collect();
            find_exe_in(&dirs, "bash")
        }
        "git_bash" => {
            let mut cands: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(pf) = std::env::var("ProgramFiles") {
                let pf = std::path::PathBuf::from(pf);
                cands.push(pf.join(r"Git\bin\bash.exe"));
                cands.push(pf.join(r"Git\usr\bin\bash.exe"));
            }
            if let Ok(la) = std::env::var("LocalAppData") {
                cands.push(std::path::PathBuf::from(la).join(r"Programs\Git\bin\bash.exe"));
            }
            cands.push(r"C:\Program Files\Git\bin\bash.exe".into());
            cands.push(r"C:\Program Files\Git\usr\bin\bash.exe".into());
            cands.push(r"C:\Git\bin\bash.exe".into());
            cands.into_iter().find(|p| p.is_file()).or_else(|| {
                let git_dirs: Vec<std::path::PathBuf> = path_dirs()
                    .into_iter()
                    .filter(|d| d.to_string_lossy().to_ascii_lowercase().contains("git"))
                    .collect();
                find_exe_in(&git_dirs, "bash")
            })
        }
        "pwsh" => {
            let mut cands: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(pf) = std::env::var("ProgramFiles") {
                cands.push(std::path::PathBuf::from(pf).join(r"PowerShell\7\pwsh.exe"));
            }
            cands.push(r"C:\Program Files\PowerShell\7\pwsh.exe".into());
            cands
                .into_iter()
                .find(|p| p.is_file())
                .or_else(|| find_exe_in(&path_dirs(), "pwsh"))
        }
        // 恒在（System32 随系统分发）；路径仅信息用途，真缺失时 spawn 显式报错
        "powershell" => Some(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".into()),
        "cmd" => Some(r"C:\Windows\System32\cmd.exe".into()),
        "wsl" => {
            let sys = std::path::PathBuf::from(r"C:\Windows\System32\wsl.exe");
            let exe = match find_exe_in(&path_dirs(), "wsl") {
                Some(p) => p,
                None if sys.is_file() => sys,
                None => return None,
            };
            wsl_status_ok(&exe).then_some(exe)
        }
        _ => None,
    }
}

/// WSL 可用性探测：`wsl.exe --status` 3s 预算内成功才算已安装可用。
#[cfg(windows)]
fn wsl_status_ok(exe: &std::path::Path) -> bool {
    let Ok(child) = std::process::Command::new(exe)
        .arg("--status")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
    else {
        return false;
    };
    wait_with_timeout(child, Duration::from_secs(3))
}

// ---------- 工具实现 ----------

#[async_trait::async_trait]
impl Tool for CommandTool {
    fn name(&self) -> &'static str {
        "command"
    }
    fn description(&self) -> &'static str {
        "执行 shell 命令（默认 cwd：项目会话为项目临时目录，自由会话为工作区根；可用 cwd 指定）。安全围栏会检查写目标与危险模式；高危命令需用户审批。超时上限 600 秒。fullOutput 经验法则：当输出本身就是你需要的完整答案时（git diff/status/log、ls、cat、grep）设 true；构建/测试/安装类运行设 false——你将拿到尾部输出加一行信号（含退出码与完整输出的落盘文件路径）。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["command", "fullOutput"],
  "properties": {
    "command": {"type": "string"},
    "cwd": {"type": "string", "description": "相对工作区，默认根目录"},
    "timeoutSeconds": {"type": "integer", "description": "默认 120，上限 600"},
    "fullOutput": {"type": "boolean", "description": "true = 保留完整输出尾部 16000 字符；false = 尾部 2000 字符 + 信号行（退出码、总行数、落盘文件路径）。运行前先判断：输出本身是不是答案？"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        if args.command.trim().is_empty() {
            return ToolOutcome::err("E_ARGS", "命令为空");
        }
        let roots = ctx.write_roots();
        // 项目会话：默认 cwd = 项目托管目录（中立区；代码目录一律用绝对路径到达）；临时会话 = 工作区根
        let default_cwd = ctx
            .rt
            .project_dir
            .clone()
            .unwrap_or_else(|| roots.workspace.clone());
        let cwd = match &args.cwd {
            Some(c) => match crate::tools::pathutil::resolve_read(&roots, c) {
                Ok(p) => p,
                Err((code, m)) => return ToolOutcome::err(&code, m),
            },
            None => default_cwd,
        };

        // fence + 审批（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md) 权限档：FullAccess 跳过确认弹窗，灾难级仍拦截）
        let mode = ctx.approval_mode();
        let mut policy = ctx.fence_policy();
        // 目标档执行期（判定见 core/agent/goal.rs 的 ledger_gate）：免确认的合法性由**账本**承担——
        // 账本内放行、账本外即拒，全程不问人（零提问）；灾难 / 高危级直接硬拦并请求硬停。
        // 账本判定的作用域 runtime：子代理不持有目标状态，判定取根会话的账本（见 goal_gate_rt）
        let goal_rt = crate::core::agent::goal::goal_gate_rt(&ctx.core, &ctx.rt);
        let goal_exec = crate::core::agent::goal::goal_execute_phase(&goal_rt);
        if goal_exec {
            policy.confirm_inside_writes = true;
        }
        // 需确认级（工作区内写 / 工作区外新建）携带的写目标：免确认后仍要过账本
        let mut confirm_path: Option<String> = None;
        match crate::safety::fence::check_command_policy(&args.command, &cwd, &roots, policy) {
            crate::safety::fence::Verdict::Allow => {}
            crate::safety::fence::Verdict::Block { code, message } => {
                // 目标档执行期：L1/L2 硬拦（删除黑名单、符号链接逃逸等）记入 blocked 并请求硬停
                if goal_exec {
                    crate::core::agent::goal::goal_hard_block(
                        &goal_rt,
                        format!("命令被安全围栏硬拦：{message}"),
                    );
                }
                return ToolOutcome::err(&code, message);
            }
            crate::safety::fence::Verdict::Confirm(reason) => {
                use crate::safety::fence::ConfirmReason;
                if goal_exec {
                    match &reason {
                        // 灾难 / 高危：硬拦 + 记 blocked + 请求硬停（无人值守，不弹审批）
                        ConfirmReason::Disaster(why) | ConfirmReason::HighRisk(why) => {
                            crate::core::agent::goal::goal_hard_block(
                                &goal_rt,
                                format!("高危命令被硬拦：{why}"),
                            );
                            return ToolOutcome::err(
                                "E_COMMAND_BLOCKED",
                                format!(
                                    "目标档执行期高危命令已硬拦（{why}）：该命令不在本次目标的授权范围内。本轮执行已请求停止，请由用户确认后再继续。"
                                ),
                            );
                        }
                        // 需确认级：免确认，落回下方账本判定（账本内放行、账本外拒绝）
                        ConfirmReason::InsideWrite(t) | ConfirmReason::OutsideCreate(t) => {
                            confirm_path = Some(t.clone());
                        }
                    }
                } else if mode == crate::core::prefs::ApprovalMode::FullAccess {
                    // 防御性兜底：灾难级在 fence 内已按 Block 处理（approval_enabled=true 时不可达）
                    if let ConfirmReason::Disaster(why) = &reason {
                        return ToolOutcome::err(
                            "E_COMMAND_BLOCKED",
                            format!("灾难性命令已拦截：{why}"),
                        );
                    }
                } else {
                    // [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：「始终允许本项目」白名单命中 → 免询问；灾难级永不适用白名单，仍需确认。
                    // 匹配 key = cwd + 完整命令文本（cwd 是会话固定的 temps/ 目录且随项目走 → 白名单不跨项目）。
                    // 已知竞态：对 host save_config 的读改写 / 并发「始终允许」写入，后写者胜
                    // （与 toggle_skill 同为既有模式）；窗口极小，留待将来统一修复。
                    let is_disaster = matches!(reason, ConfirmReason::Disaster(_));
                    let allow_key = format!("{}\u{1}{}", cwd.display(), args.command.trim());
                    let allowlisted = !is_disaster
                        && ctx
                            .core
                            .cfg
                            .read()
                            .unwrap()
                            .approval
                            .command_allowlist
                            .iter()
                            .any(|c| c.trim() == allow_key);
                    if allowlisted {
                        tracing::info!(
                            "session {} 命令命中白名单直接放行：{}",
                            ctx.rt.id,
                            args.command
                        );
                    } else {
                        let (title, detail) = match reason {
                            ConfirmReason::OutsideCreate(target) => (
                                "工作区外写入确认".into(),
                                format!("命令将创建工作区外的新路径：{target}\n\n{}", args.command),
                            ),
                            ConfirmReason::HighRisk(why) => {
                                (format!("高危命令确认：{why}"), args.command.clone())
                            }
                            ConfirmReason::Disaster(why) => {
                                (format!("灾难性命令确认：{why}"), args.command.clone())
                            }
                            ConfirmReason::InsideWrite(target) => (
                                "文件写入确认".into(),
                                format!("命令将修改工作区文件：{target}\n\n{}", args.command),
                            ),
                        };
                        // [docs/arithmetic-fixes-batch](../../../../docs/arithmetic-fixes-batch.md)：灾难级确认永不搭自动确认超时
                        let auto_confirm = crate::safety::approval::effective_auto_confirm(
                            ctx.core.cfg.read().unwrap().approval.auto_confirm,
                            is_disaster,
                        );
                        let outcome = crate::safety::approval::confirm(
                            &ctx.rt,
                            &ctx.core.sink,
                            crate::safety::approval::ApprovalRequest {
                                title,
                                detail,
                                allow_always: !is_disaster,
                                auto_confirm,
                            },
                            &ctx.cancel,
                        )
                        .await;
                        if !outcome.approved {
                            return ToolOutcome::err("E_APPROVAL_DENIED", "用户拒绝或未响应该命令");
                        }
                        // 「始终允许本项目」：把 cwd+命令写入白名单并持久化（去重；失败不阻塞已批准的执行）
                        if outcome.always {
                            let cmd = format!("{}\u{1}{}", cwd.display(), args.command.trim());
                            let mut cfg = ctx.core.cfg.write().unwrap();
                            if !cfg.approval.command_allowlist.contains(&cmd) {
                                cfg.approval.command_allowlist.push(cmd);
                                let snapshot = cfg.clone();
                                drop(cfg);
                                if let Err(e) = snapshot.save() {
                                    tracing::warn!("session {} 白名单持久化失败：{e}", ctx.rt.id);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 目标档执行期：账本判定（程序名 + 需确认级携带的写目标）——越界即拒，不弹审批
        if goal_exec {
            use crate::core::agent::goal::{LedgerTarget, ledger_denial_message, ledger_gate};
            let program = match crate::core::agent::goal::goal_command_program(&args.command) {
                Ok(program) => program,
                Err(message) => return ToolOutcome::err("E_GOAL_COMMAND_SHAPE", message),
            };
            let mut checks = vec![(LedgerTarget::Program(program.as_str()), program.clone())];
            if let Some(p) = confirm_path.as_deref() {
                checks.push((LedgerTarget::Path(p), p.to_string()));
            }
            for (target, label) in checks {
                if let Err(code) = ledger_gate(&goal_rt, target) {
                    return ToolOutcome::err(code, ledger_denial_message(&goal_rt, &label));
                }
            }
        }

        // 超时预算
        let timeout = Duration::from_secs(
            args.timeout_seconds
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS),
        );

        // 执行 shell = 配置 selection 经 resolve_shell（进程内缓存）；与 system prompt 环境段同一选择
        let selection = ctx.core.cfg.read().unwrap().shell.selection.clone();
        let shell = resolve_shell(selection.as_deref());
        run_process(&args.command, &cwd, timeout, args.full_output, &shell, ctx).await
    }
}

/// 在选定/探测的 shell 中执行命令并收集输出（fence/审批之后的具体执行体）。
/// shell 由调用方按 `ConfigState.shell.selection` 经 `resolve_shell` 解析后传入。
async fn run_process(
    command: &str,
    cwd: &std::path::Path,
    timeout: Duration,
    full_output: bool,
    shell: &Shell,
    ctx: &ToolCtx,
) -> ToolOutcome {
    let (program, argv) = shell_invocation(shell, command, cwd);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&argv);
    cmd.current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // 批次取消盲区修复：工具任务被 abort（收口 abort_all）时子进程随之回收，
        // 不留孤儿进程；正常取消路径仍走 terminate_tree 主动杀进程树
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
        Err(e) => return ToolOutcome::err("E_IO", format!("进程启动失败：{e}")),
    };
    let Some(pid) = child.id() else {
        return ToolOutcome::err("E_IO", "进程启动后立即退出");
    };

    // 输出收集：stdout/stderr 合流，节流推送进度 + 体积上限
    let collector = std::sync::Arc::new(std::sync::Mutex::new(Collector {
        buf: String::new(),
        bytes_since_emit: 0,
        last_emit: std::time::Instant::now(),
        progress_emitted: false,
        spilled: false,
        spill_path: None,
        total_bytes: 0,
        total_lines: 0,
        last_chunk_ended_newline: true,
    }));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    async fn pump<R: tokio::io::AsyncRead + Unpin>(
        mut s: R,
        tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        // 每条流各持一个清洗器：剥 ANSI/控制序列 + 跨块安全解码（不把多字节字符切出替换符），
        // 见 tools/sanitize.rs 与 [docs/command-output-ansi-sanitize-plan](../../../../docs/command-output-ansi-sanitize-plan.md)
        let mut clean = crate::tools::sanitize::OutputSanitizer::new();
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = clean.push(&buf[..n]);
                    if text.is_empty() {
                        continue; // 纯转义/纯控制字符块：不发空事件
                    }
                    if tx.send(text).is_err() {
                        break;
                    }
                }
            }
        }
        // 流结束：把清洗器里的残留（半截多字节兜底）发出去，不丢尾巴
        let rest = clean.finish();
        if !rest.is_empty() {
            let _ = tx.send(rest);
        }
    }
    tokio::spawn(pump(stdout, line_tx.clone()));
    tokio::spawn(pump(stderr, line_tx.clone()));
    drop(line_tx);

    let sink = ctx.core.sink.clone();
    let session = ctx.rt.id.clone();
    let batch = ctx.batch_id.clone();
    let call_index = ctx.call_index;
    // 工具名：本发射点仅属 command 工具（run_process 无 NormalizedCall 上下文）；
    // 字面量与 Tool::name()（:557-558）人工保持同步
    let tool_name = "command".to_string();
    let pump_collector = collector.clone();
    let pump = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let mut c = pump_collector.lock().unwrap();
            c.total_bytes += line.len();
            c.total_lines += line.matches('\n').count() as u64;
            c.last_chunk_ended_newline = line.ends_with('\n');
            if c.buf.len() < SPILL_THRESHOLD {
                c.buf.push_str(&line);
            } else if !c.spilled {
                c.spilled = true;
                let dir = crate::core::config::data_dir()
                    .join("tmp")
                    .join("cmd-output");
                let _ = std::fs::create_dir_all(&dir);
                let p = dir.join(format!("{}.txt", uuid::Uuid::new_v4()));
                let _ = std::fs::write(&p, &c.buf);
                c.spill_path = Some(p);
            }
            if let Some(p) = &c.spill_path {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(p)
                {
                    let _ = f.write_all(line.as_bytes());
                }
            }
            c.bytes_since_emit += line.len();
            // 首帧**不等 2KB**：长命令（编译/装包）一开始就要有卡片，否则输出攒够之前界面是空的；
            // 其后仍按「≥2KB 且距上次 ≥200ms」节流，避免刷帧
            let first_frame = !c.progress_emitted;
            if first_frame
                || (c.bytes_since_emit >= 2048
                    && c.last_emit.elapsed() >= Duration::from_millis(200))
            {
                c.progress_emitted = true;
                c.bytes_since_emit = 0;
                c.last_emit = std::time::Instant::now();
                let tail: String = c
                    .buf
                    .chars()
                    .rev()
                    .take(2000)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                sink.channel_frame(
                    &session,
                    &crate::core::agent::Frame::ToolProgress {
                        batch: batch.clone(),
                        index: call_index,
                        chunk: tail,
                        name: tool_name.clone(),
                    },
                );
            }
        }
    });

    // 等待：完成 / 超时 / 取消（TERM → 3s → KILL 进程组）
    let wait = child.wait();
    let status = tokio::select! {
        s = wait => match s {
            Ok(st) => st,
            Err(e) => {
                terminate_tree(pid);
                return ToolOutcome::err("E_IO", format!("等待进程失败：{e}"));
            }
        },
        _ = tokio::time::sleep(timeout) => {
            terminate_tree(pid);
            let _ = child.wait().await;
            let _ = pump.await;
            // 若 pump 持锁时 panic 会中毒锁：用 lock_ok 清理，避免二次 panic（[docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md) 评审修复）
            let mut c = crate::core::agent::lock_ok(&collector);
            let tail = model_tail(&mut c, full_output, "timeout");
            return ToolOutcome::err("E_TIMEOUT", format!("命令超时（{}s）已终止进程树。\n部分输出（尾部）：\n{tail}", timeout.as_secs()));
        }
        _ = ctx.cancel.cancelled() => {
            terminate_tree(pid);
            let _ = child.wait().await;
            let _ = pump.await;
            return ToolOutcome::err("E_CANCELLED", "命令被用户取消");
        }
    };
    let _ = pump.await;
    let mut c = crate::core::agent::lock_ok(&collector);
    let exit_code = status.code().unwrap_or(-1);
    let output = model_tail(&mut c, full_output, &format!("exit {exit_code}"));
    let mut data = json!({
        "exit_code": exit_code,
        "output": output,
        "shell": shell.describe(),
        "cwd": cwd.display().to_string(),
    });
    if let Some(p) = &c.spill_path {
        data["output_file_path"] = json!(p.display().to_string());
        data["output_truncated"] = json!(true);
    }
    if exit_code == 0 {
        ToolOutcome::ok(data)
    } else {
        ToolOutcome {
            ok: false,
            data,
            error: Some(crate::tools::ToolError::new(
                "E_EXIT_CODE",
                format!("命令退出码 {exit_code}"),
            )),
            warnings: Vec::new(),
            extra_model_content: Vec::new(),
        }
    }
}

/// 输出收集器（stdout/stderr 合流）：buf 只保留前 SPILL_THRESHOLD 字节，超出部分全部进 spill 文件。
struct Collector {
    /// 内存缓冲（溢出后不再增长）。
    buf: String,
    /// 距上次进度推送累计的字节数（节流用）。
    bytes_since_emit: usize,
    /// 是否已发过至少一帧进度（首帧不受 2KB 阈值限制：长命令一开始就要有卡片）。
    progress_emitted: bool,
    /// 上次进度推送时刻。
    last_emit: std::time::Instant,
    /// 是否已发生溢出落盘。
    spilled: bool,
    /// spill 文件路径（溢出后存在）。
    spill_path: Option<std::path::PathBuf>,
    /// 完整输出的字节总数（含落盘部分；用于截断判定与信号行）
    total_bytes: usize,
    /// 完整输出的行数（近似：按 \n 计数，末行无换行时 +1）
    total_lines: u64,
    /// 最后一个分片是否以换行结尾（行数修正用）。
    last_chunk_ended_newline: bool,
}

/// 模型侧输出组装（ally de16c8f 移植）：full_output=false 只回尾部 + 信号行，
/// true 回长尾部（原始行为）。凡截断必落盘（修复 16KB~96KB 盲区：
/// 此前被截去的头部既不返回也不持久化，模型永远无法找回）；
/// 已溢出时 buf 只存前 96KB，尾部改从 spill 文件读取。
/// `sig` 为信号行前缀（常规路径 `exit N`，超时路径 `timeout`）。
fn model_tail(c: &mut Collector, full_output: bool, sig: &str) -> String {
    let want = if full_output {
        FULL_TAIL_CHARS
    } else {
        MODEL_TAIL_CHARS
    };
    let mut lines = c.total_lines;
    if !c.last_chunk_ended_newline && c.total_bytes > 0 {
        lines += 1;
    }
    let truncated = c.spilled || c.buf.chars().count() > want;
    let mut tail = if let Some(p) = &c.spill_path {
        tail_chars_from_file(p, want)
    } else {
        c.buf
            .chars()
            .rev()
            .take(want)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    };
    if truncated {
        let p = match &c.spill_path {
            Some(p) => p.clone(),
            None => {
                let p = write_spill_file(&c.buf);
                c.spill_path = Some(p.clone());
                p
            }
        };
        tail.push_str(&format!(
            "\n[{sig} | 共 {lines} 行 | 输出已截断，完整输出：{}]",
            p.display()
        ));
    }
    tail
}

/// 把缓冲整体写入 spill 文件（写入前顺带清理过期文件）。
fn write_spill_file(content: &str) -> std::path::PathBuf {
    purge_stale_spills();
    let dir = crate::core::config::data_dir()
        .join("tmp")
        .join("cmd-output");
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(format!("{}.txt", uuid::Uuid::new_v4()));
    let _ = std::fs::write(&p, content);
    p
}

/// 读取 spill 文件的尾部（至多 want*4+8 字节；UTF-8 最坏每字符 4 字节）。
fn tail_chars_from_file(p: &std::path::Path, want: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(p) else {
        return String::new();
    };
    let byte_budget = (want * 4 + 8) as u64;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if f.seek(SeekFrom::Start(len.saturating_sub(byte_budget)))
        .is_err()
    {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf)
        .chars()
        .rev()
        .take(want)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 惰性清理超过 24h 的 spill 文件（至多每小时扫描一次，防止临时目录无限膨胀）。
fn purge_stale_spills() {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last = LAST.load(std::sync::atomic::Ordering::Relaxed);
    if now.saturating_sub(last) < 3600 {
        return;
    }
    if LAST
        .compare_exchange(
            last,
            now,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        return;
    }
    let dir = crate::core::config::data_dir()
        .join("tmp")
        .join("cmd-output");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in rd.flatten() {
        let age = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if age > 24 * 3600 {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 终止整个进程组（TERM → 3s → KILL）；M12：阻塞轮询移入 block_in_place。
/// pub(crate)：postcheck 复用同一套整树终止（不另起实现）。
pub(crate) fn terminate_tree(pid: u32) {
    tokio::task::block_in_place(|| terminate_tree_blocking(pid));
}

/// 阻塞版进程树终止实现。
fn terminate_tree_blocking(pid: u32) {
    #[cfg(unix)]
    {
        // 进程组 id == 子进程 pid（process_group(0)）
        let pgid = pid as i32;
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            unsafe {
                if libc::kill(-pgid, 0) != 0 {
                    return; // 已退出
                }
                libc::kill(-pgid, libc::SIGKILL);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x0800_0000)
            .output();
    }
}
