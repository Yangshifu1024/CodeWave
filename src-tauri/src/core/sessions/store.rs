use crate::core::sessions::{cleanup::is_safe_session_id, image_blobs, persist, repair};
use crate::core::types::{Content, Message, Role};
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

/// 一次历史保存的结果（供调用方上报用户）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveReport {
    /// 是否落盘成功（false = 拒存，磁盘上仍是上一次成功的历史）
    pub saved: bool,
    /// 被剥掉图片 payload 的张数（旧数据兜底路径；图片外置后正常不再触发）
    pub stripped_images: usize,
    /// 因超限被丢弃的轮数（按轮降级）
    pub dropped_rounds: usize,
    /// 落盘字节数（saved = false 时为 0）
    pub bytes: usize,
}

impl SaveReport {
    /// 干净落盘：成功且没有剥图 / 丢轮。
    pub fn is_clean(&self) -> bool {
        self.saved && self.stripped_images == 0 && self.dropped_rounds == 0
    }

    /// 拒存（磁盘上仍是上一次成功的历史）。
    pub fn rejected() -> Self {
        Self {
            saved: false,
            stripped_images: 0,
            dropped_rounds: 0,
            bytes: 0,
        }
    }
}

/// 上次历史保存的状态（None = 干净）。
///
/// **挂在索引上**是刻意的：历史写失败时索引仍能写成功——这是「重启后仍可见」的唯一载体
///（历史文件本身正是写不进去的那一个）。下次干净保存即自动清除。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryStatus {
    /// 落盘成功但有降级（剥了图 / 丢了轮）
    Degraded {
        /// 被剥掉图片 payload 的张数
        stripped_images: usize,
        /// 因超限被丢弃的轮数
        dropped_rounds: usize,
        /// 发生时刻（RFC3339）
        at: String,
    },
    /// 拒存：历史超过上限，磁盘上仍是上一次成功的历史
    Rejected {
        /// 发生时刻（RFC3339）
        at: String,
        /// 原因（用户可见文案）
        reason: String,
    },
}

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
    /// 上次历史保存状态（None = 干净；[docs/session-history-limits](../../../../docs/session-history-limits.md)）。
    /// 由 `save_history` 显式裁决、`set_history_status` 独占维护；检查点（`upsert_meta`）
    /// 沿用索引现值，绝不误清。serde default 向前兼容（旧索引无此字段）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_status: Option<HistoryStatus>,
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
    /// 历史保存互斥（[docs/session-history-limits](../../../../docs/session-history-limits.md)）：
    /// 串行化「外置 blob 写（快照）→ 写历史文件 → `upsert_meta` → `set_history_status` → `image_blobs::gc`」
    /// 整段临界区。
    ///
    /// 为什么必须有：GC 的判据是**本次保存内存快照**的引用集合，而保存路径本身不串行——
    /// A（旧快照）的 GC 若晚于 B（新快照，含新图）的历史写盘，A 就会删掉 B 仍引用的 blob，
    /// 该会话重开后那张图只剩占位文本（不可逆数据丢失）。可达路径：
    /// `host/commands/session.rs::rename_session`（不检查 running）与 run 内每 N 步的检查点并发。
    ///
    /// **锁序恒为 `save_lock → index_lock`**：临界区内会走 `upsert_meta` / `set_history_status`，
    /// 它们各自取 `index_lock`；临界区内**绝不可反向再取 `save_lock`**（会死锁）。
    /// 每次保存是毫秒级、检查点频率低，一把全局保存锁的串行化代价可接受。
    save_lock: std::sync::Mutex<()>,
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
            save_lock: std::sync::Mutex::new(()),
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
            // [docs/session-history-limits](../../../../docs/session-history-limits.md)：
            // 历史保存状态同属易失标记，由 set_history_status 专管——检查点不得把它抹掉
            let history_status = existing.and_then(|s| s.history_status.clone());
            idx.sessions.retain(|s| s.id != meta.id);
            let mut meta = meta.clone();
            meta.created_at = created_at;
            meta.running = running;
            meta.interrupted = interrupted;
            meta.last_opened_at = last_opened_at;
            meta.history_status = history_status;
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

    /// 写「上次历史保存状态」（None = 干净；[docs/session-history-limits](../../../../docs/session-history-limits.md)）。
    ///
    /// 持 `index_lock`（与 `mark_*` / `clear_interrupted` 同范式），**只改既有条目**：
    /// 索引里没有这个会话就什么都不做（不凭空造条目）。由 `save_history` 显式调用。
    ///
    /// **值未变时直接返回、不写盘**（与 `touch_session_open` 同一范式：先读现值再决定写不写）：
    /// 本方法在 `save_history` 里紧跟 `upsert_meta`，两者各走一次 `mutate_index`（= 各 `save_index`
    /// 落盘一遍 index.json），于是每个检查点都要多写一遍索引。索引是每次检查点都会重写的
    /// 派生缓存，值没变时这次写盘纯属冗余。语义不变：该清还是清、该写还是写，只跳过同值写。
    pub fn set_history_status(
        &self,
        id: &str,
        status: Option<HistoryStatus>,
    ) -> anyhow::Result<()> {
        if self.get(id).map(|m| m.history_status) == Some(status.clone()) {
            return Ok(());
        }
        self.mutate_index(|idx| {
            if let Some(m) = idx.sessions.iter_mut().find(|m| m.id == id) {
                m.history_status = status;
            }
        })
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
            // 两个按会话分桶的托管目录（工具结果 sidecar / 图片 blob）：
            // 与 cleanup 路径同口径，幽灵条目不得留下目录
            let _ = std::fs::remove_dir_all(self.tool_results_dir(id));
            let _ = std::fs::remove_dir_all(self.image_blobs_dir(id));
            // 子历史的 blob 目录（sessions/<父>__<sub>.imgblob/）：owner 带父会话前缀，
            // 上面的 image_blobs_dir(id) 覆盖不到——目录列必须先读（ghost 条目的子历史同为死数据）。
            // 编号先过白名单再拼路径（下面两处拼接全由编号拼出，脏编号绝不参与）
            if is_safe_session_id(id) {
                for sub in self.sub_history_ids(id) {
                    if is_safe_session_id(&sub) {
                        let _ = std::fs::remove_dir_all(self.sub_image_blobs_dir(id, &sub));
                    }
                }
            }
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

    /// 删除会话：索引条目、历史文件与全部边车（含两个按会话分桶的托管目录）。
    pub fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.mutate_index(|idx| idx.sessions.retain(|s| s.id != id))?;
        let _ = std::fs::remove_file(self.history_path(id));
        // 边车级联清理（todos 曾泄漏；此处一并修复）
        let _ = std::fs::remove_file(self.artifacts_path(id));
        let _ = std::fs::remove_file(self.todos_path(id));
        // 子代理过程历史目录级联清理（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)）
        // 及其图片 blob（blob 归子历史自己，不在父会话的 blob 目录里）——
        // 子历史目录列必须**先**读，下一步就把该目录整个删了
        for sub in self.sub_history_ids(id) {
            let _ = std::fs::remove_dir_all(self.sub_image_blobs_dir(id, &sub));
        }
        let _ = std::fs::remove_dir_all(self.sub_histories_dir(id));
        // 工具结果 sidecar 与图片 blob 目录：此前只删历史与边车，
        // 与 cleanup 路径口径不一致 → 这两个目录会永久泄漏
        let _ = std::fs::remove_dir_all(self.tool_results_dir(id));
        let _ = std::fs::remove_dir_all(self.image_blobs_dir(id));
        Ok(())
    }

    /// 历史保存：sanitize → trim → **图片外置** → gzip 落盘 + 索引更新。
    ///
    /// 落盘副本里的图片是 blob 引用（base64 原文在 `sessions/<id>.imgblob/`），因此历史文件
    /// 恒定在几百 KB 量级、8MB 上限几乎不可达，图片也不再被剥
    /// （[docs/session-history-limits](../../../../docs/session-history-limits.md)）。
    /// **内存与 wire 零改动**：转换只发生在落盘 DTO（[`persist`]）里。
    ///
    /// 降级阶梯（越限才逐级下行，**不再整份丢弃**）：
    /// ① 剥图重试（旧数据兜底）：`sanitize_keep_thinking` 只剥图片、**保留思考**——
    ///    思考是 OpenAI 兼容 thinking 上游回传 `reasoning_content` 的唯一数据源，
    ///    回退路径顺手丢掉思考会让该会话重启后每次多轮都必然 400 且不可自愈
    ///    （[docs/reasoning-content-passthrough](../../../../docs/reasoning-content-passthrough.md)）；
    /// ② 按轮降级：只保最后一轮（`trim(.., 1)`），丢掉的轮数记进 [`SaveReport`]；
    /// ③ 仍超 → **拒存**（`Err`，磁盘上仍是上一次成功的历史），并把
    ///    [`HistoryStatus::Rejected`] 写进索引——索引独立于历史文件，此时仍能写成功，
    ///    这是「重启后仍可见」的唯一载体。
    ///
    /// 顺序纪律：**先 trim 再外置**（被裁轮次的图片根本不写 blob）；
    /// **GC 必须在历史写成功之后**（否则历史没落盘却已删 blob = 不可逆数据丢失）；
    /// **只有历史文件写盘是致命步骤**，其后的索引写入（`upsert_meta` / `set_history_status`）
    /// 失败只告警、不影响返回值（索引是派生缓存，详见下方注释）。
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
    ) -> anyhow::Result<SaveReport> {
        // 保存串行化（见 `save_lock` 字段注释）：从**快照**（`to_persisted` 外置写 blob）起就持锁，
        // 而不是只锁「写历史文件之后」——blob 写若落在锁外，A 的 GC 仍可能删掉 B 刚写下的图。
        // 锁序恒为 save_lock → index_lock（临界区内的 upsert_meta / set_history_status 取后者）。
        let _guard = self.save_lock.lock().unwrap();
        let mut prepared = repair::prepare_for_save(msgs.to_vec());
        repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 2);
        let (mut persisted, mut referenced) = persist::to_persisted(self, id, &prepared);
        let mut gz = gzip_history(&persisted)?;
        let mut stripped_images = 0usize;
        let mut dropped_rounds = 0usize;
        if gz.len() > MAX_HISTORY_BYTES {
            // ① 旧数据兜底：剥图（占位文本）后重试——图片外置后正常不再走到这里
            let before_images = count_images(&prepared);
            repair::sanitize_keep_thinking(&mut prepared);
            stripped_images = before_images.saturating_sub(count_images(&prepared));
            (persisted, referenced) = persist::to_persisted(self, id, &prepared);
            gz = gzip_history(&persisted)?;
            if gz.len() > MAX_HISTORY_BYTES {
                // ② 按轮降级：只保最后一轮（此前是整份丢弃，用户会看到最近几轮消失）
                let before_rounds = count_rounds(&prepared);
                repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 1);
                dropped_rounds = before_rounds.saturating_sub(count_rounds(&prepared));
                (persisted, referenced) = persist::to_persisted(self, id, &prepared);
                gz = gzip_history(&persisted)?;
                if gz.len() > MAX_HISTORY_BYTES {
                    // ③ 拒存：先把状态写进索引（索引独立于历史文件，可写成功）再报错
                    self.set_history_status(
                        id,
                        Some(HistoryStatus::Rejected {
                            at: Utc::now().to_rfc3339(),
                            reason: "历史超过 8MB 上限".to_string(),
                        }),
                    )?;
                    anyhow::bail!("历史超过 8MB 上限，请新开会话");
                }
            }
            tracing::warn!(
                "会话 {id} 历史超上限，已降级保存：剥图 {stripped_images} 张、丢弃 {dropped_rounds} 轮"
            );
        }
        atomic_write(&self.history_path(id), &gz)?;

        // 其后的两步都只写**索引**（派生缓存），因此一律**非致命**：历史文件上面已经落盘，
        // 若索引写失败就让本函数返回 Err，调用方（`core/agent/drive.rs::checkpoint`）会把它映射成
        // `SaveReport::rejected()`，前端于是弹出「历史未能保存（超过 8MB 上限）」——可历史其实已经
        // 写成功了，提示既错（其实已保存）又误导（原因也不对）。索引写失败只告警，返回值照实上报。
        let now = Utc::now().to_rfc3339();
        if let Err(e) = self.upsert_meta(SessionMeta {
            id: id.to_string(),
            title: title.to_string(),
            workspace: workspace.to_string(),
            model_id: model_id.map(|s| s.to_string()),
            created_at: now.clone(),
            updated_at: now.clone(),
            message_count: prepared.len(),
            project_id: project_id.map(|s| s.to_string()),
            roots: roots.to_vec(),
            // running / interrupted 由 mark_running / mark_interrupted 专管，
            // upsert_meta 以索引现值（新条目取内存 running 集合）为准，此处仅占位
            running: false,
            interrupted: None,
            // 由 touch_session_open 独占维护，upsert_meta 以索引现值为准，此处仅占位
            last_opened_at: None,
            // 同理：由 set_history_status 独占维护，新值由下一行显式裁决
            history_status: None,
        }) {
            tracing::warn!("会话 {id} 索引元数据更新失败（历史已落盘，本次保存仍算成功）：{e}");
        }
        let status = if stripped_images == 0 && dropped_rounds == 0 {
            None
        } else {
            Some(HistoryStatus::Degraded {
                stripped_images,
                dropped_rounds,
                at: now,
            })
        };
        if let Err(e) = self.set_history_status(id, status) {
            tracing::warn!("会话 {id} 历史状态写入索引失败（历史已落盘，本次保存仍算成功）：{e}");
        }
        // 只在历史写成功之后回收：本会话目录内、本次引用之外的 blob 才是真孤儿
        image_blobs::gc(self, id, &referenced);
        Ok(SaveReport {
            saved: true,
            stripped_images,
            dropped_rounds,
            bytes: gz.len(),
        })
    }

    /// gunzip → 落盘形态 → 读回 base64 → repair → trim。
    ///
    /// 文件损坏返回 Err（调用方隔离该会话）；**blob 缺失只降级为占位文本**
    ///（不报错、不 panic、不阻断会话加载，[docs/session-restore-fidelity](../../../../docs/session-restore-fidelity.md)）。
    /// 旧数据的内联图片（tag 为 `image`）照常可读，无不可逆迁移。
    pub fn load_history(&self, id: &str) -> anyhow::Result<Vec<Message>> {
        let path = self.history_path(id);
        let raw = std::fs::read(&path)?;
        let mut dec = flate2::read::GzDecoder::new(&raw[..]);
        let mut json = Vec::new();
        dec.read_to_end(&mut json)
            .map_err(|e| anyhow::anyhow!("历史文件损坏（{id}）：{e}"))?;
        let persisted: Vec<persist::PersistedMessage> = serde_json::from_slice(&json)
            .map_err(|e| anyhow::anyhow!("历史 JSON 解析失败（{id}）：{e}"))?;
        let mut msgs = persist::from_persisted(self, id, persisted);
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
    ///
    /// 与主历史同一条阶梯（图片外置 / 剥图 / 按轮降级），但**不拒存**：子代理历史没有
    /// UI 载体，压到只剩最后一轮后照写（越限只记日志，状态回传给调用方）。绝不触碰会话索引。
    ///
    /// blob 归子历史自己（owner = `<父会话 id>__<sub>`，见 [`sub_blob_owner`]）：与主历史共用
    /// 一套内容寻址与 GC，且父会话的 GC 不会顺手删掉子历史还引用着的图片；目录随父会话级联删除
    ///（见 `remove` / `cleanup::delete_session_files`）。
    pub fn save_sub_history(
        &self,
        parent: &str,
        sub: &str,
        msgs: &[Message],
    ) -> anyhow::Result<SaveReport> {
        // 子历史同样有 blob 与 GC → 与主历史共用同一把保存锁（锁序同上）
        let _guard = self.save_lock.lock().unwrap();
        let owner = sub_blob_owner(parent, sub);
        let mut prepared = repair::prepare_for_save(msgs.to_vec());
        repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 2);
        let (mut persisted, mut referenced) = persist::to_persisted(self, &owner, &prepared);
        let mut gz = gzip_history(&persisted)?;
        let mut stripped_images = 0usize;
        let mut dropped_rounds = 0usize;
        if gz.len() > MAX_HISTORY_BYTES {
            let before_images = count_images(&prepared);
            repair::sanitize_keep_thinking(&mut prepared);
            stripped_images = before_images.saturating_sub(count_images(&prepared));
            (persisted, referenced) = persist::to_persisted(self, &owner, &prepared);
            gz = gzip_history(&persisted)?;
            if gz.len() > MAX_HISTORY_BYTES {
                let before_rounds = count_rounds(&prepared);
                repair::trim(&mut prepared, TRIM_BUDGET_TOKENS, 1);
                dropped_rounds = before_rounds.saturating_sub(count_rounds(&prepared));
                (persisted, referenced) = persist::to_persisted(self, &owner, &prepared);
                gz = gzip_history(&persisted)?;
            }
            tracing::warn!(
                "子代理历史 {parent}/{sub} 超上限，已降级保存：剥图 {stripped_images} 张、丢弃 {dropped_rounds} 轮"
            );
        }
        let path = self.sub_history_path(parent, sub);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        atomic_write(&path, &gz)?;
        image_blobs::gc(self, &owner, &referenced);
        Ok(SaveReport {
            saved: true,
            stripped_images,
            dropped_rounds,
            bytes: gz.len(),
        })
    }

    /// 读取子代理过程历史；文件缺失返回空（旧会话没有；前端优雅降级）。
    /// 与主历史同一套落盘形态（blob 引用按 owner = `<父会话 id>__<sub>` 读回，缺失降级为占位文本）。
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
        let persisted: Vec<persist::PersistedMessage> = serde_json::from_slice(&json)
            .map_err(|e| anyhow::anyhow!("子代理历史 JSON 解析失败（{parent}/{sub}）：{e}"))?;
        let mut msgs = persist::from_persisted(self, &sub_blob_owner(parent, sub), persisted);
        repair::repair(&mut msgs);
        Ok(msgs)
    }

    /// 子代理历史 id 列表（`histories/subs/<parent>/` 下的文件名解析）。
    /// 供级联清理子历史的图片 blob 目录用——blob 归子历史自己，
    /// 不在父会话的 `sessions/<父>.imgblob/` 里。
    pub(crate) fn sub_history_ids(&self, parent: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.sub_histories_dir(parent)) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(sub) = name.strip_suffix(".json.gz").filter(|s| !s.is_empty()) {
                    out.push(sub.to_string());
                }
            }
        }
        out
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

    // ---------- 工具结果原样 sidecar（[docs/session-restore-fidelity](../../../../docs/session-restore-fidelity.md)） ----------

    /// 工具结果 sidecar 目录：sessions/<owner>.toolres/。
    /// 与子代理历史同范式（`histories/subs/<parent>/`）：路径由编号拼出、随会话级联删除，
    /// 不 upsert 会话索引、也不进右栏「文件」面板。
    pub(crate) fn tool_results_dir(&self, owner: &str) -> PathBuf {
        self.sessions_dir().join(format!("{owner}.toolres"))
    }

    // ---------- 图片 blob（[docs/session-history-limits](../../../../docs/session-history-limits.md)） ----------

    /// 图片 blob 目录：sessions/<owner>.imgblob/。
    /// 与工具结果 sidecar 同范式：路径由编号拼出、随会话级联删除（cleanup 与 remove 两条
    /// 删除路径同口径），不 upsert 会话索引、也不进右栏「文件」面板。
    /// owner 通常是会话 id；子代理历史传 `<父会话 id>__<sub>`（见 [`sub_blob_owner`]，
    /// 不能直接用 `sub`——不同父会话下的同名 sub 会共用目录）。
    pub(crate) fn image_blobs_dir(&self, owner: &str) -> PathBuf {
        self.sessions_dir().join(format!("{owner}.imgblob"))
    }

    /// 子历史图片 blob 目录：sessions/<父会话 id>__<sub>.imgblob/。
    /// 写（`save_sub_history`）/ 读（`load_sub_history`）/ GC / 级联删除四处共用它，
    /// 保证 owner 命名只在这一处定义（[`sub_blob_owner`]）。
    pub(crate) fn sub_image_blobs_dir(&self, parent: &str, sub: &str) -> PathBuf {
        self.image_blobs_dir(&sub_blob_owner(parent, sub))
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
    pub fn touch_session_open(&self, id: &str, now: chrono::DateTime<Utc>) -> anyhow::Result<bool> {
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

/// 子历史图片 blob 的 owner 分隔符（见 [`sub_blob_owner`]）。
const SUB_BLOB_OWNER_SEP: &str = "__";

/// 子历史图片 blob 的 owner：`<父会话 id>__<sub id>`。
///
/// 为什么不直接用 `sub` 当 owner：`sub_id` 只是 uuid 前 8 位十六进制（32 bit，见
/// `tools/subagent.rs`），两个**不同父会话**下出现同名 sub 并非不可能（生日碰撞）；
/// 共用 `sessions/<sub>.imgblob/` 时，一方的 GC 会删掉另一方仍在引用的图片，
/// 删一个父会话也会连带删掉另一个会话子历史的图。拼上父会话 id 后命名空间互不重叠。
///
/// 拼接形态过 [`is_safe_session_id`] 白名单：该白名单只禁 `/`、`\`、`.` 与首尾空白
///（见 `projects::valid_id`），`_` 与 `__` 都不在其中。
pub(crate) fn sub_blob_owner(parent: &str, sub: &str) -> String {
    format!("{parent}{SUB_BLOB_OWNER_SEP}{sub}")
}

/// 反解判定：某个 blob owner 是不是子历史的目录名（`<父会话 id>__<sub_…>`）。
///
/// 供索引外残留扫描用（`cleanup::orphan_candidates`）：`sessions/<父>__<sub>.imgblob/` 的
/// 「id」既不在会话索引里、也不带 `sub_` 前缀，照旧按「不在索引即孤儿」判定会被删掉——
/// 而父会话仍活着、其子历史还引用着这些图。子 blob 目录一律由级联删除路径
///（`remove` / `cleanup::delete_session_files` / `purge_non_session_entries`）负责，扫描不碰。
///
/// 只认 `sub_` / `task_` 前缀的段，避免把普通会话 id（uuid 不含 `_`）误判进来。
pub(crate) fn is_sub_blob_owner(owner: &str) -> bool {
    match owner.split_once(SUB_BLOB_OWNER_SEP) {
        Some((parent, sub)) => {
            !parent.is_empty() && (sub.starts_with("sub_") || sub.starts_with("task_"))
        }
        None => false,
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

/// 历史序列化 + gzip（主历史与子代理历史的超上限降级重试共用）。
/// 泛型化：两侧的落盘形态都是 [`persist::PersistedMessage`] 列表，序列化只需要 `Serialize`。
fn gzip_history<T: Serialize>(msgs: &T) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec(msgs)?;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &json)?;
    Ok(enc.finish()?)
}

/// 历史里的图片块张数（降级阶梯的剥图计数用）。
fn count_images(msgs: &[Message]) -> usize {
    msgs.iter()
        .flat_map(|m| m.content.iter())
        .filter(|c| matches!(c, Content::Image { .. }))
        .count()
}

/// 历史里的用户轮数（一轮 = 一条 User 消息及其后的 Assistant/Tool 消息，与 `repair::trim` 同口径）。
fn count_rounds(msgs: &[Message]) -> usize {
    msgs.iter().filter(|m| m.role == Role::User).count()
}

#[cfg(test)]
mod tests;
