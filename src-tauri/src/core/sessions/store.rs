use crate::core::sessions::segments::{self, HistoryBoundary, HistoryFormat, SegmentInfo};
use crate::core::sessions::{cleanup::is_safe_session_id, image_blobs, persist, repair};
use crate::core::types::{Content, Message, Role};
use crate::util::atomic::atomic_write;
use crate::util::token_est::est_tokens_message;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// 会话索引条目上限（超过此数按 LRU 淘汰最旧条目；gz 保留可再发现）。
pub const MAX_INDEX_ENTRIES: usize = 2000;
/// 旧「单会话历史 8MB 上限」常量（**P1 起不再用于拒存**）。
///
/// 分段 JSONL 落地后它只剩一个用途：**剥图兜底的触发口径**——本次要落盘的内容明文超过
/// 这个量级、且其中还有内联大图时先剥图（旧数据兜底）再写，但**绝不因为超限而拒存**。
/// P4 会把它改造成软告警 / 硬熔断两条线（软 200MB / 硬 1GB）并接管现有语义，故保留定义。
pub const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
/// wire 侧（运行上下文）trim 的 token 预算：**落盘不再裁剪**，本预算只作用于
/// `load_history_full` 重建出的 `wire`（与 `context` 的自动压缩配套）；
/// 展示侧（`display`）拿到的仍是未裁剪的完整转录。
pub const TRIM_BUDGET_TOKENS: u64 = 256 * 1024;

/// 历史体积的**软告警线**（200 MB）：超过只提示、**照常写盘**（P4）。
///
/// 依据：正常会话历史是几十 KB ~ 几 MB 量级（一条 message 明文一行、几 KB，200 轮 ≈ 1~2 MB）。
/// 200 MB 已意味着数万轮、或某几条 message 异常巨大——**触达即代表异常**，值得提醒用户
///（压缩上下文 / 新开会话），但**不改变写入行为**：数据完整性优先于体积。
pub const HISTORY_SOFT_WARN_BYTES: u64 = 200 * 1024 * 1024;

/// 历史体积的**硬熔断线**（1 GB）：达到即**停止 append**（P4）。
///
/// 依据：1 GB 是软线的 5 倍，远超任何正常会话；到这一步仍继续 append 只会吃满磁盘
///（分段 JSONL 明文不压缩，1 GB 已不是「便于 grep / 排障」能平衡的量）。
/// **熔断只停写、绝不删数据**：既有段与 blob 一个字节都不动，历史仍可读（展示侧照常分页）；
/// 体积回落后下一次保存自动清除状态（可自愈）。
pub const HISTORY_HARD_FUSE_BYTES: u64 = 1024 * 1024 * 1024;

/// 历史体积的两条线（[`HISTORY_SOFT_WARN_BYTES`] / [`HISTORY_HARD_FUSE_BYTES`]）。
///
/// 生产路径恒为 [`HistoryLimits::default`]；测试用 [`SessionStore::with_history_limits`]
/// 注入小阈值（真造 200 MB 数据的用例不现实）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    /// 软告警线：`>=` 即挂 `Warned`（照常写）
    pub soft: u64,
    /// 硬熔断线：`>=` 即停写并挂 `Fused`
    pub hard: u64,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            soft: HISTORY_SOFT_WARN_BYTES,
            hard: HISTORY_HARD_FUSE_BYTES,
        }
    }
}

/// 上下文压缩产出的摘要消息前缀（`core/context.rs::compact_history` 实测形态）。
const HANDOFF_SUMMARY_PREFIX: &str = "<handoff-summary>";

/// 一次历史保存的结果（供调用方上报用户）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveReport {
    /// 是否落盘成功（false = 写盘失败，磁盘上仍是上一次成功的历史）
    pub saved: bool,
    /// 被剥掉图片 payload 的张数（**旧数据兜底**：历史里还有内联大图时；
    /// 图片外置 + 取消上限后正常不再触发）
    pub stripped_images: usize,
    /// 因超限被丢弃的轮数：**P1 起恒为 0**（按轮降级已移除；字段保留给 P4 复用或另立语义）
    pub dropped_rounds: usize,
    /// 落盘字节数（该会话**段文件字节总和**；saved = false 时为 0）
    pub bytes: usize,
    /// **本次保存的裁决状态**（P4）：`None` = 干净；`Some(_)` = 有损（剥图）/ 体积软告警 /
    /// 体积硬熔断。与 `SessionMeta.history_status` 是**同一个枚举**——`run:done` 的
    /// `history_save` 载荷因此能原样携带挂索引的那份状态，前端两条链路（当场 / 重启后）共用一套文案。
    ///
    /// 字段名与索引侧同名是刻意的（同一个东西、两处可见性载体）。
    /// serde default + 缺省不序列化：干净保存的载荷形状与 P1 之前一致（仍是 4 个键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_status: Option<HistoryStatus>,
}

/// 会话历史的读取结果（批2 P1：**wire 与 display 分离**）。
///
/// - `wire`：运行上下文（= 最后一个压缩边界之后的时间线）——
///   `core/agent/drive.rs` 与 `host::load_session` 取它，**语义与旧实现一致**
///   （重启后的上下文逐字节相同，token 与计费零变化）；
/// - `display`：完整转录（全部 message 记录，**不做 trim**）——P2/P3 的分页与「加载更早的」数据源；
/// - `segments`：段清单（序号 / 文件名 / 条数 / 字节数），P2 按段分页用；
/// - `boundaries`：压缩 / 改写边界（P3 据此画「上下文已压缩」分隔线）；
/// - `format`：`new`（段式 JSONL）/ `legacy`（旧 `.json.gz`，读兼容不迁移）；
/// - `bytes`：落盘字节数（段文件总和；旧格式 = 该文件字节数，P4 的熔断判据同源）。
#[derive(Debug, Clone)]
pub struct HistoryLoad {
    /// 运行上下文（wire）
    pub wire: Vec<Message>,
    /// 完整转录（展示侧；不 trim）
    pub display: Vec<Message>,
    /// 段清单（旧格式为空）
    pub segments: Vec<SegmentInfo>,
    /// 时间线重置点（旧格式为空）
    pub boundaries: Vec<HistoryBoundary>,
    /// 落盘格式标记
    pub format: HistoryFormat,
    /// 落盘字节数
    pub bytes: u64,
}

