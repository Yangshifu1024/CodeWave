//! 项目级常驻 LSP 语义诊断（[docs/lsp-post-write-diagnostics](../../../docs/lsp-post-write-diagnostics.md)）。
//!
//! 分层：与 `mcp/` 平级的外部协议客户端，纯 Rust 不依赖 tauri；`tools/validation.rs` 消费本模块，
//! `host/commands/lsp.rs` 只做校验 + 转调。
//!
//! 设计要点：
//! - **项目级共享池**（`pool.rs`）：key = `(project_id, root, language)`，惰性启动、闲置回收；
//! - **新鲜 PATH 发现**（`discovery.rs`）：进程启动快照会过期（装完 JDK/Flutter 不重启就看不见），
//!   故探测未命中时再读系统权威 PATH；
//! - **写前基线 + 写后差集**（`diagnostics.rs`）：只回喂本次写入**新增**的 error 级诊断；
//! - **客户端层是通用通道**：能力协商 + 可发任意 request + 服务端主动请求应答分发，
//!   本批次只交付写后诊断，将来加语义查询只写封装。

pub mod client;
pub mod diagnostics;
pub mod discovery;
pub mod install;
pub mod manager;
pub mod pool;
pub mod protocol;
pub mod sdk;
pub mod server_spec;
pub mod workspace;

pub use manager::LspManager;

// 追加（不改既有契约）：把校验相关配置类型从私有 `core` 模块再导出。
// host 层在同一 crate 内不受影响；集成测试（外部 crate 视角）需要按名构造
// `ValidationSettings` / `LspSettings`，否则无法覆盖开关与预算语义。
pub use crate::core::config::{LspSettings, ValidationSettings};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 受支持语言（六种；JSON 走内置解析不在此列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// TypeScript / JavaScript（同一 server，languageId 按扩展名区分）
    TypeScript,
    /// Rust
    Rust,
    /// Python
    Python,
    /// Go
    Go,
    /// Java
    Java,
    /// Dart / Flutter
    Dart,
}

impl Lang {
    /// 全部语言（设置页行序）。
    pub fn all() -> [Lang; 6] {
        [
            Lang::TypeScript,
            Lang::Rust,
            Lang::Python,
            Lang::Go,
            Lang::Java,
            Lang::Dart,
        ]
    }

    /// 稳定标识（前端 i18n key、配置字段名、事件 payload）。
    pub fn id(self) -> &'static str {
        match self {
            Lang::TypeScript => "typescript",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Dart => "dart",
        }
    }

    /// 展示名（设置页；前端另有 i18n，此处供后端文案拼接）。
    pub fn display_name(self) -> &'static str {
        match self {
            Lang::TypeScript => "TypeScript/JavaScript",
            Lang::Rust => "Rust",
            Lang::Python => "Python",
            Lang::Go => "Go",
            Lang::Java => "Java",
            Lang::Dart => "Dart/Flutter",
        }
    }

    /// 按扩展名路由。None = 本批次不做语义校验（`.json` 走内置解析；`.vue`/`.svelte` 明确告知未覆盖）。
    pub fn from_path(path: &Path) -> Option<Lang> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        match ext.as_str() {
            // JS 家族并入 TypeScript server（此前由 `node --check` 覆盖，删旧路径不得静默丢覆盖）
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TypeScript),
            "rs" => Some(Lang::Rust),
            "py" | "pyi" => Some(Lang::Python),
            "go" => Some(Lang::Go),
            "java" => Some(Lang::Java),
            "dart" => Some(Lang::Dart),
            _ => None,
        }
    }

    /// LSP `languageId`（决定 server 用哪套解析器）。
    pub fn language_id(self, path: &Path) -> &'static str {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match self {
            Lang::TypeScript => match ext.as_str() {
                "tsx" => "typescriptreact",
                "jsx" => "javascriptreact",
                "js" | "mjs" | "cjs" => "javascript",
                _ => "typescript",
            },
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Dart => "dart",
        }
    }

    /// 该语言的开关当前是否打开（读 `ValidationSettings` 对应 bool）。
    pub fn enabled_in(self, v: &crate::core::config::ValidationSettings) -> bool {
        match self {
            Lang::TypeScript => v.typescript,
            Lang::Rust => v.rust,
            Lang::Python => v.python,
            Lang::Go => v.go,
            Lang::Java => v.java,
            Lang::Dart => v.dart,
        }
    }
}

