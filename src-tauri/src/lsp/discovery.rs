//! server 发现：新鲜 PATH、PATH 查找、JDK 扫描、命令解析 → [`ServerResolution`]。
//!
//! 探测顺序（**写死**，设置页与报告都按此解释）：
//! ① 设置里的命令覆盖 → ② 项目内 `node_modules/.bin` → ③ 新鲜 PATH → ④ 进程快照 PATH
//! → ⑤ `extra_roots` 扫描 → ⑥ npx 降级（TS/Python）→ ⑦ Dart/Flutter 反推。

use super::Lang;
use super::server_spec::{self, ServerSpec};
use crate::core::config::LspSettings;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 子进程不弹控制台窗口（照抄 `tools/service.rs` 的既有手法）。
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一次解析的结论（设置页展示 + pool 启动 + 引导卡片三处消费）。
#[derive(Debug, Clone)]
pub struct ServerResolution {
    /// 是否找到可用 server
    pub found: bool,
    /// 来源：`config` | `project` | `fresh_path` | `path` | `extra_root` | `npx` | `heuristic` | `""`
    pub source: String,
    /// 启动程序（未找到时为空路径）
    pub program: PathBuf,
    /// 启动参数（含配置覆盖自带的参数）
    pub args: Vec<String>,
    /// server 版本（仅在版本缓存命中时填充；探测由 [`Discoverer::version_of`] 负责）
    pub version: Option<String>,
    /// 人类可读补充（未找到原因 / JDK 状态 / 降级说明）
    pub detail: String,
    /// 未找到时的安装引导
    pub install: Option<super::InstallHint>,
}

impl ServerResolution {
    /// 未找到的构造。
    fn missing(source: &str, detail: String, spec: &ServerSpec) -> Self {
        ServerResolution {
            found: false,
            source: source.into(),
            program: PathBuf::new(),
            args: Vec::new(),
            version: None,
            detail,
            install: Some(super::InstallHint {
                kind: spec.install.kind,
                command: spec.install.command.clone(),
                docs_url: Some(spec.install.docs_url.to_string()),
                prerequisite: spec.install.prerequisite.clone(),
            }),
        }
    }

    /// 展示用命令行（含参数）。
    pub fn command_line(&self) -> String {
        if !self.found {
            return String::new();
        }
        let mut s = quote_if_needed(&self.program.to_string_lossy());
        for a in &self.args {
            s.push(' ');
            s.push_str(&quote_if_needed(a));
        }
        s
    }
}

/// 含空格/中文的路径加引号（只影响展示与配置回填）。
fn quote_if_needed(s: &str) -> String {
    if s.contains(' ') || s.contains('\t') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// 合并两段 PATH：用户（前）优先，去重且保持原顺序。
pub fn merge_paths(user: &str, system: &str) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for part in user
        .split(sep)
        .chain(system.split(sep))
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
    {
        let key = dedup_key(part);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(part.to_string());
    }
    out.join(&sep.to_string())
}

/// 去重键：Windows 大小写不敏感 + 结尾分隔符归一。
fn dedup_key(p: &str) -> String {
    let p = p.trim_end_matches(['\\', '/']);
    if cfg!(windows) {
        p.to_ascii_lowercase()
    } else {
        p.to_string()
    }
}

