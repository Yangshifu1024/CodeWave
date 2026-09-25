//! MCP 连接管理：**会话可见集** + 连接池 + 调用分发，状态自持。
//!
//! # 为什么以会话为纲
//!
//! 旧实现把连接表挂在 `AgentCore` 上、以 server 名为键，`tool_defs()` 无过滤地
//! 返回全部 server 的工具 → 项目 A 的 MCP 工具会出现在项目 B 的请求里
//! （`core/agent/stream.rs` 全量注入 + 此处无过滤）。而且每次 `connect_mcp`
//! 都会 `stop_all` 级重建，打开任意会话都会掐断正在用的连接。
//!
//! 现在：
//! - 池键是 `(作用域, 项目 id, server 名)`：全局的 `fs` 与某项目的 `fs` 是**不同**条目。
//! - 每个会话登记自己的可见集（`sessions`），工具按可见集过滤后才注入模型。
//! - 引用计数归零才回收进程；有引用的条目**绝不**被 LRU 淘汰。
//! - `stop` / `disconnect` / 淘汰都会发状态事件（旧 `stop_all` 不发 → UI 状态残留）。
//!
//! 错误一律走 [`McpError`] 结构化分类：只有连接类错误允许重连，且**只重放只读调用**。

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, JsonObject};
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::WorkerTransport;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpClientWorker,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::core::types::Content;

use super::config::{McpServerConfig, McpTransport, Scope};
use super::error::McpError;
use super::process::build_stdio_wrap;
use super::tools::{McpTool, McpToolDef, build_tool_defs, filter_tools, normalize_schema};

/// 连接 + initialize + list_tools 的总超时。
pub const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// 连接池默认上限（可在 [`McpManager::new`] 调整）。
pub const DEFAULT_MAX_SERVERS: usize = 16;
/// 被淘汰的 server 再次被需要时的重拉防抖窗口（避免抖动风暴）。
const EVICT_DEBOUNCE: Duration = Duration::from_secs(5);

/// server 连接状态。
///
/// `Error(String)` 的 serde 形态是 `{ "error": "..." }`（外部标签枚举），
/// 前端 `mcpStatusRow` 依赖这个形状识别失败行；`Stopped` / `Evicted` 对旧前端
/// 表现为「未连接」，不会误报成故障。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum McpState {
    /// 连接/初始化中
    Starting,
    /// 就绪可用
    Ready,
    /// 已主动停止（断开 / 引用归零）
    Stopped,
    /// 被资源上限淘汰
    Evicted,
    /// 失败（含可读原因）
    Error(String),
}

/// 池键：一层作用域里的一个 server。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PoolKey {
    /// 作用域
    pub scope: Scope,
    /// 项目 id（全局作用域为 None）
    pub project_id: Option<String>,
    /// server 名
    pub name: String,
}

impl PoolKey {
    /// 全局作用域条目。
    pub fn global(name: impl Into<String>) -> Self {
        PoolKey {
            scope: Scope::Global,
            project_id: None,
            name: name.into(),
        }
    }

    /// 项目作用域条目。
    pub fn project(project_id: impl Into<String>, name: impl Into<String>) -> Self {
        PoolKey {
            scope: Scope::Project,
            project_id: Some(project_id.into()),
            name: name.into(),
        }
    }

    /// 线缆用的作用域字符串。
    pub fn scope_str(&self) -> &'static str {
        self.scope.as_str()
    }
}

/// 状态事件载荷（`mcp:status` 与 `mcp_status` 命令共用）。
#[derive(Debug, Clone, Serialize)]
pub struct McpStatusPayload {
    /// server 名
    pub name: String,
    /// 作用域（`global` / `project`）
    pub scope: Scope,
    /// 状态
    pub state: McpState,
    /// 已注入的工具数
    pub tools: usize,
    /// 因工具过滤而未注入的数量
    pub tools_filtered: usize,
    /// 子进程 PID（http 传输为 None）
    pub pid: Option<u32>,
    /// 结构化错误（`state` 为 `Error` 时非空）
    pub error: Option<McpError>,
    /// 可见性提示（「已被淘汰（资源上限）」「已禁用」等）
    pub note: Option<String>,
}

/// 状态变化回调：由 host 层接到 `EventSink`（core 层不能依赖 tauri）。
pub type StatusSink = Arc<dyn Fn(&str, &McpStatusPayload) + Send + Sync>;

/// 池内条目。
struct ServerEntry {
    cfg: McpServerConfig,
    state: McpState,
    error: Option<McpError>,
    tools: Vec<McpTool>,
    tools_filtered: usize,
    service: Option<ClientService>,
    peer: Option<Peer<RoleClient>>,
    pid: Option<u32>,
    generation: u64,
    /// 引用该条目的会话集合
    sessions: HashSet<String>,
    /// 最近一次使用序号（LRU）
    last_used: u64,
    note: Option<String>,
    /// `tools/list_changed` 脏标记（run 边界消费）
    dirty: Option<Arc<AtomicBool>>,
    /// 最近一次被淘汰的时刻（重拉防抖）
    last_evict: Option<Instant>,
}

struct Inner {
    entries: HashMap<PoolKey, ServerEntry>,
    /// 会话 id → 可见集
    sessions: HashMap<String, HashSet<PoolKey>>,
}

/// MCP 连接管理器。
pub struct McpManager {
    inner: Mutex<Inner>,
    max_servers: usize,
    tick: AtomicU64,
    sink: RwLock<Option<StatusSink>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SERVERS)
    }
}

/// 订阅 `tools/list_changed`：**只置脏标记**，绝不改共享状态。
///
/// 工具集变更由 [`McpManager::tool_defs_for`] 在 **run 边界**消费——若在通知回调里直接改，
/// 一次 run 进行到一半工具集就变了，模型会拿旧 schema 去调新工具。
#[derive(Clone)]
struct ToolListWatcher {
    dirty: Arc<AtomicBool>,
}

impl rmcp::ClientHandler for ToolListWatcher {
    fn on_tool_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + rmcp::service::MaybeSendFuture + '_ {
        self.dirty.store(true, Ordering::SeqCst);
        std::future::ready(())
    }
}

/// 客户端服务句柄。handler 为 [`ToolListWatcher`]（订阅 `tools/list_changed`）。
type ClientService = Arc<RunningService<RoleClient, ToolListWatcher>>;

/// 建连完成的内部句柄。
struct Ready {
    tools: Vec<McpTool>,
    filtered: usize,
    service: ClientService,
    peer: Peer<RoleClient>,
    pid: Option<u32>,
    /// `tools/list_changed` 脏标记（run 边界消费）
    dirty: Arc<AtomicBool>,
}

fn payload_of(key: &PoolKey, e: &ServerEntry) -> McpStatusPayload {
    McpStatusPayload {
        name: key.name.clone(),
        scope: key.scope,
        state: e.state.clone(),
        tools: if e.state == McpState::Ready {
            e.tools.len()
        } else {
            0
        },
        tools_filtered: e.tools_filtered,
        pid: e.pid,
        error: e.error.clone(),
        note: e.note.clone(),
    }
}

