use super::drive::run_chat;
use crate::core::config::ConfigState;
use crate::core::context::ContextBreakdown;
use crate::core::sessions::SessionStore;
use crate::core::types::{Content, Message, Role, SessionId};
use crate::tools::registry::ToolRegistry;
use crate::util::throttle::ThrottledStream;
use dashmap::DashMap;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// 单次 run 的步数硬上限（防模型无限工具循环；9999 实际由空响应/完成条件先行终止）。
pub const MAX_STEPS: usize = 9999;
/// 每步注入队列的消化上限（防一次注入洪峰撑爆历史）。
pub const INJECT_BUFFER: usize = 32;
/// 流式帧节流刷新间隔（毫秒）。
pub const STREAM_THROTTLE_MS: u64 = 64;
/// 每隔多少步做一次历史检查点落盘。
pub const CHECKPOINT_EVERY_STEPS: usize = 20;

/// 只供本 run 消费的注入信封；确认成功时消息已进入历史并尝试过 checkpoint。
pub(super) struct InjectMessage {
    pub(super) message: Message,
    pub(super) receipt: oneshot::Sender<Result<(), String>>,
}

/// 所有退出（含 panic）关闭接纳窗口并拒绝尚未消化的消息，避免跨 run 滞留。
pub(super) struct InjectionGuard<'a>(&'a SessionRuntime);
impl Drop for InjectionGuard<'_> {
    fn drop(&mut self) {
        self.0.close_injections();
    }
}

/// 高频帧（走 IPC 通道；点对点、有序）。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// gen = 节流代数：run:retry 后前端按 generation 丢弃旧次尝试的半刷残帧（评审 C2）
    DeltaText {
        #[serde(rename = "gen")]
        generation: u64,
        text: String,
    },
    /// 思考流增量（gen 语义同 DeltaText）
    DeltaThinking {
        #[serde(rename = "gen")]
        generation: u64,
        text: String,
    },
    /// 工具执行进度块（batch = 批次 id，index = 批内序号，name = 工具名：前端运行中占位卡回填显示用）
    ToolProgress {
        batch: String,
        index: usize,
        chunk: String,
        name: String,
    },
    /// 本轮 LLM 用量（可选耗时字段：仅本 step **成功那次尝试**的窗口；缺省 = 无数据，
    /// 前端不累加不显示，旧后端/旧读者不受影响，[docs/composer-token-rate](../../../../docs/composer-token-rate.md)）
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        /// 该 step 成功尝试：请求发出 → 流失结束（不含失败尝试与退避）
        #[serde(default)]
        duration_ms: Option<u64>,
        /// 该 step 成功尝试：请求发出 → 首个任意类型 delta（含 thinking）
        #[serde(default)]
        ttft_ms: Option<u64>,
    },
    /// 子代理帧信封：子代理的流式帧借父会话通道下发；前端按 sub_id 路由进该子代理自己的
    /// 消息流（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)）。
    /// 仅 host 层在子通道上发送时包装。
    Sub { sub_id: String, frame: Box<Frame> },
}

/// 事件汇抽象：core 只依赖此 trait（host 注入 Tauri 实现）。
pub trait EventSink: Send + Sync {
    /// 下发一条流式帧到指定会话通道。
    fn channel_frame(&self, session: &SessionId, frame: &Frame);
    /// 发一条具名事件（29 键契约面）到指定会话。
    fn emit(&self, session: &SessionId, event: &str, payload: serde_json::Value);
    /// 子代理启动时把它绑定到父会话通道（默认 no-op：测试 sink 与非 Tauri 环境无通道）。
    /// 绑定后该子的 channel_frame 帧由 host 包装为 Frame::Sub 转发到父通道。
    fn bind_sub_channel(&self, _parent: &SessionId, _sub: &SessionId) {}
    /// 子代理结束时解绑（与 bind_sub_channel 配对；默认 no-op 同上）。
    /// 缺失此步会让 subs 注册表只增不减：过期 sub_id 永久残留（内存泄漏）且帧可能误投。
    fn unbind_sub_channel(&self, _sub: &SessionId) {}
}

