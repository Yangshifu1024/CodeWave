//! MCP 配置：形状定稿、两层作用域装载、逐条目容错、未知键透传、来源诊断。
//!
//! # 形状（生态通用，`transport` 可推导）
//!
//! ```json
//! { "mcpServers": {
//!     "fs":     { "command": "npx", "args": ["-y", "pkg", "D:\proj"],
//!                 "env": {"K":"V"}, "cwd": "D:\proj", "enabled": true,
//!                 "timeout_ms": 120000, "read_only": false, "always_allow": false,
//!                 "tools": { "mode": "allow", "list": ["read_*"] } },
//!     "remote": { "url": "https://host/mcp",
//!                 "headers": {"Authorization": "Bearer x"}, "read_only": true } } }
//! ```
//!
//! - `transport` 推导：有 `url` → `streamable_http`；有 `command` → `stdio`；
//!   两者并存且未显式声明 → **配置错**（歧义必须暴露，静默选择正是历史 bug 的温床）。
//! - 显式 `"transport": "sse"` → **配置错**，点名改用 streamable_http 端点。
//! - 表单未覆盖的键（`$schema`、自定义键）由 `extra` 捕获，读写不丢。
//!
//! # 作用域
//!
//! 只有两层：全局（`<data_dir>/mcp.json`）与项目（`<project_dir>/mcp.json`，
//! project_dir 即 `<主目录>/.codewave`）。会话可见集 = 两层合并，同名项目级胜出。
//! 工作区级 / extra_roots 的 `<root>/.codewave/mcp.json` 兼容读取已移除。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::coordinator::{AcquireRequest, Lease};
use crate::core::process_coordination::LeaseStore;

use super::error::McpError;

/// 未配置 `timeout_ms` 时的调用超时（沿用现状 120s，不改变既有 server 行为预期）。
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 配置作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// 全局（用户级）：`<data_dir>/mcp.json`
    Global,
    /// 项目：`<project_dir>/mcp.json`
    Project,
}

impl Scope {
    /// 线缆用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
        }
    }

    /// 从线缆字符串解析（IPC 入参）。
    pub fn parse(s: &str) -> Result<Self, McpError> {
        match s {
            "global" => Ok(Scope::Global),
            "project" => Ok(Scope::Project),
            other => Err(McpError::config(format!(
                "未知配置作用域：{other}（应为 global 或 project）"
            ))),
        }
    }
}

/// 传输形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    /// 本地子进程（stdio 管道）
    Stdio,
    /// HTTP 流式端点
    StreamableHttp,
}

/// `transport` 字段的原始形态：认得的取值归一化，认不得的原样保留。
///
/// 用 untagged 是为了让 `"transport": "sse"` 能被**读进来**再报「不支持旧式 SSE」
/// 的定向错误，而不是让整个条目解析失败、退化成一条语焉不详的 parse 错。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransportSpec {
    /// 已知传输
    Known(McpTransport),
    /// 其它取值（如 `"sse"`）：原样保留，由 [`McpServerConfig::resolve_transport`] 报错
    Other(String),
}

/// 工具过滤模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolFilterMode {
    /// 全部注入（默认）
    #[default]
    All,
    /// 仅白名单
    Allow,
    /// 排除黑名单
    Deny,
}

/// 单 server 的工具过滤（精确匹配 + `*` 通配，不支持正则）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolFilter {
    /// 过滤模式
    #[serde(default)]
    pub mode: ToolFilterMode,
    /// 名单（`mode` 为 `all` 时忽略）
    #[serde(default)]
    pub list: Vec<String>,
}

impl ToolFilter {
    /// 是否放行某工具。
    pub fn allows(&self, tool: &str) -> bool {
        match self.mode {
            ToolFilterMode::All => true,
            ToolFilterMode::Allow => self.list.iter().any(|p| wildcard_match(p, tool)),
            ToolFilterMode::Deny => !self.list.iter().any(|p| wildcard_match(p, tool)),
        }
    }

    /// 是否等于默认值（`mode: all` 且名单为空）——序列化时可省。
    pub fn is_default(&self) -> bool {
        self.mode == ToolFilterMode::All && self.list.is_empty()
    }
}