fn clear_connection(e: &mut ServerEntry) {
    e.peer = None;
    e.pid = None;
    e.tools.clear();
    e.tools_filtered = 0;
    e.dirty = None;
    e.generation = e.generation.wrapping_add(1);
}

impl McpManager {
    /// 新建管理器（`max_servers` = 连接池上限）。
    pub fn new(max_servers: usize) -> Self {
        McpManager {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                sessions: HashMap::new(),
            }),
            max_servers: max_servers.max(1),
            tick: AtomicU64::new(0),
            sink: RwLock::new(None),
        }
    }

    /// 装配状态事件回调（host 层接 `EventSink`）。
    pub async fn set_status_sink(&self, sink: StatusSink) {
        *self.sink.write().await = Some(sink);
    }

    async fn emit(&self, session: &str, p: &McpStatusPayload) {
        let sink = self.sink.read().await.clone();
        if let Some(s) = sink {
            s(session, p);
        }
    }

    fn tick(&self) -> u64 {
        self.tick.fetch_add(1, Ordering::Relaxed)
    }

    /// 某会话当前登记的可见集（排序稳定）。
    pub async fn visible_keys(&self, session: &str) -> Vec<PoolKey> {
        let inner = self.inner.lock().await;
        let mut v: Vec<PoolKey> = inner
            .sessions
            .get(session)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// 会话打开时调用：登记可见集并**并行**拉起未就绪的连接。
    ///
    /// 不 await 全部完成——IPC 立即返回，各 server 独立收敛为 ready/error 并经事件推送
    /// （旧实现串行 await 每个 server，坏配置多时会长时间阻塞打开会话）。
    pub fn warm(
        self: &Arc<Self>,
        session: &str,
        visible: Vec<(PoolKey, McpServerConfig)>,
        http: reqwest::Client,
    ) {
        let this = self.clone();
        let session = session.to_string();
        tokio::spawn(async move {
            let pending = this.register_session(&session, &visible).await;
            let mut handles = Vec::new();
            for (key, cfg) in pending {
                let this = this.clone();
                let session = session.clone();
                let http = http.clone();
                handles.push(tokio::spawn(async move {
                    let _ = this.ensure(&key, cfg, http, &session).await;
                }));
            }
            for h in handles {
                let _ = h.await;
            }
        });
    }

    /// 登记（或替换）会话可见集：新增键引用 +1、移出的键引用 −1，返回需要建连的条目。
    async fn register_session(
        &self,
        session: &str,
        visible: &[(PoolKey, McpServerConfig)],
    ) -> Vec<(PoolKey, McpServerConfig)> {
        let wanted: HashSet<PoolKey> = visible.iter().map(|(k, _)| k.clone()).collect();
        let mut inner = self.inner.lock().await;
        let previous = inner.sessions.insert(session.to_string(), wanted.clone());
        // 移出的键：引用 −1
        let removed: Vec<PoolKey> = previous
            .unwrap_or_default()
            .into_iter()
            .filter(|k| !wanted.contains(k))
            .collect();
        let mut to_close: Vec<ClientService> = Vec::new();
        let mut closed_payloads: Vec<(PoolKey, McpStatusPayload)> = Vec::new();
        for key in removed {
            if let Some(e) = inner.entries.get_mut(&key) {
                e.sessions.remove(session);
                if e.sessions.is_empty() {
                    if let Some(svc) = e.service.take() {
                        to_close.push(svc);
                    }
                    clear_connection(e);
                    e.state = McpState::Stopped;
                    e.note = Some("会话已关闭".to_string());
                    closed_payloads.push((key.clone(), payload_of(&key, e)));
                }
            }
        }
        // 新增的键：引用 +1，并挑出未就绪者
        let mut pending = Vec::new();
        for (key, cfg) in visible {
            match inner.entries.get_mut(key) {
                Some(e) => {
                    e.sessions.insert(session.to_string());
                    e.cfg = cfg.clone();
                    if e.state != McpState::Ready {
                        pending.push((key.clone(), cfg.clone()));
                    }
                }
                None => {
                    inner.entries.insert(
                        key.clone(),
                        ServerEntry {
                            cfg: cfg.clone(),
                            state: McpState::Starting,
                            error: None,
                            tools: Vec::new(),
                            tools_filtered: 0,
                            service: None,
                            peer: None,
                            pid: None,
                            generation: 0,
                            sessions: HashSet::from([session.to_string()]),
                            last_used: self.tick(),
                            note: None,
                            dirty: None,
                            last_evict: None,
                        },
                    );
                    pending.push((key.clone(), cfg.clone()));
                }
            }
        }
        drop(inner);
        for svc in to_close {
            if let Some(svc) = Arc::into_inner(svc) {
                let _ = svc.cancel().await;
            }
        }
        for (key, p) in closed_payloads {
            let _ = key;
            self.emit(session, &p).await;
        }
        pending
    }

    /// 会话关闭：可见集清空，引用归零的连接立即回收。
    pub async fn release_session(&self, session: &str) {
        let keys = {
            let mut inner = self.inner.lock().await;
            inner.sessions.remove(session).unwrap_or_default()
        };
        let mut to_close: Vec<ClientService> = Vec::new();
        let mut payloads: Vec<McpStatusPayload> = Vec::new();
        {
            let mut inner = self.inner.lock().await;
            for key in keys {
                if let Some(e) = inner.entries.get_mut(&key) {
                    e.sessions.remove(session);
                    if e.sessions.is_empty() {
                        if let Some(svc) = e.service.take() {
                            to_close.push(svc);
                        }
                        clear_connection(e);
                        e.state = McpState::Stopped;
                        e.error = None;
                        e.note = Some("会话已关闭".to_string());
                        payloads.push(payload_of(&key, e));
                    }
                }
            }
        }
        for svc in to_close {
            if let Some(svc) = Arc::into_inner(svc) {
                let _ = svc.cancel().await;
            }
        }
        for p in payloads {
            self.emit(session, &p).await;
        }
    }

    /// 单 server 断开（保留可见集登记，便于重连）。
    pub async fn disconnect(&self, key: &PoolKey, session: &str) {
        let (svc, payload) = {
            let mut inner = self.inner.lock().await;
            match inner.entries.get_mut(key) {
                Some(e) => {
                    let svc = e.service.take();
                    clear_connection(e);
                    e.state = McpState::Stopped;
                    e.error = None;
                    e.note = Some("已手动断开".to_string());
                    (svc, Some(payload_of(key, e)))
                }
                None => (None, None),
            }
        };
        if let Some(svc) = svc.and_then(Arc::into_inner) {
            let _ = svc.cancel().await;
        }
        if let Some(p) = payload {
            self.emit(session, &p).await;
        }
    }

    /// 停止全部连接并清空（配置保存后热重载用）。每个条目都会发状态事件。
    pub async fn disconnect_all(&self) {
        let (svcs, payloads) = {
            let mut inner = self.inner.lock().await;
            let keys: Vec<PoolKey> = inner.entries.keys().cloned().collect();
            let mut svcs: Vec<ClientService> = Vec::new();
            let mut payloads: Vec<McpStatusPayload> = Vec::new();
            for key in keys {
                if let Some(e) = inner.entries.get_mut(&key) {
                    if let Some(svc) = e.service.take() {
                        svcs.push(svc);
                    }
                    clear_connection(e);
                    e.state = McpState::Stopped;
                    e.error = None;
                    e.note = Some("已停止".to_string());
                    payloads.push(payload_of(&key, e));
                }
            }
            inner.sessions.clear();
            (svcs, payloads)
        };
        for svc in svcs {
            if let Some(svc) = Arc::into_inner(svc) {
                let _ = svc.cancel().await;
            }
        }
        for p in payloads {
            self.emit("", &p).await;
        }
    }

    /// 腾出一个槽位：只淘汰**引用计数为 0** 的条目（LRU 优先）。
    ///
    /// 有引用的条目绝不淘汰——宁可短暂超限并 `warn`，也不能掐断正在用的连接。
    async fn reserve_slot(&self, incoming: &PoolKey) -> Vec<McpStatusPayload> {
        let mut evicted = Vec::new();
        let mut inner = self.inner.lock().await;
        // 上限只约束**占用槽位**（活跃）的条目：已淘汰的条目只是状态记录，
        // 留着才能让状态表显示「已淘汰（资源上限）」，不占槽位。
        let live_count = |m: &HashMap<PoolKey, ServerEntry>| {
            m.values().filter(|e| e.state != McpState::Evicted).count()
        };
        if live_count(&inner.entries) <= self.max_servers {
            return evicted;
        }
        // 候选：无引用、且不是 Starting（Starting 由正在跑的 ensure 持有）、且未淘汰
        loop {
            if live_count(&inner.entries) <= self.max_servers {
                break;
            }
            let victim = inner
                .entries
                .iter()
                .filter(|(k, e)| {
                    *k != incoming
                        && e.sessions.is_empty()
                        && e.state != McpState::Starting
                        && e.state != McpState::Evicted
                })
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else {
                tracing::warn!(
                    "MCP 连接池已达上限 {} 且所有条目都被会话引用，允许短暂超限",
                    self.max_servers
                );
                break;
            };
            if let Some(e) = inner.entries.get_mut(&victim) {
                e.state = McpState::Evicted;
                e.error = None;
                clear_connection(e);
                e.note = Some("已被淘汰（资源上限）；需要时会自动重拉".to_string());
                e.last_evict = Some(Instant::now());
                // service 取出后由调用方取消（不持锁 await）
                if let Some(svc) = e.service.take() {
                    // 立刻取消：淘汰语义就是回收进程
                    tokio::spawn(async move {
                        if let Some(svc) = Arc::into_inner(svc) {
                            let _ = svc.cancel().await;
                        }
                    });
                }
                evicted.push(payload_of(&victim, e));
            }
        }
        evicted
    }

    /// 确保某键已连接（幂等 + 淘汰防抖 + 并行安全）。
    ///
    /// 已 `Ready` 直接复用；处于淘汰防抖窗口内则返回配置错（不重拉）。
    pub async fn ensure(
        self: &Arc<Self>,
        key: &PoolKey,
        cfg: McpServerConfig,
        http: reqwest::Client,
        session: &str,
    ) -> Result<usize, McpError> {
        let generation = {
            let mut inner = self.inner.lock().await;
            if let Some(e) = inner.entries.get_mut(key) {
                e.last_used = self.tick();
                if e.state == McpState::Ready {
                    return Ok(e.tools.len());
                }
                if e.state == McpState::Evicted {
                    if let Some(t) = e.last_evict {
                        if t.elapsed() < EVICT_DEBOUNCE {
                            return Err(McpError::config_hint(
                                format!("server {} 刚因资源上限被淘汰，稍后自动重拉", key.name),
                                "如需立即恢复，请点「重连」；也可减少同时启用的 server 数量。",
                            ));
                        }
                    }
                }
                e.generation = e.generation.wrapping_add(1);
                e.generation
            } else {
                let mut e = blank_entry(&cfg, self.tick());
                e.generation = 1;
                inner.entries.insert(key.clone(), e);
                1
            }
        };
        for p in self.reserve_slot(key).await {
            self.emit(session, &p).await;
        }
        if !self.set_starting(key, &cfg, session, generation).await {
            return Err(McpError::config("连接已取消"));
        }
        match Self::spawn_connect(key, &cfg, http).await {
            Ok(ready) => {
                let n = ready.tools.len();
                if self.set_ready(key, ready, session, generation).await {
                    Ok(n)
                } else {
                    Err(McpError::config("连接已取消"))
                }
            }
            Err(e) => {
                self.set_error(key, e.clone(), session, generation).await;
                Err(e)
            }
        }
    }

    async fn set_starting(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
        session: &str,
        generation: u64,
    ) -> bool {
        let payload = {
            let mut inner = self.inner.lock().await;
            let tick = self.tick();
            let Some(e) = inner.entries.get_mut(key) else {
                return false;
            };
            if e.generation != generation {
                return false;
            }
            e.cfg = cfg.clone();
            e.state = McpState::Starting;
            e.error = None;
            e.note = None;
            e.pid = None;
            e.last_used = tick;
            payload_of(key, e)
        };
        self.emit(session, &payload).await;
        true
    }

    async fn set_ready(&self, key: &PoolKey, ready: Ready, session: &str, generation: u64) -> bool {
        let Ready {
            tools,
            filtered,
            service,
            peer,
            pid,
            dirty,
        } = ready;
        let mut service = Some(service);
        let payload = {
            let mut inner = self.inner.lock().await;
            inner.entries.get_mut(key).and_then(|e| {
                if e.generation != generation {
                    return None;
                }
                e.tools = tools;
                e.tools_filtered = filtered;
                e.service = service.take();
                e.peer = Some(peer);
                e.pid = pid;
                e.dirty = Some(dirty);
                e.state = McpState::Ready;
                e.error = None;
                e.note = None;
                e.last_used = self.tick();
                Some(payload_of(key, e))
            })
        };
        let Some(payload) = payload else {
            if let Some(svc) = service.and_then(Arc::into_inner) {
                let _ = svc.cancel().await;
            }
            return false;
        };
        self.emit(session, &payload).await;
        true
    }

    async fn set_error(&self, key: &PoolKey, err: McpError, session: &str, generation: u64) {
        let payload = {
            let mut inner = self.inner.lock().await;
            let Some(e) = inner.entries.get_mut(key) else {
                return;
            };
            if e.generation != generation {
                return;
            }
            e.tools.clear();
            e.tools_filtered = 0;
            e.peer = None;
            e.pid = None;
            e.state = McpState::Error(err.message.clone());
            e.error = Some(err);
            e.note = None;
            payload_of(key, e)
        };
        self.emit(session, &payload).await;
    }

    /// 建连 + initialize + list_tools（含工具过滤）。
    async fn spawn_connect(
        key: &PoolKey,
        cfg: &McpServerConfig,
        http: reqwest::Client,
    ) -> Result<Ready, McpError> {
        let transport = cfg.resolve_transport()?;
        let name = key.name.clone();
        // tools/list_changed 只置位；实际刷新发生在 run 边界（tool_defs_for）
        let dirty = Arc::new(AtomicBool::new(false));
        let connect = async {
            match transport {
                McpTransport::Stdio => {
                    let wrap = build_stdio_wrap(cfg)?;
                    let child = rmcp::transport::child_process::TokioChildProcess::new(wrap)
                        .map_err(|e| {
                            McpError::spawn(format!("进程启动失败：{e}"))
                                .with_hint("检查 command 是否存在、cwd 是否有效、env 是否合法。")
                        })?;
                    let pid = child.id();
                    let svc = ToolListWatcher {
                        dirty: dirty.clone(),
                    }
                    .serve(child)
                    .await
                    .map_err(|e| McpError::handshake(format!("initialize 失败：{e}")))?;
                    Ok::<(ClientService, Option<u32>), McpError>((Arc::new(svc), pid))
                }
                McpTransport::StreamableHttp => {
                    let url = cfg.url.clone().unwrap_or_default();
                    if url.trim().is_empty() {
                        return Err(McpError::config("streamable_http transport 需要 url"));
                    }
                    // 鉴权与自定义请求头：远程 MCP 端点普遍需要
                    // （旧实现只设 uri，带鉴权的端点根本无法配置）。
                    let mut http_cfg = StreamableHttpClientTransportConfig::with_uri(url);
                    for (k, v) in &cfg.headers {
                        if k.eq_ignore_ascii_case("authorization") {
                            http_cfg.auth_header = Some(v.clone());
                            continue;
                        }
                        match (
                            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                            reqwest::header::HeaderValue::from_str(v),
                        ) {
                            (Ok(hname), Ok(hvalue)) => {
                                http_cfg.custom_headers.insert(hname, hvalue);
                            }
                            _ => tracing::warn!("MCP server {name} 的请求头 {k} 非法，已忽略"),
                        }
                    }
                    let worker = StreamableHttpClientWorker::new(http, http_cfg);
                    let tp = WorkerTransport::spawn(worker);
                    let svc = ToolListWatcher {
                        dirty: dirty.clone(),
                    }
                    .serve(tp)
                    .await
                    .map_err(|e| {
                        McpError::handshake(format!("连接失败：{e}")).with_hint(
                            "确认服务端可达、鉴权头未过期；本地端点请确认代理未拦截 loopback。",
                        )
                    })?;
                    Ok((Arc::new(svc), None))
                }
            }
        };
        let (service, pid) = match tokio::time::timeout(INIT_TIMEOUT, connect).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(McpError::handshake(format!(
                    "initialize 超时（{}s）",
                    INIT_TIMEOUT.as_secs()
                ))
                .with_hint(
                    "server 无响应：确认它能独立启动；必要时调大该 server 的 timeout_ms。",
                ));
            }
        };
        let peer = service.peer().clone();
        let raw = match tokio::time::timeout(INIT_TIMEOUT, service.list_all_tools()).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                return Err(McpError::handshake(format!("list_tools 失败：{e}")));
            }
            Err(_) => return Err(McpError::handshake("list_tools 超时")),
        };
        let all: Vec<McpTool> = raw
            .iter()
            .map(|t| McpTool {
                server: name.clone(),
                name: t.name.to_string(),
                description: format!("[{name}] {}", t.description.as_deref().unwrap_or("")),
                schema_json: normalize_schema(t.input_schema.as_ref()),
            })
            .collect();
        let (tools, filtered) = filter_tools(&cfg.tools, &all);
        Ok(Ready {
            tools,
            filtered,
            service,
            peer,
            pid,
            dirty,
        })
    }

    /// 会话可见的工具定义（按名排序，函数名已归一化）。
    ///
    /// 只取该会话可见集内 `Ready` 条目的工具——这是「项目 A 的 MCP 工具不出现在
    /// 项目 B 请求里」的落地处。
    pub async fn tool_defs_for(&self, session: &str) -> Vec<McpToolDef> {
        let (keys, dirty) = {
            let mut inner = self.inner.lock().await;
            let keys: Vec<PoolKey> = inner
                .sessions
                .get(session)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            // 先把**所有**脏标记取走（含不在本会话可见集里的，否则永远清不掉），
            // 再挑出本会话可见且已就绪的条目去刷新。
            let mut dirty = Vec::new();
            for (k, e) in inner.entries.iter_mut() {
                let flagged = e
                    .dirty
                    .as_ref()
                    .is_some_and(|d| d.swap(false, Ordering::SeqCst));
                if flagged && e.state == McpState::Ready && keys.contains(k) {
                    dirty.push((k.clone(), e.cfg.clone()));
                }
            }
            (keys, dirty)
        };
        // run 边界刷新：工具集变更只在这里生效，绝不在通知回调里改共享状态
        for (key, cfg) in dirty {
            self.refresh_tools(&key, &cfg).await;
        }
        let tools = {
            let mut inner = self.inner.lock().await;
            let tick = self.tick();
            let mut tools = Vec::new();
            for k in &keys {
                if let Some(e) = inner.entries.get_mut(k) {
                    if e.state == McpState::Ready {
                        e.last_used = tick;
                        tools.extend(e.tools.iter().cloned());
                    }
                }
            }
            tools
        };
        let (defs, _) = build_tool_defs(&tools);
        defs
    }

    /// 重新列某条目的工具（`tools/list_changed` 后由 run 边界调用）。
    ///
    /// 失败只 `warn` 并保留旧列表——刷新失败不该把可用连接打成错误态。
    async fn refresh_tools(&self, key: &PoolKey, cfg: &McpServerConfig) {
        let peer = {
            let inner = self.inner.lock().await;
            inner.entries.get(key).and_then(|e| e.peer.clone())
        };
        let Some(peer) = peer else { return };
        let raw = match tokio::time::timeout(INIT_TIMEOUT, peer.list_all_tools()).await {
            Ok(Ok(t)) => t,
            other => {
                tracing::warn!(
                    "MCP {} 的 tools/list_changed 刷新失败：{:?}",
                    key.name,
                    other.map(|r| r.map(|_| ()))
                );
                return;
            }
        };
        let name = key.name.clone();
        let all: Vec<McpTool> = raw
            .iter()
            .map(|t| McpTool {
                server: name.clone(),
                name: t.name.to_string(),
                description: format!("[{name}] {}", t.description.as_deref().unwrap_or("")),
                schema_json: normalize_schema(t.input_schema.as_ref()),
            })
            .collect();
        let (tools, filtered) = filter_tools(&cfg.tools, &all);
        let mut inner = self.inner.lock().await;
        if let Some(e) = inner.entries.get_mut(key) {
            e.tools = tools;
            e.tools_filtered = filtered;
        }
    }

    /// 该次调用是否需要审批：返回 `(server 名, 工具名)`；`read_only` / `always_allow` 声明过则 `None`。
    pub async fn approval_target(&self, session: &str, function: &str) -> Option<(String, String)> {
        let inner = self.inner.lock().await;
        let keys = inner.sessions.get(session)?.clone();
        let mut tools = Vec::new();
        for k in &keys {
            if let Some(e) = inner.entries.get(k) {
                if e.state == McpState::Ready {
                    tools.extend(e.tools.iter().cloned());
                }
            }
        }
        let (_, index) = build_tool_defs(&tools);
        let (server, tool) = index.get(function)?.clone();
        let cfg = keys
            .iter()
            .filter(|k| k.name == server)
            .find_map(|k| inner.entries.get(k))
            .map(|e| e.cfg.clone())?;
        if cfg.read_only || cfg.always_allow {
            None
        } else {
            Some((server, tool))
        }
    }

    /// 调用工具。
    ///
    /// 三道约束：
    /// 1. **会话可见性**：只认该会话可见集内 `Ready` 条目，函数名走注册表反查
    ///    （**绝不**用 `parse_function` 切分——server 名可含 `__`）。
    /// 2. **超时**取自该 server 配置（`timeout_ms`，缺省 120s），不再硬编码。
    /// 3. **取消优先**：`cancel` 一旦置位立即返回 [`McpErrorKind::Cancelled`]，
    ///    且被取消的调用**不再重放**（取消优先于重连判定）。
    pub async fn call(
        &self,
        session: &str,
        function: &str,
        args: Value,
        cancel: &CancellationToken,
    ) -> Result<McpCallOutput, McpError> {
        let (peer, tool, timeout) = {
            let mut inner = self.inner.lock().await;
            let keys: Vec<PoolKey> = inner
                .sessions
                .get(session)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            let mut tools = Vec::new();
            for k in &keys {
                if let Some(e) = inner.entries.get(k) {
                    if e.state == McpState::Ready {
                        tools.extend(e.tools.iter().cloned());
                    }
                }
            }
            let (_, index) = build_tool_defs(&tools);
            let Some((server, tool)) = index.get(function).cloned() else {
                return Err(McpError::config(format!(
                    "该会话不可见的 MCP 工具：{function}（可能属于其它项目、已禁用或未连接）"
                )));
            };
            let Some(key) = keys.iter().find(|k| k.name == server) else {
                return Err(McpError::config(format!(
                    "MCP server 不在会话可见集：{server}"
                )));
            };
            let Some(e) = inner.entries.get_mut(key) else {
                return Err(McpError::config(format!("MCP server 未连接：{server}")));
            };
            let Some(peer) = e.peer.clone() else {
                return Err(McpError::handshake(format!(
                    "MCP server 不可用：{server}（{:?}）",
                    e.state
                )));
            };
            e.last_used = self.tick();
            (peer, tool, e.cfg.timeout())
        };

        let mut params = CallToolRequestParams::new(tool.clone());
        if let Some(obj) = args.as_object() {
            let json_obj: JsonObject = serde_json::from_value(Value::Object(obj.clone()))
                .map_err(|e| McpError::call(format!("参数序列化失败：{e}")))?;
            params.arguments = Some(json_obj);
        }

        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(McpError::cancelled()),
            r = tokio::time::timeout(timeout, peer.call_tool(params)) => match r {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return Err(classify_service_error(e)),
                Err(_) => {
                    return Err(McpError::call(format!(
                        "MCP 调用超时（{}ms）",
                        timeout.as_millis()
                    ))
                    .with_hint("可在该 server 配置里调大 timeout_ms。"))
                }
            },
        };
        Ok(map_call_output(result))
    }

    /// 会话可见的状态（含未就绪条目，供状态表展示「连接中 / 失败 / 已淘汰」）。
    pub async fn status_for(&self, session: &str) -> Vec<McpStatusPayload> {
        let inner = self.inner.lock().await;
        let mut v: Vec<McpStatusPayload> = inner
            .sessions
            .get(session)
            .map(|keys| {
                keys.iter()
                    .filter_map(|k| inner.entries.get(k).map(|e| payload_of(k, e)))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| (a.scope.as_str(), &a.name).cmp(&(b.scope.as_str(), &b.name)));
        v
    }

    /// 全部条目状态（无会话上下文的 `mcp_status` 命令用）。
    pub async fn all_status(&self) -> Vec<McpStatusPayload> {
        let inner = self.inner.lock().await;
        let mut v: Vec<McpStatusPayload> = inner
            .entries
            .iter()
            .map(|(k, e)| payload_of(k, e))
            .collect();
        v.sort_by(|a, b| (a.scope.as_str(), &a.name).cmp(&(b.scope.as_str(), &b.name)));
        v
    }

    /// 临时测试连接：起 → `tools/list` → **立即回收**，不并入连接池、不改正式状态。
    pub async fn test_connection(
        cfg: McpServerConfig,
        http: reqwest::Client,
    ) -> Result<usize, McpError> {
        let key = PoolKey::global("__test__");
        let ready = Self::spawn_connect(&key, &cfg, http).await?;
        let n = ready.tools.len();
        let Ready { service, .. } = ready;
        if let Some(svc) = Arc::into_inner(service) {
            let _ = svc.cancel().await;
        }
        Ok(n)
    }
}