/// 会话运行时：一个会话的全部可变状态（历史/计划 todos/偏好/取消/互斥/流缓冲等），
/// 以 Arc 在 core/host/tools 间共享。字段自带锁，跨 await 持锁一律短临界区。
pub struct SessionRuntime {
    pub id: SessionId,
    pub workspace: PathBuf,
    pub data_dir: PathBuf,
    /// 所属项目（None = 自由会话）
    pub project_id: Option<String>,
    /// 创建时快照的全部可读写根（含主目录；跨根解析 / @提及 / 文件树的唯一数据源）
    pub roots: Vec<String>,
    /// 项目托管目录（<主目录>/.codewave；自由会话为 None）
    pub project_dir: Option<PathBuf>,
    /// 会话标题（rename/title 子代理共用一把锁）
    pub title: Mutex<String>,
    /// 会话历史（转录语义；写入时经 stamped() 盖时间戳）
    pub history: Mutex<Vec<Message>>,
    /// 额外工作区根（多目录项目的附加目录）
    pub extra_roots: Mutex<Vec<String>>,
    /// plan 工具的 todo 状态机（内存 + 边车持久化）
    pub todos: Mutex<Vec<crate::tools::plan::Todo>>,
    /// 运行中注入通道的发送端（run.ts 队列消息入此）
    inject_tx: mpsc::Sender<InjectMessage>,
    /// 注入通道接收端（run 期间被 drive_agent 取走消化）
    inject_rx: Mutex<mpsc::Receiver<InjectMessage>>,
    /// 与 inject_rx 的锁共同使用；不能单独用此标志判定 IPC 接纳成功。
    inject_accepting: AtomicBool,
    /// 运行互斥（同一会话同时只允许一个 run 主循环持有）
    pub run_lock: Arc<tokio::sync::Mutex<()>>,
    /// 是否有 run 进行中（start_chat 以 swap 抢占，结束时恒复位）
    pub running: Arc<AtomicBool>,
    /// 压缩互斥（TOCTOU 修复）：手动/自动压缩期间置位；期间 start_chat 与二次压缩被拒
    pub compacting: AtomicBool,
    /// 文件操作互斥（edit 工具的进程级文件写互斥）
    pub file_ops: Arc<tokio::sync::Mutex<()>>,
    /// 工具并发信号量（单批次内至多 4 个工具并行）
    pub sem_tools: Arc<Semaphore>,
    /// 待应答的 ask/审批 waiter：ask_id → 应答通道
    pub asks: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
    /// 文本形态 ask 兜底恢复的原始块（[docs/text-form-ask-fallback]）：drive 兜底时写入，
    /// AskTool 打开询问卡时**取走**（take）并随 `ask:opened` 的 `text_recovered` 下发——
    /// 流式帧已把这段协议原文送到前端且无法回收，前端据此从当轮气泡里剔掉它。
    /// 每 run 至多一处（兜底本身每 run 一次）。
    pub text_ask_block: Mutex<Option<String>>,
    /// 流式节流缓冲（64ms 窗口聚合帧）
    pub stream: Arc<ThrottledStream>,
    /// 当前 run 的取消 token（空闲为 None）
    pub active_cancel: Mutex<Option<CancellationToken>>,
    /// breakdown 结果缓存（30s）
    pub breakdown_cache: Mutex<Option<(std::time::Instant, ContextBreakdown)>>,
    /// 路径清单缓存（30s）
    pub paths_cache: Mutex<Option<(std::time::Instant, Vec<String>)>>,
    /// 本 run 是否已注入计划瞬态快照（每 run 复位）
    pub injected_plan_snapshot_for_run: std::sync::atomic::AtomicBool,
    /// 上游拒收回传 `reasoning_content`（not accepted / unrecognized field）的会话级粘性标记：
    /// 命中一次后本会话出网副本不再回传思考（[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）。
    /// 与 `sanitize`（一次性历史修复）不同，这是「该端点不认该字段」的学习结果，跨 step / 跨 run 保持；
    /// 收到 `Demanded` 分类时由 `drive::update_reasoning_sticky` 复位（自愈阀，防误判 / 切端点后永久剥思考）。
    /// 子代理 runtime 由 `new_sub` 继承父会话当前值（同一进程内同一 provider 的端点特性一致）。
    pub reasoning_rejected: AtomicBool,
    /// 本 run 是否已发过「计划无进行中条目」软提醒（原子 CAS 每 run 至多一次；run_chat 起点复位）
    pub plan_hint_emitted: std::sync::atomic::AtomicBool,
    /// 本 run 是否**成功调用过 plan 工具**（即真的改过计划；run_chat 起点复位）
    ///
    /// 存在的意义（[docs/main-run-finish-with-pending-todos](../../../../docs/main-run-finish-with-pending-todos.md) §8.2）：
    /// `rt.todos` 跨 run 持久化且 run 开头从磁盘装载，而生产路径里唯一写者是 plan 工具——
    /// 故本标记能唯一区分「本 run 的计划」与「上个 run 留下的陈旧计划」。计划未收尾的收尾门
    /// （`text_turn_action` 的 `todos_pending` 与 suggest 的 `E_PLAN_PENDING`）**只看本标记**：
    /// 没碰过计划就不拦，否则用户在与计划无关的普通提问会被旧计划劫持成多轮工具循环。
    ///
    /// 置位点：`tools::plan` 写 `rt.todos` 成功后（只计成功调用——被档位排除/参数非法的
    /// 调用并未改变计划，不应把模型拖进续跑门）。
    pub plan_called_this_run: std::sync::atomic::AtomicBool,
    /// 会话已删除（评审 H5）：置位后迟到的 run 收尾不得再检查点/写日志复活幽灵会话
    pub zombie: AtomicBool,
    /// 会话级运行偏好（审批档位 / 模型 / 思考力度；内存态，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
    pub prefs: Mutex<crate::core::prefs::SessionPrefs>,
    /// 主会话偏好的状态过渡与边车持久化共用此锁，防旧模型迁移覆盖并发切档。
    pub prefs_persist_lock: Mutex<()>,
    /// G2（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) §7）：plan 档分析产物标志——pm/tester 子代理成功返回时置位。
    /// 内存态：重启丢失 = 退化为无门禁语义，无安全回退（只读由 G5 独立保证）。
    pub analysis_done: std::sync::atomic::AtomicBool,
    /// G2：因缺分析产物被拒的审批 ask 次数（会话累计；≥2 次后错误文案升级）
    pub analysis_gate_denials: std::sync::atomic::AtomicUsize,
    /// G3：批准时刻 todo 标题的冻结快照（执行范围基线；None = 未批准 / 历史会话）
    pub approved_plan: Mutex<Option<Vec<String>>>,
    /// G3：执行期间计划更新 diff 出新 todo（置位后下一次写操作前先弹范围确认）
    pub scope_expanded: std::sync::atomic::AtomicBool,
    /// G3：用户对计划外步骤选了「本次会话允许」（此后新增静默放行）
    pub scope_allowed: std::sync::atomic::AtomicBool,
    /// G3 范围确认的单飞锁：同批并发写的多个写工具只弹一张确认卡
    /// （command 工具是 ReadOnly、走并发闸，逐 call 求值会重复弹窗）。
    pub scope_gate_lock: tokio::sync::Mutex<()>,
    /// 产物归属（[docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)）：子代理 runtime 指向主会话 id，写操作登记到主会话名下；
    /// 主会话 / 任务运行 = None
    pub root_session_id: Option<String>,
    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：任务运行的隔离 runtime（无产物消费方；跳过登记避免边车泄漏）
    pub is_task_runtime: bool,
    /// 是否主会话 runtime（真正的用户会话）：子代理/任务运行为 false——checkpoint
    /// 据此早退，绝不把内部运行 upsert 进主索引（untitled 幽灵会话缺陷修复）。
    pub is_main_session: bool,
    /// 本 run 已执行的真实步数（drive_agent 每步 store）。sub:step 进度采样以此为准：
    /// history.len() 口径每步增约 2 条消息，60 步跑满会显示成 120+（前端 120/60 失真缺陷）
    pub step_count: std::sync::atomic::AtomicUsize,
    /// 最近一次请求上游回报的输入 token 数（0 = 尚无回报）。自动压缩触发同时看它：
    /// 本地估算对某些内容会少算数倍（图片 base64 实测少算 2.3 倍），只看估算会一路顶到
    /// 上游上限才被 400 拒绝（会话 6bca80f4）。压缩成功后复位为 0。
    pub last_input_tokens: std::sync::atomic::AtomicU64,
    /// 本 run 冻结的 system prompt 稳定主块（build_stream_request 首步组装后冻结，run 内复用）：
    /// 防 run 中途文件变更（如 agent 自己编辑 AGENTS.md）打穿 provider 前缀缓存，同时省每步 10+ 文件重读；
    /// drive_agent 每个 run 清空——文件变更从「下一步生效」变「下一条用户消息生效」（与子代理 spawn 冻结一致）
    pub system_frozen: Mutex<Option<String>>,
    /// Anthropic 历史代际断点锚点（req.messages 下标；滞回前移，见 prompt-caching-hardening 批次），
    /// drive_agent 每个 run 清空
    pub cache_gen_anchor: Mutex<Option<usize>>,
    /// 本 run 的生成耗时/TTFT 观测（[docs/composer-token-rate](../../../../docs/composer-token-rate.md)）：
    /// drive_agent 每 run 复位、run_llm_turn 每个成功 step 累积；run 收尾时随 usage 记录落盘。
    /// 走 runtime 同通道回传（`drive_agent` 返回签名不变，子代理/测试的 3 元组解构不受影响）。
    pub run_timing: Mutex<crate::core::stats::UsageTiming>,
}