/// `*` 通配匹配（`*` 匹配任意长度含空；其余字符字面量比较）。
fn wildcard_match(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if i == last {
            return rest.len() >= part.len() && rest.ends_with(part);
        } else if part.is_empty() {
            continue;
        } else if let Some(pos) = rest.find(part) {
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// 单个 MCP server 的连接配置（`mcpServers` 条目的值）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// 传输形态（可省：由 command/url 推导；`"sse"` 等未知值原样保留以便定向报错）
    #[serde(default, alias = "type", skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportSpec>,
    /// stdio 形态的启动命令
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// stdio 形态的命令参数（表格化，含空格参数原样保留）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// stdio 形态的子进程环境变量
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// stdio 形态的子进程工作目录
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// streamable_http 形态的端点 URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// streamable_http 形态的请求头（鉴权等）
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    /// 是否启用（false = 条目不连接，但仍可见可编辑）
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// 调用超时（毫秒）；缺省用 [`DEFAULT_TIMEOUT_MS`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// server 级只读声明：声明后免审批
    #[serde(default, skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// 审批「总是允许」记忆（粒度 = server，持久化在该作用域条目里）
    #[serde(default, skip_serializing_if = "is_false")]
    pub always_allow: bool,
    /// 工具过滤
    #[serde(default, skip_serializing_if = "ToolFilter::is_default")]
    pub tools: ToolFilter,
    /// 表单未覆盖的未知键：**原样保留**（往返无损）
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_true() -> bool {
    true
}
fn is_true(v: &bool) -> bool {
    *v
}
fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for McpServerConfig {
    fn default() -> Self {
        McpServerConfig {
            transport: None,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: None,
            headers: HashMap::new(),
            enabled: true,
            timeout_ms: None,
            read_only: false,
            always_allow: false,
            tools: ToolFilter::default(),
            extra: Map::new(),
        }
    }
}

impl McpServerConfig {
    /// 解析传输形态：显式优先；否则按 `command` / `url` 推导；歧义与不支持取值 → 配置错。
    pub fn resolve_transport(&self) -> Result<McpTransport, McpError> {
        match &self.transport {
            Some(TransportSpec::Known(t)) => Ok(*t),
            Some(TransportSpec::Other(raw)) if raw.eq_ignore_ascii_case("sse") => {
                Err(McpError::config_hint(
                    format!("不支持旧式 HTTP+SSE 传输（\"transport\": \"{raw}\"）"),
                    "请改用 streamable_http 端点：把 url 换成服务端的 /mcp 地址并删除 transport 字段。",
                ))
            }
            Some(TransportSpec::Other(raw)) => Err(McpError::config_hint(
                format!("未知的 transport 取值：\"{raw}\""),
                "可用取值：\"stdio\" / \"streamable_http\"；也可省略该字段，由 command / url 推导。",
            )),
            None => {
                let has_cmd = self
                    .command
                    .as_deref()
                    .is_some_and(|c| !c.trim().is_empty());
                let has_url = self.url.as_deref().is_some_and(|u| !u.trim().is_empty());
                match (has_cmd, has_url) {
                    (true, false) => Ok(McpTransport::Stdio),
                    (false, true) => Ok(McpTransport::StreamableHttp),
                    (true, true) => Err(McpError::config_hint(
                        "command 与 url 同时存在，无法推导 transport",
                        "请删除其一，或显式声明 \"transport\": \"stdio\" | \"streamable_http\"。",
                    )),
                    (false, false) => Err(McpError::config_hint(
                        "既没有 command 也没有 url",
                        "stdio 形态需要 command；streamable_http 形态需要 url。",
                    )),
                }
            }
        }
    }

    /// 传输是否为文件里显式声明的（供前端标注「文件内显式声明」）。
    pub fn transport_is_explicit(&self) -> bool {
        matches!(self.transport, Some(TransportSpec::Known(_)))
    }

    /// 调用超时。
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }

    /// 结构校验（不含跨条目撞名检查，那需要全部名字，见 [`load_scope`]）。
    pub fn validate(&self, name: &str) -> Vec<ConfigIssue> {
        let mut out = Vec::new();
        if name.trim().is_empty() {
            out.push(ConfigIssue::error(
                Some(name.to_string()),
                "name",
                "server 名不能为空".to_string(),
                Some("请为该条目起一个名字（字母、数字、-、_ 最稳妥）。".to_string()),
            ));
            return out;
        }
        if self.timeout_ms == Some(0) {
            out.push(ConfigIssue::error(
                Some(name.into()),
                "schema",
                "timeout_ms must be positive".into(),
                None,
            ));
        }
        if self
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        {
            out.push(ConfigIssue::error(
                Some(name.into()),
                "schema",
                "env contains an invalid key or NUL byte".into(),
                None,
            ));
        }
        if self.headers.iter().any(|(key, value)| {
            reqwest::header::HeaderName::from_bytes(key.as_bytes()).is_err()
                || reqwest::header::HeaderValue::from_str(value).is_err()
        }) {
            out.push(ConfigIssue::error(
                Some(name.into()),
                "schema",
                "headers contain an invalid name or value".into(),
                None,
            ));
        }
        match self.resolve_transport() {
            Ok(McpTransport::Stdio) => {
                if self.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    out.push(ConfigIssue::error(
                        Some(name.to_string()),
                        "schema",
                        "stdio 形态缺少 command".to_string(),
                        Some("请填写启动命令，例如 npx / node / 可执行文件绝对路径。".to_string()),
                    ));
                }
            }
            Ok(McpTransport::StreamableHttp) => {
                let url = self.url.as_deref().unwrap_or_default().trim();
                if url.is_empty() {
                    out.push(ConfigIssue::error(
                        Some(name.to_string()),
                        "schema",
                        "streamable_http 形态缺少 url".to_string(),
                        Some("请填写服务端的 /mcp 端点地址。".to_string()),
                    ));
                } else if !(url.starts_with("http://") || url.starts_with("https://")) {
                    out.push(ConfigIssue::error(
                        Some(name.to_string()),
                        "schema",
                        "url 必须以 http:// 或 https:// 开头".to_string(),
                        None,
                    ));
                }
            }
            Err(e) => {
                let kind = if e.message.contains("SSE") {
                    "unsupported"
                } else {
                    "ambiguous"
                };
                out.push(ConfigIssue {
                    server: Some(name.to_string()),
                    level: IssueLevel::Error,
                    kind: kind.to_string(),
                    message: e.message,
                    hint: e.hint,
                });
            }
        }
        out
    }
}

/// 问题级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLevel {
    /// 阻断该条目生效（也阻断保存）
    Error,
    /// 提示性（不阻断）
    Warning,
}

/// 一条配置问题（解析 / 校验），逐条展示给用户，不静默吞掉。
#[derive(Debug, Clone, Serialize)]
pub struct ConfigIssue {
    /// 关联的 server 名（文件级问题为 None）
    pub server: Option<String>,
    /// 级别
    pub level: IssueLevel,
    /// 类别：`parse` | `schema` | `name` | `ambiguous` | `unsupported`
    pub kind: String,
    /// 问题正文
    pub message: String,
    /// 可操作建议
    pub hint: Option<String>,
}

