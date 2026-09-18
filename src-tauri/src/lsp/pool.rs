//! 项目级 server 池：惰性启动、复用、重启一次、LRU 回收、按项目/全量关闭。
//!
//! key = `(语言根, 语言)`——**同一项目可以有多个同语言实例**（本仓库 `ui/` 与根各一套）。
//! `project_id` 记在条目上（`acquire` 签名不含它），供 [`LspPool::shutdown_project`] 归组。

use super::Lang;
use super::client::LspClient;
use super::discovery::ServerResolution;
use super::server_spec;
use crate::core::config::LspSettings;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// `acquire` 失败的两态：还在启动（调用方稍后再来） vs 不可用（别再试）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireError {
    /// server 正在冷启动（调用方**不等**，本轮按「加载中」处理）
    Starting,
    /// 不可用（含反复退出、并发上限），携带原因
    Unavailable(String),
}

/// 池内条目状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    /// 正在 spawn + initialize
    Starting,
    /// 可用
    Ready,
    /// 进程死了（允许再重启一次）
    Dead,
    /// 判定不可用（不再重试）
    Unavailable,
}

/// 池 key：语言根 + 语言。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    /// 语言根目录（`workspace::root_for_file` 的结论）
    pub root: PathBuf,
    /// 语言
    pub lang: Lang,
}

struct Entry {
    project_id: Option<String>,
    state: EntryState,
    client: Option<Arc<LspClient>>,
    last_used: Instant,
    restarts: u32,
    detail: String,
    generation: u64,
}

struct PoolInner {
    entries: Mutex<HashMap<PoolKey, Entry>>,
    max_servers: Mutex<usize>,
    env: Mutex<Vec<(String, String)>>,
    next_generation: AtomicU64,
}

/// 池的对外句柄（内部 `Arc<PoolInner>`，可安全克隆进后台任务）。
#[derive(Clone)]
pub struct LspPool {
    inner: Arc<PoolInner>,
}

impl Default for LspPool {
    fn default() -> Self {
        LspPool::new()
    }
}

impl LspPool {
    /// 空池（并发上限默认 8）。
    pub fn new() -> Self {
        LspPool {
            inner: Arc::new(PoolInner {
                entries: Mutex::new(HashMap::new()),
                max_servers: Mutex::new(8),
                env: Mutex::new(Vec::new()),
                next_generation: AtomicU64::new(1),
            }),
        }
    }

    /// 设置子进程追加环境变量（一般是新鲜 PATH）。
    pub fn set_env(&self, env: Vec<(String, String)>) {
        *self.inner.env.lock().unwrap() = env;
    }

    /// 设置并发上限。
    pub fn set_max_servers(&self, max: usize) {
        *self.inner.max_servers.lock().unwrap() = max.max(1);
    }

