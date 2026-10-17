use super::drive::run_chat;
use crate::core::config::ConfigState;
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
    /// 根会话新 LLM step 的边界；同一步的重试只使用 run:retry，不重复发送。
    StepStart {
        #[serde(rename = "gen")]
        generation: u64,
    },
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
    /// 前端不累加不显示，旧后端/旧读者不受影响，[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）
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
        /// 当前上下文占用（上游回报的真实输入量口径，[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）。
        ///
        /// 与 `input` 的区别必须写清：`input` 是**这一步请求带了多少**，会随历史增长；
        /// 这里是「上下文里现在有多少」，也就是自动压缩与客户端进度条要的那个数。
        #[serde(default)]
        context_used: Option<u64>,
        /// 生效模型的上下文窗口（与压缩判定同一事实源，不另算）。
        ///
        /// 没有它，宿主只能得到没有分母的 `used`，无法展示上下文占比，
        /// 「上下文还剩多少」就退化成一个猜的数。
        #[serde(default)]
        context_window: Option<u32>,
    },
    /// 子代理帧信封：子代理的流式帧借父会话通道下发；前端按 sub_id 路由进该子代理自己的
    /// 消息流（[docs/subagents](../../../../docs/subagents.md)）。
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

/// 可回查的 run 终态；前端据此区分队列可续跑的成功与需要停下的错误/取消。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTerminalKind {
    Done,
    Error,
    Cancelled,
}

/// 最近 run 的完整收尾回执，与事件载荷共用字段供界面回填。
#[derive(Debug, Clone, Serialize)]
pub struct RunTerminal {
    pub run_id: String,
    pub kind: RunTerminalKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_save: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_run: Option<crate::core::sessions::run_display::RunDisplay>,
    /// 完成门摘要（#16）：`Done` **不等于**验收通过，界面据此显示"未通过 / 还差什么"。
    /// 只增可选字段——旧会话缺它时按"未验证"显示，不补造结论。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<serde_json::Value>,
}

impl RunTerminal {
    pub(super) fn new(run_id: &str, kind: RunTerminalKind) -> Self {
        Self {
            run_id: run_id.into(),
            kind,
            suggestions: None,
            error: None,
            error_kind: None,
            history_save: None,
            task_run: None,
            acceptance: None,
        }
    }
}

/// 只保留最近一轮；与 running 标志在同一临界区发布，避免跨轮拼接。
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunSnapshot {
    pub running: bool,
    pub run_id: Option<String>,
    pub terminal: Option<RunTerminal>,
    #[serde(skip)]
    pub(super) clock: Option<RunClock>,
}

