//! 配置与数据目录（G2，[docs/p0-plan](../../../docs/p0-plan.md) §3）。
//! 所有字段 #[serde(default)]：旧 config.json 缺失的字段透明取默认。
//! Schema v2（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：模型归属供应商（providers 嵌套 models）。

use crate::util::atomic::atomic_write;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 受管数据目录名：全局数据根 `~/.codewave` 与各项目主目录下 `<主目录>/.codewave` 共用此名。
pub const MANAGED_DIR_NAME: &str = ".codewave";

/// 改名前的旧受管目录名（仅用于手工迁移场景的识别与兼容测试，运行时不再读写）。
pub const LEGACY_MANAGED_DIR_NAME: &str = ".wavestudio";

/// 全局数据目录（`~/.codewave`）。
pub fn data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(MANAGED_DIR_NAME)
}

/// 缺失时创建数据目录树；返回数据根。
pub fn ensure_dirs() -> std::io::Result<PathBuf> {
    let root = data_dir();
    for sub in [
        "sessions",
        "histories",
        "memories",
        "skills",
        "stats",
        "logs",
        "tmp",
    ] {
        fs::create_dir_all(root.join(sub))?;
    }
    fs::create_dir_all(root.join("tmp").join("compacted"))?;
    fs::create_dir_all(root.join("tmp").join("edit-backup"))?;
    fs::create_dir_all(root.join("tmp").join("cmd-output"))?;
    Ok(root)
}

/// API 协议格式（wire 字符串与前端 types.ts 契约逐字一致）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ApiFormat {
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
}

/// 摊平的运行时模型：由 ProviderConfig + ProviderModel 展开而来，供 provider 层
/// 与请求组装消费。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// 模型条目 id（与 ProviderModel.id 一致）
    pub id: String,
    /// 显示名（= wire 模型名）
    pub name: String,
    /// 请求协议
    pub api_format: ApiFormat,
    /// 端点 Base URL
    pub base_url: String,
    /// 明文/占位 key 池
    pub keys: Vec<String>,
    /// wire 模型名（请求体 model 字段）
    pub model: String,
    /// 单次回复输出上限
    pub max_tokens: u32,
    /// 上下文窗口
    pub context_window: u32,
    /// 默认思考力度（自由字符串，未知视为未设置）
    pub reasoning_effort: Option<String>,
    /// 供应商显示名（模型菜单分组头；None 落「其他」组）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// 支持图片输入（模型菜单 vision 标签）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// 所属供应商 id（key 池冷却按 provider 共享）
    #[serde(default)]
    pub provider_id: String,
    /// 回读 key 的 keyring 账户（= 供应商账户）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyring_accounts: Vec<String>,
    /// 供应商级自定义请求头（摊平自 ProviderConfig；值可含 `${session_id}` 占位符）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderPair>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "New model".into(),
            api_format: ApiFormat::OpenAiChat,
            base_url: String::new(),
            keys: Vec::new(),
            model: String::new(),
            // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md)：8k 会过早截断长回复（仅 Max thinking 预算就吃掉 ~6.5k）；新模型默认 32k
            max_tokens: 32768,
            context_window: 128_000,
            reasoning_effort: None,
            provider: None,
            vision: None,
            provider_id: String::new(),
            keyring_accounts: Vec::new(),
            headers: Vec::new(),
        }
    }
}

/// 供应商下的模型条目（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：wire id 即显示名；窗口 / 输出上限 /
/// 输入类型全部来自用户输入。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProviderModel {
    /// 条目 id（uuid；active_model_id 指向它）
    pub id: String,
    /// wire 模型 id（请求体 `model` 字段）
    pub model: String,
    /// 单次回复输出上限
    pub max_tokens: u32,
    /// 上下文窗口
    pub context_window: u32,
    /// 默认思考力度（自由字符串）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// 输入类型：图片
    pub vision: bool,
    /// 输入类型：视频（保留位，发送路径尚未消费）
    pub video: bool,
}

impl Default for ProviderModel {
    fn default() -> Self {
        ProviderModel {
            id: uuid::Uuid::new_v4().to_string(),
            model: String::new(),
            max_tokens: 32768,
            context_window: 128_000,
            reasoning_effort: None,
            vision: false,
            video: false,
        }
    }
}