/// 展开 `%NAME%` 形态的变量；未命中时保留原文（大小写不敏感由 `lookup` 决定）。
pub fn expand_vars(raw: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        if raw.as_bytes()[i] == b'%' {
            if let Some(close) = raw[i + 1..].find('%') {
                let name = &raw[i + 1..i + 1 + close];
                if !name.is_empty() && !name.contains('%') {
                    if let Some(v) = lookup(name) {
                        out.push_str(&v);
                        i += 1 + close + 1;
                        continue;
                    }
                }
            }
        }
        let ch = raw[i..].chars().next().expect("索引落在字符边界");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 探测「新鲜 PATH」——进程启动快照会过期（装完 JDK/Flutter 不重启就看不见）。
///
/// Windows 读注册表环境块（HKCU\Environment + HKLM 系统环境），非 Windows 走登录 shell。
/// 返回 `None` 时调用方回落到进程快照。
#[cfg(windows)]
pub fn fresh_env_path() -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};

    let user = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ)
        .ok();
    let system = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            KEY_READ,
        )
        .ok();

    // 变量表：系统 → 用户（用户覆盖系统），再用进程环境兜底 SystemRoot 等
    let mut table: HashMap<String, String> = HashMap::new();
    for key in [system.as_ref(), user.as_ref()].into_iter().flatten() {
        for (name, value) in key.enum_values().flatten() {
            table.insert(name.to_ascii_lowercase(), value.to_string());
        }
    }
    for (k, v) in std::env::vars() {
        table.entry(k.to_ascii_lowercase()).or_insert(v);
    }
    let lookup = |n: &str| table.get(&n.to_ascii_lowercase()).cloned();

    let read_path = |k: Option<&RegKey>| -> String {
        k.and_then(|k| k.get_value::<String, _>("Path").ok())
            .map(|raw| expand_vars(&raw, &lookup))
            .unwrap_or_default()
    };
    let user_path = read_path(user.as_ref());
    let system_path = read_path(system.as_ref());
    if user_path.trim().is_empty() && system_path.trim().is_empty() {
        return None;
    }
    Some(merge_paths(&user_path, &system_path))
}