#[derive(Debug, Clone)]
pub(super) struct RunClock {
    pub started_at: String,
    pub instant: std::time::Instant,
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
    /// 本 run 绑定的交付契约任务 id（[#17](../../../../docs/subagents.md)）。
    ///
    /// 由**宿主**在派发子代理时从已登记契约解析写入，**不是**模型每次调用现填的——
    /// `submit_delivery` 的 `task_id` / `owner` / `contract_revision` 都据此确定，
    /// 模型只能在契约内部声明完成项与未完成项。`None` 表示本次派发没有绑定契约
    /// （旧任务 / 问答 / 机械修改），此时结构化提交不可用，文本 `<report>` 仍是唯一收尾。
    pub delivery_task_id: Mutex<Option<String>>,
    pub delivery_requirement_ids: Mutex<Vec<String>>,
    pub delivery_review_assignment_id: Mutex<Option<String>>,
    pub delivery_verification_task_id: Mutex<Option<String>>,
    pub delivery_check_ids: Mutex<Option<Vec<String>>>,
    pub pending_workflow: Mutex<Option<crate::core::workflow::service::PreparedWorkflow>>,
    pub pending_contract: Mutex<Option<crate::core::delivery::types::ContractDraft>>,
    /// 本次派发收到的结构化交付草稿（[#17](../../../../docs/subagents.md)）。
    ///
    /// `submit_delivery` 写入、宿主在**终态**取走并按真实 `ended` 落成不可变记录。
    /// 分两段是因为 `ended`（[四象限](../../../../docs/subagents.md)）只有在子代理收尾那一刻
    /// 才知道；子代理自己无权声明自己是 report 还是 partial。
    pub delivery_draft: Mutex<Option<crate::tools::submit_delivery::DeliveryDraft>>,
    /// 最近一次收尾的完成门裁决（[#16](../../../../docs/plan-and-approval.md)）。
    ///
    /// **运行终态不是验收结论**：`RunTerminalKind::Done` 只说明模型停了，能不能算交付
    /// 通过看这里。恢复会话时按显示用途读取，**不据此重跑任何工具**。
    pub last_completion: Mutex<Option<crate::core::delivery::CompletionDecision>>,
    /// Host ability, independent from approval mode. No UI never means approval.
    pub interactive: AtomicBool,
    pub interaction_blocked: Mutex<Option<String>>,
    /// Actual persistence and settlement facts for the current run.
    pub save_clean: AtomicBool,
    pub token_budget: Arc<std::sync::atomic::AtomicU64>,
    pub token_spent: Arc<std::sync::atomic::AtomicU64>,
    /// Serializes finite-budget provider attempts across the root and its descendants.
    pub token_budget_gate: Arc<tokio::sync::Mutex<()>>,
    pub token_budget_uncertain: Arc<AtomicBool>,
    pub cleanup_done: AtomicBool,
    pub workspace_guard: Mutex<Option<Arc<crate::core::process_coordination::WorkspaceGuard>>>,
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
    run_snapshot: Mutex<RunSnapshot>,
    /// 纯观察摘要的弱引用；终态发布后清理，不参与运行互斥或审批决策。
    activity: std::sync::Weak<crate::core::activity::ActivityState>,
    /// 会话 store 的弱引用：**只**用于在 `finish_run` 里释放 #36-C0 的 owner 租约。
    ///
    /// 为什么必须是弱引用、且必须挂在 runtime 上：owner 的释放点是「run 收尾」，
    /// 而 `finish_run` 是**唯一宿主无关**的收尾信号。挂在 `mark_running(false)`
    /// 上会漏——宿主忘了调，那个会话就永久 busy，用户再也跑不了它。
    owner_store: std::sync::Weak<crate::core::sessions::SessionStore>,
    /// 压缩互斥（TOCTOU 修复）：手动/自动压缩期间置位；期间 start_chat 与二次压缩被拒
    pub compacting: AtomicBool,
    /// 文件操作互斥（edit 工具的进程级文件写互斥）
    pub file_ops: Arc<tokio::sync::Mutex<()>>,
    /// 工具并发信号量（单批次内至多 4 个工具并行）
    pub sem_tools: Arc<Semaphore>,
    /// 待应答的 ask/审批 waiter：ask_id → 应答通道
    pub asks: Mutex<HashMap<String, oneshot::Sender<serde_json::Value>>>,
    /// 文本形态 ask 兜底恢复的原始块（[docs/chat-and-interface]）：drive 兜底时写入，
    /// AskTool 打开询问卡时**取走**（take）并随 `ask:opened` 的 `text_recovered` 下发——
    /// 流式帧已把这段协议原文送到前端且无法回收，前端据此从当轮气泡里剔掉它。
    /// 每 run 至多一处（兜底本身每 run 一次）。
    pub text_ask_block: Mutex<Option<String>>,
    /// 流式节流缓冲（64ms 窗口聚合帧）
    pub stream: Arc<ThrottledStream>,
    /// 同步串行化 take + 下发与 step 边界，避免 ticker 已取出的残帧越过边界。
    /// 锁序 emission → stream 内部锁；不跨 await，collector 不获取此锁。
    pub(super) stream_emission_lock: Mutex<()>,
    /// 当前 run 的取消 token（空闲为 None）
    pub active_cancel: Mutex<Option<CancellationToken>>,
    /// 路径清单缓存（30s）
    pub paths_cache: Mutex<Option<(std::time::Instant, Vec<String>)>>,
    /// 本 run 是否已注入计划瞬态快照（每 run 复位）
    pub injected_plan_snapshot_for_run: std::sync::atomic::AtomicBool,
    /// 上游拒收回传 `reasoning_content`（not accepted / unrecognized field）的会话级粘性标记：
    /// 命中一次后本会话出网副本不再回传思考（[docs/providers-and-network](../../../../docs/providers-and-network.md)）。
    /// 与 `sanitize`（一次性历史修复）不同，这是「该端点不认该字段」的学习结果，跨 step / 跨 run 保持；
    /// 收到 `Demanded` 分类时由 `drive::update_reasoning_sticky` 复位（自愈阀，防误判 / 切端点后永久剥思考）。
    /// 子代理 runtime 由 `new_sub` 继承父会话当前值（同一进程内同一 provider 的端点特性一致）。
    pub reasoning_rejected: AtomicBool,
    /// 本 run 是否已发过「计划无进行中条目」软提醒（原子 CAS 每 run 至多一次；run_chat 起点复位）
    pub plan_hint_emitted: std::sync::atomic::AtomicBool,
    /// 本 run 是否**成功调用过 plan 工具**（即真的改过计划；run_chat 起点复位）
    ///
    /// 存在的意义（[docs/runtime-and-scheduling](../../../../docs/runtime-and-scheduling.md)）：
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
    /// 会话级运行偏好（审批档位 / 模型 / 思考力度；内存态，[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）
    pub prefs: Mutex<crate::core::prefs::SessionPrefs>,
    /// 主会话偏好的状态过渡与边车持久化共用此锁，防旧模型迁移覆盖并发切档。
    pub prefs_persist_lock: Mutex<()>,
    /// G2（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：plan 档分析产物标志——pm/tester 子代理成功返回时置位。
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
    /// G3：已拒绝的新标题；相同范围继续阻断写通道，不重复询问。
    pub scope_denied_titles: Mutex<Vec<String>>,
    /// G3 范围确认的单飞锁：同批并发写的多个写工具只弹一张确认卡
    /// （command 工具是 ReadOnly、走并发闸，逐 call 求值会重复弹窗）。
    pub scope_gate_lock: tokio::sync::Mutex<()>,
    /// 产物归属（[docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)）：子代理 runtime 指向主会话 id，写操作登记到主会话名下；
    /// 主会话 / 任务运行 = None
    pub root_session_id: Option<String>,
    /// 子代理出生时冻结的根 run 身份；其自己的 RunSnapshot 不代表所属主 run。
    /// 嵌套子代理继续继承同一根身份，审批事件不能按父会话当前 run 回查。
    pub root_run_id: Option<String>,
    /// [docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)：任务运行的隔离 runtime（无产物消费方；跳过登记避免边车泄漏）
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
    pub instruction_usage: Mutex<Vec<crate::core::instruction_resources::ResourceUse>>,
    /// Anthropic 历史代际断点锚点（req.messages 下标；滞回前移，见 prompt-caching-hardening 批次），
    /// drive_agent 每个 run 清空
    pub cache_gen_anchor: Mutex<Option<usize>>,
    /// 本 run 的生成耗时/TTFT 观测（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）：
    /// drive_agent 每 run 复位、run_llm_turn 每个成功 step 累积；run 收尾时随 usage 记录落盘。
    /// 走 runtime 同通道回传（`drive_agent` 返回签名不变，子代理/测试的 3 元组解构不受影响）。
    pub run_timing: Mutex<crate::core::stats::UsageTiming>,
}

impl SessionRuntime {
    /// 读取运行标志与最近终态的同一时刻快照；不访问历史或磁盘。
    pub fn observation_store_owner(&self) -> String {
        self.root_session_id
            .as_ref()
            .map(|root| format!("{root}__{}", self.id))
            .unwrap_or_else(|| self.id.clone())
    }
    pub fn tool_observation_id(&self, batch: &str, index: usize) -> Option<String> {
        self.owner_store.upgrade().and_then(|store| {
            store.tool_observation_id(&self.observation_store_owner(), batch, index)
        })
    }

    pub fn run_snapshot(&self) -> RunSnapshot {
        let state = super::guards::lock_ok(&self.run_snapshot);
        let mut snapshot = state.clone();
        snapshot.running = self.running.load(Ordering::SeqCst);
        snapshot
    }

    /// 开一轮：`pub(super)` 让 `core::agent::tests` 能构造"已开跑"的状态
    /// （否则测不到 run 收尾路径，只能靠真起跑等退避，时序不稳）。
    pub(super) fn begin_run(&self, run_id: &str) -> bool {
        self.begin_run_expected(run_id, None)
    }

    fn begin_run_expected(&self, run_id: &str, expected: Option<&str>) -> bool {
        let mut snapshot = super::guards::lock_ok(&self.run_snapshot);
        if expected.is_some_and(|id| snapshot.run_id.as_deref() != Some(id)) {
            return false;
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return false;
        }
        *self.last_completion.lock().unwrap() = None;
        *self.interaction_blocked.lock().unwrap() = None;
        self.save_clean.store(false, Ordering::SeqCst);
        if self.is_main_session {
            self.token_spent.store(0, Ordering::SeqCst);
            self.token_budget_uncertain.store(false, Ordering::SeqCst);
            *self.delivery_task_id.lock().unwrap() = None;
            self.delivery_requirement_ids.lock().unwrap().clear();
            *self.delivery_check_ids.lock().unwrap() = None;
            *self.delivery_review_assignment_id.lock().unwrap() = None;
            *self.delivery_verification_task_id.lock().unwrap() = None;
        }
        self.cleanup_done.store(false, Ordering::SeqCst);
        *snapshot = RunSnapshot {
            running: true,
            run_id: Some(run_id.into()),
            terminal: None,
            clock: Some(RunClock {
                started_at: chrono::Utc::now().to_rfc3339(),
                instant: std::time::Instant::now(),
            }),
        };
        true
    }

