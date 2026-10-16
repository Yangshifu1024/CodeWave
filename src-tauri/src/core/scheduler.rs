//! 计划任务（[docs/technical-design](../../../docs/technical-design.md)）：cron/every/once 三种计划；进程内全局串行执行，
//! 每次运行使用全新隔离上下文（复用主循环）。supervisor 每 15s tick 一次。

use crate::core::agent::SessionRuntime;
use crate::core::wait_targets::{TargetKind, TargetState};
use chrono::{DateTime, Local, TimeZone};
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
    #[serde(default)]
    pub run_id: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveTaskRun {
    pub run_id: String,
    pub source: String,
    pub scheduled_for: Option<String>,
    pub started_at: String,
}

/// 一条计划任务（内存表条目 + <主目录>/.codewave/tasks/<id>.json 持久化形态）。
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
    /// 所属项目（持久化于 <主目录>/.codewave/tasks/；None = 自由会话任务，不落盘）
    #[serde(default)]
    pub project_id: Option<String>,
    /// 是否启用（暂停的任务不参与 tick：不执行、不写状态、不记历史、不发事件）；
    /// 旧数据无此字段 → 默认启用
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 最近执行记录（新的在前，最多 MAX_TASK_RUNS 条）
    #[serde(default)]
    pub runs: Vec<TaskRun>,
    #[serde(default)]
    pub active_run: Option<ActiveTaskRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence_error: Option<String>,
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
            ScheduleKind::Cron(expr) => cron_next_after(expr, &now),
            ScheduleKind::Every(d) => Some(now + chrono::Duration::from_std(*d).ok()?),
            ScheduleKind::Once(t) if *t > now => Some(*t),
            ScheduleKind::Once(_) => None,
        }
    }
}

/// cron 表达式中的时分按传入时间的时区解释，不能先在 UTC 中求下一次再转回本地。
fn cron_next_after<Z: TimeZone>(expr: &str, now: &DateTime<Z>) -> Option<DateTime<Z>> {
    // 回拨日同一个本地钟点可能有两个绝对时刻；cron 0.17 会先枚举较早的那个，
    // 即使它对 now 而言已经过去。跳过过去候选，避免 tick 反复触发。
    cron::Schedule::from_str(expr)
        .ok()?
        .after(now)
        .find(|next| next > now)
}

/// 修正旧 UTC cron 计算留下的 next_run。未来值必须是最近一次本地触发；
/// 已过期且符合表达式的值仍交给 tick 补跑，避免改动既有停机恢复语义。
fn corrected_cron_next<Z: TimeZone>(
    expr: &str,
    now: &DateTime<Z>,
    stored: &DateTime<Z>,
) -> Option<DateTime<Z>> {
    let schedule = cron::Schedule::from_str(expr).ok()?;
    let expected = schedule.after(now).find(|next| next > now)?;
    if !schedule.includes(stored.clone()) || (stored > now && stored != &expected) {
        Some(expected)
    } else {
        None
    }
}

/// A candidate grants no authority to execute. Only a durable, post-lock claim does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueCandidate {
    pub task_id: String,
    generation: u64,
    scheduled_for: String,
}