impl ConfigIssue {
    /// 错误级问题。
    pub fn error(
        server: Option<String>,
        kind: &str,
        message: String,
        hint: Option<String>,
    ) -> Self {
        ConfigIssue {
            server,
            level: IssueLevel::Error,
            kind: kind.to_string(),
            message,
            hint,
        }
    }

    /// 告警级问题。
    pub fn warning(
        server: Option<String>,
        kind: &str,
        message: String,
        hint: Option<String>,
    ) -> Self {
        ConfigIssue {
            server,
            level: IssueLevel::Warning,
            kind: kind.to_string(),
            message,
            hint,
        }
    }
}

/// 作用域 + 该作用域 mcp.json 的绝对路径。
#[derive(Debug, Clone)]
pub struct ScopeRef {
    /// 作用域
    pub scope: Scope,
    /// mcp.json 路径
    pub path: PathBuf,
}

/// 全局作用域（用户级）。
pub fn global_scope(data_dir: &Path) -> ScopeRef {
    ScopeRef {
        scope: Scope::Global,
        path: data_dir.join("mcp.json"),
    }
}

/// 项目作用域（`project_dir` 即 `<主目录>/.codewave`）。
pub fn project_scope(project_dir: &Path) -> ScopeRef {
    ScopeRef {
        scope: Scope::Project,
        path: project_dir.join("mcp.json"),
    }
}

/// 文件不存在时使用的空文档原文。
pub fn default_doc_text() -> String {
    "{\n  \"mcpServers\": {}\n}".to_string()
}

/// 单层装载结果。
#[derive(Debug, Clone)]
pub struct ScopeDoc {
    /// 作用域
    pub scope: Scope,
    /// 文件路径
    pub path: PathBuf,
    /// 文件原文（不存在 → 空文档）
    pub raw: String,
    /// 解析出的条目（按名排序）
    pub servers: Vec<(String, McpServerConfig)>,
    /// 该层的问题
    pub issues: Vec<ConfigIssue>,
}

/// 装载一层配置。
///
/// **逐条目容错**：单个条目反序列化失败只产出一条 `parse` 级问题并跳过该条目，
/// 同文件其它条目照常生效（旧实现用 `serde_json::from_str::<McpConfigFile>` 一把梭，
/// 一条坏数据会让整份文件的 server 全部静默消失）。
pub fn load_scope(sref: &ScopeRef) -> ScopeDoc {
    match read_scope_text(sref) {
        Ok(raw) => parse_scope_text(sref, &raw),
        Err(e) => ScopeDoc {
            scope: sref.scope,
            path: sref.path.clone(),
            raw: String::new(),
            servers: vec![],
            issues: vec![ConfigIssue::error(None, "read", e.message, None)],
        },
    }
}

/// The only missing-file fallback. Permission/read failures must never become empty config.
pub fn read_scope_text(sref: &ScopeRef) -> Result<String, McpError> {
    match std::fs::read_to_string(&sref.path) {
        Ok(raw) => Ok(raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_doc_text()),
        Err(_) => Err(McpError::config("MCP configuration could not be read")),
    }
}

/// Pure parser shared by loading, commits and draft probes. Never writes credentials to disk.
pub fn parse_scope_text(sref: &ScopeRef, raw: &str) -> ScopeDoc {
    let raw = raw.to_string();
    let mut doc = ScopeDoc {
        scope: sref.scope,
        path: sref.path.clone(),
        raw: raw.clone(),
        servers: Vec::new(),
        issues: Vec::new(),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            doc.issues.push(ConfigIssue::error(
                None,
                "parse",
                format!(
                    "mcp.json 不是合法 JSON（line {}, column {}）",
                    e.line(),
                    e.column()
                ),
                Some("修正 JSON 语法后重试；该文件当前整体不生效。".to_string()),
            ));
            return doc;
        }
    };
    let Some(root) = parsed.as_object() else {
        doc.issues.push(ConfigIssue::error(
            None,
            "parse",
            "mcp.json 顶层必须是对象".to_string(),
            Some("期望形状：{ \"mcpServers\": { ... } }".to_string()),
        ));
        return doc;
    };
    let Some(servers_val) = root.get("mcpServers") else {
        // 没有 mcpServers 段 = 空配置，不算错
        return doc;
    };
    let Some(servers) = servers_val.as_object() else {
        doc.issues.push(ConfigIssue::error(
            None,
            "parse",
            "mcpServers 必须是对象".to_string(),
            Some("期望形状：{ \"mcpServers\": { \"名字\": { ... } } }".to_string()),
        ));
        return doc;
    };

    for (name, value) in servers {
        match serde_json::from_value::<McpServerConfig>(value.clone()) {
            Ok(cfg) => doc.servers.push((name.clone(), cfg)),
            Err(_) => doc.issues.push(ConfigIssue::error(
                Some(name.clone()),
                "parse",
                "条目解析失败：字段类型无效".to_string(),
                Some(
                    "检查字段类型：args 必须是字符串数组、env / headers 必须是字符串映射。"
                        .to_string(),
                ),
            )),
        }
    }

    // 逐条结构校验
    let mut all_issues = Vec::new();
    for (name, cfg) in &doc.servers {
        all_issues.extend(cfg.validate(name));
    }
    doc.issues.extend(all_issues);

    // 跨条目撞名：不同 server 名归一化后落到同一函数名前缀 → 会加 __N 后缀去重
    let mut by_component: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, _) in &doc.servers {
        by_component
            .entry(super::tools::sanitize_component(name))
            .or_default()
            .push(name.clone());
    }
    for (component, names) in by_component {
        if names.len() > 1 {
            let mut sorted = names;
            sorted.sort();
            doc.issues.push(ConfigIssue::warning(
                Some(sorted[0].clone()),
                "name",
                format!(
                    "这些 server 名归一化后相同（都映射到 `{component}`）：{}",
                    sorted.join("、")
                ),
                Some(
                    "模型可见工具名会自动加 `__2`、`__3` 后缀去重；建议改名以免混淆。".to_string(),
                ),
            ));
        }
    }

    doc.servers.sort_by(|a, b| a.0.cmp(&b.0));
    doc.issues.sort_by(|a, b| {
        (a.server.clone().unwrap_or_default(), a.kind.clone())
            .cmp(&(b.server.clone().unwrap_or_default(), b.kind.clone()))
    });
    doc
}

