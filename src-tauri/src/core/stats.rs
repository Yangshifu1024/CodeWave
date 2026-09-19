//! token 统计（[docs/p2-plan](../../../docs/p2-plan.md) §4）：异步有界队列（2048；满则丢弃并计数）→ 按天落盘
//! stats/YYYY-MM-DD.json；保留 90 天；查询侧读时聚合。

use crate::util::atomic::atomic_write;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

/// 统计队列容量。
pub const QUEUE_CAP: usize = 2048;
/// 定时冲刷间隔。
pub const FLUSH_EVERY: Duration = Duration::from_secs(60);
/// 待落盘条数达到该值即提前冲刷。
pub const FLUSH_RECORDS: usize = 512;
/// 统计文件保留天数。
pub const RETAIN_DAYS: i64 = 90;

/// 用量来源（L10：主会话/子代理/任务分账可见；[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md) 增 compact；[docs/session-auto-title](../../../docs/session-auto-title.md) 增 title）。
pub const KIND_MAIN: &str = "main";
/// 子代理来源。
pub const KIND_SUB: &str = "sub";
/// 计划任务来源。
pub const KIND_TASK: &str = "task";
/// 上下文压缩调用来源。
pub const KIND_COMPACT: &str = "compact";
/// 会话自动命名来源。
pub const KIND_TITLE: &str = "title";

/// 一条用量记录（写入侧；record 非阻塞提交进队列）。
#[derive(Debug, Clone, Serialize)]
pub struct UsageRecord {
    /// 会话 id（任务运行用合成 id）
    pub session: String,
    /// 计账模型 id
    pub model_id: String,
    /// 工作区路径（字符串化，作聚合键）
    pub workspace: String,
    /// 输入 token
    pub input: u64,
    /// 输出 token
    pub output: u64,
    /// 缓存读 token
    pub cache_read: u64,
    /// 缓存写 token
    pub cache_write: u64,
    /// run 次数
    pub runs: u64,
    /// 来源：main / sub / task / compact / title
    pub kind: String,
}

/// 单桶聚合（按模型/工作区/来源共用的五字段累加器）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelAgg {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub runs: u64,
}

impl ModelAgg {
    /// 累加一条记录的五个字段（缺一会丢数，merge 同理）。
    fn add(&mut self, r: &UsageRecord) {
        self.input += r.input;
        self.output += r.output;
        self.cache_read += r.cache_read;
        self.cache_write += r.cache_write;
        self.runs += r.runs;
    }
}

/// 单日统计文件（stats/YYYY-MM-DD.json）的形态。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DailyStats {
    /// 日期（YYYY-MM-DD，本地时区）
    pub date: String,
    /// 按模型聚合
    pub by_model: HashMap<String, ModelAgg>,
    /// 按工作区聚合
    pub by_workspace: HashMap<String, ModelAgg>,
    /// 按来源聚合（旧文件缺此字段 → serde 默认空表）
    #[serde(default)]
    pub by_kind: HashMap<String, ModelAgg>,
    /// 全量合计
    pub total: ModelAgg,
}

/// 统计收集器：非阻塞 record + 后台 writer 定时落盘。
pub struct StatsCollector {
    /// 惰性初始化：首次 record 时（异步上下文内）才启动通道与 writer
    tx: std::sync::OnceLock<mpsc::Sender<UsageRecord>>,
    /// 数据目录（构造注入；OnceLock 初始化前确定）
    data_dir: PathBuf,
    /// 队列满导致的累计丢弃计数
    dropped: AtomicU64,
}

impl StatsCollector {
    /// 构造收集器（writer 待首次 record 惰性启动）。
    pub fn new(data_dir: PathBuf) -> Self {
        StatsCollector {
            tx: std::sync::OnceLock::new(),
            data_dir,
            dropped: AtomicU64::new(0),
        }
    }

    /// 须在异步上下文调用：确保 writer 已启动（OnceLock 双检 + tokio::spawn）。
    fn ensure_writer(&self) -> Option<&mpsc::Sender<UsageRecord>> {
        if let Some(tx) = self.tx.get() {
            return Some(tx);
        }
        let data_dir = self.data_dir.clone();
        let (tx, rx) = mpsc::channel::<UsageRecord>(QUEUE_CAP);
        let _ = self.tx.set(tx);
        tokio::spawn(async move {
            Self::writer(data_dir, rx).await;
        });
        self.tx.get()
    }

