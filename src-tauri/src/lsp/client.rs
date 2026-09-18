//! LSP 客户端：进程 + stdio framing + 请求/通知通道 + 诊断缓存。
//!
//! 两条硬约束：
//! 1. **stderr 必须独立泵**（管道写满会把 server 卡死，`tools/service.rs` 的老教训）；
//! 2. **服务端主动请求必须应答**（未知也回 `null`）——忽略会让部分 server 卡住或静默降级。

use super::discovery::{self, ServerResolution};
use super::protocol::{self, FrameReader};
use super::server_spec::ServerSpec;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

/// 默认请求超时（参数化，握手另用 [`DEFAULT_INITIALIZE_TIMEOUT`]）。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 握手超时：jdtls/rust-analyzer 首次索引项目可能很慢，但不能无限等。
pub const DEFAULT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(120);

/// 退出时等待 server 自行退出的宽限。
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// 判定 [`Readiness::Warm`] 需要的最少推送次数。
///
/// 只数「推送次数」，不区分内容（空集占位一样计数）——它能证明的仅仅是「server 出过声」，
/// 所以 `Warm` **只给非空基线背书**（见 [`Readiness::ready_for`]）。
pub const MIN_PUBLISHES_FOR_WARM: u64 = 2;

/// server 实例的就绪证据（**实例级**，与具体文件无关）。
///
/// 用途：`publishDiagnostics` 的空集**本身不足以立信**——冷启动时 server 常见「先推空数组、
/// 随后才推真实诊断」，把空集当「文件本来就 0 错误」会让项目存量错误全被当成本次新增回喂。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// rust-analyzer 报 `experimental/serverStatus` 且 `quiescent == true`（索引完成，权威信号）
    Quiescent,
    /// 本实例已推送过**非空**诊断（任意文件）→ 确实在分析
    Analyzed,
    /// 距 `initialize` 成功已超该语言的预热窗（`ServerSpec::warmup_ms`，按语言分档），
    /// 且累计推送 ≥ [`MIN_PUBLISHES_FOR_WARM`] 次
    Warm,
    /// 尚无证据（冷启动：空集很可能是占位通告）
    None,
}

impl Readiness {
    /// 是否足以支撑「可信基线」（**空集基线**口径，最保守）。
    pub fn ready(self) -> bool {
        self.ready_for(true)
    }

    /// 该证据能否给这份基线背书（`empty_baseline` = 刚推来的诊断集为空）。
    ///
    /// **空集基线只认强证据**：`Quiescent`（server 亲口说索引完成）或 `Analyzed`（本实例推过
    /// **非空**诊断，证明它真的在分析）。`Warm`（预热窗已过 + 推过 ≥2 次）**只给非空基线背书**——
    /// 它只证明「时间够久且出过声」，而出声内容可能全是占位空集：全干净项目连推 N 次空集就能
    /// 凑出 `Warm`，此时把空集当「0 错误」会让 server 随后推的**项目存量错误**全部变成「本次新增」。
    ///
    /// 代价（**有意接受**）：全干净项目在首轮会被判「未就绪」而不是「通过」——宁可说未就绪，
    /// 也不谎报通过（`manager::baseline` 据此给 `reliable=false` → `Skipped{ServerNotReady}`）。
    pub fn ready_for(self, empty_baseline: bool) -> bool {
        match self {
            Readiness::Quiescent | Readiness::Analyzed => true,
            Readiness::Warm => !empty_baseline,
            Readiness::None => false,
        }
    }

    /// 日志用标签（定位真机问题时能看出是靠哪条证据立信的）。
    pub fn label(self) -> &'static str {
        match self {
            Readiness::Quiescent => "server-status-quiescent",
            Readiness::Analyzed => "non-empty-diagnostics",
            Readiness::Warm => "warmup-elapsed",
            Readiness::None => "no-evidence",
        }
    }
}

/// 共享的客户端内部状态（`Drop` 时兜底杀进程组，绝不留孤儿）。
struct Inner {
    /// 写入口：字节经通道交给专职 writer 任务（避免持锁跨 await）
    tx: mpsc::UnboundedSender<Vec<u8>>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    diagnostics: Mutex<HashMap<String, Vec<Value>>>,
    revisions: Mutex<HashMap<String, u64>>,
    versions: Mutex<HashMap<String, i64>>,
    alive: AtomicBool,
    pid: Option<u32>,
    root: PathBuf,
    label: String,
    request_timeout_ms: AtomicU64,
    initialize_timeout_ms: AtomicU64,
    /// `initialize` 握手成功的时刻（预热期从此刻起算）
    initialized_at: Mutex<Option<Instant>>,
    /// 本实例的那档预热窗（毫秒；来自 `ServerSpec::warmup_ms`，测试可另指定）
    warmup_ms: AtomicU64,
    /// 本实例是否推送过非空诊断（任意文件）
    saw_non_empty: AtomicBool,
    /// rust-analyzer 是否报过 `experimental/serverStatus` 且 `quiescent == true`
    quiescent: AtomicBool,
    /// 本实例累计收到的诊断推送次数（任意文件）
    publish_count: AtomicU64,
    /// 本实例是否已因「就绪证据不足」降级过一次（只报一条 warn，见 [`LspClient::note_degraded_once`]）
    degraded_warned: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.alive.load(Ordering::SeqCst) {
            let _ = kill_process_group(self.pid, &self.label);
        }
    }
}

impl Inner {
    /// 置死 + 唤醒全部等待者（等待者不能干等到超时）。
    fn deadify(&self, reason: &str) {
        if self.alive.swap(false, Ordering::SeqCst) {
            tracing::debug!(server = %self.label, reason, "LSP server 判定为已退出");
        }
        self.fail_pending(reason);
    }