/// 供应商级自定义请求头条目（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）：随该供应商所有
/// LLM 请求发送。`value` 支持 `${session_id}` 占位符（请求时替换为会话 uuid）；
/// 明文存储于 config.json（勿放长期密钥，日志与会话 verbose 记录均脱敏）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct HeaderPair {
    /// HTTP 头名（如 `x-opencode-session`）
    pub name: String,
    /// HTTP 头值（如 `${session_id}` / `CodeWave/1.0`）
    pub value: String,
}

/// 模型供应商（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：端点 + 协议 + key 池 + 自有模型列表；一切来自
/// 用户输入（无内置目录）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// 供应商 id（uuid；keyring 账户即此 id）
    pub id: String,
    /// 显示名
    pub name: String,
    /// 请求协议
    pub api_format: ApiFormat,
    /// 端点 Base URL
    pub base_url: String,
    /// key 池（明文或 __keyring__ 占位）
    pub keys: Vec<String>,
    /// 自有模型列表（嵌套结构，schema v2）
    pub models: Vec<ProviderModel>,
    /// 自定义请求头（随该供应商所有请求发送）
    pub headers: Vec<HeaderPair>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: "New provider".into(),
            api_format: ApiFormat::OpenAiChat,
            base_url: String::new(),
            keys: Vec::new(),
            models: Vec::new(),
            headers: Vec::new(),
        }
    }
}

/// 不允许被自定义请求头覆盖的保留名（小写；[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）：
/// 协议必需或影响传输安全。`user-agent` 不在此列——自定义 UA 允许覆盖默认值。
pub const RESERVED_REQUEST_HEADERS: &[&str] = &[
    "content-type",
    "authorization",
    "x-api-key",
    "anthropic-version",
    "host",
    "content-length",
];

/// 校验供应商自定义请求头（IPC 落盘前 + provider 应用时的共享判据）：
/// 头名非空、合法 HTTP token、非保留名、无重名；头值不含 CR/LF（防头部注入）且仅含可见 ASCII
/// （`0x20..=0x7E`）——`reqwest::header::HeaderValue::from_str` 拒绝非 ASCII，非可见值会在应用时被静默丢弃。
pub fn validate_request_headers(headers: &[HeaderPair]) -> Result<(), String> {
    let mut seen: Vec<String> = Vec::new();
    for h in headers {
        // 整行全空（前端「添加」后未填）视为待填占位，跳过；半填行按非法处理
        if h.name.trim().is_empty() && h.value.trim().is_empty() {
            continue;
        }
        let name = h.name.trim();
        if name.is_empty() {
            return Err("自定义请求头名称不能为空".into());
        }
        if !is_http_token(name) {
            return Err(format!("自定义请求头名称非法：{name}"));
        }
        let lower = name.to_ascii_lowercase();
        if RESERVED_REQUEST_HEADERS.contains(&lower.as_str()) {
            return Err(format!("自定义请求头 `{name}` 为保留名，不允许覆盖"));
        }
        if seen.contains(&lower) {
            return Err(format!("自定义请求头 `{name}` 重复"));
        }
        if h.value.contains(['\r', '\n']) {
            return Err(format!("自定义请求头 `{name}` 的值含非法换行"));
        }
        // 非可见 ASCII 会被 reqwest 静默丢弃，前端可通过但上线即失效，故落盘前拦截
        if !h.value.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
            return Err(format!(
                "自定义请求头 `{name}` 的值含非法字符（仅支持可见 ASCII）"
            ));
        }
        seen.push(lower);
    }
    Ok(())
}

/// RFC 7230 token 判定（tchar 允许集）。
fn is_http_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// 代理模式：不使用 / 跟随系统 / 手动指定。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    None,
    System,
    Manual,
}

/// 网络代理配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    /// 代理模式
    pub mode: ProxyMode,
    /// 代理地址（http(s):// 或 socks5://）
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            mode: ProxyMode::System,
            url: String::new(),
        }
    }
}

