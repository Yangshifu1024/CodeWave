//! service 工具：长驻后台进程（dev server 等）的启停与日志尾读，512KB 滚动缓冲。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// service 工具入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 操作类型：start / stop / list / read。
    action: String,
    /// start：服务短标签。
    #[serde(default)]
    name: Option<String>,
    /// start：要执行的命令。
    #[serde(default)]
    command: Option<String>,
    /// start：工作区相对的执行目录。
    #[serde(default)]
    cwd: Option<String>,
    /// 服务用途：preview 可在待验收/完成后保留，其余随目标停止。
    #[serde(default)]
    purpose: ServicePurpose,
    /// stop / read：服务 id。
    #[serde(default)]
    id: Option<String>,
    /// read：读取的尾部字节数，默认 8192。
    #[serde(default)]
    tail_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePurpose {
    #[default]
    Development,
    Preview,
}

/// service 工具：管理长驻后台服务（dev server 等）。
/// 入参为 action 四选一：start {name, command, cwd?} 产出 svc id、stop {id}、list、read {id, tailBytes?}；
/// 声明为 ReadOnly，但 start 的命令同样过 fence 审批（与 command 工具一致，灾难级仍拦截）。
/// 日志存于每服务 512KB 滚动环形缓冲，节流事件推送到前端。
pub struct ServiceTool;

/// 环形日志容量（字节）：超出即丢弃最旧数据。
const RING_CAP: usize = 512 * 1024;

/// 环形日志内部状态：字节环 + 输出清洗器。
///
/// 清洗器必须与字节环同在锁内：stdout / stderr 两条 drain 共享同一个 `RingLog`，
/// 串行化后它们的半截转义/多字节状态才不会互相污染。
struct RingLogInner {
    ring: VecDeque<u8>,
    clean: crate::tools::sanitize::OutputSanitizer,
}

/// 字节级环形日志：Arc + Mutex 共享，超过 RING_CAP 时逐字节淘汰最旧数据。
/// 写入前先过 [`crate::tools::sanitize::OutputSanitizer`]（剥 ANSI/控制序列 + 跨块安全解码）。
#[derive(Clone)]
pub struct RingLog(Arc<std::sync::Mutex<RingLogInner>>);

impl RingLog {
    /// 创建空日志。
    fn new() -> Self {
        RingLog(Arc::new(std::sync::Mutex::new(RingLogInner {
            ring: VecDeque::with_capacity(RING_CAP),
            clean: crate::tools::sanitize::OutputSanitizer::new(),
        })))
    }
    /// 追加一块原始输出（先清洗），超容量时淘汰最旧的。
    pub fn push(&self, bytes: &[u8]) {
        let mut g = self.0.lock().unwrap();
        let text = g.clean.push(bytes);
        Self::append(&mut g, &text);
    }
    /// 流结束：把清洗器里的残留吐进环（半截多字节兜底；半截转义序列丢弃）。
    pub fn flush(&self) {
        let mut g = self.0.lock().unwrap();
        let text = g.clean.finish();
        Self::append(&mut g, &text);
    }
    /// 已清洗文本入环（超容量逐字节淘汰最旧）。
    fn append(inner: &mut RingLogInner, text: &str) {
        if text.is_empty() {
            return;
        }
        for b in text.as_bytes() {
            if inner.ring.len() >= RING_CAP {
                inner.ring.pop_front();
            }
            inner.ring.push_back(*b);
        }
    }
    /// 读尾部 n 字节并按 UTF-8 宽松解码。
    pub fn tail(&self, n: usize) -> String {
        let g = self.0.lock().unwrap();
        let skip = g.ring.len().saturating_sub(n);
        let s: Vec<u8> = g.ring.iter().skip(skip).copied().collect();
        String::from_utf8_lossy(&s).into_owned()
    }
    /// 当前缓冲字节数。
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().ring.len()
    }
}

