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
        }
    }
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

/// 写文件后自动语法校验的语言开关集。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ValidationSettings {
    /// Python 校验开关
    pub python: bool,
    /// Rust 校验开关
    pub rust: bool,
    /// TypeScript 校验开关
    pub typescript: bool,
    /// Go 校验开关
    pub go: bool,
    /// JSON 校验开关
    pub json: bool,
}

impl Default for ValidationSettings {
    fn default() -> Self {
        ValidationSettings {
            python: true,
            rust: true,
            typescript: true,
            go: true,
            json: true,
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
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            font_size: 15.0,
            accent: "cyan".into(),
            language: "zh-CN".into(),
            close_to_tray: true,
            ai_language: None,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// shell id（None / "auto" = 自动探测；其余值经 resolve_shell 按本机可用性解析）
    pub selection: Option<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        ShellConfig { selection: None }
    }
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
        });
        let m = cfg.find_model("m1").unwrap();
        assert_eq!(m.model, "model-x");
        assert_eq!(m.name, "model-x");
        assert_eq!(m.api_format, ApiFormat::AnthropicMessages);
        assert_eq!(m.provider_id, "p1");
        assert_eq!(m.keyring_accounts, vec!["p1".to_string()]);
        assert_eq!(m.reasoning_effort.as_deref(), Some("high"));
        assert!(cfg.find_model("nope").is_none());
        assert!(cfg.active_model().is_none());
    }
}