/// 审批设置（全局默认；新会话由此推导初始档位）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovalSettings {
    /// 审批门总开关（false = 新会话默认 FullAccess）
    pub enabled: bool,
    /// 创建区外写目标需确认
    pub confirm_outside_create: bool,
    /// git push 需确认
    pub confirm_git_push: bool,
    /// [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：勾选后审批 5 分钟无应答自动确认推荐选项（仅 allow，绝不「始终允许」）；
    /// 不勾选则审批永不超时、无限等待用户
    pub auto_confirm: bool,
    /// 审批「始终允许（本项目）」写入的命令白名单（trim 后整条全文匹配；[docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)）
    pub command_allowlist: Vec<String>,
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        ApprovalSettings {
            enabled: true,
            confirm_outside_create: true,
            confirm_git_push: true,
            auto_confirm: false,
            command_allowlist: Vec::new(),
        }
    }
}

/// 写文件后自动语义校验的语言开关集。
/// [docs/lsp-post-write-diagnostics](../../../docs/lsp-post-write-diagnostics.md)：v3 起
/// typescript/rust/python/go/java/dart 为对应语言的 **LSP 语义诊断**开关；json 仍走内置 serde_json 解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ValidationSettings {
    /// Python 语义校验开关
    pub python: bool,
    /// Rust 语义校验开关
    pub rust: bool,
    /// TypeScript/JavaScript 语义校验开关
    pub typescript: bool,
    /// Go 语义校验开关
    pub go: bool,
    /// JSON 校验开关（内置解析，非 LSP）
    pub json: bool,
    /// Java 语义校验开关（默认关闭：jdtls 需要 JDK 21+ 与依赖树索引，首次启用需用户确认）
    pub java: bool,
    /// Dart/Flutter 语义校验开关
    pub dart: bool,
    /// LSP 服务配置（命令覆盖 / JDK / 额外 SDK 根 / 预算）
    pub lsp: LspSettings,
}

impl Default for ValidationSettings {
    fn default() -> Self {
        ValidationSettings {
            python: true,
            rust: true,
            typescript: true,
            go: true,
            json: true,
            java: false,
            dart: true,
            lsp: LspSettings::default(),
        }
    }
}

/// 六语言 server 命令覆盖（留空 = 自动探测；探测顺序见 `lsp::discovery`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LspCommands {
    /// TypeScript/JavaScript server 命令
    pub typescript: String,
    /// Rust server 命令
    pub rust: String,
    /// Python server 命令
    pub python: String,
    /// Go server 命令
    pub go: String,
    /// Java server 命令（jdtls）
    pub java: String,
    /// Dart server 命令
    pub dart: String,
}

/// 语言服务器诊断的全局预算与发现配置（新字段必须 serde default 向前兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LspSettings {
    /// 各语言 server 命令覆盖
    pub commands: LspCommands,
    /// jdtls 使用的 JDK 21+ 路径（留空 = 自动探测；**不读 JAVA_HOME**，它常指向旧版本）
    pub java_home: String,
    /// 额外 SDK 根目录（如 `D:\\Sdk`；探测 `<root>/<lang>/bin`）
    pub extra_roots: Vec<String>,
    /// 写后同步等待诊断的毫秒预算
    pub sync_window_ms: u64,
    /// 单次回喂的诊断条数上限
    pub max_diagnostics: usize,
    /// 单次回喂的诊断文本字符上限
    pub max_chars: usize,
    /// 项目级 server 闲置回收时长（毫秒）
    pub idle_ttl_ms: u64,
    /// 单项目并发 server 上限
    pub max_servers: usize,
    /// 超过该体积的文件跳过语义校验
    pub max_file_bytes: u64,
    /// 同一诊断指纹在同一文件最多回喂次数
    pub dedupe_limit: usize,
}

impl Default for LspSettings {
    fn default() -> Self {
        LspSettings {
            commands: LspCommands::default(),
            java_home: String::new(),
            extra_roots: Vec::new(),
            sync_window_ms: 1500,
            max_diagnostics: 20,
            max_chars: 4000,
            idle_ttl_ms: 600_000,
            max_servers: 8,
            max_file_bytes: 1024 * 1024,
            dedupe_limit: 2,
        }
    }
}

