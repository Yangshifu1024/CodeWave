//! 计划任务（[docs/p2-plan](../../../docs/p2-plan.md) §3）：cron/every/once 三种计划；进程内全局串行执行，
//! 每次运行使用全新隔离上下文（复用主循环）。supervisor 每 15s tick 一次。

use crate::core::agent::SessionRuntime;
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// 任务运行步数预算（force_report 前的收敛余量由此保证）。
pub const TASK_BUDGET_STEPS: usize = 30;

/// 每任务保留的执行记录条数上限（新的在前）。
pub const MAX_TASK_RUNS: usize = 20;
/// 执行来源：定时触发（supervisor tick）。
pub const RUN_SOURCE_SCHEDULE: &str = "schedule";
/// 执行来源：立即运行（用户手动触发）。
pub const RUN_SOURCE_MANUAL: &str = "manual";

/// 单次执行记录（任务内嵌的最近 MAX_TASK_RUNS 条；新的在前）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskRun {
    /// 结束时间（RFC3339，本地时区）
    pub at: String,
    /// 执行状态：ok / error / skipped
    pub status: String,
    /// 执行摘要（沿用既有 300 字符截断口径）
    pub summary: String,
    /// 来源：schedule（定时触发）/ manual（立即运行）
    pub source: String,
    /// 本次运行的输出 token 数（= usage.output）
    #[serde(default)]
    pub out_tokens: u64,
}

/// 旧数据兼容：`enabled` 缺省为 true（裸 `#[serde(default)]` 会得到 false，
/// 会把既有任务误判为暂停）。
fn default_true() -> bool {
    true
}

/// 一条计划任务（内存表条目 + projects/<project_id>/tasks/<id>.json 持久化形态）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduledTask {
    /// 任务 id（uuid）
    pub id: String,
    /// 显示名
    pub name: String,
    /// 任务指令（成为隔离 run 的首条用户消息）
    pub instruction: String,
    /// 计划表达式（cron:/every:/once: 前缀语法）
    pub schedule: String,
    /// 下次触发时间（RFC3339；None = 已暂停/一次性已触发）
    pub next_run: Option<String>,
    /// 最近一次执行状态（ok/error/skipped）
    pub last_status: Option<String>,
    /// 最近一次执行摘要
    pub last_summary: Option<String>,
    /// 所属项目（持久化于 projects/<project_id>/tasks/；None = 自由会话任务，不落盘）
    #[serde(default)]
    pub project_id: Option<String>,
    /// 是否启用（暂停的任务不参与 tick：不执行、不写状态、不记历史、不发事件）；
    /// 旧数据无此字段 → 默认启用
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 最近执行记录（新的在前，最多 MAX_TASK_RUNS 条）
    #[serde(default)]
    pub runs: Vec<TaskRun>,
}

/// 计划语法：`cron:<5 字段>` | `every:<n m|h|d>` | `once:<RFC3339>`。
pub fn parse_schedule(s: &str) -> Result<ScheduleKind, String> {
    let s = s.trim();
    if let Some(expr) = s.strip_prefix("cron:") {
        // 5 字段 → cron crate 的 6/7 字段（前补秒位）
        let expr = format!("0 {expr}");
        cron::Schedule::from_str(&expr).map_err(|e| format!("cron 表达式无效：{e}"))?;
        return Ok(ScheduleKind::Cron(expr));
    }
    if let Some(spec) = s.strip_prefix("every:") {
        let spec = spec.trim();
        let (n, unit) = spec
            .split_once(|c: char| c.is_whitespace())
            .ok_or_else(|| "every 格式应为 every:<n> <m|h|d>".to_string())?;
        let n: u64 = n.trim().parse().map_err(|_| "every 间隔必须为正整数")?;
        if n == 0 {
            return Err("every 间隔必须 > 0".into());
        }
        let secs = match unit.trim() {
            "m" => n.checked_mul(60),
            "h" => n.checked_mul(3600),
            "d" => n.checked_mul(86400),
            other => return Err(format!("未知单位 {other:?}（支持 m/h/d）")),
        }
        .ok_or("every 间隔过大")?;
        // 30 天产品上限：超出任何合理排程需求；且极端值曾令 next_after 的 DateTime
        // 运算溢出——任务被 tick 静默隔离，而不是在解析期报错浮出
        if secs > 30 * 86400 {
            return Err("every 间隔过大（上限 30 天）".into());
        }
        return Ok(ScheduleKind::Every(Duration::from_secs(secs)));
    }
    if let Some(t) = s.strip_prefix("once:") {
        let dt = DateTime::parse_from_rfc3339(t.trim())
            .map_err(|e| format!("once 时间无效（需 RFC3339）：{e}"))?;
        return Ok(ScheduleKind::Once(dt.with_timezone(&Local)));
    }
    Err("计划需以 cron: / every: / once: 开头".into())
}

/// 解析后的计划种类（对应 cron:/every:/once: 三种前缀语法）。
#[derive(Debug, Clone, PartialEq)]
pub enum ScheduleKind {
    /// 5 字段 cron 表达式（内部已补秒位）
    Cron(String),
    /// 固定间隔
    Every(Duration),
    /// 一次性时间点（本地时区）
    Once(DateTime<Local>),
}

impl ScheduleKind {
    /// 计算下次触发时间；Once 触发后返回 None。
    pub fn next_after(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            ScheduleKind::Cron(expr) => {
                let sched = cron::Schedule::from_str(expr).ok()?;
                sched.upcoming(Utc).next().map(|u| u.with_timezone(&Local))
            }
            ScheduleKind::Every(d) => Some(now + chrono::Duration::from_std(*d).ok()?),
            ScheduleKind::Once(t) if *t > now => Some(*t),
            ScheduleKind::Once(_) => None,
        }
    }
}