/// 非 Windows：登录 shell 里 `echo $PATH`（`-lc` 失败再试 `-lic`）。
#[cfg(not(windows))]
pub fn fresh_env_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    for flag in ["-lc", "-lic"] {
        let out = std::process::Command::new(&shell)
            .args([flag, "echo $PATH"])
            .output()
            .ok();
        if let Some(out) = out {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// PATH 解析器：新鲜探测一次并永久缓存（`OnceLock` 无锁快路径）。
///
/// 需要重新探测时由持有方**整体替换实例**（[`super::manager::LspManager::redetect`] 如此做），
/// [`Discoverer::redetect`] 只清版本缓存。
pub struct Discoverer {
    fresh: OnceLock<Option<String>>,
    fallback: String,
    versions: Mutex<HashMap<PathBuf, Option<String>>>,
}

impl Default for Discoverer {
    fn default() -> Self {
        Discoverer::new()
    }
}

impl Discoverer {
    /// 新建（进程 PATH 快照立即取好；新鲜 PATH 惰性探测）。
    pub fn new() -> Self {
        Discoverer {
            fresh: OnceLock::new(),
            fallback: std::env::var("PATH").unwrap_or_default(),
            versions: Mutex::new(HashMap::new()),
        }
    }

    /// 生效 PATH：新鲜探测成功用新鲜值，失败回落进程快照。
    pub fn path(&self) -> &str {
        match self.fresh.get_or_init(fresh_env_path) {
            Some(p) => p.as_str(),
            None => self.fallback.as_str(),
        }
    }

    /// 当前缓存的 PATH（不触发探测）。
    pub fn snapshot_cached(&self) -> &str {
        match self.fresh.get() {
            Some(Some(p)) => p.as_str(),
            _ => self.fallback.as_str(),
        }
    }

    /// 清掉可变的缓存（版本探测结论）。PATH 本身由实例替换实现重置。
    pub fn redetect(&self) {
        self.versions.lock().unwrap().clear();
    }

    /// 版本缓存命中（不探测、不起进程；写路径专用，避免每次写文件都拉慢）。
    pub fn cached_version(&self, program: &Path) -> Option<String> {
        self.versions
            .lock()
            .unwrap()
            .get(program)
            .cloned()
            .flatten()
    }

    /// 探测版本（带缓存；探测失败也缓存 `None`，避免反复起进程）。
    pub fn version_of(&self, program: &Path) -> Option<String> {
        if let Some(hit) = self.versions.lock().unwrap().get(program) {
            return hit.clone();
        }
        let probed = probe_version(program);
        self.versions
            .lock()
            .unwrap()
            .insert(program.to_path_buf(), probed.clone());
        probed
    }
}

/// Windows 补全用扩展名：读 `PATHEXT`（如 `.COM;.EXE;.BAT;.CMD`）→ 小写、不带点的列表。
///
/// 非 Windows 返回空（Unix 没有「同名 + 扩展名」这套解析规则）。
/// `PATHEXT` 缺失时用与 cmd.exe 一致的默认顺序兜底。
pub fn executable_extensions() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let raw = std::env::var("PATHEXT").unwrap_or_default();
    let mut out: Vec<String> = raw
        .split(';')
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if out.is_empty() {
        out = ["com", "exe", "bat", "cmd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    out
}

/// 在给定 PATH 里查找可执行文件（自己实现，不引依赖）。
///
/// Windows 先按 `PATHEXT` 顺序试「名字 + 扩展名」，**最后**才试原名；传入的 `name`
/// 带路径分隔符时按路径处理。
pub fn which_in(name: &str, path: &str) -> Option<PathBuf> {
    which_in_with_ext(name, path, &executable_extensions())
}

/// 同 [`which_in`]，但扩展名列表由调用方注入（安装器与单测需要显式控制解析面）。
///
/// `exts` 为空 = 只认原名（Unix 行为）。扩展名只在原名没有扩展名时补。
pub fn which_in_with_ext(name: &str, path: &str, exts: &[String]) -> Option<PathBuf> {
    if name.trim().is_empty() {
        return None;
    }
    let looks_like_path =
        name.contains('/') || name.contains('\\') || Path::new(name).is_absolute();
    if looks_like_path {
        return candidates_with(Path::new(name), exts)
            .into_iter()
            .find(|p| p.is_file());
    }
    for dir in std::env::split_paths(path).filter(|d| !d.as_os_str().is_empty()) {
        for cand in candidates_with(&dir.join(name), exts) {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 候选文件名（等价于 [`candidates_with`] + 系统 `PATHEXT`）。
fn candidates(base: &Path) -> Vec<PathBuf> {
    candidates_with(base, &executable_extensions())
}

/// 候选文件名：`PATHEXT` 补全的形态在前，原名在**最后**。
///
/// 裸名排最后是修 Windows「找不到可执行文件」的关键：Node 安装目录里 `npm`（Git-Bash 用的
/// shell 脚本）、`npm.cmd`、`npm.ps1` 三件套并存，裸名不是可执行文件，`CreateProcess` 会
/// 直接失败（`npm.cmd` 才跑得了）。
fn candidates_with(base: &Path, exts: &[String]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if !exts.is_empty() && base.extension().is_none() {
        for ext in exts {
            // 用「追加」而非 `with_extension`：`python3.11` 这类带点的名字不该被顶掉后缀
            let mut os = base.as_os_str().to_os_string();
            os.push(".");
            os.push(ext);
            v.push(PathBuf::from(os));
        }
    }
    v.push(base.to_path_buf());
    v
}

/// 定位 JDK 21+：先认配置里的显式路径，再按优先级扫候选根。
///
/// **绝不读 `JAVA_HOME`**（实测常指向旧版本）。返回 JDK 根目录（不是 `bin`）。
pub fn find_jdk21(cfg_java_home: &str, extra_roots: &[String]) -> Option<PathBuf> {
    let configured = cfg_java_home.trim();
    if !configured.is_empty() {
        let raw = expand_vars(configured, &|n| std::env::var(n).ok());
        let p = PathBuf::from(&raw);
        if is_jdk_home(&p) {
            return Some(p);
        }
        if let Some(parent) = p.parent() {
            if is_jdk_home(parent) {
                return Some(parent.to_path_buf());
            }
        }
    }
    let roots = jdk_candidate_roots(extra_roots);
    for root in roots {
        if let Some(home) = best_jdk_in_root(&root) {
            return Some(home);
        }
    }
    None
}

/// 某个候选根里符合「≥21」的最高版本 JDK（先看根本身，再看一层子目录）。
fn best_jdk_in_root(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for home in jdk_homes_under(root) {
        let Some(version) = jdk_version_score(&home) else {
            continue;
        };
        if jdk_major(&version) < 21 {
            continue;
        }
        if best.as_ref().is_none_or(|(v, _)| *v < version) {
            best = Some((version, home));
        }
    }
    best.map(|(_, home)| home)
}

/// 候选根（优先级：extra_roots 在前 = 用户指定优先，再是各安装器默认位置）。
fn jdk_candidate_roots(extra_roots: &[String]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = extra_roots
        .iter()
        .filter(|r| !r.trim().is_empty())
        .map(|r| PathBuf::from(expand_vars(r, &|n| std::env::var(n).ok())))
        .collect();
    #[cfg(windows)]
    {
        for base in [
            r"C:\Program Files\Java",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files\Eclipse Adoptium",
            r"C:\Program Files\Zulu",
            r"C:\Program Files\IBM\Semeru",
            r"C:\Program Files\Amazon Corretto",
            r"C:\Program Files\BellSoft",
        ] {
            roots.push(PathBuf::from(base));
        }
    }
    #[cfg(not(windows))]
    {
        for base in [
            "/Library/Java/JavaVirtualMachines",
            "/usr/lib/jvm",
            "/opt/java",
        ] {
            roots.push(PathBuf::from(base));
        }
    }
    roots
}

/// 某个根下的全部 JDK 根：根本身（用户可能直接指向 JDK）+ 一层子目录。
fn jdk_homes_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if is_jdk_home(root) {
        out.push(root.to_path_buf());
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() && is_jdk_home(&p) {
            out.push(p);
        }
    }
    out
}

/// JDK 根标志：`release` 文件或 `bin/javac`。
fn is_jdk_home(dir: &Path) -> bool {
    if dir.join("release").is_file() {
        return true;
    }
    let javac = dir.join("bin").join("javac");
    javac.is_file() || javac.with_extension("exe").is_file()
}

/// 读 `<dir>/release` 的 `JAVA_VERSION=` 并解析成可比较版本向量。
fn jdk_version_score(home: &Path) -> Option<Vec<u32>> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != "JAVA_VERSION" {
            continue;
        }
        return parse_version(v.trim().trim_matches('"'));
    }
    None
}

/// 版本向量的主版本号（`1.8.0_302` 的主版本是 8 —— JDK 8 的老写法）。
fn jdk_major(v: &[u32]) -> u32 {
    match (v.first(), v.get(1)) {
        (Some(1), Some(second)) => *second,
        (Some(first), _) => *first,
        _ => 0,
    }
}

/// 从任意文本里解析首个版本号（`21.0.1` / `1.8.0_302` / `17`）。
fn parse_version(s: &str) -> Option<Vec<u32>> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && !bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    let text = s[start..i].trim_end_matches('.');
    let parts: Vec<u32> = text
        .split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect();
    if parts.is_empty() { None } else { Some(parts) }
}

/// 跑 `--version` 并取首个版本串（10s 超时；超时放弃，绝不挂死写路径）。
pub fn probe_version(program: &Path) -> Option<String> {
    use std::process::Stdio;
    let (tx, rx) = std::sync::mpsc::channel();
    let program = program.to_path_buf();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new(&program);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let _ = tx.send(cmd.output());
    });
    let out = rx.recv_timeout(Duration::from_secs(10)).ok()?.ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    first_version_token(&text)
}