impl SaveReport {
    /// 干净落盘：成功、没有剥图 / 丢轮，且**体积裁决为干净**（无软告警、无熔断）。
    ///
    /// 这里是「新状态能不能发出去」的总闸：`core/agent/drive.rs` 的上报判据是
    /// `filter(|r| !r.is_clean())`——漏改会让 P4 的提示永远发不出去。
    pub fn is_clean(&self) -> bool {
        self.saved
            && self.stripped_images == 0
            && self.dropped_rounds == 0
            && self.history_status.is_none()
    }

    /// 拒存（磁盘上仍是上一次成功的历史）。
    pub fn rejected() -> Self {
        Self {
            saved: false,
            stripped_images: 0,
            dropped_rounds: 0,
            bytes: 0,
            history_status: None,
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
    /// 拒存：历史超过上限，磁盘上仍是上一次成功的历史。
    ///
    /// **P1 起不再产生**（8MB 拒存分支已移除，AC-6/AC-7）；保留该变体是为了兼容
    /// 旧索引里的存量状态与前端既有文案（`notice.historyRejected`）——删掉会让
    /// 旧数据少一条可见提示。
    Rejected {
        /// 发生时刻（RFC3339）
        at: String,
        /// 原因（用户可见文案）
        reason: String,
    },
    /// **体积软告警**（P4）：历史总量已超 [`HISTORY_SOFT_WARN_BYTES`]——**照常写盘**，只是提醒。
    ///
    /// 带上体积与阈值是刻意的：前端要能说清「多大 / 限到多少」。人类可读格式化（MB / GB）
    /// 由前端做，后端不塞格式化字符串。
    Warned {
        /// 裁决时刻的历史总字节数（段文件 + 旧格式文件）
        bytes: u64,
        /// 触发的阈值（= [`HISTORY_SOFT_WARN_BYTES`]；随状态一起存，日后改常量仍可解释旧状态）
        threshold: u64,
        /// 发生时刻（RFC3339）
        at: String,
    },
    /// **体积硬熔断**（P4）：历史总量已达 [`HISTORY_HARD_FUSE_BYTES`] → **停止 append**。
    ///
    /// 语义边界（reviewer 关注点）：熔断只**停止写入**，**绝不删除任何既有段 / blob**——
    /// 已有历史一个字节都不动，仍可读（展示侧照常分页）。体积回落后下一次保存自动清除（自愈）。
    Fused {
        /// 裁决时刻的历史总字节数
        bytes: u64,
        /// 触发的阈值（= [`HISTORY_HARD_FUSE_BYTES`]）
        threshold: u64,
        /// 发生时刻（RFC3339）
        at: String,
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

/// 会话持久化存储：历史（gzip）+ 索引（JSON）+ 边车（todos / goal / 产物 / 子代理历史）。
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
    /// 分段历史的**进程内增量水位**（键 = 段目录）：`save_history` 靠它做「只追加新增消息」
    /// 的判定；冷启动（本进程没有该会话）时从段文件重算。
    ///
    /// 只是**缓存而不是真相**：删除会话 / 幽灵条目清扫 / 写盘失败都会丢弃对应条目，
    /// 下一次保存自动重扫。锁序 `save_lock → watermarks`：只在极短作用域内取用，
    /// 绝不跨索引写持有它。
    watermarks: std::sync::Mutex<std::collections::HashMap<PathBuf, segments::SaveState>>,
    /// M8：索引读-改-写互斥（并发 checkpoint/delete/rename 不得丢条目）
    index_lock: std::sync::Mutex<()>,
    /// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：产物边车读-改-写互斥（并发工具写不得丢条目）
    artifacts_lock: std::sync::Mutex<()>,
    /// 进程内「运行中」会话 id 集合：索引 `running` 字段的唯一事实源。
    /// 新建会话首次 run 尚未检查点时索引里没有条目，标记先记在这里，待其首次 upsert 时带上
    ///（否则该 run 崩溃后将无从标记）。进程消亡即消失，恢复由 running.marker 机制接管。
    running: std::sync::Mutex<std::collections::HashSet<String>>,
    /// P4 历史体积的两条线（软告警 / 硬熔断）：生产恒为默认常量，测试用
    /// [`SessionStore::with_history_limits`] 注入小阈值。
    history_limits: HistoryLimits,
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
            watermarks: std::sync::Mutex::new(std::collections::HashMap::new()),
            index_lock: std::sync::Mutex::new(()),
            artifacts_lock: std::sync::Mutex::new(()),
            running: std::sync::Mutex::new(std::collections::HashSet::new()),
            history_limits: HistoryLimits::default(),
            #[cfg(test)]
            index_writes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 覆盖历史体积的两条线（软告警 / 硬熔断）——**测试用**（生产恒用默认常量）：
    /// 真造 200 MB / 1 GB 数据的用例不现实，故阈值可注入。
    #[must_use]
    pub fn with_history_limits(mut self, limits: HistoryLimits) -> Self {
        self.history_limits = limits;
        self
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

    /// 会话历史**段目录**：histories/<id>/（批2 新格式；清理按同一路径级联删除）。
    pub(crate) fn history_dir(&self, id: &str) -> PathBuf {
        self.histories_dir().join(id)
    }

    /// 旧格式历史文件路径：histories/<id>.json.gz。
    /// 新格式落地后只剩两处用途：**旧数据读回落**与**清理时顺带删除**（不做迁移）。
    pub(crate) fn history_path(&self, id: &str) -> PathBuf {
        self.histories_dir().join(format!("{id}.json.gz"))
    }

    /// 子代理历史的**段目录**：histories/subs/<parent>/<sub>/。
    pub(crate) fn sub_history_dir(&self, parent: &str, sub: &str) -> PathBuf {
        self.sub_histories_dir(parent).join(sub)
    }

    /// 会话历史占用的字节数（段文件总和 + 旧格式文件）：P4 的软告警 / 硬熔断判据预留。
    pub fn session_history_bytes(&self, id: &str) -> u64 {
        let legacy = std::fs::metadata(self.history_path(id))
            .map(|m| m.len())
            .unwrap_or(0);
        segments::dir_bytes(&self.history_dir(id)) + legacy
    }

    /// 丢弃某个段目录（含其下子历史目录）的增量水位缓存。
    /// 删除会话 / 幽灵清扫 / 写盘失败时调用：水位只是缓存，丢掉后下一次保存会自动重扫。
    fn drop_watermark(&self, dir: &Path) {
        let mut map = self.watermarks.lock().unwrap();
        map.remove(dir);
        map.retain(|k, _| !k.starts_with(dir));
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

    /// 索引里的标题 / 模型是否与本次保存传入的值不同（`noop` 分支据此决定要不要补一次索引写）。
    ///
    /// 索引里没有该条目也返回 true：那正是「新会话首次落盘前改名」的情形，`upsert_meta` 会新建条目。
    /// 只在历史 `noop` 时被调用，常规检查点（历史真的变了）不会因此多读一次索引。
    fn meta_differs(&self, id: &str, title: &str, model_id: Option<&str>) -> bool {
        match self.get(id) {
            Some(m) => m.title != title || m.model_id.as_deref() != model_id,
            None => true,
        }
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
                // 新格式：histories/<id>/ 段目录（`subs/` 是子历史桶，不是会话）
                if e.path().is_dir() {
                    if name != "subs" && !name.is_empty() && !known.contains(&name) {
                        out.push(name);
                    }
                    continue;
                }
                // 旧格式：histories/<id>.json.gz
                if let Some(id) = name.strip_suffix(".json.gz") {
                    if !known.contains(id) {
                        out.push(id.to_string());
                    }
                }
            }
        }
        out
    }

    /// 清理索引中的非会话条目（sub_*/task_* 前缀）及其 gz 与 todos/goal/artifacts 边车。
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
            // 两种历史形态都清：新格式段目录 + 旧格式单文件
            self.drop_watermark(&self.history_dir(id));
            let _ = std::fs::remove_dir_all(self.history_dir(id));
            let _ = std::fs::remove_file(self.history_path(id));
            let _ = std::fs::remove_file(self.todos_path(id));
            let _ = std::fs::remove_file(self.goal_path(id));
            let _ = std::fs::remove_dir_all(self.goal_sources_dir(id));
            let _ = std::fs::remove_file(self.prefs_path(id));
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
        // 水位缓存先丢弃（纯缓存；留着会让「删后同名重建」沿用旧水位）
        self.drop_watermark(&self.sub_histories_dir(id));
        self.drop_watermark(&self.history_dir(id));
        // 历史：新格式段目录 + 旧格式单文件都删（读兼容期两种形态可能并存）
        let _ = std::fs::remove_dir_all(self.history_dir(id));
        let _ = std::fs::remove_file(self.history_path(id));
        // 边车级联清理（todos 曾泄漏；此处一并修复）
        let _ = std::fs::remove_file(self.artifacts_path(id));
        let _ = std::fs::remove_file(self.todos_path(id));
        let _ = std::fs::remove_file(self.goal_path(id));
        let _ = std::fs::remove_dir_all(self.goal_sources_dir(id));
        let _ = std::fs::remove_file(self.prefs_path(id));
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

    /// 历史保存：sanitize → repair → **分段 append-only JSONL**（**不再 trim**、不再整份改写）。
    ///
    /// 增量管线（水位判定在 [`segments`] 里，本函数把结果接到索引与 GC 上）：
    /// - `len(prepared) > written` 且前缀指纹一致 → **只追加** `prepared[written..]`（O(增量)）；
    /// - `len(prepared) == written` 且指纹一致 → **什么都不做**（不写段、不写索引、不 GC）；
    /// - 其余（前缀被改写 / 历史被压缩缩短）→ **基线段**：完整快照写成一个新段，
    ///   **既有段一律保留不动**（展示侧还要能翻到更早内容）。
    ///
    /// 旧「8MB 上限阶梓」的两级已移除：**按轮降级**与**拒存**都不再发生（AC-6/AC-7）；
    /// 只保留一级**剥图**作旧数据兜底：本次要落盘的内容明文超 [`MAX_HISTORY_BYTES`]
    /// 且历史里还有内联大图时先剥图（`sanitize_keep_thinking` 只剥图片、**保留思考**——
    /// 思考是 OpenAI 兼容 thinking 上游回传 `reasoning_content` 的唯一数据源，剥掉会让该会话
    /// 重启后多轮必然 400 且不可自愈，见 [docs/reasoning-content-passthrough]），
    /// 但**不再因为体积而拒存**。
    ///
    /// 顺序纪律：**历史写盘成功之后**才做索引两步与 blob GC；`upsert_meta` /
    /// `set_history_status` 失败只告警（历史已落盘即算成功）；GC 的引用集合是
    /// **磁盘上全部保留段引用的 blob 并集**（不是本次保存的集合）——旧段仍引用旧 blob，
    /// 用「本次集合」会删掉它们（不可逆数据丢失）。
    ///
    /// P4 磁盘约束：写盘**前**预判硬熔断线（[`HISTORY_HARD_FUSE_BYTES`]）——超线则本次**跳过写入**
    ///（既有段与 blob 一个字节都不动、历史仍可读），写盘后再量一次总量裁决状态：
    /// `>= 硬线` → `Fused`；`>= 软线` → `Warned`（**照常写**）；都未超 → `None`
    ///（**自愈**：体积回落后状态自动清除，提示不再挂）。
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
        // 保存串行化（见 `save_lock` 字段注释）：从「转换落盘形态（图片外置写 blob）」起就持锁。
        // 锁序恒为 save_lock → index_lock（临界区内的 upsert_meta / set_history_status 取后者）。
        let _guard = self.save_lock.lock().unwrap();
        let mut prepared = repair::prepare_for_save(msgs.to_vec());
        // **不再 trim**：内存里有就落盘（AC-1）；wire 侧的预算裁剪留给加载路径
        let dir = self.history_dir(id);
        // P4 硬熔断**预判**：必须在进入写入之前量一次——先写再停会让「停止增长」被本次写入打破，
        // 也会把本该被拦下的内容白落一次盘。判据含旧格式文件（两者并存时同样计入总量）。
        let fused = self.session_history_bytes(id) >= self.history_limits.hard;
        let written = if fused {
            // 熔断：本次**完全不写历史**——不 append、不写基线段、不动既有段与 blob。
            // `None` 同时让下面的 GC 被跳过：GC 的引用集合来自本次写入结果，跳过写入时它是空的，
            // 据此回收会把旧段仍在引用的图片删掉（不可逆数据丢失）。
            None
        } else {
            Some(self.write_history_locked(&dir, id, &mut prepared)?)
        };
        let stripped_images = written.as_ref().map_or(0, |r| r.stripped_images);
        let did_write = written.as_ref().is_some_and(|r| !r.write.noop);
        // 🟡-4：历史没变（`noop`）时索引仍要跟上**标题 / 模型**的变化——`rename_session`（带 runtime
        // 的那条路径）正是靠本函数落标题的；历史没变就跳过 `upsert_meta`，用户看到的就是「改名没生效」。
        // 判据取「与索引现值不同」，故**真·无变化**的那次保存照旧不产生任何索引写（同值跳过语义不破）。
        let meta_stale = written.as_ref().is_some_and(|r| r.write.noop)
            && self.meta_differs(id, title, model_id);

        // 状态裁决：**熔断时也必须落索引**（否则「已停止增长」这条提示重启后就没了）——
        // 所以本轮进块的条件是「写了东西 或 熔断（或如前一行：只变了标题 / 模型的 noop）」，
        // 而不是只看 `noop`。
        let mut status: Option<HistoryStatus> = None;
        if did_write || fused || meta_stale {
            // 其后的两步都只写**索引**（派生缓存），因此一律**非致命**：历史上面已经落盘，
            // 若索引写失败就让本函数返回 Err，调用方（`core/agent/drive.rs::checkpoint`）会把它
            // 映射成 `SaveReport::rejected()`，前端于是弹出「历史未能保存」——可历史其实已经写成功了。
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
            // P4 体积裁决（软告警 / 硬熔断）**优先于**剥图降级：两者同时出现时用户先要处理的是体积
            //（剥图只影响本次新增内容的保真度，且下一次保存通常就自愈了）。
            let total = self.session_history_bytes(id);
            let next = size_status(total, self.history_limits, &now)
                .or_else(|| degraded_status(stripped_images, &now));
            // 「同一个量值」再次裁决时**沿用索引里现存的 `at`**（变体与数字一致、只有时间戳不同）：
            // 否则每个检查点都会因 `at` 变化而重写索引，`set_history_status` 的同值跳过因此失效。
            status = match (next, self.get(id).and_then(|m| m.history_status)) {
                (Some(n), Some(prev)) if same_measure(&prev, &n) => Some(prev),
                (n, _) => n,
            };
            if let Err(e) = self.set_history_status(id, status.clone()) {
                tracing::warn!(
                    "会话 {id} 历史状态写入索引失败（历史已落盘，本次保存仍算成功）：{e}"
                );
            }
            // 只在历史**写成功**之后回收：判据 = 磁盘上全部保留段引用的 blob 并集；
            // 熔断路径不写历史、引用集合为空，故一并跳过（见上方 `fused` 分支注释）。
            if let Some(r) = written.as_ref().filter(|r| !r.write.noop) {
                image_blobs::gc(self, id, &r.write.blobs);
            }
        }
        Ok(SaveReport {
            saved: true,
            stripped_images,
            dropped_rounds: 0,
            bytes: segments::dir_bytes(&dir) as usize,
            history_status: status,
        })
    }

    /// 落盘历史（主历史与子历史共用）：水位判定 → 增量追加 / 基线段。
    ///
    /// 由调用方持 `save_lock`（本方法**不自行取锁**）；内部只碰段文件与水位缓存，
    /// 不写索引（索引由调用方在写盘成功后按需接线）。
    fn write_history_locked(
        &self,
        dir: &Path,
        owner: &str,
        prepared: &mut Vec<Message>,
    ) -> anyhow::Result<WriteResult> {
        // 水位：本进程有缓存就用，没有（冷启动 / 首个保存）就走**有界**推导
        let cached = self.watermarks.lock().unwrap().remove(dir);
        let mut state = cached.unwrap_or_else(|| Self::derive_watermark(dir));

        let len = prepared.len();
        // 前缀快照校验：O(len) 的哈希（CPU、无大分配、无压缩、无整份写盘）——比旧实现便宜一个量级
        let prefix_ok =
            len >= state.written && segments::prefix_sig(prepared, state.written) == state.sig;
        let mut stripped_images = 0usize;

        let write = if len == state.written && prefix_ok {
            // 增量游标命中：直接返回（省掉旧实现每次检查点的整份重写 + 索引写）
            segments::WriteOutcome::nothing()
        } else if len > state.written && prefix_ok {
            // 常态路径：只把新增部分转成落盘形态（图片外置也只写新图）
            let (increment, referenced) =
                persist::to_persisted(self, owner, &prepared[state.written..]);
            if segments::records_bytes(&increment) > MAX_HISTORY_BYTES {
                // 旧数据兜底：**本次增量**本身超量（典型：刚贴了一张内联巨图）→ 剥图后整体重算。
                // 判据取增量而不是整份历史：历史总量大不再是问题（这正是本批的目的）
                stripped_images = strip_images(prepared);
                let (all, refs) = persist::to_persisted(self, owner, prepared);
                segments::write_base_segment(dir, owner, &mut state, &all, None)?;
                state.blobs.extend(refs);
                segments::WriteOutcome {
                    appended: all.len(),
                    base: true,
                    noop: false,
                    blobs: blob_union(&state),
                }
            } else {
                segments::append_messages(dir, owner, &mut state, &increment)?;
                state.blobs.extend(referenced);
                segments::WriteOutcome {
                    appended: increment.len(),
                    base: false,
                    noop: false,
                    blobs: blob_union(&state),
                }
            }
        } else {
            // 前缀被改写（repair 原地修改 / 400 降级阀 sanitize）或历史被压缩缩短 →
            // 基线段：完整快照写成新段，**既有段一律保留**（不丢历史）
            let (mut all, mut referenced) = persist::to_persisted(self, owner, prepared);
            if segments::records_bytes(&all) > MAX_HISTORY_BYTES {
                stripped_images = strip_images(prepared);
                let (stripped, refs) = persist::to_persisted(self, owner, prepared);
                all = stripped;
                referenced = refs;
            }
            // 缩短 = 压缩 / repair 丢消息造成的时间线重置 → 记一条压缩边界，head 里存
            // **重置后历史的完整前缀**，wire 侧据此逐字节还原重启后的上下文
            let compaction = if len < state.written {
                Some(compaction_source(prepared))
            } else {
                None
            };
            segments::write_base_segment(dir, owner, &mut state, &all, compaction)?;
            state.blobs.extend(referenced);
            segments::WriteOutcome {
                appended: all.len(),
                base: true,
                noop: false,
                blobs: blob_union(&state),
            }
        };

        if !write.noop {
            // 写入成功后统一重设水位（条数 + 指纹）；失败路径不缓存，下次自动重扫
            state.reset(prepared);
            // 水位边车：下次冷启动据此**有界**推导水位（只读一个小文件 + 必要时尾读一个段）。
            // 写失败**不致命**（历史已落盘）：下次保存回退整目录扫描，结果一样、只是慢。
            // `noop` 时跳过：此时磁盘一个字节都没变，边车照旧作数。
            if let Err(e) = segments::write_meta(dir, &state) {
                tracing::warn!(
                    "历史水位边车写入失败（{}）：{e}（下次冷启动回退整目录扫描）",
                    dir.display()
                );
            }
        }
        self.watermarks
            .lock()
            .unwrap()
            .insert(dir.to_path_buf(), state);
        Ok(WriteResult {
            write,
            stripped_images,
        })
    }

    /// 读会话历史的 **wire**（运行上下文）——`drive.rs` 的入口，语义与旧实现一致
    ///（重启后的上下文逐字节不变），但**读取有界**：只从段尾向前读到「够用」为止。
    ///
    /// 落盘取消 8MB 上限后，旧的「先 `load_history_full` 再取 `wire`」是 O(全部历史字节)
    ///（历史越长、恢复运行上下文越慢），与本批要保住的「大会话秒开」直接冲突，故单开一个
    /// 只取 wire 的入口。`load_history_full` 仍保留（它要为展示侧给出**全量** `display`），
    /// 两者产出的 `wire` 由对拍用例钉死一致。
    pub fn load_history(&self, id: &str) -> anyhow::Result<Vec<Message>> {
        self.load_history_wire(id)
    }

    /// 读路径的格式裁决（**唯一入口**：`load_history_wire` / `load_history_full` 与命令层
    /// `load_for_open`、分页入口全部走它，绝不各自判一次）：
    ///
    /// - 段目录不存在 → 旧格式（现状不变；两种格式都没落盘时由 [`Self::load_legacy_history`] 照旧报 Err）；
    /// - 段目录存在、旧文件也不在 → 新格式（唯一副本：即便段全坏也走新格式路径，如实报空历史 + 坏段数，
    ///   绝不因为「新格式没内容」而报错）；
    /// - **两者并存** → 探针裁决（[`segments::has_readable_messages`]，与清理入口
    ///   `cleanup::has_new_format_history` 同一类判据且更严：还要有**可读消息**）。
    ///   空段目录 / 全是坏段 / 只有头与封口的段都不算「新格式有历史」，此时旧文件才是那份内容
    ///   唯一的可读副本 → 回落旧格式。
    ///
    /// 为什么必须查内容：只判「目录存在」会让这类会话走进新格式路径，而它一条可读消息都没有 ⇒
    /// 用户打开会话看到**空历史**（数据其实还在旧 `.json.gz` 里），清理入口又保守地不肯删旧文件。
    /// 探针只在两者并存时跑（常见情形只读一个段），转写期之外零开销。
    pub fn reads_new_format(&self, id: &str) -> bool {
        let dir = self.history_dir(id);
        if !segments::dir_exists(&dir) {
            return false;
        }
        if !self.history_path(id).is_file() {
            return true;
        }
        segments::has_readable_messages(&dir)
    }

    /// 只取 wire 的**有界**装载（语义 = `repair` + `trim` 后的 `scan.ledger`）。
    ///
    /// 「够用」的推导（保证与「全量扫描 + repair + trim」逐字节一致）：
    ///
    /// 记 `L` = 重置点之后的时间线（`scan.ledger`），`W` = `L` 的一个后缀窗口（起点落在
    /// 最老的 User 上），`F(·)` = `repair`、`T(·)` = `trim(_, TRIM_BUDGET_TOKENS, 2)`。
    /// `T` 的语义是「从最老的轮开始丢，直到总量 ≤ 预算或只剩 `keep_last = 2` 轮」，即保留
    /// 后缀为 `max(M, 2)` 轮（`M` = 使「后 m 轮总量 ≤ 预算」成立的最大 m；总量自尾部累计单调不减）。
    ///
    /// 1. 起点是 User ⇒ 窗口与全量在**轮边界**上对齐：`repair` 的配对不变量只涉及同一轮内的
    ///    `tool_use` / `tool_result`，窗口之外的改写（含给悬空 `tool_use` 补 `[interrupted]`）
    ///    全都落在被丢掉的轮里；再要求窗口内没有「引用窗口外 `tool_use`」的孤儿 `tool_result`
    ///    （那种结果会被窗口内的 `repair` 删掉、而全量的不会）⇒ `F(W)` 与 `F(L)` 的尾部相同；
    /// 2. 「`W` 自身总量 > 预算且 ≥ 3 轮」⇒ 全量的 `trim` **至少丢掉一轮**（它的条件是「总量
    ///    超预算且轮数 > keep_last = 2」），时间线开头可能存在的非 User 前缀与窗口之前的轮次
    ///    因此都被丢干净；又 `M_W ≤ 轮数 - 1`，而 `M_L == M_W`（更早的轮只会把总量抬得更大，
    ///    越界这一点已由 `W` 自身给出）⇒ 两边保留的都是 `max(M, 2)` 轮，逐条相同
    ///    ⇒ `T(F(W)) == T(F(L))`。
    ///
    /// 读到时间线重置点（压缩边界 / 基线段起点 / 目录起点）时窗口就是完整 `L`，直接走全量同路。
    /// 对拍与量化用例：`store/tests.rs::bounded_wire_*`。
    pub fn load_history_wire(&self, id: &str) -> anyhow::Result<Vec<Message>> {
        let dir = self.history_dir(id);
        if !self.reads_new_format(id) {
            // 旧格式（`.json.gz`）：没有段可分页，照旧整份读取（读兼容、不迁移）
            return Ok(self.load_legacy_history(id)?.wire);
        }
        let mut win = segments::WireWindow::open(&dir, TRIM_BUDGET_TOKENS);
        loop {
            if win.at_reset() {
                // 窗口 = 完整 wire 时间线：与全量路径逐字节同路
                let mut msgs = persist::from_persisted(self, id, win.window());
                repair::repair(&mut msgs);
                repair::trim(&mut msgs, TRIM_BUDGET_TOKENS, 2);
                return Ok(msgs);
            }
            if win.ready() {
                let mut msgs = persist::from_persisted(self, id, win.window());
                repair::repair(&mut msgs);
                // 精确判据（等价性推导见上方文档）：窗口自身超预算且 ≥ 3 轮
                let total: u64 = msgs.iter().map(est_tokens_message).sum();
                let rounds = msgs.iter().filter(|m| m.role == Role::User).count();
                if total > TRIM_BUDGET_TOKENS && rounds >= 3 {
                    repair::trim(&mut msgs, TRIM_BUDGET_TOKENS, 2);
                    return Ok(msgs);
                }
                // `repair` 削掉的量多于粗估（孤儿结果 / 空 assistant）→ 按差额要求多读一点，
                // 免得每读一段就重跑一次 repair（多读的代价有界，正确性由上面的精确判据兜底）
                let deficit = TRIM_BUDGET_TOKENS.saturating_sub(total) + 1;
                win.require_tokens(win.approx_tokens() + deficit);
            }
            win.extend();
        }
    }

    /// 水位推导（**有界**）：边车命中直接用（读一个小文件 + 必要时尾读一个段）；边车缺失 /
    /// 与磁盘不符（崩溃残行、上一批数据、手工改动）→ 回退整目录扫描（慢但一定对）。
    ///
    /// 判据与等价性见 [`segments::SegmentMeta`]/[`segments::state_from_meta`]。
    fn derive_watermark(dir: &Path) -> segments::SaveState {
        segments::state_from_meta(dir).unwrap_or_else(|| segments::scan(dir, false).state)
    }

    /// 读会话历史（wire + display + 段清单 + 格式标记）。
    ///
    /// 新格式：段回放 → `wire` = 最后一个压缩边界之后的时间线，再 `repair` + `trim`
    ///（**wire 侧的裁剪与落盘解耦**：落盘不再裁剪，运行上下文仍守 256k 预算，
    /// 因此 token 与计费零变化）；`display` = 全部 message 记录（**不 trim**，展示侧完整转录）。
    /// 旧格式（`.json.gz` 且新目录不存在）→ 走原逻辑读取，`wire = display = 原结果`，标记 `legacy`；
    /// 两者并存时**以新格式为准**。
    ///
    /// 这里的 `display` 是全量语义（它必须完整），所以本函数仍是整份回放；
    /// **只要 wire 的调用方请走 [`Self::load_history_wire`]**（有界）。两者产出的 `wire` 一致
    ///（对拍用例守护）。
    ///
    /// 容错：半行丢弃、非法行跳过、坏段跳过（各记 warn），**绝不因容错而报「历史损坏」**；
    /// blob 缺失仍只降级为占位文本（[docs/session-restore-fidelity]）。
    pub fn load_history_full(&self, id: &str) -> anyhow::Result<HistoryLoad> {
        let dir = self.history_dir(id);
        if self.reads_new_format(id) {
            let scan = segments::scan(&dir, true);
            let display = persist::from_persisted(self, id, scan.display);
            let mut wire = persist::from_persisted(self, id, scan.ledger);
            repair::repair(&mut wire);
            repair::trim(&mut wire, TRIM_BUDGET_TOKENS, 2);
            return Ok(HistoryLoad {
                wire,
                display,
                segments: scan.segments,
                boundaries: scan.boundaries,
                format: HistoryFormat::New,
                bytes: scan.bytes,
            });
        }
        self.load_legacy_history(id)
    }

    /// 旧格式回落：gunzip → 落盘形态 → 读回 base64 → repair → trim（**原逻辑**，读兼容不迁移）。
    ///
    /// 文件不存在返回 Err（与旧行为一致：调用方据此知道该会话没有历史）；
    /// 文件损坏 / 无法解析**不再整会话失败**——记 warn 后按空历史处理（容错优先）。
    fn load_legacy_history(&self, id: &str) -> anyhow::Result<HistoryLoad> {
        let path = self.history_path(id);
        let raw = std::fs::read(&path)?;
        let mut msgs = match decode_legacy_history(self, id, &raw) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("旧格式历史无法解析（{id}），按空历史处理（不阻断会话）：{e}");
                Vec::new()
            }
        };
        repair::repair(&mut msgs);
        repair::trim(&mut msgs, TRIM_BUDGET_TOKENS, 2);
        Ok(HistoryLoad {
            wire: msgs.clone(),
            display: msgs,
            segments: Vec::new(),
            boundaries: Vec::new(),
            format: HistoryFormat::Legacy,
            bytes: raw.len() as u64,
        })
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
    /// 与主历史**共用同一套段式模块**（[`segments`]）与同一把保存锁：增量 append、不 trim、
    /// **不拒存**（子代理历史没有 UI 载体，越限只记日志，状态回传给调用方）。绝不触碰会话索引。
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
        let dir = self.sub_history_dir(parent, sub);
        let mut prepared = repair::prepare_for_save(msgs.to_vec());
        let result = self.write_history_locked(&dir, &owner, &mut prepared)?;
        if !result.write.noop {
            image_blobs::gc(self, &owner, &result.write.blobs);
        }
        Ok(SaveReport {
            saved: true,
            stripped_images: result.stripped_images,
            dropped_rounds: 0,
            bytes: segments::dir_bytes(&dir) as usize,
            // 子代理路径**不参与体积裁决**（无 UI 载体，停写无人可见）：只有剥图算「不干净」，
            // 与 P1 之前 `is_clean()` 的判据等价（`tools/subagent.rs` 据此记日志 / 回报）
            history_status: degraded_status(result.stripped_images, &Utc::now().to_rfc3339()),
        })
    }
    /// 读取子代理过程历史（wire = 完整时间线；子代理历史没有 wire/display 之分）。
    ///
    /// 文件缺失返回空数组（旧会话没有；前端优雅降级）；**坏段 / 坏行不再整份失败**
    ///（记 warn 后跳过，能读多少读多少）。blob 引用按 owner = `<父会话 id>__<sub>` 读回，
    /// 缺失降级为占位文本。
    pub fn load_sub_history(&self, parent: &str, sub: &str) -> anyhow::Result<Vec<Message>> {
        let owner = sub_blob_owner(parent, sub);
        let dir = self.sub_history_dir(parent, sub);
        if segments::dir_exists(&dir) {
            let scan = segments::scan(&dir, true);
            let mut msgs = persist::from_persisted(self, &owner, scan.ledger);
            repair::repair(&mut msgs);
            return Ok(msgs);
        }
        // 旧格式回落（读兼容，不迁移）
        let path = self.sub_history_path(parent, sub);
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut msgs = match decode_legacy_history(self, &owner, &raw) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("旧格式子代理历史无法解析（{owner}），按空历史处理：{e}");
                Vec::new()
            }
        };
        repair::repair(&mut msgs);
        Ok(msgs)
    }
    /// 子代理历史 id 列表（`histories/subs/<parent>/` 下的目录名 / 旧文件名解析）。
    /// 供级联清理子历史的图片 blob 目录用——blob 归子历史自己，
    /// 不在父会话的 `sessions/<父>.imgblob/` 里。
    pub(crate) fn sub_history_ids(&self, parent: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.sub_histories_dir(parent)) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                // 新格式：histories/subs/<parent>/<sub>/ 段目录
                if e.path().is_dir() {
                    if !name.is_empty() {
                        out.push(name);
                    }
                    continue;
                }
                // 旧格式：histories/subs/<parent>/<sub>.json.gz
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

    // ---------- 目标模式（goal mode）边车 ----------

    /// goal 边车文件路径：sessions/<id>.goal.json（清理按同一路径删除）。
    pub(crate) fn goal_sources_dir(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.goal.sources"))
    }

    pub(crate) fn goal_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.goal.json"))
    }

    /// 持久化目标状态（原子写）。`None` 落盘为 JSON `null`（清除语义；读回仍是 None），
    /// 保留文件而非删文件——删除只走 `remove` / 清理链，避免「清目标」与「删会话」两条语义混用。
    pub fn save_goal(
        &self,
        id: &str,
        goal: &Option<crate::core::agent::goal::GoalState>,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(goal)?;
        atomic_write(&self.goal_path(id), &bytes)?;
        Ok(())
    }

    /// 读目标状态；缺失/损坏/`null` 一律回 None（边车只是辅助视图，绝不阻断会话）。
    pub fn load_goal(&self, id: &str) -> Option<crate::core::agent::goal::GoalState> {
        std::fs::read(self.goal_path(id))
            .ok()
            .and_then(|b| {
                serde_json::from_slice::<Option<crate::core::agent::goal::GoalState>>(&b).ok()
            })
            .flatten()
    }

    /// 主会话运行偏好边车；与 goal 一样随会话级联删除。
    pub(crate) fn prefs_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.prefs.json"))
    }

    pub fn save_prefs(
        &self,
        id: &str,
        snapshot: &crate::core::prefs::SessionPrefsSnapshot,
    ) -> anyhow::Result<()> {
        atomic_write(&self.prefs_path(id), &serde_json::to_vec_pretty(snapshot)?)?;
        Ok(())
    }

    /// 旧会话无边车时交由调用方选用全局默认；损坏边车不阻断会话打开。
    pub fn load_prefs(&self, id: &str) -> Option<crate::core::prefs::SessionPrefsSnapshot> {
        std::fs::read(self.prefs_path(id))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
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

/// 一次落盘的内部结果（段写入结果 + 剥图计数）。
struct WriteResult {
    write: segments::WriteOutcome,
    stripped_images: usize,
}

/// P4 体积裁决（纯函数，三条分支可逐条单测）：超硬线 → [`HistoryStatus::Fused`]
///（本次**跳过写入**）；超软线 → [`HistoryStatus::Warned`]（**照常写**）；都未超 → `None`
///（**自愈**：体积回落后状态自动清除，提示不再挂）。
fn size_status(total: u64, limits: HistoryLimits, at: &str) -> Option<HistoryStatus> {
    if total >= limits.hard {
        Some(HistoryStatus::Fused {
            bytes: total,
            threshold: limits.hard,
            at: at.to_string(),
        })
    } else if total >= limits.soft {
        Some(HistoryStatus::Warned {
            bytes: total,
            threshold: limits.soft,
            at: at.to_string(),
        })
    } else {
        None
    }
}

/// 剥图兜底的状态（无剥图 = `None`）——与 P1 的 `is_clean()` 判据同源。
fn degraded_status(stripped_images: usize, at: &str) -> Option<HistoryStatus> {
    (stripped_images > 0).then(|| HistoryStatus::Degraded {
        stripped_images,
        dropped_rounds: 0,
        at: at.to_string(),
    })
}

/// 两次裁决是否**同一个量值**（变体 + 关键数字一致，`at` 除外）。
///
/// 用途：避免每次检查点因时间戳不同而重写索引（`set_history_status` 的同值跳过会因此失效）。
fn same_measure(a: &HistoryStatus, b: &HistoryStatus) -> bool {
    match (a, b) {
        (
            HistoryStatus::Warned {
                bytes: x,
                threshold: t,
                ..
            },
            HistoryStatus::Warned {
                bytes: y,
                threshold: u,
                ..
            },
        )
        | (
            HistoryStatus::Fused {
                bytes: x,
                threshold: t,
                ..
            },
            HistoryStatus::Fused {
                bytes: y,
                threshold: u,
                ..
            },
        ) => x == y && t == u,
        (
            HistoryStatus::Degraded {
                stripped_images: x,
                dropped_rounds: y,
                ..
            },
            HistoryStatus::Degraded {
                stripped_images: p,
                dropped_rounds: q,
                ..
            },
        ) => x == p && y == q,
        _ => false,
    }
}

/// 旧数据兜底：剥掉图片 payload（占位文本）但**保留思考**，返回被剥的张数。
///
/// 为什么保留调用：本批之前的历史里可能还有内联大图（`image_inline`），剥图是它们唯一的
/// 减负手段，移除会让旧会话内容发生变化；但**不再因为它而拒存**
///（上限阶梓的拒存 / 按轮降级两级已删）。
fn strip_images(prepared: &mut Vec<Message>) -> usize {
    let before = count_images(prepared);
    repair::sanitize_keep_thinking(prepared);
    before.saturating_sub(count_images(prepared))
}

/// 时间线重置的**来源判定**（上下文压缩 vs 其它缩短）。
///
/// 实探结论（**不猜**）：`core/context.rs::compact_history` 把 `rt.history` 整体替换为
/// `[<handoff-summary>…</handoff-summary> 的 user 消息]`，`keep_last_user` 为真时再追加一条
/// 「压缩前最后一条 user 消息」——即「摘要首条 +（最多再一条）尾部」，与旧实现重启后的上下文同形。
/// 因此按「首条是不是 handoff 摘要」区分 `compact` 与 `shrink`（后者 = repair 等路径造成的缩短）。
/// 两者对 wire 重建的处理完全相同，此标记只给排障与展示侧一个可读来源。
fn compaction_source(prepared: &[Message]) -> &'static str {
    let is_summary = prepared.first().is_some_and(|m| {
        m.role == Role::User
            && m.content.iter().any(
                |c| matches!(c, Content::Text { text } if text.starts_with(HANDOFF_SUMMARY_PREFIX)),
            )
    });
    if is_summary { "compact" } else { "shrink" }
}