    pub(crate) fn prepare_actor_run(&self, run_id: &str) -> bool {
        let snapshot = self.run_snapshot();
        if snapshot.running {
            return snapshot.run_id.as_deref() == Some(run_id);
        }
        self.begin_run(run_id)
    }

    pub(crate) fn finish_actor_run(&self, run_id: &str, failed: bool, cancelled: bool) {
        let kind = if cancelled {
            RunTerminalKind::Cancelled
        } else if failed {
            RunTerminalKind::Error
        } else {
            RunTerminalKind::Done
        };
        self.finish_run(RunTerminal::new(run_id, kind));
    }

    /// 旧轮迟到收尾不得覆盖新轮；终态完成保存后才允许下一轮启动。
    pub(super) fn finish_run(&self, terminal: RunTerminal) {
        let mut snapshot = super::guards::lock_ok(&self.run_snapshot);
        if snapshot.run_id.as_deref() != Some(terminal.run_id.as_str()) {
            return;
        }
        snapshot.running = false;
        snapshot.terminal = Some(terminal);
        let run_id = snapshot.run_id.clone().unwrap_or_default();
        let status = match snapshot.terminal.as_ref().map(|t| &t.kind) {
            Some(RunTerminalKind::Done) => "done",
            Some(RunTerminalKind::Cancelled) => "cancelled",
            _ => "error",
        };
        // 先发布摘要终态再释放运行槽位，防新 start 覆盖尚未归档的旧轮。
        // 锁序 snapshot → activity；摘要对账先释放 activity 后读取 runtime。
        if let Some(activity) = self.activity.upgrade() {
            activity.finish(&self.id, &run_id, status);
        }
        self.try_settle_cleanup_for_run(&run_id);
        self.running.store(false, Ordering::SeqCst);
    }

    fn try_settle_cleanup_for_run(&self, expected_run: &str) -> bool {
        let mut slot = self.workspace_guard.lock().unwrap();
        if self.root_session_id.is_some() {
            // Child cleanup releases only its reference; the root owns the lease.
            slot.take();
            self.cleanup_done.store(true, Ordering::SeqCst);
            return true;
        }
        if let Some(guard) = slot.as_ref() {
            if Arc::strong_count(guard) > 1 {
                self.cleanup_done.store(false, Ordering::SeqCst);
                return false;
            }
            if let Err(error) = guard.settle() {
                tracing::error!("工作根清理未能确认：{error}");
                self.cleanup_done.store(false, Ordering::SeqCst);
                return false;
            }
        }
        slot.take();
        if self.is_main_session
            && let Some(store) = self.owner_store.upgrade()
            && let Err(error) = store.mark_running_for_run(&self.id, expected_run, false)
        {
            self.save_clean.store(false, Ordering::SeqCst);
            self.cleanup_done.store(false, Ordering::SeqCst);
            tracing::warn!("运行状态落盘失败：{error}");
            return false;
        }
        self.cleanup_done.store(true, Ordering::SeqCst);
        true
    }