/// 运行中服务的句柄：进程信息 + 共享环形日志 + 停止信号 + 退出标记。
pub struct ServiceHandle {
    /// 服务 id（`svc_` 前缀 + 8 位随机）。
    pub id: String,
    /// 根会话所有者，子代理启动的服务归根目标管理。
    pub owner_root_id: String,
    pub purpose: ServicePurpose,
    /// 展示名。
    pub name: String,
    /// 启动命令原文。
    pub command: String,
    /// 启动时刻（计算 uptime 用）。
    pub started_at: SystemTime,
    /// 子进程 PID（进程组 id）。
    pub pid: u32,
    /// 共享环形日志。
    pub log: RingLog,
    /// 停止信号。
    pub cancel: tokio_util::sync::CancellationToken,
    /// 进程是否已退出。
    pub done: Arc<std::sync::atomic::AtomicBool>,
}

impl ServiceHandle {
    /// 已运行秒数。
    pub fn uptime_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(self.started_at)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// 服务表：进程内全部运行中后台服务的并发映射。
#[derive(Default)]
pub struct ServiceTable {
    services: dashmap::DashMap<String, Arc<ServiceHandle>>,
}

impl ServiceTable {
    /// 列出全部服务句柄。
    pub fn list(&self) -> Vec<Arc<ServiceHandle>> {
        self.services.iter().map(|e| e.value().clone()).collect()
    }
    /// 按 id 取服务句柄。
    pub fn get(&self, id: &str) -> Option<Arc<ServiceHandle>> {
        self.services.get(id).map(|e| e.value().clone())
    }
    /// 插入新服务（受上限约束）。
    fn try_insert(&self, h: Arc<ServiceHandle>) -> Result<(), String> {
        // 上限 16 个服务；超限显式报错（此前静默失败，导致 stop/read 全部落空）
        if self.services.len() >= 16 {
            return Err("后台服务数已达上限（16），请先停止部分服务".into());
        }
        self.services.insert(h.id.clone(), h);
        Ok(())
    }
    /// 移除并返回服务句柄。
    pub fn remove(&self, id: &str) -> Option<Arc<ServiceHandle>> {
        self.services.remove(id).map(|(_, v)| v)
    }
}

/// Stop via the owned job/process group, then await confirmed process-tree exit.
pub async fn stop_service(handle: &Arc<ServiceHandle>) -> Result<(), String> {
    if handle.done.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    handle.cancel.cancel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !handle.done.load(std::sync::atomic::Ordering::SeqCst)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if handle.done.load(std::sync::atomic::Ordering::SeqCst) {
        Ok(())
    } else {
        Err(format!("服务 {} 仍在停止，尚未确认进程树退出", handle.name))
    }
}

/// Stop only this goal's services. A failed reap must keep the goal visibly stopping.
pub async fn stop_goal_services(
    core: &crate::core::agent::AgentCore,
    root_id: &str,
    keep_preview: bool,
) -> Result<(), String> {
    let handles: Vec<_> = core
        .services
        .list()
        .into_iter()
        .filter(|h| {
            h.owner_root_id == root_id && !(keep_preview && h.purpose == ServicePurpose::Preview)
        })
        .collect();
    let mut pending = Vec::new();
    for handle in handles {
        let _ = stop_service(&handle).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !handle.done.load(std::sync::atomic::Ordering::SeqCst)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if handle.done.load(std::sync::atomic::Ordering::SeqCst) {
            core.services.remove(&handle.id);
            core.sink.emit(
                &root_id.to_string(),
                "service:update",
                json!({"session":root_id,"removed":handle.id}),
            );
        } else {
            pending.push(handle.name.clone());
        }
    }
    if pending.is_empty() {
        Ok(())
    } else {
        Err(format!("后台服务尚未确认退出：{}", pending.join("、")))
    }
}

#[async_trait::async_trait]
impl Tool for ServiceTool {
    fn name(&self) -> &'static str {
        "service"
    }
    fn description(&self) -> &'static str {
        "管理长驻后台服务（dev server 等）。start {name, command, cwd?} → 返回服务 id；stop {id}；list；read {id, tailBytes?}。日志保存在 512KB 滚动缓冲中。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["action"],
  "properties": {
    "action": {"type": "string", "enum": ["start", "stop", "list", "read"]},
    "name": {"type": "string", "description": "短标签，start 用"},
    "command": {"type": "string", "description": "start 用"},
    "cwd": {"type": "string", "description": "相对工作区，start 用"},
    "purpose": {"type": "string", "enum": ["development", "preview"], "description": "默认 development；明确用于人工验收的预览服务用 preview"},
    "id": {"type": "string", "description": "服务 id，stop/read 用"},
    "tailBytes": {"type": "integer", "description": "默认 8192，read 用"}
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        match args.action.as_str() {
            "start" => start_service(ctx, args).await,
            "stop" => {
                let id = args.id.unwrap_or_default();
                let Some(h) = ctx.core.services.get(&id) else {
                    return ToolOutcome::err("E_NOT_FOUND", format!("服务不存在：{id}"));
                };
                if let Err(error) = stop_service(&h).await {
                    return ToolOutcome::err("E_SERVICE_STOPPING", error);
                }
                ctx.core.services.remove(&id);
                ctx.core.sink.emit(
                    &ctx.rt.id,
                    "service:update",
                    json!({ "session": ctx.rt.id, "removed": id }),
                );
                ToolOutcome::ok(json!({ "stopped": id }))
            }
            "list" => {
                let items: Vec<Value> = ctx
                    .core
                    .services
                    .list()
                    .iter()
                    .map(|h| {
                        json!({ "id": h.id, "name": h.name, "command": h.command,
                                "pid": h.pid, "uptime_secs": h.uptime_secs(), "log_bytes": h.log.len(),
                                "owner_root_id": h.owner_root_id, "purpose": h.purpose })
                    })
                    .collect();
                ToolOutcome::ok(json!({ "services": items }))
            }
            "read" => {
                let id = args.id.unwrap_or_default();
                let Some(h) = ctx.core.services.get(&id) else {
                    return ToolOutcome::err("E_NOT_FOUND", format!("服务不存在：{id}"));
                };
                let tail = h.log.tail(args.tail_bytes.unwrap_or(8192).min(RING_CAP));
                ToolOutcome::ok(json!({ "id": id, "name": h.name, "tail": tail }))
            }
            other => ToolOutcome::err("E_ARGS", format!("未知 action：{other}")),
        }
    }
}

