use crate::core::sessions::repair;
use crate::core::types::Message;
use crate::util::atomic::atomic_write;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

/// 会话索引条目上限（超过此数按 LRU 淘汰最旧条目；gz 保留可再发现）。
pub const MAX_INDEX_ENTRIES: usize = 2000;
/// 单会话历史 gzip 后字节上限（8MB 压缩后封顶）。
pub const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
/// 保存/加载时 trim 的 token 预算。
pub const TRIM_BUDGET_TOKENS: u64 = 256 * 1024;

/// 会话索引元数据条目（index.json 的一行；左栏会话列表的直接数据源）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// 会话 id
    pub id: String,
    /// 标题（自动命名/手动改名更新）
    pub title: String,
    /// 主工作区目录快照
    pub workspace: String,
    /// 检查点时刻的会话生效模型（可能为 None）
    #[serde(default)]
    pub model_id: Option<String>,
    /// 创建时间（upsert 时保留首见值，M7）
    pub created_at: String,
    /// 最近更新时间（列表排序依据）
    pub updated_at: String,
    /// 消息条数
    #[serde(default)]
    pub message_count: usize,
    /// 所属项目（None = 自由会话，仅主目录）
    #[serde(default)]
    pub project_id: Option<String>,
    /// 创建时快照的全部可读写根（含主目录；跨根解析 / @提及 / 文件树的唯一数据源）
    #[serde(default)]
    pub roots: Vec<String>,
    /// 是否有 run 在本进程内运行中（批1）：run 开始置位、收尾清除；进程非正常退出遗留的 true
    /// 会在下次启动时转成 `interrupted { kind: "crash" }`。serde default 向前兼容（旧索引无此字段）
    #[serde(default)]
    pub running: bool,
    /// 上次非正常收尾的中断标记（None = 无）；前端「已读/续跑」后经 clear_session_interrupt 清除
    #[serde(default)]
    pub interrupted: Option<InterruptInfo>,
    /// 最近打开时间（RFC3339；None = 本版本尚未打开过）。加载会话时刷新（10 分钟节流），
    /// 参与会话清理的「最近活动时间」判定 = max(updated_at, last_opened_at)；
    /// 列表排序与行内时间仍用 updated_at（点开会话不会被顶到列表最前）。
    #[serde(default)]
    pub last_opened_at: Option<String>,
}

/// 中断标记（批1，需求共识 20/23）：进程被强杀或用户中断退出时留下的痕迹，供前端展示与续跑提示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptInfo {
    /// 中断类型：`crash`（崩溃/强杀）| `quit`（退出时中断）
    pub kind: String,
    /// 中断时刻（RFC3339）
    pub at: String,
}

/// 会话索引文件（sessions/index.json）的整体形态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndex {
    /// schema 版本（当前恒 1）
    pub version: u32,
    /// 元数据列表（LRU 上限内）
    pub sessions: Vec<SessionMeta>,
}

/// 会话产物登记（[docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)）：create/edit 工具成功写入文件的归属清单
/// （逻辑所有权；文件留在原地）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArtifact {
    /// 绝对路径（resolve 之后）
    pub path: String,
    /// 首次登记时的操作
    pub first_op: ArtifactOp,
    /// 最近一次操作
    pub last_op: ArtifactOp,
    /// 首次登记时间（RFC3339）
    pub first_at: String,
    /// 最近操作时间（RFC3339）
    pub last_at: String,
    /// 累计写入次数
    pub count: u32,
    /// 产物种类（[docs/session-cleanup](../../../../docs/session-cleanup.md)）：普通产物只登记不删，
    /// 计划文件（ask 落盘的中间产物）随会话一并清理且不进右栏「文件」列表；
    /// serde default 向前兼容（旧边车无此字段 → `file`）
    #[serde(default)]
    pub kind: ArtifactKind,
}

/// 产物种类（wire 形态固定小写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// 普通产物（create/edit 工具写入的用户文件）
    #[default]
    #[serde(rename = "file")]
    File,
    /// 计划文件（ask 落盘到 `<工作区>/.codewave/tasks/plan-*.md`）
    #[serde(rename = "plan")]
    Plan,
}