/// 合并后的条目：带生效来源与被覆盖信息。
#[derive(Debug, Clone)]
pub struct MergedServer {
    /// server 名
    pub name: String,
    /// 生效配置
    pub cfg: McpServerConfig,
    /// 生效来源
    pub source: Scope,
    /// 被它覆盖掉的下层（无则 None）
    pub overridden: Option<Scope>,
    /// 与该条目相关的问题
    pub issues: Vec<ConfigIssue>,
}

/// 两层合并：同名项目级胜出。
pub fn merge_scopes(global: &ScopeDoc, project: Option<&ScopeDoc>) -> Vec<MergedServer> {
    let mut map: BTreeMap<String, MergedServer> = BTreeMap::new();
    let take_issues = |doc: &ScopeDoc, name: &str| -> Vec<ConfigIssue> {
        doc.issues
            .iter()
            .filter(|i| i.server.as_deref() == Some(name))
            .cloned()
            .collect()
    };
    for (name, cfg) in &global.servers {
        map.insert(
            name.clone(),
            MergedServer {
                name: name.clone(),
                cfg: cfg.clone(),
                source: Scope::Global,
                overridden: None,
                issues: take_issues(global, name),
            },
        );
    }
    if let Some(p) = project {
        for (name, cfg) in &p.servers {
            let overridden = map.get(name).map(|m| m.source);
            map.insert(
                name.clone(),
                MergedServer {
                    name: name.clone(),
                    cfg: cfg.clone(),
                    source: Scope::Project,
                    overridden,
                    issues: take_issues(p, name),
                },
            );
        }
    }
    map.into_values().collect()
}

/// Serialized with full scope saves and approval policy patches.
pub static CONFIG_COMMIT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// #36-C0：MCP 配置的**跨进程**提交守卫
// ---------------------------------------------------------------------------

/// 争用时的重试次数与间隔。租约只在真正落盘的那几毫秒里被持有，
/// 所以短暂等待远好于直接失败；但**必须有上限**——另一个实例崩溃后留下的保守 busy
/// 租约（见 `process_coordination` 模块文档第 2 条：绝不按 PID 自动回收）
/// 绝不能把本进程拖成一次看起来像卡死的长等待。
const COMMIT_RETRY: u32 = 6;
const COMMIT_RETRY_DELAY_MS: u64 = 40;

/// 租约的 `session_id`：MCP 配置提交不是某次会话的事，用固定标识占位。
const SCOPE_HOLDER: &str = "mcp-config";

/// 跨进程提交守卫：进程内互斥 + 落盘租约。
///
/// [`CONFIG_COMMIT`] 是进程内互斥，它只挡得住**本实例**的并发。两个 CodeWave 实例
/// 同时改同一份 `mcp.json` 时它完全不起作用：两边各自读一份快照，后写者会拿自己的
/// 旧快照**整份覆盖**，另一端刚做的改动当场消失且不留痕迹。守卫在进程内互斥之外
/// 再加一把落盘租约，由 `create_new`（O_CREAT|O_EXCL）在跨进程下唯一裁决。
///
/// ## 分域：按作用域各管各的
///
/// 协调根取 `mcp.json` **所在目录**下的 `coord/`，而不是全局数据目录：
/// 全局配置的父目录就是全局数据目录（与 [`crate::core::coordinator`] 的数据根一致），
/// 项目配置的父目录就是项目托管目录。写同一个文件的两边必然算出同一个协调根，
/// 而**不同文件之间互不阻塞**——全局配置被占用不该卡住项目配置的保存。
/// 代价是 [`crate::mcp::McpManager`] 这类自身不持有 data_dir 的组件无需为守卫穿线。
///
/// ## 为什么必须持有进程内互斥
///
/// 它顺带杜绝了重入事故：若租约可在同一进程内被两把守卫同时持有，
/// 内层守卫 `Drop` 时会把外层仍在用的租约释放掉。临界区全程由同一把
/// `CONFIG_COMMIT` 串行化，这个前提就成立。
pub struct ScopeCommitGuard {
    /// 进程内互斥。持有一它，上面的重入前提才成立。
    _inproc: tokio::sync::MutexGuard<'static, ()>,
    store: LeaseStore,
    lease: Lease,
}

impl ScopeCommitGuard {
    /// 取得进程内互斥**并**申请跨进程租约（全新临界区用这个入口）。
    pub async fn acquire(sref: &ScopeRef) -> Result<Self, McpError> {
        Self::extend(CONFIG_COMMIT.lock().await, sref).await
    }