/// 水位里的 blob 并集 → 排序后的列表（GC 判据；排序只为让日志与测试可复现）。
fn blob_union(state: &segments::SaveState) -> Vec<String> {
    let mut out: Vec<String> = state.blobs.iter().cloned().collect();
    out.sort();
    out
}

/// 历史里的图片块张数（旧数据兜底的剥图计数用）。
fn count_images(msgs: &[Message]) -> usize {
    msgs.iter()
        .flat_map(|m| m.content.iter())
        .filter(|c| matches!(c, Content::Image { .. }))
        .count()
}

/// 旧格式（`.json.gz` 里的 JSON 数组）解码 + 读回 base64。
/// blob 缺失仍只降级为占位文本（见 [`persist::from_persisted`]）。
fn decode_legacy_history(
    store: &SessionStore,
    id: &str,
    raw: &[u8],
) -> anyhow::Result<Vec<Message>> {
    let mut dec = flate2::read::GzDecoder::new(raw);
    let mut json = Vec::new();
    dec.read_to_end(&mut json)
        .map_err(|e| anyhow::anyhow!("历史文件损坏（{id}）：{e}"))?;
    let persisted: Vec<persist::PersistedMessage> = serde_json::from_slice(&json)
        .map_err(|e| anyhow::anyhow!("历史 JSON 解析失败（{id}）：{e}"))?;
    Ok(persist::from_persisted(store, id, persisted))
}
#[cfg(test)]
mod tests;