    /// 当前条目数（诊断/测试用）。
    pub fn len(&self) -> usize {
        self.inner.entries.lock().unwrap().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 取（或启动）某语言根上的 client。
    ///
    /// - `Ready` 且进程活着 → 复用（刷新 `last_used`）；
    /// - 从未启动 → 插入 `Starting` 条目并在后台推进，**立即**返回 [`AcquireError::Starting`]；
    /// - `Dead` 且 `restarts < 1` → 重启一次；再坏 → `Unavailable`。
    pub async fn acquire(
        &self,
        lang: Lang,
        root: &Path,
        project_root: &Path,
        res: &ServerResolution,
        cfg: &LspSettings,
    ) -> Result<Arc<LspClient>, AcquireError> {
        self.set_max_servers(cfg.max_servers);
        let key = PoolKey {
            root: root.to_path_buf(),
            lang,
        };
        let max = *self.inner.max_servers.lock().unwrap();
        let mut entries = self.inner.entries.lock().unwrap();

        if let Some(entry) = entries.get_mut(&key) {
            match entry.state {
                EntryState::Ready => {
                    if let Some(client) = entry.client.clone().filter(|c| c.is_alive()) {
                        entry.last_used = Instant::now();
                        return Ok(client);
                    }
                    // 进程已死：降级为 Dead 走重启路径
                    entry.client = None;
                    entry.state = EntryState::Dead;
                    entry.detail = "server 进程已退出".into();
                }
                EntryState::Starting => return Err(AcquireError::Starting),
                EntryState::Unavailable => {
                    return Err(AcquireError::Unavailable(entry.detail.clone()));
                }
                EntryState::Dead => {}
            }
            if entry.restarts < 1 {
                entry.restarts += 1;
                entry.state = EntryState::Starting;
                entry.last_used = Instant::now();
                let generation = entry.generation + 1;
                entry.generation = generation;
                let detail = entry.detail.clone();
                drop(entries);
                self.spawn_start(
                    lang,
                    key.clone(),
                    generation,
                    res.clone(),
                    project_root,
                    detail,
                    cfg.clone(),
                );
                return Err(AcquireError::Starting);
            }
            entry.state = EntryState::Unavailable;
            let detail = if entry.detail.is_empty() {
                "server 反复退出".to_string()
            } else {
                entry.detail.clone()
            };
            return Err(AcquireError::Unavailable(detail));
        }

        // 新条目：先做 LRU 回收（正在 Starting 的不回收）
        if entries.len() >= max {
            let victim = entries
                .iter()
                .filter(|(_, e)| e.state != EntryState::Starting)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    if let Some(old) = entries.remove(&k) {
                        if let Some(client) = old.client {
                            tokio::spawn(async move { client.shutdown().await });
                        }
                    }
                }
                None => {
                    return Err(AcquireError::Unavailable(format!(
                        "并发 server 已达上限（{max}），且没有可回收的实例"
                    )));
                }
            }
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::SeqCst);
        entries.insert(
            key.clone(),
            Entry {
                project_id: None,
                state: EntryState::Starting,
                client: None,
                last_used: Instant::now(),
                restarts: 0,
                detail: String::new(),
                generation,
            },
        );
        drop(entries);
        self.spawn_start(
            lang,
            key,
            generation,
            res.clone(),
            project_root,
            String::new(),
            cfg.clone(),
        );
        Err(AcquireError::Starting)
    }

    /// 在后台推进「spawn + initialize」，完成后写回状态（调用方不等）。
    fn spawn_start(
        &self,
        lang: Lang,
        key: PoolKey,
        generation: u64,
        res: ServerResolution,
        project_root: &Path,
        fallback_detail: String,
        cfg: LspSettings,
    ) {
        let inner = self.inner.clone();
        let project_root = project_root.to_path_buf();
        tokio::spawn(async move {
            let spec = server_spec::spec(lang);
            let mut env = inner.env.lock().unwrap().clone();
            // Java 专有：把定位到的 JDK 21+ 以 JAVA_HOME 注入子进程（绝不读机器现有的 JAVA_HOME）
            if lang == Lang::Java {
                env.extend(super::discovery::java_launch_env(&cfg));
            }
            let data_dir = project_root.join(".codewave");
            let mut detail = fallback_detail;
            let client = match LspClient::spawn_resolved(
                &res,
                &spec,
                &key.root,
                &env,
                Some(&data_dir),
            )
            .await
            {
                Err(e) => {
                    detail = e;
                    None
                }
                Ok(client) => {
                    let init = server_spec::assemble_init_options(lang, &key.root, &spec);
                    match client.initialize(&key.root, init).await {
                        Ok(_) => Some(client),
                        Err(e) => {
                            detail = e;
                            let _ = client.shutdown().await;
                            None
                        }
                    }
                }
            };
            let mut orphan: Option<Arc<LspClient>> = None;
            {
                let mut entries = inner.entries.lock().unwrap();
                match entries.get_mut(&key) {
                    // 条目已被回收：把刚起来的 client 关掉，绝不留孤儿
                    None => orphan = client,
                    Some(entry) => {
                        if entry.generation != generation {
                            // 更晚的操作已接管该条目
                            orphan = client;
                        } else {
                            match client {
                                Some(client) => {
                                    entry.client = Some(client);
                                    entry.state = EntryState::Ready;
                                    entry.detail.clear();
                                }
                                None => {
                                    entry.client = None;
                                    entry.state = EntryState::Dead;
                                    entry.detail = if detail.is_empty() {
                                        "server 启动失败".into()
                                    } else {
                                        detail
                                    };
                                }
                            }
                        }
                    }
                }
            }
            // 锁已释放（guard 不能跨 await）
            if let Some(client) = orphan {
                client.shutdown().await;
            }
        });
    }

    /// 登记条目所属项目（`acquire` 签名不含 project_id，故由 manager 在成功复用后补记）。
    pub fn tag_project(&self, lang: Lang, root: &Path, project_id: Option<&str>) {
        let key = PoolKey {
            root: root.to_path_buf(),
            lang,
        };
        if let Some(entry) = self.inner.entries.lock().unwrap().get_mut(&key) {
            entry.project_id = project_id.map(|s| s.to_string());
        }
    }

    /// 闲置回收（由 manager 的 ticker 调用；`Starting` 的不回收）。
    pub async fn evict_idle(&self, ttl: Duration) {
        let victims = {
            let mut entries = self.inner.entries.lock().unwrap();
            let keys: Vec<PoolKey> = entries
                .iter()
                .filter(|(_, e)| e.state != EntryState::Starting && e.last_used.elapsed() >= ttl)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| entries.remove(&k))
                .filter_map(|e| e.client)
                .collect::<Vec<_>>()
        };
        for client in victims {
            tracing::debug!(server = %client.label(), "闲置回收 LSP server");
            client.shutdown().await;
        }
    }

    /// 关闭并移除某项目的全部条目。
    pub async fn shutdown_project(&self, project_id: &str) {
        self.shutdown_where(|_, e| e.project_id.as_deref() == Some(project_id))
            .await;
    }

    /// 关闭并移除某语言的全部条目（`restart` 用）。
    pub async fn shutdown_lang(&self, lang: Lang) {
        self.shutdown_where(|k, _| k.lang == lang).await;
    }

    /// 关闭并移除全部条目。
    pub async fn shutdown_all(&self) {
        self.shutdown_where(|_, _| true).await;
    }

    /// 移除 `Unavailable` 条目（重新探测后允许再试）。
    pub fn clear_unavailable(&self) {
        self.inner
            .entries
            .lock()
            .unwrap()
            .retain(|_, e| e.state != EntryState::Unavailable);
    }

    /// 按条件移除条目并关闭其 client。
    async fn shutdown_where<F: Fn(&PoolKey, &Entry) -> bool>(&self, pred: F) {
        let victims = {
            let mut entries = self.inner.entries.lock().unwrap();
            let keys: Vec<PoolKey> = entries
                .iter()
                .filter(|(k, e)| pred(k, e))
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .filter_map(|k| entries.remove(&k))
                .filter_map(|e| e.client)
                .collect::<Vec<_>>()
        };
        for client in victims {
            client.shutdown().await;
        }
    }

    /// 当前条目快照（诊断/测试用）。
    pub fn states(&self) -> Vec<(String, Lang, EntryState)> {
        self.inner
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|(k, e)| (k.root.to_string_lossy().to_string(), k.lang, e.state))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolution_to_missing_program() -> ServerResolution {
        ServerResolution {
            found: true,
            source: "config".into(),
            program: PathBuf::from("definitely-not-a-real-lsp-binary-xyz"),
            args: vec![],
            version: None,
            detail: String::new(),
            install: None,
        }
    }

    #[tokio::test]
    async fn acquire_starts_then_goes_unavailable_after_one_restart() {
        let pool = LspPool::new();
        let res = resolution_to_missing_program();
        let cfg = LspSettings::default();
        let root = PathBuf::from(".");

        // 第一次：插入 Starting
        let e1 = pool.acquire(Lang::Rust, &root, &root, &res, &cfg).await;
        assert_eq!(e1.err(), Some(AcquireError::Starting));
        // 背景任务会 spawn 失败 → Dead
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let got = pool.acquire(Lang::Rust, &root, &root, &res, &cfg).await;
            match got {
                Err(AcquireError::Starting) => continue,
                Err(AcquireError::Unavailable(msg)) => {
                    assert!(!msg.is_empty());
                    return;
                }
                Ok(_) => panic!("不存在的程序不可能成功启动"),
            }
        }
        panic!("未在预期步数内进入 Unavailable");
    }

    #[tokio::test]
    async fn idle_eviction_removes_idle_entries() {
        let pool = LspPool::new();
        let res = resolution_to_missing_program();
        let cfg = LspSettings::default();
        let root = PathBuf::from(".");
        let _ = pool.acquire(Lang::Rust, &root, &root, &res, &cfg).await;
        // 等背景任务把 Starting 推进到终态（程序不存在 → spawn 失败 → Dead）
        for _ in 0..100 {
            if pool
                .states()
                .iter()
                .all(|(_, _, s)| *s != EntryState::Starting)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(pool.len(), 1);
        pool.evict_idle(Duration::from_millis(0)).await;
        assert_eq!(
            pool.len(),
            0,
            "闲置回收必须清空条目（Starting 不回收，已在上面等完）"
        );
    }
}