    /// 把所有挂起请求立即失败掉。
    fn fail_pending(&self, reason: &str) {
        let mut pending = self.pending.lock().unwrap();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(format!("LSP server 已退出：{reason}")));
        }
    }

    /// 发一条消息（同步、非阻塞：写盘由 writer 任务负责）。
    fn send(&self, v: &Value) -> Result<(), String> {
        self.tx
            .send(protocol::encode_message(v))
            .map_err(|_| "LSP 写入通道已关闭（server 已退出）".to_string())
    }

    /// 分发一条入站消息。
    fn dispatch(&self, msg: &Value) {
        let id = msg.get("id");
        let method = msg.get("method").and_then(|m| m.as_str());
        match (id, method) {
            // 服务端主动请求：**必须应答**（未知也回 null）
            (Some(id), Some(method)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let result = self.handle_server_request(method, &params);
                let _ = self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
            }
            // 我们的请求的响应：唤醒等待者
            (Some(id), None) => {
                let Some(key) = id.as_i64() else {
                    tracing::debug!(server = %self.label, "响应 id 非数字，忽略");
                    return;
                };
                let waiter = self.pending.lock().unwrap().remove(&key);
                match waiter {
                    Some(tx) => {
                        let res = match msg.get("error") {
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                            Some(err) => {
                                let text = match err.get("message").and_then(|m| m.as_str()) {
                                    Some(m) => m.to_string(),
                                    None => err.to_string(),
                                };
                                Err(format!("server 返回错误：{text}"))
                            }
                        };
                        let _ = tx.send(res);
                    }
                    None => tracing::debug!(server = %self.label, id = key, "收到无人等待的响应"),
                }
            }
            // 通知
            (None, Some(method)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                if method == "textDocument/publishDiagnostics" {
                    self.cache_diagnostics(&params);
                } else if method == "experimental/serverStatus" {
                    // rust-analyzer 的权威就绪信号（通知形态；请求形态见 handle_server_request）
                    self.note_server_status(&params);
                } else if method == "window/logMessage" || method == "$/progress" {
                    tracing::debug!(server = %self.label, method, "LSP 通知");
                } else {
                    tracing::debug!(server = %self.label, method, "忽略 LSP 通知");
                }
            }
            (None, None) => {}
        }
    }

    /// 服务端主动请求的应答表。
    fn handle_server_request(&self, method: &str, params: &Value) -> Value {
        match method {
            // 每项回一个空配置对象（长度必须与 items 对齐，否则 server 可能错位取值）
            "workspace/configuration" => {
                let n = params
                    .get("items")
                    .and_then(|i| i.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                Value::Array(vec![json!({}); n])
            }
            "workspace/workspaceFolders" => {
                let uri = path_to_uri(&self.root);
                let name = self
                    .root
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.root.to_string_lossy().to_string());
                json!([{ "uri": uri, "name": name }])
            }
            "experimental/serverStatus" => {
                // 部分 server 版本把它作为**请求**发来：同样记就绪证据，并回 null 应答
                self.note_server_status(params);
                Value::Null
            }
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create" => Value::Null,
            "window/showMessageRequest" => {
                tracing::debug!(
                    server = %self.label,
                    message = params.get("message").and_then(|m| m.as_str()).unwrap_or(""),
                    "LSP server 请求展示消息（自动选「知道了」）"
                );
                Value::Null
            }
            other => {
                tracing::debug!(server = %self.label, method = other, "应答未知服务端请求 → null");
                Value::Null
            }
        }
    }

    /// 缓存 `publishDiagnostics`（同时推进 revision，供同步窗口轮询判定「新诊断到达」）。
    ///
    /// 键统一用[归一化](normalize_uri_key)后的字符串：server 回的 `file:///d%3A/...` 与我们的
    /// `D:\...` 必须命中同一个槽位，否则诊断会凭空消失（写错代码却收到「通过」）。
    fn cache_diagnostics(&self, params: &Value) {
        let Some(uri) = params.get("uri").and_then(|u| u.as_str()) else {
            return;
        };
        let items = params
            .get("diagnostics")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        let key = normalize_uri_key(uri);
        // 就绪证据：非空推送证明 server 确实在分析（空集不做任何承诺）
        self.publish_count.fetch_add(1, Ordering::SeqCst);
        if !items.is_empty() {
            self.saw_non_empty.store(true, Ordering::SeqCst);
        }
        if !self.versions.lock().unwrap().contains_key(&key) {
            tracing::debug!(
                server = %self.label,
                uri,
                key = %key,
                "收到未映射到已知文件的诊断推送（uri 已在归一化后比对）"
            );
        }
        self.diagnostics.lock().unwrap().insert(key.clone(), items);
        *self.revisions.lock().unwrap().entry(key).or_insert(0) += 1;
    }

    /// 记录 rust-analyzer 的 `experimental/serverStatus`（`quiescent == true` = 索引完成）。
    fn note_server_status(&self, params: &Value) {
        let quiescent = params
            .get("quiescent")
            .and_then(|q| q.as_bool())
            .unwrap_or(false);
        if !quiescent {
            return;
        }
        if !self.quiescent.swap(true, Ordering::SeqCst) {
            tracing::debug!(server = %self.label, "server 报告 quiescent：就绪证据已获得");
        }
    }

    /// 推进文档版本号（Full 同步下仍必须递增，server 用它丢弃乱序变更）。
    ///
    /// `key` 是[归一化](normalize_path_key)后的文档键（不是 URI 原文）。
    fn bump_version(&self, key: &str) -> i64 {
        let mut versions = self.versions.lock().unwrap();
        let entry = versions.entry(key.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// 当前请求超时。
    fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms.load(Ordering::Relaxed))
    }

    /// 当前握手超时。
    fn initialize_timeout(&self) -> Duration {
        Duration::from_millis(self.initialize_timeout_ms.load(Ordering::Relaxed))
    }

    /// 记录握手完成（就绪判定的预热期起点）。
    fn mark_initialized(&self) {
        *self.initialized_at.lock().unwrap() = Some(Instant::now());
    }

    /// 当前的就绪证据（取最强的一条；无证据 = 空集不得立信）。
    fn readiness(&self) -> Readiness {
        if self.quiescent.load(Ordering::SeqCst) {
            return Readiness::Quiescent;
        }
        if self.saw_non_empty.load(Ordering::SeqCst) {
            return Readiness::Analyzed;
        }
        let started = *self.initialized_at.lock().unwrap();
        if let Some(t) = started {
            if t.elapsed() >= Duration::from_millis(self.warmup_ms.load(Ordering::Relaxed))
                && self.publish_count.load(Ordering::SeqCst) >= MIN_PUBLISHES_FOR_WARM
            {
                return Readiness::Warm;
            }
        }
        Readiness::None
    }

    /// 首次降级（就绪证据不足）报一次 warn：同实例只报一次，绝不刷屏。
    /// **不**改 `ServerStatus` 字段（那是前后端契约）。
    fn note_degraded_once(&self) -> bool {
        !self.degraded_warned.swap(true, Ordering::SeqCst)
    }
}