/// 目标模式边车测试（写在 store.rs 内，与 `store/tests.rs` 的同名模块并存）。
#[cfg(test)]
mod goal_sidecar_tests {
    use super::*;
    use crate::core::agent::goal::{GoalCriterion, GoalLedger, GoalState, GoalStatus};
    use crate::core::prefs::{EffortLevel, SessionPrefsSnapshot};

    fn prefs() -> SessionPrefsSnapshot {
        SessionPrefsSnapshot {
            goal_mode_active: true,
            model_choice_recorded: true,
            model_id: Some("m1".into()),
            reasoning_effort: Some(EffortLevel::Max),
        }
    }

    fn sample() -> GoalState {
        GoalState {
            text: "把 X 改成 Y".into(),
            criteria: vec![GoalCriterion {
                title: "测试通过".into(),
                done: true,
                manual: false,
                verification: None,
            }],
            ledger: GoalLedger {
                paths: vec!["/work/proj/src".into()],
                programs: vec!["cargo".into()],
            },
            status: GoalStatus::Executing,
            decisions: vec!["用 A 方案".into()],
            pending: vec!["补测试".into()],
            blocked: vec![],
            rounds: 2,
            stall_streak: 1,
            ledger_denials: 0,
            delivery: Default::default(),
        }
    }

