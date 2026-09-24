//! MCP 配置与连接状态命令。
//!
//! 作用域只有两层：全局（用户级 `<data_dir>/mcp.json`）与项目（`<project_dir>/mcp.json`，
//! project_dir 即 `<主目录>/.codewave`）。会话可见集 = 两层合并，同名项目级胜出。
//!
//! 命令只做校验 + 转调 core（分层约定）；状态事件由 [`wire_sink`] 接到 `EventSink`
//! （core 层不能依赖 tauri）。

use super::util::{Core, err};
use crate::core::agent::{AgentCore, SessionRuntime};
use crate::mcp::{
    ConfigIssue, McpServerConfig, McpTransport, MergedServer, PoolKey, Scope, ScopeDoc, ScopeRef,
    global_scope, load_scope, merge_scopes, project_scope,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// 装配状态事件汇（幂等覆盖）。
async fn wire_sink(core: &AgentCore) {
    let sink = core.sink.clone();
    core.mcp
        .set_status_sink(Arc::new(
            move |session: &str, p: &crate::mcp::McpStatusPayload| {
                // 无会话上下文（全停）不发：投递到不存在的 tab 会被前端静默丢弃，
                // 设置页靠 mcp_status / mcp_snapshot 重读。
                if session.is_empty() {
                    return;
                }
                let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.insert("session".to_string(), json!(session));
                }
                let sid = session.to_string();
                sink.emit(&sid, "mcp:status", v);
            },
        ))
        .await;
}

/// 解析作用域到具体文件路径。
fn scope_ref(core: &AgentCore, session_id: Option<&str>, scope: Scope) -> Result<ScopeRef, String> {
    match scope {
        Scope::Global => Ok(global_scope(&core.data_dir)),
        Scope::Project => {
            let sid = session_id.ok_or("项目级 MCP 配置需要 session_id")?;
            let rt = core.session(sid).ok_or("会话不存在")?;
            rt.project_dir
                .as_deref()
                .map(project_scope)
                .ok_or_else(|| "当前会话没有项目目录，无法读写项目级 MCP 配置".to_string())
        }
    }
}

/// 会话可见集：全局 ∪ 项目（同名项目级胜出），剔除 `enabled: false`。
fn visible_servers(rt: &SessionRuntime) -> Vec<(PoolKey, McpServerConfig)> {
    let g = load_scope(&global_scope(&rt.data_dir));
    let p = rt
        .project_dir
        .as_deref()
        .map(|pd| load_scope(&project_scope(pd)));
    merge_scopes(&g, p.as_ref())
        .into_iter()
        .filter(|m| m.cfg.enabled)
        .map(|m| {
            let key = match m.source {
                Scope::Project => match &rt.project_id {
                    Some(id) => PoolKey::project(id.clone(), m.name.clone()),
                    None => PoolKey::global(m.name.clone()),
                },
                Scope::Global => PoolKey::global(m.name.clone()),
            };
            (key, m.cfg)
        })
        .collect()
}

/// 单条 server 的结构化视图（前端表单与状态表共用）。
#[derive(Serialize)]
pub struct McpServerView {
    name: String,
    /// `stdio` / `streamable_http` / `unknown`（无法推导时由 issues 说明原因）
    transport: String,
    /// true = 由 command/url 推导，false = 文件里显式声明
    transport_inferred: bool,
    enabled: bool,
    read_only: bool,
    always_allow: bool,
    command: Option<String>,
    args: Vec<String>,
    env: std::collections::HashMap<String, String>,
    cwd: Option<String>,
    url: Option<String>,
    headers: std::collections::HashMap<String, String>,
    timeout_ms: Option<u64>,
    tools: crate::mcp::ToolFilter,
    /// 表单未展示、保存时会原样保留的键
    extra_keys: Vec<String>,
    source: String,
    overridden: Option<String>,
}

fn view_of(m: &MergedServer) -> McpServerView {
    let (transport, inferred) = match m.cfg.resolve_transport() {
        Ok(McpTransport::Stdio) => ("stdio", !m.cfg.transport_is_explicit()),
        Ok(McpTransport::StreamableHttp) => ("streamable_http", !m.cfg.transport_is_explicit()),
        Err(_) => ("unknown", false),
    };
    McpServerView {
        name: m.name.clone(),
        transport: transport.to_string(),
        transport_inferred: inferred,
        enabled: m.cfg.enabled,
        read_only: m.cfg.read_only,
        always_allow: m.cfg.always_allow,
        command: m.cfg.command.clone(),
        args: m.cfg.args.clone(),
        env: m.cfg.env.clone(),
        cwd: m.cfg.cwd.clone(),
        url: m.cfg.url.clone(),
        headers: m.cfg.headers.clone(),
        timeout_ms: m.cfg.timeout_ms,
        tools: m.cfg.tools.clone(),
        extra_keys: m.cfg.extra.keys().cloned().collect(),
        source: m.source.as_str().to_string(),
        overridden: m.overridden.map(|s| s.as_str().to_string()),
    }
}