impl SessionRuntime {
    /// 开启当前 run 的接纳窗口。与发送/收尾使用同一锁，避免检查标志后发送的 TOCTOU。
    pub(super) fn begin_injections(&self) -> InjectionGuard<'_> {
        let _gate = super::guards::lock_ok(&self.inject_rx);
        self.inject_accepting.store(true, Ordering::SeqCst);
        InjectionGuard(self)
    }

    /// 入队只是候选接纳；调用方必须等收据，不能把 try_send 成功当成已经进入历史。
    pub(super) fn enqueue_injection(
        &self,
        text: String,
    ) -> Result<oneshot::Receiver<Result<(), String>>, String> {
        let _gate = super::guards::lock_ok(&self.inject_rx);
        if !self.inject_accepting.load(Ordering::SeqCst) || self.zombie.load(Ordering::SeqCst) {
            return Err("本 run 已收尾或尚未开始，消息未注入，请保留队列项".into());
        }
        let (receipt, rx) = oneshot::channel();
        self.inject_tx
            .try_send(InjectMessage {
                message: Message::user_text(text),
                receipt,
            })
            .map_err(|e| format!("注入失败：{e}"))?;
        Ok(rx)
    }

    /// IPC 唯一注入入口；成功 = 消息已进入本 run 历史，失败 = 消息未被消费。
    pub async fn inject_message(&self, text: String) -> Result<(), String> {
        self.enqueue_injection(text)?
            .await
            .map_err(|_| "本 run 已终止，消息未注入，请保留队列项".to_string())?
    }

    /// 步首与收尾前复用；空队列收尾和发送在同一临界区线性化。
    pub(super) fn take_injections(&self, close_if_empty: bool) -> Vec<InjectMessage> {
        let mut rx = super::guards::lock_ok(&self.inject_rx);
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        if close_if_empty && out.is_empty() {
            self.inject_accepting.store(false, Ordering::SeqCst);
        }
        out
    }

    /// 取消、错误、步数/纯文本上限及 panic 的统一兜底；未消费项不带到下一个 run。
    pub(super) fn close_injections(&self) {
        let mut rx = super::guards::lock_ok(&self.inject_rx);
        self.inject_accepting.store(false, Ordering::SeqCst);
        while let Ok(msg) = rx.try_recv() {
            let _ = msg
                .receipt
                .send(Err("本 run 已收尾，消息未注入，请保留队列项".into()));
        }
    }

    /// 构造最小 runtime（Arc 包装；其余字段由 get_or_create_session / new_sub / new_task 补齐）。
    pub(super) fn new(id: SessionId, workspace: PathBuf, data_dir: PathBuf) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(INJECT_BUFFER);
        Arc::new(SessionRuntime {
            id,
            workspace,
            data_dir,
            project_id: None,
            roots: Vec::new(),
            project_dir: None,
            title: Mutex::new(String::new()),
            history: Mutex::new(Vec::new()),
            extra_roots: Mutex::new(Vec::new()),
            todos: Mutex::new(Vec::new()),
            inject_tx: tx,
            inject_rx: Mutex::new(rx),
            inject_accepting: AtomicBool::new(false),
            run_lock: Arc::new(tokio::sync::Mutex::new(())),
            running: Arc::new(AtomicBool::new(false)),
            compacting: AtomicBool::new(false),
            file_ops: Arc::new(tokio::sync::Mutex::new(())),
            sem_tools: Arc::new(Semaphore::new(4)),
            asks: Mutex::new(HashMap::new()),
            text_ask_block: Mutex::new(None),
            stream: Arc::new(ThrottledStream::new()),
            active_cancel: Mutex::new(None),
            breakdown_cache: Mutex::new(None),
            paths_cache: Mutex::new(None),
            injected_plan_snapshot_for_run: std::sync::atomic::AtomicBool::new(false),
            reasoning_rejected: AtomicBool::new(false),
            plan_hint_emitted: std::sync::atomic::AtomicBool::new(false),
            plan_called_this_run: std::sync::atomic::AtomicBool::new(false),
            zombie: AtomicBool::new(false),
            prefs: Mutex::new(crate::core::prefs::SessionPrefs::default()),
            prefs_persist_lock: Mutex::new(()),
            analysis_done: std::sync::atomic::AtomicBool::new(false),
            analysis_gate_denials: std::sync::atomic::AtomicUsize::new(0),
            approved_plan: Mutex::new(None),
            scope_expanded: std::sync::atomic::AtomicBool::new(false),
            scope_allowed: std::sync::atomic::AtomicBool::new(false),
            scope_gate_lock: tokio::sync::Mutex::new(()),
            root_session_id: None,
            is_task_runtime: false,
            is_main_session: false,
            step_count: std::sync::atomic::AtomicUsize::new(0),
            last_input_tokens: std::sync::atomic::AtomicU64::new(0),
            system_frozen: Mutex::new(None),
            cache_gen_anchor: Mutex::new(None),
            run_timing: Mutex::new(crate::core::stats::UsageTiming::default()),
        })
    }

    /// 当前会话偏好的快照。
    pub fn prefs(&self) -> crate::core::prefs::SessionPrefs {
        self.prefs.lock().unwrap().clone()
    }

    /// 整体替换会话偏好（前端 set_session_prefs；前端 Tab.prefs 是事实源）。
    pub fn set_prefs(&self, p: crate::core::prefs::SessionPrefs) {
        *self.prefs.lock().unwrap() = p;
    }

    /// 任务运行专用隔离 runtime（无注入队列语义；is_task_runtime 置位后跳过产物登记）。
    /// 无父 runtime 可继承（签名不含 parent，且可来自 scheduler 的任意项目）——`reasoning_rejected`
    /// 从 false 起步，首个 400 自行分类置位。
    pub fn new_task(
        id: String,
        data_dir: PathBuf,
        workspace: PathBuf,
        _data_dir2: PathBuf,
    ) -> Arc<Self> {
        let mut rt = Self::new(id, workspace, data_dir);
        if let Some(r) = Arc::get_mut(&mut rt) {
            r.is_task_runtime = true;
        }
        rt
    }

    /// 子代理 runtime：独立历史与取消树；复用父会话的 workspace/data_dir 与项目快照。
    pub fn new_sub(parent: &SessionRuntime, sub_id: String) -> Arc<Self> {
        let mut rt = Self::new(sub_id, parent.workspace.clone(), parent.data_dir.clone());
        if let Some(r) = Arc::get_mut(&mut rt) {
            r.project_id = parent.project_id.clone();
            r.roots = parent.roots.clone();
            r.project_dir = parent.project_dir.clone();
            // 继承父会话的粘性剥思考标记（[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）：
            // 同一进程内同一 provider 的端点特性一致，子代理不必在首个请求上再撞一次 400。
            // 继承的是「当前值」：父此后由 400 分类自愈（`Demanded` 复位）时已存在的子 runtime 不跟随
            // ——下一条 400 会让子自己重新分类（对称自愈，无需父子间反向回写）。
            r.reasoning_rejected =
                AtomicBool::new(parent.reasoning_rejected.load(Ordering::SeqCst));
            // [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：子代理写登记归属主会话（父本身也是子代理时取其归属，
            // 防嵌套链条断裂）
            r.root_session_id = Some(
                parent
                    .root_session_id
                    .clone()
                    .unwrap_or_else(|| parent.id.clone()),
            );
        }
        // 继承 extra roots 与会话偏好（审批档位 / 模型 / 力度对子代理语义相同，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
        *rt.extra_roots.lock().unwrap() = parent.extra_roots.lock().unwrap().clone();
        *rt.prefs.lock().unwrap() = parent.prefs.lock().unwrap().clone();
        rt
    }

    /// 开一个待应答的 ask/审批（事件下发是调用方的职责）。
    pub fn open_ask(&self, ask_id: &str, responder: oneshot::Sender<serde_json::Value>) {
        self.asks
            .lock()
            .unwrap()
            .insert(ask_id.to_string(), responder);
    }

    /// 兑现一个待应答的 ask；有 waiter 收到值时返回 true。
    pub fn resolve_ask(&self, ask_id: &str, value: serde_json::Value) -> bool {
        match self.asks.lock().unwrap().remove(ask_id) {
            Some(tx) => {
                let _ = tx.send(value);
                true
            }
            _ => false,
        }
    }

    /// 取消当前 run（空闲时无操作）。
    pub fn cancel_active(&self) {
        if let Some(t) = self.active_cancel.lock().unwrap().as_ref() {
            t.cancel();
        }
    }

    /// 整体写 prefs（仅内存态；持久化由 `AgentCore::transition_prefs` 负责）。
    pub fn transition_prefs(&self, new: crate::core::prefs::SessionPrefs) -> bool {
        *self.prefs.lock().unwrap() = new;
        false
    }
}