/// 界面偏好（字体大小 / 主题语言 / 关闭行为等）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    /// 界面基础字号
    pub font_size: f32,
    /// 主题 accent（历史遗留键；现行强调色为墨色，见 docs/43）
    pub accent: String,
    /// 界面语言（zh-CN 等）
    pub language: String,
    /// 关闭按钮隐藏到托盘（P2-I，默认开；托盘菜单提供退出）
    pub close_to_tray: bool,
    /// 指示 AI 回复所用语言（设置 → 通用 → AI 语言；自由输入，如 "中文" /
    /// "English" / "日本語"）。None/空 = 跟随用户消息语言（默认 output-style 行为）。
    pub ai_language: Option<String>,
    /// 界面字体（逗号分隔的已安装字体名，空 = 默认链）。
    ///
    /// 真源在后端配置文件（[docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)）：
    /// 2026-09-19 之前只存 WebView 的 localStorage，实测出现过「输了界面字体却从来没写进去」；
    /// 现改为「配置为真源 + localStorage 当首帧缓存」（防闪变），对照逻辑在前端 `utils/fonts.ts`。
    /// 该字段由 `set_font_prefs` 独占维护，页级 `save_config` 会把它护住不回写旧值。
    pub font_sans: String,
    /// 等宽字体（代码与日志），同上。
    pub font_mono: String,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            font_size: 15.0,
            accent: "cyan".into(),
            language: "zh-CN".into(),
            close_to_tray: true,
            ai_language: None,
            font_sans: String::new(),
            font_mono: String::new(),
        }
    }
}

/// 网络层设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct NetworkConfig {
    /// 让 SSRF 守卫放行私网地址（访问本地模型 / API 网关时开启）
    pub allow_private_network: bool,
}

/// 日志设置（[docs/session-logging-report](../../../docs/session-logging-report.md)）：全局级别可热切换 + 会话级 verbose 模式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// 全局诊断日志级别（存在 RUST_LOG 环境变量时被覆盖）
    pub level: String,
    /// 会话日志额外记录完整 LLM 请求/响应文本（默认只记元数据）
    pub session_verbose: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        LogConfig {
            level: "info".into(),
            session_verbose: false,
        }
    }
}

/// shell 选择配置（设置 → 命令 shell；执行与 prompt 环境段共用同一事实源）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// shell id（None / "auto" = 自动探测；其余值经 resolve_shell 按本机可用性解析）
    pub selection: Option<String>,
}

/// 会话保留期设置（[docs/session-cleanup](../../../docs/session-cleanup.md)）：超过保留期且不在运行中的会话
/// 会被清理（历史 / 边车 / 会话日志 / 计划文件）。默认不清理——删除不可恢复，默认值必须保守。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSettings {
    /// 保留天数（None = 不清理；设置页档位 1/3/7/14/30）；wire 名 `sessions.retention_days`
    pub retention_days: Option<u32>,
}

/// 全局配置状态（config.json 的根结构；所有新字段必须 serde default 向前兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigState {
    /// schema 版本（v2；读入后恒强制为 2）
    pub schema_version: u32,
    /// 供应商列表（嵌套模型）
    pub providers: Vec<ProviderConfig>,
    /// 全局 active 模型（指向模型条目 id）
    pub active_model_id: Option<String>,
    /// 代理配置（None = 未配置）
    pub proxy: Option<ProxyConfig>,
    /// 网络设置
    pub network: NetworkConfig,
    /// 自动压缩阈值（占窗口比例）
    pub compact_threshold: f32,
    /// 压缩摘要请求超时秒数（[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md)，钳到 [30, 3600]；serde default 保旧配置兼容）
    pub compact_timeout_seconds: u64,
    /// LLM 流停滞判定阈值秒数（窗口内无任何 delta 即视为停滞：取消本次尝试并按可重试
    /// 错误退避重试，[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)；
    /// 钳到 [5, 3600]；serde default 保旧配置兼容；本地大上下文模型预首 token 慢可调大）
    pub stall_timeout_seconds: u64,
    /// 审批设置
    pub approval: ApprovalSettings,
    /// 语法校验开关
    pub validation: ValidationSettings,
    /// 界面偏好
    pub ui: UiPrefs,
    /// 自定义 prompt（第 6 层注入）
    pub custom_prompt: Option<String>,
    /// 禁用技能名列表
    pub disabled_skills: Vec<String>,
    /// 日志设置
    pub log: LogConfig,
    /// shell 选择设置（[docs/shell-selection](../../../docs/shell-selection.md)）
    pub shell: ShellConfig,
    /// 会话保留期设置（[docs/session-cleanup](../../../docs/session-cleanup.md)）
    pub sessions: SessionSettings,
}

