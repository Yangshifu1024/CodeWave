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
    /// 全局串行执行（[docs/p2-plan](../../../docs/p2-plan.md) §3：同时至多 1 个任务运行）
    pub exec_lock: Mutex<()>,
    /// 持久化根（~/.codewave）；测试注入临时目录
    pub data_dir: std::path::PathBuf,
}

impl TaskTable {
    /// 以给定数据根构造空任务表。
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        TaskTable {
            tasks: Mutex::new(HashMap::new()),
            exec_lock: Mutex::new(()),
            data_dir,
        }
    }

    /// 新增/更新任务（先落盘再进内存表）。
    pub async fn upsert(&self, task: ScheduledTask) {
        self.persist(&task);
        self.tasks.lock().await.insert(task.id.clone(), task);
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
    pub async fn tick(&self, now: DateTime<Local>) -> Vec<ScheduledTask> {
        let mut due = Vec::new();
        let mut tasks = self.tasks.lock().await;
        let ids: Vec<String> = tasks.keys().cloned().collect();
        for id in ids {
            let Some(t) = tasks.get_mut(&id) else {
                continue;
            };
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
                    Ok(Some(n)) => t.next_run = Some(n.to_rfc3339()),
                    // once：触发后移除（Every/Cron 经解析期上限后恒返回 Some）
                    Ok(None) => {
                        tasks.remove(&id);
                    }
                    Err(e) => {
                        // 计划表达式损坏（如手改任务文件）：隔离而非静默删除循环任务——
                        // next_run=None 跳过后续 tick
                        tracing::warn!("计划任务「{}」计划表达式失效（{e}），已暂停调度", t.name);
                        t.next_run = None;
                    }
                }
            }
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
                    run_task(core, task).await;
                });
            }
        }
    });
}

/// 单任务执行：全新隔离上下文（独立 SessionRuntime），在串行锁内运行。
async fn run_task(core: Arc<crate::core::agent::AgentCore>, task: ScheduledTask) {
    let _guard = core.tasks.exec_lock.lock().await;
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
}