/// 项目级 LSP 客户端句柄。
pub struct LspClient {
    inner: Arc<Inner>,
}

impl LspClient {
    /// 按 server 静态描述启动（程序自己从 PATH 解析；pool 一般用 [`Self::spawn_resolved`]）。
    pub async fn spawn(
        spec: &ServerSpec,
        root: &Path,
        env_extra: &[(String, String)],
        data_dir: Option<&Path>,
    ) -> Result<Arc<LspClient>, String> {
        let path = env_extra
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
        let program = discovery::which_in(spec.server_name, &path)
            .unwrap_or_else(|| PathBuf::from(spec.server_name));
        let mut args = spec.args.clone();
        args.extend(java_data_args(spec.lang, data_dir));
        Self::launch(program, args, root, env_extra, spec.warmup_ms).await
    }

    /// 按已解析的结论启动（保留发现阶段选中的 program/args，优先级语义才不丢）。
    ///
    /// 追加接口：pool 用它，避免「解析出的 program」与「再查一次 PATH」不一致。
    pub async fn spawn_resolved(
        res: &ServerResolution,
        spec: &ServerSpec,
        root: &Path,
        env_extra: &[(String, String)],
        data_dir: Option<&Path>,
    ) -> Result<Arc<LspClient>, String> {
        let mut args = res.args.clone();
        args.extend(java_data_args(spec.lang, data_dir));
        Self::launch(res.program.clone(), args, root, env_extra, spec.warmup_ms).await
    }