/// 文本里首个形如 `\d+\.\d+` 的版本串（不足两段则取纯数字段）。
fn first_version_token(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || bytes[j] == b'.' || bytes[j].is_ascii_alphabetic())
            {
                j += 1;
            }
            let token = &text[start..j];
            let cleaned = token
                .trim_end_matches('.')
                .trim_end_matches(|c: char| c.is_ascii_alphabetic());
            // 含字母的 token（如 `21ea`）截断到数字部分后仍要有 `\d+\.\d+` 形态才采用
            if let Some(dot) = cleaned.find('.') {
                if cleaned[..dot].chars().all(|c| c.is_ascii_digit())
                    && cleaned[dot + 1..].chars().any(|c| c.is_ascii_digit())
                {
                    return Some(cleaned.to_string());
                }
            }
            i = j.max(start + 1);
            continue;
        }
        i += 1;
    }
    None
}

/// jdtls 子进程需要 JDK 21+：把定位到的 JDK 以 `JAVA_HOME` 注入子进程环境。
///
/// 不读机器现有的 `JAVA_HOME`（实测它常指向旧版），只认自己找到的那个；
/// 找不到就什么都不注入（jdtls 启动失败会走到重启/不可用分支，而不是拿错 JDK 硬跑）。
pub fn java_launch_env(cfg: &crate::core::config::LspSettings) -> Vec<(String, String)> {
    match find_jdk21(&cfg.java_home, &cfg.extra_roots) {
        Some(jdk) => vec![("JAVA_HOME".to_string(), jdk.to_string_lossy().to_string())],
        None => Vec::new(),
    }
}

