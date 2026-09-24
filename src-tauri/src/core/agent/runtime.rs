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
    /// 目标模式（goal mode）状态：None = 未登记（内存 + `sessions/<id>.goal.json` 边车）。
    /// 阶段判定/账本判定是 `crate::core::agent::goal` 里的纯函数，读写入口是 `goal` 工具。
    pub goal: Mutex<Option<crate::core::agent::goal::GoalState>>,
    /// 目标模式硬停标记：L3 高危命令被硬拦时由工具层置位、驱动层 `take_goal_abort` 消费
    ///（take 语义 = 读一次即复位，收尾只响应一次）。
    pub goal_abort: AtomicBool,
    /// 进入目标档前的档位快照（目标收尾自动回落用；None = 未记录 → 走全局默认语义）。
    /// **刻意不放 `SessionPrefs`**：prefs 是前端整体替换写的事实源，纯后端运行时状态
    /// 放进去会被前端补丁（如 AskPanel 的 updatePrefs）冲掉。
    pub goal_prev_mode: Mutex<Option<crate::core::prefs::ApprovalMode>>,
    /// 运行中注入通道的发送端（run.ts 队列消息入此）
    pub inject_tx: mpsc::Sender<Message>,
    /// 注入通道接收端（run 期间被 drive_agent 取走消化）
    pub inject_rx: Mutex<Option<mpsc::Receiver<Message>>>,
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
    /// 会话已删除（评审 H5）：置位后迟到的 run 收尾不得再检查点/写日志复活幽灵会话
    pub zombie: AtomicBool,
    /// 会话级运行偏好（审批档位 / 模型 / 思考力度；内存态，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)）
    pub prefs: Mutex<crate::core::prefs::SessionPrefs>,
    /// G2（[docs/plan-mode-workflow](../../../../docs/plan-mode-workflow.md) §7）：plan 档分析产物标志——pm/tester 子代理成功返回时置位。
    /// 内存态：重启丢失 = 退化为无门禁语义，无安全回退（只读由 G5 独立保证）。
    pub analysis_done: std::sync::atomic::AtomicBool,
    /// G2：因缺分析产物被拒的审批 ask 次数（会话累计；≥2 次后错误文案升级）
    pub analysis_gate_denials: std::sync::atomic::AtomicUsize,
    /// G3：批准时刻 todo 标题的冻结快照（执行范围基线；None = 未批准 / 历史会话）
    pub approved_plan: Mutex<Option<Vec<String>>>,
    /// G3：执行期间计划更新 diff 出新 todo（置位后下一次写操作前先弹范围确认）
    pub scope_expanded: std::sync::atomic::AtomicBool,
    /// G3：用户对计划外步骤选了「本次会话允许」（此后新增静默放行 + 记录偏差）
    pub scope_allowed: std::sync::atomic::AtomicBool,
    /// G3：范围确认拒绝计数（拒绝后保持标记，下一次写操作再次询问；仅作诊断记录）
    pub scope_denials: std::sync::atomic::AtomicUsize,
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
            goal: Mutex::new(None),
            goal_abort: AtomicBool::new(false),
            goal_prev_mode: Mutex::new(None),
            inject_tx: tx,
            inject_rx: Mutex::new(Some(rx)),
            run_lock: Arc::new(tokio::sync::Mutex::new(())),
            running: Arc::new(AtomicBool::new(false)),
            compacting: AtomicBool::new(false),
            file_ops: Arc::new(tokio::sync::Mutex::new(())),
            sem_tools: Arc::new(Semaphore::new(4)),
            asks: Mutex::new(HashMap::new()),
            stream: Arc::new(ThrottledStream::new()),
            active_cancel: Mutex::new(None),
            breakdown_cache: Mutex::new(None),
            paths_cache: Mutex::new(None),
            injected_plan_snapshot_for_run: std::sync::atomic::AtomicBool::new(false),
            reasoning_rejected: AtomicBool::new(false),
            plan_hint_emitted: std::sync::atomic::AtomicBool::new(false),
            zombie: AtomicBool::new(false),
            prefs: Mutex::new(crate::core::prefs::SessionPrefs::default()),
            analysis_done: std::sync::atomic::AtomicBool::new(false),
            analysis_gate_denials: std::sync::atomic::AtomicUsize::new(0),
            approved_plan: Mutex::new(None),
            scope_expanded: std::sync::atomic::AtomicBool::new(false),
            scope_allowed: std::sync::atomic::AtomicBool::new(false),
            scope_denials: std::sync::atomic::AtomicUsize::new(0),
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

    /// 目标状态快照（None = 未登记）。
    pub fn goal_snapshot(&self) -> Option<crate::core::agent::goal::GoalState> {
        self.goal.lock().unwrap().clone()
    }

    /// 整体替换目标状态（None = 清除）。
    pub fn set_goal(&self, g: Option<crate::core::agent::goal::GoalState>) {
        *self.goal.lock().unwrap() = g;
    }

    /// 目标状态的**就地改写**（读-改-写全程持同一把锁），返回改写后的快照；目标缺席时返回 None。
    ///
    /// 为什么需要：账本闸门的拒绝计数与 `blocked` 追加由**根会话与并行子代理共享**同一份状态
    ///（子代理经 `goal::goal_gate_rt` 取根会话），「snapshot → 改 → set」会互相覆盖——
    /// 丢计数会让自停阈值延后触发（连续越界 3 次自停的保证因此不可靠）。
    pub fn mutate_goal(
        &self,
        f: impl FnOnce(&mut crate::core::agent::goal::GoalState),
    ) -> Option<crate::core::agent::goal::GoalState> {
        let mut guard = self.goal.lock().unwrap();
        let state = guard.as_mut()?;
        f(state);
        Some(state.clone())
    }

    /// 请求硬停（L3 高危命令被硬拦时由工具层调用；驱动层在 step 边界消费）。
    pub fn request_goal_abort(&self) {
        self.goal_abort.store(true, Ordering::SeqCst);
    }

    /// 消费硬停标记：返回是否曾被请求，并把标记复位（同一请求只响应一次）。
    pub fn take_goal_abort(&self) -> bool {
        self.goal_abort.swap(false, Ordering::SeqCst)
    }

    /// 进入目标档前的档位快照（None = 未记录）。
    pub fn goal_prev_mode(&self) -> Option<crate::core::prefs::ApprovalMode> {
        *self.goal_prev_mode.lock().unwrap()
    }

    /// 记录 / 清除进入目标档前的档位快照。
    pub fn set_goal_prev_mode(&self, m: Option<crate::core::prefs::ApprovalMode>) {
        *self.goal_prev_mode.lock().unwrap() = m;
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

    /// 用户显式切档的过渡点（host `set_session_prefs` 专用）：写 prefs + 目标档进出的状态迁移，
    /// 返回是否发生了目标状态变更。本方法**只动内存**——落边车与 `goal:update` 事件由
    /// `AgentCore::transition_prefs` 收尾。
    ///
    /// - 进目标档（旧档 != Goal 且新档 == Goal）：记下进入前的档位快照
    ///   （`goal_prev_mode`，达成后自动回落用）；**快照已存在时不覆盖**。
    /// - 离开目标档（旧档 == Goal 且新档 != Goal）且目标正在执行：置 `Paused`
    ///   （**不锁档**——用户可自由切走，前端收到状态变化）。
    pub fn transition_prefs(&self, new: crate::core::prefs::SessionPrefs) -> bool {
        use crate::core::agent::goal::GoalStatus;
        use crate::core::prefs::ApprovalMode;
        let old = self.prefs.lock().unwrap().approval_mode;
        if old != ApprovalMode::Goal && new.approval_mode == ApprovalMode::Goal {
            let mut prev = self.goal_prev_mode.lock().unwrap();
            if prev.is_none() {
                *prev = Some(old);
            }
        }
        let leaving_goal = old == ApprovalMode::Goal && new.approval_mode != ApprovalMode::Goal;
        let mut paused = false;
        if leaving_goal {
            let mut goal = self.goal.lock().unwrap();
            if let Some(s) = goal.as_mut()
                && s.status == GoalStatus::Executing
            {
                s.status = GoalStatus::Paused;
                paused = true;
            }
        }
        *self.prefs.lock().unwrap() = new;
        paused
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
        let initial_prefs =
            crate::core::prefs::SessionPrefs::from_config(&self.cfg.read().unwrap());
        let mut rt = SessionRuntime::new(id.to_string(), workspace, self.data_dir.clone());
        if let Some(r) = Arc::get_mut(&mut rt) {
            r.project_id = project_id;
            r.roots = roots;
            r.project_dir = project_dir;
            r.is_main_session = true;
            *r.extra_roots.lock().unwrap() = extra_roots;
            *r.prefs.lock().unwrap() = initial_prefs;
        }
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
        if rt.prefs().approval_mode == crate::core::prefs::ApprovalMode::Goal
            && self
                .goal_view(&rt)
                .is_some_and(|goal| goal.status == crate::core::agent::goal::GoalStatus::Paused)
        {
            anyhow::bail!("E_GOAL_PAUSED：目标已暂停，请使用「继续推进」显式续跑");
        }
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
    /// 目标档进出的状态迁移（`SessionRuntime::transition_prefs`）+ 状态变更的落边车与
    /// `goal:update` 事件。返回是否发生了目标状态变更。
    pub fn transition_prefs(
        &self,
        rt: &SessionRuntime,
        new: crate::core::prefs::SessionPrefs,
    ) -> bool {
        let paused = rt.transition_prefs(new);
        if paused {
            self.persist_goal(rt);
        }
        paused
    }

    /// 目标状态视图（右栏目标卡 / 会话恢复时拉初始状态）：内存态优先，缺席时回退边车。
    /// 刚重建的 runtime 内存里还没有目标（`run_chat` 起跑时才装回），只读内存会让目标卡
    /// 在恢复会话后显示为空。
    pub fn goal_view(&self, rt: &SessionRuntime) -> Option<crate::core::agent::goal::GoalState> {
        rt.goal_snapshot().or_else(|| self.store.load_goal(&rt.id))
    }

    /// 续跑暂停中的目标（host `resume_goal` 的唯一入口）：校验档位 + 「已暂停」→ 置「执行中」
    /// + 落边车 + 发 `goal:update`。状态不是「已暂停」时返回 Err（幂等保护，不重复起跑）。
    /// 运行态缺席时从边车装回：重启后 runtime 刚重建、尚未跑过 run 时内存里还没有目标。
    pub fn resume_goal(&self, rt: &SessionRuntime) -> anyhow::Result<()> {
        use crate::core::agent::goal::GoalStatus;
        // 档位自校验（纵深防御）：host 命令层（`goal_resume_mode_guard`）已有同一道门，但 core
        // 不得依赖 host 的守卫——续跑会把目标置回执行期，而账本闸门的档位门读的是 prefs：
        // 档位已切走时会把「非目标档 + 目标执行中 + 账本闸门关闭」的静默不一致固化下来。
        // 文案与 host 共用同一份常量（绝不允许两套）。
        if rt.prefs().approval_mode != crate::core::prefs::ApprovalMode::Goal {
            anyhow::bail!("{}", crate::core::agent::goal::GOAL_RESUME_MODE_REQUIRED);
        }
        let current = rt.goal_snapshot().or_else(|| self.store.load_goal(&rt.id));
        let Some(mut goal) = current else {
            anyhow::bail!("尚未登记目标，无法续跑");
        };
        if goal.status != GoalStatus::Paused {
            anyhow::bail!(
                "目标当前状态为「{}」，只有暂停中的目标可以续跑",
                goal.status.label()
            );
        }
        goal.status = GoalStatus::Executing;
        rt.set_goal(Some(goal));
        self.persist_goal(rt);
        Ok(())
    }

    /// 目标状态落盘 + `goal:update` 事件（core 侧改写目标状态后的统一收尾）。
    fn persist_goal(&self, rt: &SessionRuntime) {
        if let Some(goal) = rt.goal_snapshot() {
            let _ = self.store.save_goal(&rt.id, &Some(goal.clone()));
            self.sink.emit(
                &rt.id,
                "goal:update",
                serde_json::json!({ "session": rt.id, "goal": goal }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState, GoalStatus};
    use crate::core::prefs::{ApprovalMode, SessionPrefs};
    use crate::core::types::SessionId;

    /// 记录事件的测试 sink：断言 `goal:update` 的键名与载荷（其余事件不关心）。
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
            "goal-transition",
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

    fn goal_state(status: GoalStatus) -> GoalState {
        GoalState {
            text: "把 X 改成 Y".into(),
            criteria: vec![GoalCriterion {
                title: "改完 X".into(),
                done: false,
            }],
            ledger: GoalLedger::default(),
            status,
            decisions: Vec::new(),
            pending: Vec::new(),
            blocked: Vec::new(),
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
        }
    }

    fn goal_events(h: &Harness) -> Vec<serde_json::Value> {
        h.sink
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e, _)| e == "goal:update")
            .map(|(_, _, p)| p.clone())
            .collect()
    }

    // ---------- 前档快照（切进目标档） ----------

    #[test]
    fn entering_goal_records_prev_mode_once() {
        let h = harness();
        h.rt.set_prefs(prefs_of(ApprovalMode::AutoEdit));
        assert!(
            !h.core.transition_prefs(&h.rt, prefs_of(ApprovalMode::Goal)),
            "仅进档不产生目标状态变更"
        );
        assert_eq!(h.rt.goal_prev_mode(), Some(ApprovalMode::AutoEdit));
        assert_eq!(h.rt.prefs().approval_mode, ApprovalMode::Goal);
        // 重复切进（目标档内再写同档）：快照不被覆盖
        assert!(!h.core.transition_prefs(&h.rt, prefs_of(ApprovalMode::Goal)));
        assert_eq!(h.rt.goal_prev_mode(), Some(ApprovalMode::AutoEdit));
        // 外部（非过渡路径）改档后再切进目标档：仍是首次记录（回落目标不得被改写）
        h.rt.set_prefs(prefs_of(ApprovalMode::ConfirmEach));
        assert!(!h.core.transition_prefs(&h.rt, prefs_of(ApprovalMode::Goal)));
        assert_eq!(h.rt.goal_prev_mode(), Some(ApprovalMode::AutoEdit));
        assert!(goal_events(&h).is_empty(), "无目标状态变更则不发事件");
    }

    #[test]
    fn entering_goal_from_plan_records_plan() {
        let h = harness();
        h.rt.set_prefs(prefs_of(ApprovalMode::Plan));
        h.core.transition_prefs(&h.rt, prefs_of(ApprovalMode::Goal));
        assert_eq!(h.rt.goal_prev_mode(), Some(ApprovalMode::Plan));
    }

    // ---------- 切走档位：执行中的目标自动暂停 ----------

    #[test]
    fn leaving_goal_pauses_executing_goal_and_persists() {
        let h = harness();
        h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
        h.rt.set_goal(Some(goal_state(GoalStatus::Executing)));
        assert!(
            h.core
                .transition_prefs(&h.rt, prefs_of(ApprovalMode::AutoEdit)),
            "执行中切走档位应报告目标状态变更"
        );
        // 内存 + 边车都是已暂停；档位已切走（不锁档）
        assert_eq!(h.rt.goal_snapshot().unwrap().status, GoalStatus::Paused);
        assert_eq!(
            h.core.store.load_goal(&h.rt.id).unwrap().status,
            GoalStatus::Paused,
            "暂停迁移必须落边车"
        );
        assert_eq!(h.rt.prefs().approval_mode, ApprovalMode::AutoEdit);
        // 事件：键名 + 载荷（session 与 goal）
        let ev = goal_events(&h);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["session"], serde_json::json!("goal-transition"));
        assert_eq!(ev[0]["goal"]["status"], serde_json::json!("paused"));
    }

    #[test]
    fn leaving_goal_keeps_other_states_untouched() {
        for status in [
            GoalStatus::Clarify,
            GoalStatus::Paused,
            GoalStatus::Done,
            GoalStatus::Aborted,
        ] {
            let h = harness();
            h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
            h.rt.set_goal(Some(goal_state(status)));
            assert!(
                !h.core
                    .transition_prefs(&h.rt, prefs_of(ApprovalMode::FullAccess)),
                "{status:?} 不应发生暂停迁移"
            );
            assert_eq!(
                h.rt.goal_snapshot().unwrap().status,
                status,
                "{status:?} 状态不得被改写"
            );
            assert_eq!(h.rt.prefs().approval_mode, ApprovalMode::FullAccess);
            assert!(goal_events(&h).is_empty(), "{status:?} 无变更不发事件");
        }
        // 未登记目标：切走档位不 panic、不产生状态变更
        let h = harness();
        h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
        assert!(!h.core.transition_prefs(&h.rt, prefs_of(ApprovalMode::Plan)));
        assert!(h.rt.goal_snapshot().is_none());
    }

    // ---------- 续跑（Paused → Executing） ----------

    #[test]
    fn paused_goal_rejects_ordinary_chat_even_after_runtime_restore() {
        let h = harness();
        h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
        h.rt.set_goal(Some(goal_state(GoalStatus::Paused)));
        let e = h
            .core
            .start_chat(h.rt.clone(), "继续".into(), vec![])
            .unwrap_err();
        assert!(e.to_string().contains("E_GOAL_PAUSED"));
        assert!(!h.rt.running.load(Ordering::SeqCst));

        h.core
            .store
            .save_goal(&h.rt.id, &h.rt.goal_snapshot())
            .unwrap();
        h.rt.set_goal(None);
        let e = h
            .core
            .start_chat(h.rt.clone(), "继续".into(), vec![])
            .unwrap_err();
        assert!(e.to_string().contains("E_GOAL_PAUSED"));
        assert!(!h.rt.running.load(Ordering::SeqCst));
    }

    #[test]
    fn resume_goal_requires_paused_and_flips_to_executing() {
        let h = harness();
        // 档位前提：续跑本身要过目标档校验（core 层纵深防御，见下一条用例）
        h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
        // 未登记：拒绝
        assert!(h.core.resume_goal(&h.rt).is_err());
        // 澄清期 / 执行中 / 终态：拒绝（幂等保护）
        for status in [
            GoalStatus::Clarify,
            GoalStatus::Executing,
            GoalStatus::Done,
            GoalStatus::Aborted,
        ] {
            h.rt.set_goal(Some(goal_state(status)));
            let e = h.core.resume_goal(&h.rt).expect_err("非暂停不得续跑");
            assert!(
                e.to_string().contains(status.label()),
                "{status:?} 的错误文案应点明当前状态：{e}"
            );
            assert_eq!(h.rt.goal_snapshot().unwrap().status, status);
        }
        // 暂停中：置回执行期 + 落边车 + 发事件
        h.rt.set_goal(Some(goal_state(GoalStatus::Paused)));
        assert!(h.core.resume_goal(&h.rt).is_ok());
        assert_eq!(h.rt.goal_snapshot().unwrap().status, GoalStatus::Executing);
        assert_eq!(
            h.core.store.load_goal(&h.rt.id).unwrap().status,
            GoalStatus::Executing
        );
        let ev = goal_events(&h);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["goal"]["status"], serde_json::json!("executing"));
    }

    #[test]
    fn resume_goal_loads_paused_goal_from_sidecar() {
        // 重启后 runtime 刚重建（内存无目标）、边车是暂停态：续跑仍应成功（从边车装回）
        let h = harness();
        h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
        h.core
            .store
            .save_goal(&h.rt.id, &Some(goal_state(GoalStatus::Paused)))
            .unwrap();
        assert!(h.core.resume_goal(&h.rt).is_ok());
        assert_eq!(h.rt.goal_snapshot().unwrap().status, GoalStatus::Executing);
    }

    /// 🟡-3 续跑的档位自校验（纵深防御）：host 命令层已有 `goal_resume_mode_guard`，core 层
    /// 不得依赖它——非 host 调用方绕过时会留下「非目标档 + 目标执行中 + 账本闸门关闭」的
    /// 静默不一致。文案与 host 同源（同一份常量）。
    #[test]
    fn resume_goal_requires_goal_mode() {
        let h = harness();
        h.rt.set_goal(Some(goal_state(GoalStatus::Paused)));
        for mode in [
            ApprovalMode::ConfirmEach,
            ApprovalMode::AutoEdit,
            ApprovalMode::Plan,
            ApprovalMode::FullAccess,
        ] {
            h.rt.set_prefs(prefs_of(mode));
            let e = h.core.resume_goal(&h.rt).unwrap_err();
            assert!(
                e.to_string().contains("E_GOAL_MODE_REQUIRED"),
                "{mode:?} 的错误文案应与 host 层同源：{e}"
            );
            // 拒绝时不动状态、不落边车、不发事件
            assert_eq!(
                h.rt.goal_snapshot().unwrap().status,
                GoalStatus::Paused,
                "{mode:?} 不得被置回执行期"
            );
            assert!(h.core.store.load_goal(&h.rt.id).is_none(), "{mode:?}");
            assert!(goal_events(&h).is_empty(), "{mode:?} 无变更不发事件");
        }
        // 目标档：正常续跑
        h.rt.set_prefs(prefs_of(ApprovalMode::Goal));
        assert!(h.core.resume_goal(&h.rt).is_ok());
        assert_eq!(h.rt.goal_snapshot().unwrap().status, GoalStatus::Executing);
    }

    #[test]
    fn goal_view_prefers_memory_and_falls_back_to_sidecar() {
        let h = harness();
        assert!(h.core.goal_view(&h.rt).is_none());
        h.core
            .store
            .save_goal(&h.rt.id, &Some(goal_state(GoalStatus::Paused)))
            .unwrap();
        assert_eq!(
            h.core.goal_view(&h.rt).unwrap().status,
            GoalStatus::Paused,
            "内存缺席时回退边车"
        );
        h.rt.set_goal(Some(goal_state(GoalStatus::Executing)));
        assert_eq!(
            h.core.goal_view(&h.rt).unwrap().status,
            GoalStatus::Executing,
            "内存态优先"
        );
    }
}
