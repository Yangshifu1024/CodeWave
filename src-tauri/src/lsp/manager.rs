//! 写后语义校验编排：写前基线 → 写后差集 → 过滤 → 回喂三态。
//!
//! 三条不可退让的语义（历史 bug 的结构性修复）：
//! 1. **`Skipped` 永不被渲染成「通过」**（未运行必须如实带原因）；
//! 2. **基线不可信 → 不回喂任何诊断**（宁可不回喂，也不把项目存量错误当本次改动的问题）；
//! 3. 只回喂**本次新增的 error 级**诊断（差集 + severity 过滤 + 指纹刹车 + 预算截断）。

use super::client::LspClient;
use super::diagnostics::{self, DedupBrake};
use super::discovery::{self, Discoverer, ServerResolution};
use super::pool::{AcquireError, LspPool};
use super::sdk;
use super::server_spec;
use super::workspace;
use super::{
    Baseline, DiagnosticItem, InstallHint, InstallKind, Lang, ServerStatus, SkipReason,
    ValidateRequest, ValidationOutcome,
};
use crate::core::config::{LspSettings, ValidationSettings};
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// 同步窗口内的轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 闲置回收 ticker 周期。
pub const TICKER_INTERVAL: Duration = Duration::from_secs(30);

/// 异步补条时等待 server 就绪的预算。
pub const ASYNC_READY_BUDGET: Duration = Duration::from_secs(60);

/// 异步补条时等待诊断的窗口（远大于同步窗口——「晚到」正是本方法的用武之地）。
pub const ASYNC_DIAG_WINDOW: Duration = Duration::from_secs(30);

/// 写后语义校验管理器（pool + 刹车 + ticker）。
pub struct LspManager {
    pool: LspPool,
    discoverer: RwLock<Arc<Discoverer>>,
    brake: Mutex<DedupBrake>,
    /// 语言开关快照（`new()` 起为默认值；由 `status`/`set_settings` 刷新）。
    ///
    /// 冻结签名只把 `LspSettings` 交给 `validate`，语言开关在它的父结构
    /// `ValidationSettings` 里，故这里保留一份快照（`java` 默认关闭的语义因此天然成立）。
    settings: Mutex<ValidationSettings>,
    idle_ttl_ms: Arc<AtomicU64>,
    ticker_started: AtomicBool,
}

impl Default for LspManager {
    fn default() -> Self {
        LspManager::new()
    }
}

impl LspManager {
    /// 新建（不启动任何进程；ticker 在首次使用时惰性启动）。
    pub fn new() -> Self {
        let discoverer = Arc::new(Discoverer::new());
        let pool = LspPool::new();
        pool.set_env(pool_env(&discoverer));
        LspManager {
            pool,
            discoverer: RwLock::new(discoverer),
            brake: Mutex::new(DedupBrake::new()),
            settings: Mutex::new(ValidationSettings::default()),
            idle_ttl_ms: Arc::new(AtomicU64::new(LspSettings::default().idle_ttl_ms)),
            ticker_started: AtomicBool::new(false),
        }
    }

    /// 刷新语言开关快照（tools 层拿到配置后调用一次即可，`status` 也会顺带刷新）。
    ///
    /// 追加接口（不改冻结签名）：开关在 `ValidationSettings` 上，而 `validate` 只收到
    /// `LspSettings`，因此需要这一条投喂路径；不投喂时使用 `ValidationSettings::default()`。
    pub fn set_settings(&self, v: &ValidationSettings) {
        *self.settings.lock().unwrap() = v.clone();
    }

