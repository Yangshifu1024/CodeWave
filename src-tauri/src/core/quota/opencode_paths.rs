//! opencode 运行时路径、配置与 auth.json 解析
//! （[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//!
//! 语义与上游 opencode-quota 对齐：配置目录按 `OPENCODE_CONFIG_DIR` → `$XDG_CONFIG_HOME/opencode`
//! → `~/.config/opencode`；数据目录按 `$XDG_DATA_HOME/opencode` → `~/.local/share/opencode`；
//! Windows 额外接受 `%APPDATA%\opencode` 与 `%LOCALAPPDATA%\opencode` 候选。只读，绝不写回。

use serde_json::Value;
use std::path::{Path, PathBuf};

/// 注入的运行时基目录（生产 `from_process()`，测试注入临时目录）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeEnv {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub opencode_config_dir: Option<PathBuf>,
    pub app_data: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl RuntimeEnv {
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        Self {
            home: dirs::home_dir(),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            opencode_config_dir: var("OPENCODE_CONFIG_DIR"),
            app_data: var("APPDATA"),
            local_app_data: var("LOCALAPPDATA"),
        }
    }
}

/// opencode 全局配置候选（`.jsonc` 与 `.json` 都受理，jsonc 在前）。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigCandidate {
    pub path: PathBuf,
    pub is_jsonc: bool,
}

fn dedupe(mut dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    dirs.retain(|d| {
        if seen.contains(d) {
            false
        } else {
            seen.push(d.clone());
            true
        }
    });
    dirs
}

/// 配置目录候选（按优先序去重）。
pub fn config_dirs(env: &RuntimeEnv) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match (&env.opencode_config_dir, &env.xdg_config_home, &env.home) {
        (Some(configured), ..) => dirs.push(configured.clone()),
        (None, Some(xdg), _) => dirs.push(xdg.join("opencode")),
        (None, None, Some(home)) => dirs.push(home.join(".config").join("opencode")),
        (None, None, None) => {}
    }
    if let Some(app_data) = &env.app_data {
        dirs.push(app_data.join("opencode"));
    }
    if let Some(local) = &env.local_app_data {
        dirs.push(local.join("opencode"));
    }
    dedupe(dirs)
}

/// 数据目录候选（auth.json 所在）。
pub fn data_dirs(env: &RuntimeEnv) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match (&env.xdg_data_home, &env.home) {
        (Some(xdg), _) => dirs.push(xdg.join("opencode")),
        (None, Some(home)) => dirs.push(home.join(".local").join("share").join("opencode")),
        (None, None) => {}
    }
    if let Some(app_data) = &env.app_data {
        dirs.push(app_data.join("opencode"));
    }
    if let Some(local) = &env.local_app_data {
        dirs.push(local.join("opencode"));
    }
    dedupe(dirs)
}

/// 全局配置候选文件（每个配置目录先 `.jsonc` 后 `.json`）。
pub fn config_candidates(env: &RuntimeEnv) -> Vec<ConfigCandidate> {
    let mut out = Vec::new();
    for dir in config_dirs(env) {
        out.push(ConfigCandidate {
            path: dir.join("opencode.jsonc"),
            is_jsonc: true,
        });
        out.push(ConfigCandidate {
            path: dir.join("opencode.json"),
            is_jsonc: false,
        });
    }
    out
}

/// auth.json 候选路径。
pub fn auth_candidates(env: &RuntimeEnv) -> Vec<PathBuf> {
    data_dirs(env)
        .into_iter()
        .map(|d| d.join("auth.json"))
        .collect()
}

/// 从配置里取第一个命中的 `provider.<key>.options.apiKey`（原始值，可能含 `${ENV}` 模板）。
pub fn provider_api_key(config: &Value, keys: &[&str]) -> Option<String> {
    let key_of = |key: &str| -> Option<String> {
        let raw = config
            .get("provider")?
            .get(key)?
            .get("options")?
            .get("apiKey")?
            .as_str()?;
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    keys.iter().find_map(|k| key_of(k))
}

/// `${ENV}` 模板展开：仅允许该提供商自己的环境变量名单，未知占位符一律判失败
/// （避免把别人的密钥名解析成空值静默降级）。
pub fn resolve_env_template(
    value: &str,
    allowed: &[&str],
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}')?;
        let name = &after[..end];
        if !allowed.contains(&name) {
            return None;
        }
        out.push_str(lookup(name)?.as_str());
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// JSONC → JSON：剥掉字符串外的 `//`、`/* */` 注释与尾逗号。
pub fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    strip_trailing_commas(&out)
}

/// 去掉 `}` / `]` 前的尾逗号（字符串外）。
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut pending_comma: Option<usize> = None;
    for c in text.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                pending_comma = None;
                out.push(c);
            }
            ',' => {
                pending_comma = Some(out.len());
                out.push(c);
            }
            '}' | ']' => {
                if let Some(pos) = pending_comma.take() {
                    out.remove(pos);
                }
                out.push(c);
            }
            c if c.is_whitespace() => out.push(c),
            _ => {
                pending_comma = None;
                out.push(c);
            }
        }
    }
    out
}

/// 宽松解析：JSONC 容错后走 serde_json。
pub fn parse_jsonc(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(&strip_jsonc(text)).ok()
}

/// auth.json 条目的解析结果。
#[derive(Debug, Clone, PartialEq)]
pub enum AuthEntry {
    /// 没有该提供商的条目
    Missing,
    /// 有条目但形态不可用（oauth / 空 key / 非对象）
    Invalid(String),
    Key(String),
}

/// 取 auth.json 里第一个存在的提供商条目并校验形态（与上游 lenient 语义一致：
/// 首个存在的键胜出，形态异常即 Invalid，不再回退更后的键）。
pub fn auth_entry(auth: &Value, keys: &[&str]) -> AuthEntry {
    let Some(map) = auth.as_object() else {
        return AuthEntry::Invalid("auth.json 根节点不是对象".to_string());
    };
    for key in keys {
        let Some(entry) = map.get(*key) else {
            continue;
        };
        if !entry.is_object() {
            return AuthEntry::Invalid(format!("auth.json 的 {key} 条目格式异常"));
        }
        let kind = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "api" {
            return AuthEntry::Invalid(format!("auth.json 的 {key} 条目类型不支持（{kind}）"));
        }
        let secret = entry
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if secret.is_empty() {
            return AuthEntry::Invalid(format!("auth.json 的 {key} 条目缺少 key"));
        }
        return AuthEntry::Key(secret.to_string());
    }
    AuthEntry::Missing
}

/// 读取文件文本（不存在/无权限返回 None）。
pub fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