/// 工具调用结果。
#[derive(Debug, Clone)]
pub struct McpCallOutput {
    /// 前端工具卡数据。**形状确定**：恒为 `{"text": ...}`（失败时附 `"isError": true`），
    /// 不再由「内容是否恰好是合法 JSON」决定形状（那让同一 server 的结果忽而对象忽而文本）。
    pub data: Value,
    /// 模型可见文本（内容块按序拼接）
    pub text: String,
    /// server 侧 `is_error`（显式传递，不再折叠成 `Err` 丢掉语义）
    pub is_error: bool,
    /// 给模型的**多模态**内容块：文本按原序，图片走 `Content::Image`。
    /// 由批次层灌进 `ToolOutcome.extra_model_content`（不进前端 outcome JSON）。
    pub blocks: Vec<Content>,
}

fn blank_entry(cfg: &McpServerConfig, tick: u64) -> ServerEntry {
    ServerEntry {
        cfg: cfg.clone(),
        state: McpState::Starting,
        error: None,
        tools: Vec::new(),
        tools_filtered: 0,
        service: None,
        peer: None,
        pid: None,
        generation: 0,
        sessions: HashSet::new(),
        last_used: tick,
        dirty: None,
        note: None,
        last_evict: None,
    }
}

/// rmcp 的服务错误 → 结构化分类。
///
/// **只有** `TransportSend` / `TransportClosed` 判为连接类（可重连 + 只重放只读调用）；
/// 协议错与超时一律 `Call`（可能已有副作用，绝不重放）。
fn classify_service_error(e: rmcp::service::ServiceError) -> McpError {
    use rmcp::service::ServiceError as E;
    match e {
        E::TransportSend(inner) => McpError::handshake(format!("传输发送失败：{inner}")),
        E::TransportClosed => McpError::handshake("连接已断开（transport closed）"),
        E::Cancelled { .. } => McpError::cancelled(),
        E::Timeout { timeout } => McpError::call(format!("MCP 调用超时（{timeout:?}）")),
        other => McpError::call(format!("{other}")),
    }
}