/// 产物操作类型（首记操作区分 create/edit；wire 形态固定小写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactOp {
    #[serde(rename = "create")]
    Create,
    #[serde(rename = "edit")]
    Edit,
}

impl Default for SessionIndex {
    fn default() -> Self {
        SessionIndex {
            version: 1,
            sessions: Vec::new(),
        }
    }
}

/// 会话持久化存储：历史（gzip）+ 索引（JSON）+ 边车（todos / 产物 / 子代理历史）。
/// 索引与产物是读-改-写全程持锁，并发检查点/删除/改名不得互相覆盖丢条目。
pub struct SessionStore {
    /// 数据根目录（~/.codewave 或测试临时目录）
    root: PathBuf,
    /// M8：索引读-改-写互斥（并发 checkpoint/delete/rename 不得丢条目）
    index_lock: std::sync::Mutex<()>,
    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：产物边车读-改-写互斥（并发工具写不得丢条目）
    artifacts_lock: std::sync::Mutex<()>,
    /// 进程内「运行中」会话 id 集合：索引 `running` 字段的唯一事实源。
    /// 新建会话首次 run 尚未检查点时索引里没有条目，标记先记在这里，待其首次 upsert 时带上
    ///（否则该 run 崩溃后将无从标记）。进程消亡即消失，恢复由 running.marker 机制接管。
    running: std::sync::Mutex<std::collections::HashSet<String>>,
    /// 测试用：索引写次数（会话清理「整批只写一次索引」回归断言的观察点）。
    /// 仅测试构建存在，不参与序列化也不影响生产路径。
    #[cfg(test)]
    index_writes: std::sync::atomic::AtomicUsize,
}