/// 一层配置的视图载荷。
#[derive(Serialize)]
pub struct McpConfigDocPayload {
    scope: String,
    path: String,
    /// 该层 mcp.json 原文（前端编辑用；不存在则为空文档）
    json: String,
    /// 该层文件里的条目（用于编辑）
    servers: Vec<McpServerView>,
    /// 合并后的生效条目（带来源 / 被覆盖信息）
    effective: Vec<McpServerView>,
    /// 该层的解析与校验问题
    issues: Vec<ConfigIssue>,
}

fn doc_payload(sref: &ScopeRef, doc: &ScopeDoc, effective: &[MergedServer]) -> McpConfigDocPayload {
    McpConfigDocPayload {
        scope: sref.scope.as_str().to_string(),
        path: sref.path.display().to_string(),
        json: doc.raw.clone(),
        servers: doc
            .servers
            .iter()
            .map(|(name, cfg)| {
                view_of(&MergedServer {
                    name: name.clone(),
                    cfg: cfg.clone(),
                    source: sref.scope,
                    overridden: None,
                    issues: Vec::new(),
                })
            })
            .collect(),
        effective: effective.iter().map(view_of).collect(),
        issues: doc.issues.clone(),
    }
}

/// 合并后的生效条目（全局 ∪ 项目）。
fn effective_servers(core: &AgentCore, session_id: Option<&str>) -> Vec<MergedServer> {
    let g = load_scope(&global_scope(&core.data_dir));
    let p = session_id.and_then(|sid| core.session(sid)).and_then(|rt| {
        rt.project_dir
            .as_deref()
            .map(|pd| load_scope(&project_scope(pd)))
    });
    merge_scopes(&g, p.as_ref())
}

/// 读取某作用域的 MCP 配置：该层原文 + 条目视图 + 生效条目 + 问题。
#[tauri::command]
pub async fn mcp_list_config(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
) -> Result<McpConfigDocPayload, String> {
    let scope = Scope::parse(&scope).map_err(err)?;
    let sref = scope_ref(&core, session_id.as_deref(), scope)?;
    let doc = load_scope(&sref);
    let effective = effective_servers(&core, session_id.as_deref());
    Ok(doc_payload(&sref, &doc, &effective))
}

/// 保存某作用域的 MCP 配置。
///
/// 结构校验走**与运行时同一套规则**（先把文本按正常装载路径解析一遍），
/// 只要存在 error 级问题就**不落盘**并原样返回问题清单（不静默改写用户文件）。
/// 落盘后只重载受影响连接：全停再按各会话的新可见集重连，其它会话不受牵连。
#[tauri::command]
pub async fn mcp_save_config(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
    json: String,
) -> Result<Value, String> {
    let scope = Scope::parse(&scope).map_err(err)?;
    let sref = scope_ref(&core, session_id.as_deref(), scope)?;
    serde_json::from_str::<Value>(&json).map_err(|e| format!("JSON 无效：{e}"))?;

    let tmp = sref.path.with_extension("mcp.validate.json");
    std::fs::write(&tmp, json.as_bytes()).map_err(err)?;
    let probe = load_scope(&ScopeRef {
        scope: sref.scope,
        path: tmp.clone(),
    });
    let _ = std::fs::remove_file(&tmp);

    let blocking = probe
        .issues
        .iter()
        .any(|i| matches!(i.level, crate::mcp::IssueLevel::Error));
    if blocking {
        return Ok(serde_json::json!({ "saved": false, "issues": probe.issues }));
    }
    if let Some(parent) = sref.path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    crate::util::atomic::atomic_write(&sref.path, json.as_bytes()).map_err(err)?;

    core.mcp.disconnect_all().await;
    let http = core.client.read().unwrap().clone();
    for entry in core.sessions.iter() {
        let rt = entry.value().clone();
        core.mcp.warm(&rt.id, visible_servers(&rt), http.clone());
    }
    Ok(serde_json::json!({ "saved": true, "issues": probe.issues }))
}

/// 连接该会话可见集里的 MCP server（并行建连，立即返回；状态走事件）。
///
/// `names` 为 None 时按完整可见集重连；给了名字则只重连这几个。
#[tauri::command]
pub async fn mcp_connect(
    core: Core<'_>,
    session_id: String,
    names: Option<Vec<String>>,
) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    wire_sink(&core).await;
    let http = core.client.read().unwrap().clone();
    let visible = visible_servers(&rt);
    match names {
        None => core.mcp.warm(&rt.id, visible, http),
        Some(names) => {
            for (key, cfg) in visible.into_iter().filter(|(k, _)| names.contains(&k.name)) {
                let mcp = core.mcp.clone();
                let sid = rt.id.clone();
                let http = http.clone();
                tokio::spawn(async move {
                    let _ = mcp.ensure(&key, cfg, http, &sid).await;
                });
            }
        }
    }
    Ok(())
}