/// 任务表：内存态任务 + 全局串行执行锁 + 持久化根。
#[derive(Default)]
pub struct TaskTable {
    /// 任务表（id → 任务）
    pub tasks: Mutex<HashMap<String, ScheduledTask>>,
    /// 全局串行执行（[docs/p2-plan](../../../docs/p2-plan.md) §3：同时至多 1 个任务运行）。
    /// 用 `Arc` 持有：`trigger_now` 要把 `OwnedMutexGuard` move 进 `tokio::spawn` 的
    /// 后台任务（裸 `MutexGuard` 借用 `AgentCore` 字段，无法跨 'static 任务）。
    pub exec_lock: Arc<Mutex<()>>,
    /// 持久化根（~/.codewave）；测试注入临时目录
    pub data_dir: std::path::PathBuf,
}

impl TaskTable {
    /// 以给定数据根构造空任务表。
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        TaskTable {
            tasks: Mutex::new(HashMap::new()),
            exec_lock: Arc::new(Mutex::new(())),
            data_dir,
        }
    }

    /// 新增/更新任务（先落盘再进内存表）。
    pub async fn upsert(&self, task: ScheduledTask) {
        self.persist(&task);
        self.tasks.lock().await.insert(task.id.clone(), task);
    }

    /// 编辑任务（名称 / 指令 / 计划）：计划变化时才重算 next_run。
    /// 计划语法非法、或（改后的）once 时间已过去 → Err（内存与磁盘都不动）。
    pub async fn update(
        &self,
        id: &str,
        name: String,
        instruction: String,
        schedule: String,
    ) -> Result<ScheduledTask, String> {
        let kind = parse_schedule(&schedule)?;
        let snap = {
            let mut tasks = self.tasks.lock().await;
            let Some(t) = tasks.get_mut(id) else {
                return Err("任务不存在".into());
            };
            // 先算后改：校验失败不得留下半改状态
            let next = if t.schedule == schedule {
                None
            } else {
                match kind.next_after(Local::now()) {
                    Some(n) => Some(n.to_rfc3339()),
                    None => return Err("once 时间已过去，请改计划".into()),
                }
            };
            t.name = name;
            t.instruction = instruction;
            if let Some(n) = next {
                t.schedule = schedule;
                t.next_run = Some(n);
            }
            t.clone()
        };
        self.persist(&snap);
        Ok(snap)
    }

    /// 暂停 / 启用：暂停保留 next_run 不动；启用重算 next_run（once 已过期 → Err）。
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<ScheduledTask, String> {
        let snap = {
            let mut tasks = self.tasks.lock().await;
            let Some(t) = tasks.get_mut(id) else {
                return Err("任务不存在".into());
            };
            let next = if enabled {
                let kind = parse_schedule(&t.schedule)?;
                match kind.next_after(Local::now()) {
                    Some(n) => Some(n.to_rfc3339()),
                    None => return Err("once 时间已过去，请改计划".into()),
                }
            } else {
                None
            };
            t.enabled = enabled;
            if let Some(n) = next {
                t.next_run = Some(n);
            }
            t.clone()
        };
        self.persist(&snap);
        Ok(snap)
    }

    /// 追加一条执行记录（新的在前，最多 MAX_TASK_RUNS 条）并落盘。
    pub async fn record_run(&self, id: &str, run: TaskRun) {
        let snap = {
            let mut tasks = self.tasks.lock().await;
            let Some(t) = tasks.get_mut(id) else {
                return;
            };
            t.runs.insert(0, run);
            t.runs.truncate(MAX_TASK_RUNS);
            t.clone()
        };
        self.persist(&snap);
    }
    /// 移除任务并删除其持久化文件（评审 H3：此前只改内存 + 重写 json，
    /// 任务重启后复活）。
    pub async fn remove(&self, id: &str) -> Option<ScheduledTask> {
        let removed = self.tasks.lock().await.remove(id);
        if let Some(t) = &removed {
            if let Some(pid) = &t.project_id {
                let file = crate::core::projects::data_dir_by_id(&self.data_dir, pid)
                    .join("tasks")
                    .join(format!("{}.json", t.id));
                let _ = std::fs::remove_file(&file);
            }
        }
        removed
    }

    /// 持久化到 projects/<project_id>/tasks/<task-id>.json（自由会话任务跳过）。
    fn persist(&self, task: &ScheduledTask) {
        let Some(pid) = &task.project_id else { return };
        let dir = crate::core::projects::data_dir_by_id(&self.data_dir, pid).join("tasks");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let file = dir.join(format!("{}.json", task.id));
        if let Ok(bytes) = serde_json::to_vec_pretty(task) {
            let _ = crate::util::atomic::atomic_write(&file, &bytes);
        }
    }

    /// 启动时从各项目目录装载持久化任务（已过期的 once 任务不恢复）。
    pub async fn load_all_from_projects(data_dir: &std::path::Path) -> Vec<ScheduledTask> {
        let root = crate::core::projects::projects_root(data_dir);
        let Ok(rd) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for pdir in rd.flatten() {
            let tdir = pdir.path().join("tasks");
            let Ok(trd) = std::fs::read_dir(&tdir) else {
                continue;
            };
            for f in trd.flatten() {
                let Ok(bytes) = std::fs::read(f.path()) else {
                    continue;
                };
                let Ok(t) = serde_json::from_slice::<ScheduledTask>(&bytes) else {
                    continue;
                };
                // once 且已过期 → 不恢复（并删除文件）
                if t.schedule.starts_with("once:") {
                    if let Some(Ok(at)) = t
                        .next_run
                        .as_ref()
                        .map(|s| chrono::DateTime::parse_from_rfc3339(s))
                    {
                        if at < chrono::Local::now() {
                            let _ = std::fs::remove_file(f.path());
                            continue;
                        }
                    }
                }
                out.push(t);
            }
        }
        out
    }
    /// 列出全部任务（按名称排序）。
    pub async fn list(&self) -> Vec<ScheduledTask> {
        let mut v: Vec<ScheduledTask> = self.tasks.lock().await.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
    /// tick：返回到期任务（并更新 next_run；Once 触发即移除）。
    /// 暂停（!enabled）的任务一律跳过：不执行、不写状态、不记历史、不发事件。
    pub async fn tick(&self, now: DateTime<Local>) -> Vec<ScheduledTask> {
        let mut due = Vec::new();
        // 锁内只改内存：推进过 next_run 的任务快照留到释放锁之后再落盘
        let mut rescheduled: Vec<ScheduledTask> = Vec::new();
        {
            let mut tasks = self.tasks.lock().await;
            let ids: Vec<String> = tasks.keys().cloned().collect();
            for id in ids {
                let Some(t) = tasks.get_mut(&id) else {
                    continue;
                };
                if !t.enabled {
                    continue;
                }
                let Some(next) = t.next_run.as_deref().and_then(|s| {
                    DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|d| d.with_timezone(&Local))
                }) else {
                    continue;
                };
                if next <= now {
                    due.push(t.clone());
                    match parse_schedule(&t.schedule).map(|k| k.next_after(now)) {
                        Ok(Some(n)) => {
                            t.next_run = Some(n.to_rfc3339());
                            rescheduled.push(t.clone());
                        }
                        // once：触发后移除（Every/Cron 经解析期上限后恒返回 Some）
                        Ok(None) => {
                            tasks.remove(&id);
                        }
                        Err(e) => {
                            // 计划表达式损坏（如手改任务文件）：隔离而非静默删除循环任务——
                            // next_run=None 跳过后续 tick
                            tracing::warn!(
                                "计划任务「{}」计划表达式失效（{e}），已暂停调度",
                                t.name
                            );
                            t.next_run = None;
                            rescheduled.push(t.clone());
                        }
                    }
                }
            }
        }
        for t in rescheduled {
            self.persist(&t);
        }
        due
    }
}