    #[test]
    fn goal_sidecar_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        assert!(store.load_goal("s1").is_none(), "缺失时回 None");
        store.save_goal("s1", &Some(sample())).unwrap();
        assert!(dir.path().join("sessions/s1.goal.json").exists());
        let back = store.load_goal("s1").expect("应能读回");
        assert_eq!(back, sample());
        // 会话隔离
        assert!(store.load_goal("s2").is_none());
        // 清除语义：写 None → 读回 None（文件仍在，但不再有目标）
        store.save_goal("s1", &None).unwrap();
        assert!(store.load_goal("s1").is_none());
    }

    #[test]
    fn goal_sidecar_corrupt_or_null_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(dir.path().join("sessions/bad.goal.json"), b"not json").unwrap();
        std::fs::write(dir.path().join("sessions/nil.goal.json"), b"null").unwrap();
        assert!(store.load_goal("bad").is_none());
        assert!(store.load_goal("nil").is_none());
    }

    #[test]
    fn prefs_sidecar_roundtrip_and_corrupt_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        assert!(store.load_prefs("s1").is_none());
        store.save_prefs("s1", &prefs()).unwrap();
        let loaded = store.load_prefs("s1").unwrap();
        assert!(loaded.goal_mode_active);
        assert!(loaded.model_choice_recorded);
        assert_eq!(loaded.model_id.as_deref(), Some("m1"));
        assert_eq!(loaded.reasoning_effort, Some(EffortLevel::Max));
        std::fs::write(store.prefs_path("s1"), b"bad json").unwrap();
        assert!(store.load_prefs("s1").is_none());
    }

    #[test]
    fn remove_and_purge_cascade_goal_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        // remove 级联
        store.save_goal("s1", &Some(sample())).unwrap();
        store.save_prefs("s1", &prefs()).unwrap();
        store.remove("s1").unwrap();
        assert!(!dir.path().join("sessions/s1.goal.json").exists());
        assert!(!store.prefs_path("s1").exists());
        // 启动清扫：sub_/task_ 前缀条目的 goal 边车同样要清（否则幽灵文件永久残留）
        store.save_goal("sub_x", &Some(sample())).unwrap();
        store.save_prefs("sub_x", &prefs()).unwrap();
        store
            .upsert_meta(SessionMeta {
                id: "sub_x".into(),
                title: "t".into(),
                workspace: "/w".into(),
                model_id: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: chrono::Utc::now().to_rfc3339(),
                message_count: 0,
                project_id: None,
                roots: vec!["/w".into()],
                running: false,
                interrupted: None,
                last_opened_at: None,
                history_status: None,
            })
            .unwrap();
        assert_eq!(store.purge_non_session_entries(), 1);
        assert!(!dir.path().join("sessions/sub_x.goal.json").exists());
        assert!(!store.prefs_path("sub_x").exists());
    }
}