/// 单条诊断（已裁剪为回喂所需字段；行列均为 1-based，与模型习惯一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticItem {
    /// 相对项目根的路径（展示与回喂用）
    pub path: String,
    /// 1-based 行
    pub line: u32,
    /// 1-based 列
    pub col: u32,
    /// 诊断 code（server 未给时填 `?`）
    pub code: String,
    /// 单行化 message（`relatedInformation` 折进尾部）
    pub message: String,
    /// severity+range+code+message 的指纹（去重刹车用）
    pub fingerprint: String,
}

impl DiagnosticItem {
    /// 回喂文本的一行：`<path>:<line>:<col> <code> <message>`。
    pub fn render(&self) -> String {
        format!(
            "{}:{}:{} {} {}",
            self.path, self.line, self.col, self.code, self.message
        )
    }
}

/// 单文件写后校验结果（**三态**：通过 / 有诊断 / 跳过）。
///
/// 跳过必须带原因 —— 历史实现「未运行却显示校验通过」的隐性 bug 由此结构性消除。
#[derive(Debug, Clone)]
pub enum ValidationOutcome {
    /// 跑通了且无 error 级诊断
    Passed {
        /// 使用的语言
        lang: Lang,
    },
    /// 有 error 级诊断（已按差集与预算裁剪）
    Diagnosed {
        /// 使用的语言
        lang: Lang,
        /// 回喂的诊断
        items: Vec<DiagnosticItem>,
        /// 本次写入顺带消除的既有 error 条数（正反馈）
        removed: usize,
        /// 因预算被截断的条数（>0 时文案必须明示「已截断 N 条」）
        truncated: usize,
    },
    /// 未运行（原因明确，文案必须如实告知）
    Skipped {
        /// 语言（`.vue` 等未覆盖类型为 None）
        lang: Option<Lang>,
        /// 跳过原因
        reason: SkipReason,
    },
}

impl ValidationOutcome {
    /// 是否真的跑过校验（`false` ⇒ 文案里**绝不允许**出现「通过」）。
    pub fn ran(&self) -> bool {
        !matches!(self, ValidationOutcome::Skipped { .. })
    }
}

/// 跳过语义校验的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// 未找到该语言的 server（触发引导安装卡片）
    NoServer {
        /// server 名（展示用，如 `rust-analyzer`）
        server: String,
    },
    /// server 正在冷启动/加载项目（异步补条接管；文案与 NoServer 必须区分）
    ServerLoading,
    /// server 尚无就绪证据、基线不可信（**本轮不回喂**，也不会补发；与 [`Self::ServerLoading`] 的
    /// 区别：后者是「基线可信、只是诊断迟到」，可异步补发）
    ServerNotReady,
    /// 该文件类型本批次不覆盖（`.vue`/`.svelte`/其他）
    Unsupported,
    /// 临时会话（免目录，无项目主目录）→ 不做语义校验
    NoProject,
    /// 该语言开关关闭（含 Java 默认关闭）
    Disabled,
    /// 文件体积超预算
    FileTooLarge {
        /// 实际字节数
        bytes: u64,
    },
}

/// 一次写后校验请求（由 `tools/create.rs` / `tools/edit/tool.rs` 构造）。
#[derive(Debug, Clone)]
pub struct ValidateRequest {
    /// 项目 id（临时会话为 None → 不做语义校验）
    pub project_id: Option<String>,
    /// 项目主目录（root 扫描起点）
    pub project_root: PathBuf,
    /// 被写文件绝对路径
    pub path: PathBuf,
    /// 被写文件相对项目根的路径（展示用）
    pub rel_path: String,
    /// 语言
    pub lang: Lang,
    /// 写入后内容（None = 由本模块读盘）
    pub new_content: Option<String>,
    /// 写入前内容（None = 新文件或读不到 → 基线视为空集）
    pub prev_content: Option<String>,
}