    /// 已在 [`CONFIG_COMMIT`] 临界区内时，只补跨进程租约。
    ///
    /// 刻意单列这个入口：tokio 的互斥量**非重入**，在已持锁处再 `lock().await`
    /// 就是当场自死锁。调用方传入它自己刚拿到的 guard，本函数绝不再加锁。
    pub async fn extend(
        _inproc: tokio::sync::MutexGuard<'static, ()>,
        sref: &ScopeRef,
    ) -> Result<Self, McpError> {
        let store = LeaseStore::new(&coord_root_of(sref));
        let req = AcquireRequest {
            session_id: SCOPE_HOLDER.to_string(),
            // 身份唯一来源是「本进程」：同进程重复申请靠这三个字段的相等走幂等分支，
            // 别的进程则必然拿到 `resource_held`。
            root_run_id: format!("pid-{}", std::process::id()),
            resource: format!("mcp:{}", source_identity(&sref.path).to_string_lossy()),
            attempt: 0,
            expected_generation: None,
        };
        let mut lease = None;
        for i in 0..COMMIT_RETRY {
            match store.acquire(&req) {
                Ok(l) => {
                    lease = Some(l);
                    break;
                }
                // 只有"别人正占着"才重试：其余失败（磁盘错误、代次失效）重试没有意义。
                Err(e) if e.code == "resource_held" && i + 1 < COMMIT_RETRY => {
                    tokio::time::sleep(Duration::from_millis(COMMIT_RETRY_DELAY_MS)).await;
                }
                Err(e) => {
                    return Err(McpError::config(format!(
                        "Another instance is modifying this MCP configuration; retry later ({}) : {e}",
                        e.code
                    )));
                }
            }
        }
        let Some(lease) = lease else {
            return Err(McpError::config(format!(
                "Another instance is modifying this MCP configuration; retry later (after {COMMIT_RETRY} attempts)"
            )));
        };
        Ok(ScopeCommitGuard {
            _inproc,
            store,
            lease,
        })
    }

    pub fn lease(&self) -> &Lease {
        &self.lease
    }
}

impl Drop for ScopeCommitGuard {
    fn drop(&mut self) {
        // 尽力释放：失败不 panic（进程正在被杀、磁盘错误）。残留租约按保守规则继续占用，
        // 由 guardian 显式回收——这正是「崩溃保守 busy」要的行为。
        if let Err(e) = self.store.release(&self.lease)
            && e.code != "no_lease"
        {
            tracing::warn!(
                "MCP 配置租约释放失败（{}，generation {}）：{e}；按保守规则继续占用，由 guardian 处理",
                self.lease.resource,
                self.lease.generation
            );
        }
    }
}

/// 协调根：`mcp.json` 所在目录下的 `coord/`。
///
/// 拿不到非空父目录时**不**退回 CWD 下的相对路径——那会让协调租约散落到
/// 当前工作目录里，而它本该待在配置旁边。调用方据此把「无有效共享文件」显式判掉。
fn coord_root_of(sref: &ScopeRef) -> PathBuf {
    sref.path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("coord")
}