impl SessionStore {
    /// 以给定数据根构造存储（不立即建目录；写路径各自兜底）。
    pub fn new(data_root: PathBuf) -> Self {
        SessionStore {
            root: data_root,
            index_lock: std::sync::Mutex::new(()),
            artifacts_lock: std::sync::Mutex::new(()),
            running: std::sync::Mutex::new(std::collections::HashSet::new()),
            #[cfg(test)]
            index_writes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 测试用：本 store 已写索引的次数。
    #[cfg(test)]
    pub fn index_write_count(&self) -> usize {
        self.index_writes.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 索引文件路径：sessions/index.json。
    fn index_path(&self) -> PathBuf {
        self.sessions_dir().join("index.json")
    }

    /// 损坏索引的备份路径：sessions/index.json.corrupt。
    ///
    /// 由 `load_index()` 在解析失败时改名保全（M8：先备份再回默认，否则下次写会静默覆写），
    /// 本身**不自动清理**——它同时是「索引曾损坏且未复原」这个持久信号，`index_is_trusted()` 会读它。
    pub(crate) fn corrupt_index_backup_path(&self) -> PathBuf {
        self.sessions_dir().join("index.json.corrupt")
    }

    /// 历史目录：<数据根>/histories（清理扫描索引外残留用）。
    pub(crate) fn histories_dir(&self) -> PathBuf {
        self.root.join("histories")
    }

    /// 会话边车/索引目录：<数据根>/sessions（清理扫描索引外残留用）。
    pub(crate) fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// 会话历史文件路径：histories/<id>.json.gz（清理按同一路径删除）。
    pub(crate) fn history_path(&self, id: &str) -> PathBuf {
        self.histories_dir().join(format!("{id}.json.gz"))
    }

    /// 读索引；缺失回默认；损坏先备份为 index.json.corrupt 保全证据再回默认（M8）。
    pub fn load_index(&self) -> SessionIndex {
        let bytes = match std::fs::read(self.index_path()) {
            Ok(b) => b,
            Err(_) => return SessionIndex::default(),
        };
        match serde_json::from_slice(&bytes) {
            Ok(idx) => idx,
            Err(e) => {
                // M8：损坏索引先备份保全证据，再回默认（否则下次写会静默覆写）
                let backup = self.corrupt_index_backup_path();
                let _ = std::fs::rename(self.index_path(), &backup);
                tracing::warn!("会话索引损坏，已备份到 {}：{e}", backup.display());
                SessionIndex::default()
            }
        }
    }

    /// 索引是否**可信**：`sessions/index.json` 存在、能成功解析，且**没有遗留的损坏备份**。
    ///
    /// 三种「不可信」：① 文件缺失（全新安装、被手工删掉）；② 解析失败（损坏）；
    /// ③ `sessions/index.json.corrupt` 存在——索引曾损坏且尚未复原。此时 `load_index()`
    /// 会返回**空索引**，而空索引与「用户真的没有会话」在返回值上无法区分——按空索引去清
    /// 「索引外残留」会把全部历史文件当孤儿删掉，所以清理路径必须先问这个方法
    ///（[docs/session-cleanup](../../../../docs/session-cleanup.md)：宁可不删也不误删）。
    ///
    /// 为什么 ③ 必须单独成立：真实启动序列里，损坏索引被 `load_index()` 改名成 `.corrupt` 之后，
    /// 紧接着的既有写路径（启动清扫 `purge_non_session_entries()` 走 `mutate_index`）**必定**把
    /// 一份合法的空索引写回同一路径。等到启动清理跑 `execute()` 时，「文件存在且能解析」已经恢复为真，
    /// 于是守卫被同一轮启动序列顶掉，一份空索引就把用户的旧历史与边车当孤儿删光（判据还被偷换成
    /// 文件修改时间，近期打开过、很久没写入的会话照样中招，而启动路径没有确认框可问）。
    /// 所以「曾损坏」不能只看当下这一份文件，必须是**持久信号**：只要 `.corrupt` 还在，就不扫残留。
    ///
    /// 取舍（有意为之）：`.corrupt` 是保全证据、不被自动删除，因此索引损坏过的用户会长期停用
    /// 「索引外残留」清理——代价只是陈旧孤儿文件不被回收，绝不误删任何东西；索引从未损坏的用户
    /// 完全不受影响（判据 ③ 恒为假，与加这道条件之前的行为一致）。
    /// 只读，不做任何修复动作（损坏备份仍在 `load_index` 里）。
    pub fn index_is_trusted(&self) -> bool {
        let backup = self.corrupt_index_backup_path();
        if backup.exists() {
            tracing::warn!(
                "会话索引曾损坏且未复原（{} 存在），本次不扫索引外残留：宁可不删也不误删",
                backup.display()
            );
            return false;
        }
        match std::fs::read(self.index_path()) {
            Ok(bytes) => serde_json::from_slice::<SessionIndex>(&bytes).is_ok(),
            Err(_) => false,
        }
    }

    /// 保存索引（超过条目上限先按 updated_at LRU 淘汰），原子写。
    pub fn save_index(&self, idx: &SessionIndex) -> anyhow::Result<()> {
        // LRU：超过上限淘汰最旧条目（gz 保留，可再发现）
        let mut idx = idx.clone();
        if idx.sessions.len() > MAX_INDEX_ENTRIES {
            idx.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            idx.sessions.truncate(MAX_INDEX_ENTRIES);
        }
        let bytes = serde_json::to_vec_pretty(&idx)?;
        atomic_write(&self.index_path(), &bytes)?;
        #[cfg(test)]
        self.index_writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// M8：索引读-改-写全程持锁，并发检查点/删除不得互相覆盖。
    fn mutate_index<T>(&self, f: impl FnOnce(&mut SessionIndex) -> T) -> anyhow::Result<T> {
        let _guard = self.index_lock.lock().unwrap();
        let mut idx = self.load_index();
        let out = f(&mut idx);
        self.save_index(&idx)?;
        Ok(out)
    }

    /// 插入或更新一条会话元数据（持锁）。
    ///
    /// 批1：`running` / `interrupted` 是会话的易失标记，由 `mark_running` / `mark_interrupted` 专管——
    /// 检查点只刷新内容与元数据，不得把标记抹掉：既有条目沿用索引现值，新条目按内存 running 集合
    /// 初始化（传入值不参与裁决）。
    pub fn upsert_meta(&self, meta: SessionMeta) -> anyhow::Result<()> {
        // 先取内存集合再进索引锁路径（锁序恒为 index → running 以外的方向，避免与 mark_* 互死锁）
        let is_running = self.running.lock().unwrap().contains(&meta.id);
        self.mutate_index(|idx| {
            // M7 修复：保留原 created_at（此前每次检查点都会重置创建时间）
            let existing = idx.sessions.iter().find(|s| s.id == meta.id);
            let created_at = existing
                .map(|s| s.created_at.clone())
                .unwrap_or_else(|| meta.created_at.clone());
            let running = existing.map(|s| s.running).unwrap_or(is_running);
            let interrupted = existing.and_then(|s| s.interrupted.clone());
            // [docs/session-cleanup](../../../../docs/session-cleanup.md)：检查点不得抹掉已落盘的「最近打开时间」
            //（与 created_at 同一处理：既有条目沿用索引现值，新条目取传入值）
            let last_opened_at = existing
                .and_then(|s| s.last_opened_at.clone())
                .or_else(|| meta.last_opened_at.clone());
            idx.sessions.retain(|s| s.id != meta.id);
            let mut meta = meta.clone();
            meta.created_at = created_at;
            meta.running = running;
            meta.interrupted = interrupted;
            meta.last_opened_at = last_opened_at;
            idx.sessions.push(meta);
        })
    }

    /// M8：仅改索引中的标题（会话不在内存时的改名路径），持锁。
    pub fn rename_in_index(&self, id: &str, title: &str) -> anyhow::Result<bool> {
        self.mutate_index(|idx| match idx.sessions.iter_mut().find(|m| m.id == id) {
            Some(m) => {
                m.title = title.to_string();
                true
            }
            None => false,
        })
    }

    // ---------- 运行 / 中断标记（会话保存与恢复优化 · 批1，需求共识 20/23） ----------

    /// run 开始（`true`）/ 收尾（`false`）时落盘 `running` 标志。
    /// 索引中尚无该会话（新建会话首次 run 尚未检查点）时只记内存集合，待其首次 upsert 时带上。
    /// 返回索引中是否已有该条目（调用方据此判断标记是否已落盘）。
    pub fn mark_running(&self, id: &str, running: bool) -> anyhow::Result<bool> {
        {
            let mut set = self.running.lock().unwrap();
            if running {
                set.insert(id.to_string());
            } else {
                set.remove(id);
            }
        }
        let mut found = false;
        self.mutate_index(|idx| {
            if let Some(m) = idx.sessions.iter_mut().find(|m| m.id == id) {
                m.running = running;
                found = true;
            }
        })?;
        Ok(found)
    }

    /// 写中断标记（`kind` = crash / quit，`at` 为 RFC3339；不动 `running`）。
    /// 返回索引中是否命中该会话。
    pub fn mark_interrupted(&self, id: &str, kind: &str, at: &str) -> anyhow::Result<bool> {
        let mut found = false;
        self.mutate_index(|idx| {
            if let Some(m) = idx.sessions.iter_mut().find(|m| m.id == id) {
                m.interrupted = Some(InterruptInfo {
                    kind: kind.to_string(),
                    at: at.to_string(),
                });
                found = true;
            }
        })?;
        Ok(found)
    }

    /// 清中断标记（前端「已读/续跑」后调用；幂等）。返回索引中是否命中该会话。
    pub fn clear_interrupted(&self, id: &str) -> anyhow::Result<bool> {
        let mut found = false;
        self.mutate_index(|idx| {
            if let Some(m) = idx.sessions.iter_mut().find(|m| m.id == id) {
                m.interrupted = None;
                found = true;
            }
        })?;
        Ok(found)
    }

    /// 批量中断收尾（崩溃恢复 / 退出中断）：一次索引写把给定会话标 `interrupted` 并清 `running`，
    /// 缺席会话（子代理/任务运行、已删除）跳过；返回命中条数。
    /// 收尾路径无法再向上抛错，写失败按 E9 告警并返回 0（不假装成功）。
    pub fn mark_interrupted_batch(&self, ids: &[String], kind: &str, at: &str) -> usize {
        if ids.is_empty() {
            return 0;
        }
        {
            let mut set = self.running.lock().unwrap();
            for id in ids {
                set.remove(id);
            }
        }
        let mut n = 0usize;
        let result = self.mutate_index(|idx| {
            for m in idx.sessions.iter_mut() {
                if ids.iter().any(|id| id == &m.id) {
                    m.interrupted = Some(InterruptInfo {
                        kind: kind.to_string(),
                        at: at.to_string(),
                    });
                    m.running = false;
                    n += 1;
                }
            }
        });
        match result {
            Ok(()) => n,
            Err(e) => {
                tracing::warn!("中断标记落盘失败（{kind}）：{e}");
                0
            }
        }
    }

    /// 索引中标记为 `running` 的会话 id（崩溃恢复用；进程内实时集合见 host 的 list_running_sessions）。
    pub fn running_ids(&self) -> Vec<String> {
        self.load_index()
            .sessions
            .into_iter()
            .filter(|m| m.running)
            .map(|m| m.id)
            .collect()
    }

    /// 列出全部会话元数据（按 updated_at 倒序）。
    pub fn list(&self) -> Vec<SessionMeta> {
        let mut v = self.load_index().sessions;
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    /// 不在索引但磁盘上仍有 gz 的会话（LRU 淘汰后仍可再发现）。
    pub fn discover_orphans(&self) -> Vec<String> {
        let known: std::collections::HashSet<String> = self
            .load_index()
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let dir = self.root.join("histories");
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(id) = name.strip_suffix(".json.gz") {
                    if !known.contains(id) {
                        out.push(id.to_string());
                    }
                }
            }
        }
        out
    }

    /// 清理索引中的非会话条目（sub_*/task_* 前缀）及其 gz 与 todos/artifacts 边车。
    /// 存量修复：checkpoint 曾把子代理/任务运行 upsert 进主索引（untitled 幽灵会话），
    /// 本方法在应用启动时一次性清扫；真会话 id 为 uuid，不与两类前缀冲突。幂等。
    pub fn purge_non_session_entries(&self) -> usize {
        let mut removed_ids: Vec<String> = Vec::new();
        let _ = self.mutate_index(|idx| {
            let mut kept = Vec::with_capacity(idx.sessions.len());
            for s in idx.sessions.drain(..) {
                if s.id.starts_with("sub_") || s.id.starts_with("task_") {
                    removed_ids.push(s.id);
                } else {
                    kept.push(s);
                }
            }
            idx.sessions = kept;
        });
        for id in &removed_ids {
            let _ = std::fs::remove_file(self.history_path(id));
            let _ = std::fs::remove_file(self.todos_path(id));
            let _ = std::fs::remove_file(self.artifacts_path(id));
        }
        let n = removed_ids.len();
        if n > 0 {
            tracing::info!("已清理 {n} 条非会话索引条目（sub_*/task_* 幽灵会话存量修复）");
        }
        n
    }

    /// 按 id 查会话元数据。
    pub fn get(&self, id: &str) -> Option<SessionMeta> {
        self.load_index().sessions.into_iter().find(|s| s.id == id)
    }

    /// 删除会话：索引条目、历史文件与全部边车。
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.mutate_index(|idx| idx.sessions.retain(|s| s.id != id))?;
        let _ = std::fs::remove_file(self.history_path(id));
        // 边车级联清理（todos 曾泄漏；此处一并修复）
        let _ = std::fs::remove_file(self.artifacts_path(id));
        let _ = std::fs::remove_file(self.todos_path(id));
        // 子代理过程历史目录级联清理（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)）
        let _ = std::fs::remove_dir_all(self.sub_histories_dir(id));
        Ok(())
    }

    /// sanitize → trim → repair → gzip 落盘 + 索引更新。
    /// 图片 payload 保留在历史中（重开后附件仍显示）；超 8MB 上限时优雅降级
    /// 为「剥图 + 保留思考」后再存（[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）：
    /// 回退只为把转录压进上限，而思考块恰是 OpenAI 兼容 thinking 上游回传
    /// `reasoning_content` 的唯一数据源——回退路径若顺手丢掉思考，该会话重启后每次
    /// 多轮对话都必然 400 且不可自愈，因此回退只剥图片（`sanitize_keep_thinking`），
    /// 绝不丢思考。剥图后仍超限才拒绝保存。
    #[allow(clippy::too_many_arguments)]
    pub fn save_history(
        &self,
        id: &str,
        title: &str,
        workspace: &str,
        model_id: Option<&str>,
        project_id: Option<&str>,
        roots: &[String],
        msgs: &[Message],
    ) -> anyhow::Result<usize> {
        let mut prepared = repair::prepare_for_save(msgs.to_vec());
        repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 2);
        let gz = gzip_history(&prepared)?;
        // 大图片历史可能顶到上限：剥离图片 payload 但**保留思考块**后重试
        // （附件退化为占位文本，会话仍可保存；思考是回传 reasoning_content 的数据源，
        // 见函数文档注释）。
        let gz = if gz.len() > MAX_HISTORY_BYTES {
            let stripped_images = repair::sanitize_keep_thinking(&mut prepared);
            let gz2 = gzip_history(&prepared)?;
            if gz2.len() > MAX_HISTORY_BYTES {
                anyhow::bail!("历史超过 8MB 上限，请新开会话");
            }
            tracing::warn!(
                "会话 {id} 历史含图片超上限，已剥离图片 payload（保留思考）保存：{stripped_images:?}"
            );
            gz2
        } else {
            gz
        };
        atomic_write(&self.history_path(id), &gz)?;

        let now = Utc::now().to_rfc3339();
        self.upsert_meta(SessionMeta {
            id: id.to_string(),
            title: title.to_string(),
            workspace: workspace.to_string(),
            model_id: model_id.map(|s| s.to_string()),
            created_at: now.clone(),
            updated_at: now,
            message_count: prepared.len(),
            project_id: project_id.map(|s| s.to_string()),
            roots: roots.to_vec(),
            // running / interrupted 由 mark_running / mark_interrupted 专管，
            // upsert_meta 以索引现值（新条目取内存 running 集合）为准，此处仅占位
            running: false,
            interrupted: None,
            // 由 touch_session_open 独占维护，upsert_meta 以索引现值为准，此处仅占位
            last_opened_at: None,
        })?;
        Ok(prepared.len())
    }

    /// gunzip → repair → trim。文件损坏返回 Err（调用方隔离该会话）。
    pub fn load_history(&self, id: &str) -> anyhow::Result<Vec<Message>> {
        let path = self.history_path(id);
        let raw = std::fs::read(&path)?;
        let mut dec = flate2::read::GzDecoder::new(&raw[..]);
        let mut json = Vec::new();
        dec.read_to_end(&mut json)
            .map_err(|e| anyhow::anyhow!("历史文件损坏（{id}）：{e}"))?;
        let mut msgs: Vec<Message> = serde_json::from_slice(&json)
            .map_err(|e| anyhow::anyhow!("历史 JSON 解析失败（{id}）：{e}"))?;
        repair::repair(&mut msgs);
        repair::trim(&mut msgs, TRIM_BUDGET_TOKENS, 2);
        Ok(msgs)
    }

    // ---------- 子代理过程历史（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)） ----------
    //
    // 与主历史（save_history）的区别：不 upsert 会话索引（子代理不是会话，不得出现在
    // list_sessions）；目录按父会话分桶，随父会话级联删除。

    /// 子代理历史目录：histories/subs/<parent>/（清理按同一路径级联删除）。
    pub(crate) fn sub_histories_dir(&self, parent: &str) -> PathBuf {
        self.histories_dir().join("subs").join(parent)
    }

    /// 子代理历史文件路径：histories/subs/<parent>/<sub>.json.gz。
    fn sub_history_path(&self, parent: &str, sub: &str) -> PathBuf {
        self.sub_histories_dir(parent)
            .join(format!("{sub}.json.gz"))
    }

    /// 持久化子代理的完整执行历史（drive_agent 结束/异常路径调用）。
    /// 复用主历史的 sanitize/trim/gzip 管线，但绝不触碰索引。
    pub fn save_sub_history(
        &self,
        parent: &str,
        sub: &str,
        msgs: &[Message],
    ) -> anyhow::Result<()> {
        let mut prepared = repair::prepare_for_save(msgs.to_vec());
        repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 2);
        let gz = gzip_history(&prepared)?;
        let path = self.sub_history_path(parent, sub);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        atomic_write(&path, &gz)?;
        Ok(())
    }