/// start 动作实现：fence/审批 → 以 bash -lc 启动进程 → 双泵写环形日志 + ticker 节流推送 + 退出监听。
async fn start_service(ctx: &ToolCtx, args: Args) -> ToolOutcome {
    let Some(command) = args.command.filter(|c| !c.trim().is_empty()) else {
        return ToolOutcome::err("E_ARGS", "start 需要 command");
    };
    let name = args.name.unwrap_or_else(|| "service".into());
    let roots = ctx.write_roots();
    let cwd = match &args.cwd {
        Some(c) => match super::pathutil::resolve_read(&roots, c) {
            Ok(p) => p,
            Err((code, m)) => return ToolOutcome::err(&code, m),
        },
        None => roots.workspace.clone(),
    };

    // fence：后台服务走同一套安全检查（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md) 权限档：FullAccess 跳过确认，灾难级仍拦截）
    let mode = ctx.execution_approval_mode();
    let policy = ctx.fence_policy();
    match crate::safety::fence::check_command_policy(&command, &cwd, &roots, policy) {
        crate::safety::fence::Verdict::Allow => {}
        crate::safety::fence::Verdict::Block { code, message } => {
            return ToolOutcome::err(&code, message);
        }
        crate::safety::fence::Verdict::Confirm(reason) => {
            use crate::safety::fence::ConfirmReason;
            if mode == crate::core::prefs::ApprovalMode::FullAccess {
                if let ConfirmReason::Disaster(why) = &reason {
                    return ToolOutcome::err(
                        "E_COMMAND_BLOCKED",
                        format!("灾难性命令已拦截：{why}"),
                    );
                }
            } else {
                // [docs/arithmetic-fixes-batch](../../../docs/arithmetic-fixes-batch.md)：灾难级确认永不搭自动确认超时
                let is_disaster = matches!(reason, ConfirmReason::Disaster(_));
                let title = match reason {
                    ConfirmReason::HighRisk(w) => format!("后台服务高危命令确认：{w}"),
                    ConfirmReason::Disaster(w) => format!("后台服务灾难性命令确认：{w}"),
                    ConfirmReason::InsideWrite(t) => format!("后台服务将修改工作区文件：{t}"),
                    ConfirmReason::OutsideCreate(t) => format!("后台服务将在工作区外创建路径：{t}"),
                };
                let auto_confirm = crate::safety::approval::effective_auto_confirm(
                    ctx.core.cfg.read().unwrap().approval.auto_confirm,
                    is_disaster,
                );
                let ok = crate::safety::approval::confirm(
                    &ctx.rt,
                    &ctx.core.sink,
                    crate::safety::approval::ApprovalRequest {
                        title,
                        detail: command.clone(),
                        allow_always: false,
                        auto_confirm,
                    },
                    &ctx.cancel,
                )
                .await;
                if !ok.approved {
                    return ToolOutcome::err("E_APPROVAL_DENIED", "用户拒绝或未响应");
                }
            }
        }
    }

    // 执行 shell 跟随配置 selection（与 command 工具同一拼接事实源；长驻任务不再硬编码 bash）
    let selection = ctx.core.cfg.read().unwrap().shell.selection.clone();
    let shell = crate::tools::command::resolve_shell(selection.as_deref());
    let (program, argv) = crate::tools::command::shell_invocation(&shell, &command, &cwd);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&argv)
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    let mut wrapped = crate::mcp::wrap_process_tree(cmd);
    let mut child = match wrapped.spawn() {
        Ok(c) => c,
        Err(e) => return ToolOutcome::err("E_IO", format!("进程启动失败：{e}")),
    };
    // PID 为 0 会让 kill(-0) 杀掉我们自己的进程组，PID 为 1 会让 kill(-1) 信号发给该用户的
    // 全部进程——已回收的子进程（id 为 None）必须拒绝，绝不回退
    let Some(pid) = child.id() else {
        return ToolOutcome::err("E_IO", "无法获取子进程 PID，无法注册为后台服务");
    };
    let id = format!("svc_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let log = RingLog::new();
    let cancel = tokio_util::sync::CancellationToken::new();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // M9 修复：stdout/stderr 各自独立泵（顺序读在任一流沉寂时会饿死另一流），
    // 只写 RingLog；节流事件由独立 ticker 发出
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    async fn drain<R: tokio::io::AsyncRead + Unpin>(mut s: R, log: RingLog) {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => log.push(&buf[..n]),
            }
        }
        // 流结束：把清洗器的残留（半截多字节兜底）吐进环
        log.flush();
    }
    let pump_a = tokio::spawn(drain(stdout.expect("stdout 已 piped"), log.clone()));
    let pump_b = tokio::spawn(drain(stderr.expect("stderr 已 piped"), log.clone()));

    // ticker：每 1s 发一次尾部事件，直到双泵结束
    let sink = ctx.core.sink.clone();
    let session = ctx.rt.id.clone();
    let svc_id = id.clone();
    let ticker_log = log.clone();
    let ticker_done = done.clone();
    let _ticker = tokio::spawn(async move {
        let mut last_tail = String::new();
        loop {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            let tail = ticker_log.tail(2000);
            if tail != last_tail {
                sink.emit(
                    &session,
                    "service:update",
                    json!({ "session": session, "id": svc_id, "tail": tail }),
                );
                last_tail = tail;
            }
            if ticker_done.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
        }
    });

    // Only process reaping marks completion; closed stdout alone is not termination.
    let done2 = done.clone();
    let sink2 = ctx.core.sink.clone();
    let session2 = ctx.rt.id.clone();
    let svc2 = id.clone();
    let stop_token = cancel.clone();
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                result = child.wait() => result,
                _ = stop_token.cancelled() => {
                    match child.start_kill() {
                        Ok(()) => child.wait().await,
                        Err(error) => Err(error),
                    }
                }
            };
            if let Err(error) = result {
                tracing::warn!("service {svc2} exit could not be confirmed: {error}");
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            break;
        }
        let _ = pump_a.await;
        let _ = pump_b.await;
        done2.store(true, std::sync::atomic::Ordering::SeqCst);
        sink2.emit(
            &session2,
            "service:update",
            json!({"session":session2,"id":svc2,"exited":true}),
        );
    });
    let owner = crate::core::agent::goal::goal_gate_rt(&ctx.core, &ctx.rt);
    let handle = Arc::new(ServiceHandle {
        id: id.clone(),
        owner_root_id: owner.id.clone(),
        purpose: args.purpose,
        name,
        command,
        started_at: SystemTime::now(),
        pid,
        log,
        cancel,
        done,
    });
    if let Err(e) = ctx.core.services.try_insert(handle.clone()) {
        if let Err(stopping) = stop_service(&handle).await {
            // Preserve ownership even when capacity rejection cannot finish cleanup.
            ctx.core.services.services.insert(id.clone(), handle);
            return ToolOutcome::err("E_SERVICE_STOPPING", format!("{e}；{stopping}（{id}）"));
        }
        return ToolOutcome::err("E_SERVICE_FULL", e);
    }
    ToolOutcome::ok(
        json!({ "id": id, "pid": pid, "purpose": args.purpose, "owner_root_id": owner.id, "note": "用 read 查看日志；stop 停止（进程树）" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// service 环形日志的写作入口必须清洗：带色输出不得把 ANSI 转义写进日志。
    #[test]
    fn ring_log_strips_ansi_and_keeps_surviving_bytes() {
        let log = RingLog::new();
        log.push(b"\x1b[32mok\x1b[0m\n");
        assert_eq!(log.tail(4096), "ok\n");
        assert!(!log.tail(4096).contains('\u{1b}'));

        // 多字节字符被分块切开也不出替换符
        let check = "✓".as_bytes();
        log.push(&[check[0], check[1]]);
        log.push(&check[2..]);
        assert_eq!(log.tail(4096), "ok\n✓");

        // 超容量逐字节淘汰仍然成立
        let big = vec![b'x'; RING_CAP + 16];
        log.push(&big);
        assert_eq!(log.len(), RING_CAP);
    }

    #[tokio::test]
    async fn service_lifecycle() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt =
            core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
        // 本机制测试显式设为 AutoEdit：默认 Plan 档的只读白名单会把 `sleep 30` 段送进审批超时（审批链路有专门测试）
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::AutoEdit,
            model_id: None,
            reasoning_effort: None,
        });
        let ctx = ToolCtx {
            core: core.clone(),
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let tool = ServiceTool;
        // start
        let out = tool.run(&ctx, serde_json::json!({"action":"start","name":"echoer","command":"echo started; sleep 30"})).await;
        assert!(out.ok, "{out:?}");
        let id = out.data["id"].as_str().unwrap().to_string();
        // 等输出进入环形缓冲
        tokio::time::sleep(Duration::from_millis(700)).await;
        // read
        let out = tool
            .run(&ctx, serde_json::json!({"action":"read","id":id}))
            .await;
        assert!(out.ok);
        assert!(
            out.data["tail"].as_str().unwrap().contains("started"),
            "{:?}",
            out.data
        );
        // list
        let out = tool.run(&ctx, serde_json::json!({"action":"list"})).await;
        assert_eq!(out.data["services"].as_array().unwrap().len(), 1);
        // stop（杀进程树）
        let out = tool
            .run(&ctx, serde_json::json!({"action":"stop","id":id}))
            .await;
        assert!(out.ok);
        let out = tool.run(&ctx, serde_json::json!({"action":"list"})).await;
        assert_eq!(out.data["services"].as_array().unwrap().len(), 0);
    }

    // ===== 目标档执行期：账本闸门（与 command 工具同构接线，🟡-2）=====

    /// 目标档 ctx 夹具（工作区 / 数据目录 TempDir 随夹具存活，避免进程 cwd 失效）。
    struct GoalSvcFixture {
        _ws: tempfile::TempDir,
        _dd: tempfile::TempDir,
        ctx: ToolCtx,
    }

    fn goal_svc_fixture(
        mode: crate::core::prefs::ApprovalMode,
        status: crate::core::agent::goal::GoalStatus,
        programs: Vec<String>,
    ) -> GoalSvcFixture {
        use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState};
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "goalsvc",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: mode,
            model_id: None,
            reasoning_effort: None,
        });
        rt.set_goal(Some(GoalState {
            text: "把 X 改成 Y".into(),
            criteria: vec![GoalCriterion {
                title: "改完 X".into(),
                done: false,
                manual: false,
                verification: None,
            }],
            ledger: GoalLedger {
                paths: vec![],
                programs,
            },
            status,
            decisions: Vec::new(),
            pending: Vec::new(),
            blocked: Vec::new(),
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
            delivery: Default::default(),
        }));
        let ctx = ToolCtx {
            core: core.clone(),
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        GoalSvcFixture {
            _ws: ws,
            _dd: dd,
            ctx,
        }
    }

    /// ① 目标档执行期：程序名在账本内 → 放行（且不产生审批请求：预取消 token 未被消费）。
    #[tokio::test]
    async fn goal_service_gate_allows_ledger_program() {
        use crate::core::prefs::ApprovalMode;
        let f = goal_svc_fixture(
            ApprovalMode::Goal,
            crate::core::agent::goal::GoalStatus::Executing,
            vec!["echo".into()],
        );
        // 预取消：若走了审批链路必然拿到 E_APPROVAL_DENIED（证明零提问）
        f.ctx.cancel.cancel();
        let out = ServiceTool
            .run(
                &f.ctx,
                serde_json::json!({"action":"start","name":"echoer","command":"echo started"}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        let g = f.ctx.rt.goal_snapshot().unwrap();
        assert!(g.blocked.is_empty(), "{:?}", g.blocked);
        assert_eq!(g.ledger_denials, 0);
        assert!(!f.ctx.rt.take_goal_abort());
        // 收尾：停掉刚起的服务（命令本身是 echo，会立即退出）
        let id = out.data["id"].as_str().unwrap().to_string();
        let _ = ServiceTool
            .run(&f.ctx, serde_json::json!({"action":"stop","id":id}))
            .await;
    }

    /// 目标执行期不再要求程序预登记，服务仍受完全访问的安全围栏约束。
    #[tokio::test]
    async fn goal_service_allows_program_without_ledger() {
        use crate::core::prefs::ApprovalMode;
        let f = goal_svc_fixture(
            ApprovalMode::Goal,
            crate::core::agent::goal::GoalStatus::Executing,
            vec!["cargo".into()],
        );
        let out = ServiceTool
            .run(
                &f.ctx,
                serde_json::json!({"action":"start","name":"lister","command":"ls -la"}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(f.ctx.rt.goal_snapshot().unwrap().ledger_denials, 0);
        stop_goal_services(&f.ctx.core, &f.ctx.rt.id, false)
            .await
            .unwrap();
        assert!(f.ctx.core.services.list().is_empty());
    }

    /// ③（回归红线）非目标档 / 目标档澄清期：账本闸门完全不受影响。
    #[tokio::test]
    async fn goal_service_gate_inert_outside_goal_execute() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        for (mode, status) in [
            (ApprovalMode::AutoEdit, GoalStatus::Executing),
            (ApprovalMode::Goal, GoalStatus::Clarify),
        ] {
            let f = goal_svc_fixture(mode, status, vec![]);
            f.ctx.cancel.cancel();
            let out = ServiceTool
                .run(
                    &f.ctx,
                    serde_json::json!({"action":"start","name":"lister","command":"echo listed"}),
                )
                .await;
            assert_ne!(
                out.error.as_ref().map(|e| e.code.as_str()),
                Some("E_GOAL_OUTSIDE_LEDGER")
            );
            stop_goal_services(&f.ctx.core, &f.ctx.rt.id, false)
                .await
                .unwrap();
            assert_eq!(
                f.ctx.rt.goal_snapshot().unwrap().ledger_denials,
                0,
                "{mode:?}/{status:?}"
            );
            assert!(!f.ctx.rt.take_goal_abort(), "{mode:?}/{status:?}");
        }
    }

    #[tokio::test]
    async fn stop_timeout_keeps_service_visible_for_retry() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let f = goal_svc_fixture(ApprovalMode::Goal, GoalStatus::Executing, vec![]);
        let handle = Arc::new(ServiceHandle {
            id: "unconfirmed".into(),
            owner_root_id: f.ctx.rt.id.clone(),
            purpose: ServicePurpose::Development,
            name: "unconfirmed".into(),
            command: "test fixture".into(),
            started_at: SystemTime::now(),
            pid: 0,
            log: RingLog::new(),
            cancel: tokio_util::sync::CancellationToken::new(),
            done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        f.ctx.core.services.try_insert(handle.clone()).unwrap();
        let out = ServiceTool
            .run(&f.ctx, json!({"action":"stop","id":"unconfirmed"}))
            .await;
        assert_eq!(
            out.error.as_ref().map(|e| e.code.as_str()),
            Some("E_SERVICE_STOPPING")
        );
        assert!(f.ctx.core.services.get("unconfirmed").is_some());
        handle.done.store(true, std::sync::atomic::Ordering::SeqCst);
        let out = ServiceTool
            .run(&f.ctx, json!({"action":"stop","id":"unconfirmed"}))
            .await;
        assert!(out.ok);
        assert!(f.ctx.core.services.get("unconfirmed").is_none());
    }

    /// Service cleanup is scoped to the root goal, and previews survive only acceptance cleanup.
    #[tokio::test]
    async fn goal_service_cleanup_respects_owner_and_preview() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let f = goal_svc_fixture(ApprovalMode::Goal, GoalStatus::Executing, vec![]);
        for (id, owner, purpose) in [
            ("own-dev", "goalsvc", ServicePurpose::Development),
            ("own-preview", "goalsvc", ServicePurpose::Preview),
            ("other", "other-session", ServicePurpose::Development),
        ] {
            f.ctx
                .core
                .services
                .try_insert(Arc::new(ServiceHandle {
                    id: id.into(),
                    owner_root_id: owner.into(),
                    purpose,
                    name: id.into(),
                    command: "echo".into(),
                    started_at: SystemTime::now(),
                    pid: 0,
                    log: RingLog::new(),
                    cancel: tokio_util::sync::CancellationToken::new(),
                    done: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                }))
                .unwrap();
        }
        stop_goal_services(&f.ctx.core, "goalsvc", true)
            .await
            .unwrap();
        assert!(f.ctx.core.services.get("own-dev").is_none());
        assert!(f.ctx.core.services.get("own-preview").is_some());
        assert!(f.ctx.core.services.get("other").is_some());
        stop_goal_services(&f.ctx.core, "goalsvc", false)
            .await
            .unwrap();
        assert!(f.ctx.core.services.get("own-preview").is_none());
        assert!(f.ctx.core.services.get("other").is_some());
    }

    #[tokio::test]
    async fn goal_subagent_service_has_root_owner() {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let f = goal_svc_fixture(ApprovalMode::Goal, GoalStatus::Executing, vec![]);
        let ctx = ToolCtx {
            core: f.ctx.core.clone(),
            rt: crate::core::agent::SessionRuntime::new_sub(&f.ctx.rt, "service-sub".into()),
            batch_id: "sub-service".into(),
            call_index: 0,
            call_key: "sub-service:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let out = ServiceTool
            .run(
                &ctx,
                json!({"action":"start","command":"echo preview", "purpose":"preview"}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        let id = out.data["id"].as_str().unwrap();
        let handle = ctx.core.services.get(id).unwrap();
        assert_eq!(handle.owner_root_id, f.ctx.rt.id);
        assert_eq!(handle.purpose, ServicePurpose::Preview);
        stop_goal_services(&ctx.core, &f.ctx.rt.id, false)
            .await
            .unwrap();
        assert!(ctx.core.services.get(id).is_none());
    }

    #[test]
    fn ring_caps_size() {
        let r = RingLog::new();
        r.push(&vec![b'x'; RING_CAP + 1000]);
        assert!(r.len() <= RING_CAP);
    }
}
