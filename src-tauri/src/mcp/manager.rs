//! 连接管理：单 server 的建连 / 列表 / 调用 / 全停，状态自持。

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, JsonObject};
use rmcp::service::{Peer, RoleClient, RunningService};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransportConfig, StreamableHttpClientWorker,
};
use rmcp::transport::{ConfigureCommandExt, WorkerTransport};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use super::config::{McpServerConfig, McpTransport, load_configs};
use super::tools::{
    McpTool, McpToolDef, build_tool_defs, normalize_schema, parse_function, sanitize_component,
};

/// 连接 + initialize + list_tools 的总超时。
pub const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// 单次工具调用超时。
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// server 连接状态（mcp:status 事件与右栏展示用）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum McpState {
    /// 连接/初始化中
    Starting,
    /// 就绪可用
    Ready,
    /// 失败（含可读原因）
    Error(String),
}

/// manager 内部记录的单 server 条目。
struct ServerEntry {
    /// 最近一次连接使用的配置（重连复用）
    cfg: McpServerConfig,
    /// 当前状态
    state: McpState,
    /// 列出的工具（Ready 时非空）
    tools: Vec<McpTool>,
    /// 持有 RunningService（Drop 即取消连接）；调用经 peer 克隆
    service: Option<Arc<RunningService<RoleClient, ()>>>,
    /// 请求端点克隆（调用工具用）
    peer: Option<Peer<RoleClient>>,
}

/// MCP server 连接管理器：启动/重连/调用/全停，状态自持。
#[derive(Default)]
pub struct McpManager {
    /// server 名 → 连接条目（互斥锁串行化全部状态变更）
    servers: Mutex<HashMap<String, ServerEntry>>,
    /// 模型可见函数名 → (server 名, 工具原名)：反查不经字符串切分
    /// （server 名可含 `__` 或 provider 不支持的字符，切分不可靠）
    fn_index: Mutex<HashMap<String, (String, String)>>,
}

/// 单 server 建连完成句柄：工具列表 + 运行中服务 + 请求 peer。
type McpReady = (
    Vec<McpTool>,
    Arc<RunningService<RoleClient, ()>>,
    Peer<RoleClient>,
);

