//! 外部打开器：在文件管理器中打开目录 / 在编辑器中打开目录
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//!
//! 分层：探测与进程启动都在 core（不依赖 tauri），host 命令只做校验 + 转调。
//! 设计要点：**探测与启动分离**——所有探测函数接收注入的基目录与存在性谓词，
//! 单测不触碰真实安装目录、不真开进程。
//!
//! Windows 文件管理器：按序探测 Files 应用（files-stable → files-dev → files-preview →
//! files-canary 渠道别名），命中用 `files-*.exe -Directory <目录>`（Files 源码
//! `CommandLineParser` 支持该参数），spawn 失败降级为裸路径参数，再失败回退 explorer。

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Windows 无控制台窗口标志（CREATE_NO_WINDOW）：`.cmd`/`.bat` 垫片与控制台程序都不得闪黑框。
/// `pub(crate)`：open_url 等其它外部启动点共用同一常量，避免各写一份魔术数字。
#[cfg(target_os = "windows")]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 一个可用的编辑器（前端下拉的选项）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EditorInfo {
    /// 稳定 id（前端 localStorage 记忆的键；JetBrains 为 `jetbrains:<product>`）
    pub id: String,
    /// 显示名（前端直接展示）
    pub name: String,
    /// 可执行文件路径（Windows 可能是 `.cmd`/`.bat` 垫片，启动时经 `cmd /C`）
    pub path: String,
}

/// 探测所需的基目录与扩展名（生产从环境变量取，测试注入）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnvBases {
    /// PATH 拆出的目录列表（按序探测，靠前者优先）
    pub path_dirs: Vec<PathBuf>,
    /// PATH 内的可执行扩展名（Windows: .exe/.cmd/.bat；其他平台为空 = 名称即文件名）
    pub ext: Vec<String>,
    pub home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub program_files: Option<PathBuf>,
    pub program_files_x86: Option<PathBuf>,
    /// macOS `/Applications`（其他平台 None）
    pub applications: Option<PathBuf>,
}

impl EnvBases {
    /// 从当前进程环境构造（唯一触碰真实环境变量的入口）。
    pub fn from_process() -> Self {
        let path_dirs = std::env::var_os("PATH")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        #[cfg(target_os = "windows")]
        let ext = vec![".exe".to_string(), ".cmd".to_string(), ".bat".to_string()];
        #[cfg(not(target_os = "windows"))]
        let ext: Vec<String> = Vec::new();
        #[cfg(target_os = "macos")]
        let applications = Some(PathBuf::from("/Applications"));
        #[cfg(not(target_os = "macos"))]
        let applications = None;
        Self {
            path_dirs,
            ext,
            home: dirs::home_dir(),
            local_app_data: var("LOCALAPPDATA"),
            program_files: var("ProgramFiles"),
            program_files_x86: var("ProgramFiles(x86)"),
            applications,
        }
    }
}

/// 存在性谓词（生产 `Path::exists`，测试注入假文件系统）。
pub type Probe<'a> = &'a dyn Fn(&Path) -> bool;

/// 目录枚举谓词（生产读目录，测试注入固定列表）——JetBrains 扫描用。
pub type ReadDir<'a> = &'a dyn Fn(&Path) -> Vec<PathBuf>;

/// 编辑器候选表：同一张表同时定义「探测顺序」与「默认项 = 第一个检测到的」语义。
struct EditorSpec {
    id: &'static str,
    name: &'static str,
    /// PATH 内的可执行名（不含扩展名），按序尝试
    path_names: &'static [&'static str],
    /// 固定安装位置模板：`{LOCALAPPDATA}`/`{PROGRAMFILES}`/`{PROGRAMFILES_X86}`/`{HOME}`/`{APPLICATIONS}`
    fixed: &'static [&'static str],
}