    /// Reconcile delayed cleanup without replaying a run or releasing a newer owner.
    pub fn try_settle_cleanup(&self) -> bool {
        let snapshot = super::guards::lock_ok(&self.run_snapshot);
        if self.running.load(Ordering::SeqCst) {
            return false;
        }
        if self.cleanup_done.load(Ordering::SeqCst) {
            return true;
        }
        match snapshot.run_id.as_deref() {
            Some(run_id) => self.try_settle_cleanup_for_run(run_id),
            None => false,
        }
    }

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
            delivery_task_id: Mutex::new(None),
            delivery_requirement_ids: Mutex::new(Vec::new()),
            delivery_review_assignment_id: Mutex::new(None),
            delivery_verification_task_id: Mutex::new(None),
            delivery_check_ids: Mutex::new(None),
            pending_contract: Mutex::new(None),
            pending_workflow: Mutex::new(None),
            delivery_draft: Mutex::new(None),
            last_completion: Mutex::new(None),
            interactive: AtomicBool::new(true),
            interaction_blocked: Mutex::new(None),
            save_clean: AtomicBool::new(true),
            token_budget: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            token_spent: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            token_budget_gate: Arc::new(tokio::sync::Mutex::new(())),
            token_budget_uncertain: Arc::new(AtomicBool::new(false)),
            cleanup_done: AtomicBool::new(true),
            workspace_guard: Mutex::new(None),
            inject_tx: tx,
            inject_rx: Mutex::new(rx),
            inject_accepting: AtomicBool::new(false),
            run_lock: Arc::new(tokio::sync::Mutex::new(())),
            running: Arc::new(AtomicBool::new(false)),
            run_snapshot: Mutex::new(RunSnapshot::default()),
            activity: std::sync::Weak::new(),
            owner_store: std::sync::Weak::new(),
            compacting: AtomicBool::new(false),
            file_ops: Arc::new(tokio::sync::Mutex::new(())),
            sem_tools: Arc::new(Semaphore::new(4)),
            asks: Mutex::new(HashMap::new()),
            text_ask_block: Mutex::new(None),
            stream: Arc::new(ThrottledStream::new()),
            stream_emission_lock: Mutex::new(()),
            active_cancel: Mutex::new(None),
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
            scope_denied_titles: Mutex::new(Vec::new()),
            scope_gate_lock: tokio::sync::Mutex::new(()),
            root_session_id: None,
            root_run_id: None,
            is_task_runtime: false,
            is_main_session: false,
            step_count: std::sync::atomic::AtomicUsize::new(0),
            last_input_tokens: std::sync::atomic::AtomicU64::new(0),
            system_frozen: Mutex::new(None),
            instruction_usage: Mutex::new(Vec::new()),
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
            r.interactive
                .store(parent.interactive.load(Ordering::SeqCst), Ordering::SeqCst);
            *r.workspace_guard.lock().unwrap() = parent.workspace_guard.lock().unwrap().clone();
            r.owner_store = parent.owner_store.clone();
            r.token_budget = parent.token_budget.clone();
            r.token_spent = parent.token_spent.clone();
            r.token_budget_gate = parent.token_budget_gate.clone();
            r.token_budget_uncertain = parent.token_budget_uncertain.clone();
            r.project_id = parent.project_id.clone();
            r.roots = parent.roots.clone();
            r.project_dir = parent.project_dir.clone();
            r.root_run_id = parent
                .root_run_id
                .clone()
                .or_else(|| parent.run_snapshot().run_id);
            // 继承父会话的粘性剥思考标记（[docs/providers-and-network](../../../../docs/providers-and-network.md)）：
            // 同一进程内同一 provider 的端点特性一致，子代理不必在首个请求上再撞一次 400。
            // 继承的是「当前值」：父此后由 400 分类自愈（`Demanded` 复位）时已存在的子 runtime 不跟随
            // ——下一条 400 会让子自己重新分类（对称自愈，无需父子间反向回写）。
            r.reasoning_rejected =
                AtomicBool::new(parent.reasoning_rejected.load(Ordering::SeqCst));
            // [docs/sessions-and-projects](../../../../docs/sessions-and-projects.md)：子代理写登记归属主会话（父本身也是子代理时取其归属，
            // 防嵌套链条断裂）
            r.root_session_id = Some(
                parent
                    .root_session_id
                    .clone()
                    .unwrap_or_else(|| parent.id.clone()),
            );
        }
        // 继承 extra roots 与会话偏好（审批档位 / 模型 / 力度对子代理语义相同，[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）
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

    /// A stale child decision must never stop a newer run on the owning session.
    pub(super) fn cancel_run_if_matches(&self, run_id: &str) {
        let snapshot = super::guards::lock_ok(&self.run_snapshot);
        if snapshot.running && snapshot.run_id.as_deref() == Some(run_id) {
            self.cancel_active();
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
    pub activity: Arc<crate::core::activity::ActivityState>,
    /// 全局配置（RwLock；save_config 热更新写、各处读）
    pub cfg: std::sync::RwLock<ConfigState>,
    pub credentials: Arc<dyn crate::core::config_commit::CredentialStore>,
    pub config_commits: crate::core::config_commit::ConfigCoordinator,
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
    pub instructions: crate::core::instruction_resources::InstructionTrust,
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
    pub interactive: AtomicBool,
    pub host_excluded_tools: Mutex<Vec<String>>,
    /// 交付与验收服务（[docs/tools-and-safety](../../../../docs/tools-and-safety.md)）：
    /// 工具与 host 命令共用同一实例，内部按会话串行化索引写入。
    /// 与 `store` 同生命周期（都锚在 `data_dir` 上），不另开数据根。
    pub delivery: Arc<crate::core::delivery::DeliveryService>,
    /// 可等待目标登记表（`wait` 工具 `until` 条件等待的数据面）。
    /// 独立于 services/subs/tasks 三张表：子代理完成即从 subs 移除、service 自然退出不移除、
    /// 计划任务无 running 中间态——反查既有表都分不清「已完成」与「不存在」。
    /// （[docs/runtime-and-scheduling](../../../../docs/runtime-and-scheduling.md)）
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
        Self::new_with_credentials(
            cfg,
            sink,
            store,
            client,
            data_dir,
            Arc::new(crate::core::credentials::UnavailableCredentialStore),
        )
    }

    pub fn new_with_credentials(
        cfg: ConfigState,
        sink: Arc<dyn EventSink>,
        store: Arc<SessionStore>,
        client: reqwest::Client,
        data_dir: PathBuf,
        credentials: Arc<dyn crate::core::config_commit::CredentialStore>,
    ) -> Self {
        let activity = Arc::new(crate::core::activity::ActivityState::default());
        let sink = Arc::new(crate::core::activity::ActivitySink {
            activity: activity.clone(),
            downstream: sink,
        });
        AgentCore {
            activity,
            cfg: std::sync::RwLock::new(cfg),
            credentials: credentials.clone(),
            // 接入 #36-C0 跨进程协调：配置提交持 `config:root` 租约，
            // 并在提交前检查磁盘是否已被别的实例改过（否则静默覆盖别人的设置）。
            config_commits: crate::core::config_commit::ConfigCoordinator::new(
                credentials,
                Arc::new(crate::core::config_commit::DiskConfigStore::new(
                    data_dir.clone(),
                )),
            )
            .with_coordination(data_dir.as_path()),
            sessions: DashMap::new(),
            sink,
            tools: Arc::new(ToolRegistry::default_tools()),
            store,
            client: std::sync::RwLock::new(client),
            data_dir: data_dir.clone(),
            services: crate::tools::service::ServiceTable::default(),
            skills: crate::skills::SkillIndex::new(),
            instructions: crate::core::instruction_resources::InstructionTrust::default(),
            key_pool: crate::provider::keys::KeyPool::default(),
            mcp: std::sync::Arc::new(crate::mcp::McpManager::default()),
            stats: std::sync::Arc::new(crate::core::stats::StatsCollector::new(data_dir.clone())),
            tasks: std::sync::Arc::new(crate::core::scheduler::TaskTable::new(data_dir.clone())),
            subs: DashMap::new(),
            start_gate: Mutex::new(()),
            interactive: AtomicBool::new(true),
            host_excluded_tools: Mutex::new(Vec::new()),
            delivery: Arc::new(crate::core::delivery::DeliveryService::new(
                data_dir.join("sessions"),
            )),
            wait_targets: crate::core::wait_targets::WaitTargets::default(),
        }
    }

    /// Blocking commit entry used by all writers. Call from spawn_blocking in async code.
    pub fn commit_config(
        &self,
        patch: crate::core::config_commit::ConfigPatch,
        operation_id: &str,
    ) -> Result<
        crate::core::config_commit::ConfigSaveResult,
        crate::core::config_commit::ConfigCommitError,
    > {
        self.config_commits
            .commit_with(&self.cfg, patch, operation_id, |updated, effects| {
                if effects.log_changed {
                    crate::core::logging::apply_config_level(&updated.log.level);
                }
                if effects.proxy_changed {
                    *self.client.write().unwrap() = crate::provider::proxy::build_client(updated);
                }
                if effects.providers_changed {
                    self.key_pool.reset_all();
                }
            })
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
            r.interactive
                .store(self.interactive.load(Ordering::SeqCst), Ordering::SeqCst);
            r.activity = Arc::downgrade(&self.activity);
            // 让 `finish_run` 能在 run 收尾时释放 owner 租约（#36-C0）。
            r.owner_store = Arc::downgrade(&self.store);
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
        self.start_chat_expected(rt, text, images, None)
    }

    pub fn start_chat_expected(
        self: &Arc<Self>,
        rt: Arc<SessionRuntime>,
        text: String,
        images: Vec<crate::core::prefs::ImageIn>,
        expected: Option<&str>,
    ) -> anyhow::Result<String> {
        let start_guard = self.start_gate.lock().unwrap();
        self.start_chat_locked(rt, text, images, expected, &start_guard)
    }

    pub(crate) fn start_chat_locked(
        self: &Arc<Self>,
        rt: Arc<SessionRuntime>,
        text: String,
        images: Vec<crate::core::prefs::ImageIn>,
        expected: Option<&str>,
        _guard: &std::sync::MutexGuard<'_, ()>,
    ) -> anyhow::Result<String> {
        if !rt.running.load(Ordering::SeqCst) && !rt.try_settle_cleanup() {
            anyhow::bail!("上一轮的工作根清理尚未确认，请稍后重试");
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        if !rt.begin_run_expected(&run_id, expected) {
            anyhow::bail!("该会话已有运行中的任务");
        }
        // #36-C0：认领会话 owner（`history:<id>` 租约）。
        //
        // 放在唯一执行入口，保证所有桌面调用遵循同一写入互斥。
        // 位置必须在 `begin_run` 之后、任何写入之前：
        // 起完跑再抢锁，等于第二个 writer 已经写下去了。
        if let Err(e) = self.store.claim_session_owner(&rt.id, &run_id, 0) {
            rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
            anyhow::bail!("会话 {} 已被其它 CodeWave 实例占用：{e}", rt.id);
        }
        match crate::core::process_coordination::WorkspaceGuard::acquire_global(
            &rt.id,
            &run_id,
            0,
            &rt.workspace,
        ) {
            Ok(guard) => *rt.workspace_guard.lock().unwrap() = Some(Arc::new(guard)),
            Err(e) => {
                rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
                anyhow::bail!("工作根已被其它运行占用：{e}");
            }
        }
        // 压缩互斥：压缩占位期间拒绝新 run（TOCTOU 守卫——running 检查通过后压缩
        // 仍可能整体替换历史）
        if rt.compacting.load(Ordering::SeqCst) {
            rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
            anyhow::bail!("上下文压缩进行中，请稍后重试");
        }
        {
            let cfg = self.cfg.read().unwrap();
            let prefs = rt.prefs();
            if crate::core::prefs::effective_model(&cfg, &prefs).is_none() {
                rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
                anyhow::bail!("请先在设置中配置并选择模型");
            }
        }
        if let Some(draft) = rt.pending_contract.lock().unwrap().take() {
            let owner = crate::core::delivery::DeliveryService::identity_of(&rt);
            let roots = crate::core::delivery::DeliveryService::roots_from_runtime(&rt);
            let models = self
                .cfg
                .read()
                .unwrap()
                .providers
                .iter()
                .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
                .collect();
            let frozen = rt.pending_workflow.lock().unwrap().take();
            match self.delivery.create_contract_with(
                &owner,
                &draft,
                &roots,
                &format!("host-{run_id}"),
                |contract| {
                    if let Some(frozen) = frozen {
                        crate::core::workflow::lifecycle::record_resume_provenance(
                            &frozen, contract,
                        )
                        .map_err(|message| {
                            crate::core::delivery::DeliveryError::Save { message }
                        })?;
                        let rebound = frozen.rebind(contract);
                        crate::core::workflow::service::persist(&rt.data_dir, &rebound).map_err(
                            |message| crate::core::delivery::DeliveryError::Save { message },
                        )?;
                        if let Some(budget) = &frozen.resume_budget {
                            self.delivery.save_budget(
                                &owner.session_id,
                                &owner,
                                &contract.task_id,
                                budget,
                                &format!("resume_budget_{run_id}"),
                            )?;
                        }
                        return Ok(());
                    }
                    crate::core::workflow::service::prepare(
                        contract,
                        &roots,
                        &rt.data_dir,
                        rt.project_id.as_deref().unwrap_or("temporary"),
                        rt.project_id.as_ref().map(|_| rt.workspace.as_path()),
                        &models,
                    )
                    .and_then(|prepared| {
                        prepared.validate_models(&self.cfg.read().unwrap())?;
                        crate::core::workflow::service::persist(&rt.data_dir, &prepared)
                    })
                    .map_err(|message| crate::core::delivery::DeliveryError::Save { message })
                },
            ) {
                Ok((contract, _)) => {
                    *rt.delivery_task_id.lock().unwrap() = Some(contract.task_id.clone());
                    if let Err(error) = crate::core::workflow::service::advance_automatic_nodes(
                        &rt.data_dir,
                        &owner,
                        &contract.task_id,
                        false,
                    ) {
                        rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
                        anyhow::bail!("工作流自动节点恢复失败，未启动模型：{error}");
                    }
                }
                Err(e) => {
                    rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
                    anyhow::bail!("交付契约无效，运行未开始：{e}");
                }
            }
        }
        if let Err(e) = self.store.mark_running(&rt.id, true) {
            rt.finish_run(RunTerminal::new(&run_id, RunTerminalKind::Error));
            anyhow::bail!("无法保存运行状态，未启动模型：{e}");
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
        let core = self.clone();
        self.activity.begin(&rt, &run_id);
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

    #[test]
    fn terminal_snapshot_keeps_result_and_rejects_stale_finish() {
        let h = harness();
        assert!(h.rt.run_snapshot().run_id.is_none());
        assert!(h.rt.begin_run("first"));
        assert!(!h.rt.begin_run("duplicate"));
        assert!(h.rt.run_snapshot().running);
        h.rt.finish_run(RunTerminal {
            acceptance: None,
            run_id: "first".into(),
            kind: RunTerminalKind::Done,
            suggestions: Some(vec!["继续".into()]),
            error: None,
            error_kind: None,
            history_save: Some(serde_json::json!({"history_status":"warned"})),
            task_run: None,
        });
        let ended = serde_json::to_value(h.rt.run_snapshot()).unwrap();
        assert_eq!(ended["running"], false);
        assert_eq!(ended["terminal"]["kind"], "done");
        assert_eq!(ended["terminal"]["suggestions"][0], "继续");
        assert!(h.rt.begin_run("second"));
        h.rt.finish_run(RunTerminal::new("first", RunTerminalKind::Error));
        let current = h.rt.run_snapshot();
        assert!(current.running);
        assert_eq!(current.run_id.as_deref(), Some("second"));
        assert!(current.terminal.is_none());
        h.rt.finish_run(RunTerminal::new("second", RunTerminalKind::Cancelled));
        assert!(!h.rt.run_snapshot().running);
        assert_eq!(
            serde_json::to_value(h.rt.run_snapshot()).unwrap()["terminal"]["kind"],
            "cancelled"
        );
    }

    #[tokio::test]
    async fn run_snapshot_steer_checkpoint_does_not_end_run() {
        let h = harness();
        assert!(h.rt.begin_run("steer"));
        let _window = h.rt.begin_injections();
        let receipt = h.rt.enqueue_injection("补充约束".into()).unwrap();
        assert_eq!(
            super::super::drive::drain_inject(&h.core, &h.rt, "steer", true, false).await,
            1
        );
        assert!(receipt.await.unwrap().is_ok());
        assert!(
            h.rt.run_snapshot().running,
            "steer 只保存注入消息，不收尾当前 run"
        );
        assert!(h.rt.run_snapshot().terminal.is_none());
        assert!(!h.rt.begin_run("parallel"), "steer 后仍拒绝并发新 run");
    }

    #[tokio::test]
    async fn run_snapshot_cancel_without_injection_publishes_cancelled() {
        let h = harness();
        assert!(h.rt.begin_run("cancel"));
        super::super::drive::mark_cancelled(&h.core, &h.rt, "cancel").await;
        let snapshot = serde_json::to_value(h.rt.run_snapshot()).unwrap();
        assert_eq!(snapshot["running"], false);
        assert_eq!(snapshot["terminal"]["run_id"], "cancel");
        assert_eq!(snapshot["terminal"]["kind"], "cancelled");
        assert!(h.rt.begin_run("next"));
    }

    #[test]
    fn skip_cancellation_matches_the_owning_run_identity() {
        let h = harness();
        assert!(h.rt.begin_run("new-run"));
        let cancel = CancellationToken::new();
        *h.rt.active_cancel.lock().unwrap() = Some(cancel.clone());
        h.rt.cancel_run_if_matches("old-run");
        assert!(!cancel.is_cancelled());
        h.rt.cancel_run_if_matches("new-run");
        assert!(cancel.is_cancelled());
    }

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

    fn activity_begin(h: &Harness, run_id: &str) {
        assert!(h.rt.begin_run(run_id));
        h.core.activity.begin(&h.rt, run_id);
    }

    fn activity_emit(h: &Harness, event: &str, payload: serde_json::Value) {
        h.core.sink.emit(&h.rt.id, event, payload);
    }

    #[test]
    fn activity_run_identity_rejects_old_events_and_late_finish() {
        let h = harness();
        activity_begin(&h, "first");
        activity_emit(
            &h,
            "tool:start",
            serde_json::json!({"run_id":"first", "tool":"read"}),
        );
        assert_eq!(
            h.core.activity.snapshot().sessions[0].activity,
            "执行工具 read"
        );
        h.rt.finish_run(RunTerminal::new("first", RunTerminalKind::Done));
        activity_begin(&h, "second");
        let (tx, _rx) = oneshot::channel();
        h.rt.open_ask("old-ask", tx);
        activity_emit(
            &h,
            "ask:opened",
            serde_json::json!({"run_id":"first", "ask_id":"old-ask", "kind":"ask"}),
        );
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"first", "sub_id":"old", "role":"tester"}),
        );
        activity_emit(&h, "tool:result", serde_json::json!({"run_id":"first"}));
        h.rt.finish_run(RunTerminal::new("first", RunTerminalKind::Error));
        let snapshot = h.core.activity.snapshot();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].run_id, "second");
        assert_eq!(snapshot.sessions[0].activity, "准备运行");
        assert!(snapshot.sessions[0].subs.is_empty());
        assert!(snapshot.sessions[0].waiting.is_empty());
        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].run_id, "first");
        assert_eq!(snapshot.recent[0].status, "done");
    }

    #[test]
    fn activity_waiter_filters_and_recovers_original_payload() {
        let h = harness();
        activity_begin(&h, "run");
        let (tx, rx) = oneshot::channel();
        h.rt.open_ask("q", tx);
        let payload = serde_json::json!({"ask_id":"q", "kind":"ask", "questions":[{"header":"目标", "question":"选择目标", "options":[{"id":"approve", "mode":"auto_edit"}]}], "plan_body":"完整原方案"});
        activity_emit(&h, "ask:opened", payload.clone());
        activity_emit(
            &h,
            "ask:opened",
            serde_json::json!({"ask_id":"phantom", "kind":"approval"}),
        );
        let snapshot = h.core.activity.snapshot();
        assert_eq!(snapshot.sessions[0].waiting.len(), 1);
        assert_eq!(snapshot.sessions[0].waiting[0].detail, "选择目标");
        assert_eq!(h.core.activity.ask(&h.rt.id, "run", "q"), Some(payload));
        assert!(h.core.activity.ask(&h.rt.id, "wrong-run", "q").is_none());
        drop(rx);
        assert!(h.core.activity.ask(&h.rt.id, "run", "q").is_none());
        assert!(h.core.activity.snapshot().sessions[0].waiting.is_empty());
    }

    #[test]
    fn activity_sub_approval_validates_child_waiter_and_clears_on_root_terminal() {
        let h = harness();
        activity_begin(&h, "run");
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"run", "sub_id":"child", "role":"tester", "max_steps":90}),
        );
        let child = SessionRuntime::new_sub(&h.rt, "child".into());
        h.core.activity.register_sub(&h.rt, &child, Some("run"));
        let (tx, _rx) = oneshot::channel();
        child.open_ask("sub-approval", tx);
        let payload = serde_json::json!({"ask_id":"sub-approval", "kind":"approval", "sub_id":"child", "title":"执行命令", "detail":"请求确认"});
        activity_emit(&h, "ask:opened", payload.clone());
        assert!(h.rt.asks.lock().unwrap().is_empty());
        assert_eq!(h.core.activity.snapshot().sessions[0].waiting.len(), 1);
        assert_eq!(
            h.core.activity.ask(&h.rt.id, "run", "sub-approval"),
            Some(payload)
        );
        h.rt.finish_run(RunTerminal::new("run", RunTerminalKind::Cancelled));
        let snapshot = h.core.activity.snapshot();
        assert!(snapshot.sessions.is_empty());
        assert!(
            h.core
                .activity
                .ask(&h.rt.id, "run", "sub-approval")
                .is_none()
        );
        assert_eq!(snapshot.recent[0].status, "cancelled");
        assert_eq!(snapshot.recent[1].status, "cancelled");
    }

    #[test]
    fn activity_sub_terminal_ignores_late_step_and_preserves_budget_report() {
        let h = harness();
        activity_begin(&h, "run");
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"run", "sub_id":"child", "role":"tester", "task":"任务", "max_steps":90}),
        );
        activity_emit(
            &h,
            "sub:step",
            serde_json::json!({"sub_id":"child", "step":18, "detail":"读文件", "tool":"read"}),
        );
        activity_emit(
            &h,
            "sub:report",
            serde_json::json!({"sub_id":"child", "report":"部分成果"}),
        );
        activity_emit(
            &h,
            "sub:done",
            serde_json::json!({"sub_id":"child", "steps_used":90, "ended":"partial"}),
        );
        let before = h.core.activity.snapshot();
        activity_emit(
            &h,
            "sub:step",
            serde_json::json!({"sub_id":"child", "step":100, "detail":"迟到步骤"}),
        );
        activity_emit(
            &h,
            "sub:error",
            serde_json::json!({"sub_id":"child", "error":"迟到错误"}),
        );
        let after = h.core.activity.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.sessions[0].subs[0].status, "done");
        assert_eq!(after.sessions[0].subs[0].step, 90);
        assert_eq!(after.sessions[0].subs[0].ended.as_deref(), Some("partial"));
        assert_eq!(after.recent.len(), 1);
        assert_eq!(
            after.recent[0].sub.as_ref().unwrap().report.as_deref(),
            Some("部分成果")
        );
    }

    #[test]
    fn activity_recent_and_active_completed_children_are_bounded() {
        let h = harness();
        activity_begin(&h, "run");
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"run", "sub_id":"still-running", "role":"tester"}),
        );
        for i in 0..70 {
            let id = format!("s{i}");
            activity_emit(
                &h,
                "sub:spawn",
                serde_json::json!({"run_id":"run", "sub_id":id, "role":"tester", "task":"长".repeat(1000)}),
            );
            activity_emit(
                &h,
                "sub:report",
                serde_json::json!({"sub_id":id, "report":"长".repeat(3000)}),
            );
            activity_emit(
                &h,
                "sub:done",
                serde_json::json!({"sub_id":id, "ended":"budget"}),
            );
        }
        let snapshot = h.core.activity.snapshot();
        assert_eq!(snapshot.recent.len(), 50);
        assert_eq!(snapshot.sessions[0].subs.len(), 51);
        assert!(
            snapshot.sessions[0]
                .subs
                .iter()
                .any(|s| s.sub_id == "still-running" && s.status == "running")
        );
        assert_eq!(
            snapshot.recent[0]
                .sub
                .as_ref()
                .unwrap()
                .task
                .chars()
                .count(),
            500
        );
        assert_eq!(
            snapshot.recent[0]
                .sub
                .as_ref()
                .unwrap()
                .report
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            2000
        );
        h.rt.finish_run(RunTerminal::new("run", RunTerminalKind::Done));
        assert_eq!(h.core.activity.snapshot().recent.len(), 50);
    }

    #[test]
    fn activity_works_without_tabs_or_delivered_webview_events() {
        let h = harness();
        let core = AgentCore::new(
            ConfigState::default(),
            Arc::new(super::super::test_support::NoopSink),
            h.core.store.clone(),
            reqwest::Client::new(),
            h.core.data_dir.clone(),
        );
        let rt = core.get_or_create_session(
            "background",
            h.rt.workspace.clone(),
            Some("project".into()),
            vec![],
            None,
            vec![],
        );
        assert!(rt.begin_run("run"));
        core.activity.begin(&rt, "run");
        core.sink.emit(
            &rt.id,
            "sub:spawn",
            serde_json::json!({"run_id":"run", "sub_id":"s", "role":"tester"}),
        );
        let snapshot = core.activity.snapshot();
        assert_eq!(snapshot.sessions[0].session_id, "background");
        assert_eq!(snapshot.sessions[0].project_id.as_deref(), Some("project"));
        assert_eq!(snapshot.sessions[0].subs.len(), 1);
        rt.finish_run(RunTerminal::new("run", RunTerminalKind::Error));
        assert!(core.activity.snapshot().sessions.is_empty());
        assert_eq!(core.activity.snapshot().recent[0].status, "error");
    }

    #[test]
    fn activity_closed_waiter_and_missing_terminal_event_reconcile() {
        let h = harness();
        activity_begin(&h, "run");
        let (tx, rx) = oneshot::channel();
        h.rt.open_ask("q", tx);
        activity_emit(
            &h,
            "ask:opened",
            serde_json::json!({"ask_id":"q", "kind":"ask"}),
        );
        assert!(h.rt.resolve_ask("q", serde_json::json!({"answer":"yes"})));
        drop(rx);
        assert!(h.core.activity.snapshot().sessions[0].waiting.is_empty());
        // 模拟旧运行实现/事件丢失，仅运行真值释放，快照读取必须收口。
        h.rt.running.store(false, Ordering::SeqCst);
        let snapshot = h.core.activity.snapshot();
        assert!(snapshot.sessions.is_empty());
        assert_eq!(snapshot.recent[0].status, "error");
    }

    #[tokio::test]
    async fn activity_run_panic_before_drive_cleans_waiters_and_cancels_children() {
        let h = harness();
        activity_begin(&h, "panic-run");
        h.rt.run_snapshot
            .lock()
            .unwrap()
            .clock
            .as_mut()
            .unwrap()
            .instant = std::time::Instant::now() - std::time::Duration::from_secs(120);
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"panic-run", "sub_id":"child", "role":"tester"}),
        );
        let child = SessionRuntime::new_sub(&h.rt, "child".into());
        let cancel = CancellationToken::new();
        child.step_count.store(7, Ordering::SeqCst);
        *child.active_cancel.lock().unwrap() = Some(cancel.clone());
        h.core.subs.insert(child.id.clone(), child.clone());
        h.core
            .activity
            .register_sub(&h.rt, &child, Some("panic-run"));
        let (tx, _rx) = oneshot::channel();
        h.rt.open_ask("q", tx);
        activity_emit(
            &h,
            "ask:opened",
            serde_json::json!({"ask_id":"q", "kind":"ask"}),
        );
        // 模拟装载前历史锁被早先 panic 毒化：此路径在原 drive catch 外。
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = h.rt.history.lock().unwrap();
            panic!("poison history before run");
        }));
        let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
            super::super::drive::run_chat(
                h.core.clone(),
                h.rt.clone(),
                Message::user_text("运行"),
                "panic-run".into(),
            ),
        ))
        .await;
        assert!(result.is_err());
        assert!(!h.rt.run_snapshot().running);
        let terminal = h.rt.run_snapshot().terminal.unwrap();
        let display = terminal.task_run.expect("panic 收尾也应保留真实时长");
        assert!(display.duration_ms >= 120_000);
        assert!(matches!(
            display.status,
            crate::core::sessions::run_display::RunDisplayStatus::Error
        ));
        assert!(display.final_answer.is_none());
        assert!(h.rt.asks.lock().unwrap().is_empty());
        assert!(cancel.is_cancelled());
        let snapshot = h.core.activity.snapshot();
        assert!(snapshot.sessions.is_empty());
        assert_eq!(snapshot.recent[0].status, "error");
        assert_eq!(snapshot.recent[1].sub.as_ref().unwrap().status, "error");
        assert_eq!(snapshot.recent[1].sub.as_ref().unwrap().step, 7);
    }

    #[test]
    fn activity_individual_child_cancel_is_explicit_and_not_error() {
        let h = harness();
        activity_begin(&h, "run");
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"run", "sub_id":"child", "role":"tester"}),
        );
        activity_emit(
            &h,
            "sub:error",
            serde_json::json!({"sub_id":"child", "cancelled":true, "error":"用户停止", "steps_used":3}),
        );
        let snapshot = h.core.activity.snapshot();
        assert_eq!(snapshot.sessions[0].subs[0].status, "cancelled");
        assert_eq!(snapshot.sessions[0].subs[0].step, 3);
        assert_eq!(snapshot.recent[0].status, "cancelled");
    }

    /// 走真实审批生产节点，验证原载荷携带的是出生时的根 run 而非子的空快照。
    #[tokio::test]
    async fn activity_real_child_approval_freezes_root_run_identity() {
        use crate::safety::approval::{ApprovalKind, ApprovalRequest, confirm};
        let h = harness();
        activity_begin(&h, "first");
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"first", "sub_id":"child", "role":"tester"}),
        );
        let child = SessionRuntime::new_sub(&h.rt, "child".into());
        assert!(child.run_snapshot().run_id.is_none());
        assert_eq!(child.root_run_id.as_deref(), Some("first"));
        let nested = SessionRuntime::new_sub(&child, "nested".into());
        assert_eq!(nested.root_run_id.as_deref(), Some("first"));
        h.core.activity.register_sub(&h.rt, &child, Some("first"));

        let spawn = |cancel: CancellationToken| {
            let child = child.clone();
            let sink = h.core.sink.clone();
            tokio::spawn(async move {
                confirm(
                    &child,
                    &sink,
                    ApprovalRequest {
                        title: "命令审批".into(),
                        detail: "cargo test".into(),
                        allow_always: false,
                        kind: ApprovalKind::Once,
                        auto_confirm: false,
                        call_key: None,
                    },
                    &cancel,
                )
                .await
            })
        };
        let task = spawn(CancellationToken::new());
        let ask_id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(id) =
                    h.sink
                        .events
                        .lock()
                        .unwrap()
                        .iter()
                        .find_map(|(_, event, payload)| {
                            (event == "ask:opened")
                                .then(|| payload["ask_id"].as_str().unwrap().to_string())
                        })
                {
                    break id;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let payload = h.core.activity.ask(&h.rt.id, "first", &ask_id).unwrap();
        assert_eq!(payload["run_id"], "first");
        assert_eq!(payload["sub_id"], "child");
        assert_eq!(h.core.activity.snapshot().sessions[0].waiting.len(), 1);
        child.resolve_ask(&ask_id, serde_json::json!({"approved":false}));
        assert!(!task.await.unwrap().approved);

        h.rt.finish_run(RunTerminal::new("first", RunTerminalKind::Done));
        activity_begin(&h, "second");
        let cancel = CancellationToken::new();
        let old_child_task = spawn(cancel.clone());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let count = h
                    .sink
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, event, _)| event == "ask:opened")
                    .count();
                if count == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let events = h.sink.events.lock().unwrap().clone();
        let late = &events
            .iter()
            .rev()
            .find(|(_, e, _)| e == "ask:opened")
            .unwrap()
            .2;
        assert_eq!(late["run_id"], "first", "旧子代理不能借父会话新轮身份提问");
        assert!(h.core.activity.snapshot().sessions[0].waiting.is_empty());
        cancel.cancel();
        assert!(!old_child_task.await.unwrap().approved);
    }

    #[test]
    fn activity_completed_children_keep_latest_completion_not_latest_spawn() {
        let h = harness();
        activity_begin(&h, "run");
        activity_emit(
            &h,
            "sub:spawn",
            serde_json::json!({"run_id":"run", "sub_id":"early-long", "role":"tester"}),
        );
        for i in 0..51 {
            let id = format!("s{i}");
            activity_emit(
                &h,
                "sub:spawn",
                serde_json::json!({"run_id":"run", "sub_id":id, "role":"tester"}),
            );
            activity_emit(
                &h,
                "sub:done",
                serde_json::json!({"sub_id":id, "ended":"report"}),
            );
        }
        activity_emit(
            &h,
            "sub:done",
            serde_json::json!({"sub_id":"early-long", "ended":"report"}),
        );
        let snapshot = h.core.activity.snapshot();
        let subs = &snapshot.sessions[0].subs;
        assert_eq!(subs.len(), 50);
        assert!(
            subs.iter()
                .any(|s| s.sub_id == "early-long" && s.status == "done")
        );
        assert!(!subs.iter().any(|s| s.sub_id == "s0" || s.sub_id == "s1"));
        assert_eq!(
            snapshot.recent[0].sub.as_ref().unwrap().sub_id,
            "early-long"
        );
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
    #[test]
    fn config_commit_entry_preserves_instant_fields_and_scopes_side_effects() {
        use crate::core::config_commit::{ConfigPatch, ConfigVersion};
        let h = harness();
        configure_two_models(&h);
        let initial = h.core.config_commits.snapshot(&h.core.cfg);
        let mut draft = initial.config.clone();
        draft.log.session_verbose = true;
        let keys = vec!["fake-pool-key".to_string()];
        h.core.key_pool.report(
            "provider-test",
            &keys,
            0,
            Some(&crate::provider::dto::ProviderError::Auth("fixture".into())),
        );
        let role = h
            .core
            .commit_config(
                ConfigPatch::Role {
                    role: "backend-dev".into(),
                    model_id: Some("m-session".into()),
                    expected: initial.version.clone(),
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        assert!(!role.effects.providers_changed && !role.effects.proxy_changed);
        assert_eq!(
            h.core.key_pool.debug_state("provider-test"),
            vec![(true, 0)]
        );
        h.core
            .commit_config(
                ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "Committed Sans".into(),
                    mono: "Committed Mono".into(),
                    font_size_sans: 15.0,
                    font_size_content: 14.0,
                    font_size_mono: 15.0,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        let saved = h
            .core
            .commit_config(
                ConfigPatch::Page {
                    config: Box::new(draft),
                    expected: ConfigVersion {
                        boot_id: initial.version.boot_id,
                        sequence: initial.page_version,
                    },
                    edits: HashMap::new(),
                    skip_cleanup: false,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        assert_eq!(
            saved.snapshot.config.subagent_models["backend-dev"].as_deref(),
            Some("m-session")
        );
        assert_eq!(saved.snapshot.config.ui.font_sans, "Committed Sans");
        assert!(saved.snapshot.config.log.session_verbose);
        assert!(saved.effects.cleanup_days.is_none());
        assert_eq!(
            h.core.key_pool.debug_state("provider-test"),
            vec![(true, 0)]
        );
        let disk: ConfigState =
            serde_json::from_slice(&std::fs::read(h.core.data_dir.join("config.json")).unwrap())
                .unwrap();
        assert_eq!(
            serde_json::to_value(disk).unwrap(),
            serde_json::to_value(h.core.cfg.read().unwrap().clone()).unwrap()
        );
    }

    #[test]
    fn config_commit_entry_protects_memory_on_real_file_failure() {
        use crate::core::config_commit::ConfigPatch;
        let h = harness();
        std::fs::create_dir(h.core.data_dir.join("config.json")).unwrap();
        assert!(
            h.core
                .commit_config(
                    ConfigPatch::AllowCommand {
                        command: "fixture-command".into()
                    },
                    &uuid::Uuid::new_v4().to_string()
                )
                .is_err()
        );
        assert!(
            h.core
                .cfg
                .read()
                .unwrap()
                .approval
                .command_allowlist
                .is_empty()
        );
        assert_eq!(
            h.core.config_commits.snapshot(&h.core.cfg).version.sequence,
            0
        );
    }
    #[test]
    fn delayed_child_reference_keeps_owner_until_retry_proves_cleanup() {
        let h = harness();
        assert!(h.rt.begin_run("delayed-cleanup"));
        h.core
            .store
            .claim_session_owner(&h.rt.id, "delayed-cleanup", 0)
            .unwrap();
        let guard = Arc::new(
            crate::core::process_coordination::WorkspaceGuard::acquire_global(
                &h.rt.id,
                "delayed-cleanup",
                0,
                &h.rt.workspace,
            )
            .unwrap(),
        );
        let child_reference = guard.clone();
        *h.rt.workspace_guard.lock().unwrap() = Some(guard);
        h.rt.finish_run(RunTerminal::new("delayed-cleanup", RunTerminalKind::Done));
        assert!(!h.rt.cleanup_done.load(Ordering::SeqCst));
        assert!(h.rt.workspace_guard.lock().unwrap().is_some());
        assert!(!h.core.store.claimed_sessions().is_empty());
        assert!(!h.rt.try_settle_cleanup());
        drop(child_reference);
        assert!(h.rt.try_settle_cleanup());
        assert!(h.rt.try_settle_cleanup());
        assert!(h.rt.cleanup_done.load(Ordering::SeqCst));
        assert!(h.rt.workspace_guard.lock().unwrap().is_none());
        assert!(h.core.store.claimed_sessions().is_empty());
    }
}