    /// 真正的启动：piped stdio + stderr 泵 + 读循环 + 退出监听。
    ///
    /// `warmup_ms` 是该语言的预热档位（`ServerSpec::warmup_ms`）——就绪判据 `Warm` 取它，
    /// **不再是统一的 3s**。
    async fn launch(
        program: PathBuf,
        args: Vec<String>,
        root: &Path,
        env_extra: &[(String, String)],
        warmup_ms: u64,
    ) -> Result<Arc<LspClient>, String> {
        let label = program
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| program.to_string_lossy().to_string());
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args)
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in env_extra {
            cmd.env(k, v);
        }
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 LSP server（{}）失败：{e}", program.display()))?;
        let pid = child.id();
        let Some(stdin) = child.stdin.take() else {
            let _ = kill_process_group(pid, &label);
            return Err("无法取得 LSP server 的 stdin".into());
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = kill_process_group(pid, &label);
            return Err("无法取得 LSP server 的 stdout".into());
        };
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let inner = Arc::new(Inner {
            tx,
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(HashMap::new()),
            revisions: Mutex::new(HashMap::new()),
            versions: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            pid,
            root: root.to_path_buf(),
            label: label.clone(),
            request_timeout_ms: AtomicU64::new(DEFAULT_REQUEST_TIMEOUT.as_millis() as u64),
            initialize_timeout_ms: AtomicU64::new(DEFAULT_INITIALIZE_TIMEOUT.as_millis() as u64),
            initialized_at: Mutex::new(None),
            warmup_ms: AtomicU64::new(warmup_ms),
            saw_non_empty: AtomicBool::new(false),
            quiescent: AtomicBool::new(false),
            publish_count: AtomicU64::new(0),
            degraded_warned: AtomicBool::new(false),
        });

        let weak_writer = Arc::downgrade(&inner);
        tokio::spawn(write_loop(stdin, rx, weak_writer, label.clone()));
        if let Some(err) = stderr {
            tokio::spawn(pump_stderr(err, label.clone()));
        }
        let weak_read = Arc::downgrade(&inner);
        tokio::spawn(read_loop(stdout, weak_read));
        // 退出监听：回收僵尸 + 置死（等待者立即失败）
        let weak_wait = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let status = child.wait().await;
            if let Some(inner) = weak_wait.upgrade() {
                inner.deadify(&format!(
                    "进程退出（{}）",
                    status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|e| e.to_string())
                ));
            }
        });

        Ok(Arc::new(LspClient { inner }))
    }

    /// 覆盖请求/握手超时（测试与特殊 server 用）。
    pub fn set_timeouts(&self, request: Duration, initialize: Duration) {
        self.inner
            .request_timeout_ms
            .store(request.as_millis() as u64, Ordering::Relaxed);
        self.inner
            .initialize_timeout_ms
            .store(initialize.as_millis() as u64, Ordering::Relaxed);
    }

    /// 发请求并等响应（默认超时）。
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let timeout = self.inner.request_timeout();
        self.request_with_timeout(method, params, timeout).await
    }

    /// 发请求并等响应（指定超时）。
    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if !self.is_alive() {
            return Err("LSP server 已退出".into());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        self.inner.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(format!("等待 {method} 响应时通道被丢弃（server 已退出）")),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(format!("{method} 超时（{} ms）", timeout.as_millis()))
            }
        }
    }

    /// 发通知（不等待、失败只记日志）。
    pub async fn notify(&self, method: &str, params: Value) {
        if let Err(e) = self.inner.send(&json!({
            "jsonrpc": "2.0", "method": method, "params": params
        })) {
            tracing::debug!(method, error = %e, "LSP 通知发送失败");
        }
    }

    /// 握手：`initialize` + `initialized`。
    pub async fn initialize(&self, root: &Path, init_options: Value) -> Result<Value, String> {
        let root_uri = path_to_uri(root);
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| root.to_string_lossy().to_string());
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "CodeWave", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{ "uri": root_uri, "name": name }],
            "capabilities": capabilities_json(),
            "initializationOptions": init_options,
        });
        let timeout = self.inner.initialize_timeout();
        let res = self
            .request_with_timeout("initialize", params, timeout)
            .await
            .map_err(|e| format!("LSP 握手失败：{e}"))?;
        self.inner.mark_initialized();
        self.notify("initialized", json!({})).await;
        Ok(res)
    }

    /// 打开文档（Full 同步，version 从 1 开始）。
    ///
    /// 发给 server 的是[规范 URI](path_to_uri)；内部版本表按[归一化键](normalize_path_key)记，
    /// 保证 server 回的 uri 写法差异（盘符大小写/编码）也能命中同一份文档。
    pub async fn did_open(&self, path: &Path, text: &str, language_id: &str) {
        let uri = path_to_uri(path);
        let version = self.inner.bump_version(&normalize_path_key(path));
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": language_id, "version": version, "text": text
            }}),
        )
        .await;
    }

    /// 全量变更文档（`textDocumentSyncKind=1` 语义：整篇替换，version 自增）。
    pub async fn did_change(&self, path: &Path, text: &str) {
        let uri = path_to_uri(path);
        let version = self.inner.bump_version(&normalize_path_key(path));
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }),
        )
        .await;
    }

    /// 读该文件最近一次 `publishDiagnostics` 的原始诊断数组。
    pub fn diagnostics_for(&self, path: &Path) -> Vec<Value> {
        let key = normalize_path_key(path);
        self.inner
            .diagnostics
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default()
    }

    /// 该文件的诊断 revision（每次 `publishDiagnostics` 自增；轮询「新诊断是否到达」用）。
    pub fn diagnostics_revision(&self, path: &Path) -> u64 {
        let key = normalize_path_key(path);
        self.inner
            .revisions
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(0)
    }

    /// 当前的就绪证据（基线可信的实例级前提，见 [`Readiness`]）。
    pub fn readiness(&self) -> Readiness {
        self.inner.readiness()
    }

    /// 该实例是否**首次**因「就绪证据不足」降级（`true` = 首次，调用方可据此报一条 warn）。
    ///
    /// 同一实例只返回一次 `true`：不刷屏，也不动 `ServerStatus` 字段（那是前后端契约）。
    pub fn note_degraded_once(&self) -> bool {
        self.inner.note_degraded_once()
    }

    /// 进程是否还活着（含「读循环已断/进程已退出」的判定）。
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// 子进程 PID（诊断与测试用）。
    pub fn pid(&self) -> Option<u32> {
        self.inner.pid
    }

    /// 展示名（日志/状态用）。
    pub fn label(&self) -> &str {
        &self.inner.label
    }

    /// 优雅退出：`shutdown` → `exit` → 等 ≤3s → 杀进程组。
    pub async fn shutdown(&self) {
        if self.inner.alive.load(Ordering::SeqCst) {
            let _ = self
                .request_with_timeout("shutdown", Value::Null, SHUTDOWN_GRACE)
                .await;
            self.notify("exit", Value::Null).await;
            let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
            while self.inner.alive.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        if self.inner.alive.swap(false, Ordering::SeqCst) {
            let _ = kill_process_group(self.inner.pid, &self.inner.label);
        }
    }
}

/// 专职 writer：独占 stdin，把通道里的字节顺序写出。
async fn write_loop(
    mut stdin: ChildStdin,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    weak: Weak<Inner>,
    label: String,
) {
    while let Some(bytes) = rx.recv().await {
        if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
            tracing::debug!(server = %label, "LSP stdin 写入失败");
            if let Some(inner) = weak.upgrade() {
                inner.deadify("stdin 写入失败");
            }
            return;
        }
    }
}

/// stderr 独立泵：**必须读走**，否则管道写满会把 server 卡死。
async fn pump_stderr(mut err: ChildStderr, label: String) {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    loop {
        match err.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                tracing::debug!(server = %label, "{}", text.trim_end());
            }
        }
    }
}

/// stdout 读循环：分片安全的 framing → 分发。
async fn read_loop(mut stdout: ChildStdout, weak: Weak<Inner>) {
    let mut frame = FrameReader::new();
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        match protocol::read_message(&mut stdout, &mut frame).await {
            Ok(Some(msg)) => inner.dispatch(&msg),
            Ok(None) => {
                inner.deadify("server 关闭了 stdout");
                return;
            }
            Err(e) => {
                tracing::debug!(server = %inner.label, error = %e, "LSP 读循环结束");
                inner.deadify(&e.to_string());
                return;
            }
        }
    }
}