/// 写入前建立的诊断基线（差集计算的参照；不透明句柄）。
#[derive(Debug, Clone, Default)]
pub struct Baseline {
    /// 写入前该文件的 error 级诊断指纹集合
    pub(crate) fingerprints: Vec<String>,
    /// 基线是否可信（false = server 未就绪，差集不可用 → 调用方应放弃回喂）
    pub(crate) reliable: bool,
}

impl Baseline {
    /// 基线是否可信。
    pub fn reliable(&self) -> bool {
        self.reliable
    }
}

/// 安装引导的形态（前端卡片三景）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    /// 可一键安装（TS/JS、Python 走 npx；Go 走 go install）
    Installable,
    /// 需用户手动安装（Java：jdtls 无跨平台官方装法）
    Manual,
    /// 需用户先确认启用（Java 默认关闭）
    ConfirmEnable,
}

/// 安装引导信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallHint {
    /// 形态
    pub kind: InstallKind,
    /// 可一键执行的命令（Manual/ConfirmEnable 为 None）
    pub command: Option<String>,
    /// 官方文档/下载地址（Manual 必填）
    pub docs_url: Option<String>,
    /// 前置条件（如「需 JDK 21+」）
    pub prerequisite: Option<String>,
    /// 一键安装所需的前置命令探测（`installable` 才有值）
    #[serde(default)]
    pub requires: Option<InstallRequirement>,
}

/// 一键安装的前置命令（如 TS/Python 依赖的 `npm`）。
///
/// 为什么要单独探测：安装命令写死为 `npm i -g ...`，而机器上没装 Node.js 时用户只会在点击后
/// 拿到一句含糊的「未找到 npm，无法执行安装」；有了本字段，设置页能在点击前就提示「先装 Node.js」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRequirement {
    /// 可执行名（`npm` / `go` / `rustup`）
    pub command: String,
    /// 展示名（如 `Node.js`）：与 [`SdkStatus::name`] 分开——Python 行缺的是 Node.js，
    /// 不能拿「Python」充数。
    #[serde(default)]
    pub name: String,
    /// 当前能否解析到（生效 PATH + 语言约定目录）
    pub ready: bool,
    /// 缺失时的官方下载地址（如 Node.js 下载页）
    pub docs_url: Option<String>,
}

/// 语言工具链（SDK）就绪情况（[`ServerStatus::sdk`] 的数据源）。
///
/// 与「server 是否找到」分开表达：前者是语言本身（Go 工具链 / JDK / Node.js），后者是语言服务器
/// （gopls / jdtls / typescript-language-server）——两件事的解决方式完全不同，不能混成一句「未找到」。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkStatus {
    /// 是否已就绪
    pub ready: bool,
    /// 展示名（如 `Node.js` / `JDK 21+`）
    pub name: String,
    /// 人类可读补充（找到的可执行文件路径，或没找到的原因）
    pub detail: String,
}

/// 单语言 server 状态（设置页展示 + `lsp:server_missing` 事件判定）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    /// `Lang::id()`
    pub language: String,
    /// 该语言开关是否打开
    pub enabled: bool,
    /// 是否找到可用 server
    pub found: bool,
    /// 来源：`config` | `project` | `fresh_path` | `lang_bin` | `path` | `extra_root` | `npx`
    /// | `heuristic` | `""`
    pub source: String,
    /// 解析出的启动命令（展示用，含参数）
    pub command: String,
    /// server 版本（能探测到时）
    pub version: Option<String>,
    /// 人类可读补充（未找到原因 / JDK 缺失等）
    pub detail: String,
    /// 只有在「未启用」或「未找到」时才有值
    pub install: Option<InstallHint>,
    /// 该语言的工具链（SDK）是否就绪（与 server 是否找到分开表达）
    #[serde(default)]
    pub sdk: SdkStatus,
}