    /// 非阻塞提交；队列满则丢弃并计数（绝不阻塞聊天热路径）。
    pub fn record(&self, r: UsageRecord) {
        let Some(tx) = self.ensure_writer() else {
            return;
        };
        if tx.try_send(r).is_err() {
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 100 == 1 {
                tracing::warn!("统计队列已满，累计丢弃 {n} 条");
            }
        }
    }

    /// 累计丢弃条数（诊断用）。
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 后台 writer：聚合进 pending 表，定时/定量冲刷；通道全关退出前做最终冲刷。
    async fn writer(data_dir: PathBuf, mut rx: mpsc::Receiver<UsageRecord>) {
        let stats_dir = data_dir.join("stats");
        let _ = std::fs::create_dir_all(&stats_dir);
        let mut pending: HashMap<String, DailyStats> = HashMap::new();
        let mut last_flush = tokio::time::Instant::now();
        loop {
            let timeout = FLUSH_EVERY.saturating_sub(last_flush.elapsed());
            let received = tokio::select! {
                r = rx.recv() => match r {
                    Some(rec) => rec,
                        None => break, // 发送端全部丢弃（退出）
                },
                _ = tokio::time::sleep(timeout) => {
                    flush(&stats_dir, &mut pending);
                    last_flush = tokio::time::Instant::now();
                    continue;
                }
            };
            let date = chrono::Local::now().format("%Y-%m-%d").to_string();
            let day = pending.entry(date.clone()).or_insert_with(|| DailyStats {
                date: date.clone(),
                ..Default::default()
            });
            let rec = received;
            let m = day.by_model.entry(rec.model_id.clone()).or_default();
            m.add(&rec);
            let w = day.by_workspace.entry(rec.workspace.clone()).or_default();
            w.add(&rec);
            let k = day.by_kind.entry(rec.kind.clone()).or_default();
            k.add(&rec);
            day.total.add(&rec);

            let pending_count: usize = pending.values().map(|d| d.total.runs as usize).sum();
            if last_flush.elapsed() >= FLUSH_EVERY || pending_count >= FLUSH_RECORDS {
                flush(&stats_dir, &mut pending);
                last_flush = tokio::time::Instant::now();
            }
        }
        flush(&stats_dir, &mut pending);
    }
}