impl Default for ConfigState {
    fn default() -> Self {
        ConfigState {
            schema_version: 2,
            providers: Vec::new(),
            active_model_id: None,
            proxy: None,
            network: NetworkConfig::default(),
            compact_threshold: 0.6,
            compact_timeout_seconds: 180,
            stall_timeout_seconds: 300,
            approval: ApprovalSettings::default(),
            validation: ValidationSettings::default(),
            ui: UiPrefs::default(),
            custom_prompt: None,
            disabled_skills: Vec::new(),
            log: LogConfig::default(),
            shell: ShellConfig::default(),
            sessions: SessionSettings::default(),
        }
    }
}

impl ConfigState {
    /// 配置文件路径：~/.codewave/config.json。
    pub fn config_path() -> PathBuf {
        data_dir().join("config.json")
    }

    /// 从磁盘加载配置；解析失败回退默认。
    pub fn load() -> Self {
        let path = Self::config_path();
        let mut cfg = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ConfigState>(&bytes) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::warn!("config.json 解析失败，回退默认：{e}");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        };
        cfg.schema_version = 2;
        cfg
    }

    /// 原子持久化配置。
    pub fn save(&self) -> anyhow::Result<()> {
        ensure_dirs()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        atomic_write(&Self::config_path(), &bytes)?;
        Ok(())
    }

    /// 供应商 + 模型摊平为运行时视图；wire id 兼作显示名。
    fn flatten(p: &ProviderConfig, m: &ProviderModel) -> ModelConfig {
        let keyring_accounts = vec![p.id.clone()];
        ModelConfig {
            id: m.id.clone(),
            name: m.model.clone(),
            api_format: p.api_format.clone(),
            base_url: p.base_url.clone(),
            keys: p.keys.clone(),
            model: m.model.clone(),
            max_tokens: m.max_tokens,
            context_window: m.context_window,
            reasoning_effort: m.reasoning_effort.clone(),
            provider: Some(p.name.clone()),
            vision: Some(m.vision),
            provider_id: p.id.clone(),
            keyring_accounts,
            headers: p.headers.clone(),
        }
    }

    /// 跨全部供应商按 id 查模型（摊平运行时视图）。
    pub fn find_model(&self, id: &str) -> Option<ModelConfig> {
        self.providers.iter().find_map(|p| {
            p.models
                .iter()
                .find(|m| m.id == id)
                .map(|m| Self::flatten(p, m))
        })
    }

    /// 当前 active 模型（摊平形态）。
    pub fn active_model(&self) -> Option<ModelConfig> {
        self.find_model(self.active_model_id.as_deref()?)
    }

    /// 定位模型所属供应商（可变；供 e2e 注入 key 等场景）。
    pub fn provider_of_model_mut(&mut self, model_id: &str) -> Option<&mut ProviderConfig> {
        self.providers
            .iter_mut()
            .find(|p| p.models.iter().any(|m| m.id == model_id))
    }

    /// 配置脱敏：key 只保留末 4 字符与存在性；明文绝不出境（[docs/p0-plan](../../../docs/p0-plan.md) §3.3）。
    pub fn sanitized(&self) -> ConfigState {
        let mut c = self.clone();
        for p in &mut c.providers {
            p.keys = p
                .keys
                .iter()
                .map(|k| {
                    if k.is_empty() {
                        String::new()
                    } else if k == KEYRING_PLACEHOLDER {
                        KEYRING_PLACEHOLDER.into()
                    } else {
                        Self::mask(k)
                    }
                })
                .collect();
        }
        c
    }

    /// 前端回传的 key 可能是掩码形态（***abcd）——
    /// H2 修复：仅当掩码与旧 key 的掩码完全相等时才还原明文；
    /// 插入/删除行导致的下标移位绝不还原出另一把 key，匹配不上的掩码行
    /// （行序重排或新供应商）直接丢弃——掩码字符串绝不作为 key 落盘。
    pub fn unmask_from(&mut self, previous: &ConfigState) {
        for p in &mut self.providers {
            let prev_keys = previous
                .providers
                .iter()
                .find(|x| x.id == p.id)
                .map(|x| x.keys.as_slice())
                .unwrap_or(&[]);
            let mut resolved: Vec<String> = Vec::with_capacity(p.keys.len());
            for k in &p.keys {
                if !k.starts_with("***") {
                    resolved.push(k.clone());
                    continue;
                }
                if let Some(orig) = prev_keys
                    .iter()
                    .find(|q| Self::mask(q) == *k && !q.starts_with("***"))
                {
                    resolved.push(orig.clone());
                }
                // 匹配不上的掩码行：无法还原为明文 key → 直接丢弃
            }
            p.keys = resolved;
        }
    }

    /// 掩码生成：*** + 末 4 字符（空 key 返回空串）。
    fn mask(key: &str) -> String {
        if key.is_empty() {
            return String::new();
        }
        // 末 4 字节可能落在多字节字符内部；回退到字符边界避免 panic
        let mut take = 4.min(key.len());
        while take > 0 && !key.is_char_boundary(key.len() - take) {
            take -= 1;
        }
        format!("***{}", &key[key.len() - take..])
    }
}