#[derive(Debug, Clone)]
pub struct ExecutionSnapshot {
    pub task: ScheduledTask,
    pub run: ActiveTaskRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIssue {
    pub kind: String,
    pub path: String,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct TaskLoadReport {
    pub tasks: Vec<ScheduledTask>,
    pub issues: Vec<TaskIssue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskDiagnostics {
    pub state: String,
    pub issues: Vec<TaskIssue>,
}

#[derive(Default)]
struct TaskState {
    tasks: HashMap<String, ScheduledTask>,
    generations: HashMap<String, u64>,
    pending: HashMap<String, DueCandidate>,
    closed_projects: std::collections::HashSet<String>,
    initialized: bool,
    serial: u64,
    issues: Vec<TaskIssue>,
}

impl TaskState {
    fn invalidate(&mut self, id: &str) {
        self.serial += 1;
        self.generations.insert(id.to_owned(), self.serial);
        self.pending.remove(id);
    }
}

/// Lock order: exec_lock -> mutation_lock -> state. Never hold state during filesystem IO.
#[derive(Default)]
pub struct TaskTable {
    state: std::sync::Mutex<TaskState>,
    pub exec_lock: Arc<Mutex<()>>,
    mutation_lock: Mutex<()>,
    pub data_dir: std::path::PathBuf,
    #[cfg(test)]
    fail_writes: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_directory_sync: std::sync::atomic::AtomicBool,
}

impl TaskTable {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self {
            data_dir,
            ..Self::default()
        }
    }

    /// One initialization gate covers supervisor, list, and every mutation. No startup snapshot
    /// can overwrite a later create/delete, even when the first request precedes supervisor.
    async fn initialize_locked(&self) {
        if self.state.lock().unwrap().initialized {
            return;
        }
        let mut report = Self::load_all_from_projects(&self.data_dir).await;
        let now = Local::now();
        for task in &mut report.tasks {
            let changed = recover_task(task, now);
            if changed {
                let warning = match self.persist(task) {
                    Ok(warning) => warning,
                    Err(error) => Some(error),
                };
                if let Some(error) = warning {
                    task.persistence_error = Some(error.clone());
                    report.issues.push(task_issue(task, "restore_write", error));
                }
            }
        }
        let mut state = self.state.lock().unwrap();
        state.issues = report.issues;
        for task in report.tasks {
            state.invalidate(&task.id);
            state.tasks.insert(task.id.clone(), task);
        }
        state.initialized = true;
    }

    pub async fn restore(&self) {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
    }

    pub async fn diagnostics(&self) -> TaskDiagnostics {
        // Refresh availability without touching registry anchors or holding the state lock.
        let projects = crate::core::projects::load_report(&self.data_dir);
        let state = self.state.lock().unwrap();
        let mut issues: Vec<_> = state
            .issues
            .iter()
            .filter(|issue| {
                !issue.kind.starts_with("index_") && !issue.kind.starts_with("project_")
            })
            .cloned()
            .collect();
        issues.extend(projects.issues.into_iter().map(|issue| TaskIssue {
            kind: issue.kind,
            path: issue.path,
            project_id: issue.project_id,
            task_id: None,
            message: issue.message,
        }));
        TaskDiagnostics {
            state: if !state.initialized {
                "loading"
            } else if issues.is_empty() {
                "ready"
            } else {
                "degraded"
            }
            .into(),
            issues,
        }
    }

    pub async fn upsert(&self, mut task: ScheduledTask) -> Result<ScheduledTask, String> {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        validate_task(&task)?;
        {
            let state = self.state.lock().unwrap();
            check_project_open(&state, &task)?;
            if state.tasks.contains_key(&task.id) {
                return Err("任务 id 已存在".into());
            }
        }
        task.persistence_error = None;
        task.persistence_error = self.persist(&task)?;
        let mut state = self.state.lock().unwrap();
        state.invalidate(&task.id);
        state.tasks.insert(task.id.clone(), task.clone());
        Ok(task)
    }

    pub async fn update(
        &self,
        id: &str,
        name: String,
        instruction: String,
        schedule: String,
    ) -> Result<ScheduledTask, String> {
        let kind = parse_schedule(&schedule)?;
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        let mut task = self.current(id).await?;
        if task.schedule != schedule {
            task.next_run = Some(
                kind.next_after(Local::now())
                    .ok_or("once 时间已过去，请改计划")?
                    .to_rfc3339(),
            );
            task.schedule = schedule;
        }
        task.name = name;
        task.instruction = instruction;
        self.commit_edit(task).await
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<ScheduledTask, String> {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        let mut task = self.current(id).await?;
        if enabled && !task.enabled {
            task.next_run = Some(
                parse_schedule(&task.schedule)?
                    .next_after(Local::now())
                    .ok_or("once 时间已过去，请修改时间以重新安排")?
                    .to_rfc3339(),
            );
        }
        task.enabled = enabled;
        self.commit_edit(task).await
    }

    async fn current(&self, id: &str) -> Result<ScheduledTask, String> {
        let state = self.state.lock().unwrap();
        let task = state.tasks.get(id).ok_or("任务不存在")?;
        check_project_open(&state, task)?;
        Ok(task.clone())
    }

    async fn commit_edit(&self, mut task: ScheduledTask) -> Result<ScheduledTask, String> {
        task.persistence_error = None;
        task.persistence_error = self.persist(&task)?;
        let mut state = self.state.lock().unwrap();
        state.invalidate(&task.id);
        state.tasks.insert(task.id.clone(), task.clone());
        Ok(task)
    }

    pub async fn remove(&self, id: &str) -> Result<Option<ScheduledTask>, String> {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        let task = self.state.lock().unwrap().tasks.get(id).cloned();
        let Some(task) = task else { return Ok(None) };
        if let Some(path) = task_file(&self.data_dir, &task)? {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("删除任务失败 {}: {e}", path.display())),
            }
        }
        let mut state = self.state.lock().unwrap();
        state.invalidate(id);
        state.tasks.remove(id);
        Ok(Some(task))
    }

    /// Project deletion shares the task transaction boundary. Late finish/log writes cannot
    /// recreate its managed directory. A failed deletion still invalidates pending snapshots.
    pub async fn delete_project(&self, project_id: &str) -> Result<(), String> {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        {
            let mut state = self.state.lock().unwrap();
            state.closed_projects.insert(project_id.into());
            let ids: Vec<_> = state
                .tasks
                .values()
                .filter(|t| t.project_id.as_deref() == Some(project_id))
                .map(|t| t.id.clone())
                .collect();
            for id in ids {
                state.invalidate(&id);
            }
        }
        if let Err(error) = crate::core::projects::delete_project(&self.data_dir, project_id) {
            self.state
                .lock()
                .unwrap()
                .closed_projects
                .remove(project_id);
            return Err(error.to_string());
        }
        self.state
            .lock()
            .unwrap()
            .tasks
            .retain(|_, task| task.project_id.as_deref() != Some(project_id));
        Ok(())
    }

    /// Err is strictly pre-rename failure. Ok(Some(warning)) means the new file is
    /// already committed but directory durability is uncertain; never pretend it was rolled back.
    fn persist(&self, task: &ScheduledTask) -> Result<Option<String>, String> {
        #[cfg(test)]
        if self.fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Injected task persistence failure".into());
        }
        let Some(path) = task_file(&self.data_dir, task)? else {
            return Ok(None);
        };
        let bytes = serde_json::to_vec_pretty(task).map_err(|e| e.to_string())?;
        crate::util::atomic::atomic_write(&path, &bytes)
            .map_err(|e| format!("保存任务失败 {}: {e}", path.display()))?;
        #[cfg(test)]
        if self
            .fail_directory_sync
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(Some(
                "任务文件已更新，但目录同步失败，持久化未确认（注入故障）".into(),
            ));
        }
        #[cfg(unix)]
        if let Err(error) = std::fs::File::open(path.parent().expect("validated task path"))
            .and_then(|directory| directory.sync_all())
        {
            return Ok(Some(format!(
                "任务文件已更新，但目录同步失败，持久化未确认 {}: {error}",
                path.display()
            )));
        }
        Ok(None)
    }

    pub async fn load_all_from_projects(data_dir: &std::path::Path) -> TaskLoadReport {
        let projects = crate::core::projects::load_report(data_dir);
        let mut report = TaskLoadReport {
            tasks: Vec::new(),
            issues: projects
                .issues
                .into_iter()
                .map(|issue| TaskIssue {
                    kind: issue.kind,
                    path: issue.path,
                    project_id: issue.project_id,
                    task_id: None,
                    message: issue.message,
                })
                .collect(),
        };
        let mut seen = std::collections::HashSet::new();
        let mut duplicates = std::collections::HashSet::new();
        for project in projects.entries {
            let dir = crate::core::projects::project_data_dir(data_dir, &project).join("tasks");
            let entries = match std::fs::symlink_metadata(&dir) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    report.issues.push(load_issue(
                        "task_directory_symlink",
                        &dir,
                        &project.id,
                        None,
                        "Task directory must not be a symlink".into(),
                    ));
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                _ => std::fs::read_dir(&dir),
            };
            let entries = match entries {
                Ok(entries) => entries,
                Err(e) => {
                    report.issues.push(load_issue(
                        "task_directory_read",
                        &dir,
                        &project.id,
                        None,
                        e.to_string(),
                    ));
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        report.issues.push(load_issue(
                            "task_read",
                            &dir,
                            &project.id,
                            None,
                            e.to_string(),
                        ));
                        continue;
                    }
                };
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let result = (|| -> Result<ScheduledTask, (&str, String)> {
                    let meta = std::fs::symlink_metadata(&path)
                        .map_err(|e| ("task_read", e.to_string()))?;
                    if meta.file_type().is_symlink() {
                        return Err(("task_symlink", "Task file must not be a symlink".into()));
                    }
                    if !meta.is_file() {
                        return Err(("task_file_type", "Task path is not a regular file".into()));
                    }
                    let bytes = std::fs::read(&path).map_err(|e| ("task_read", e.to_string()))?;
                    let task: ScheduledTask =
                        serde_json::from_slice(&bytes).map_err(|e| ("task_json", e.to_string()))?;
                    if !crate::core::projects::valid_id(&task.id) {
                        return Err(("task_id", "Invalid task id".into()));
                    }
                    if path.file_stem().and_then(|s| s.to_str()) != Some(task.id.as_str()) {
                        return Err(("task_filename", "Filename and task id disagree".into()));
                    }
                    if task.project_id.as_deref() != Some(project.id.as_str()) {
                        return Err(("task_project", "Task belongs to a different project".into()));
                    }
                    Ok(task)
                })();
                match result {
                    Ok(task) => {
                        if let Err(error) = parse_schedule(&task.schedule) {
                            report.issues.push(load_issue(
                                "schedule",
                                &path,
                                &project.id,
                                Some(task.id.clone()),
                                error,
                            ));
                        }
                        if task
                            .next_run
                            .as_ref()
                            .is_some_and(|value| DateTime::parse_from_rfc3339(value).is_err())
                        {
                            report.issues.push(load_issue(
                                "next_run",
                                &path,
                                &project.id,
                                Some(task.id.clone()),
                                "Invalid next_run timestamp; task quarantined from scheduling"
                                    .into(),
                            ));
                        }
                        if !seen.insert(task.id.clone()) {
                            duplicates.insert(task.id.clone());
                            report.issues.push(load_issue(
                                "task_duplicate",
                                &path,
                                &project.id,
                                Some(task.id.clone()),
                                "Conflicting task id; all copies quarantined".into(),
                            ));
                        }
                        report.tasks.push(task);
                    }
                    Err((kind, message)) => {
                        report
                            .issues
                            .push(load_issue(kind, &path, &project.id, None, message))
                    }
                }
            }
        }
        report.tasks.retain(|t| !duplicates.contains(&t.id));
        report.tasks.sort_by(|a, b| a.id.cmp(&b.id));
        report
    }

    pub async fn list(&self) -> Vec<ScheduledTask> {
        self.restore().await;
        let mut tasks: Vec<_> = self.state.lock().unwrap().tasks.values().cloned().collect();
        tasks.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        tasks
    }

    /// Reserve at most one pending identity per task. No IO, schedule advancement, or events.
    pub async fn collect_due(&self, now: DateTime<Local>) -> Vec<DueCandidate> {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        let mut state = self.state.lock().unwrap();
        let mut due = Vec::new();
        let tasks: Vec<_> = state.tasks.values().cloned().collect();
        for task in tasks {
            if !task.enabled
                || task.active_run.is_some()
                || task.persistence_error.is_some()
                || state.pending.contains_key(&task.id)
                || check_project_open(&state, &task).is_err()
            {
                continue;
            }
            if let Err(error) = parse_schedule(&task.schedule) {
                if !state
                    .issues
                    .iter()
                    .any(|i| i.kind == "schedule" && i.task_id.as_deref() == Some(&task.id))
                {
                    state.issues.push(task_issue(&task, "schedule", error));
                }
                continue;
            }
            let Some(next) = task
                .next_run
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            else {
                continue;
            };
            if next > now {
                continue;
            }
            let candidate = DueCandidate {
                task_id: task.id.clone(),
                generation: *state.generations.get(&task.id).unwrap_or(&0),
                scheduled_for: task.next_run.unwrap(),
            };
            state.pending.insert(task.id, candidate.clone());
            due.push(candidate);
        }
        due.sort_by(|a, b| {
            DateTime::parse_from_rfc3339(&a.scheduled_for)
                .unwrap()
                .cmp(&DateTime::parse_from_rfc3339(&b.scheduled_for).unwrap())
                .then(a.task_id.cmp(&b.task_id))
        });
        due
    }

    async fn release_candidate(&self, candidate: &DueCandidate) {
        let mut state = self.state.lock().unwrap();
        if state.pending.get(&candidate.task_id) == Some(candidate) {
            state.pending.remove(&candidate.task_id);
        }
    }

    /// Caller holds exec_lock. Persist the active run before exposing any execution side effect.
    pub async fn claim(
        &self,
        id: &str,
        candidate: Option<&DueCandidate>,
        now: DateTime<Local>,
    ) -> Result<Option<ExecutionSnapshot>, String> {
        let _gate = self.mutation_lock.lock().await;
        self.initialize_locked().await;
        let result = self.claim_locked(id, candidate, now).await;
        if let Some(candidate) = candidate {
            self.release_candidate(candidate).await;
        }
        result
    }

    async fn claim_locked(
        &self,
        id: &str,
        candidate: Option<&DueCandidate>,
        now: DateTime<Local>,
    ) -> Result<Option<ExecutionSnapshot>, String> {
        let mut task = {
            let state = self.state.lock().unwrap();
            let Some(task) = state.tasks.get(id) else {
                return if candidate.is_some() {
                    Ok(None)
                } else {
                    Err("任务不存在".into())
                };
            };
            check_project_open(&state, task)?;
            if task.active_run.is_some() {
                return Err("任务已在运行".into());
            }
            if task.persistence_error.is_some() {
                return Err("任务结果尚未保存，请重试保存后再运行".into());
            }
            if let Some(candidate) = candidate
                && (state.pending.get(id) != Some(candidate)
                    || state.generations.get(id) != Some(&candidate.generation)
                    || !task.enabled
                    || task.next_run.as_deref() != Some(&candidate.scheduled_for)
                    || DateTime::parse_from_rfc3339(&candidate.scheduled_for)
                        .map_err(|e| e.to_string())?
                        > now)
            {
                return Ok(None);
            }
            task.clone()
        };
        let kind = parse_schedule(&task.schedule)?;
        let mut unexecuted = task.clone();
        let run = ActiveTaskRun {
            run_id: uuid::Uuid::new_v4().to_string(),
            source: if candidate.is_some() {
                RUN_SOURCE_SCHEDULE
            } else {
                RUN_SOURCE_MANUAL
            }
            .into(),
            scheduled_for: candidate.map(|c| c.scheduled_for.clone()),
            started_at: now.to_rfc3339(),
        };
        if candidate.is_some() {
            task.next_run = kind.next_after(now).map(|t| t.to_rfc3339());
        }
        task.active_run = Some(run.clone());
        if let Some(warning) = self.persist(&task)? {
            // The disk may contain the claim, but execution has not begun. Quarantine the
            // unexecuted schedule; retry_unsaved must durably restore it before another claim.
            let error = format!("任务未执行；认领持久化未确认，等待安全恢复：{warning}");
            unexecuted.persistence_error = Some(error.clone());
            self.state
                .lock()
                .unwrap()
                .tasks
                .insert(id.into(), unexecuted);
            return Err(error);
        }
        let mut state = self.state.lock().unwrap();
        state
            .issues
            .retain(|issue| !(issue.kind == "claim_write" && issue.task_id.as_deref() == Some(id)));
        state.tasks.insert(id.into(), task.clone());
        Ok(Some(ExecutionSnapshot { task, run }))
    }

    /// Merge only this run's terminal fields into the current entity; edits and deletion win.
    pub async fn finish_run(&self, id: &str, run_id: &str, mut run: TaskRun) -> Result<(), String> {
        let _gate = self.mutation_lock.lock().await;
        let mut task = match self.current(id).await {
            Ok(task) => task,
            Err(_) => return Ok(()),
        };
        if task.active_run.as_ref().map(|r| r.run_id.as_str()) != Some(run_id) {
            if task
                .runs
                .iter()
                .any(|run| run.run_id.as_deref() == Some(run_id))
                && let Some(error) = task.persistence_error
            {
                return Err(error);
            }
            return Ok(());
        }
        run.run_id = Some(run_id.into());
        append_result(&mut task, run);
        task.active_run = None;
        task.persistence_error = None;
        let warning = match self.persist(&task) {
            Ok(warning) => warning,
            Err(error) => Some(error),
        };
        task.persistence_error = warning.clone();
        // A completed external action cannot be rolled back; keep its real result in memory.
        self.state.lock().unwrap().tasks.insert(id.into(), task);
        warning.map_or(Ok(()), Err)
    }

    /// One bounded retry per supervisor tick; deleted/project-deleted tasks are absent.
    pub async fn retry_unsaved(&self) {
        let _gate = self.mutation_lock.lock().await;
        let tasks: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .tasks
            .values()
            .filter(|t| t.persistence_error.is_some())
            .cloned()
            .collect();
        for mut task in tasks {
            task.persistence_error = None;
            if matches!(self.persist(&task), Ok(None)) {
                self.state
                    .lock()
                    .unwrap()
                    .tasks
                    .insert(task.id.clone(), task);
            }
        }
    }
}