    /// 写前基线。server 未就绪 → `None`（调用方据此收紧为「不回喂」）。
    pub async fn baseline(&self, req: &ValidateRequest, cfg: &LspSettings) -> Option<Baseline> {
        self.tick(cfg);
        let lang = self.precheck(req, cfg).ok()?;
        let disc = self.discoverer();
        let res = discovery::resolve(lang, cfg, &req.project_root, &disc);
        if !res.found {
            tracing::debug!(lang = lang.id(), "未解析到 server，跳过基线");
            return None;
        }
        let root = workspace::root_for_file(&req.project_root, &req.path, lang);
        let client = match self
            .pool
            .acquire(lang, &root, &req.project_root, &res, cfg)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(lang = lang.id(), ?e, "基线阶段 server 未就绪");
                return None;
            }
        };
        self.pool
            .tag_project(lang, &root, req.project_id.as_deref());

        let prev = match &req.prev_content {
            Some(text) => Some(text.clone()),
            None => std::fs::read_to_string(&req.path).ok(),
        };
        let language_id = lang.language_id(&req.path);
        let before_rev = client.diagnostics_revision(&req.path);
        client
            .did_open(&req.path, prev.as_deref().unwrap_or(""), language_id)
            .await;
        if prev.is_none() {
            // 写前文件不存在 → 不存在存量错误，空基线即可信
            return Some(Baseline {
                fingerprints: Vec::new(),
                reliable: true,
            });
        }
        let window = Duration::from_millis(cfg.sync_window_ms);
        if !wait_for_new_diagnostics(&client, &req.path, before_rev, window).await {
            tracing::debug!(
                path = %req.rel_path,
                "基线诊断未在同步窗口内到达 → 标记不可信（本轮不回喂）"
            );
            return Some(Baseline {
                fingerprints: Vec::new(),
                reliable: false,
            });
        }
        // 空集 publish 本身**不足以立信**：server 冷启动常见「先推空数组、随后才推真实诊断」，
        // 把它当「文件本来就 0 错误」会让项目存量错误在写后差集里全部变成「本次新增」并回喂给模型
        // （本需求要消灭的痛点）。除「本次 didOpen 后确实收到过 publish」（上面的 wait 已保证）外，
        // 还要求**实例级就绪证据**足以支撑**这份**基线（见 `client::Readiness::ready_for`）：
        // - 强证据（rust-analyzer `quiescent` / 本实例推过**非空**诊断）→ 空集与非空基线都可信；
        // - `Warm`（预热窗已过 + 推过 ≥2 次）→ **只给非空基线背书**。它只证明「时间够久且出过声」，
        //   而出声内容可能全是占位空集（全干净项目连推 N 次空集就能凑出 `Warm`），若拿它给空集
        //   背书，server 随后推的项目存量错误就会被当成「本次改动引入」。
        // 代价是有意接受的：全干净项目首轮报「未就绪」而不是「通过」——宁可不说话，也不谎报通过。
        let current = client.diagnostics_for(&req.path);
        let evidence = client.readiness();
        if !evidence.ready_for(current.is_empty()) {
            // 同一 server 实例**首次**降级报一条 warn：含语言 / server 名 / 就绪证据标签。
            // 「功能哑火且查不出原因」是最难定位的故障形态，这条 warn 是它唯一的可观测点。
            // 注意：**不**改 `ServerStatus` 字段（前后端契约，只允许纯追加新字段）；后续降级降回 debug 不刷屏。
            if client.note_degraded_once() {
                tracing::warn!(
                    lang = lang.id(),
                    server = server_spec::spec(lang).server_name,
                    executable = client.label(),
                    evidence = evidence.label(),
                    empty_baseline = current.is_empty(),
                    path = %req.rel_path,
                    "LSP 基线就绪证据不足：本轮不回喂、也不补发（同一实例仅报这一次）"
                );
            }
            tracing::debug!(
                path = %req.rel_path,
                evidence = evidence.label(),
                empty_baseline = current.is_empty(),
                "基线不可信：就绪证据不足以支撑这份基线 → 本轮不回喂"
            );
            return Some(Baseline {
                fingerprints: Vec::new(),
                reliable: false,
            });
        }
        let items = diagnostics::parse_diagnostics(&current, &req.rel_path, &req.project_root);
        Some(Baseline {
            fingerprints: items.into_iter().map(|i| i.fingerprint).collect(),
            reliable: true,
        })
    }

    /// 写后诊断（同步窗口内）。超窗返回 `Skipped{ServerLoading}`，由调用方接 [`Self::validate_async`]。
    pub async fn validate(
        &self,
        req: &ValidateRequest,
        baseline: Option<&Baseline>,
        cfg: &LspSettings,
    ) -> ValidationOutcome {
        self.tick(cfg);
        let lang = match self.precheck(req, cfg) {
            Ok(l) => l,
            Err(outcome) => return outcome,
        };
        let spec = server_spec::spec(lang);
        let disc = self.discoverer();
        let res = discovery::resolve(lang, cfg, &req.project_root, &disc);
        if !res.found {
            return ValidationOutcome::Skipped {
                lang: Some(lang),
                reason: SkipReason::NoServer {
                    server: spec.server_name.to_string(),
                },
            };
        }
        let root = workspace::root_for_file(&req.project_root, &req.path, lang);
        let client = match self
            .pool
            .acquire(lang, &root, &req.project_root, &res, cfg)
            .await
        {
            Ok(c) => c,
            Err(AcquireError::Starting) => {
                return ValidationOutcome::Skipped {
                    lang: Some(lang),
                    reason: SkipReason::ServerLoading,
                };
            }
            Err(AcquireError::Unavailable(msg)) => {
                tracing::warn!(server = spec.server_name, detail = %msg, "LSP server 不可用，按未找到处理");
                return ValidationOutcome::Skipped {
                    lang: Some(lang),
                    reason: SkipReason::NoServer {
                        server: spec.server_name.to_string(),
                    },
                };
            }
        };
        self.pool
            .tag_project(lang, &root, req.project_id.as_deref());
        self.collect(
            lang,
            req,
            &client,
            baseline,
            Duration::from_millis(cfg.sync_window_ms),
            cfg,
        )
        .await
    }

    /// 异步补条：超窗后由调用方 spawn；仍无基线或拿不到结果 → `None`（**不回喂**）。
    pub async fn validate_async(
        &self,
        req: ValidateRequest,
        baseline: Option<Baseline>,
        cfg: LspSettings,
    ) -> Option<ValidationOutcome> {
        // 基线是不可信/缺失 → 宁可什么都不回喂（避免把存量错误当本次改动的问题）
        let base = baseline?;
        if !base.reliable() {
            tracing::debug!(path = %req.rel_path, "基线不可信，异步补条放弃回喂");
            return None;
        }
        self.tick(&cfg);
        let lang = self.precheck(&req, &cfg).ok()?;
        let disc = self.discoverer();
        let res = discovery::resolve(lang, &cfg, &req.project_root, &disc);
        if !res.found {
            return None;
        }
        let root = workspace::root_for_file(&req.project_root, &req.path, lang);
        let client = self
            .wait_for_client(
                lang,
                &root,
                &req.project_root,
                &res,
                &cfg,
                ASYNC_READY_BUDGET,
            )
            .await?;
        self.pool
            .tag_project(lang, &root, req.project_id.as_deref());
        let outcome = self
            .collect(lang, &req, &client, Some(&base), ASYNC_DIAG_WINDOW, &cfg)
            .await;
        match outcome {
            ValidationOutcome::Skipped { .. } => None,
            other => Some(other),
        }
    }

    /// 六语言 server 状态（设置页展示 + 缺失事件判定）。
    ///
    /// **会同步跑 `--version`**（每语言最坏 10s）——只允许显式查询（`lsp_status` / `lsp_redetect` /
    /// `lsp_install`）走；写路径必须用 [`Self::status_cached`]。探测本身丢给 `spawn_blocking`，
    /// 不占住 async 执行线程。
    pub async fn status(&self, v: &ValidationSettings) -> Vec<ServerStatus> {
        self.status_with(v, true).await
    }

    /// 同上，但**不触发版本探测**（只读已有缓存）。
    ///
    /// 写路径专用：每次写文件都可能发引导事件，若在那里同步探测版本，写入返回时间会被拖长
    /// （评审 🟡-5）。
    pub async fn status_cached(&self, v: &ValidationSettings) -> Vec<ServerStatus> {
        self.status_with(v, false).await
    }

    /// 状态查询公共实现（`probe_version` 控制是否跑版本探测）。
    async fn status_with(&self, v: &ValidationSettings, probe_version: bool) -> Vec<ServerStatus> {
        self.set_settings(v);
        let disc = self.discoverer();
        let mut out: Vec<ServerStatus> = Vec::new();
        for lang in Lang::all() {
            let spec = server_spec::spec(lang);
            let enabled = lang.enabled_in(v);
            // status 不带项目根（冻结签名），故跳过「项目内 node_modules」这一步
            let res = discovery::resolve(lang, &v.lsp, Path::new(""), &disc);
            let version = if !res.found {
                None
            } else if probe_version {
                // 版本探测是同步阻塞调用（起进程 + wait）——丢到阻塞线程池
                let disc = disc.clone();
                let program = res.program.clone();
                tokio::task::spawn_blocking(move || disc.version_of(&program))
                    .await
                    .ok()
                    .flatten()
            } else {
                disc.cached_version(&res.program)
            };
            let install = if !enabled {
                Some(InstallHint {
                    kind: InstallKind::ConfirmEnable,
                    command: None,
                    docs_url: Some(spec.install.docs_url.to_string()),
                    prerequisite: spec.install.prerequisite.clone(),
                    // 未启用态没有「一键安装」一说（动作是启用），故不探测前置命令
                    requires: None,
                })
            } else if !res.found {
                res.install.clone()
            } else {
                None
            };
            out.push(ServerStatus {
                language: lang.id().to_string(),
                enabled,
                found: res.found,
                source: res.source.clone(),
                command: res.command_line(),
                version,
                detail: res.detail.clone(),
                install,
                // SDK 探测不启进程（纯文件存在性检查），故每语言都可同步做
                sdk: sdk::probe(lang, &v.lsp, &disc),
            });
        }
        out
    }

    /// 重新探测（设置页「重新检测」）：换用新的 [`Discoverer`]（新 PATH 快照）并清掉不可用条目。
    pub async fn redetect(&self) {
        let fresh = Arc::new(Discoverer::new());
        self.pool.set_env(pool_env(&fresh));
        // 先探测一次，让 PATH 结论立刻生效（也避免下次写路径时才付探测成本）
        let _ = fresh.path();
        *self.discoverer.write().unwrap() = fresh;
        self.pool.clear_unavailable();
        self.brake.lock().unwrap().clear();
    }

    /// 重启某语言的全部实例（下一次写入会重新 spawn + initialize）。
    pub async fn restart(&self, lang: Lang) {
        self.pool.shutdown_lang(lang).await;
    }

    /// 关闭某项目的全部实例（删除项目 / 关闭项目会话时调用）。
    pub async fn shutdown_project(&self, project_id: &str) {
        self.pool.shutdown_project(project_id).await;
    }

    /// 关闭全部实例（应用退出时调用）。
    ///
    /// 追加接口（不改冻结签名）：host 层退出清理需要它。
    pub async fn shutdown_all(&self) {
        self.pool.shutdown_all().await;
    }

    /// 闲置回收一轮（host 常驻 ticker 调用）。
    ///
    /// 追加接口（不改冻结签名）：`tick()` 里那条惰性 30s ticker 只在有过校验活动后才存在，
    /// host 侧常驻 ticker 保证「长时间无写入」时也在回收（同时刷新闲置 TTL 快照）。
    pub async fn evict_idle(&self, ttl_ms: u64) {
        self.idle_ttl_ms.store(ttl_ms, Ordering::Relaxed);
        self.pool.evict_idle(Duration::from_millis(ttl_ms)).await;
    }

    /// 当前池内实例数（诊断/测试用）。
    pub fn server_count(&self) -> usize {
        self.pool.len()
    }

    /// 写后诊断的公共实现（`validate` 与 `validate_async` 只差窗口与就绪等待）。
    async fn collect(
        &self,
        lang: Lang,
        req: &ValidateRequest,
        client: &Arc<LspClient>,
        baseline: Option<&Baseline>,
        window: Duration,
        cfg: &LspSettings,
    ) -> ValidationOutcome {
        let Some(base) = baseline else {
            return ValidationOutcome::Skipped {
                lang: Some(lang),
                reason: SkipReason::ServerNotReady,
            };
        };
        if !base.reliable() {
            // 基线不可信：即使拿到新诊断也不回喂（存量错误会被误判为本次改动引入）；
            // 用 ServerNotReady 而非 ServerLoading——后者会承诺异步补发，而补发同样没有可信基线
            return ValidationOutcome::Skipped {
                lang: Some(lang),
                reason: SkipReason::ServerNotReady,
            };
        }
        let text = req
            .new_content
            .clone()
            .or_else(|| std::fs::read_to_string(&req.path).ok())
            .unwrap_or_default();
        let before_rev = client.diagnostics_revision(&req.path);
        client.did_change(&req.path, &text).await;
        if !wait_for_new_diagnostics(client, &req.path, before_rev, window).await {
            return ValidationOutcome::Skipped {
                lang: Some(lang),
                reason: SkipReason::ServerLoading,
            };
        }
        let now_items = diagnostics::parse_diagnostics(
            &client.diagnostics_for(&req.path),
            &req.rel_path,
            &req.project_root,
        );
        let now_fps: Vec<String> = now_items.iter().map(|i| i.fingerprint.clone()).collect();
        let (added, removed) = diagnostics::diff(&base.fingerprints, &now_fps);
        let added_set: HashSet<String> = added.into_iter().collect();
        let mut fresh: Vec<DiagnosticItem> = now_items
            .into_iter()
            .filter(|i| added_set.contains(&i.fingerprint))
            .collect();
        {
            let mut brake = self.brake.lock().unwrap();
            fresh.retain(|i| brake.allow(&i.path, &i.fingerprint, cfg.dedupe_limit));
        }
        if fresh.is_empty() {
            return ValidationOutcome::Passed { lang };
        }
        let limit = cfg.max_diagnostics.max(1);
        let truncated = fresh.len().saturating_sub(limit);
        fresh.truncate(limit);
        ValidationOutcome::Diagnosed {
            lang,
            items: fresh,
            removed,
            truncated,
        }
    }

    /// 前置检查（顺序固定，见 `docs/lsp-post-write-diagnostics`）。
    fn precheck(
        &self,
        req: &ValidateRequest,
        cfg: &LspSettings,
    ) -> Result<Lang, ValidationOutcome> {
        let bytes = match &req.new_content {
            Some(t) => t.len() as u64,
            None => std::fs::metadata(&req.path).map(|m| m.len()).unwrap_or(0),
        };
        if bytes > cfg.max_file_bytes {
            return Err(ValidationOutcome::Skipped {
                lang: Lang::from_path(&req.path),
                reason: SkipReason::FileTooLarge { bytes },
            });
        }
        if req.project_id.is_none() {
            return Err(ValidationOutcome::Skipped {
                lang: Lang::from_path(&req.path),
                reason: SkipReason::NoProject,
            });
        }
        let Some(lang) = Lang::from_path(&req.path) else {
            return Err(ValidationOutcome::Skipped {
                lang: None,
                reason: SkipReason::Unsupported,
            });
        };
        if lang != req.lang {
            tracing::debug!(
                path = %req.rel_path,
                request_lang = req.lang.id(),
                derived = lang.id(),
                "请求声明的语言与扩展名推导不一致，以扩展名为准"
            );
        }
        if !lang.enabled_in(&self.settings.lock().unwrap()) {
            return Err(ValidationOutcome::Skipped {
                lang: Some(lang),
                reason: SkipReason::Disabled,
            });
        }
        Ok(lang)
    }

    /// 每轮入口的簿记：刷新闲置 TTL + 惰性启动回收 ticker。
    fn tick(&self, cfg: &LspSettings) {
        self.idle_ttl_ms.store(cfg.idle_ttl_ms, Ordering::Relaxed);
        if self.ticker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let pool = self.pool.clone();
        let ttl = self.idle_ttl_ms.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(TICKER_INTERVAL).await;
                pool.evict_idle(Duration::from_millis(ttl.load(Ordering::Relaxed)))
                    .await;
            }
        });
    }

    /// 当前新鲜 PATH 探测器的快照。
    fn discoverer(&self) -> Arc<Discoverer> {
        self.discoverer.read().unwrap().clone()
    }

    /// 等 server 进入 `Ready`（最长 `budget`）。
    async fn wait_for_client(
        &self,
        lang: Lang,
        root: &Path,
        project_root: &Path,
        res: &ServerResolution,
        cfg: &LspSettings,
        budget: Duration,
    ) -> Option<Arc<LspClient>> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            match self.pool.acquire(lang, root, project_root, res, cfg).await {
                Ok(client) => return Some(client),
                Err(AcquireError::Starting) => {
                    if tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(AcquireError::Unavailable(msg)) => {
                    tracing::debug!(lang = lang.id(), detail = %msg, "异步补条：server 不可用");
                    return None;
                }
            }
        }
    }
}