/// 计算初始 next_run（创建任务时）。
pub fn initial_next(schedule: &str) -> Result<Option<String>, String> {
    let kind = parse_schedule(schedule)?;
    Ok(kind.next_after(Local::now()).map(|t| t.to_rfc3339()))
}

/// supervisor：每 15s tick 一次；到期任务串行执行。
pub async fn start_supervisor(core: Arc<crate::core::agent::AgentCore>) {
    // 恢复持久化任务（projects/<id>/tasks/*.json）
    let restored = TaskTable::load_all_from_projects(&core.data_dir).await;
    if !restored.is_empty() {
        tracing::info!("恢复持久化计划任务 {} 个", restored.len());
        for t in restored {
            core.tasks.upsert(t).await;
        }
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            let due = core.tasks.tick(Local::now()).await;
            for task in due {
                let core = core.clone();
                tokio::spawn(async move {
                    run_task(core, task, RUN_SOURCE_SCHEDULE).await;
                });
            }
        }
    });
}

/// 单任务执行（定时路径）：等待串行锁后运行。
async fn run_task(core: Arc<crate::core::agent::AgentCore>, task: ScheduledTask, source: &str) {
    let _guard = Arc::clone(&core.tasks.exec_lock).lock_owned().await;
    run_task_locked(core, task, source).await;
}

/// 立即运行一次任务（IPC「立即运行」路径）：**不修改 next_run**。
/// 串行锁被占用 → 直接拒绝（不排队）；否则把 guard move 进后台任务后立即返回。
pub async fn trigger_now(core: Arc<crate::core::agent::AgentCore>, id: &str) -> Result<(), String> {
    let task = {
        let tasks = core.tasks.tasks.lock().await;
        tasks.get(id).cloned()
    };
    let Some(task) = task else {
        return Err("任务不存在".into());
    };
    let guard = match Arc::clone(&core.tasks.exec_lock).try_lock_owned() {
        Ok(g) => g,
        Err(_) => return Err("已有任务正在运行，请稍后再试".into()),
    };
    tokio::spawn(async move {
        let _guard = guard;
        run_task_locked(core, task, RUN_SOURCE_MANUAL).await;
    });
    Ok(())
}

/// 任务执行内核：调用方必须已持有 exec_lock；全新隔离上下文（独立 SessionRuntime）。
/// 计划任务运行档位（C1）：无人值守 → 完全访问档。
/// 任务需要写文件与执行命令，而审批弹窗无人应答；默认档（Plan）会把写工具排除、
/// 并把白名单外的命令一律锁成 `E_PLAN_READONLY`——那与任务运行参数（`run_task_agent`
/// 不排除写工具、也不弹审批）自相矛盾：写放行、命令全锁。改为 FullAccess 后，写能力与
/// 现状一致，命令从「全被锁死」变为「正常执行」（灾难级命令仍被 fence 直接拦截）。
/// 只动档位：模型 / 力度保持默认（跟随全局）。
fn task_prefs() -> crate::core::prefs::SessionPrefs {
    crate::core::prefs::SessionPrefs {
        approval_mode: crate::core::prefs::ApprovalMode::FullAccess,
        ..Default::default()
    }
}