/// Java 的 `-data <项目数据目录>/lsp/java`（jdtls 的工作区必须可写且随项目走）。
fn java_data_args(lang: super::Lang, data_dir: Option<&Path>) -> Vec<String> {
    match (lang, data_dir) {
        (super::Lang::Java, Some(dir)) => vec![
            "-data".to_string(),
            dir.join("lsp").join("java").to_string_lossy().to_string(),
        ],
        _ => Vec::new(),
    }
}

/// 客户端能力声明：三个硬要求（配置、工作区文件夹、诊断）必须为 true。
fn capabilities_json() -> Value {
    use lsp_types::{
        ClientCapabilities, PublishDiagnosticsClientCapabilities, TextDocumentClientCapabilities,
        WorkspaceClientCapabilities,
    };
    let caps = ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            workspace_folders: Some(true),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                version_support: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut v = serde_json::to_value(&caps).unwrap_or_else(|_| json!({}));
    // lsp-types 各版本的 serde 命名差异兜底：三个硬要求必须落到 LSP 规定的键位上
    if !v.is_object() {
        v = json!({});
    }
    {
        let obj = v.as_object_mut().expect("已确保是对象");
        let workspace = obj
            .entry("workspace")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("workspace 是对象");
        workspace.insert("configuration".into(), json!(true));
        workspace.insert("workspaceFolders".into(), json!(true));
        let text_document = obj
            .entry("textDocument")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("textDocument 是对象");
        let publish = text_document
            .entry("publishDiagnostics")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("publishDiagnostics 是对象");
        publish.insert("relatedInformation".into(), json!(true));
        publish.insert("versionSupport".into(), json!(true));
    }
    v
}

/// 强杀进程组（进程树）：Windows 走 taskkill /T /F，Unix 走 kill(-pid)（照抄 `tools/service.rs`）。
pub fn kill_process_group(pid: Option<u32>, label: &str) -> std::io::Result<()> {
    let Some(pid) = pid.filter(|p| *p > 1) else {
        return Ok(());
    };
    tracing::debug!(server = %label, pid, "终止 LSP server 进程组");
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/T", "/F", "/PID", &pid.to_string()]);
        cmd.creation_flags(0x0800_0000);
        cmd.output().map(|_| ())
    }
    #[cfg(unix)]
    {
        // SAFETY: kill 是幂等的信号投递；pid 已校验 > 1（不会波及自身进程组或全体进程）
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        Ok(())
    }
}

/// `Path` → `file://` URI（Windows 盘符按 VSCode 惯例编码为 `%3A`）。
pub fn path_to_uri(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = raw.strip_prefix("//") {
        // UNC：\\server\share\x → file://server/share/x
        return format!("file://{}", encode_uri_path(rest));
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("file:///{drive}%3A{}", encode_uri_path(&raw[2..]));
    }
    if raw.starts_with('/') {
        format!("file://{}", encode_uri_path(&raw))
    } else {
        format!("file:///{}", encode_uri_path(&raw))
    }
}

/// `file://` URI → `Path`（非 file scheme 返回 None）。
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest)?;
    #[cfg(windows)]
    {
        let trimmed = decoded.trim_start_matches('/');
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            let tail = trimmed[2..].trim_start_matches('/').replace('/', "\\");
            let joined = if tail.is_empty() {
                format!("{drive}:\\")
            } else {
                format!("{drive}:\\{tail}")
            };
            return Some(PathBuf::from(joined));
        }
        if decoded.starts_with("//") {
            // UNC（带前导斜杠的写法）
            return Some(PathBuf::from(decoded.replace('/', "\\")));
        }
        if !decoded.starts_with('/') {
            // UNC 主机形态：file://server/share/x（VSCode 就是这种写法）
            return Some(PathBuf::from(format!("\\\\{}", decoded.replace('/', "\\"))));
        }
        Some(PathBuf::from(decoded))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from(decoded))
    }
}

/// 诊断缓存与文档版本表的**归一化键**：URI 写法与本地路径写法落到同一字符串。
///
/// 两侧都得用它——server 回的 `file:///d%3A/Work/a%20b/x.ts` 与我们的 `D:\Work\a b\x.ts`、
/// 盘符大小写、`\` 与 `/`、尾斜杠，任一差异都不该让诊断凭空消失（LSP 客户端最经典的坑，
/// 症状就是「写了错代码却收到通过」）。
///
/// 步骤：去 `file://` 前缀 → 百分号解码 → 分隔符/盘符归一 → 去尾斜杠 → `canonicalize` 兜底。
pub(crate) fn normalize_uri_key(uri: &str) -> String {
    let stripped = strip_file_scheme(uri);
    let decoded = percent_decode(stripped).unwrap_or_else(|| stripped.to_string());
    canonicalize_key(&normalize_path_text(&decoded))
}

/// 本地路径 → 同一个键（与 [`normalize_uri_key`] 成对使用）。
///
/// 路径侧**不做**百分号解码：`a%20b` 是合法目录名，URI 侧编码为 `%2520`，解一次正好还原。
pub(crate) fn normalize_path_key(path: &Path) -> String {
    canonicalize_key(&normalize_path_text(&path.to_string_lossy()))
}