fn check_project_open(state: &TaskState, task: &ScheduledTask) -> Result<(), String> {
    if task
        .project_id
        .as_ref()
        .is_some_and(|id| state.closed_projects.contains(id))
    {
        return Err("项目已关闭，任务不可执行或保存".into());
    }
    Ok(())
}

fn validate_task(task: &ScheduledTask) -> Result<(), String> {
    if !crate::core::projects::valid_id(&task.id) {
        return Err("非法任务 id".into());
    }
    parse_schedule(&task.schedule)?;
    Ok(())
}

fn load_issue(
    kind: &str,
    path: &std::path::Path,
    project_id: &str,
    task_id: Option<String>,
    message: String,
) -> TaskIssue {
    TaskIssue {
        kind: kind.into(),
        path: path.display().to_string(),
        project_id: Some(project_id.into()),
        task_id,
        message,
    }
}

fn task_issue(task: &ScheduledTask, kind: &str, message: String) -> TaskIssue {
    TaskIssue {
        kind: kind.into(),
        path: String::new(),
        project_id: task.project_id.clone(),
        task_id: Some(task.id.clone()),
        message,
    }
}

/// Strict task-only resolver: never fall back to ~/.codewave/projects/<id>.
fn resolve_task_project_dir(
    data_dir: &std::path::Path,
    id: &str,
) -> Result<std::path::PathBuf, String> {
    if !crate::core::projects::valid_id(id) {
        return Err("非法项目 id".into());
    }
    let report = crate::core::projects::load_report(data_dir);
    if let Some(project) = report.entries.iter().find(|p| p.id == id) {
        let managed = crate::core::projects::project_data_dir(data_dir, project);
        if !managed.is_dir() {
            return Err(format!("项目目录不可用: {}", managed.display()));
        }
        return Ok(managed);
    }
    if let Some(issue) = report
        .issues
        .iter()
        .find(|i| i.project_id.as_deref() == Some(id) || i.project_id.is_none())
    {
        return Err(format!(
            "项目 {id} 不可读 ({}): {}",
            issue.path, issue.message
        ));
    }
    Err(format!("项目不存在: {id}"))
}

fn task_file(
    data_dir: &std::path::Path,
    task: &ScheduledTask,
) -> Result<Option<std::path::PathBuf>, String> {
    if !crate::core::projects::valid_id(&task.id) {
        return Err("非法任务 id".into());
    }
    let Some(id) = &task.project_id else {
        return Ok(None);
    };
    let dir = resolve_task_project_dir(data_dir, id)?.join("tasks");
    if let Ok(meta) = std::fs::symlink_metadata(&dir)
        && (!meta.is_dir() || meta.file_type().is_symlink())
    {
        return Err(format!("任务目录不是普通目录: {}", dir.display()));
    }
    let path = dir.join(format!("{}.json", task.id));
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(format!("任务路径不是普通文件: {}", path.display()));
    }
    Ok(Some(path))
}

fn append_result(task: &mut ScheduledTask, run: TaskRun) {
    if run
        .run_id
        .as_ref()
        .is_some_and(|id| task.runs.iter().any(|r| r.run_id.as_ref() == Some(id)))
    {
        return;
    }
    task.last_status = Some(run.status.clone());
    task.last_summary = Some(run.summary.clone());
    task.runs.insert(0, run);
    task.runs.truncate(MAX_TASK_RUNS);
}

fn recover_task(task: &mut ScheduledTask, now: DateTime<Local>) -> bool {
    task.persistence_error = None;
    if let Some(active) = task.active_run.take() {
        append_result(
            task,
            TaskRun {
                run_id: Some(active.run_id),
                at: now.to_rfc3339(),
                status: "error".into(),
                summary: "上次执行中断，结果未确认".into(),
                source: active.source,
                out_tokens: 0,
            },
        );
        if task.schedule.starts_with("once:")
            && task
                .next_run
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_none_or(|next| next <= now)
        {
            // Interrupted manual runs must not turn an overdue once into an automatic replay.
            // An explicitly edited future schedule is still preserved.
            task.next_run = None;
        }
        return true;
    }
    if !task.enabled {
        return false;
    }
    let Some(at) = task
        .next_run
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    else {
        return false;
    };
    match parse_schedule(&task.schedule) {
        Ok(ScheduleKind::Once(_)) if at <= now => {
            let id = format!("missed:{}:{}", task.id, at.to_rfc3339());
            append_result(
                task,
                TaskRun {
                    run_id: Some(id),
                    at: now.to_rfc3339(),
                    status: "skipped".into(),
                    summary: "应用离线时已错过计划时间".into(),
                    source: RUN_SOURCE_SCHEDULE.into(),
                    out_tokens: 0,
                },
            );
            task.next_run = None;
            true
        }
        Ok(ScheduleKind::Cron(expr)) => {
            if let Some(next) = corrected_cron_next(&expr, &now, &at.with_timezone(&Local)) {
                task.next_run = Some(next.to_rfc3339());
                return true;
            }
            false
        }
        _ => false,
    }
}

/// 计算初始 next_run（创建任务时）。
pub fn initial_next(schedule: &str) -> Result<Option<String>, String> {
    let kind = parse_schedule(schedule)?;
    Ok(kind.next_after(Local::now()).map(|t| t.to_rfc3339()))
}