/// rmcp 的调用结果 → [`McpCallOutput`]。
fn map_call_output(result: rmcp::model::CallToolResult) -> McpCallOutput {
    use rmcp::model::ContentBlock as B;
    // `text` 是给人/前端看的纯文本投影（非文本块留可读标记）；
    // `blocks` 是给模型的多模态内容（图片走 Image 块，不带标记）。
    let mut text = String::new();
    let mut blocks: Vec<Content> = Vec::new();
    for block in &result.content {
        if !text.is_empty() {
            text.push(char::from(10)); // 换行（不用字符字面量，避免转义）
        }
        match block {
            B::Text(t) => {
                text.push_str(&t.text);
                blocks.push(Content::Text {
                    text: t.text.clone(),
                });
            }
            B::Image(img) => {
                text.push_str(&format!("[image {}]", img.mime_type));
                blocks.push(Content::Image {
                    media_type: img.mime_type.to_string(),
                    data: img.data.to_string(),
                });
            }
            B::Audio(a) => {
                text.push_str(&format!("[audio {}]", a.mime_type));
                blocks.push(Content::Text {
                    text: format!("[audio {}]", a.mime_type),
                });
            }
            B::Resource(_) => {
                text.push_str("[embedded resource]");
                blocks.push(Content::Text {
                    text: "[embedded resource]".to_string(),
                });
            }
            B::ResourceLink(_) => {
                text.push_str("[resource link]");
                blocks.push(Content::Text {
                    text: "[resource link]".to_string(),
                });
            }
            _ => {
                text.push_str("[non-text content]");
                blocks.push(Content::Text {
                    text: "[non-text content]".to_string(),
                });
            }
        }
    }
    let is_error = result.is_error.unwrap_or(false);
    let data = if is_error {
        serde_json::json!({ "text": text, "isError": true })
    } else {
        serde_json::json!({ "text": text })
    };
    McpCallOutput {
        data,
        text,
        is_error,
        blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::error::McpErrorKind;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    /// 收集状态事件的 sink。
    fn collector() -> (StatusSink, Arc<StdMutex<Vec<(String, McpStatusPayload)>>>) {
        let log: Arc<StdMutex<Vec<(String, McpStatusPayload)>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let l = log.clone();
        let sink: StatusSink = Arc::new(move |session, p| {
            l.lock().unwrap().push((session.to_string(), p.clone()));
        });
        (sink, log)
    }

    fn bogus_cfg() -> McpServerConfig {
        serde_json::from_value(json!({ "command": "definitely-not-a-real-binary-xyz" })).unwrap()
    }

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn test_server_script() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/mcp-test-server.mjs")
    }

    fn node_cfg() -> McpServerConfig {
        serde_json::from_value(json!({
            "command": "node",
            "args": [test_server_script().to_string_lossy()],
        }))
        .unwrap()
    }

    #[test]
    fn pool_key_scope_and_name_are_part_of_identity() {
        assert_ne!(PoolKey::global("fs"), PoolKey::project("p1", "fs"));
        assert_ne!(PoolKey::project("p1", "fs"), PoolKey::project("p2", "fs"));
        assert_eq!(PoolKey::global("fs").scope_str(), "global");
        assert_eq!(PoolKey::project("p1", "fs").scope_str(), "project");
    }

    #[test]
    fn classify_only_transport_errors_as_connection_class() {
        let closed = classify_service_error(rmcp::service::ServiceError::TransportClosed);
        assert!(closed.is_connection_class());
        assert_eq!(closed.kind, McpErrorKind::Handshake);

        let cancelled = classify_service_error(rmcp::service::ServiceError::Cancelled {
            reason: Some("user".into()),
        });
        assert_eq!(cancelled.kind, McpErrorKind::Cancelled);
        assert!(!cancelled.is_connection_class(), "取消绝不重放");

        let timeout = classify_service_error(rmcp::service::ServiceError::Timeout {
            timeout: Duration::from_secs(1),
        });
        assert_eq!(timeout.kind, McpErrorKind::Call);
        assert!(!timeout.is_connection_class(), "超时可能已有副作用");
    }

    #[test]
    fn map_call_output_is_shape_deterministic() {
        // CallToolResult 是 non_exhaustive：用 serde 构造，避免依赖其私有字段
        let mk = |text: &str, err: Option<bool>| -> rmcp::model::CallToolResult {
            let mut v = json!({ "content": [{ "type": "text", "text": text }] });
            if let Some(e) = err {
                v["isError"] = json!(e);
            }
            serde_json::from_value(v).expect("CallToolResult 反序列化失败")
        };
        // 文本恰好是合法 JSON 也**不**改形状（旧实现会把它透出成对象）
        let a = map_call_output(mk("{\"a\":1}", None));
        assert_eq!(a.data, json!({ "text": "{\"a\":1}" }));
        assert!(!a.is_error);
        let b = map_call_output(mk("123", None));
        assert_eq!(b.data, json!({ "text": "123" }));
        let c = map_call_output(mk("boom", Some(true)));
        assert!(c.is_error);
        assert_eq!(c.data["isError"], true);
        assert_eq!(c.text, "boom");
    }

    /// `tools/list_changed` 的脏标记必须在 **run 边界**被取走。
    ///
    /// 这里用一个起不来的 server（状态 Error）验证「标记照样清掉」——
    /// 若只在就绪条目上清，坏连接的标记会永久残留、下次就绪时被误当成刚变更。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_boundary_clears_tool_list_dirty_flag() {
        let mgr = Arc::new(McpManager::default());
        let key = PoolKey::global("a");
        mgr.warm(
            "s",
            vec![(key.clone(), bogus_cfg())],
            reqwest::Client::new(),
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        {
            let mut inner = mgr.inner.lock().await;
            let e = inner.entries.get_mut(&key).expect("条目应已登记");
            e.dirty = Some(Arc::new(AtomicBool::new(true)));
        }
        let _ = mgr.tool_defs_for("s").await;
        let inner = mgr.inner.lock().await;
        let e = inner.entries.get(&key).expect("条目仍在");
        assert!(
            !e.dirty.as_ref().expect("脏标记存在").load(Ordering::SeqCst),
            "run 边界必须把 tools/list_changed 脏标记清掉"
        );
    }

    /// 图片内容必须以 `Content::Image` 进模型通道（旧实现只留 `[image ...]` 文本标记，
    /// 图片实际不可用）；文本块按原序保留。
    #[test]
    fn map_call_output_keeps_images_as_model_blocks() {
        let v = json!({
            "content": [
                { "type": "text", "text": "see this" },
                { "type": "image", "data": "QUFB", "mimeType": "image/png" },
            ]
        });
        let result: rmcp::model::CallToolResult = serde_json::from_value(v).unwrap();
        let out = map_call_output(result);
        // 前端投影里图片留可读标记；模型通道里是真正的 Image 块
        assert_eq!(
            out.text,
            format!("see this{}[image image/png]", char::from(10))
        );
        assert_eq!(out.blocks.len(), 2);
        assert!(matches!(&out.blocks[0], Content::Text { text } if text == "see this"));
        assert!(matches!(
            &out.blocks[1],
            Content::Image { media_type, data } if media_type == "image/png" && data == "QUFB"
        ));
    }

    #[test]
    fn call_rejects_tools_outside_session_visibility() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mgr = Arc::new(McpManager::default());
            // 会话 A 可见 fs；会话 B 完全看不到任何 MCP
            let visible = vec![(PoolKey::global("fs"), bogus_cfg())];
            mgr.warm("sA", visible, reqwest::Client::new());
            // 等 warm 的内部任务跑完
            tokio::time::sleep(Duration::from_millis(300)).await;
            let cancel = CancellationToken::new();
            let e = mgr
                .call("sB", "mcp__fs__read_file", json!({}), &cancel)
                .await
                .unwrap_err();
            assert_eq!(e.kind, McpErrorKind::Config);
            assert!(e.message.contains("不可见"), "{}", e.message);
            // 隔离的真正证据是可见集本身：A 登记了 fs，B 一条都没有
            assert_eq!(mgr.visible_keys("sA").await.len(), 1);
            assert!(mgr.visible_keys("sB").await.is_empty());
            // A 的可见集里有 fs，但它起不来 → 工具未进注册表，仍是「不可见」而非跨会话泄漏
            let e = mgr
                .call("sA", "mcp__fs__read_file", json!({}), &cancel)
                .await
                .unwrap_err();
            assert_eq!(e.kind, McpErrorKind::Config);
        });
    }

    #[test]
    fn release_session_reclaims_and_emits_stopped() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mgr = Arc::new(McpManager::default());
            let (sink, log) = collector();
            mgr.set_status_sink(sink).await;
            mgr.warm(
                "s1",
                vec![(PoolKey::global("a"), bogus_cfg())],
                reqwest::Client::new(),
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert_eq!(mgr.visible_keys("s1").await.len(), 1);
            mgr.release_session("s1").await;
            assert!(mgr.visible_keys("s1").await.is_empty());
            // 引用归零 → 发出 Stopped 事件（旧 stop_all 不发事件，UI 状态残留）
            let events = log.lock().unwrap().clone();
            assert!(
                events
                    .iter()
                    .any(|(s, p)| s == "s1" && p.state == McpState::Stopped),
                "应发出 Stopped 事件：{events:?}"
            );
        });
    }

    fn pid_alive(pid: &str) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(pid))
                .unwrap_or(false)
        }
        #[cfg(unix)]
        {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = pid;
            false
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnect_clears_pid_and_stops_stdio_process() {
        if !node_available() {
            return;
        }
        let mgr = Arc::new(McpManager::default());
        let key = PoolKey::global("test");
        mgr.warm("s", vec![(key.clone(), node_cfg())], reqwest::Client::new());
        let pid = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(p) = mgr
                    .status_for("s")
                    .await
                    .into_iter()
                    .find(|p| p.state == McpState::Ready)
                {
                    break p.pid.expect("stdio PID");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        mgr.disconnect(&key, "s").await;
        let status = mgr.status_for("s").await;
        assert_eq!(status[0].state, McpState::Stopped);
        assert_eq!(status[0].pid, None);
        assert_eq!(status[0].tools, 0);
        for _ in 0..20 {
            if !pid_alive(&pid.to_string()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("stdio 进程 {pid} 断开后仍在运行");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnect_rejects_late_ready_result() {
        if !node_available() {
            return;
        }
        let mgr = Arc::new(McpManager::default());
        let key = PoolKey::global("late");
        let cfg = node_cfg();
        let generation = {
            let mut inner = mgr.inner.lock().await;
            let mut entry = blank_entry(&cfg, mgr.tick());
            entry.generation = 1;
            entry.sessions.insert("s".to_string());
            inner.entries.insert(key.clone(), entry);
            inner
                .sessions
                .insert("s".to_string(), HashSet::from([key.clone()]));
            1
        };
        let ready = McpManager::spawn_connect(&key, &cfg, reqwest::Client::new())
            .await
            .expect("test server should connect");
        let pid = ready.pid.expect("stdio PID");
        mgr.disconnect(&key, "s").await;
        assert!(!mgr.set_ready(&key, ready, "s", generation).await);
        let status = mgr.status_for("s").await;
        assert_eq!(status[0].state, McpState::Stopped);
        assert_eq!(status[0].pid, None);
        for _ in 0..20 {
            if !pid_alive(&pid.to_string()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("迟到连接的进程 {pid} 未被回收");
    }

    #[test]
    fn repeated_open_close_cycles_leave_no_entries() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mgr = Arc::new(McpManager::default());
            for i in 0..5 {
                let s = format!("s{i}");
                mgr.warm(
                    &s,
                    vec![(PoolKey::global("a"), bogus_cfg())],
                    reqwest::Client::new(),
                );
                tokio::time::sleep(Duration::from_millis(120)).await;
                mgr.release_session(&s).await;
            }
            let all = mgr.all_status().await;
            assert!(
                all.iter().all(|p| p.state != McpState::Ready),
                "5 轮开关后不应残留就绪连接：{all:?}"
            );
        });
    }

    #[test]
    fn lru_evicts_only_unreferenced_and_emits_evicted() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mgr = Arc::new(McpManager::new(4));
            let (sink, log) = collector();
            mgr.set_status_sink(sink).await;
            for i in 0..4 {
                mgr.warm(
                    &format!("s{i}"),
                    vec![(PoolKey::global(format!("srv{i}")), bogus_cfg())],
                    reqwest::Client::new(),
                );
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            // 全部释放 → 引用归零，但条目仍在（Stopped）
            for i in 0..4 {
                mgr.release_session(&format!("s{i}")).await;
            }
            // 第 5 个条目触发淘汰
            mgr.warm(
                "sx",
                vec![(PoolKey::global("srv-new"), bogus_cfg())],
                reqwest::Client::new(),
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
            let events = log.lock().unwrap().clone();
            assert!(
                events.iter().any(|(_, p)| p.state == McpState::Evicted),
                "应发出 Evicted 事件：{events:?}"
            );
        });
    }

    #[test]
    fn referenced_entries_are_never_evicted() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mgr = Arc::new(McpManager::new(2));
            mgr.warm(
                "s0",
                vec![(PoolKey::global("srv0"), bogus_cfg())],
                reqwest::Client::new(),
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
            // 再塞 3 个新键：池上限 2，但 srv0 被引用，不得淘汰
            for i in 1..4 {
                mgr.warm(
                    &format!("s{i}"),
                    vec![(PoolKey::global(format!("srv{i}")), bogus_cfg())],
                    reqwest::Client::new(),
                );
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            let keys = mgr.visible_keys("s0").await;
            assert_eq!(keys, vec![PoolKey::global("srv0")]);
            let st = mgr.status_for("s0").await;
            assert_eq!(st.len(), 1);
            assert_ne!(st[0].state, McpState::Evicted, "有引用的条目不得被淘汰");
        });
    }

    #[test]
    fn pool_key_carries_project_identity() {
        let k = PoolKey::project("p", "fs");
        assert_eq!(k.project_id.as_deref(), Some("p"));
        assert_eq!(k.name, "fs");
        assert_eq!(PoolKey::global("fs").project_id, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_stdio_server_session_scoped_tools_and_call() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        let mgr = Arc::new(McpManager::default());
        let (sink, log) = collector();
        mgr.set_status_sink(sink).await;
        mgr.warm(
            "s-main",
            vec![(PoolKey::global("wavetest.proxy"), node_cfg())],
            reqwest::Client::new(),
        );
        for _ in 0..50 {
            if mgr
                .status_for("s-main")
                .await
                .iter()
                .any(|p| p.state == McpState::Ready)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let defs = mgr.tool_defs_for("s-main").await;
        let echo = defs
            .iter()
            .find(|d| d.function_name.ends_with("__echo"))
            .expect("应有 echo 工具");
        // server 名里的 `.` 必须归一化，否则 provider 400
        assert_eq!(echo.function_name, "mcp__wavetest_proxy__echo");
        assert_eq!(
            serde_json::from_str::<Value>(&echo.schema_json).unwrap()["additionalProperties"],
            json!(false)
        );
        // 另一个会话看不到它（跨会话隔离）
        assert!(mgr.tool_defs_for("s-other").await.is_empty());
        let cancel = CancellationToken::new();
        let out = mgr
            .call(
                "s-main",
                &echo.function_name,
                json!({ "text": "hello codewave" }),
                &cancel,
            )
            .await
            .expect("真实 echo 调用失败");
        assert_eq!(out.text, "echo: hello codewave");
        assert!(!out.is_error);
        assert_eq!(out.data, json!({ "text": "echo: hello codewave" }));
        let st = mgr.status_for("s-main").await;
        assert!(st[0].pid.is_some(), "stdio 连接应记录 PID");
        let events = log.lock().unwrap().clone();
        assert!(events.iter().any(|(_, p)| p.state == McpState::Starting));
        assert!(events.iter().any(|(_, p)| p.state == McpState::Ready));
        mgr.disconnect_all().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn call_can_be_cancelled() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        let mgr = Arc::new(McpManager::default());
        mgr.warm(
            "s",
            vec![(PoolKey::global("t"), node_cfg())],
            reqwest::Client::new(),
        );
        for _ in 0..50 {
            if mgr
                .status_for("s")
                .await
                .iter()
                .any(|p| p.state == McpState::Ready)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        let e = mgr
            .call("s", "mcp__t__echo", json!({ "text": "x" }), &cancel)
            .await
            .unwrap_err();
        assert_eq!(e.kind, McpErrorKind::Cancelled);
        mgr.disconnect_all().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_streamable_http_server_connect_list_call() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut child = tokio::process::Command::new("node")
            .arg(test_server_script())
            .arg("--http")
            .arg(port.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("node http server 启动失败");
        let mut up = false;
        for _ in 0..50 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                up = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(up, "测试 server 未在 5s 内监听");

        let mgr = Arc::new(McpManager::default());
        let cfg: McpServerConfig =
            serde_json::from_value(json!({ "url": format!("http://127.0.0.1:{port}/mcp") }))
                .unwrap();
        mgr.warm(
            "s",
            vec![(PoolKey::global("h"), cfg)],
            reqwest::Client::new(),
        );
        for _ in 0..50 {
            if mgr
                .status_for("s")
                .await
                .iter()
                .any(|p| p.state == McpState::Ready)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let defs = mgr.tool_defs_for("s").await;
        assert!(defs.iter().any(|d| d.function_name.ends_with("__add")));
        let cancel = CancellationToken::new();
        let out = mgr
            .call("s", "mcp__h__add", json!({ "a": 6, "b": 7 }), &cancel)
            .await
            .expect("真实 add 调用失败");
        assert_eq!(out.text, "13");
        let _ = child.kill().await;
        mgr.disconnect_all().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_connection_probes_without_joining_pool() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        let n = McpManager::test_connection(node_cfg(), reqwest::Client::new())
            .await
            .expect("临时测试连接应成功");
        assert!(n >= 2, "应发现 echo / add 两个工具，实际 {n}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_connection_reports_spawn_failure() {
        let e = McpManager::test_connection(bogus_cfg(), reqwest::Client::new())
            .await
            .unwrap_err();
        assert_eq!(e.kind, McpErrorKind::Spawn);
    }
}

/// 可见集与作用域查询（供 host / batch 层做「总是允许」写回）。
impl McpManager {
    /// 该 server 在会话可见集里的生效作用域。
    pub async fn server_scope(&self, session: &str, server: &str) -> Option<Scope> {
        let inner = self.inner.lock().await;
        let keys = inner.sessions.get(session)?.clone();
        keys.iter().find(|k| k.name == server).map(|k| k.scope)
    }
}

/// 决策 9 的硬验证：stdio server **自己 spawn 的孙进程**必须在连接回收时被连带杀掉。
///
/// 旧实现 `TokioChildProcess::new(Command)` 的 wrappers 表为空 → 没有 Job Object →
/// 只会 kill 直接子进程，孙进程变孤儿。这里用一个「包装进程」制造真实的三层结构：
/// 包装进程把真正的 MCP server 作为子进程继承 stdio，另起一个长驻孙进程并把 PID 写盘。
#[cfg(test)]
mod process_tree_tests {
    use super::*;
    use serde_json::json;

    /// 指定 PID 是否仍在运行。
    fn pid_alive(pid: &str) -> bool {
        #[cfg(windows)]
        {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output();
            match out {
                Ok(o) => String::from_utf8_lossy(&o.stdout).contains(pid),
                Err(_) => false,
            }
        }
        #[cfg(unix)]
        {
            std::process::Command::new("kill")
                .args(["-0", pid])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = pid;
            false
        }
    }

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn test_server_script() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/mcp-test-server.mjs")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn process_tree_is_reclaimed_on_session_release() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let wrapper = r#"
const { spawn } = require("node:child_process");
const fs = require("node:fs");
spawn("node", [process.env.CW_SERVER], { stdio: "inherit" });
const extra = spawn("node", ["-e", "setInterval(()=>{},1000)"]);
fs.writeFileSync(process.env.CW_PIDFILE, String(extra.pid));
setInterval(()=>{}, 1000);
"#;
        let cfg: McpServerConfig = serde_json::from_value(json!({
            "command": "node",
            "args": ["-e", wrapper],
            "env": {
                "CW_SERVER": test_server_script().to_string_lossy(),
                "CW_PIDFILE": pidfile.to_string_lossy(),
            },
        }))
        .unwrap();
        let mgr = Arc::new(McpManager::default());
        mgr.warm(
            "s",
            vec![(PoolKey::global("t"), cfg)],
            reqwest::Client::new(),
        );
        let mut ready = false;
        for _ in 0..80 {
            if mgr
                .status_for("s")
                .await
                .iter()
                .any(|p| p.state == McpState::Ready)
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "包装进程应能完成 MCP 握手");

        // 等孙进程 PID 落盘
        let mut gpid = String::new();
        for _ in 0..50 {
            if let Ok(t) = std::fs::read_to_string(&pidfile) {
                if !t.trim().is_empty() {
                    gpid = t.trim().to_string();
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!gpid.is_empty(), "孙进程 PID 未落盘");
        assert!(pid_alive(&gpid), "前提校验：孙进程此刻应在运行");

        // 回收：会话释放 → 引用归零 → 关连接 → Job Object 关闭 → 整棵树连带被杀
        mgr.release_session("s").await;
        let mut gone = false;
        for _ in 0..60 {
            if !pid_alive(&gpid) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "孙进程 {gpid} 未被连带回收（进程树泄漏）");
    }
}