/// 子进程 PATH：新鲜探测与进程快照不同才覆盖（相同就继承，别多事）。
fn pool_env(disc: &Discoverer) -> Vec<(String, String)> {
    let fresh = disc.path();
    let current = std::env::var("PATH").unwrap_or_default();
    if fresh.is_empty() || fresh == current {
        Vec::new()
    } else {
        vec![("PATH".to_string(), fresh.to_string())]
    }
}

/// 轮询等待「该文件的新诊断到达」（或 server 已死/超窗）。
async fn wait_for_new_diagnostics(
    client: &Arc<LspClient>,
    path: &Path,
    before_rev: u64,
    window: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        if client.diagnostics_revision(path) > before_rev {
            return true;
        }
        if !client.is_alive() {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let sleep = POLL_INTERVAL.min(deadline - now);
        tokio::time::sleep(sleep.max(Duration::from_millis(1))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ValidationSettings;
    use std::path::PathBuf;

    fn request(path: &str, project: Option<&str>) -> ValidateRequest {
        ValidateRequest {
            project_id: project.map(|s| s.to_string()),
            project_root: PathBuf::from("."),
            path: PathBuf::from(path),
            rel_path: path.replace('\\', "/"),
            lang: Lang::from_path(Path::new(path)).unwrap_or(Lang::Rust),
            new_content: Some("fn main() {}".to_string()),
            prev_content: Some(String::new()),
        }
    }

    #[tokio::test]
    async fn unsupported_extension_is_skipped_as_unsupported() {
        let m = LspManager::new();
        let req = request("ui/src/App.vue", Some("p1"));
        let out = m.validate(&req, None, &LspSettings::default()).await;
        match out {
            ValidationOutcome::Skipped {
                lang: None,
                reason: SkipReason::Unsupported,
            } => {}
            other => panic!("应判 Unsupported，实得 {other:?}"),
        }
        assert!(!out.ran(), "Skipped 的 ran() 必须为 false");
    }

    #[tokio::test]
    async fn temporary_session_is_skipped_without_project() {
        let m = LspManager::new();
        let req = request("src/main.rs", None);
        match m.validate(&req, None, &LspSettings::default()).await {
            ValidationOutcome::Skipped {
                reason: SkipReason::NoProject,
                ..
            } => {}
            other => panic!("临时会话应判 NoProject，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_file_wins_over_everything() {
        let m = LspManager::new();
        let mut req = request("src/main.rs", None);
        req.new_content = Some("x".repeat(2048));
        let mut cfg = LspSettings::default();
        cfg.max_file_bytes = 1024;
        match m.validate(&req, None, &cfg).await {
            ValidationOutcome::Skipped {
                reason: SkipReason::FileTooLarge { bytes },
                ..
            } => assert_eq!(bytes, 2048),
            other => panic!("应判 FileTooLarge，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn disabled_language_is_skipped() {
        let m = LspManager::new();
        let mut v = ValidationSettings::default();
        v.rust = false;
        m.set_settings(&v);
        let req = request("src/main.rs", Some("p1"));
        match m.validate(&req, None, &LspSettings::default()).await {
            ValidationOutcome::Skipped {
                lang: Some(Lang::Rust),
                reason: SkipReason::Disabled,
            } => {}
            other => panic!("应判 Disabled，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_server_is_reported_as_no_server() {
        let m = LspManager::new();
        let mut cfg = LspSettings::default();
        // 指向一个不存在的程序 → 解析结果 found=false 且 source=config
        cfg.commands.rust = "definitely-not-a-real-server-xyz".into();
        let req = request("src/main.rs", Some("p1"));
        match m.validate(&req, None, &cfg).await {
            ValidationOutcome::Skipped {
                lang: Some(Lang::Rust),
                reason: SkipReason::NoServer { server },
            } => assert_eq!(server, "rust-analyzer"),
            other => panic!("应判 NoServer，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_covers_six_languages_and_flags_disabled() {
        let m = LspManager::new();
        let v = ValidationSettings::default();
        let list = m.status(&v).await;
        assert_eq!(list.len(), 6);
        let java = list.iter().find(|s| s.language == "java").expect("java");
        assert!(!java.enabled, "Java 默认关闭");
        assert!(java.install.is_some(), "未启用时必须给引导");
        assert_eq!(
            java.install.as_ref().unwrap().kind,
            InstallKind::ConfirmEnable
        );
        let ts = list
            .iter()
            .find(|s| s.language == "typescript")
            .expect("ts");
        assert!(ts.enabled);
    }
}