/// 冲刷 pending 各日数据：与磁盘同日旧文件逐桶合并后原子写；随后清理 90 天前过期文件。
fn flush(stats_dir: &std::path::Path, pending: &mut HashMap<String, DailyStats>) {
    if pending.is_empty() {
        return;
    }
    for (date, day) in pending.iter() {
        // 读旧数据合并（多次启动 / 未冲刷的遗留）
        let path = stats_dir.join(format!("{date}.json"));
        let mut merged = day.clone();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(old) = serde_json::from_str::<DailyStats>(&text) {
                for (k, v) in old.by_model {
                    let m = merged.by_model.entry(k).or_default();
                    m.input += v.input;
                    m.output += v.output;
                    m.cache_read += v.cache_read;
                    m.cache_write += v.cache_write;
                    m.runs += v.runs;
                }
                for (k, v) in old.by_workspace {
                    let w = merged.by_workspace.entry(k).or_default();
                    w.input += v.input;
                    w.output += v.output;
                    w.cache_read += v.cache_read;
                    w.cache_write += v.cache_write;
                    w.runs += v.runs;
                }
                for (k, v) in old.by_kind {
                    let g = merged.by_kind.entry(k).or_default();
                    g.input += v.input;
                    g.output += v.output;
                    g.cache_read += v.cache_read;
                    g.cache_write += v.cache_write;
                    g.runs += v.runs;
                }
                merged.total.input += old.total.input;
                merged.total.output += old.total.output;
                merged.total.cache_read += old.total.cache_read;
                merged.total.cache_write += old.total.cache_write;
                merged.total.runs += old.total.runs;
            }
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&merged) {
            if let Err(e) = atomic_write(&path, &bytes) {
                tracing::warn!("统计落盘失败（{date}）：{e}");
            }
        }
    }
    pending.clear();
    // 90 天保留期清理
    if let Ok(rd) = std::fs::read_dir(stats_dir) {
        let cutoff = chrono::Local::now() - chrono::Duration::days(RETAIN_DAYS);
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(d) = name.strip_suffix(".json") {
                if let Ok(day) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                    if chrono::DateTime::<chrono::Local>::from_naive_utc_and_offset(
                        day.and_hms_opt(0, 0, 0).unwrap(),
                        *chrono::Local::now().offset(),
                    ) < cutoff
                    {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
    }
}

/// 查询最近 n 天（服务端聚合，直接读文件）。
pub fn query(data_dir: &std::path::Path, days: u32) -> Vec<DailyStats> {
    let stats_dir = data_dir.join("stats");
    let mut out: Vec<DailyStats> = Vec::new();
    for i in 0..days.min(90) {
        let date = (chrono::Local::now() - chrono::Duration::days(i as i64))
            .format("%Y-%m-%d")
            .to_string();
        if let Ok(text) = std::fs::read_to_string(stats_dir.join(format!("{date}.json"))) {
            if let Ok(d) = serde_json::from_str::<DailyStats>(&text) {
                out.push(d);
            }
        }
    }
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn collect_flush_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let collector = Arc::new(StatsCollector::new(dir.path().to_path_buf()));
        collector.record(UsageRecord {
            session: "s1".into(),
            model_id: "m1".into(),
            workspace: "/w".into(),
            input: 100,
            output: 50,
            cache_read: 10,
            cache_write: 5,
            runs: 1,
            kind: KIND_MAIN.into(),
        });
        collector.record(UsageRecord {
            session: "s2".into(),
            model_id: "m2".into(),
            workspace: "/w".into(),
            input: 200,
            output: 20,
            cache_read: 0,
            cache_write: 0,
            runs: 1,
            kind: KIND_SUB.into(),
        });
        // 显式关闭发送端以触发最终冲刷
        let inner: mpsc::Sender<UsageRecord> = collector.tx.get().unwrap().clone();
        drop(inner);
        let closed = std::sync::Arc::downgrade(&collector);
        drop(collector);
        while closed.upgrade().is_some() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let days = query(dir.path(), 1);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].total.input, 300);
        assert_eq!(days[0].total.runs, 2);
        assert_eq!(days[0].by_model.len(), 2);
        assert_eq!(days[0].by_workspace["/w"].input, 300);
        // L10：分来源聚合可见
        assert_eq!(days[0].by_kind[KIND_MAIN].input, 100);
        assert_eq!(days[0].by_kind[KIND_SUB].input, 200);
    }

    fn day(date: &str, model: &str, input: u64, runs: u64, kind: &str) -> DailyStats {
        let mut d = DailyStats {
            date: date.into(),
            ..Default::default()
        };
        let m = d.by_model.entry(model.into()).or_default();
        m.input += input;
        m.runs += runs;
        let w = d.by_workspace.entry("/w".into()).or_default();
        w.input += input;
        w.cache_read += 7;
        w.cache_write += 3;
        w.runs += runs;
        let k = d.by_kind.entry(kind.into()).or_default();
        k.input += input;
        k.runs += runs;
        d.total.input += input;
        d.total.runs += runs;
        d
    }

    #[test]
    fn flush_merges_into_existing_same_day_file() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        // 模拟上次启动的遗留：同一天、部分重叠的桶
        let old = day(&today, "m1", 10, 1, KIND_MAIN);
        std::fs::create_dir_all(dir.path().join("stats")).unwrap();
        std::fs::write(
            dir.path().join("stats").join(format!("{today}.json")),
            serde_json::to_vec_pretty(&old).unwrap(),
        )
        .unwrap();

        let mut pending = HashMap::new();
        let mut new_stats = day(&today, "m1", 100, 2, KIND_MAIN);
        // day() 预置了 by_workspace 的 cache 计数；清零以模拟「新内存记录没有 cache
        // 计数」——合并后旧文件的 cache 字段必须原样浮出
        if let Some(w) = new_stats.by_workspace.get_mut("/w") {
            w.cache_read = 0;
            w.cache_write = 0;
        }
        pending.insert(today.clone(), new_stats);
        flush(&dir.path().join("stats"), &mut pending);
        assert!(pending.is_empty(), "pending cleared after flush");

        let days = query(dir.path(), 1);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].total.input, 110, "old + new totals merged");
        assert_eq!(days[0].total.runs, 3);
        assert_eq!(days[0].by_model["m1"].input, 110);
        assert_eq!(days[0].by_kind[KIND_MAIN].input, 110);
        // 新内存记录没有 cache 计数；旧 by_workspace 的 cache 字段在合并后保留
        assert_eq!(days[0].by_workspace["/w"].cache_read, 7);
        assert_eq!(days[0].by_workspace["/w"].cache_write, 3);
    }

    /// [docs/arithmetic-audit](../../../docs/arithmetic-audit.md)#2：by_workspace 合并必须像 by_model/by_kind 一样带齐五个字段
    /// （曾是潜伏缺陷：同日二次冲刷时 cache_read/cache_write 被丢掉）。
    #[test]
    fn flush_should_merge_by_workspace_cache_fields() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let old = day(&today, "m1", 10, 1, KIND_MAIN);
        std::fs::create_dir_all(dir.path().join("stats")).unwrap();
        std::fs::write(
            dir.path().join("stats").join(format!("{today}.json")),
            serde_json::to_vec_pretty(&old).unwrap(),
        )
        .unwrap();
        // 旧记录带 cache 计数，从空 pending 表冲刷
        let mut pending: HashMap<String, DailyStats> = HashMap::new();
        pending.insert(today.clone(), DailyStats::default());
        flush(&dir.path().join("stats"), &mut pending);
        let days = query(dir.path(), 1);
        assert_eq!(
            days[0].by_workspace["/w"].cache_read, 7,
            "old cache_read must survive"
        );
    }

    #[test]
    fn flush_writes_each_pending_date_and_query_sorts_ascending() {
        let dir = tempfile::tempdir().unwrap();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let yesterday = (chrono::Local::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        let mut pending = HashMap::new();
        pending.insert(today.clone(), day(&today, "m1", 1, 1, KIND_MAIN));
        pending.insert(yesterday.clone(), day(&yesterday, "m1", 2, 1, KIND_TASK));
        flush(&dir.path().join("stats"), &mut pending);
        let days = query(dir.path(), 7);
        let dates: Vec<&str> = days.iter().map(|d| d.date.as_str()).collect();
        assert_eq!(
            dates,
            vec![yesterday.as_str(), today.as_str()],
            "ascending by date"
        );
        assert_eq!(days[0].total.input, 2);
        assert_eq!(days[1].total.input, 1);
        // 跨天的 kind 桶互不混淆
        assert!(days[0].by_kind.contains_key(KIND_TASK));
        assert!(days[1].by_kind.contains_key(KIND_MAIN));
    }

    #[test]
    fn retention_removes_files_older_than_90_days() {
        let dir = tempfile::tempdir().unwrap();
        let stats_dir = dir.path().join("stats");
        std::fs::create_dir_all(&stats_dir).unwrap();
        let ancient = stats_dir.join("2000-01-01.json");
        std::fs::write(&ancient, b"{}").unwrap();
        let recent_name = chrono::Local::now().format("%Y-%m-%d.json").to_string();
        let recent = stats_dir.join(&recent_name);
        std::fs::write(&recent, b"{}").unwrap();
        // 非日期文件绝不触碰
        let stray = stats_dir.join("notes.txt");
        std::fs::write(&stray, b"x").unwrap();

        let mut pending = HashMap::new();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        pending.insert(today.clone(), day(&today, "m1", 1, 1, KIND_MAIN));
        flush(&stats_dir, &mut pending);

        assert!(!ancient.exists(), "90+ day old stats must be pruned");
        assert!(recent.exists(), "today's stats must survive");
        assert!(stray.exists(), "non-date files are not cleaned");
    }

    #[test]
    fn query_clamps_window_to_90_days() {
        let dir = tempfile::tempdir().unwrap();
        // 请求 200 天 → 内部钳制；空目录不 panic、返回空结果
        let days = query(dir.path(), 200);
        assert!(days.is_empty());
    }
}