#[cfg(target_os = "windows")]
const EDITOR_SPECS: &[EditorSpec] = &[
    EditorSpec {
        id: "vscode",
        name: "VS Code",
        path_names: &["code"],
        fixed: &[
            "{LOCALAPPDATA}/Programs/Microsoft VS Code/Code.exe",
            "{PROGRAMFILES}/Microsoft VS Code/Code.exe",
        ],
    },
    EditorSpec {
        id: "vscode-insiders",
        name: "VS Code Insiders",
        path_names: &["code-insiders"],
        fixed: &["{LOCALAPPDATA}/Programs/Microsoft VS Code Insiders/Code - Insiders.exe"],
    },
    EditorSpec {
        id: "cursor",
        name: "Cursor",
        path_names: &["cursor"],
        fixed: &["{LOCALAPPDATA}/Programs/cursor/Cursor.exe"],
    },
    EditorSpec {
        id: "windsurf",
        name: "Windsurf",
        path_names: &["windsurf"],
        fixed: &["{LOCALAPPDATA}/Programs/Windsurf/Windsurf.exe"],
    },
    EditorSpec {
        id: "zed",
        name: "Zed",
        path_names: &["zed"],
        fixed: &[
            "{LOCALAPPDATA}/Programs/Zed/zed.exe",
            "{PROGRAMFILES}/Zed/zed.exe",
        ],
    },
    EditorSpec {
        id: "sublime",
        name: "Sublime Text",
        path_names: &["subl", "sublime_text"],
        fixed: &[
            "{PROGRAMFILES}/Sublime Text/sublime_text.exe",
            "{PROGRAMFILES}/Sublime Text 3/sublime_text.exe",
            "{LOCALAPPDATA}/Programs/Sublime Text/sublime_text.exe",
        ],
    },
    EditorSpec {
        id: "notepadpp",
        name: "Notepad++",
        path_names: &["notepad++"],
        fixed: &[
            "{PROGRAMFILES}/Notepad++/notepad++.exe",
            "{PROGRAMFILES_X86}/Notepad++/notepad++.exe",
        ],
    },
];

#[cfg(target_os = "macos")]
const EDITOR_SPECS: &[EditorSpec] = &[
    EditorSpec {
        id: "vscode",
        name: "VS Code",
        path_names: &["code"],
        fixed: &[
            "{APPLICATIONS}/Visual Studio Code.app/Contents/Resources/app/bin/code",
            "{HOME}/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
        ],
    },
    EditorSpec {
        id: "vscode-insiders",
        name: "VS Code Insiders",
        path_names: &["code-insiders"],
        fixed: &["{APPLICATIONS}/Visual Studio Code - Insiders.app/Contents/Resources/app/bin/code"],
    },
    EditorSpec {
        id: "cursor",
        name: "Cursor",
        path_names: &["cursor"],
        fixed: &["{APPLICATIONS}/Cursor.app/Contents/Resources/app/bin/cursor"],
    },
    EditorSpec {
        id: "windsurf",
        name: "Windsurf",
        path_names: &["windsurf"],
        fixed: &["{APPLICATIONS}/Windsurf.app/Contents/Resources/app/bin/windsurf"],
    },
    EditorSpec {
        id: "zed",
        name: "Zed",
        path_names: &["zed"],
        fixed: &["{APPLICATIONS}/Zed.app/Contents/MacOS/zed"],
    },
    EditorSpec {
        id: "sublime",
        name: "Sublime Text",
        path_names: &["subl"],
        fixed: &[
            "{APPLICATIONS}/Sublime Text.app/Contents/SharedSupport/bin/subl",
            "{APPLICATIONS}/Sublime Text 3.app/Contents/SharedSupport/bin/subl",
        ],
    },
    EditorSpec {
        id: "notepadpp",
        name: "Notepad++",
        path_names: &["notepad++"],
        fixed: &[],
    },
];

#[cfg(all(unix, not(target_os = "macos")))]
const EDITOR_SPECS: &[EditorSpec] = &[
    EditorSpec {
        id: "vscode",
        name: "VS Code",
        path_names: &["code"],
        fixed: &["/snap/bin/code"],
    },
    EditorSpec {
        id: "vscode-insiders",
        name: "VS Code Insiders",
        path_names: &["code-insiders"],
        fixed: &[],
    },
    EditorSpec {
        id: "cursor",
        name: "Cursor",
        path_names: &["cursor"],
        fixed: &[],
    },
    EditorSpec {
        id: "windsurf",
        name: "Windsurf",
        path_names: &["windsurf"],
        fixed: &[],
    },
    EditorSpec {
        id: "zed",
        name: "Zed",
        path_names: &["zed"],
        fixed: &[],
    },
    EditorSpec {
        id: "sublime",
        name: "Sublime Text",
        path_names: &["subl", "sublime_text"],
        fixed: &[],
    },
    EditorSpec {
        id: "notepadpp",
        name: "Notepad++",
        path_names: &["notepad++"],
        fixed: &[],
    },
];

/// 模板展开：仅识别全部受支持的基目录前缀，未知前缀/缺失基目录返回 None。
/// 路径按 `/` 分段 join，避免 Windows 上分隔符混用。
fn expand_template(template: &str, env: &EnvBases) -> Option<PathBuf> {
    let (token, rest) = template.split_once('/')?;
    let base: &Path = match token {
        "{HOME}" => env.home.as_deref()?,
        "{LOCALAPPDATA}" => env.local_app_data.as_deref()?,
        "{PROGRAMFILES}" => env.program_files.as_deref()?,
        "{PROGRAMFILES_X86}" => env.program_files_x86.as_deref()?,
        "{APPLICATIONS}" => env.applications.as_deref()?,
        _ => return None,
    };
    let mut path = base.to_path_buf();
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        path.push(seg);
    }
    Some(path)
}