/// 去掉 `file:` 前缀（大小写不敏感；非 file scheme 原样返回）。
///
/// 兼容面**必须宽**：各家 server 回的写法不一，认窄了诊断会**静默消失**（写了错代码却收到「通过」）。
/// 三种变体都归一：
/// - `file:///d%3A/Work/x.ts`（规范三斜杠）；
/// - `file://localhost/d%3A/Work/x.ts`（带 authority，丢弃 `localhost`）；
/// - `file:/d%3A/Work/x.ts`（单斜杠，部分 server 这么发）。
///
/// 实现：先吃掉 `file:` 后**任意个** `/`，再丢 `localhost` authority（大小写不敏感）。
/// UNC 形态（`file://server/share/x`）不受影响：吃完斜杠后剩下的 `server/...` 会在
/// [`strip_localhost`] 里因不叫 localhost 而原样保留。
fn strip_file_scheme(raw: &str) -> &str {
    match raw.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("file:") => {}
        _ => return raw,
    }
    let rest = raw[5..].trim_start_matches('/');
    strip_localhost(rest).unwrap_or(rest)
}

/// 丢掉 `file://localhost/…` 的 authority（大小写不敏感）。
///
/// 只认第一段是 `localhost` 且后面紧跟 `/`（或到此为止）的情形；`server/share/x` 这类
/// 真实 UNC authority 原样返回（`None`）。
fn strip_localhost(rest: &str) -> Option<&str> {
    let (head, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    head.eq_ignore_ascii_case("localhost").then_some(tail)
}

/// 文本归一：分隔符统一为 `/` → 去盘符前的多余斜杠 → 去尾斜杠（盘根 `d:/` → `d:`）。
fn normalize_path_text(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/");
    // `/d:/…`（`file:///d%3A/…` 解码后的形态）→ `d:/…`
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic() {
        s = s[1..].to_string();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// 落盘兜底：能 `canonicalize` 就用系统权威结论（真实大小写、8.3 短名、junction 一并归一）。
///
/// 目标不存在时回落文本形态（调用两侧在同一前提下仍能对上：文件刚写入过，两侧都能 canonicalize）。
fn canonicalize_key(text: &str) -> String {
    match std::fs::canonicalize(PathBuf::from(text)) {
        Ok(c) => {
            let s = c.to_string_lossy().to_string();
            // Windows 的 verbatim 前缀（`\\?\D:\…`）在两侧不会同时出现，剔掉更可比
            let s = s.strip_prefix(r"\\?\").map(|r| r.to_string()).unwrap_or(s);
            platform_fold(&normalize_path_text(&s))
        }
        Err(_) => platform_fold(text),
    }
}

/// Windows 路径大小写不敏感 → 键统一折叠为 ASCII 小写（中文路径无大小写，`to_ascii_lowercase` 不改字节长度）。
fn platform_fold(s: &str) -> String {
    if cfg!(windows) {
        s.to_ascii_lowercase()
    } else {
        s.to_string()
    }
}

/// 百分号编码：只放行 URI 路径里的 unreserved 字符与 `/`。
fn encode_uri_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '/') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 百分号解码（非法转义保留原样；UTF-8 非法序列按 lossy 处理）。
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            match std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                Some(b) => {
                    out.push(b);
                    i += 3;
                    continue;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip_plain() {
        let p = PathBuf::from(r"D:\Work\proj\src\index.ts");
        let uri = path_to_uri(&p);
        assert_eq!(uri, "file:///d%3A/Work/proj/src/index.ts");
        #[cfg(windows)]
        assert_eq!(uri_to_path(&uri).as_deref(), Some(p.as_path()));
    }

    #[test]
    fn uri_roundtrip_chinese_and_spaces() {
        let p = PathBuf::from(r"D:\Work\中文 目录\x.ts");
        let uri = path_to_uri(&p);
        assert!(uri.starts_with("file:///d%3A/"), "{uri}");
        assert!(
            uri.contains("%E4%B8%AD%E6%96%87"),
            "中文必须百分号编码：{uri}"
        );
        assert!(uri.contains("%20"), "空格必须百分号编码：{uri}");
        #[cfg(windows)]
        assert_eq!(uri_to_path(&uri).as_deref(), Some(p.as_path()));
        #[cfg(not(windows))]
        assert!(uri_to_path(&uri).is_some());
    }

    #[test]
    fn uri_roundtrip_drive_root() {
        let uri = path_to_uri(Path::new("D:\\"));
        assert_eq!(uri, "file:///d%3A/");
        #[cfg(windows)]
        assert_eq!(uri_to_path(&uri).as_deref(), Some(Path::new("D:\\")));
    }

    #[test]
    fn uri_roundtrip_unc() {
        let p = PathBuf::from(r"\\server\share\dir\a.ts");
        let uri = path_to_uri(&p);
        assert_eq!(uri, "file://server/share/dir/a.ts");
        #[cfg(windows)]
        assert_eq!(uri_to_path(&uri).as_deref(), Some(p.as_path()));
    }

    #[test]
    fn uri_to_path_rejects_non_file_scheme() {
        assert!(uri_to_path("https://example.com/a").is_none());
        assert!(uri_to_path("untitled:Untitled-1").is_none());
    }

    #[test]
    fn uri_roundtrip_posix_path_text() {
        let uri = path_to_uri(Path::new("/home/me/项目/a.rs"));
        assert!(uri.starts_with("file:///home/me/"), "{uri}");
        assert!(uri.contains("%E9%A1%B9%E7%9B%AE"), "{uri}");
        assert!(uri_to_path(&uri).is_some());
    }

    // ---- 🔴-3：URI ↔ 路径归一化（诊断不能因写法差异凭空消失） ----

    /// 两个归一化键在**本平台**是否等价。
    ///
    /// Windows 折叠大小写（`platform_fold`），用例里的 `D:` / `d:` 两种写法在那边天然相等；
    /// Unix 保留大小写（ext4 上更是如此），故在 Unix 侧按大小写不敏感比——它们表达的仍是
    /// 「同一路径的两种写法」。不这么做，下面几个用例在 macOS/Linux 上必红（CI 矩阵含两者）。
    fn keys_equivalent(a: &str, b: &str) -> bool {
        if cfg!(windows) {
            a == b
        } else {
            a.eq_ignore_ascii_case(b)
        }
    }

    #[test]
    fn uri_and_path_keys_match_for_drive_encoding_and_spaces() {
        // ① `file:///d%3A/Work/a%20b/x.ts` ↔ `D:\Work\a b\x.ts`（不存在 → 走文本归一兜底）
        let uri = "file:///d%3A/Work/a%20b/x.ts";
        let path = PathBuf::from(r"D:\Work\a b\x.ts");
        let (uk, pk) = (normalize_uri_key(uri), normalize_path_key(&path));
        assert!(keys_equivalent(&uk, &pk), "{uk} != {pk}");
    }

    #[test]
    fn uri_and_path_keys_match_for_real_files() {
        // 真实存在的文件：两侧都能 canonicalize，结论必须一致（含中文目录）
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("中文 目录");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.ts");
        std::fs::write(&file, "x").unwrap();
        let uri = path_to_uri(&file);
        assert!(uri.contains("%E4%B8%AD%E6%96%87"), "{uri}");
        let (uk, pk) = (normalize_uri_key(&uri), normalize_path_key(&file));
        assert!(keys_equivalent(&uk, &pk), "{uk} != {pk}");
        // ③ 大小写/写法差异也要落到同一键（Unix 上大小写敏感的 FS 里 canonicalize 会失手，
        // 结论仍应大小写等价）
        let upper = normalize_uri_key(&uri.to_ascii_uppercase());
        assert!(keys_equivalent(&upper, &pk), "{upper} != {pk}");
    }

    #[test]
    fn normalization_folds_drive_case_and_separators() {
        // ② 盘符大小写 + 斜杠方向差异
        let a = normalize_path_key(Path::new(r"D:\Work\proj\src\index.ts"));
        let b = normalize_path_key(Path::new("d:/work/proj/src/index.ts"));
        assert!(keys_equivalent(&a, &b), "{a} != {b}");
        let c = normalize_uri_key("file:///D%3A/Work/proj/src/index.ts/");
        assert!(
            keys_equivalent(&a, &c),
            "{a} != {c}（尾斜杠 + 大写盘符必须归一）"
        );
    }

    #[test]
    fn unmapped_uri_is_cached_without_touching_other_files() {
        // ④ 未映射 uri：不 panic、不误挂到别的文件
        let inner = test_inner();
        inner.dispatch(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///c%3A/elsewhere/other.ts",
                "diagnostics": [{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }, "message": "boom" }]
            }
        }));
        let key = normalize_path_key(Path::new(r"D:\Work\proj\src\mine.ts"));
        assert!(!inner.diagnostics.lock().unwrap().contains_key(&key));
        assert_eq!(*inner.revisions.lock().unwrap().get(&key).unwrap_or(&0), 0);
        assert_eq!(inner.publish_count.load(Ordering::SeqCst), 1);
    }

    // ---- 🔴-2：就绪证据（空集不得立信） ----

    #[test]
    fn empty_publish_alone_is_not_readiness() {
        let inner = test_inner();
        push_diags(&inner, r"D:\Work\proj\a.ts", &[]);
        assert_eq!(inner.readiness(), Readiness::None, "单次空集推送不得立信");
    }

    #[test]
    fn non_empty_publish_marks_analyzed() {
        let inner = test_inner();
        push_diags(&inner, r"D:\Work\proj\a.ts", &[json!({ "message": "e" })]);
        assert_eq!(inner.readiness(), Readiness::Analyzed);
    }

    #[test]
    fn warmup_needs_period_and_two_publishes() {
        let inner = test_inner();
        *inner.initialized_at.lock().unwrap() = Some(Instant::now());
        push_diags(&inner, r"D:\Work\proj\a.ts", &[]);
        push_diags(&inner, r"D:\Work\proj\b.ts", &[]);
        assert_eq!(inner.readiness(), Readiness::None, "预热期未过不得立信");
        // 预热期已过 + 已推 ≥2 次 → Warm（窗口取本实例的档位，不再是统一常量）
        let warmup = Duration::from_millis(inner.warmup_ms.load(Ordering::Relaxed));
        *inner.initialized_at.lock().unwrap() = Some(Instant::now() - warmup * 2);
        assert_eq!(inner.readiness(), Readiness::Warm);
        // 但空集基线不认 Warm：全干净项目「连推 N 次空集」不得变成「通过」
        assert!(!inner.readiness().ready(), "Warm 不得给空集基线背书");
        assert!(!inner.readiness().ready_for(true));
        assert!(inner.readiness().ready_for(false));
    }

    /// 返工验收：连推**多次空集**跨过预热窗后，证据档位到 `Warm`，但空集基线**仍不可信**。
    /// （旧实现里 `Warm` 单独为空集背书 → 第三个文件的空集基线被标 reliable → server 随后推的
    /// 项目存量错误全被当「本次新增」回喂。）
    #[test]
    fn warm_evidence_does_not_back_an_empty_baseline() {
        let inner = test_inner();
        *inner.initialized_at.lock().unwrap() = Some(Instant::now());
        for i in 0..3 {
            push_diags(&inner, &format!(r"D:\Work\proj\f{i}.ts"), &[]);
        }
        assert_eq!(inner.publish_count.load(Ordering::SeqCst), 3);
        let warmup = Duration::from_millis(inner.warmup_ms.load(Ordering::Relaxed));
        *inner.initialized_at.lock().unwrap() = Some(Instant::now() - warmup * 2);
        assert_eq!(inner.readiness(), Readiness::Warm, "跨过预热窗的证据档位");
        assert!(!inner.readiness().ready_for(true), "空集基线不得被 Warm 背书");
        // 强证据才给空集背书
        assert!(Readiness::Quiescent.ready_for(true));
        assert!(Readiness::Analyzed.ready_for(true));
        assert!(!Readiness::None.ready_for(false));
    }

    /// 同实例只允许首次降级返回 true（避免刷屏）。
    #[test]
    fn degraded_flag_only_fires_once() {
        let inner = test_inner();
        assert!(inner.note_degraded_once());
        assert!(!inner.note_degraded_once());
        assert!(!inner.note_degraded_once());
    }

    #[test]
    fn server_status_quiescent_is_readiness() {
        let inner = test_inner();
        inner.dispatch(&json!({
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": false }
        }));
        assert_eq!(inner.readiness(), Readiness::None, "quiescent=false 不算证据");
        inner.dispatch(&json!({
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": true }
        }));
        assert_eq!(inner.readiness(), Readiness::Quiescent);
        assert!(inner.readiness().ready());
        assert!(!Readiness::None.ready());
    }

    /// 一个不带进程的 `Inner`（单测用：不 spawn、不握手）。
    fn test_inner() -> Inner {
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        // 写通道无消费者也无所谓：单测只碰缓存与状态，不发消息
        drop(rx);
        Inner {
            tx,
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(HashMap::new()),
            revisions: Mutex::new(HashMap::new()),
            versions: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            pid: None,
            root: PathBuf::from("."),
            label: "test".into(),
            request_timeout_ms: AtomicU64::new(1000),
            initialize_timeout_ms: AtomicU64::new(1000),
            initialized_at: Mutex::new(None),
            warmup_ms: AtomicU64::new(1_000),
            saw_non_empty: AtomicBool::new(false),
            quiescent: AtomicBool::new(false),
            publish_count: AtomicU64::new(0),
            degraded_warned: AtomicBool::new(false),
        }
    }

    /// 模拟一条 `publishDiagnostics` 通知（uri 用规范的 [`path_to_uri`]）。
    fn push_diags(inner: &Inner, path: &str, items: &[Value]) {
        inner.dispatch(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": path_to_uri(Path::new(path)), "diagnostics": items }
        }));
    }

    // ---- 🟡-3：URI 兼容面（`file:` 写法差异不得让诊断静默消失） ----

    /// 各种合法写法都得落到同一个键：`file:///`、`file://localhost/`、`file:/`（大小写不敏感）。
    #[test]
    fn uri_variants_normalize_to_the_same_key() {
        let canonical = normalize_uri_key("file:///d%3A/Work/proj/src/index.ts");
        for uri in [
            "file://localhost/d%3A/Work/proj/src/index.ts",
            "file:/d%3A/Work/proj/src/index.ts",
            "FILE://LOCALHOST/d%3A/Work/proj/src/index.ts",
            "file:////d%3A/Work/proj/src/index.ts",
        ] {
            assert_eq!(normalize_uri_key(uri), canonical, "{uri} 未归一到同一键");
        }
        // 与本地路径键也要对上（Windows 折叠大小写；Unix 大小写敏感，按等价比）
        let path = normalize_path_key(Path::new(r"D:\Work\proj\src\index.ts"));
        assert!(keys_equivalent(&canonical, &path), "{canonical} != {path}");
    }

    /// 原有形态不得回归：UNC authority（非 localhost）必须原样保留，非 file scheme 不动。
    #[test]
    fn uri_variants_do_not_regress_unc_or_other_schemes() {
        // authority 不叫 localhost → 原样保留（UNC 形态）
        assert_eq!(
            strip_file_scheme("file://server/share/dir/a.ts"),
            "server/share/dir/a.ts"
        );
        // authority 叫 localhost → 丢弃
        assert_eq!(strip_file_scheme("file://localhost/a/b"), "a/b");
        // 非 file scheme 一律不动
        assert_eq!(
            strip_file_scheme("https://example.com/a.ts"),
            "https://example.com/a.ts"
        );
        assert_eq!(
            strip_file_scheme("untitled:Untitled-1"),
            "untitled:Untitled-1"
        );
        // 只有恰好是 `file:` scheme 才动（`filex:` 不是）
        assert_eq!(strip_file_scheme("filex://a/b"), "filex://a/b");
        // `file:` 后任意个斜杠都吃掉（双斜杠形态）
        assert_eq!(strip_file_scheme("file://d%3A/x.ts"), "d%3A/x.ts");
    }

    #[test]
    fn capabilities_declare_required_flags() {
        let v = capabilities_json();
        assert_eq!(v["workspace"]["configuration"], json!(true));
        assert_eq!(v["workspace"]["workspaceFolders"], json!(true));
        assert_eq!(
            v["textDocument"]["publishDiagnostics"]["relatedInformation"],
            json!(true)
        );
    }

    #[test]
    fn java_data_args_only_for_java() {
        let dir = PathBuf::from(r"D:\proj\.codewave");
        let args = java_data_args(super::super::Lang::Java, Some(&dir));
        assert_eq!(args[0], "-data");
        assert!(args[1].replace('\\', "/").ends_with("lsp/java"));
        assert!(java_data_args(super::super::Lang::Rust, Some(&dir)).is_empty());
        assert!(java_data_args(super::super::Lang::Java, None).is_empty());
    }
}
