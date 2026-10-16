//! MCP 配置与连接状态命令。
//!
//! 作用域只有两层：全局（用户级 `<data_dir>/mcp.json`）与项目（`<project_dir>/mcp.json`，
//! project_dir 即 `<主目录>/.codewave`）。会话可见集 = 两层合并，同名项目级胜出。
//!
//! 命令只做校验 + 转调 core（分层约定）；状态事件由 [`wire_sink`] 接到 `EventSink`
//! （core 层不能依赖 tauri）。

use super::util::{Core, err};
use crate::core::agent::AgentCore;
use crate::mcp::{
    ConfigIssue, McpTransport, MergedServer, Scope, ScopeDoc, ScopeRef, global_scope, load_scope,
    merge_scopes, project_scope, visible_servers,
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
            if rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("会话已关闭".into());
            }
            rt.project_dir
                .as_deref()
                .map(project_scope)
                .ok_or_else(|| "当前会话没有项目目录，无法读写项目级 MCP 配置".to_string())
        }
    }
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
    source_path: String,
    launch: crate::mcp::LaunchStatus,
}

fn view_of(
    m: &MergedServer,
    core: &AgentCore,
    session_id: Option<&str>,
) -> Result<McpServerView, String> {
    let source = scope_ref(core, session_id, m.source)?;
    let key = crate::mcp::launch_key(core, &source, session_id, &m.name).map_err(err)?;
    let (transport, inferred) = match m.cfg.resolve_transport() {
        Ok(McpTransport::Stdio) => ("stdio", !m.cfg.transport_is_explicit()),
        Ok(McpTransport::StreamableHttp) => ("streamable_http", !m.cfg.transport_is_explicit()),
        Err(_) => ("unknown", false),
    };
    Ok(McpServerView {
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
        source_path: key.source.display().to_string(),
        launch: core.mcp.launch_status(&key, &m.cfg),
    })
}

/// 一层配置的视图载荷。
#[derive(Serialize)]
pub struct McpConfigDocPayload {
    scope: String,
    path: String,
    /// 该层 mcp.json 原文（前端编辑用；不存在则为空文档）
    json: String,
    version: String,
    /// 该层文件里的条目（用于编辑）
    servers: Vec<McpServerView>,
    /// 合并后的生效条目（带来源 / 被覆盖信息）
    effective: Vec<McpServerView>,
    /// 该层的解析与校验问题
    issues: Vec<ConfigIssue>,
}

fn doc_payload(
    sref: &ScopeRef,
    doc: &ScopeDoc,
    effective: &[MergedServer],
    core: &AgentCore,
    session_id: Option<&str>,
) -> Result<McpConfigDocPayload, String> {
    Ok(McpConfigDocPayload {
        scope: sref.scope.as_str().to_string(),
        path: sref.path.display().to_string(),
        json: doc.raw.clone(),
        version: crate::mcp::scope_version(&doc.raw),
        servers: doc
            .servers
            .iter()
            .map(|(name, cfg)| {
                view_of(
                    &MergedServer {
                        name: name.clone(),
                        cfg: cfg.clone(),
                        source: sref.scope,
                        overridden: None,
                        issues: Vec::new(),
                    },
                    core,
                    session_id,
                )
            })
            .collect::<Result<_, _>>()?,
        effective: effective
            .iter()
            .map(|server| view_of(server, core, session_id))
            .collect::<Result<_, _>>()?,
        issues: doc.issues.clone(),
    })
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
    doc_payload(&sref, &doc, &effective, &core, session_id.as_deref())
}

#[tauri::command]
pub async fn mcp_set_launch_trust(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
    name: String,
    expected_version: String,
    trusted: bool,
) -> Result<crate::mcp::LaunchStatus, String> {
    let source = scope_ref(
        &core,
        session_id.as_deref(),
        Scope::parse(&scope).map_err(err)?,
    )?;
    wire_sink(&core).await;
    crate::mcp::set_launch_trust(
        &core,
        &source,
        session_id.as_deref(),
        &name,
        &expected_version,
        trusted,
    )
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn mcp_prepare_test(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
    name: String,
    config: Value,
) -> Result<crate::mcp::ProbeChallenge, String> {
    let source = scope_ref(
        &core,
        session_id.as_deref(),
        Scope::parse(&scope).map_err(err)?,
    )?;
    let key = crate::mcp::launch_key(&core, &source, session_id.as_deref(), &name).map_err(err)?;
    let cfg = crate::mcp::validate_server(&name, config).map_err(err)?;
    core.mcp.prepare_probe(&key, &cfg).map_err(err)
}

/// 保存某作用域的 MCP 配置。
///
/// 结构校验走**与运行时同一套规则**（先把文本按正常装载路径解析一遍），
/// 只要存在 error 级问题就**不落盘**并原样返回问题清单（不静默改写用户文件）。
/// 落盘后差量接纳受影响连接的应用计划；Ready/Error 通过事件与快照异步收敛。
#[tauri::command]
pub async fn mcp_save_config(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
    json: String,
    expected_version: String,
) -> Result<crate::mcp::SaveReceipt, String> {
    let scope = Scope::parse(&scope).map_err(err)?;
    let source = scope_ref(&core, session_id.as_deref(), scope)?;
    wire_sink(&core).await;
    crate::mcp::save_configuration(&core, &source, &json, &expected_version)
        .await
        .map_err(err)
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
    let _commit = crate::mcp::CONFIG_COMMIT.lock().await;
    if core.session(&session_id).is_none() || rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("会话已关闭".into());
    }
    let visible = visible_servers(&rt);
    match names {
        None => core.mcp.warm(&rt.id, visible, http).await,
        Some(names) => core.mcp.connect_named(&rt.id, visible, &names, http).await,
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
        .find(|(key, _)| key.name == name)
        .ok_or_else(|| format!("会话可见集里没有名为 {name} 的 server"))?;
    core.mcp.disconnect(&target.0, &rt.id).await;
    let _commit = crate::mcp::CONFIG_COMMIT.lock().await;
    if rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("会话已关闭".into());
    }
    core.mcp
        .connect_named(&rt.id, visible_servers(&rt), &[name], http)
        .await;
    Ok(())
}

/// 临时测试连接：起 → `tools/list` → 立即回收，**不并入连接池、不改正式状态**。
#[tauri::command]
pub async fn mcp_test(
    core: Core<'_>,
    session_id: Option<String>,
    scope: String,
    name: String,
    config: Value,
    confirmation: Option<String>,
) -> Result<Value, String> {
    let scope = Scope::parse(&scope).map_err(err)?;
    // Validate project context but never read any on-disk server or change pool membership.
    let source = scope_ref(&core, session_id.as_deref(), scope)?;
    let key = crate::mcp::launch_key(&core, &source, session_id.as_deref(), &name).map_err(err)?;
    let cfg = crate::mcp::validate_server(&name, config).map_err(err)?;
    let http = core.client.read().unwrap().clone();
    Ok(
        match core
            .mcp
            .test_connection(&key, cfg, confirmation.as_deref().unwrap_or_default(), http)
            .await
        {
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