/// 解析某语言的 server（优先级见模块头注释）。
pub fn resolve(
    lang: Lang,
    cfg: &LspSettings,
    project_root: &Path,
    discoverer: &Discoverer,
) -> ServerResolution {
    let spec = server_spec::spec(lang);
    let mut jdk_note = String::new();
    if lang == Lang::Java {
        jdk_note = match find_jdk21(&cfg.java_home, &cfg.extra_roots) {
            Some(p) => format!("已定位 JDK 21+：{}", p.display()),
            None => "未定位到 JDK 21+（jdtls 需要 JDK 21+，且不会使用 JAVA_HOME）".to_string(),
        };
    }

    // ① 设置里的命令覆盖（用户显式指定最优先）
    let override_cmd = commands_field(&cfg.commands, lang);
    if !override_cmd.trim().is_empty() {
        let words = split_command(override_cmd);
        if let Some((program_raw, rest)) = words.split_first() {
            let found = locate_program(program_raw, discoverer);
            match found {
                Some(program) => {
                    let args = if rest.is_empty() {
                        spec.args.clone()
                    } else {
                        rest.to_vec()
                    };
                    let mut detail = format!("使用设置中的命令：{}", quote_if_needed(program_raw));
                    if !jdk_note.is_empty() {
                        detail.push('；');
                        detail.push_str(&jdk_note);
                    }
                    return ServerResolution {
                        found: true,
                        source: "config".into(),
                        version: discoverer.cached_version(&program),
                        program,
                        args,
                        detail,
                        install: None,
                    };
                }
                None => {
                    return ServerResolution::missing(
                        "config",
                        format!("设置中的命令不存在或不可执行：{program_raw}"),
                        &spec,
                    );
                }
            }
        }
    }

    // ② 项目内 node_modules/.bin（仅 TS/Python：npm 生态才这么装）
    if matches!(lang, Lang::TypeScript | Lang::Python) {
        let bin = project_root.join("node_modules").join(".bin");
        for cand in candidates(&bin.join(spec.server_name)) {
            if cand.is_file() {
                return ServerResolution {
                    found: true,
                    source: "project".into(),
                    program: cand.clone(),
                    args: spec.args.clone(),
                    version: discoverer.cached_version(&cand),
                    detail: format!("使用项目内 server：{}", cand.display()),
                    install: None,
                };
            }
        }
    }

    // ③ 新鲜 PATH
    if let Some(p) = which_in(spec.server_name, discoverer.path()) {
        let mut detail = format!("在新鲜 PATH 中找到：{}", p.display());
        if !jdk_note.is_empty() {
            detail.push('；');
            detail.push_str(&jdk_note);
        }
        return ServerResolution {
            found: true,
            source: "fresh_path".into(),
            args: spec.args.clone(),
            version: discoverer.cached_version(&p),
            program: p,
            detail,
            install: None,
        };
    }

    // ④ 进程快照 PATH（新鲜探测失败时的回落）
    let snapshot = discoverer.snapshot_cached();
    if snapshot != discoverer.path() {
        if let Some(p) = which_in(spec.server_name, snapshot) {
            return ServerResolution {
                found: true,
                source: "path".into(),
                program: p.clone(),
                args: spec.args.clone(),
                version: discoverer.cached_version(&p),
                detail: format!("在进程 PATH 快照中找到：{}", p.display()),
                install: None,
            };
        }
    }

    // ⑤ extra_roots：`<root>/<lang>/bin/<server>`
    for root in cfg.extra_roots.iter().filter(|r| !r.trim().is_empty()) {
        let dir = PathBuf::from(expand_vars(root, &|n| std::env::var(n).ok()))
            .join(lang.id())
            .join("bin");
        for cand in candidates(&dir.join(spec.server_name)) {
            if cand.is_file() {
                return ServerResolution {
                    found: true,
                    source: "extra_root".into(),
                    program: cand.clone(),
                    args: spec.args.clone(),
                    version: discoverer.cached_version(&cand),
                    detail: format!("在额外 SDK 根中找到：{}", cand.display()),
                    install: None,
                };
            }
        }
    }

    // ⑥ npx 降级（仅 TS/Python）
    if let Some(fallback) = &spec.npx_fallback {
        if let Some(npx) = which_in(&fallback[0], discoverer.path()) {
            return ServerResolution {
                found: true,
                source: "npx".into(),
                program: npx,
                args: fallback[1..].to_vec(),
                version: None,
                detail: "未安装本地 server，降级用 npx 临时拉取（首次会慢）".into(),
                install: None,
            };
        }
    }

    // ⑦ Dart 专有：由 flutter 反推 `<flutter>/bin/dart`
    if lang == Lang::Dart {
        if let Some(flutter) = which_in("flutter", discoverer.path()) {
            if let Some(bin_dir) = flutter.parent() {
                for cand in candidates(&bin_dir.join("dart")) {
                    if cand.is_file() {
                        return ServerResolution {
                            found: true,
                            source: "heuristic".into(),
                            program: cand.clone(),
                            args: spec.args.clone(),
                            version: discoverer.cached_version(&cand),
                            detail: "由 Flutter SDK 反推 dart（未单独安装 Dart SDK）".into(),
                            install: None,
                        };
                    }
                }
            }
        }
    }

    let mut detail = format!("未找到 {}（{}）", spec.server_name, spec.install.docs_url);
    if !jdk_note.is_empty() {
        detail.push('；');
        detail.push_str(&jdk_note);
    }
    ServerResolution::missing("", detail, &spec)
}