    /// 读取子代理过程历史；文件缺失返回空（旧会话没有；前端优雅降级）。
    pub fn load_sub_history(&self, parent: &str, sub: &str) -> anyhow::Result<Vec<Message>> {
        let path = self.sub_history_path(parent, sub);
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut dec = flate2::read::GzDecoder::new(&raw[..]);
        let mut json = Vec::new();
        dec.read_to_end(&mut json)
            .map_err(|e| anyhow::anyhow!("子代理历史损坏（{parent}/{sub}）：{e}"))?;
        let mut msgs: Vec<Message> = serde_json::from_slice(&json)
            .map_err(|e| anyhow::anyhow!("子代理历史 JSON 解析失败（{parent}/{sub}）：{e}"))?;
        repair::repair(&mut msgs);
        Ok(msgs)
    }

    // ---------- 计划 todos 边车 ----------

    /// todos 边车文件路径：sessions/<id>.todos.json（清理按同一路径删除）。
    pub(crate) fn todos_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.todos.json"))
    }

    /// 持久化计划 todos（原子写）。
    pub fn save_todos(&self, id: &str, todos: &[crate::tools::plan::Todo]) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(todos)?;
        atomic_write(&self.todos_path(id), &bytes)?;
        Ok(())
    }

    /// 读 todos；缺失/损坏回空列表（边车只是辅助视图）。
    pub fn load_todos(&self, id: &str) -> Vec<crate::tools::plan::Todo> {
        std::fs::read(self.todos_path(id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    // ---------- 会话产物登记边车（[docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)） ----------

    /// 产物边车文件路径：sessions/<id>.artifacts.json（清理按同一路径删除）。
    pub(crate) fn artifacts_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.artifacts.json"))
    }

    /// 登记一次写入（普通产物；[docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)）：
    /// 按路径去重/合并（首记 create/edit；重复则累加 count 并刷新 last_*）。
    pub fn append_artifact(&self, id: &str, path: &str, op: ArtifactOp) -> anyhow::Result<()> {
        self.append_artifact_kind(id, path, op, ArtifactKind::File)
    }

    /// 带种类的登记入口（[docs/session-cleanup](../../../../docs/session-cleanup.md)）：计划文件传 `ArtifactKind::Plan`。
    /// 合并时 `Plan` 是粘性的（已有 Plan 条目不会被普通产物的登记降级为 `file`）。
    pub fn append_artifact_kind(
        &self,
        id: &str,
        path: &str,
        op: ArtifactOp,
        kind: ArtifactKind,
    ) -> anyhow::Result<()> {
        let _guard = self.artifacts_lock.lock().unwrap();
        let mut items = self.load_artifacts(id);
        let now = Utc::now().to_rfc3339();
        if let Some(a) = items.iter_mut().find(|a| a.path == path) {
            a.last_op = op;
            a.last_at = now;
            a.count = a.count.saturating_add(1);
            if kind == ArtifactKind::Plan {
                a.kind = ArtifactKind::Plan;
            }
        } else {
            items.push(SessionArtifact {
                path: path.to_string(),
                first_op: op,
                last_op: op,
                first_at: now.clone(),
                last_at: now,
                count: 1,
                kind,
            });
        }
        let bytes = serde_json::to_vec_pretty(&items)?;
        atomic_write(&self.artifacts_path(id), &bytes)?;
        Ok(())
    }

    /// 读产物列表（缺失/损坏回空：登记表只是辅助视图，损坏绝不阻断会话）。
    pub fn load_artifacts(&self, id: &str) -> Vec<SessionArtifact> {
        std::fs::read(self.artifacts_path(id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// 读**普通产物**列表：右栏「文件」面板的数据源（计划文件不进该列表，[docs/session-cleanup](../../../../docs/session-cleanup.md)）。
    pub fn load_file_artifacts(&self, id: &str) -> Vec<SessionArtifact> {
        self.load_artifacts(id)
            .into_iter()
            .filter(|a| a.kind == ArtifactKind::File)
            .collect()
    }

    // ---------- 会话保留期清理的存储侧支持（[docs/session-cleanup](../../../../docs/session-cleanup.md)） ----------

    /// 进程内运行中会话 id 的快照（清理跳过用；索引标记另由 `SessionMeta::running` 给出）。
    pub fn running_in_memory(&self) -> std::collections::HashSet<String> {
        self.running.lock().unwrap().clone()
    }

    /// 刷新「最近打开时间」（加载会话时调用）：持索引锁；距上次刷新不足节流窗口时**不写**
    ///（返回 false）；索引里没有这个会话则什么都不做（返回 false，不凭空造条目）。
    pub fn touch_session_open(
        &self,
        id: &str,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let Some(meta) = self.get(id) else {
            return Ok(false);
        };
        if !needs_open_touch(meta.last_opened_at.as_deref(), now) {
            return Ok(false);
        }
        let at = now.to_rfc3339();
        let mut hit = false;
        self.mutate_index(|idx| {
            if let Some(m) = idx.sessions.iter_mut().find(|m| m.id == id) {
                m.last_opened_at = Some(at.clone());
                hit = true;
            }
        })?;
        Ok(hit)
    }

    /// 批量删除索引行（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3-17）：整批只写**一次**索引，
    /// 返回真正被移除的 id（索引中没有的不计入）；同时把 id 从内存运行集合中移除，
    /// 否则迟到的检查点会带着 `running` 把索引行“复活”。
    ///
    /// 索引写入失败返回 `Err` 而不是吞掉：此时文件已删、索引行还在，调用方必须把受影响的
    /// 会话计入失败（下次清理会再次命中，重删文件视为成功——自愈）。
    pub fn remove_many(&self, ids: &[String]) -> anyhow::Result<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut set = self.running.lock().unwrap();
            for id in ids {
                set.remove(id);
            }
        }
        let mut removed: Vec<String> = Vec::new();
        self.mutate_index(|idx| {
            let mut kept = Vec::with_capacity(idx.sessions.len());
            for s in idx.sessions.drain(..) {
                if ids.iter().any(|id| id == &s.id) {
                    removed.push(s.id);
                } else {
                    kept.push(s);
                }
            }
            idx.sessions = kept;
        })?;
        Ok(removed)
    }
}

/// 「最近打开时间」的落盘节流窗口（秒）：同一会话 10 分钟内不重复写索引
///（每次 `load_session` 都重写整份索引代价过高；精度损失对「天」级清理判定无影响）。
pub const OPEN_TOUCH_THROTTLE_SECS: i64 = 600;

/// 纯函数：是否需要刷新「最近打开时间」（无记录 / 记录不可解析 → 需要；
/// 距上次刷新不足节流窗口（含时钟倒拨导致记录落在未来）→ 不需要）。
pub fn needs_open_touch(last_opened_at: Option<&str>, now: chrono::DateTime<Utc>) -> bool {
    match last_opened_at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        Some(prev) => (now - prev.with_timezone(&Utc)).num_seconds() >= OPEN_TOUCH_THROTTLE_SECS,
        None => true,
    }
}

/// 历史序列化 + gzip（save_history 超上限降级重试时复用）。
fn gzip_history(msgs: &[Message]) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec(msgs)?;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &json)?;
    Ok(enc.finish()?)
}

#[cfg(test)]
mod tests;