/// 钥匙串 key 的占位符（配置文件里永不落明文的哨兵值）。
pub const KEYRING_PLACEHOLDER: &str = "__keyring__";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_format_contract_strings_roundtrip() {
        // 前端契约使用 openai_chat / openai_responses（types.ts）；后端 serde snake_case
        // 曾序列化出 open_ai_chat / open_ai_responses → save_config 因 unknown variant 失败。
        // 回归：序列化输出契约字符串；旧 snake_case 仍可反序列化。
        for (variant, modern) in [
            (ApiFormat::OpenAiChat, "openai_chat"),
            (ApiFormat::OpenAiResponses, "openai_responses"),
        ] {
            // 序列化：输出新契约字符串
            let s = serde_json::to_string(&variant).unwrap();
            assert_eq!(s, format!("\"{modern}\""));
            // 反序列化：新字符串可读回
            assert_eq!(
                serde_json::from_str::<ApiFormat>(&format!("\"{modern}\"")).unwrap(),
                variant
            );
        }
        // AnthropicMessages 契约字符串不变
        assert_eq!(
            serde_json::to_string(&ApiFormat::AnthropicMessages).unwrap(),
            "\"anthropic_messages\""
        );
    }

    #[test]
    fn mask_non_ascii_tail_no_panic() {
        for k in ["你好世界", "😀", "a中😀文z", "abc"] {
            let m = ConfigState::mask(k);
            assert!(m.starts_with("***"));
            assert_eq!(m, ConfigState::mask(k)); // 掩码自洽（unmask 匹配依赖此性质）
        }
        assert_ne!(
            ConfigState::mask("你好世界甲"),
            ConfigState::mask("你好世界乙")
        );
    }

    #[test]
    fn default_roundtrip_and_compat() {
        let cfg = ConfigState::default();
        let s = serde_json::to_string(&cfg).unwrap();
        // 旧配置缺失字段：只有 schema_version
        let old = r#"{"schema_version":1}"#;
        let parsed: ConfigState = serde_json::from_str(old).unwrap();
        assert_eq!(parsed.compact_threshold, 0.6);
        assert!(parsed.approval.enabled);
        // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：旧 approval 段缺 command_allowlist → serde 默认空列表
        let legacy_approval: ApprovalSettings = serde_json::from_str(
            r#"{"enabled":true,"confirm_outside_create":false,"confirm_git_push":true}"#,
        )
        .unwrap();
        assert!(legacy_approval.command_allowlist.is_empty());
        assert!(!legacy_approval.confirm_outside_create);
        let _ = s;
    }

    /// shell 小节：全新默认 = None；旧格式缺 `shell` 键时 serde default 兼容。
    #[test]
    fn shell_config_default_and_legacy_compat() {
        let cfg = ConfigState::default();
        assert_eq!(cfg.shell.selection, None);

        // 旧格式（无 shell 键）解析后 selection = None，旧数据零迁移成本
        let old: ConfigState = serde_json::from_str(r#"{"schema_version":1}"#).unwrap();
        assert_eq!(old.shell.selection, None);

        // 显式值 roundtrip：序列化保留选择、反序列化读回一致
        let with_sel: ConfigState =
            serde_json::from_str(r#"{"schema_version":2,"shell":{"selection":"cmd"}}"#).unwrap();
        assert_eq!(with_sel.shell.selection.as_deref(), Some("cmd"));
        let json = serde_json::to_string(&with_sel).unwrap();
        assert!(json.contains("\"selection\":\"cmd\""));
        // null selection 与 None 等价（serde Option）
        let null_sel: ConfigState =
            serde_json::from_str(r#"{"shell":{"selection":null}}"#).unwrap();
        assert_eq!(null_sel.shell.selection, None);
    }

    /// 会话保留期（[docs/session-cleanup](../../../docs/session-cleanup.md)）：默认不清理；
    /// 旧配置缺 `sessions` 键可读；显式值 roundtrip 且 wire 形态为 `sessions.retention_days`。
    #[test]
    fn session_settings_default_and_legacy_compat() {
        let cfg = ConfigState::default();
        assert_eq!(cfg.sessions.retention_days, None, "默认必须是「不清理」");

        // 旧配置（无 sessions 键）→ 透明取默认（零迁移成本）
        let old: ConfigState = serde_json::from_str(r#"{"schema_version":1}"#).unwrap();
        assert_eq!(old.sessions.retention_days, None);

        // 显式值：反序列化读回一致，且序列化形态是前端契约的嵌套键
        let with_days: ConfigState =
            serde_json::from_str(r#"{"schema_version":2,"sessions":{"retention_days":7}}"#)
                .unwrap();
        assert_eq!(with_days.sessions.retention_days, Some(7));
        let v = serde_json::to_value(&with_days).unwrap();
        assert_eq!(v["sessions"]["retention_days"], serde_json::json!(7));

        // null 与 None 等价（前端「不清理」可能传 null）
        let null_days: ConfigState =
            serde_json::from_str(r#"{"sessions":{"retention_days":null}}"#).unwrap();
        assert_eq!(null_days.sessions.retention_days, None);
    }

    #[test]
    fn unmask_index_shift_does_not_swap_keys() {
        // H2 回归：在掩码行上方插入行导致下标移位时，不得还原出另一个 key
        let mut prev = ConfigState::default();
        prev.providers.push(ProviderConfig {
            keys: vec!["sk-first-1111".into(), "sk-second-2222".into()],
            ..Default::default()
        });
        let mut incoming = ConfigState::default();
        // H2 按 provider id 匹配 → 复用 prev 的 id
        let provider_id = prev.providers[0].id.clone();
        incoming.providers.push(ProviderConfig {
            id: provider_id,
            keys: vec!["sk-new-plain".into(), mask2("sk-second-2222")],
            ..Default::default()
        });
        incoming.unmask_from(&prev);
        // 第 0 行是用户新输入的明文（旧行 0 的掩码 ***1111 不匹配 → 保留用户输入）
        assert_eq!(incoming.providers[0].keys[0], "sk-new-plain");
        // 第 1 行掩码等于旧第二把 key → 还原
        assert_eq!(incoming.providers[0].keys[1], "sk-second-2222");
    }

    fn mask2(k: &str) -> String {
        format!("***{}", &k[k.len() - 4..])
    }

    #[test]
    fn sanitize_hides_keys() {
        let mut cfg = ConfigState::default();
        cfg.providers.push(ProviderConfig {
            keys: vec!["sk-abcdef123456".into()],
            ..Default::default()
        });
        let s = cfg.sanitized();
        assert_eq!(s.providers[0].keys[0], "***3456");
        assert!(!serde_json::to_string(&s).unwrap().contains("abcdef123456"));
    }

    /// unmask 回归：新供应商（prev 无同 id）保留明文；匹配不上的掩码行被丢弃。
    #[test]
    fn unmask_new_provider_drops_unmatched_mask() {
        let prev = ConfigState::default();
        let mut incoming = ConfigState::default();
        incoming.providers.push(ProviderConfig {
            id: "new-p".into(),
            keys: vec!["sk-fresh".into(), "***1234".into()],
            ..Default::default()
        });
        incoming.unmask_from(&prev);
        assert_eq!(incoming.providers[0].keys, vec!["sk-fresh".to_string()]);
    }

    #[test]
    fn find_model_scopes_ids_within_all_providers() {
        let mut cfg = ConfigState::default();
        cfg.providers.push(ProviderConfig {
            id: "p1".into(),
            name: "P1".into(),
            api_format: ApiFormat::AnthropicMessages,
            base_url: "https://a.example".into(),
            keys: vec!["k".into()],
            models: vec![ProviderModel {
                id: "m1".into(),
                model: "model-x".into(),
                max_tokens: 1024,
                context_window: 64000,
                reasoning_effort: Some("high".into()),
                vision: true,
                video: false,
            }],
            headers: vec![HeaderPair {
                name: "x-opencode-session".into(),
                value: "${session_id}".into(),
            }],
        });
        let m = cfg.find_model("m1").unwrap();
        assert_eq!(m.model, "model-x");
        assert_eq!(m.name, "model-x");
        assert_eq!(m.api_format, ApiFormat::AnthropicMessages);
        assert_eq!(m.provider_id, "p1");
        assert_eq!(m.keyring_accounts, vec!["p1".to_string()]);
        assert_eq!(m.reasoning_effort.as_deref(), Some("high"));
        // 自定义请求头随 provider 摊平到运行时视图（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）
        assert_eq!(m.headers.len(), 1);
        assert_eq!(m.headers[0].name, "x-opencode-session");
        assert_eq!(m.headers[0].value, "${session_id}");
        assert!(cfg.find_model("nope").is_none());
        assert!(cfg.active_model().is_none());
    }

    /// [docs/provider-custom-headers](../../../docs/provider-custom-headers.md)：旧 config.json 无 `headers` 字段时透明取空（serde default 前向兼容）。
    #[test]
    fn provider_headers_default_when_absent() {
        let json = r#"{
            "providers": [{
                "id": "p1",
                "name": "P1",
                "api_format": "openai_chat",
                "base_url": "https://a.example/v1",
                "keys": ["k"],
                "models": [{"id": "m1", "model": "m"}]
            }]
        }"#;
        let cfg: ConfigState = serde_json::from_str(json).unwrap();
        assert!(cfg.providers[0].headers.is_empty());
        assert!(cfg.find_model("m1").unwrap().headers.is_empty());
    }

    /// [docs/provider-custom-headers](../../../docs/provider-custom-headers.md)：自定义头校验规则（空名/非法名/保留名/重名/换行/值仅可见 ASCII）。
    #[test]
    fn validate_request_headers_rules() {
        let ok = vec![HeaderPair {
            name: "x-opencode-session".into(),
            value: "${session_id}".into(),
        }];
        assert!(validate_request_headers(&ok).is_ok());

        let empty = vec![HeaderPair {
            name: "  ".into(),
            value: "v".into(),
        }];
        assert!(validate_request_headers(&empty).is_err());

        // 整行全空视为待填占位，跳过
        let blank = vec![HeaderPair {
            name: "".into(),
            value: "".into(),
        }];
        assert!(validate_request_headers(&blank).is_ok());

        let bad = vec![HeaderPair {
            name: "bad name".into(),
            value: "v".into(),
        }];
        assert!(validate_request_headers(&bad).is_err());

        let reserved = vec![HeaderPair {
            name: "Authorization".into(),
            value: "Bearer x".into(),
        }];
        assert!(validate_request_headers(&reserved).is_err());

        let dup = vec![
            HeaderPair {
                name: "X-A".into(),
                value: "1".into(),
            },
            HeaderPair {
                name: "x-a".into(),
                value: "2".into(),
            },
        ];
        assert!(validate_request_headers(&dup).is_err());

        let crlf = vec![HeaderPair {
            name: "x-a".into(),
            value: "a\r\nb".into(),
        }];
        assert!(validate_request_headers(&crlf).is_err());

        // 非 ASCII（CJK）值：前端可通过，但 reqwest HeaderValue 解析失败会静默丢弃 → 必须拦截
        let cjk = vec![HeaderPair {
            name: "x-a".into(),
            value: "中文值".into(),
        }];
        assert!(validate_request_headers(&cjk).is_err());

        // 控制字符（TAB）值非法
        let tab = vec![HeaderPair {
            name: "x-a".into(),
            value: "a\tb".into(),
        }];
        assert!(validate_request_headers(&tab).is_err());

        // 普通可见 ASCII 值合法
        let ascii = vec![HeaderPair {
            name: "x-a".into(),
            value: "Bearer abc-123/._~".into(),
        }];
        assert!(validate_request_headers(&ascii).is_ok());
    }
}