/// Every 15 seconds collect identity-only candidates; the execution lock is global.
pub async fn start_supervisor(core: Arc<crate::core::agent::AgentCore>) {
    core.tasks.restore().await;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            core.tasks.retry_unsaved().await;
            let due = core.tasks.collect_due(Local::now()).await;
            for candidate in due {
                let core = core.clone();
                let pending = PendingGuard {
                    table: core.tasks.clone(),
                    candidate: candidate.clone(),
                };
                tokio::spawn(async move {
                    let _pending = pending;
                    let runner = core.clone();
                    match dispatch_candidate(
                        core.tasks.clone(),
                        &candidate,
                        Local::now,
                        move |snapshot, execution| execute_claimed(runner, snapshot, execution),
                    )
                    .await
                    {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(task_id = %candidate.task_id, %error, "Task claim rejected");
                            let mut state = core.tasks.state.lock().unwrap();
                            if let Some(task) = state.tasks.get(&candidate.task_id).cloned() {
                                state.issues.retain(|i| {
                                    !(i.kind == "claim_write"
                                        && i.task_id.as_deref() == Some(&candidate.task_id))
                                });
                                state.issues.push(task_issue(&task, "claim_write", error));
                            }
                        }
                    }
                });
            }
        }
    });
}

/// The production dispatcher and fault tests share this exact post-lock authorization gate.
async fn dispatch_candidate<F, Fut, Clock>(
    table: Arc<TaskTable>,
    candidate: &DueCandidate,
    clock: Clock,
    execute: F,
) -> Result<bool, String>
where
    F: FnOnce(ExecutionSnapshot, tokio::sync::OwnedMutexGuard<()>) -> Fut,
    Fut: std::future::Future<Output = ()>,
    Clock: FnOnce() -> DateTime<Local>,
{
    let execution = table.exec_lock.clone().lock_owned().await;
    let Some(snapshot) = table
        .claim(&candidate.task_id, Some(candidate), clock())
        .await?
    else {
        return Ok(false);
    };
    execute(snapshot, execution).await;
    Ok(true)
}

struct PendingGuard {
    table: Arc<TaskTable>,
    candidate: DueCandidate,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let table = self.table.clone();
            let candidate = self.candidate.clone();
            handle.spawn(async move {
                table.release_candidate(&candidate).await;
            });
        }
    }
}

struct TaskRunGuard {
    table: Arc<TaskTable>,
    core: Option<Arc<crate::core::agent::AgentCore>>,
    execution: Option<tokio::sync::OwnedMutexGuard<()>>,
    task_id: String,
    run: ActiveTaskRun,
    armed: bool,
}
impl Drop for TaskRunGuard {
    fn drop(&mut self) {
        if self.armed
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let table = self.table.clone();
            let id = self.task_id.clone();
            let run = self.run.clone();
            let core = self.core.clone();
            let execution = self.execution.take();
            handle.spawn(async move {
                let _execution = execution;
                let _ = table
                    .finish_run(
                        &id,
                        &run.run_id,
                        TaskRun {
                            run_id: Some(run.run_id.clone()),
                            at: Local::now().to_rfc3339(),
                            status: "error".into(),
                            summary: "任务执行中断，结果未确认".into(),
                            source: run.source,
                            out_tokens: 0,
                        },
                    )
                    .await;
                if let Some(core) = core {
                    core.wait_targets.finish(
                        &id,
                        TargetState::Failed,
                        Some("任务执行中断，结果未确认".into()),
                        Some("error".into()),
                    );
                }
            });
        }
    }
}

async fn execute_claimed(
    core: Arc<crate::core::agent::AgentCore>,
    snapshot: ExecutionSnapshot,
    execution: tokio::sync::OwnedMutexGuard<()>,
) {
    let mut cleanup = TaskRunGuard {
        table: core.tasks.clone(),
        core: Some(core.clone()),
        execution: Some(execution),
        task_id: snapshot.task.id.clone(),
        run: snapshot.run.clone(),
        armed: true,
    };
    run_task_locked(core, snapshot).await;
    cleanup.armed = false;
}

/// Manual execution does not advance next_run. A busy executor fails immediately.
pub async fn trigger_now(core: Arc<crate::core::agent::AgentCore>, id: &str) -> Result<(), String> {
    let execution = core
        .tasks
        .exec_lock
        .clone()
        .try_lock_owned()
        .map_err(|_| "已有任务正在运行，请稍后再试")?;
    let snapshot = core
        .tasks
        .claim(id, None, Local::now())
        .await?
        .ok_or("任务不存在")?;
    // Construct the cleanup before spawning so cancellation before the first poll is covered.
    let cleanup = TaskRunGuard {
        table: core.tasks.clone(),
        core: Some(core.clone()),
        execution: Some(execution),
        task_id: id.into(),
        run: snapshot.run.clone(),
        armed: true,
    };
    tokio::spawn(async move {
        let mut cleanup = cleanup;
        run_task_locked(core, snapshot).await;
        cleanup.armed = false;
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

async fn run_task_locked(core: Arc<crate::core::agent::AgentCore>, snapshot: ExecutionSnapshot) {
    let task = snapshot.task;
    let active = snapshot.run;
    core.sink.emit(
        &String::new(),
        "scheduled:fired",
        json!({ "name": task.name, "id": task.id, "run_id": active.run_id }),
    );
    // 登记为可等待目标（`wait` 的 until 可等它跑完）：计划任务无 running 中间态，
    // 每轮登记都会重置上一轮终态；baseline 仅供诊断，连续两轮状态相同也算完成。
    // 等待目标按本轮重新登记；任务实体的 active_run 仍是列表对账真值。
    core.wait_targets.register(
        task.id.clone(),
        TargetKind::ScheduledTask,
        task.last_status.clone(),
    );
    // 评审 H4：任务必须跑在所属项目的多根快照内（此前会抓「某个会话的 workspace」，
    // 随机跑在错误位置）
    let Some((workspace, pid, project_dir, extra_roots)) = task_scope(&core, &task) else {
        task_log(&core, &task, "跳过：所属项目不存在或无有效目录").await;
        finish_and_emit(
            &core,
            &task,
            &active,
            "skipped",
            "所属项目不存在或无有效目录",
            0,
        )
        .await;
        return;
    };
    let data_dir = core.data_dir.clone();
    let mut rt = SessionRuntime::new_task(
        format!("task_{}_{}", task.id, active.run_id),
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
    )
    .await;
    let core2 = core.clone();
    let task2 = task.clone();
    // 隔离运行：指令成为首条用户消息；步数预算 30
    use futures::FutureExt;
    let (result, usage) = std::panic::AssertUnwindSafe(crate::core::agent::run_task_agent(
        core2.clone(),
        rt.clone(),
        task2.clone(),
    ))
    .catch_unwind()
    .await
    .unwrap_or_else(|_| (Err("任务执行异常：panic".into()), Default::default()));
    let (status, summary) = match &result {
        Ok(text) => ("ok".to_string(), text.chars().take(300).collect::<String>()),
        Err(e) => ("error".to_string(), e.clone()),
    };
    finish_and_emit(&core, &task, &active, &status, &summary, usage.output).await;
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
            )
            .await;
        }
        Err(e) => {
            task_log(&core, &task, &format!("失败：{e}")).await;
        }
    }
}

async fn finish_and_emit(
    core: &crate::core::agent::AgentCore,
    task: &ScheduledTask,
    active: &ActiveTaskRun,
    status: &str,
    summary: &str,
    out_tokens: u64,
) {
    let saved = core
        .tasks
        .finish_run(
            &task.id,
            &active.run_id,
            TaskRun {
                run_id: Some(active.run_id.clone()),
                at: Local::now().to_rfc3339(),
                status: status.into(),
                summary: summary.into(),
                source: active.source.clone(),
                out_tokens,
            },
        )
        .await;
    core.wait_targets.finish(
        &task.id,
        if status == "ok" {
            TargetState::Succeeded
        } else {
            TargetState::Failed
        },
        Some(summary.into()),
        Some(status.into()),
    );
    core.sink.emit(&String::new(), "scheduled:done", json!({ "name": task.name, "id": task.id, "run_id": active.run_id, "status": status, "summary": summary, "persistence_error": saved.err() }));
}