/// Agent 编排核心：配置 + 会话表 + 各子系统（工具注册表/存储/键池/MCP/统计/任务表）的聚合根，
/// 由 host 层构造并以 Arc 全局共享。
pub struct AgentCore {
    /// 全局配置（RwLock；save_config 热更新写、各处读）
    pub cfg: std::sync::RwLock<ConfigState>,
    /// 活跃会话运行时表：session id → runtime
    pub sessions: DashMap<SessionId, Arc<SessionRuntime>>,
    /// 事件汇（host 注入）
    pub sink: Arc<dyn EventSink>,
    /// 内置工具注册表
    pub tools: Arc<ToolRegistry>,
    /// 会话持久化存储
    pub store: Arc<SessionStore>,
    /// HTTP 客户端（provider 层共用；RwLock 支持代理配置变更后由 save_config 热替换——
    /// 读取方读锁 clone（reqwest::Client 内部 Arc，clone 廉价），std 锁不可跨 await）
    pub client: std::sync::RwLock<reqwest::Client>,
    /// 全局数据目录 ~/.codewave
    pub data_dir: PathBuf,
    /// service 工具的后台进程表
    pub services: crate::tools::service::ServiceTable,
    /// 技能索引（TTL 缓存）
    pub skills: crate::skills::SkillIndex,
    /// 多 key 池（健康表按 provider 隔离）
    pub key_pool: crate::provider::keys::KeyPool,
    /// MCP 客户端管理器
    pub mcp: std::sync::Arc<crate::mcp::McpManager>,
    /// token 统计收集器
    pub stats: std::sync::Arc<crate::core::stats::StatsCollector>,
    /// 计划任务表（P2-G）
    pub tasks: std::sync::Arc<crate::core::scheduler::TaskTable>,
    /// 运行中的子代理（sub_id → runtime；StopSubagent 使用）
    pub subs: DashMap<String, Arc<SessionRuntime>>,
    /// Serialize start checks across sessions sharing a working directory.
    pub start_gate: Mutex<()>,
    /// 可等待目标登记表（`wait` 工具 `until` 条件等待的数据面）。
    /// 独立于 services/subs/tasks 三张表：子代理完成即从 subs 移除、service 自然退出不移除、
    /// 计划任务无 running 中间态——反查既有表都分不清「已完成」与「不存在」。
    /// （[docs/wait-conditional-wait](../../../docs/wait-conditional-wait.md)）
    pub wait_targets: crate::core::wait_targets::WaitTargets,
}