impl McpManager {
    /// Start (or restart) a server: connect → initialize → list_tools。
    /// `http` = 代理感知的共享 client（host 层从 core.client 读锁 clone 传入），
    /// streamable-http 连接与应用请求走同一代理配置。
    pub async fn start(
        &self,
        name: &str,
        cfg: McpServerConfig,
        http: reqwest::Client,
    ) -> Result<Vec<McpTool>, String> {
        if sanitize_component(name) != name {
            tracing::warn!(
                "MCP server 名 {:?} 含 provider 不支持的字符，模型可见函数名将归一化为 {:?}",
                name,
                sanitize_component(name)
            );
        }
        self.set_state(name, cfg.clone(), McpState::Starting, None)
            .await;
        let connect = async {
            match cfg.transport {
                McpTransport::Stdio => {
                    let command = cfg.command.clone().unwrap_or_default();
                    if command.is_empty() {
                        return Err("stdio transport 需要 command".into());
                    }
                    let transport = TokioChildProcess::new(
                        tokio::process::Command::new(&command).configure(|c| {
                            c.args(&cfg.args).envs(&cfg.env);
                            #[cfg(windows)]
                            c.creation_flags(0x0800_0000);
                        }),
                    )
                    .map_err(|e| format!("进程启动失败：{e}"))?;
                    ().serve(transport)
                        .await
                        .map_err(|e| format!("initialize 失败：{e}"))
                }
                McpTransport::StreamableHttp => {
                    let url = cfg.url.clone().unwrap_or_default();
                    if url.is_empty() {
                        return Err("streamable_http transport 需要 url".into());
                    }
                    // 注入代理感知 client（替代 new_simple 的默认 client，后者不带代理）
                    let worker = StreamableHttpClientWorker::new(
                        http,
                        StreamableHttpClientTransportConfig::with_uri(url),
                    );
                    let transport = WorkerTransport::spawn(worker);
                    ().serve(transport)
                        .await
                        .map_err(|e| format!("连接失败：{e}"))
                }
            }
        };
        let service = match tokio::time::timeout(INIT_TIMEOUT, connect).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                self.set_state(name, cfg, McpState::Error(e.clone()), None)
                    .await;
                return Err(e);
            }
            Err(_) => {
                let msg = "initialize 超时（30s）".to_string();
                self.set_state(name, cfg, McpState::Error(msg.clone()), None)
                    .await;
                return Err(msg);
            }
        };

        let peer = service.peer().clone();
        let tools_result = tokio::time::timeout(INIT_TIMEOUT, service.list_all_tools()).await;
        let tools = match tools_result {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                let msg = format!("list_tools 失败：{e}");
                self.set_state(name, cfg, McpState::Error(msg.clone()), None)
                    .await;
                return Err(msg);
            }
            Err(_) => {
                let msg = "list_tools 超时".to_string();
                self.set_state(name, cfg, McpState::Error(msg.clone()), None)
                    .await;
                return Err(msg);
            }
        };

        let mcp_tools: Vec<McpTool> = tools
            .iter()
            .map(|t| McpTool {
                server: name.to_string(),
                name: t.name.to_string(),
                description: format!("[{name}] {}", t.description.as_deref().unwrap_or("")),
                schema_json: normalize_schema(t.input_schema.as_ref()),
            })
            .collect();

        self.set_state(
            name,
            cfg,
            McpState::Ready,
            Some((mcp_tools.clone(), Arc::new(service), peer)),
        )
        .await;
        Ok(mcp_tools)
    }

    async fn set_state(
        &self,
        name: &str,
        cfg: McpServerConfig,
        state: McpState,
        ready: Option<McpReady>,
    ) {
        let mut servers = self.servers.lock().await;
        // 关闭旧连接（取得所有权后再取消）
        if let Some(old) = servers.get_mut(name) {
            if let Some(svc) = old.service.take() {
                if let Some(svc) = Arc::into_inner(svc) {
                    tokio::spawn(async move {
                        let _ = svc.cancel().await;
                    });
                }
            }
        }
        let entry = servers
            .entry(name.to_string())
            .or_insert_with(|| ServerEntry {
                cfg,
                state: McpState::Starting,
                tools: Vec::new(),
                service: None,
                peer: None,
            });
        match ready {
            Some((tools, svc, peer)) => {
                entry.tools = tools;
                entry.service = Some(svc);
                entry.peer = Some(peer);
            }
            None => {
                entry.tools.clear();
                entry.peer = None;
            }
        }
        entry.state = state;
    }

    /// 列出全部 server 的（名, 状态, 工具数），按名排序保证输出稳定。
    pub async fn status(&self) -> Vec<(String, McpState, usize)> {
        let servers = self.servers.lock().await;
        let mut v: Vec<(String, McpState, usize)> = servers
            .iter()
            .map(|(name, e)| (name.clone(), e.state.clone(), e.tools.len()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// 对模型暴露的就绪工具定义（函数名已归一化），并刷新函数名反查表。
    pub async fn tool_defs(&self) -> Vec<McpToolDef> {
        let all: Vec<McpTool> = {
            let servers = self.servers.lock().await;
            servers
                .values()
                .flat_map(|e| e.tools.iter().cloned())
                .collect()
        };
        let (defs, index) = build_tool_defs(&all);
        *self.fn_index.lock().await = index;
        defs
    }

    /// 函数名反查 (server, tool)：优先查注册表，缺失时退回字符串切分（兼容未登记场景）。
    async fn resolve_function(&self, function: &str) -> Option<(String, String)> {
        if let Some(hit) = self.fn_index.lock().await.get(function) {
            return Some(hit.clone());
        }
        parse_function(function)
    }

    /// 按 `mcp__server__tool` 函数名调用；会话失效时自动重连一次。
    /// `http` = 代理感知 client（与 start 同源），供重连路径复用。
    // 7 个参数各对应一处独立输入（函数名 / 工具参数 / 数据目录 / 工作区 / 项目目录 /
    // 额外根 / HTTP client），缺一不可；收成结构体只是换壳，却要连带动 batch.rs 的调用点与
    // 本文件测试模块里的两个真实 server 调用点（后者本次范围禁改），收益不划算。
    #[allow(clippy::too_many_arguments)]
    pub async fn call(
        self: &Arc<Self>,
        function: &str,
        args: Value,
        data_dir: &std::path::Path,
        workspace: &std::path::Path,
        project_dir: Option<&std::path::Path>,
        extra_roots: &[String],
        http: reqwest::Client,
    ) -> Result<String, String> {
        let Some((server, tool)) = self.resolve_function(function).await else {
            return Err(format!(
                "非法 MCP 函数名：{function}（应为 mcp__<server>__<tool>）"
            ));
        };
        // 重连只在 attempt 0 发生一次：Option::take 满足循环体内的 move 检查
        let mut http = Some(http);
        for attempt in 0..2 {
            match self.call_once(&server, &tool, &args).await {
                Ok(text) => return Ok(text),
                Err(e) if attempt == 0 => {
                    // M10 修复：仅会话失效/连接断开才重连；业务错误（is_error）与超时可能已有副作用，
                    // 重放非幂等工具很危险 → 直接返回
                    let reconnectable = is_invalid_session(&e)
                        || e.contains("channel closed")
                        || e.contains("send failed");
                    if !reconnectable {
                        return Err(e);
                    }
                    let cfg = {
                        let servers = self.servers.lock().await;
                        servers.get(&server).map(|s| s.cfg.clone())
                    };
                    let cfg = match cfg {
                        Some(c) => Some(c),
                        None => load_configs(data_dir, workspace, project_dir, extra_roots)
                            .into_iter()
                            .find(|(n, _)| n == &server)
                            .map(|(_, c)| c),
                    };
                    if let Some(cfg) = cfg {
                        tracing::info!("MCP {server} 会话失效，重连…");
                        let _ = self
                            .start(&server, cfg, http.take().unwrap_or_default())
                            .await;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err("MCP 调用重连后仍失败".into())
    }

    async fn call_once(&self, server: &str, tool: &str, args: &Value) -> Result<String, String> {
        let peer = {
            let servers = self.servers.lock().await;
            let Some(entry) = servers.get(server) else {
                return Err(format!("MCP server 未连接：{server}"));
            };
            let Some(peer) = entry.peer.clone() else {
                return Err(format!("MCP server 不可用：{server}（{:?}）", entry.state));
            };
            peer
        };
        let mut params = CallToolRequestParams::new(tool.to_string());
        if let Some(obj) = args.as_object() {
            let json_obj: JsonObject = serde_json::from_value(Value::Object(obj.clone()))
                .map_err(|e| format!("参数序列化失败：{e}"))?;
            params.arguments = Some(json_obj);
        }
        let result = tokio::time::timeout(CALL_TIMEOUT, peer.call_tool(params))
            .await
            .map_err(|_| "MCP 调用超时（120s）".to_string())?
            .map_err(|e| format!("{e}"))?;

        let mut text_out = String::new();
        for block in &result.content {
            if !text_out.is_empty() {
                text_out.push('\n');
            }
            match block {
                rmcp::model::ContentBlock::Text(t) => text_out.push_str(&t.text),
                rmcp::model::ContentBlock::Image(_) => text_out.push_str("[image content]"),
                _ => text_out.push_str("[non-text content]"),
            }
        }
        if result.is_error.unwrap_or(false) {
            return Err(format!("MCP 工具返回错误：{text_out}"));
        }
        Ok(text_out)
    }

    /// 停止全部连接并清空注册表（配置保存后热重载用）。
    pub async fn stop_all(&self) {
        let mut servers = self.servers.lock().await;
        let svcs: Vec<Arc<RunningService<RoleClient, ()>>> = servers
            .values_mut()
            .filter_map(|e| e.service.take())
            .collect();
        servers.clear();
        self.fn_index.lock().await.clear();
        for svc in svcs {
            if let Some(svc) = Arc::into_inner(svc) {
                let _ = svc.cancel().await;
            }
        }
    }
}

/// 错误文本是否表明会话失效（可安全重连的那类）。
fn is_invalid_session(e: &str) -> bool {
    e.contains("invalid session") || e.contains("session expired") || e.contains("not initialized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ===== A3：真实 MCP server 集成验证（官方 TS SDK，scripts/mcp-test-server.mjs）=====
    // 无 node 环境时跳过（dev 机器有 node；三平台 CI 需预装 node）。

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

    fn noop_ctx() -> (tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_stdio_server_connect_list_call() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        let mgr = Arc::new(McpManager::default());
        let cfg = McpServerConfig {
            transport: McpTransport::Stdio,
            command: Some("node".into()),
            args: vec![test_server_script().to_string_lossy().into_owned()],
            env: HashMap::new(),
            url: None,
        };
        let _ = mgr
            .start("wavetest.proxy", cfg, reqwest::Client::new())
            .await
            .expect("真实 stdio server 连接失败");
        // 经 tool_defs 取得归一化后的函数名（server 名里的 `.` → `_`）
        let defs = mgr.tool_defs().await;
        let echo = defs
            .iter()
            .find(|d| d.function_name.ends_with("__echo"))
            .expect("应有 echo 工具");
        assert_eq!(
            echo.function_name, "mcp__wavetest_proxy__echo",
            "server 名里的 `.` 必须归一化，否则 provider 400"
        );
        assert!(
            serde_json::from_str::<Value>(&echo.schema_json).unwrap()["additionalProperties"]
                == json!(false),
            "schema 归一化应补 additionalProperties:false"
        );
        // 真实工具调用往返（归一化函数名经反查表映射回原 server/tool）
        let (_dd, _ws) = noop_ctx();
        let out = mgr
            .call(
                &echo.function_name,
                json!({ "text": "hello codewave" }),
                _dd.path(),
                _ws.path(),
                None,
                &[],
                reqwest::Client::new(),
            )
            .await
            .expect("真实 echo 调用失败");
        assert_eq!(out, "echo: hello codewave");
        // State flip Starting→Ready
        let status = mgr.status().await;
        assert!(matches!(&status[0].1, McpState::Ready));
        mgr.stop_all().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_streamable_http_server_connect_list_call() {
        if !node_available() {
            eprintln!("skip：node 不可用");
            return;
        }
        // 预占一个空闲端口，随后释放给 node
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
        // 等服务就绪（最长 5s）
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
        let cfg = McpServerConfig {
            transport: McpTransport::StreamableHttp,
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some(format!("http://127.0.0.1:{port}/mcp")),
        };
        let tools = mgr
            .start("wavetest-http", cfg, reqwest::Client::new())
            .await
            .expect("真实 streamable-http server 连接失败");
        assert!(tools.iter().any(|t| t.name == "add"), "应列出 add 工具");
        let (_dd, _ws) = noop_ctx();
        let out = mgr
            .call(
                "mcp__wavetest-http__add",
                json!({ "a": 6, "b": 7 }),
                _dd.path(),
                _ws.path(),
                None,
                &[],
                reqwest::Client::new(),
            )
            .await
            .expect("真实 add 调用失败");
        assert_eq!(out, "13", "add(6,7) 应返回 13");
        let _ = child.kill().await;
    }
}
