//! service 工具：长驻后台进程（dev server 等）的启停与日志尾读，512KB 滚动缓冲。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::wait_targets::{TargetKind, TargetState};
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
                // 登记表条目随显式 stop 一并移除：service 不再存在，等待它没有意义
                // （自然退出不同——services 表不删，登记表也刻意留终态条目供 wait 事后回查）。
                ctx.core.wait_targets.remove(&id);
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
    // 登记表句柄：ticker 顺带把日志尾写进可等待目标，让 `wait` 的 `match.text` 在服务
    // **运行期间**就是活日志判据（而不是只能等进程退出后才命中）。
    let ticker_core = ctx.core.clone();
    let ticker_svc_id = id.clone();
    let _ticker = tokio::spawn(async move {
        let mut last_tail = String::new();
        loop {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            let tail = ticker_log.tail(2000);
            // 无条件写入（含空串）：服务无输出时 tail 恒为空，但「它还活着」本身就是
            // wait 超时时该看到的证据，不能因为空而丢。
            ticker_core
                .wait_targets
                .update_text(&ticker_svc_id, tail.clone());
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
    // 收尾闭包需访问登记表：spawn 进的是 'static 任务，拿 Arc<AgentCore> 的 clone
    // （AgentCore 句柄本就是 Arc，见 ToolCtx.core）；不去动 wait_targets.rs 的内部结构。
    let exit_core = ctx.core.clone();
    let exit_log = log.clone();
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
        // 登记表置终态：进程退出即成功结束——service 的 exit code 语义目前仓库无消费方
        // （ServiceHandle 不留存退出码），臆造失败判定会凭空造出不存在的事实。
        // 摘要取环形日志尾，与 ticker 推送同口径。
        exit_core.wait_targets.finish(
            &svc2,
            TargetState::Succeeded,
            Some(exit_log.tail(2000)),
            None,
        );
        sink2.emit(
            &session2,
            "service:update",
            json!({"session":session2,"id":svc2,"exited":true}),
        );
    });
    // 目标模式已删除：owner 取 ctx.rt 自身（服务表按 owner_root_id 清理）。
    let owner = ctx.rt.clone();
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
    // 入表成功后才登记为可等待目标（两条容量早退路径不登记：它们要么已停掉、要么已撤销）
    ctx.core
        .wait_targets
        .register(id.clone(), TargetKind::Service, None);
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

    // ===== 目标档服务测试已整体移除 =====

    #[test]
    fn ring_caps_size() {
        let r = RingLog::new();
        r.push(&vec![b'x'; RING_CAP + 1000]);
        assert!(r.len() <= RING_CAP);
    }
}