impl AgentCore {
    /// 构造核心（tools/skills/key_pool/mcp/stats/tasks 均取默认实现）。
    pub fn new(
        cfg: ConfigState,
        sink: Arc<dyn EventSink>,
        store: Arc<SessionStore>,
        client: reqwest::Client,
        data_dir: PathBuf,
    ) -> Self {
        AgentCore {
            cfg: std::sync::RwLock::new(cfg),
            sessions: DashMap::new(),
            sink,
            tools: Arc::new(ToolRegistry::default_tools()),
            store,
            client: std::sync::RwLock::new(client),
            data_dir: data_dir.clone(),
            services: crate::tools::service::ServiceTable::default(),
            skills: crate::skills::SkillIndex::new(),
            key_pool: crate::provider::keys::KeyPool::default(),
            mcp: std::sync::Arc::new(crate::mcp::McpManager::default()),
            stats: std::sync::Arc::new(crate::core::stats::StatsCollector::new(data_dir.clone())),
            tasks: std::sync::Arc::new(crate::core::scheduler::TaskTable::new(data_dir.clone())),
            subs: DashMap::new(),
            start_gate: Mutex::new(()),
            wait_targets: crate::core::wait_targets::WaitTargets::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// 取会话 runtime，缺席则创建并登记（初始偏好取自全局配置）。
    pub fn get_or_create_session(
        &self,
        id: &str,
        workspace: PathBuf,
        project_id: Option<String>,
        roots: Vec<String>,
        project_dir: Option<PathBuf>,
        extra_roots: Vec<String>,
    ) -> Arc<SessionRuntime> {
        if let Some(rt) = self.sessions.get(id) {
            return rt.clone();
        }
        let saved_prefs = self.store.load_prefs(id);
        // 旧索引的 model_id 是“实际使用模型”，无法区分显式选择与跟随全局。
        // 缺新版边车时先保持跟随全局；前端恢复的 Tab 快照可经专用入口补回显式选择。
        let cfg = self.cfg.read().unwrap();
        let default_prefs = crate::core::prefs::SessionPrefs::from_config(&cfg);
        let mut initial_prefs = default_prefs.clone();
        initial_prefs.model_id = saved_prefs
            .as_ref()
            .filter(|saved| saved.model_choice_recorded)
            .and_then(|saved| saved.model_id.clone())
            .filter(|model_id| cfg.find_model(model_id).is_some());
        initial_prefs.reasoning_effort = saved_prefs
            .as_ref()
            .filter(|saved| saved.model_choice_recorded)
            .and_then(|saved| saved.reasoning_effort);
        drop(cfg);
        let mut rt = SessionRuntime::new(id.to_string(), workspace, self.data_dir.clone());
        if let Some(r) = Arc::get_mut(&mut rt) {
            r.project_id = project_id;
            r.roots = roots;
            r.project_dir = project_dir;
            r.is_main_session = true;
            *r.extra_roots.lock().unwrap() = extra_roots;
            *r.prefs.lock().unwrap() = initial_prefs;
        }
        self.persist_session_prefs_with_choice(
            &rt,
            saved_prefs
                .as_ref()
                .is_some_and(|saved| saved.model_choice_recorded),
        );
        self.sessions.insert(id.to_string(), rt.clone());
        rt
    }

    /// 按 id 查活跃会话 runtime。
    pub fn session(&self, id: &str) -> Option<Arc<SessionRuntime>> {
        self.sessions.get(id).map(|r| r.clone())
    }

    /// 启动一次 run。并发守卫：每会话同时只允许一个活跃 run。
    pub fn start_chat(
        self: &Arc<Self>,
        rt: Arc<SessionRuntime>,
        text: String,
        images: Vec<crate::core::prefs::ImageIn>,
    ) -> anyhow::Result<String> {
        let _start_guard = self.start_gate.lock().unwrap();
        if rt.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("该会话已有运行中的任务");
        }
        // 压缩互斥：压缩占位期间拒绝新 run（TOCTOU 守卫——running 检查通过后压缩
        // 仍可能整体替换历史）
        if rt.compacting.load(Ordering::SeqCst) {
            rt.running.store(false, Ordering::SeqCst);
            anyhow::bail!("上下文压缩进行中，请稍后重试");
        }
        {
            let cfg = self.cfg.read().unwrap();
            let prefs = rt.prefs();
            if crate::core::prefs::effective_model(&cfg, &prefs).is_none() {
                rt.running.store(false, Ordering::SeqCst);
                anyhow::bail!("请先在设置中配置并选择模型");
            }
        }
        // 兜底标题（需求 2.2：至多 10 字符——按字符数截断，中英文一视同仁）
        {
            let mut title = rt.title.lock().unwrap();
            if title.is_empty() {
                *title = text.chars().take(10).collect();
            }
        }
        let mut content = vec![Content::Text { text }];
        for img in images {
            content.push(Content::Image {
                media_type: img.mime,
                data: img.data,
            });
        }
        let user = if content.len() == 1 {
            Message::user_text(match &content[0] {
                Content::Text { text } => text.clone(),
                _ => String::new(),
            })
        } else {
            Message {
                role: Role::User,
                content,
                created_at: None,
            }
        };
        let run_id = uuid::Uuid::new_v4().to_string();
        let core = self.clone();
        tokio::spawn(run_chat(core, rt, user, run_id.clone()));
        Ok(run_id)
    }
    /// 用户显式切档的完整过渡（host `set_session_prefs` 的唯一入口）：core 内完成 prefs 写入 +
    /// 状态迁移（`SessionRuntime::transition_prefs`）+ 落边车。
    pub fn transition_prefs(
        &self,
        rt: &SessionRuntime,
        new: crate::core::prefs::SessionPrefs,
    ) -> bool {
        let _guard = rt.prefs_persist_lock.lock().unwrap();
        rt.transition_prefs(new);
        self.save_session_prefs_locked(rt, true);
        false
    }

    /// 仅改档位的后端路径（ask 批准 / 目标收尾）：在同一临界区读取当前模型，避免覆盖并发迁移。
    pub fn set_session_mode_and_persist(
        &self,
        rt: &SessionRuntime,
        mode: crate::core::prefs::ApprovalMode,
    ) {
        let _guard = rt.prefs_persist_lock.lock().unwrap();
        let mut prefs = rt.prefs();
        prefs.approval_mode = mode;
        rt.set_prefs(prefs);
        self.save_session_prefs_locked(rt, true);
    }

    /// 保存目标档活动标记、模型与力度；其它权限档及目标完成后的回落档按全局默认重建。
    pub fn persist_session_prefs(&self, rt: &SessionRuntime) {
        self.persist_session_prefs_with_choice(rt, true);
    }

    fn persist_session_prefs_with_choice(&self, rt: &SessionRuntime, model_choice_recorded: bool) {
        let _guard = rt.prefs_persist_lock.lock().unwrap();
        self.save_session_prefs_locked(rt, model_choice_recorded);
    }

    fn save_session_prefs_locked(&self, rt: &SessionRuntime, model_choice_recorded: bool) {
        let prefs = rt.prefs();
        let snapshot = crate::core::prefs::SessionPrefsSnapshot {
            model_choice_recorded,
            model_id: prefs.model_id,
            reasoning_effort: prefs.reasoning_effort,
        };
        if let Err(e) = self.store.save_prefs(&rt.id, &snapshot) {
            tracing::warn!("会话 {} 运行偏好落盘失败：{e}", rt.id);
        }
    }

    /// 旧版 ui-state Tab 的模型选择只迁移一次；已有新版边车时服务端选择优先。
    /// 只改模型与力度，权限档位始终保留当前 runtime 的安全恢复结果。
    pub fn restore_legacy_model_prefs(
        &self,
        rt: &SessionRuntime,
        model_id: Option<String>,
        reasoning_effort: Option<crate::core::prefs::EffortLevel>,
    ) -> anyhow::Result<()> {
        let _guard = rt.prefs_persist_lock.lock().unwrap();
        if self
            .store
            .load_prefs(&rt.id)
            .is_some_and(|saved| saved.model_choice_recorded)
        {
            return Ok(());
        }
        let mut prefs = rt.prefs();
        prefs.model_id = model_id.filter(|id| self.cfg.read().unwrap().find_model(id).is_some());
        prefs.reasoning_effort = reasoning_effort;
        rt.set_prefs(prefs.clone());
        self.store.save_prefs(
            &rt.id,
            &crate::core::prefs::SessionPrefsSnapshot {
                model_choice_recorded: true,
                model_id: prefs.model_id,
                reasoning_effort: prefs.reasoning_effort,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::prefs::{ApprovalMode, EffortLevel, SessionPrefs};
    use crate::core::types::SessionId;

    /// 记录事件的测试 sink（不关心具体事件名，仅作 runtime 装配）。
    #[derive(Default)]
    struct RecSink {
        events: Mutex<Vec<(String, String, serde_json::Value)>>,
    }

    impl EventSink for RecSink {
        fn channel_frame(&self, _s: &SessionId, _f: &Frame) {}
        fn emit(&self, s: &SessionId, e: &str, p: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((s.clone(), e.to_string(), p));
        }
    }

    /// 测试夹具：真实 store（边车落临时目录）+ 记录事件的 sink + 主会话 runtime。
    struct Harness {
        core: Arc<AgentCore>,
        rt: Arc<SessionRuntime>,
        sink: Arc<RecSink>,
        _ws: tempfile::TempDir,
        _dd: tempfile::TempDir,
    }

    fn harness() -> Harness {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let sink = Arc::new(RecSink::default());
        let store = Arc::new(SessionStore::new(dd.path().to_path_buf()));
        let core = Arc::new(AgentCore::new(
            crate::core::config::ConfigState::default(),
            sink.clone(),
            store,
            reqwest::Client::new(),
            dd.path().to_path_buf(),
        ));
        let rt = core.get_or_create_session(
            "prefs-transition",
            std::fs::canonicalize(ws.path()).unwrap(),
            None,
            vec![],
            None,
            vec![],
        );
        Harness {
            core,
            rt,
            sink,
            _ws: ws,
            _dd: dd,
        }
    }

    fn prefs_of(mode: ApprovalMode) -> SessionPrefs {
        SessionPrefs {
            approval_mode: mode,
            model_id: None,
            reasoning_effort: None,
        }
    }

    fn configure_two_models(h: &Harness) {
        let mut cfg = h.core.cfg.write().unwrap();
        cfg.providers.push(crate::core::config::ProviderConfig {
            id: "provider-test".into(),
            models: vec![
                crate::core::config::ProviderModel {
                    id: "m-global".into(),
                    ..Default::default()
                },
                crate::core::config::ProviderModel {
                    id: "m-session".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        cfg.active_model_id = Some("m-global".into());
    }

    fn rebuild(h: &Harness) -> Arc<SessionRuntime> {
        h.core.sessions.remove(&h.rt.id);
        h.core
            .get_or_create_session(&h.rt.id, h.rt.workspace.clone(), None, vec![], None, vec![])
    }

    fn write_legacy_model_index(h: &Harness) {
        h.core
            .store
            .save_history(
                &h.rt.id,
                "模型恢复",
                &h.rt.workspace.to_string_lossy(),
                Some("m-session"),
                None,
                &[],
                &[Message::user_text("旧消息")],
            )
            .unwrap();
    }

    #[test]
    fn session_model_and_effort_survive_restart_without_restoring_full_access() {
        let h = harness();
        configure_two_models(&h);
        h.core.transition_prefs(
            &h.rt,
            SessionPrefs {
                approval_mode: ApprovalMode::FullAccess,
                model_id: Some("m-session".into()),
                reasoning_effort: Some(EffortLevel::Max),
            },
        );
        let rt = rebuild(&h);
        assert_eq!(rt.prefs().approval_mode, ApprovalMode::Plan);
        assert_eq!(rt.prefs().model_id.as_deref(), Some("m-session"));
        assert_eq!(rt.prefs().reasoning_effort, Some(EffortLevel::Max));
        assert_eq!(
            crate::core::prefs::effective_model(&h.core.cfg.read().unwrap(), &rt.prefs())
                .unwrap()
                .id,
            "m-session"
        );
    }

    #[test]
    fn explicit_follow_global_is_not_overridden_by_legacy_index() {
        let h = harness();
        configure_two_models(&h);
        write_legacy_model_index(&h);
        h.core.transition_prefs(&h.rt, prefs_of(ApprovalMode::Plan));
        let rt = rebuild(&h);
        assert!(rt.prefs().model_id.is_none());
        assert_eq!(
            crate::core::prefs::effective_model(&h.core.cfg.read().unwrap(), &rt.prefs())
                .unwrap()
                .id,
            "m-global"
        );
    }

    #[test]
    fn legacy_global_following_session_does_not_pin_index_model() {
        let h = harness();
        configure_two_models(&h);
        write_legacy_model_index(&h);
        std::fs::remove_file(h.core.store.prefs_path(&h.rt.id)).unwrap();
        let rt = rebuild(&h);
        assert!(rt.prefs().model_id.is_none());
        assert_eq!(
            crate::core::prefs::effective_model(&h.core.cfg.read().unwrap(), &rt.prefs())
                .unwrap()
                .id,
            "m-global"
        );
        h.core.restore_legacy_model_prefs(&rt, None, None).unwrap();
        assert!(rt.prefs().model_id.is_none());
        assert!(
            h.core
                .store
                .load_prefs(&rt.id)
                .unwrap()
                .model_choice_recorded
        );
    }

    #[test]
    fn migration_and_mode_change_preserve_whichever_model_choice_is_newer() {
        let h = harness();
        configure_two_models(&h);
        // 迁移先于 ask 切档：后端仅改 mode 的路径须保留迁移后的模型。
        h.core
            .restore_legacy_model_prefs(&h.rt, Some("m-session".into()), None)
            .unwrap();
        h.core
            .set_session_mode_and_persist(&h.rt, ApprovalMode::AutoEdit);
        assert_eq!(h.rt.prefs().approval_mode, ApprovalMode::AutoEdit);
        assert_eq!(h.rt.prefs().model_id.as_deref(), Some("m-session"));

        // 显式更新先于迟到的旧快照：迁移为空操作，档位与模型均不倒退。
        h.core.transition_prefs(
            &h.rt,
            SessionPrefs {
                approval_mode: ApprovalMode::FullAccess,
                model_id: Some("m-global".into()),
                reasoning_effort: Some(EffortLevel::High),
            },
        );
        h.core
            .restore_legacy_model_prefs(&h.rt, Some("m-session".into()), None)
            .unwrap();
        assert_eq!(h.rt.prefs().approval_mode, ApprovalMode::FullAccess);
        assert_eq!(h.rt.prefs().model_id.as_deref(), Some("m-global"));
        assert_eq!(h.rt.prefs().reasoning_effort, Some(EffortLevel::High));
    }

    #[test]
    fn removed_session_model_falls_back_to_global_model() {
        let h = harness();
        configure_two_models(&h);
        h.core.transition_prefs(
            &h.rt,
            SessionPrefs {
                model_id: Some("m-session".into()),
                reasoning_effort: Some(EffortLevel::High),
                ..prefs_of(ApprovalMode::Plan)
            },
        );
        h.core.cfg.write().unwrap().providers[0].models.pop();
        let rt = rebuild(&h);
        assert!(rt.prefs().model_id.is_none());
        assert_eq!(rt.prefs().reasoning_effort, Some(EffortLevel::High));
        assert_eq!(
            crate::core::prefs::effective_model(&h.core.cfg.read().unwrap(), &rt.prefs())
                .unwrap()
                .id,
            "m-global"
        );
    }
}