/// 断开该会话的 MCP 连接（`names` 为 None 时释放整个可见集）。
#[tauri::command]
pub async fn mcp_disconnect(
    core: Core<'_>,
    session_id: String,
    names: Option<Vec<String>>,
) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    wire_sink(&core).await;
    match names {
        None => core.mcp.release_session(&rt.id).await,
        Some(names) => {
            for key in core.mcp.visible_keys(&rt.id).await {
                if names.contains(&key.name) {
                    core.mcp.disconnect(&key, &rt.id).await;
                }
            }
        }
    }
    Ok(())
}

/// 重连单个 server（含被淘汰的条目：绕过防抖立即重拉）。
#[tauri::command]
pub async fn mcp_reconnect(core: Core<'_>, session_id: String, name: String) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    wire_sink(&core).await;
    let http = core.client.read().unwrap().clone();
    let target = visible_servers(&rt)
        .into_iter()
        .find(|(k, _)| k.name == name)
        .ok_or_else(|| format!("会话可见集里没有名为 {name} 的 server"))?;
    core.mcp
        .ensure(&target.0, target.1, http, &rt.id)
        .await
        .map_err(err)?;
    Ok(())
}

/// 临时测试连接：起 → `tools/list` → 立即回收，**不并入连接池、不改正式状态**。
#[tauri::command]
pub async fn mcp_test(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
    name: String,
) -> Result<Value, String> {
    let scope = Scope::parse(&scope).map_err(err)?;
    let sref = scope_ref(&core, session_id.as_deref(), scope)?;
    let doc = load_scope(&sref);
    let cfg = doc
        .servers
        .iter()
        .find(|(n, _)| n == &name)
        .map(|(_, c)| c.clone())
        .ok_or_else(|| format!("配置里没有名为 {name} 的 server"))?;
    let http = core.client.read().unwrap().clone();
    Ok(
        match crate::mcp::McpManager::test_connection(cfg, http).await {
            Ok(n) => serde_json::json!({ "ok": true, "tools": n, "error": Value::Null }),
            Err(e) => serde_json::json!({ "ok": false, "tools": 0, "error": e }),
        },
    )
}

/// 该会话的 MCP 状态快照（全量，覆盖断开 / 淘汰 / 批量停止等场景）。
#[tauri::command]
pub async fn mcp_snapshot(core: Core<'_>, session_id: String) -> Result<Value, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    wire_sink(&core).await;
    Ok(serde_json::json!({
        "session": rt.id,
        "servers": core.mcp.status_for(&rt.id).await,
    }))
}

// ---------- 旧命令适配器（前端尚未迁移，保持可用） ----------

/// 用户级 mcp.json 原文。
#[tauri::command]
pub async fn get_mcp_config(core: Core<'_>) -> Result<String, String> {
    Ok(load_scope(&global_scope(&core.data_dir)).raw)
}

/// 保存用户级 mcp.json（等价 `mcp_save_config(scope = global)`）。
#[tauri::command]
pub async fn save_mcp_config(core: Core<'_>, json: String) -> Result<(), String> {
    let out = mcp_save_config(core, None, "global".to_string(), json).await?;
    if out["saved"] == Value::Bool(false) {
        return Err(format!("配置未通过校验：{}", out["issues"]));
    }
    Ok(())
}

/// 为该会话连接全部应启用的 MCP server。
#[tauri::command]
pub async fn connect_mcp(core: Core<'_>, session_id: String) -> Result<Value, String> {
    mcp_connect(core.clone(), session_id.clone(), None).await?;
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let status = core.mcp.status_for(&rt.id).await;
    let started: Vec<Value> = status
        .iter()
        .filter(|p| p.state == crate::mcp::McpState::Ready)
        .map(|p| serde_json::json!({ "name": p.name, "tools": p.tools }))
        .collect();
    let failed: Vec<Value> = status
        .iter()
        .filter_map(|p| {
            p.error
                .as_ref()
                .map(|e| serde_json::json!({ "name": p.name, "error": e.message }))
        })
        .collect();
    Ok(serde_json::json!({ "started": started, "failed": failed }))
}

/// 当前 MCP 连接状态（全部条目）。
#[tauri::command]
pub async fn mcp_status(core: Core<'_>) -> Result<Vec<crate::mcp::McpStatusPayload>, String> {
    wire_sink(&core).await;
    Ok(core.mcp.all_status().await)
}