/// Logs use the same strict project path and deletion lock as task state; no fallback/mkdir.
async fn task_log(core: &crate::core::agent::AgentCore, task: &ScheduledTask, line: &str) {
    let Some(pid) = &task.project_id else { return };
    let _gate = core.tasks.mutation_lock.lock().await;
    if check_project_open(&core.tasks.state.lock().unwrap(), task).is_err() {
        return;
    }
    let Ok(managed) = resolve_task_project_dir(&core.data_dir, pid) else {
        return;
    };
    let dir = managed.join("logs");
    let Ok(meta) = std::fs::symlink_metadata(&dir) else {
        return;
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return;
    }
    let path = dir.join(format!("{}.log", task.id));
    if !crate::core::projects::valid_id(&task.id) {
        return;
    }
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(
            file,
            "[{}] {line}",
            Local::now().format("%Y-%m-%dT%H:%M:%S")
        );
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
    use chrono::{FixedOffset, LocalResult};

    #[test]
    fn cron_clock_time_uses_schedule_timezone() {
        let guangzhou = FixedOffset::east_opt(8 * 3600).unwrap();
        let before = guangzhou.with_ymd_and_hms(2026, 9, 26, 8, 30, 0).unwrap();
        let next = cron_next_after("0 0 9 * * *", &before).unwrap();
        assert_eq!(
            next,
            guangzhou.with_ymd_and_hms(2026, 9, 26, 9, 0, 0).unwrap()
        );

        let after = guangzhou.with_ymd_and_hms(2026, 9, 26, 9, 30, 0).unwrap();
        let next_day = cron_next_after("0 0 9 * * *", &after).unwrap();
        assert_eq!(
            next_day,
            guangzhou.with_ymd_and_hms(2026, 9, 27, 9, 0, 0).unwrap()
        );

        // 旧版按 UTC 计算出的 17:00 即使也是合法 cron 时刻，仍跳过了更早的 09:00。
        let at_eight = guangzhou.with_ymd_and_hms(2026, 9, 26, 8, 0, 0).unwrap();
        let stored_seventeen = guangzhou.with_ymd_and_hms(2026, 9, 26, 17, 0, 0).unwrap();
        assert_eq!(
            corrected_cron_next("0 0 9,17 * * *", &at_eight, &stored_seventeen),
            Some(guangzhou.with_ymd_and_hms(2026, 9, 26, 9, 0, 0).unwrap())
        );
        let overdue_nine = guangzhou.with_ymd_and_hms(2026, 9, 25, 9, 0, 0).unwrap();
        assert_eq!(
            corrected_cron_next("0 0 9,17 * * *", &at_eight, &overdue_nine),
            None,
            "已过期但符合 cron 的时刻仍保留给 tick"
        );
    }

    #[test]
    fn cron_after_dst_fallback_skips_the_first_past_clock_time() {
        let zone = chrono_tz::America::New_York;
        let LocalResult::Ambiguous(_, second_one_ten) =
            zone.with_ymd_and_hms(2026, 11, 1, 1, 10, 0)
        else {
            panic!("测试日期必须落在秋季回拨的重复小时内");
        };
        let LocalResult::Ambiguous(_, second_one_thirty) =
            zone.with_ymd_and_hms(2026, 11, 1, 1, 30, 0)
        else {
            panic!("01:30 必须出现两次");
        };
        assert_eq!(
            cron_next_after("0 30 1 * * *", &second_one_ten),
            Some(second_one_thirty),
            "第一次 01:30 已在过去，下一次应为回拨后的第二次 01:30"
        );
    }

    fn task(id: &str, now: DateTime<Local>) -> ScheduledTask {
        ScheduledTask {
            id: id.into(),
            name: id.into(),
            instruction: "safe fixture".into(),
            schedule: "every:30 m".into(),
            next_run: Some(now.to_rfc3339()),
            last_status: None,
            last_summary: None,
            project_id: None,
            enabled: true,
            runs: vec![],
            active_run: None,
            persistence_error: None,
        }
    }

    fn project(data: &std::path::Path, root: &std::path::Path, id: &str) {
        crate::core::projects::save_project(
            data,
            &crate::core::projects::ProjectEntry {
                id: id.into(),
                name: id.into(),
                directory: root.display().to_string(),
                data_dir: None,
                created_at: Local::now().to_rfc3339(),
                allowed_dirs: vec![],
            },
        )
        .unwrap();
    }

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        TaskTable,
        ScheduledTask,
    ) {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        project(data.path(), root.path(), "project");
        let table = TaskTable::new(data.path().into());
        let mut task = task("task", Local::now());
        task.project_id = Some("project".into());
        (data, root, table, task)
    }

    fn result(status: &str) -> TaskRun {
        TaskRun {
            status: status.into(),
            summary: format!("safe {status}"),
            at: Local::now().to_rfc3339(),
            source: RUN_SOURCE_SCHEDULE.into(),
            ..Default::default()
        }
    }

    #[test]
    fn task_runtime_prefs_and_identity_are_explicit() {
        let data = tempfile::tempdir().unwrap();
        let rt = SessionRuntime::new_task(
            "task_fixture_run".into(),
            data.path().into(),
            data.path().into(),
            data.path().into(),
        );
        assert!(!rt.is_main_session);
        assert!(rt.is_task_runtime);
        rt.set_prefs(task_prefs());
        assert_eq!(
            rt.prefs().approval_mode,
            crate::core::prefs::ApprovalMode::FullAccess
        );
        assert!(rt.prefs().model_id.is_none());
        assert!(rt.prefs().reasoning_effort.is_none());
    }

    #[tokio::test]
    async fn real_registered_project_roundtrip_and_delete() {
        let (data, root, table, mut task) = fixture();
        task.enabled = false;
        task.last_status = Some("ok".into());
        task.runs = vec![result("ok")];
        table.upsert(task.clone()).await.unwrap();
        let path = root.path().join(".codewave/tasks/task.json");
        assert!(path.is_file());
        assert!(!data.path().join("projects/project/tasks").exists());
        let restored = TaskTable::new(data.path().into());
        assert_eq!(restored.list().await, vec![task]);
        restored.remove("task").await.unwrap().unwrap();
        assert!(!path.exists());
        assert!(TaskTable::new(data.path().into()).list().await.is_empty());
        assert!(restored.remove("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_project_never_uses_global_fallback() {
        let data = tempfile::tempdir().unwrap();
        let table = TaskTable::new(data.path().into());
        let mut task = task("task", Local::now());
        task.project_id = Some("unknown".into());
        assert!(table.upsert(task).await.is_err());
        assert!(table.list().await.is_empty());
        assert!(!data.path().join("projects/unknown").exists());
    }

    #[tokio::test]
    async fn offline_project_discovery_is_non_destructive_even_with_normal_list_reads() {
        let (data, root, table, task) = fixture();
        table.upsert(task).await.unwrap();
        let index = data.path().join("projects/directories.json");
        let bytes = std::fs::read(&index).unwrap();
        let managed = root.path().join(".codewave");
        let offline = root.path().join("offline");
        std::fs::rename(&managed, &offline).unwrap();
        assert!(
            table
                .diagnostics()
                .await
                .issues
                .iter()
                .any(|issue| issue.kind == "project_read")
        );
        let (load, normal) = tokio::join!(TaskTable::load_all_from_projects(data.path()), async {
            crate::core::projects::load(data.path())
        });
        assert!(normal.is_empty());
        assert!(load.tasks.is_empty());
        assert!(load.issues.iter().any(|i| i.kind == "project_read"));
        assert_eq!(std::fs::read(&index).unwrap(), bytes);
        std::fs::rename(&offline, &managed).unwrap();
        assert!(
            !table
                .diagnostics()
                .await
                .issues
                .iter()
                .any(|issue| issue.kind == "project_read")
        );
        assert_eq!(
            TaskTable::load_all_from_projects(data.path())
                .await
                .tasks
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn malformed_registry_and_files_are_retained_and_diagnosed() {
        let (data, root, table, task) = fixture();
        table.upsert(task.clone()).await.unwrap();
        let dir = root.path().join(".codewave/tasks");
        std::fs::write(dir.join("bad.json"), b"{").unwrap();
        std::fs::write(dir.join("ignore.tmp"), b"{").unwrap();
        let mut mismatch = task.clone();
        mismatch.id = "other".into();
        std::fs::write(
            dir.join("wrong.json"),
            serde_json::to_vec(&mismatch).unwrap(),
        )
        .unwrap();
        mismatch.id = "foreign".into();
        mismatch.project_id = Some("not-project".into());
        std::fs::write(
            dir.join("foreign.json"),
            serde_json::to_vec(&mismatch).unwrap(),
        )
        .unwrap();
        let report = TaskTable::load_all_from_projects(data.path()).await;
        assert_eq!(report.tasks.len(), 1);
        for kind in ["task_json", "task_filename", "task_project"] {
            assert!(report.issues.iter().any(|i| i.kind == kind));
        }
        assert_eq!(std::fs::read(dir.join("bad.json")).unwrap(), b"{");
        let index = data.path().join("projects/directories.json");
        std::fs::write(&index, b"broken registry").unwrap();
        assert!(
            TaskTable::load_all_from_projects(data.path())
                .await
                .issues
                .iter()
                .any(|i| i.kind == "index_json")
        );
        assert!(crate::core::projects::load(data.path()).is_empty());
        assert_eq!(std::fs::read(index).unwrap(), b"broken registry");
    }

    #[tokio::test]
    async fn duplicate_task_ids_are_all_quarantined() {
        let (data, _root, table, task) = fixture();
        table.upsert(task.clone()).await.unwrap();
        let second = tempfile::tempdir().unwrap();
        project(data.path(), second.path(), "second");
        let mut conflict = task;
        conflict.project_id = Some("second".into());
        std::fs::write(
            second.path().join(".codewave/tasks/task.json"),
            serde_json::to_vec(&conflict).unwrap(),
        )
        .unwrap();
        let report = TaskTable::load_all_from_projects(data.path()).await;
        assert!(report.tasks.is_empty());
        assert!(report.issues.iter().any(|i| i.kind == "task_duplicate"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn task_file_and_directory_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        let (data, root, table, task) = fixture();
        let outside = root.path().join("outside.json");
        std::fs::write(&outside, serde_json::to_vec(&task).unwrap()).unwrap();
        symlink(&outside, root.path().join(".codewave/tasks/task.json")).unwrap();
        assert!(table.upsert(task).await.is_err());
        let report = TaskTable::load_all_from_projects(data.path()).await;
        assert!(report.tasks.is_empty());
        assert!(report.issues.iter().any(|i| i.kind == "task_symlink"));
        std::fs::remove_dir_all(root.path().join(".codewave/tasks")).unwrap();
        symlink(root.path(), root.path().join(".codewave/tasks")).unwrap();
        assert!(
            TaskTable::load_all_from_projects(data.path())
                .await
                .issues
                .iter()
                .any(|i| i.kind == "task_directory_symlink")
        );
        assert!(outside.is_file());
    }

    #[tokio::test]
    async fn mutation_write_failures_preserve_memory_and_disk() {
        let (_data, root, table, task) = fixture();
        table.upsert(task.clone()).await.unwrap();
        let dir = root.path().join(".codewave/tasks");
        let file = dir.join("task.json");
        let original = std::fs::read(&file).unwrap();
        std::fs::rename(&dir, root.path().join("backup")).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        assert!(
            table
                .update(
                    "task",
                    "changed".into(),
                    "changed".into(),
                    "every:1 h".into()
                )
                .await
                .is_err()
        );
        assert!(table.set_enabled("task", false).await.is_err());
        assert!(table.remove("task").await.is_err());
        assert_eq!(table.list().await, vec![task]);
        assert_eq!(
            std::fs::read(root.path().join("backup/task.json")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn collect_is_bounded_deterministic_and_does_not_advance() {
        let table = TaskTable::default();
        let now = Local::now();
        for id in ["b", "a"] {
            table.upsert(task(id, now)).await.unwrap();
        }
        let before = table.list().await;
        let due = table.collect_due(now).await;
        assert_eq!(
            due.iter().map(|c| c.task_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        for _ in 0..10 {
            assert!(table.collect_due(now).await.is_empty());
        }
        assert_eq!(table.list().await, before);
    }

    #[tokio::test]
    async fn pending_pause_delete_edit_and_recreate_revoke_old_claims() {
        for action in ["pause", "delete", "edit", "reenable", "recreate"] {
            let table = Arc::new(TaskTable::default());
            let now = Local::now();
            table.upsert(task("b", now)).await.unwrap();
            let candidate = table.collect_due(now).await.remove(0);
            let held = table.exec_lock.clone().lock_owned().await;
            let (queued, waiting) = tokio::sync::oneshot::channel();
            let worker = table.clone();
            let run = tokio::spawn(async move {
                queued.send(()).unwrap();
                let _lock = worker.exec_lock.clone().lock_owned().await;
                worker.claim("b", Some(&candidate), now).await.unwrap()
            });
            waiting.await.unwrap();
            match action {
                "pause" => {
                    table.set_enabled("b", false).await.unwrap();
                }
                "delete" => {
                    table.remove("b").await.unwrap();
                }
                "edit" => {
                    table
                        .update("b", "new".into(), "new".into(), "every:1 d".into())
                        .await
                        .unwrap();
                }
                "reenable" => {
                    table.set_enabled("b", false).await.unwrap();
                    table.set_enabled("b", true).await.unwrap();
                }
                "recreate" => {
                    table.remove("b").await.unwrap();
                    table.upsert(task("b", now)).await.unwrap();
                }
                _ => unreachable!(),
            }
            drop(held);
            assert!(run.await.unwrap().is_none(), "{action}");
            assert!(
                table
                    .list()
                    .await
                    .iter()
                    .all(|t| t.active_run.is_none() && t.runs.is_empty())
            );
        }
    }

    #[tokio::test]
    async fn old_candidate_cleanup_cannot_clear_new_generation_pending() {
        let table = TaskTable::default();
        let now = Local::now();
        table.upsert(task("b", now)).await.unwrap();
        let old = table.collect_due(now).await.remove(0);
        table
            .update("b", "new".into(), "new".into(), "every:30 m".into())
            .await
            .unwrap();
        let new = table.collect_due(now).await.remove(0);
        table.release_candidate(&old).await;
        assert!(table.collect_due(now).await.is_empty());
        assert!(table.claim("b", Some(&old), now).await.unwrap().is_none());
        assert!(table.claim("b", Some(&new), now).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn claimed_snapshot_is_immutable_but_finish_preserves_new_edits() {
        let table = TaskTable::default();
        let now = Local::now();
        table.upsert(task("task", now)).await.unwrap();
        let due = table.collect_due(now).await.remove(0);
        let actual = now + chrono::Duration::hours(3);
        let snapshot = table
            .claim("task", Some(&due), actual)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot.task.next_run,
            Some((actual + chrono::Duration::minutes(30)).to_rfc3339())
        );
        let edited = table
            .update(
                "task",
                "new".into(),
                "new instruction".into(),
                "every:1 d".into(),
            )
            .await
            .unwrap();
        table.set_enabled("task", false).await.unwrap();
        table
            .finish_run("task", &snapshot.run.run_id, result("ok"))
            .await
            .unwrap();
        let current = table.list().await.remove(0);
        assert_eq!(snapshot.task.instruction, "safe fixture");
        assert_eq!(current.instruction, "new instruction");
        assert_eq!(current.next_run, edited.next_run);
        assert!(!current.enabled);
        assert_eq!(current.runs.len(), 1);
        assert!(current.active_run.is_none());
    }

    #[tokio::test]
    async fn once_all_terminal_results_remain_and_finish_is_idempotent() {
        for status in ["ok", "error", "skipped"] {
            let (data, root, table, mut task) = fixture();
            let now = Local::now();
            task.schedule = format!("once:{}", now.to_rfc3339());
            task.next_run = Some(now.to_rfc3339());
            table.upsert(task).await.unwrap();
            let due = table.collect_due(now).await.remove(0);
            let claimed = table.claim("task", Some(&due), now).await.unwrap().unwrap();
            assert!(table.list().await[0].next_run.is_none());
            assert!(table.collect_due(now).await.is_empty());
            for _ in 0..2 {
                table
                    .finish_run("task", &claimed.run.run_id, result(status))
                    .await
                    .unwrap();
            }
            let after = table.list().await.remove(0);
            assert!(after.enabled);
            assert!(after.next_run.is_none());
            assert_eq!(after.runs.len(), 1);
            assert_eq!(after.last_status.as_deref(), Some(status));
            let restored = TaskTable::new(data.path().into());
            assert_eq!(restored.list().await, vec![after]);
            assert!(
                restored
                    .collect_due(now + chrono::Duration::days(1))
                    .await
                    .is_empty()
            );
            assert!(root.path().join(".codewave/tasks/task.json").exists());
        }
    }

    #[tokio::test]
    async fn active_run_restart_is_interrupted_once_and_never_replayed() {
        let (data, _root, table, mut task) = fixture();
        let now = Local::now();
        task.schedule = format!("once:{}", now.to_rfc3339());
        task.next_run = Some(now.to_rfc3339());
        table.upsert(task).await.unwrap();
        let due = table.collect_due(now).await.remove(0);
        let claimed = table.claim("task", Some(&due), now).await.unwrap().unwrap();
        let restored = TaskTable::new(data.path().into());
        let recovered = restored.list().await.remove(0);
        assert_eq!(recovered.last_status.as_deref(), Some("error"));
        assert_eq!(
            recovered.runs[0].run_id.as_deref(),
            Some(claimed.run.run_id.as_str())
        );
        assert!(recovered.last_summary.unwrap().contains("结果未确认"));
        assert!(recovered.active_run.is_none());
        assert!(recovered.next_run.is_none());
        assert!(restored.collect_due(now).await.is_empty());
        assert_eq!(
            TaskTable::new(data.path().into()).list().await[0]
                .runs
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn offline_once_is_missed_but_paused_once_is_untouched() {
        let (data, _root, table, mut task) = fixture();
        task.schedule = "once:2020-01-01T00:00:00Z".into();
        task.next_run = Some("2020-01-01T00:00:00Z".into());
        table.upsert(task.clone()).await.unwrap();
        task.id = "paused".into();
        task.enabled = false;
        table.upsert(task.clone()).await.unwrap();
        let restored = TaskTable::new(data.path().into());
        let list = restored.list().await;
        assert_eq!(list.iter().find(|t| t.id == "paused").unwrap(), &task);
        let missed = list.iter().find(|t| t.id == "task").unwrap();
        assert!(missed.next_run.is_none());
        assert_eq!(missed.last_status.as_deref(), Some("skipped"));
        assert!(restored.set_enabled("paused", true).await.is_err());
    }

    #[tokio::test]
    async fn claim_failure_cannot_execute_and_can_retry() {
        let (_data, root, table, task) = fixture();
        let now = Local::now();
        table.upsert(task).await.unwrap();
        let dir = root.path().join(".codewave/tasks");
        let backup = root.path().join("backup");
        std::fs::rename(&dir, &backup).unwrap();
        std::fs::write(&dir, b"blocked").unwrap();
        let due = table.collect_due(now).await.remove(0);
        assert!(table.claim("task", Some(&due), now).await.is_err());
        assert!(table.list().await[0].active_run.is_none());
        std::fs::remove_file(&dir).unwrap();
        std::fs::rename(backup, dir).unwrap();
        let due = table.collect_due(now).await.remove(0);
        assert!(
            table
                .claim("task", Some(&due), now)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn completion_write_failure_keeps_memory_result_and_retry_is_idempotent() {
        let (data, root, table, task) = fixture();
        let now = Local::now();
        table.upsert(task).await.unwrap();
        let claimed = table.claim("task", None, now).await.unwrap().unwrap();
        let dir = root.path().join(".codewave/tasks");
        let backup = root.path().join("backup");
        std::fs::rename(&dir, &backup).unwrap();
        std::fs::write(&dir, b"blocked").unwrap();
        assert!(
            table
                .finish_run("task", &claimed.run.run_id, result("ok"))
                .await
                .is_err()
        );
        let current = table.list().await.remove(0);
        assert_eq!(current.runs.len(), 1);
        assert!(current.active_run.is_none());
        assert!(current.persistence_error.is_some());
        assert!(table.claim("task", None, now).await.is_err());
        std::fs::remove_file(&dir).unwrap();
        std::fs::rename(backup, dir).unwrap();
        for _ in 0..2 {
            table.retry_unsaved().await;
        }
        let restored = TaskTable::new(data.path().into()).list().await.remove(0);
        assert_eq!(restored.runs.len(), 1);
        assert_eq!(restored.last_status.as_deref(), Some("ok"));
        assert!(restored.persistence_error.is_none());
    }

    #[tokio::test]
    async fn manual_paused_preserves_schedule_and_history_cap() {
        let table = TaskTable::default();
        let now = Local::now();
        let mut task = task("t", now);
        task.enabled = false;
        let next = task.next_run.clone();
        table.upsert(task).await.unwrap();
        for index in 0..21 {
            let claim = table.claim("t", None, now).await.unwrap().unwrap();
            assert_eq!(claim.run.source, RUN_SOURCE_MANUAL);
            let mut run = result("ok");
            run.summary = index.to_string();
            run.source = RUN_SOURCE_MANUAL.into();
            table.finish_run("t", &claim.run.run_id, run).await.unwrap();
        }
        let task = table.list().await.remove(0);
        assert_eq!(task.next_run, next);
        assert_eq!(task.runs.len(), 20);
        assert_eq!(task.runs[0].summary, "20");
        assert!(table.collect_due(now).await.is_empty());
    }

    #[tokio::test]
    async fn delete_during_run_never_revives_task_or_project() {
        let (data, root, table, task) = fixture();
        let now = Local::now();
        table.upsert(task.clone()).await.unwrap();
        let claimed = table.claim("task", None, now).await.unwrap().unwrap();
        table.remove("task").await.unwrap();
        table
            .finish_run("task", &claimed.run.run_id, result("ok"))
            .await
            .unwrap();
        assert!(table.list().await.is_empty());
        assert!(!root.path().join(".codewave/tasks/task.json").exists());
        table.upsert(task).await.unwrap();
        let claimed = table.claim("task", None, now).await.unwrap().unwrap();
        table.delete_project("project").await.unwrap();
        table
            .finish_run("task", &claimed.run.run_id, result("ok"))
            .await
            .unwrap();
        table.retry_unsaved().await;
        assert!(table.list().await.is_empty());
        assert!(!root.path().join(".codewave").exists());
        assert!(!data.path().join("projects/project").exists());
    }

    #[tokio::test]
    async fn invalid_schedule_is_quarantined_before_candidate_creation() {
        let table = TaskTable::default();
        let now = Local::now();
        table.upsert(task("t", now)).await.unwrap();
        table
            .state
            .lock()
            .unwrap()
            .tasks
            .get_mut("t")
            .unwrap()
            .schedule = "invalid".into();
        assert!(table.collect_due(now).await.is_empty());
        assert!(
            table
                .diagnostics()
                .await
                .issues
                .iter()
                .any(|i| i.kind == "schedule")
        );
        assert!(table.list().await[0].runs.is_empty());
    }

    #[tokio::test]
    async fn cancelled_pending_and_running_guards_release_only_their_identity() {
        let table = Arc::new(TaskTable::default());
        let now = Local::now();
        table.upsert(task("t", now)).await.unwrap();
        let candidate = table.collect_due(now).await.remove(0);
        let pending = PendingGuard {
            table: table.clone(),
            candidate,
        };
        drop(pending);
        tokio::task::yield_now().await;
        assert_eq!(table.collect_due(now).await.len(), 1);
        let claimed = table.claim("t", None, now).await.unwrap().unwrap();
        let guard = TaskRunGuard {
            table: table.clone(),
            core: None,
            execution: None,
            task_id: "t".into(),
            run: claimed.run,
            armed: true,
        };
        drop(guard);
        tokio::task::yield_now().await;
        let current = table.list().await.remove(0);
        assert!(current.active_run.is_none());
        assert_eq!(current.last_status.as_deref(), Some("error"));
    }

    #[tokio::test]
    async fn restore_and_mutations_share_initialization_gate() {
        let (data, _root, table, task) = fixture();
        table.upsert(task).await.unwrap();
        let restored = TaskTable::new(data.path().into());
        let ((), deleted) = tokio::join!(restored.restore(), restored.remove("task"));
        deleted.unwrap().unwrap();
        restored.restore().await;
        assert!(restored.list().await.is_empty());
        assert!(TaskTable::new(data.path().into()).list().await.is_empty());
    }

    #[test]
    fn serde_defaults_and_schedule_bounds() {
        let value = serde_json::json!({ "id":"task", "name":"task", "instruction":"safe", "schedule":"every:1 h", "next_run":null, "last_status":null, "last_summary":null });
        let task: ScheduledTask = serde_json::from_value(value).unwrap();
        assert!(task.enabled);
        assert!(task.runs.is_empty());
        assert!(task.active_run.is_none());
        assert!(parse_schedule("every:30 d").is_ok());
        assert!(parse_schedule("every:31 d").is_err());
        assert!(parse_schedule("every:18446744073709551615 d").is_err());
        assert!(
            parse_schedule("once:2099-01-01T00:00:00Z")
                .unwrap()
                .next_after(Local::now())
                .is_some()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn virtual_time_dispatch_revocation_has_zero_execution_effects() {
        for action in ["pause", "delete", "edit"] {
            let table = Arc::new(TaskTable::default());
            let now = Local::now();
            table.upsert(task("b", now)).await.unwrap();
            let candidate = table.collect_due(now).await.remove(0);
            let hold = table.exec_lock.clone().lock_owned().await;
            let effects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = effects.clone();
            let worker = table.clone();
            let (ready, queued) = tokio::sync::oneshot::channel();
            let dispatch = tokio::spawn(async move {
                ready.send(()).unwrap();
                dispatch_candidate(
                    worker,
                    &candidate,
                    || now,
                    move |_, _lock| async move {
                        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    },
                )
                .await
                .unwrap()
            });
            queued.await.unwrap();
            tokio::time::advance(Duration::from_secs(150)).await;
            for _ in 0..10 {
                assert!(table.collect_due(now).await.is_empty());
            }
            match action {
                "pause" => {
                    table.set_enabled("b", false).await.unwrap();
                }
                "delete" => {
                    table.remove("b").await.unwrap();
                }
                "edit" => {
                    table
                        .update("b", "new".into(), "new".into(), "every:1 d".into())
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            drop(hold);
            assert!(!dispatch.await.unwrap());
            assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert!(table.list().await.iter().all(|t| t.runs.is_empty()));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fake_executor_serializes_tasks_and_retains_each_terminal_result() {
        let table = Arc::new(TaskTable::default());
        let now = Local::now();
        for id in ["ok", "error", "skipped"] {
            let mut item = task(id, now);
            item.schedule = format!("once:{}", now.to_rfc3339());
            table.upsert(item).await.unwrap();
        }
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut workers = Vec::new();
        for candidate in table.collect_due(now).await {
            let executor_table = table.clone();
            let dispatcher = table.clone();
            let active = active.clone();
            let completed = completed.clone();
            workers.push(tokio::spawn(async move {
                dispatch_candidate(
                    dispatcher,
                    &candidate,
                    || now,
                    move |snapshot, execution| async move {
                        let _execution = execution;
                        assert_eq!(active.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        executor_table
                            .finish_run(
                                &snapshot.task.id,
                                &snapshot.run.run_id,
                                result(&snapshot.task.id),
                            )
                            .await
                            .unwrap();
                        active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                        completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    },
                )
                .await
                .unwrap()
            }));
        }
        for worker in workers {
            assert!(worker.await.unwrap());
        }
        assert_eq!(completed.load(std::sync::atomic::Ordering::SeqCst), 3);
        for item in table.list().await {
            assert_eq!(item.runs.len(), 1);
            assert_eq!(item.last_status, Some(item.id));
            assert!(item.next_run.is_none());
        }
    }

    #[tokio::test]
    async fn failed_recovery_write_retains_interruption_and_never_replays() {
        let (data, _root, table, mut item) = fixture();
        let now = Local::now();
        item.schedule = format!("once:{}", now.to_rfc3339());
        item.next_run = Some(now.to_rfc3339());
        table.upsert(item).await.unwrap();
        table.claim("task", None, now).await.unwrap().unwrap();
        let recovered = TaskTable::new(data.path().into());
        recovered
            .fail_writes
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let item = recovered.list().await.remove(0);
        assert!(item.persistence_error.is_some());
        assert!(item.active_run.is_none());
        assert!(item.next_run.is_none());
        assert_eq!(item.runs.len(), 1);
        assert!(recovered.collect_due(now).await.is_empty());
        assert!(
            recovered
                .diagnostics()
                .await
                .issues
                .iter()
                .any(|issue| issue.kind == "restore_write")
        );
        recovered
            .fail_writes
            .store(false, std::sync::atomic::Ordering::SeqCst);
        recovered.retry_unsaved().await;
        assert_eq!(
            TaskTable::new(data.path().into()).list().await[0]
                .runs
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn real_project_cron_restore_corrects_future_but_preserves_matching_overdue() {
        let (data, _root, table, mut item) = fixture();
        item.schedule = "cron:0 9 * * *".into();
        item.next_run = Some(
            (Local::now() + chrono::Duration::days(1))
                .format("%Y-%m-%dT17:00:00%:z")
                .to_string(),
        );
        table.upsert(item.clone()).await.unwrap();
        item.id = "overdue".into();
        item.next_run = Some(
            (Local::now() - chrono::Duration::days(1))
                .format("%Y-%m-%dT09:00:00%:z")
                .to_string(),
        );
        table.upsert(item.clone()).await.unwrap();
        let restored = TaskTable::new(data.path().into()).list().await;
        assert_eq!(
            restored
                .iter()
                .find(|t| t.id == "overdue")
                .unwrap()
                .next_run,
            item.next_run
        );
        let repaired = restored.iter().find(|t| t.id == "task").unwrap();
        let next = DateTime::parse_from_rfc3339(repaired.next_run.as_ref().unwrap())
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(chrono::Timelike::hour(&next), 9);
        assert!(next > Local::now());
    }
    #[tokio::test]
    async fn post_rename_sync_warning_publishes_edits_instead_of_claiming_rollback() {
        let (_data, root, table, item) = fixture();
        table
            .fail_directory_sync
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let created = table.upsert(item).await.unwrap();
        assert!(created.persistence_error.is_some());
        let updated = table
            .update(
                "task",
                "changed".into(),
                "new instruction".into(),
                "every:1 d".into(),
            )
            .await
            .unwrap();
        assert!(updated.persistence_error.is_some());
        assert_eq!(table.list().await[0].name, "changed");
        let disk: ScheduledTask = serde_json::from_slice(
            &std::fs::read(root.path().join(".codewave/tasks/task.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(disk.name, "changed");
        assert_eq!(disk.instruction, "new instruction");
        let paused = table.set_enabled("task", false).await.unwrap();
        assert!(!paused.enabled);
        assert!(paused.persistence_error.is_some());
        table
            .fail_directory_sync
            .store(false, std::sync::atomic::Ordering::SeqCst);
        table.retry_unsaved().await;
        assert!(table.list().await[0].persistence_error.is_none());
    }

    #[tokio::test]
    async fn post_rename_claim_uncertainty_quarantines_until_safe_rollback_is_durable() {
        let (_data, root, table, item) = fixture();
        let now = Local::now();
        let original_next = item.next_run.clone();
        table.upsert(item).await.unwrap();
        let candidate = table.collect_due(now).await.remove(0);
        table
            .fail_directory_sync
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(table.claim("task", Some(&candidate), now).await.is_err());
        let disk: ScheduledTask = serde_json::from_slice(
            &std::fs::read(root.path().join(".codewave/tasks/task.json")).unwrap(),
        )
        .unwrap();
        assert!(disk.active_run.is_some());
        let quarantined = table.list().await.remove(0);
        assert!(quarantined.persistence_error.is_some());
        assert!(quarantined.active_run.is_none());
        assert_eq!(quarantined.next_run, original_next);
        assert!(table.collect_due(now).await.is_empty());
        assert!(table.claim("task", None, now).await.is_err());
        table.retry_unsaved().await;
        assert!(table.list().await[0].persistence_error.is_some());
        table
            .fail_directory_sync
            .store(false, std::sync::atomic::Ordering::SeqCst);
        table.retry_unsaved().await;
        let candidate = table.collect_due(now).await.remove(0);
        assert!(
            table
                .claim("task", Some(&candidate), now)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn post_rename_finish_uncertainty_keeps_real_result_and_warning() {
        let (_data, root, table, item) = fixture();
        table.upsert(item).await.unwrap();
        let run = table
            .claim("task", None, Local::now())
            .await
            .unwrap()
            .unwrap()
            .run;
        table
            .fail_directory_sync
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            table
                .finish_run("task", &run.run_id, result("ok"))
                .await
                .is_err()
        );
        assert!(
            table
                .finish_run("task", &run.run_id, result("ok"))
                .await
                .is_err()
        );
        let current = table.list().await.remove(0);
        let disk: ScheduledTask = serde_json::from_slice(
            &std::fs::read(root.path().join(".codewave/tasks/task.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(current.runs, disk.runs);
        assert_eq!(current.runs.len(), 1);
        assert!(current.active_run.is_none());
        assert!(current.persistence_error.is_some());
        table
            .fail_directory_sync
            .store(false, std::sync::atomic::Ordering::SeqCst);
        table.retry_unsaved().await;
        assert!(table.list().await[0].persistence_error.is_none());
    }
    #[tokio::test]
    async fn task_scope_and_logs_share_registered_project_and_do_not_recreate_deletion() {
        let (data, root, _table, item) = fixture();
        let roots = crate::tools::pathutil::WriteRoots {
            project_dir: None,
            workspace: std::fs::canonicalize(root.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(data.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let (workspace, project_id, project_dir, extra) = task_scope(&core, &item).unwrap();
        // Project scope preserves the registered spelling. On macOS a temp
        // directory under /var canonicalizes to /private/var.
        assert_eq!(workspace, root.path());
        assert_eq!(std::fs::canonicalize(&workspace).unwrap(), roots.workspace);
        assert_eq!(project_id.as_deref(), Some("project"));
        assert_eq!(project_dir, Some(root.path().join(".codewave")));
        assert!(extra.is_empty());
        let missing = ScheduledTask {
            project_id: Some("missing".into()),
            ..item.clone()
        };
        assert!(task_scope(&core, &missing).is_none());
        let free = ScheduledTask {
            project_id: None,
            ..item.clone()
        };
        assert_eq!(task_scope(&core, &free).unwrap().0, roots.data_dir);
        task_log(&core, &item, "started").await;
        task_log(&core, &item, "finished").await;
        let log = std::fs::read_to_string(root.path().join(".codewave/logs/task.log")).unwrap();
        assert!(log.contains("started") && log.contains("finished"));
        task_log(&core, &missing, "not written").await;
        assert!(!data.path().join("projects/missing").exists());
        core.tasks.delete_project("project").await.unwrap();
        task_log(&core, &item, "late completion").await;
        assert!(!root.path().join(".codewave").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn task_scope_preserves_registered_symlink_path() {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("project-alias");
        std::os::unix::fs::symlink(root.path(), &alias).unwrap();
        project(data.path(), &alias, "project");
        let roots = crate::tools::pathutil::WriteRoots {
            project_dir: None,
            workspace: std::fs::canonicalize(root.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(data.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let mut item = task("task", Local::now());
        item.project_id = Some("project".into());
        let (workspace, project_id, project_dir, _) = task_scope(&core, &item).unwrap();
        assert_eq!(workspace, alias);
        assert_eq!(std::fs::canonicalize(&workspace).unwrap(), roots.workspace);
        assert_eq!(project_id.as_deref(), Some("project"));
        assert_eq!(project_dir, Some(alias.join(".codewave")));
    }
}