/// Opaque content token. No configuration or credential values are exposed in receipts.
pub fn scope_version(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(raw.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Resolve existing parent directories too, so creating mcp.json does not change identity.
pub fn source_identity(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        return source_identity(parent).join(name);
    }
    path.to_path_buf()
}

pub fn validate_server(name: &str, value: Value) -> Result<McpServerConfig, McpError> {
    let cfg: McpServerConfig = serde_json::from_value(value)
        .map_err(|_| McpError::config("MCP draft has invalid field types"))?;
    let errors: Vec<_> = cfg
        .validate(name)
        .into_iter()
        .filter(|i| i.level == IssueLevel::Error)
        .collect();
    if !errors.is_empty() || cfg.timeout_ms == Some(0) {
        return Err(McpError::config(
            "MCP draft configuration is invalid; check name, transport, endpoint and timeout",
        ));
    }
    Ok(cfg)
}

/// `always_allow` 在文件里可能的拼写（写回时沿用已有拼写，避免同义键并存）。
const ALWAYS_ALLOW_KEYS: [&str; 3] = ["always_allow", "alwaysAllow", "alwaysAllowServer"];

/// 定向写回某条目的 `always_allow`。
///
/// 走「读原文 JSON → 只改该条目的该键 → 原子写」，**不整份重建**，
/// 因此 `$schema`、自定义键、顶层其它键、条目内其它键一律不动。
///
/// **必须**传 [`ScopeCommitGuard`]：本函数是「读 → 改 → 写」的读改写，
/// 漏了跨进程租约就等于把另一个实例刚写入的配置整份覆盖掉。
/// 参数类型强制这一点——不靠注释提醒。
pub(crate) fn set_always_allow(
    sref: &ScopeRef,
    name: &str,
    value: bool,
    expected: &McpServerConfig,
    _guard: &ScopeCommitGuard,
) -> Result<(), McpError> {
    let raw = read_scope_text(sref)?;
    let mut doc: Value = serde_json::from_str(&raw).map_err(|e| {
        McpError::config_hint(
            format!("{} 不是合法 JSON：{e}", sref.path.display()),
            "请先修正 JSON 语法，再重试「总是允许」。",
        )
    })?;
    let servers = doc
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| {
            McpError::config(format!(
                "{} 缺少 mcpServers 对象，无法写回",
                sref.path.display()
            ))
        })?;
    let entry = servers
        .get_mut(name)
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| McpError::config(format!("配置里没有名为 {name} 的 server")))?;
    let current: McpServerConfig =
        serde_json::from_value(Value::Object(entry.clone())).map_err(|_| {
            McpError::config("MCP configuration changed before approval could be saved")
        })?;
    if &current != expected {
        return Err(McpError::config(
            "MCP configuration changed before approval could be saved",
        ));
    }
    let key = ALWAYS_ALLOW_KEYS
        .iter()
        .find(|k| entry.contains_key(**k))
        .copied()
        .unwrap_or("always_allow");
    entry.insert(key.to_string(), Value::Bool(value));
    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| McpError::config(format!("序列化失败：{e}")))?;
    if let Some(parent) = sref.path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| McpError::config(format!("创建目录失败 {}：{e}", parent.display())))?;
    }
    crate::util::atomic::atomic_write(&sref.path, text.as_bytes())
        .map_err(|e| McpError::config(format!("写入失败 {}：{e}", sref.path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg_of(v: Value) -> McpServerConfig {
        serde_json::from_value(v).expect("应能解析")
    }

    fn write(dir: &Path, text: &str) -> ScopeRef {
        let path = dir.join("mcp.json");
        std::fs::write(&path, text).unwrap();
        ScopeRef {
            scope: Scope::Global,
            path,
        }
    }

    #[test]
    fn transport_inferred_from_command_or_url() {
        let stdio = cfg_of(json!({ "command": "npx", "args": ["-y", "p"] }));
        assert_eq!(stdio.resolve_transport().unwrap(), McpTransport::Stdio);
        assert!(!stdio.transport_is_explicit());

        let http = cfg_of(json!({ "url": "https://h/mcp" }));
        assert_eq!(
            http.resolve_transport().unwrap(),
            McpTransport::StreamableHttp
        );
        assert!(!http.transport_is_explicit());

        // 显式声明优先（即便同时给了 command）
        let explicit = cfg_of(json!({ "transport": "stdio", "command": "x", "url": "https://h" }));
        assert_eq!(explicit.resolve_transport().unwrap(), McpTransport::Stdio);
        assert!(explicit.transport_is_explicit());

        // 生态里 `type` 也常作 transport 的别名
        let aliased = cfg_of(json!({ "type": "streamable_http", "url": "https://h" }));
        assert_eq!(
            aliased.resolve_transport().unwrap(),
            McpTransport::StreamableHttp
        );
    }

    #[test]
    fn ambiguous_command_and_url_is_config_error_not_silent_choice() {
        let c = cfg_of(json!({ "command": "npx", "url": "https://h/mcp" }));
        let e = c.resolve_transport().unwrap_err();
        assert!(e.message.contains("同时存在"));
        assert!(e.hint.as_deref().unwrap().contains("transport"));
        assert!(c.validate("bad").iter().any(|i| i.kind == "ambiguous"));
    }

    #[test]
    fn neither_command_nor_url_is_error() {
        let c = cfg_of(json!({ "enabled": false }));
        assert!(
            c.resolve_transport()
                .unwrap_err()
                .message
                .contains("既没有")
        );
    }

    #[test]
    fn legacy_sse_is_rejected_with_actionable_hint() {
        let c = cfg_of(json!({ "transport": "sse", "url": "https://h/sse" }));
        let e = c.resolve_transport().unwrap_err();
        assert!(e.message.contains("SSE"));
        assert!(e.hint.as_deref().unwrap().contains("streamable_http"));
        // 报的是 unsupported 而不是含糊的 parse 错（条目本身能被读进来）
        let issues = c.validate("legacy-sse");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "unsupported");
        assert_eq!(issues[0].level, IssueLevel::Error);
    }

    #[test]
    fn unknown_transport_value_is_preserved_and_reported() {
        let c = cfg_of(json!({ "transport": "websocket", "url": "https://h" }));
        assert!(
            c.resolve_transport()
                .unwrap_err()
                .message
                .contains("websocket")
        );
        // 原样保留：序列化回去还是 websocket，不被静默改写成 stdio
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!(back["transport"], "websocket");
    }

    #[test]
    fn one_bad_entry_does_not_kill_the_whole_file() {
        let dd = tempfile::tempdir().unwrap();
        // 第二条 args 类型非法（应为字符串数组）→ 只有它失效
        let sref = write(
            dd.path(),
            r#"{"mcpServers":{
                "good":{"command":"npx","args":["-y","p"]},
                "bad":{"command":"x","args":"not-an-array"},
                "alsoGood":{"url":"https://h/mcp"}}}"#,
        );
        let doc = load_scope(&sref);
        let names: Vec<&str> = doc.servers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["alsoGood", "good"], "好条目必须照常生效");
        let bad: Vec<&ConfigIssue> = doc
            .issues
            .iter()
            .filter(|i| i.server.as_deref() == Some("bad"))
            .collect();
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].kind, "parse");
        assert_eq!(bad[0].level, IssueLevel::Error);
    }

    #[test]
    fn unknown_keys_round_trip() {
        let c = cfg_of(json!({
            "$schema": "https://x/schema.json",
            "command": "npx",
            "experimental": { "nested": [1, 2] },
            "read_only": true
        }));
        assert!(c.extra.contains_key("$schema"));
        assert!(c.extra.contains_key("experimental"));
        let back = serde_json::to_value(&c).unwrap();
        assert_eq!(back["$schema"], "https://x/schema.json");
        assert_eq!(back["experimental"]["nested"], json!([1, 2]));
        assert_eq!(back["read_only"], true);
    }

    #[test]
    fn enabled_defaults_true_and_false_round_trips() {
        let c = cfg_of(json!({ "command": "x" }));
        assert!(c.enabled);
        let off = cfg_of(json!({ "command": "x", "enabled": false }));
        assert!(!off.enabled);
        let back = serde_json::to_value(&off).unwrap();
        assert_eq!(back["enabled"], false);
        // 默认 true 时省略该键，保持文件干净
        let on_back = serde_json::to_value(&c).unwrap();
        assert!(on_back.get("enabled").is_none());
    }

    #[test]
    fn timeout_defaults_and_overrides() {
        let c = cfg_of(json!({ "command": "x" }));
        assert_eq!(c.timeout(), Duration::from_millis(DEFAULT_TIMEOUT_MS));
        let c = cfg_of(json!({ "command": "x", "timeout_ms": 5000 }));
        assert_eq!(c.timeout(), Duration::from_millis(5000));
    }

    #[test]
    fn scope_parse_round_trip() {
        assert_eq!(Scope::parse("global").unwrap(), Scope::Global);
        assert_eq!(Scope::parse("project").unwrap(), Scope::Project);
        assert!(Scope::parse("workspace").is_err());
        assert!(Scope::parse("session").is_err());
        assert!(serde_json::from_str::<Scope>(r#""session""#).is_err());
        assert_eq!(Scope::Global.as_str(), "global");
        assert_eq!(Scope::Project.as_str(), "project");
    }

    #[test]
    fn empty_name_is_error() {
        let c = cfg_of(json!({ "command": "x" }));
        let issues = c.validate("  ");
        assert_eq!(issues[0].kind, "name");
        assert_eq!(issues[0].level, IssueLevel::Error);
    }

    #[test]
    fn missing_command_or_bad_url_is_schema_error() {
        // 显式 stdio 但没 command
        let c = cfg_of(json!({ "transport": "stdio", "command": "" }));
        assert!(c.validate("s").iter().any(|i| i.kind == "schema"));
        // 显式 http 但 url 不是 http(s)
        let c = cfg_of(json!({ "transport": "streamable_http", "url": "ftp://h" }));
        let issues = c.validate("h");
        assert_eq!(issues[0].kind, "schema");
        assert!(issues[0].message.contains("http://"));
    }

    #[tokio::test]
    async fn set_always_allow_preserves_other_keys_and_reuses_existing_spelling() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(
            dd.path(),
            r#"{"$schema":"https://x/s","mcpServers":{
                "fs":{"command":"npx","alwaysAllow":false,"custom":"keep-me"},
                "other":{"url":"https://h/mcp"}}}"#,
        );
        let expected = load_scope(&sref)
            .servers
            .into_iter()
            .find(|(name, _)| name == "fs")
            .unwrap()
            .1;
        let guard = ScopeCommitGuard::acquire(&sref).await.unwrap();
        set_always_allow(&sref, "fs", true, &expected, &guard).unwrap();
        let text = std::fs::read_to_string(&sref.path).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        // 沿用文件里已有的 camelCase 拼写，不新增同义键
        assert_eq!(v["mcpServers"]["fs"]["alwaysAllow"], true);
        assert!(v["mcpServers"]["fs"].get("always_allow").is_none());
        // 其它一切不动
        assert_eq!(v["$schema"], "https://x/s");
        assert_eq!(v["mcpServers"]["fs"]["custom"], "keep-me");
        assert_eq!(v["mcpServers"]["other"]["url"], "https://h/mcp");
    }

    #[tokio::test]
    async fn set_always_allow_errors_when_entry_missing() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(dd.path(), r#"{"mcpServers":{"fs":{"command":"npx"}}}"#);
        let guard = ScopeCommitGuard::acquire(&sref).await.unwrap();
        let e = set_always_allow(
            &sref,
            "nope",
            true,
            &cfg_of(json!({"command": "npx"})),
            &guard,
        )
        .unwrap_err();
        assert!(e.message.contains("nope"));
    }

    /// #36-C0：他人持有租约时**必须**取不到守卫——这正是「后写者覆盖前写者」的防线。
    #[tokio::test]
    async fn 他人持有配置租约时_提交守卫必须被拒() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(dd.path(), r#"{"mcpServers":{"fs":{"command":"npx"}}}"#);
        // 以「另一个进程」的身份占住同一份配置的租约。
        // root_run_id 用一个不可能是本进程 pid 的字面量，避免与幂等分支混淆。
        let store = LeaseStore::new(&coord_root_of(&sref));
        store
            .acquire(&AcquireRequest {
                session_id: SCOPE_HOLDER.to_string(),
                root_run_id: "other-process".to_string(),
                resource: format!("mcp:{}", source_identity(&sref.path).to_string_lossy()),
                attempt: 0,
                expected_generation: None,
            })
            .expect("模拟他人占用");

        let result = ScopeCommitGuard::acquire(&sref).await;
        let msg = match result {
            Err(e) => e.message,
            // 拿到就等于测试环境没起作用，不许当成通过。
            Ok(_) => panic!("他人持有租约时不得取得提交守卫"),
        };
        assert!(
            msg.contains("Another instance"),
            "错误文案要说明是另一个实例在改：{msg}"
        );
    }

    /// #36-C0：守卫在临界区内**真的占着租约**，离开临界区后**真的还掉了**。
    #[tokio::test]
    async fn 提交守卫在临界区内持有租约_离开后释放() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(dd.path(), r#"{"mcpServers":{"fs":{"command":"npx"}}}"#);
        let store = LeaseStore::new(&coord_root_of(&sref));
        let resource = format!("mcp:{}", source_identity(&sref.path).to_string_lossy());
        assert!(
            store.read(&resource).is_none(),
            "进入临界区前不应有残留租约"
        );
        {
            let _guard = ScopeCommitGuard::acquire(&sref).await.unwrap();
            let rec = store.read(&resource).expect("临界区内必须占着租约");
            assert_eq!(rec.lease.resource, resource);
            assert_eq!(rec.pid, std::process::id());
            // 临界区结束后（作用域结束）租约必须已释放
        }
        assert!(
            store.read(&resource).is_none(),
            "离开临界区后租约必须已释放，否则同一进程再也写不进去"
        );
    }

    #[test]
    fn merge_prefers_project_and_records_override() {
        let dd = tempfile::tempdir().unwrap();
        let pd = tempfile::tempdir().unwrap();
        let g = write(
            dd.path(),
            r#"{"mcpServers":{"fs":{"command":"global-fs"},"only-global":{"command":"g"}}}"#,
        );
        let p = write(
            pd.path(),
            r#"{"mcpServers":{"fs":{"command":"project-fs"},"only-proj":{"command":"p"}}}"#,
        );
        let g = load_scope(&g);
        let p = load_scope(&p);
        let merged = merge_scopes(&g, Some(&p));
        let names: Vec<&str> = merged.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["fs", "only-global", "only-proj"]);
        let fs = merged.iter().find(|m| m.name == "fs").unwrap();
        assert_eq!(fs.source, Scope::Project);
        assert_eq!(fs.overridden, Some(Scope::Global));
        assert_eq!(fs.cfg.command.as_deref(), Some("project-fs"));
        let og = merged.iter().find(|m| m.name == "only-global").unwrap();
        assert_eq!(og.source, Scope::Global);
        assert_eq!(og.overridden, None);
    }

    #[test]
    fn merge_without_project_is_global_only() {
        let dd = tempfile::tempdir().unwrap();
        let g = load_scope(&write(dd.path(), r#"{"mcpServers":{"a":{"command":"x"}}}"#));
        let merged = merge_scopes(&g, None);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, Scope::Global);
    }

    #[test]
    fn tool_filter_wildcards() {
        let all = ToolFilter::default();
        assert!(all.allows("anything"));

        let allow = ToolFilter {
            mode: ToolFilterMode::Allow,
            list: vec!["read_*".into(), "echo".into()],
        };
        assert!(allow.allows("read_file"));
        assert!(allow.allows("echo"));
        assert!(!allow.allows("write_file"));

        let deny = ToolFilter {
            mode: ToolFilterMode::Deny,
            list: vec!["write_*".into(), "*_danger".into()],
        };
        assert!(deny.allows("read_file"));
        assert!(!deny.allows("write_file"));
        assert!(!deny.allows("do_danger"));

        // `*` 在中间、连续 `**`、纯 `*`、无通配
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("a*b*c", "aXXc"));
        assert!(wildcard_match("**", "whatever"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exact2"));
        assert!(wildcard_match("*_file", "read_file"));
        assert!(!wildcard_match("*_file", "read_file_x"));
    }

    #[test]
    fn tool_filter_default_is_omitted_from_serialization() {
        let c = cfg_of(json!({ "command": "x" }));
        let back = serde_json::to_value(&c).unwrap();
        assert!(back.get("tools").is_none());
        let with = cfg_of(json!({ "command": "x", "tools": { "mode": "allow", "list": ["a"] } }));
        assert_eq!(with.tools.mode, ToolFilterMode::Allow);
        assert!(!with.tools.is_default());
        let back = serde_json::to_value(&with).unwrap();
        assert_eq!(back["tools"]["mode"], "allow");
        assert_eq!(back["tools"]["list"], json!(["a"]));
    }

    #[test]
    fn missing_file_yields_empty_doc_without_issues() {
        let dd = tempfile::tempdir().unwrap();
        let sref = ScopeRef {
            scope: Scope::Project,
            path: dd.path().join("nope.json"),
        };
        let doc = load_scope(&sref);
        assert!(doc.servers.is_empty());
        assert!(doc.issues.is_empty(), "文件不存在不算错");
        assert!(doc.raw.contains("mcpServers"));
    }

    #[test]
    fn invalid_json_reports_file_level_issue() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(dd.path(), "{ not json");
        let doc = load_scope(&sref);
        assert!(doc.servers.is_empty());
        assert_eq!(doc.issues.len(), 1);
        assert_eq!(doc.issues[0].server, None);
        assert_eq!(doc.issues[0].kind, "parse");
    }

    #[test]
    fn json_without_mcp_servers_is_empty_not_error() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(dd.path(), r#"{"$schema":"https://x/s"}"#);
        let doc = load_scope(&sref);
        assert!(doc.servers.is_empty());
        assert!(doc.issues.is_empty(), "没有 mcpServers 段不算错");
    }

    #[test]
    fn colliding_sanitized_names_warn() {
        let dd = tempfile::tempdir().unwrap();
        let sref = write(
            dd.path(),
            r#"{"mcpServers":{"PowerShell.MCP":{"command":"a"},"PowerShell_MCP":{"command":"b"}}}"#,
        );
        let doc = load_scope(&sref);
        assert_eq!(doc.servers.len(), 2);
        let warn = doc
            .issues
            .iter()
            .find(|i| i.level == IssueLevel::Warning)
            .expect("应有撞名告警");
        assert_eq!(warn.kind, "name");
        assert!(warn.message.contains("PowerShell_MCP"));
    }

    #[test]
    fn sanitize_allows_arbitrary_server_names() {
        // 刚合并的修复明确允许任意 server 名（`.` / 中文 / 空格）并归一化到函数名，
        // 配置层不得拒绝它们——只对「归一化后撞名」发告警。
        let dd = tempfile::tempdir().unwrap();
        let sref = write(
            dd.path(),
            r#"{"mcpServers":{"PowerShell.MCP":{"command":"a"},"我的 server":{"command":"b"}}}"#,
        );
        let doc = load_scope(&sref);
        assert_eq!(doc.servers.len(), 2);
        assert!(
            doc.issues.iter().all(|i| i.level == IssueLevel::Warning),
            "任意名不得产生错误级问题：{:?}",
            doc.issues
        );
    }
}