async fn run_task_locked(
    core: Arc<crate::core::agent::AgentCore>,
    task: ScheduledTask,
    source: &str,
) {
    core.sink.emit(
        &String::new(),
        "scheduled:fired",
        json!({ "name": task.name, "id": task.id }),
    );
    // 评审 H4：任务必须跑在所属项目的多根快照内（此前会抓「某个会话的 workspace」，
    // 随机跑在错误位置）
    let Some((workspace, pid, project_dir, extra_roots)) = task_scope(&core, &task) else {
        task_log(&core, &task, "跳过：所属项目不存在或无有效目录");
        {
            let mut tasks = core.tasks.tasks.lock().await;
            if let Some(t) = tasks.get_mut(&task.id) {
                t.last_status = Some("skipped".into());
                t.last_summary = Some("所属项目不存在或无有效目录".into());
            }
        }
        core.tasks
            .record_run(
                &task.id,
                TaskRun {
                    at: Local::now().to_rfc3339(),
                    status: "skipped".into(),
                    summary: "所属项目不存在或无有效目录".into(),
                    source: source.to_string(),
                    out_tokens: 0,
                },
            )
            .await;
        core.sink.emit(&String::new(), "scheduled:done", json!({
            "name": task.name, "id": task.id, "status": "skipped", "summary": "所属项目不存在或无有效目录",
        }));
        return;
    };
    let data_dir = core.data_dir.clone();
    let mut rt = SessionRuntime::new_task(
        format!("task_{}", task.id),
        core.data_dir.clone(),
        workspace.clone(),
        data_dir,
    );
    {
        // 装载项目快照：fence 多根覆盖 / project_log / 统计归属都依赖这些字段
        if let Some(r) = Arc::get_mut(&mut rt) {
            r.project_id = pid.clone();
            r.project_dir = project_dir;
            let mut roots = vec![workspace.to_string_lossy().into_owned()];
            roots.extend(extra_roots.iter().cloned());
            r.roots = roots;
            *r.extra_roots.lock().unwrap() = extra_roots;
        }
    }
    // C1：任务运行档位显式置完全访问（无人值守，见 task_prefs 注释）——
    // 不设则落到 `ApprovalMode::default()` = Plan，与任务参数（写工具未排除、不弹审批）矛盾
    rt.set_prefs(task_prefs());
    let first_line: String = task
        .instruction
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    task_log(
        &core,
        &task,
        &format!("触发（{}）：{first_line}", task.schedule),
    );
    let core2 = core.clone();
    let task2 = task.clone();
    // 隔离运行：指令成为首条用户消息；步数预算 30
    let (result, usage) = tokio::spawn(crate::core::agent::run_task_agent(
        core2.clone(),
        rt.clone(),
        task2.clone(),
    ))
    .await
    .unwrap_or_else(|e| (Err(format!("任务执行异常：{e}")), Default::default()));
    let (status, summary) = match &result {
        Ok(text) => ("ok".to_string(), text.chars().take(300).collect::<String>()),
        Err(e) => ("error".to_string(), e.clone()),
    };
    {
        let mut tasks = core.tasks.tasks.lock().await;
        if let Some(t) = tasks.get_mut(&task.id) {
            t.last_status = Some(status.clone());
            t.last_summary = Some(summary.clone());
        }
    }
    // 执行记录（新的在前，上限 20）+ 落盘：record_run 落盘时连带写入上一步的
    // last_status / last_summary（同一份快照）
    core.tasks
        .record_run(
            &task.id,
            TaskRun {
                at: Local::now().to_rfc3339(),
                status: status.clone(),
                summary: summary.clone(),
                source: source.to_string(),
                out_tokens: usage.output,
            },
        )
        .await;
    // L10：任务运行用量计入统计（来源 kind=task）
    if usage.input + usage.output > 0 {
        let model_id = core
            .cfg
            .read()
            .unwrap()
            .active_model_id
            .clone()
            .unwrap_or_default();
        core.stats.record(crate::core::stats::UsageRecord {
            session: rt.id.clone(),
            model_id,
            workspace: rt.workspace.to_string_lossy().into_owned(),
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            runs: 1,
            kind: crate::core::stats::KIND_TASK.into(),
        });
    }
    match &result {
        Ok(text) => {
            task_log(
                &core,
                &task,
                &format!(
                    "完成（input {} / output {} tokens）汇报：\n{text}",
                    usage.input, usage.output
                ),
            );
        }
        Err(e) => {
            task_log(&core, &task, &format!("失败：{e}"));
        }
    }
    core.sink.emit(
        &String::new(),
        "scheduled:done",
        json!({
            "name": task.name, "id": task.id, "status": status, "summary": summary,
        }),
    );
}

/// B7：任务执行日志追加到 projects/<project_id>/logs/<task-id>.log（自由会话任务跳过）。
fn task_log(core: &crate::core::agent::AgentCore, task: &ScheduledTask, line: &str) {
    let Some(pid) = &task.project_id else { return };
    let dir = crate::core::projects::data_dir_by_id(&core.data_dir, pid).join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{}.log", task.id)))
    {
        use std::io::Write as _;
        let _ = writeln!(f, "[{ts}] {line}");
    }
}

/// 任务执行范围（评审 H4）：项目任务 → 项目主目录 + 完整成员根快照；
/// 自由会话任务 → 中立的 data_dir（不再随机抓「第一个会话的工作区」）。
/// 解析后的执行范围元组：(项目目录, 分支, 额外工作区路径, 额外根)。
type TaskScope = (
    std::path::PathBuf,
    Option<String>,
    Option<std::path::PathBuf>,
    Vec<String>,
);