/// PATH 探测：按扩展名顺序逐个候选、按 PATH 目录顺序逐个目录，命中即返回。
fn resolve_in_path(name: &str, env: &EnvBases, exists: Probe<'_>) -> Option<PathBuf> {
    for dir in &env.path_dirs {
        if env.ext.is_empty() {
            let candidate = dir.join(name);
            if exists(&candidate) {
                return Some(candidate);
            }
            continue;
        }
        for ext in &env.ext {
            let candidate = dir.join(format!("{name}{ext}"));
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// 编辑器中段的 JetBrains 产品名归一：`rustrover64.exe` → `RustRover`。
fn jetbrains_product_name(stem: &str) -> Option<String> {
    let lower = stem.to_ascii_lowercase();
    let base = lower.strip_suffix("64").unwrap_or(&lower);
    let product = match base {
        "rustrover" => "RustRover",
        "idea" => "IntelliJ IDEA",
        "webstorm" => "WebStorm",
        "pycharm" => "PyCharm",
        "goland" => "GoLand",
        "clion" => "CLion",
        "phpstorm" => "PhpStorm",
        "datagrip" => "DataGrip",
        "rubymine" => "RubyMine",
        "rider" => "Rider",
        "androidstudio" => "Android Studio",
        "aqua" => "Aqua",
        "rustrover-eap" => "RustRover (EAP)",
        _ => return None,
    };
    Some(product.to_string())
}

/// JetBrains 探测（追加在候选表末位）：Toolbox 脚本目录 + 各产品安装根的 `bin/*64.exe`。
/// 非标准目录安装（如 `D:\App\RustRover …`）探不到——按决策不做手填路径兜底。
fn detect_jetbrains(env: &EnvBases, exists: Probe<'_>, read_dir: ReadDir<'_>) -> Vec<EditorInfo> {
    let mut found: Vec<EditorInfo> = Vec::new();
    let mut push = |product: &str, path: PathBuf| {
        let id = format!("jetbrains:{}", product.to_ascii_lowercase().replace(' ', ""));
        let path = path.to_string_lossy().to_string();
        if !found.iter().any(|e| e.id == id) {
            found.push(EditorInfo {
                id,
                name: product.to_string(),
                path,
            });
        }
    };

    // 1) Toolbox 生成的可执行垫片目录
    let script_dirs: Vec<PathBuf> = [
        env.local_app_data
            .as_ref()
            .map(|b| b.join("JetBrains").join("Toolbox").join("scripts")),
        env.home
            .as_ref()
            .map(|b| b.join("Library/Application Support/JetBrains/Toolbox/scripts")),
        env.home
            .as_ref()
            .map(|b| b.join(".local/share/JetBrains/Toolbox/scripts")),
    ]
    .into_iter()
    .flatten()
    .collect();
    for dir in script_dirs {
        for entry in read_dir(&dir) {
            let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let lower = stem.to_ascii_lowercase();
            // Toolbox 脚本名形如 rustrover / idea / zed；只认 JetBrains 产品名
            if let Some(product) = jetbrains_product_name(&lower) {
                if exists(&entry) {
                    push(&product, entry);
                }
            }
        }
    }

    // 2) 安装根下的 `<root>/<Product>/bin/*64.exe`
    let install_roots = [
        env
            .program_files
            .as_ref()
            .map(|b| b.join("JetBrains")),
        env
            .local_app_data
            .as_ref()
            .map(|b| b.join("Programs").join("JetBrains")),
    ];
    for root in install_roots.into_iter().flatten() {
        for product_dir in read_dir(&root) {
            let bin = product_dir.join("bin");
            for entry in read_dir(&bin) {
                let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let Some(product) = jetbrains_product_name(stem) {
                    if exists(&entry) {
                        push(&product, entry);
                    }
                }
            }
        }
    }

    found.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    found
}

/// 按注入环境探测编辑器列表（候选表顺序 + 末位 JetBrains 组）。
pub fn detect_editors_with(env: &EnvBases, exists: Probe<'_>, read_dir: ReadDir<'_>) -> Vec<EditorInfo> {
    let mut out: Vec<EditorInfo> = Vec::new();
    for spec in EDITOR_SPECS {
        let hit = spec
            .path_names
            .iter()
            .find_map(|name| resolve_in_path(name, env, exists))
            .or_else(|| {
                spec.fixed
                    .iter()
                    .filter_map(|t| expand_template(t, env))
                    .find(|p| exists(p))
            });
        if let Some(path) = hit {
            out.push(EditorInfo {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                path: path.to_string_lossy().to_string(),
            });
        }
    }
    out.extend(detect_jetbrains(env, exists, read_dir));
    out
}

/// 生产环境探测（IPC `list_editors` 用）。
pub fn detect_editors() -> Vec<EditorInfo> {
    let read_dir = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|it| it.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    detect_editors_with(&EnvBases::from_process(), &|p| p.exists(), &read_dir)
}

/// 构造 `cmd /C` 的脚本命令行（Windows）。
/// 为什么不能直接 `cmd /C <脚本> <参数>`：cmd 对 `/C` 后的命令行有一套引号规则，
/// 当命令行不是「恰好两个引号且中间是带空格的 exe 名」时，它会**剔掉首尾两个引号**——
/// `"C:\Program Files\x\code.cmd" "D:\dir"` 因此会被切成坏命令（路径含空格的编辑器垫片必失败）。
/// 稳妥写法是再包一层引号：`""<脚本>" "<参数1>""`，剥壳后正好是 `"<脚本>" "<参数1>"`。
/// 已知边界：双引号内的 `%VAR%`（以及开启延迟展开时的 `!X!`）仍会被 cmd 展开——
/// Windows 文件名允许 `%`，但这是 cmd 自身的语义，与本层拼接无关。
#[cfg(target_os = "windows")]
fn script_command_line(program: &Path, args: &[&Path]) -> String {
    let mut line = String::from("\"\"");
    line.push_str(&program.to_string_lossy());
    line.push('"');
    for arg in args {
        line.push_str(" \"");
        line.push_str(&arg.to_string_lossy());
        line.push('"');
    }
    line.push('"');
    line
}

/// 启动一个分离进程（不等它退出）；Windows 统一 CREATE_NO_WINDOW。
/// `.cmd`/`.bat` 垫片不是可执行镜像：经 `cmd /C` 并按上述规则自己拼命令行（`raw_arg`）。
/// 返回裸错误串（调用方自行加中文前后缀，避免重复拼接）。
fn spawn_detached(program: &Path, args: &[&Path]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let is_script = program
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "cmd" | "bat"))
            .unwrap_or(false);
        let mut command = if is_script {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C");
            // raw_arg：跳过 std 的逐参数引号包裹，按 cmd 规则自己拼
            c.raw_arg(script_command_line(program, args));
            c
        } else {
            let mut c = std::process::Command::new(program);
            c.args(args);
            c
        };
        command.creation_flags(CREATE_NO_WINDOW);
        return command.spawn().map(|_| ()).map_err(|e| e.to_string());
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = std::process::Command::new(program);
        command.args(args);
        command.spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

/// 在编辑器中打开目录（IPC `open_in_editor`）：按 id 重新探测，未知 id 直接报错。
pub fn open_in_editor(id: &str, dir: &Path) -> Result<(), String> {
    let editors = detect_editors();
    let Some(editor) = editors.iter().find(|e| e.id == id) else {
        return Err(format!("未检测到编辑器：{id}"));
    };
    spawn_detached(Path::new(&editor.path), &[dir])
}

/// Windows Files 应用的渠道别名候选（按优先序）。
pub fn files_alias_candidates(local_app_data: &Path) -> Vec<PathBuf> {
    let dir = local_app_data.join("Microsoft").join("WindowsApps");
    ["files-stable.exe", "files-dev.exe", "files-preview.exe", "files-canary.exe"]
        .iter()
        .map(|name| dir.join(name))
        .collect()
}

/// 挑选 Windows 文件管理器：命中 Files 别名返回其路径；全不命中返回 None（= explorer）。
pub fn pick_windows_file_manager(local_app_data: &Path, probe: Probe<'_>) -> Option<PathBuf> {
    files_alias_candidates(local_app_data)
        .into_iter()
        .find(|p| probe(p))
}

/// 在系统文件管理器中打开目录（IPC `open_dir` / open_data_dir / open_logs_dir 共用）。
/// Windows：Files（`-Directory` → 裸路径 → explorer 三级降级）；macOS `open`；Linux `xdg-open`。
pub fn open_dir(dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let alias = EnvBases::from_process()
            .local_app_data
            .and_then(|local| pick_windows_file_manager(&local, &|p| p.exists()));
        if let Some(alias) = alias {
            // 三级降级：`-Directory`（Files 解析器支持）→ 裸路径（Cmdless/OpenPath 分支）→ explorer
            if spawn_detached(&alias, &[Path::new("-Directory"), dir]).is_ok() {
                return Ok(());
            }
            if spawn_detached(&alias, &[dir]).is_ok() {
                return Ok(());
            }
        }
        return spawn_detached(Path::new("explorer"), &[dir]);
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_detached(Path::new("open"), &[dir]);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return spawn_detached(Path::new("xdg-open"), &[dir]);
    }
}

#[cfg(test)]
mod tests;