/// 配置里的语言命令覆盖字段。
fn commands_field(c: &crate::core::config::LspCommands, lang: Lang) -> &str {
    match lang {
        Lang::TypeScript => &c.typescript,
        Lang::Rust => &c.rust,
        Lang::Python => &c.python,
        Lang::Go => &c.go,
        Lang::Java => &c.java,
        Lang::Dart => &c.dart,
    }
}

/// 记一个「是否为命令/程序」的定位：绝对/含分隔符 → 查文件；否则走 PATH。
fn locate_program(token: &str, discoverer: &Discoverer) -> Option<PathBuf> {
    let looks_like_path =
        token.contains('/') || token.contains('\\') || Path::new(token).is_absolute();
    if looks_like_path {
        return candidates(Path::new(token))
            .into_iter()
            .find(|p| p.is_file());
    }
    which_in(token, discoverer.path()).or_else(|| which_in(token, discoverer.snapshot_cached()))
}

/// 拆分命令串：支持双引号包裹的含空格路径。
pub fn split_command(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in raw.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_paths_user_first_and_dedup() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let merged = merge_paths(&format!("a{sep}b"), &format!("b{sep}c{sep}a{sep}d{sep}"));
        let parts: Vec<&str> = merged.split(sep).collect();
        assert_eq!(parts, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn merge_paths_drops_empty_segments() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let merged = merge_paths(&format!("{sep}a{sep}{sep}"), &format!("{sep}{sep}b"));
        assert_eq!(merged.split(sep).collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn expand_vars_is_case_insensitive_and_keeps_unknown() {
        let lookup = |n: &str| match n.to_ascii_lowercase().as_str() {
            "systemroot" => Some(r"C:\Windows".to_string()),
            "p(x86)" => Some(r"C:\PF86".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_vars(r"%SystemRoot%\System32;%P(X86)%\bin", &lookup),
            r"C:\Windows\System32;C:\PF86\bin"
        );
        assert_eq!(
            expand_vars("%UNKNOWN%\\x", &lookup),
            "%UNKNOWN%\\x",
            "未命中必须保留原文"
        );
        assert_eq!(expand_vars("no vars", &lookup), "no vars");
        assert_eq!(expand_vars("100% 完成", &lookup), "100% 完成");
    }

    #[test]
    fn which_in_finds_extensioned_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let name = if cfg!(windows) {
            "cooltool.exe"
        } else {
            "cooltool"
        };
        std::fs::write(dir.join(name), "x").unwrap();
        let path = dir.to_string_lossy().to_string();
        let found = which_in("cooltool", &path).expect("应能找到");
        assert_eq!(found.file_name().unwrap().to_string_lossy(), name);
        assert!(which_in("missing-tool", &path).is_none());
        assert!(which_in("cooltool", "").is_none());
    }

    #[test]
    fn which_in_handles_direct_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(if cfg!(windows) {
            "direct.exe"
        } else {
            "direct"
        });
        std::fs::write(&file, "x").unwrap();
        assert_eq!(
            which_in(&file.to_string_lossy(), "").as_deref(),
            Some(file.as_path())
        );
    }

    #[test]
    fn which_in_prefers_extensioned_executable_over_bare_name() {
        // 修 Windows 不可执行 bug：Node 目录里 `npm` / `npm.cmd` 并存，裸名不是可执行文件
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("pawn"), "#!/bin/sh\n").unwrap();
        std::fs::write(tmp.path().join("pawn.cmd"), "@echo off\n").unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let found = which_in("pawn", &path).expect("应能解析到 pawn");
        let name = found
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        if cfg!(windows) {
            assert_eq!(
                name, "pawn.cmd",
                "Windows 必须选中可执行的 .cmd（原名只是脚本）"
            );
        } else {
            assert_eq!(name, "pawn", "Unix 不补扩展名，只认原名");
        }
    }

    #[test]
    fn which_in_with_ext_honours_injected_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tool.ps1"), "x").unwrap();
        let path = tmp.path().to_string_lossy().to_string();
        let exts = vec!["ps1".to_string()];
        let found = which_in_with_ext("tool", &path, &exts).expect("注入的扩展名应生效");
        assert_eq!(found.extension().unwrap(), "ps1");
        // 注入列表里没有的扩展名不得命中；空列表 = 只认原名
        assert!(which_in_with_ext("tool", &path, &["exe".to_string()]).is_none());
        assert!(which_in_with_ext("tool", &path, &[]).is_none());
        assert!(which_in_with_ext("", &path, &exts).is_none());
    }

    #[test]
    fn executable_extensions_are_platform_shaped() {
        let exts = executable_extensions();
        if cfg!(windows) {
            assert!(exts.iter().any(|e| e == "exe"), "{exts:?}");
            assert!(exts.iter().any(|e| e == "cmd"), "{exts:?}");
            assert!(exts.iter().all(|e| !e.starts_with('.')), "{exts:?}");
        } else {
            assert!(exts.is_empty(), "Unix 不补扩展名：{exts:?}");
        }
    }

    #[test]
    fn find_jdk21_picks_highest_in_root_and_skips_old() {
        let tmp = tempfile::tempdir().unwrap();
        let make = |name: &str, version: &str| {
            let home = tmp.path().join(name);
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(
                home.join("release"),
                format!("IMPLEMENTOR=\"X\"\nJAVA_VERSION=\"{version}\"\n"),
            )
            .unwrap();
        };
        make("jdk-17.0.9", "17.0.9");
        make("jdk-21.0.1", "21.0.1");
        make("jdk-21.0.5", "21.0.5");
        let roots = vec![tmp.path().to_string_lossy().to_string()];
        let picked = find_jdk21("", &roots).expect("应选中 JDK 21+");
        assert_eq!(picked.file_name().unwrap().to_string_lossy(), "jdk-21.0.5");
    }

    #[test]
    fn find_jdk21_honours_explicit_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("some-jdk");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin").join("javac"), "x").unwrap();
        let picked = find_jdk21(&home.to_string_lossy(), &[]).expect("显式配置必须被采纳");
        assert_eq!(picked, home);
        // 指向 bin 的形态也要认
        let picked_bin = find_jdk21(&home.join("bin").to_string_lossy(), &[]).expect("bin 形态");
        assert_eq!(picked_bin, home);
    }

    #[test]
    fn best_jdk_in_root_prefers_highest_and_filters_old() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, version) in [
            ("jdk-17.0.9", "17.0.9"),
            ("jdk-21.0.1", "21.0.1"),
            ("jdk-21.0.5", "21.0.5"),
            ("jdk-25-ea", "25-ea"),
        ] {
            let home = tmp.path().join(name);
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(
                home.join("release"),
                format!("JAVA_VERSION=\"{version}\"\n"),
            )
            .unwrap();
        }
        let picked = best_jdk_in_root(tmp.path()).expect("应选中 21+");
        assert_eq!(picked.file_name().unwrap().to_string_lossy(), "jdk-25-ea");

        let old = tempfile::tempdir().unwrap();
        let home = old.path().join("jdk-8.0.1");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"1.8.0_302\"\n").unwrap();
        assert!(
            best_jdk_in_root(old.path()).is_none(),
            "JDK 8 的主版本解析为 8，必须被过滤"
        );
    }

    #[test]
    fn find_jdk21_returns_none_when_nothing_qualifies() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("jdk-8.0.1");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"1.8.0_302\"\n").unwrap();
        // 本机可能真装了 JDK 21+（候选根里还有系统默认位置），故只断言「不会选中 JDK 8」
        let picked = find_jdk21("", &[tmp.path().to_string_lossy().to_string()]);
        assert_ne!(picked.as_deref(), Some(home.as_path()));
    }

    #[test]
    fn parse_version_variants() {
        assert_eq!(parse_version("21.0.1+12"), Some(vec![21, 0, 1]));
        assert_eq!(parse_version("1.8.0_302"), Some(vec![1, 8, 0]));
        assert_eq!(parse_version("17"), Some(vec![17]));
        assert_eq!(jdk_major(&[1, 8, 0]), 8, "JDK 8 的老写法主版本是 8");
        assert_eq!(jdk_major(&[21, 0, 1]), 21);
    }

    #[test]
    fn split_command_respects_quotes() {
        assert_eq!(
            split_command(r#""C:\Program Files\nodejs\npx.cmd" -y pyright-langserver --stdio"#),
            vec![
                r"C:\Program Files\nodejs\npx.cmd",
                "-y",
                "pyright-langserver",
                "--stdio"
            ]
        );
        assert_eq!(split_command("  gopls   "), vec!["gopls"]);
    }

    #[test]
    fn resolve_prefers_configured_command() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join(if cfg!(windows) {
            "my-lsp.exe"
        } else {
            "my-lsp"
        });
        std::fs::write(&fake, "x").unwrap();
        let mut cfg = LspSettings::default();
        cfg.commands.rust = fake.to_string_lossy().to_string();
        let d = Discoverer::new();
        let res = resolve(Lang::Rust, &cfg, tmp.path(), &d);
        assert!(res.found, "{res:?}");
        assert_eq!(res.source, "config");
        assert_eq!(res.program, fake);
        assert!(res.command_line().contains("my-lsp"));

        // 非法命令 → found=false 且给安装引导
        let mut bad = LspSettings::default();
        bad.commands.rust = "definitely-not-a-real-binary-xyz".into();
        let res = resolve(Lang::Rust, &bad, tmp.path(), &d);
        assert!(!res.found);
        assert!(res.install.is_some());
        assert!(res.detail.contains("不存在"));
    }

    #[test]
    fn resolve_uses_project_local_bin_for_typescript() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        let name = if cfg!(windows) {
            "typescript-language-server.cmd"
        } else {
            "typescript-language-server"
        };
        std::fs::write(bin.join(name), "x").unwrap();
        let d = Discoverer::new();
        let res = resolve(Lang::TypeScript, &LspSettings::default(), tmp.path(), &d);
        assert!(res.found, "{res:?}");
        assert_eq!(res.source, "project");
        assert_eq!(res.args, vec!["--stdio".to_string()]);
    }

    #[test]
    fn java_launch_env_injects_located_jdk() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("jdk-21.0.2");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("release"), "JAVA_VERSION=\"21.0.2\"\n").unwrap();
        let mut cfg = LspSettings::default();
        cfg.extra_roots = vec![tmp.path().to_string_lossy().to_string()];
        let env = java_launch_env(&cfg);
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "JAVA_HOME");
        assert_eq!(PathBuf::from(&env[0].1), home);
    }

    #[test]
    fn missing_server_detail_mentions_docs() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LspSettings::default();
        let d = Discoverer::new();
        let res = resolve(Lang::Dart, &cfg, tmp.path(), &d);
        // 本机可能真装了 dart/flutter：两种形态都必须自洽
        if res.found {
            assert!(!res.source.is_empty());
        } else {
            assert!(res.detail.contains("未找到"), "{}", res.detail);
            assert!(res.install.is_some());
        }
    }
}