/// 返回 None = 项目缺失 / 无有效目录；调用方跳过执行。
fn task_scope(core: &crate::core::agent::AgentCore, task: &ScheduledTask) -> Option<TaskScope> {
    let Some(pid) = &task.project_id else {
        return Some((core.data_dir.clone(), None, None, Vec::new()));
    };
    let p = crate::core::projects::find(&core.data_dir, pid)?;
    let primary = std::path::PathBuf::from(&p.directory);
    if !primary.is_dir() {
        return None;
    }
    Some((
        primary,
        Some(pid.clone()),
        Some(crate::core::projects::project_data_dir(&core.data_dir, &p)),
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C1：计划任务运行档位 = 完全访问（无人值守，需写文件与执行命令）。
    #[test]
    fn task_runtime_prefs_are_full_access() {
        let p = task_prefs();
        assert_eq!(
            p.approval_mode,
            crate::core::prefs::ApprovalMode::FullAccess
        );
        // 模型 / 力度保持默认（跟随全局），只动档位
        assert!(p.model_id.is_none());
        assert!(p.reasoning_effort.is_none());
        // 端到端：新建任务 runtime 默认落在 Plan（本改造前的矛盾源头），置位后为 FullAccess
        let ws = tempfile::tempdir().unwrap();
        let rt = SessionRuntime::new_task(
            "task_t1".into(),
            ws.path().to_path_buf(),
            ws.path().to_path_buf(),
            ws.path().to_path_buf(),
        );
        assert_eq!(
            rt.prefs().approval_mode,
            crate::core::prefs::ApprovalMode::Plan
        );
        rt.set_prefs(task_prefs());
        assert_eq!(
            rt.prefs().approval_mode,
            crate::core::prefs::ApprovalMode::FullAccess
        );
    }

    #[test]
    fn every_interval_parse_bounds() {
        assert!(parse_schedule("every:30 d").is_ok());
        assert!(
            parse_schedule("every:31 d").is_err(),
            "超过 30 天上限应报错"
        );
        // u64 溢出域必须是解析期错误，而不是 tick 期静默隔离
        assert!(parse_schedule("every:99999999999999999999 d").is_err());
        assert!(parse_schedule("every:18446744073709551615 d").is_err());
        assert!(parse_schedule("every:4467440737095516155 h").is_err());
    }

    #[test]
    fn task_log_appends_under_project_dir() {
        // B7：任务日志落在 projects/<project_id>/logs/<task-id>.log；自由会话任务跳过
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::core::config::ConfigState::default();
        let store = std::sync::Arc::new(crate::core::sessions::SessionStore::new(
            dir.path().to_path_buf(),
        ));
        let core = std::sync::Arc::new(crate::core::agent::AgentCore::new(
            cfg,
            std::sync::Arc::new(crate::core::agent::test_support::NoopSink),
            store,
            reqwest::Client::new(),
            dir.path().to_path_buf(),
        ));
        let task = ScheduledTask {
            id: "t1".into(),
            name: "daily".into(),
            instruction: "x".into(),
            schedule: "every:1 h".into(),
            next_run: None,
            last_status: None,
            last_summary: None,
            project_id: Some("p1".into()),
            enabled: true,
            runs: Vec::new(),
        };
        task_log(&core, &task, "触发");
        task_log(&core, &task, "完成");
        let log = dir.path().join("projects/p1/logs/t1.log");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("触发") && text.contains("完成"),
            "两行都应追加：{text}"
        );
        assert!(text.starts_with('['), "应有时间戳前缀");
        // 自由会话任务（project_id=None）不落盘
        let free = ScheduledTask {
            project_id: None,
            id: "t2".into(),
            ..task.clone()
        };
        task_log(&core, &free, "x");
        assert!(!dir.path().join("projects/p1/logs/t2.log").exists());
    }

    #[tokio::test]
    async fn tick_fires_due_and_reschedules() {
        let table = TaskTable::default();
        let past = (Local::now() - chrono::Duration::minutes(5)).to_rfc3339();
        // every：到期 → 触发并重排
        table
            .upsert(ScheduledTask {
                id: "e1".into(),
                name: "every-task".into(),
                instruction: "x".into(),
                schedule: "every:30 m".into(),
                next_run: Some(past.clone()),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: true,
                runs: Vec::new(),
            })
            .await;
        // once：到期 → 触发后移除
        table
            .upsert(ScheduledTask {
                id: "o1".into(),
                name: "once-task".into(),
                instruction: "y".into(),
                schedule: "once:2020-01-01T00:00:00Z".into(),
                next_run: Some(past),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: true,
                runs: Vec::new(),
            })
            .await;
        // 未到期：不触发
        let future = (Local::now() + chrono::Duration::hours(1)).to_rfc3339();
        table
            .upsert(ScheduledTask {
                id: "f1".into(),
                name: "future".into(),
                instruction: "z".into(),
                schedule: "cron:0 9 * * *".into(),
                next_run: Some(future),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: true,
                runs: Vec::new(),
            })
            .await;

        let due = table.tick(Local::now()).await;
        assert_eq!(due.len(), 2, "到期应触发 2 个");
        assert!(due.iter().any(|t| t.id == "e1"));
        assert!(due.iter().any(|t| t.id == "o1"));
        let list = table.list().await;
        assert_eq!(list.len(), 2, "once 应被移除，剩 every + future");
        let e1 = list.iter().find(|t| t.id == "e1").unwrap();
        let next = DateTime::parse_from_rfc3339(e1.next_run.as_deref().unwrap()).unwrap();
        assert!(
            next.with_timezone(&Local) > Local::now(),
            "every 应重排到未来"
        );
    }

    #[test]
    fn once_future_schedules_next() {
        let kind = parse_schedule("once:2099-01-01T00:00:00+08:00").unwrap();
        assert!(kind.next_after(Local::now()).is_some());
        let past = parse_schedule("once:2020-01-01T00:00:00+08:00").unwrap();
        assert!(past.next_after(Local::now()).is_none());
    }

    #[tokio::test]
    async fn removed_task_file_is_deleted() {
        // 评审 H3：删除任务必须删除持久化文件，否则 load_all 在重启后复活它
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        let task = ScheduledTask {
            id: "e9".into(),
            name: "persist".into(),
            instruction: "x".into(),
            schedule: "every:1 h".into(),
            next_run: Some(Local::now().to_rfc3339()),
            last_status: None,
            last_summary: None,
            project_id: Some("p9".into()),
            enabled: true,
            runs: Vec::new(),
        };
        table.upsert(task).await;
        let file = dir.path().join("projects/p9/tasks/e9.json");
        assert!(file.exists(), "upsert 应落盘");
        table.remove("e9").await;
        assert!(!file.exists(), "remove 必须删除持久化文件（H3）");
    }

    #[test]
    fn task_scope_resolves_project_roots() {
        // 评审 H4：任务范围必须来自项目注册表（主目录 + 附加根）；项目缺失 → None（跳过执行）
        let dir = tempfile::tempdir().unwrap();
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let _ = &root_b; // 单目录语义后已不使用；保留 tempdir 防止作用域过早清理
        let cfg = crate::core::config::ConfigState::default();
        let store = std::sync::Arc::new(crate::core::sessions::SessionStore::new(
            dir.path().to_path_buf(),
        ));
        let core = crate::core::agent::AgentCore::new(
            cfg,
            std::sync::Arc::new(crate::core::agent::test_support::NoopSink),
            store,
            reqwest::Client::new(),
            dir.path().to_path_buf(),
        );
        // p-missing 不在注册表中
        let missing = ScheduledTask {
            id: "m1".into(),
            name: "x".into(),
            instruction: "x".into(),
            schedule: "every:1 h".into(),
            next_run: None,
            last_status: None,
            last_summary: None,
            project_id: Some("p-missing".into()),
            enabled: true,
            runs: Vec::new(),
        };
        assert!(task_scope(&core, &missing).is_none(), "项目缺失应跳过执行");
        // 注册项目（先建子目录结构：save_project 需要项目目录存在）
        let a = std::fs::canonicalize(root_a.path()).unwrap();
        crate::core::projects::save_project(
            &core.data_dir,
            &crate::core::projects::ProjectEntry {
                id: "p-multi".into(),
                name: "multi".into(),
                directory: a.to_string_lossy().into_owned(),
                data_dir: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                allowed_dirs: vec![],
            },
        )
        .unwrap();
        let task = ScheduledTask {
            id: "m2".into(),
            name: "x".into(),
            instruction: "x".into(),
            schedule: "every:1 h".into(),
            next_run: None,
            last_status: None,
            last_summary: None,
            project_id: Some("p-multi".into()),
            enabled: true,
            runs: Vec::new(),
        };
        let (ws, pid, pdir, extra) = task_scope(&core, &task).unwrap();
        assert_eq!(ws, a, "主目录 = 项目目录");
        assert_eq!(pid.as_deref(), Some("p-multi"));
        assert_eq!(
            pdir,
            Some(crate::core::projects::data_dir_by_id(
                &core.data_dir,
                "p-multi"
            ))
        );
        assert!(extra.is_empty(), "单目录语义无 extra 根");
        // 自由会话任务 → 中立 data_dir
        let free = ScheduledTask {
            project_id: None,
            ..task
        };
        let (ws2, pid2, pdir2, extra2) = task_scope(&core, &free).unwrap();
        assert_eq!(ws2, core.data_dir);
        assert!(pid2.is_none() && pdir2.is_none() && extra2.is_empty());
    }

    /// 暂停：任务不参与 tick（不执行、不推进 next_run、不记历史、不写状态）。
    #[tokio::test]
    async fn tick_skips_disabled_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        let past = (Local::now() - chrono::Duration::minutes(5)).to_rfc3339();
        table
            .upsert(ScheduledTask {
                id: "d1".into(),
                name: "paused".into(),
                instruction: "x".into(),
                schedule: "every:30 m".into(),
                next_run: Some(past.clone()),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: false,
                runs: Vec::new(),
            })
            .await;
        let due = table.tick(Local::now()).await;
        assert!(due.is_empty(), "暂停任务不应被触发：{due:?}");
        let list = table.list().await;
        assert_eq!(
            list[0].next_run.as_deref(),
            Some(past.as_str()),
            "暂停不推进 next_run"
        );
        assert!(list[0].runs.is_empty(), "暂停不记历史");
        assert!(list[0].last_status.is_none(), "暂停不写状态");
        // 启用后 next_run 重算到未来，重新入调度
        let t = table.set_enabled("d1", true).await.unwrap();
        let next = DateTime::parse_from_rfc3339(t.next_run.as_deref().unwrap()).unwrap();
        assert!(next.with_timezone(&Local) > Local::now());
    }

    /// update 仅在 schedule 变化时重算 next_run。
    #[tokio::test]
    async fn update_recomputes_next_run_only_when_schedule_changes() {
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        let fixed = (Local::now() + chrono::Duration::hours(3)).to_rfc3339();
        table
            .upsert(ScheduledTask {
                id: "u1".into(),
                name: "old".into(),
                instruction: "i1".into(),
                schedule: "every:1 h".into(),
                next_run: Some(fixed.clone()),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: true,
                runs: Vec::new(),
            })
            .await;
        // 只改 name / instruction：next_run 原样保留
        let t = table
            .update("u1", "new".into(), "i2".into(), "every:1 h".into())
            .await
            .unwrap();
        assert_eq!(t.name, "new");
        assert_eq!(t.instruction, "i2");
        assert_eq!(
            t.next_run.as_deref(),
            Some(fixed.as_str()),
            "计划未变不重排"
        );
        // 改 schedule：重算到未来
        let t = table
            .update("u1", "new".into(), "i2".into(), "every:2 h".into())
            .await
            .unwrap();
        assert_eq!(t.schedule, "every:2 h");
        assert_ne!(t.next_run.as_deref(), Some(fixed.as_str()));
        let next = DateTime::parse_from_rfc3339(t.next_run.as_deref().unwrap()).unwrap();
        assert!(next.with_timezone(&Local) > Local::now(), "计划变化应重排");
        // 未知 id
        assert!(
            table
                .update("ghost", "n".into(), "i".into(), "every:1 h".into())
                .await
                .is_err()
        );
    }

    /// update 传非法 schedule → 返回错误原文，且内存与磁盘都不改。
    #[tokio::test]
    async fn update_rejects_invalid_schedule_without_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        table
            .upsert(ScheduledTask {
                id: "u2".into(),
                name: "orig".into(),
                instruction: "i1".into(),
                schedule: "every:1 h".into(),
                next_run: None,
                last_status: None,
                last_summary: None,
                project_id: Some("proj".into()),
                enabled: true,
                runs: Vec::new(),
            })
            .await;
        let file = dir.path().join("projects/proj/tasks/u2.json");
        let before = std::fs::read_to_string(&file).unwrap();
        let err = table
            .update("u2", "changed".into(), "i2".into(), "weekly".into())
            .await
            .unwrap_err();
        assert!(
            err.contains("cron: / every: / once:"),
            "应为解析错误原文：{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            before,
            "非法计划不得落盘"
        );
        assert_eq!(table.list().await[0].name, "orig", "非法计划不得改内存");
        // 过去的 once 计划：同样报错且不改内存
        let err = table
            .update(
                "u2",
                "changed".into(),
                "i2".into(),
                "once:2020-01-01T00:00:00Z".into(),
            )
            .await
            .unwrap_err();
        assert_eq!(err, "once 时间已过去，请改计划");
        assert_eq!(table.list().await[0].name, "orig");
    }

    /// set_enabled(true) 重算 next_run；暂停保留；once 已过期 → 报错且保持暂停。
    #[tokio::test]
    async fn set_enabled_recomputes_and_rejects_expired_once() {
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        let stale = (Local::now() - chrono::Duration::hours(2)).to_rfc3339();
        table
            .upsert(ScheduledTask {
                id: "s1".into(),
                name: "s".into(),
                instruction: "i".into(),
                schedule: "every:1 h".into(),
                next_run: Some(stale),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: false,
                runs: Vec::new(),
            })
            .await;
        let t = table.set_enabled("s1", true).await.unwrap();
        assert!(t.enabled);
        let after_enable = t.next_run.clone().unwrap();
        let next = DateTime::parse_from_rfc3339(&after_enable).unwrap();
        assert!(
            next.with_timezone(&Local) > Local::now(),
            "启用应重算 next_run"
        );
        // 暂停：next_run 原样保留
        let t = table.set_enabled("s1", false).await.unwrap();
        assert!(!t.enabled);
        assert_eq!(t.next_run.as_deref(), Some(after_enable.as_str()));
        // once 已过期：启用报错且保持暂停
        table
            .upsert(ScheduledTask {
                id: "s2".into(),
                name: "s2".into(),
                instruction: "i".into(),
                schedule: "once:2020-01-01T00:00:00Z".into(),
                next_run: None,
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: false,
                runs: Vec::new(),
            })
            .await;
        assert_eq!(
            table.set_enabled("s2", true).await.unwrap_err(),
            "once 时间已过去，请改计划"
        );
        let list = table.list().await;
        let s2 = list.iter().find(|t| t.id == "s2").unwrap();
        assert!(!s2.enabled, "报错后仍保持暂停");
        assert!(table.set_enabled("ghost", true).await.is_err());
    }

    /// 执行记录新的在前 + 20 条上限（插 21 条丢最旧）。
    #[tokio::test]
    async fn record_run_keeps_newest_first_with_cap() {
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        table
            .upsert(ScheduledTask {
                id: "r1".into(),
                name: "r".into(),
                instruction: "i".into(),
                schedule: "every:1 h".into(),
                next_run: None,
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: true,
                runs: Vec::new(),
            })
            .await;
        for i in 0..21u64 {
            table
                .record_run(
                    "r1",
                    TaskRun {
                        at: format!("2026-01-01T00:00:{i:02}+08:00"),
                        status: "ok".into(),
                        summary: format!("run-{i}"),
                        source: RUN_SOURCE_SCHEDULE.into(),
                        out_tokens: i,
                    },
                )
                .await;
        }
        let list = table.list().await;
        let t = list.iter().find(|t| t.id == "r1").unwrap();
        assert_eq!(t.runs.len(), MAX_TASK_RUNS, "上限 20 条");
        assert_eq!(t.runs[0].summary, "run-20", "新的在前");
        assert_eq!(t.runs[19].summary, "run-1", "最旧的 run-0 被丢弃");
        assert_eq!(t.runs[0].out_tokens, 20);
        assert!(t.runs.iter().all(|r| r.source == RUN_SOURCE_SCHEDULE));
        // 未知 id：静默忽略（不 panic）
        table.record_run("ghost", TaskRun::default()).await;
    }

    /// 旧 JSON（不含 enabled / runs）兼容：enabled == true（丰 default 会得 false）。
    #[test]
    fn legacy_json_defaults_enabled_true() {
        let json = r#"{
            "id": "o1", "name": "n", "instruction": "i", "schedule": "every:1 h",
            "next_run": null, "last_status": null, "last_summary": null, "project_id": null
        }"#;
        let t: ScheduledTask = serde_json::from_str(json).unwrap();
        assert!(t.enabled, "旧数据必须默认启用");
        assert!(t.runs.is_empty());
        // TaskRun.out_tokens 同样带默认值
        let r: TaskRun = serde_json::from_str(
            r#"{"at":"2026-01-01T00:00:00+08:00","status":"ok","summary":"s","source":"manual"}"#,
        )
        .unwrap();
        assert_eq!(r.out_tokens, 0);
        assert_eq!(r.source, RUN_SOURCE_MANUAL);
    }

    /// persist 覆盖 tick / update / set_enabled / record_run，load_all 能读回全部字段。
    #[tokio::test]
    async fn persist_roundtrip_covers_new_fields() {
        let dir = tempfile::tempdir().unwrap();
        let table = TaskTable::new(dir.path().to_path_buf());
        let future = (Local::now() + chrono::Duration::hours(1)).to_rfc3339();
        table
            .upsert(ScheduledTask {
                id: "p1".into(),
                name: "orig".into(),
                instruction: "i".into(),
                schedule: "every:1 h".into(),
                next_run: Some(future),
                last_status: Some("ok".into()),
                last_summary: Some("done".into()),
                project_id: Some("proj".into()),
                enabled: true,
                runs: vec![TaskRun {
                    at: Local::now().to_rfc3339(),
                    status: "ok".into(),
                    summary: "first".into(),
                    source: RUN_SOURCE_MANUAL.into(),
                    out_tokens: 42,
                }],
            })
            .await;
        // update 落盘
        table
            .update("p1", "renamed".into(), "i2".into(), "every:2 h".into())
            .await
            .unwrap();
        // tick 落盘：now 拨到未来令任务到期 → next_run 推进
        let advanced = table.tick(Local::now() + chrono::Duration::hours(5)).await;
        assert_eq!(advanced.len(), 1);
        let mut loaded = TaskTable::load_all_from_projects(dir.path()).await;
        assert_eq!(loaded.len(), 1);
        let l = loaded.remove(0);
        assert_eq!(l.name, "renamed", "update 应落盘");
        assert_eq!(l.instruction, "i2");
        assert_eq!(l.schedule, "every:2 h");
        assert_eq!(l.last_status.as_deref(), Some("ok"));
        assert_eq!(l.last_summary.as_deref(), Some("done"));
        assert_eq!(l.runs.len(), 1);
        assert_eq!(l.runs[0].summary, "first");
        assert_eq!(l.runs[0].out_tokens, 42);
        assert_eq!(
            l.next_run,
            table.list().await[0].next_run,
            "tick 推进的 next_run 应落盘"
        );
        // set_enabled 落盘（暂停保留 next_run）
        table.set_enabled("p1", false).await.unwrap();
        let l = TaskTable::load_all_from_projects(dir.path())
            .await
            .remove(0);
        assert!(!l.enabled, "set_enabled 应落盘");
        assert!(l.next_run.is_some(), "暂停保留 next_run");
        // record_run 落盘（新的在前）
        table
            .record_run(
                "p1",
                TaskRun {
                    at: "2026-01-02T00:00:00+08:00".into(),
                    status: "error".into(),
                    summary: "boom".into(),
                    source: RUN_SOURCE_SCHEDULE.into(),
                    out_tokens: 0,
                },
            )
            .await;
        let l = TaskTable::load_all_from_projects(dir.path())
            .await
            .remove(0);
        assert_eq!(l.runs.len(), 2);
        assert_eq!(l.runs[0].summary, "boom", "record_run 应落盘");
    }

    /// exec_lock 被占用时 trigger_now 直接拒绝（且不改 next_run、不记历史）。
    #[tokio::test]
    async fn trigger_now_rejects_when_busy() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let next = (Local::now() + chrono::Duration::hours(1)).to_rfc3339();
        core.tasks
            .upsert(ScheduledTask {
                id: "m1".into(),
                name: "manual".into(),
                instruction: "i".into(),
                schedule: "every:1 h".into(),
                next_run: Some(next.clone()),
                last_status: None,
                last_summary: None,
                project_id: None,
                enabled: true,
                runs: Vec::new(),
            })
            .await;
        // 未知 id
        assert_eq!(
            trigger_now(core.clone(), "ghost").await.unwrap_err(),
            "任务不存在"
        );
        // 串行锁被占用 → 拒绝（不排队）
        let held = core.tasks.exec_lock.lock().await;
        assert_eq!(
            trigger_now(core.clone(), "m1").await.unwrap_err(),
            "已有任务正在运行，请稍后再试"
        );
        drop(held);
        let mut list = core.tasks.list().await;
        let t = list.remove(0);
        assert_eq!(
            t.next_run.as_deref(),
            Some(next.as_str()),
            "立即运行不改 next_run"
        );
        assert!(t.runs.is_empty(), "被拒绝时不写记录");
    }
}
