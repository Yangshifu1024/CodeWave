//! 分段 append-only JSONL 历史存储（会话保存与恢复优化 · 批2 P1）。
//!
//! **不可违反的不变量**：任何 fallback 路径都只**新增段**，绝不覆盖 / 截断 / 删除既有段——
//! 这是「不丢历史」的底线（上下文压缩、repair 改写、崩溃之后，旧内容在磁盘上永远可回溯）。
//! 唯一的例外是显式删除入口（`SessionStore::remove` / 保留期清理 / 幽灵条目清扫）。
//!
//! 布局：`histories/<id>/0001.jsonl`、`0002.jsonl`…（4 位零填充，序号从 1 起），**明文不压缩**——
//! `grep`/`tail`/手工截断的排障价值是本批收益之一
//!（[docs/session-history-limits](../../../../docs/session-history-limits.md)）。
//!
//! 每个段文件：
//! - 首行 = **头记录** `{"kind":"header","schema":1,"session":"<id>","seq":N,"base":bool,"at":…}`
//!   —— 让**单段可独立解析**（归属与序号自带，不依赖目录名或旁挂索引）；
//! - 其后每行一条记录，`kind` 区分三种：
//!   - `message`：一条消息（复用 [`PersistedMessage`]，图片是 `image_blob` 引用或 `image_inline` 兜底）；
//!   - `compaction`：**时间线重置点**，`head` = 重置后时间线的完整前缀（逐字节还原 wire 上下文用）；
//!   - `seal`：段封口（每段最后一行），记本段 message 记录数 + 段指纹；
//! - `base: true` 的段 = 一次「改写」（承载完整快照）：读了它的 message 记录即重置时间线。
//!
//! 封口规则：一条 message 一行；**按 message 记录数 [`SEGMENT_MAX_MESSAGES`] 或段文件体积
//! [`SEGMENT_MAX_BYTES`] 先触者封口**；**段边界绝不切在 tool_use / tool_result 配对中间**——
//! 配对修复是会话级逻辑，切开会让单段无法独立解析，因此推迟到下一条安全边界。
//!
//! 崩溃语义（AC-4）：加载时**丢弃尾部残行**、跳过坏行、跳过坏段（各记 warn），
//! **绝不因容错而报「历史损坏」**；最坏情况 = 丢最后一个未封口段的尾部（≤1 段）。
//!
//! 写入策略：新建段走 `tmp + rename`（原子创建，崩溃不留半截新段）；向未封口尾段追加走
//! `OpenOptions::append`（真追加、O(增量)，崩溃留下的半行由加载侧丢弃）；段封口与每次保存收尾
//! 各做一次 `sync_all()`（不逐行 fsync）。

use crate::core::sessions::image_blobs;
use crate::core::sessions::persist::{PersistedContent, PersistedMessage};
use crate::core::types::{Content, Message, Role};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 段头 schema 版本（`header.schema`；格式变更时递增，旧段照旧可读）。
pub const SCHEMA_VERSION: u32 = 1;

/// 单段 message 记录数阈值（封口条件之一）。
///
/// 200 条的来源：批2 需求共识 OQ-1「按条数（200）或体积（512KB）先触者切段」。
/// 200 条 ≈ 一次 run 内的完整工具往返序列，单段大致覆盖「最近一段」的首屏渲染（分页粒度 = 1 段）。
pub const SEGMENT_MAX_MESSAGES: usize = 200;

/// 单段文件字节阈值（封口条件之二）。
///
/// 512KB 的来源：同一处需求共识（OQ-1/OQ-2）。明文 JSONL 下 ≈ 十几万 token 量级的转录，
/// 单段仍在 `grep`/`tail`/手工截断都顺手的量级，单次分页传输（1 段）也落在半 MB 级。
pub const SEGMENT_MAX_BYTES: u64 = 512 * 1024;

/// 段文件的一行记录（wire 形态即本枚举的序列化结果：`kind` 在前，其余字段按定义顺序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// 头记录（每段首行）
    Header {
        schema: u32,
        session: String,
        seq: u32,
        /// true = 基线段（承载一份完整快照，是重建时间线的起点）
        base: bool,
        at: String,
    },
    /// 一条消息
    Message { msg: PersistedMessage },
    /// 压缩 / 改写边界（时间线重置点）
    Compaction {
        /// 重置后时间线的**完整前缀**
        head: Vec<PersistedMessage>,
        /// 触发来源：`compact`（上下文压缩）/ `shrink`（repair 等其它前缀缩短）
        #[serde(default)]
        source: String,
        at: String,
    },
    /// 段封口（每段最后一行）
    Seal {
        /// 本段 message **记录**数（不含头 / 压缩 / 封口行）
        messages: usize,
        /// 段指纹（本段时间线贡献的十六进制摘要，仅供校验与排障）
        sig: String,
        at: String,
    },
}

/// 段文件的元信息（P2 分页与排障用；不落盘，读时现算）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentInfo {
    /// 段序号（1 起）
    pub seq: u32,
    /// 文件名（`0001.jsonl`）
    pub file: String,
    /// 是否基线段
    pub base: bool,
    /// 是否有封口记录
    pub sealed: bool,
    /// 本段 message 记录数
    pub messages: usize,
    /// 文件字节数
    pub bytes: u64,
}

/// 时间线重置点（压缩 / 改写边界；展示侧据此画「上下文已压缩」分隔线）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryBoundary {
    /// 所在段序号
    pub seq: u32,
    /// 段内记录序号（0-based）
    pub index: usize,
    /// 触发来源：`compact` / `shrink`
    pub source: String,
    /// 重置前缀条数
    pub head_messages: usize,
    /// 记录时刻（RFC3339）
    pub at: String,
}

/// 历史落盘格式标记（P4/P5 的「清理旧格式历史」入口按它分流）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryFormat {
    /// 段式 JSONL（本批新增）
    New,
    /// 旧 `.json.gz` 单文件（读兼容，不迁移）
    Legacy,
}

/// 未封口的尾段状态（追加时据此决定「继续追加」还是「先封口再开新段」）。
#[derive(Debug, Clone)]
pub struct TailState {
    /// 段序号
    pub seq: u32,
    /// 是否基线段
    pub base: bool,
    /// 本段 message 记录数
    pub messages: usize,
    /// 本段文件字节数（含已缓冲、未刷盘的字节）
    pub bytes: u64,
    /// 本段内仍未配对的 tool_use id（非空 = 不能在此封口）
    pub pending: HashSet<String>,
    /// 本段时间线贡献的指纹（封口时写进 `seal.sig`）
    pub sig: DefaultHasher,
}

impl TailState {
    /// 是否已到封口阈值**且**不在 tool_use / tool_result 配对中间。
    pub fn should_seal(&self) -> bool {
        (self.messages >= SEGMENT_MAX_MESSAGES || self.bytes >= SEGMENT_MAX_BYTES)
            && self.pending.is_empty()
    }
}

/// 增量水位（进程内缓存，随会话；**不落盘**——冷启动可从段文件重算）。
///
/// `written` + `sig` 是「上一次落盘的那份 prepared」的条数与指纹：保存时对
/// `prepared[..written]` 重算同样口径的指纹，一致即可只追加尾部（O(增量)），
/// 不一致就走基线段（新增段，绝不覆盖）。
#[derive(Debug, Clone)]
pub struct SaveState {
    /// 已落盘的、当前时间线上的消息条数
    pub written: usize,
    /// 上述消息的指纹（[`sig_messages`] / [`sig_persisted`] 同口径）
    pub sig: u64,
    /// **磁盘上全部保留段**引用的 blob 并集（不是本次保存的集合）——
    /// 图片 GC 必须用它：旧段仍引用旧 blob，用「本次集合」会删掉它们（不可逆数据丢失）
    pub blobs: HashSet<String>,
    /// 下一个可用的段序号
    pub next_seq: u32,
    /// 未封口的尾段（None = 下一个新增消息需要新开段）
    pub open_tail: Option<TailState>,
}

impl SaveState {
    /// 空水位（从未写过任何段）。
    pub fn empty() -> Self {
        SaveState {
            written: 0,
            sig: sig_persisted(&[]),
            blobs: HashSet::new(),
            next_seq: 1,
            open_tail: None,
        }
    }

    /// 一次写完（增量追加 / 基线段）之后重设水位：条数 + 指纹。
    ///
    /// 由调用方在**写入成功后**统一调用（写入路径不自行维护 `written` / `sig`，
    /// 避免两处状态各自漂移）。
    pub fn reset(&mut self, prepared: &[Message]) {
        self.written = prepared.len();
        self.sig = sig_messages(prepared);
    }
}

/// 一次扫描（读全部段）的结果。
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    /// 水位（保存路径用它做增量判定；load 路径只用 `segments` / `boundaries` / 消息）
    pub state: SaveState,
    /// 段清单（按序号升序）
    pub segments: Vec<SegmentInfo>,
    /// **全部** message 记录（按段序 + 段内序，不做任何裁剪）——展示侧数据源
    pub display: Vec<PersistedMessage>,
    /// 按最后一个重置点重建的时间线——wire 侧数据源（`keep_messages = false` 时为空）
    pub ledger: Vec<PersistedMessage>,
    /// 重置点清单
    pub boundaries: Vec<HistoryBoundary>,
    /// 段文件字节总和
    pub bytes: u64,
    /// 坏段（已跳过）条数
    pub bad_segments: usize,
}

/// 一次落盘的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    /// 本次追加的 message 条数（基线段 = 重置前缀条数）
    pub appended: usize,
    /// 是否写了基线段（时间线重置）
    pub base: bool,
    /// 是否什么都没做（`len == written` 且前缀指纹一致 → 直接返回，不产生任何写）
    pub noop: bool,
    /// 磁盘上全部保留段引用的 blob 并集（GC 判据；`noop` 时为空）
    pub blobs: Vec<String>,
}

impl WriteOutcome {
    /// 无事可做（增量游标命中：不写段、不写索引、不 GC）。
    pub fn nothing() -> Self {
        WriteOutcome {
            appended: 0,
            base: false,
            noop: true,
            blobs: Vec::new(),
        }
    }
}

// ---------- 路径与文件名 ----------

/// 段文件名：`0001.jsonl`（4 位零填充）。
pub fn segment_file_name(seq: u32) -> String {
    format!("{seq:04}.jsonl")
}

/// 从文件名反解段序号（不是段文件名则返回 None）。
pub fn segment_seq_of(name: &str) -> Option<u32> {
    let digits = name.strip_suffix(".jsonl")?;
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// 段目录是否存在（决定走新格式还是旧格式回落）。
pub fn dir_exists(dir: &Path) -> bool {
    dir.is_dir()
}

/// 段文件清单（按序号升序；非段文件名一律忽略）。
pub fn list_segments(dir: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(u32, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let seq = segment_seq_of(&name)?;
            Some((seq, e.path()))
        })
        .collect();
    out.sort_by_key(|(seq, _)| *seq);
    out
}

/// 段目录里的字节总和（熔断判据 / `SaveReport.bytes`）。
pub fn dir_bytes(dir: &Path) -> u64 {
    list_segments(dir)
        .iter()
        .filter_map(|(_, p)| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum()
}

// ---------- 指纹（水位与段校验共用一套口径） ----------

fn feed_role(h: &mut impl Hasher, role: Role) {
    h.write_u8(match role {
        Role::System => 0,
        Role::User => 1,
        Role::Assistant => 2,
        Role::Tool => 3,
    });
}

fn feed_opt_str(h: &mut impl Hasher, s: Option<&str>) {
    match s {
        Some(s) => {
            h.write_u8(1);
            h.write(s.as_bytes());
        }
        None => h.write_u8(0),
    }
}

fn feed_content(h: &mut impl Hasher, tag: u8, a: &str, b: &str) {
    h.write_u8(tag);
    h.write(a.as_bytes());
    h.write_u8(0x1f);
    h.write(b.as_bytes());
}

/// 图片内容的指纹口径（**内存形态与落盘形态共用**）。
///
/// 可外置的图片（base64 长度 ≤ 单图上限）按**内容寻址**折成 blob id——[`image_blobs::blob_id`]
/// 是纯计算（sha256，不读不写 blob 文件），两侧不必真去外置就能算出同一个值；超单图上限的
/// 图片与落盘一致地按内联原文算。
///
/// 为什么要共用同一处：外置**失败**（写盘出错 / owner 非法）时落盘形态是 `image_inline`，
/// 本批之前的旧数据也长这样——若落盘侧按「内联原文」算，两侧指纹就分叉，前缀校验恒失配
///（每次保存都退化成基线段，展示侧还会多出一份重复转录）。
fn feed_image(h: &mut impl Hasher, media_type: &str, data: &str) {
    if data.len() > image_blobs::MAX_DATA_CHARS {
        feed_content(h, 5, media_type, data);
    } else {
        feed_content(h, 6, media_type, &image_blobs::blob_id(data));
    }
}

fn feed_content_memory(h: &mut impl Hasher, c: &Content) {
    match c {
        Content::Text { text } => feed_content(h, 1, text, ""),
        Content::Thinking { text } => feed_content(h, 2, text, ""),
        Content::ToolUse { id, name, args } => {
            feed_content(h, 3, id, name);
            h.write(args.to_string().as_bytes());
        }
        Content::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            feed_content(h, 4, tool_use_id, content);
            h.write_u8(u8::from(*is_error));
        }
        Content::Image { media_type, data } => {
            // 与落盘形态共用同一口径（[`feed_image`]）：**纯计算，不读不写 blob 文件**
            feed_image(h, media_type, data);
        }
    }
}

fn feed_content_persisted(h: &mut impl Hasher, c: &PersistedContent) {
    match c {
        PersistedContent::Text { text } => feed_content(h, 1, text, ""),
        PersistedContent::Thinking { text } => feed_content(h, 2, text, ""),
        PersistedContent::ToolUse { id, name, args } => {
            feed_content(h, 3, id, name);
            h.write(args.to_string().as_bytes());
        }
        PersistedContent::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            feed_content(h, 4, tool_use_id, content);
            h.write_u8(u8::from(*is_error));
        }
        // 内联图片（外置失败 / 旧数据）与内存侧**共用同一口径**：见 [`feed_image`]
        PersistedContent::ImageInline { media_type, data } => feed_image(h, media_type, data),
        PersistedContent::ImageBlob { media_type, blob } => feed_content(h, 6, media_type, blob),
    }
}

/// 一条消息的指纹（内存形态）。
pub fn feed_message(h: &mut impl Hasher, m: &Message) {
    h.write_u8(0xff);
    feed_role(h, m.role);
    feed_opt_str(h, m.created_at.as_deref());
    h.write_u64(m.content.len() as u64);
    for c in &m.content {
        feed_content_memory(h, c);
    }
}

/// 一条消息的指纹（落盘形态）。
pub fn feed_message_persisted(h: &mut impl Hasher, m: &PersistedMessage) {
    h.write_u8(0xff);
    feed_role(h, m.role);
    feed_opt_str(h, m.created_at.as_deref());
    h.write_u64(m.content.len() as u64);
    for c in &m.content {
        feed_content_persisted(h, c);
    }
}

/// 内存消息列表的指纹。
pub fn sig_messages(msgs: &[Message]) -> u64 {
    let mut h = DefaultHasher::new();
    for m in msgs {
        feed_message(&mut h, m);
    }
    h.finish()
}

/// 落盘消息列表的指纹。
pub fn sig_persisted(msgs: &[PersistedMessage]) -> u64 {
    let mut h = DefaultHasher::new();
    for m in msgs {
        feed_message_persisted(&mut h, m);
    }
    h.finish()
}

/// 内存消息**前缀**（`msgs[..len]`）的指纹：增量保存的前缀快照校验。
///
/// 这一步是 O(len) 的**哈希**：CPU 开销、无大分配、无压缩、无整份写盘——比旧实现
/// （整份 serialize + gzip + 覆盖写）便宜一个量级，是刻意的取舍。
/// `len` 超出实际长度时按实际长度截断（不 panic）。
pub fn prefix_sig(msgs: &[Message], len: usize) -> u64 {
    let mut h = DefaultHasher::new();
    for m in msgs.iter().take(len) {
        feed_message(&mut h, m);
    }
    h.finish()
}

/// 一批落盘消息的明文字节数（剥图兜底的判据：**不再与压缩挂钩**）。
pub fn records_bytes(msgs: &[PersistedMessage]) -> usize {
    msgs.iter()
        .map(|m| {
            serde_json::to_string(&Record::Message { msg: m.clone() })
                .map(|s| s.len() + 1)
                .unwrap_or(0)
        })
        .sum()
}

/// 收集一段消息引用的 blob id（GC 并集用）。
pub fn collect_blobs(msgs: &[PersistedMessage], out: &mut HashSet<String>) {
    for m in msgs {
        for c in &m.content {
            if let PersistedContent::ImageBlob { blob, .. } = c {
                out.insert(blob.clone());
            }
        }
    }
}

// ---------- 配对（封口不得切在 tool_use / tool_result 中间） ----------

fn update_pending(pending: &mut HashSet<String>, msg: &PersistedMessage) {
    for c in &msg.content {
        match c {
            PersistedContent::ToolUse { id, .. } => {
                pending.insert(id.clone());
            }
            PersistedContent::ToolResult { tool_use_id, .. } => {
                pending.remove(tool_use_id);
            }
            _ => {}
        }
    }
}

fn pending_of(msgs: &[PersistedMessage]) -> HashSet<String> {
    let mut pending = HashSet::new();
    for m in msgs {
        update_pending(&mut pending, m);
    }
    pending
}

// ---------- 读 ----------

/// 读取量自测钩子（**仅测试编译**）：把「有界装载」这个性质钉成可断言的事实。
///
/// 用 thread-local 而不是全局原子：用例并行跑时全局计数器会把别的用例的读取量算进来，
/// 「读取量远小于全量」这类断言于是变成随机结果；本窗口的读取**不派线程**，整个装载在
/// 本线程内完成，thread-local 的 delta 因此只含本次装载。
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ReadStats {
    /// 整段读取的字节数（[`read_segment`]）
    pub full_bytes: u64,
    /// 整段读取的段数（[`read_segment`]）
    pub full_segments: u64,
    /// 窗口读取的字节数（[`read_window`]：段头 1KB / 段尾 4KB）
    pub window_bytes: u64,
}

#[cfg(test)]
thread_local! {
    static READ_STATS: std::cell::Cell<ReadStats> = const {
        std::cell::Cell::new(ReadStats {
            full_bytes: 0,
            full_segments: 0,
            window_bytes: 0,
        })
    };
}

/// 本线程累计的读取量（测试用）。
#[cfg(test)]
pub(crate) fn read_stats() -> ReadStats {
    READ_STATS.with(std::cell::Cell::get)
}

/// 清零本线程的读取量（测试用）。
#[cfg(test)]
pub(crate) fn reset_read_stats() {
    READ_STATS.with(|c| c.set(ReadStats::default()));
}

#[cfg(test)]
fn note_full_read(bytes: u64) {
    READ_STATS.with(|c| {
        let s = c.get();
        c.set(ReadStats {
            full_bytes: s.full_bytes + bytes,
            full_segments: s.full_segments + 1,
            window_bytes: s.window_bytes,
        });
    });
}

#[cfg(test)]
fn note_window_read(bytes: u64) {
    READ_STATS.with(|c| {
        let s = c.get();
        c.set(ReadStats {
            full_bytes: s.full_bytes,
            full_segments: s.full_segments,
            window_bytes: s.window_bytes + bytes,
        });
    });
}

struct SegRead {
    info: SegmentInfo,
    /// 不含头记录（头记录已解析进 `info`）
    records: Vec<Record>,
}

/// 读一个段文件；返回 `None` = 坏段（读不到 / 头记录非法），调用方跳过该段并记 warn。
///
/// 容错：尾部残行（进程被杀留下的半行）丢弃；非法行跳过；两者都只记 warn、绝不向上报错——
/// 「部分可见」优于「整会话打不开」。
fn read_segment(path: &Path, seq: u32) -> Option<SegRead> {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("历史段读取失败（{}），跳过该段：{e}", path.display());
            return None;
        }
    };
    #[cfg(test)]
    note_full_read(raw.len() as u64);
    let bytes = raw.len() as u64;
    // 非法 UTF-8（含被截断的多字节字符）走 lossy 转换：受影响的那一行解析失败被跳过，
    // 它前面完整的行照常可用
    let text = String::from_utf8_lossy(&raw);
    let file = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut bad_lines = 0usize;
    let mut truncated_tail = false;
    let mut base = false;
    let mut header_seen = false;
    let mut records: Vec<Record> = Vec::new();
    let mut messages = 0usize;
    let mut sealed = false;

    let ends_with_newline = text.ends_with('\n');
    let lines: Vec<&str> = text.split('\n').collect();
    let last_idx = lines.len().saturating_sub(1);
    for (i, raw_line) in lines.iter().enumerate() {
        let is_last = i == last_idx;
        let half_line = is_last && !ends_with_newline;
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            if half_line && !raw_line.is_empty() {
                truncated_tail = true;
            }
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Header {
                schema,
                seq: hseq,
                base: hbase,
                ..
            }) => {
                if header_seen {
                    bad_lines += 1;
                    continue;
                }
                header_seen = true;
                if schema != SCHEMA_VERSION {
                    tracing::warn!(
                        "历史段 schema 版本不符（{}：{schema}），跳过该段（本版本只认 {SCHEMA_VERSION}）",
                        path.display()
                    );
                    return None;
                }
                if hseq != seq {
                    tracing::warn!(
                        "历史段序号与文件名不符（{}：头记录 {hseq} / 文件名 {seq}），跳过该段",
                        path.display()
                    );
                    return None;
                }
                base = hbase;
            }
            Ok(Record::Message { msg }) => {
                messages += 1;
                records.push(Record::Message { msg });
            }
            Ok(rec @ Record::Compaction { .. }) => records.push(rec),
            Ok(rec @ Record::Seal { .. }) => {
                sealed = true;
                records.push(rec);
            }
            Err(_) => {
                if half_line {
                    truncated_tail = true;
                } else {
                    bad_lines += 1;
                }
            }
        }
    }

    if !header_seen {
        tracing::warn!(
            "历史段缺少合法头记录（{}），跳过该段（坏段不影响其余段）",
            path.display()
        );
        return None;
    }
    if bad_lines > 0 {
        tracing::warn!(
            "历史段有 {bad_lines} 行非法记录（{}），已跳过这些行，其余内容照常可用",
            path.display()
        );
    }
    if truncated_tail {
        tracing::warn!(
            "历史段尾部有半行（{}），已丢弃该行（崩溃残留；其余记录完整）",
            path.display()
        );
    }
    Some(SegRead {
        info: SegmentInfo {
            seq,
            file,
            base,
            sealed,
            messages,
            bytes,
        },
        records,
    })
}

/// 扫描一个历史目录（会话历史或子代理历史）。
///
/// `keep_messages = false`：只累计计数 / 指纹 / blob 并集 / 尾段状态（保存路径用，省内存）；
/// `keep_messages = true`：另外产出 `display`（全部 message 记录）与 `ledger`（时间线基线）。
pub fn scan(dir: &Path, keep_messages: bool) -> ScanOutcome {
    let mut state = SaveState::empty();
    let mut segments: Vec<SegmentInfo> = Vec::new();
    let mut display: Vec<PersistedMessage> = Vec::new();
    let mut ledger: Vec<PersistedMessage> = Vec::new();
    let mut boundaries: Vec<HistoryBoundary> = Vec::new();
    let mut bad_segments = 0usize;
    let mut bytes = 0u64;
    let mut timeline_hasher = DefaultHasher::new();
    let mut open_tail: Option<TailState> = None;

    for (seq, path) in list_segments(dir) {
        // 坏段也占序号：先推进 next_seq，保证后续新建段不会覆盖磁盘上的既有文件名
        state.next_seq = state.next_seq.max(seq + 1);
        let Some(seg) = read_segment(&path, seq) else {
            bad_segments += 1;
            continue;
        };
        bytes += seg.info.bytes;
        // 基线段 = 时间线重置点：先清空时间线（条数 / 指纹 / 基线），再回放本段的 message 记录。
        // 带 `compaction` 记录的基线段会在回放时再重置一次（重复无害）；
        // `blobs` 与 `display` **不重置**（并集与完整转录覆盖全部保留段）。
        if seg.info.base {
            timeline_hasher = DefaultHasher::new();
            state.written = 0;
            if keep_messages {
                ledger.clear();
            }
        }
        for rec in seg.records.iter() {
            match rec {
                Record::Header { .. } | Record::Seal { .. } => {}
                Record::Message { msg } => {
                    collect_blobs(std::slice::from_ref(msg), &mut state.blobs);
                    feed_message_persisted(&mut timeline_hasher, msg);
                    state.written += 1;
                    if keep_messages {
                        display.push(msg.clone());
                        ledger.push(msg.clone());
                    }
                }
                Record::Compaction { head, .. } => {
                    // 时间线重置：条数 / 指纹 / blob 引用全部以 head 为基准重算
                    //（边界清单在段末统一取，与逐段读共用 [`boundaries_of`] 的口径）
                    timeline_hasher = DefaultHasher::new();
                    for m in head {
                        feed_message_persisted(&mut timeline_hasher, m);
                        collect_blobs(std::slice::from_ref(m), &mut state.blobs);
                    }
                    state.written = head.len();
                    if keep_messages {
                        ledger = head.clone();
                    }
                }
            }
        }
        // 段内边界按记录顺序追加（两条读路径同口径）
        boundaries.extend(boundaries_of(seq, &seg.records));
        // 段内「时间线贡献」（条数 / 配对残留 / 段指纹）与 `open_tail_at` 共用同一套口径，
        // 两处不各写一份、不漂移
        let contrib = segment_contribution(&seg.records);
        if let Some(Record::Seal { messages, sig, .. }) = seg
            .records
            .iter()
            .find(|r| matches!(r, Record::Seal { .. }))
        {
            let actual = format!("{:016x}", contrib.sig.clone().finish());
            if sig != &actual {
                tracing::warn!(
                    "历史段封口指纹不符（{}）：封口记录 {sig} / 实算 {actual}（内容按实际记录使用）",
                    path.display()
                );
            }
            if messages != &contrib.messages {
                tracing::warn!(
                    "历史段封口条数不符（{}）：封口记录 {messages} / 实算 {}",
                    path.display(),
                    contrib.messages
                );
            }
        }
        if seg.info.sealed {
            open_tail = None;
        } else {
            open_tail = Some(TailState {
                seq,
                base: seg.info.base,
                messages: seg.info.messages,
                bytes: seg.info.bytes,
                pending: contrib.pending,
                sig: contrib.sig,
            });
        }
        segments.push(seg.info);
    }

    state.sig = timeline_hasher.finish();
    state.open_tail = open_tail;
    ScanOutcome {
        state,
        segments,
        display,
        ledger,
        boundaries,
        bytes,
        bad_segments,
    }
}

// ---------- 按段分页读（批2 P2：首屏 / 加载更早） ----------
//
// 这一节是分页的**读侧**：只读需要的段文件与段尾，绝不为了「首屏」「往前翻一段」去整段扫描
//（`scan` 是全量回放：它要重建时间线，代价与历史总量成正比）。

/// 段尾读取窗口：封口行很短（`kind` + 条数 + 16 位十六进制指纹 + 时刻，约 120 字节），
/// 4KB 足够；文件更长时只读尾部这 4KB，超窗只会退化成整段解析，**不会读错**。
const TAIL_READ_WINDOW: u64 = 4096;

/// 段头读取窗口（头记录约 120 字节）：用于「该段是否可读」的廉价判定。
const HEAD_READ_WINDOW: u64 = 1024;

/// 读文件的一段窗口（`from_end = true` 时取尾部 `len` 字节；文件不足则整份）。
fn read_window(path: &Path, from_end: bool, len: u64) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let take = size.min(len) as usize;
    if take == 0 {
        return Some(Vec::new());
    }
    let start = if from_end { size - take as u64 } else { 0 };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = vec![0u8; take];
    f.read_exact(&mut buf).ok()?;
    #[cfg(test)]
    note_window_read(take as u64);
    Some(buf)
}

/// 段头是否合法（schema 版本 + 序号与文件名一致）——与 [`read_segment`] 同一判据，
/// 但**只读开头 1KB**：分页元信息（段数 / 总条数）不该为此解析整段。
fn header_ok(path: &Path, seq: u32) -> bool {
    let Some(buf) = read_window(path, false, HEAD_READ_WINDOW) else {
        return false;
    };
    let text = String::from_utf8_lossy(&buf);
    let Some(first) = text.split('\n').next() else {
        return false;
    };
    match serde_json::from_str::<Record>(first.trim_end_matches('\r')) {
        Ok(Record::Header {
            schema, seq: hseq, ..
        }) => schema == SCHEMA_VERSION && hseq == seq,
        // 文件为空 / 头记录被截断 / 不是头记录：与「缺合法头记录」同类，按坏段处理
        _ => false,
    }
}

/// 段尾的封口记录里记的 message 条数（`None` = 尾部不可信 / 没有封口行）。
///
/// 尾部半行（进程被杀留下的残行）与空文件一律返回 `None`，由调用方回退整段解析——宁慢不错。
fn tail_seal_messages(path: &Path) -> Option<usize> {
    let buf = read_window(path, true, TAIL_READ_WINDOW)?;
    if buf.is_empty() || !buf.ends_with(b"\n") {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    let last = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .rfind(|l| !l.is_empty())?;
    match serde_json::from_str::<Record>(last) {
        Ok(Record::Seal { messages, .. }) => Some(messages),
        _ => None,
    }
}

/// 段文件的 message 记录数（**display 口径**，与 [`scan`] 一致）。
///
/// 读取顺序：头记录校验（坏段记 0，与 `scan` 的「跳过坏段」同一口径）→ **尾读封口行**
///（不解析整段）→ 拿不到封口行（未封口 / 尾部半行）才回退整段解析。
pub fn message_count(path: &Path, seq: u32) -> usize {
    if !header_ok(path, seq) {
        return 0;
    }
    if let Some(n) = tail_seal_messages(path) {
        return n;
    }
    read_segment(path, seq)
        .map(|s| s.info.messages)
        .unwrap_or(0)
}

/// 段目录里是否存在**可读的消息记录**（新格式是否真有内容）。
///
/// 读路径的权威判据（[`SessionStore::reads_new_format`]）与清理入口
///（`cleanup::has_new_format_history`）**同源**：只看「段目录存在」不够——空目录 / 全是坏段 /
/// 只有头与封口的段都不算「有历史」。两者并存时若不查内容，用户会看到一个**空**会话
///（数据其实还在旧 `.json.gz` 里），而清理入口又保守地不肯删旧文件。
///
/// 代价有界：从**最新段往前**找，常见情形（尾段有内容）只读一个段（段头 1KB + 段尾 4KB）；
/// 尾段未封口时退化为整段解析（那一个段）；全部坏段时退化为整目录的窗口读。
pub fn has_readable_messages(dir: &Path) -> bool {
    list_segments(dir)
        .into_iter()
        .rev()
        .any(|(seq, path)| message_count(&path, seq) > 0)
}

/// 段目录摘要（P2 首屏分页元信息）。
///
/// **只读目录项、文件元数据与段尾**（每段至多 4KB + 1KB），不整段解析——
/// 首屏的元信息开销因此与历史总量无关。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirSummary {
    /// 段文件总数（含坏段：它仍占一个序号与一份磁盘占用）
    pub segment_count: usize,
    /// 磁盘上的 message 记录总数（display 口径；坏段记 0，与 `scan` 一致）
    pub total_messages: usize,
    /// 段文件字节总和
    pub bytes: u64,
    /// 最大段序号（None = 一个段文件都没有）
    pub last_seq: Option<u32>,
}

/// 汇总一个段目录（见 [`DirSummary`]）。
pub fn summarize(dir: &Path) -> DirSummary {
    let segs = list_segments(dir);
    let mut out = DirSummary {
        segment_count: segs.len(),
        ..DirSummary::default()
    };
    for (seq, path) in &segs {
        out.bytes += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        out.total_messages += message_count(path, *seq);
        // `list_segments` 按序号升序 → 循环结束即最大序号
        out.last_seq = Some(*seq);
    }
    out
}

/// 是否还存在**可读**且早于 `seq` 的段（分页终态判定：false = 已到最早）。
///
/// 判据是**头记录**（每段只读开头 1KB），不是「文件名存在」：更早只剩坏段时如实报 false，
/// 界面才不会一直挂着一个点了没反应的「加载更早」。代价上也不需整段解析——
/// 坏段照旧只影响它自己。
pub fn has_readable_segments_before(dir: &Path, seq: u32) -> bool {
    list_segments(dir)
        .into_iter()
        .any(|(s, path)| s < seq && header_ok(&path, s))
}

/// 单段读取结果（P2 分页数据源）。
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentRead {
    /// 段元信息（序号 / 文件名 / 基线段 / 封口 / 条数 / 字节数）
    pub info: SegmentInfo,
    /// 本段的 `message` 记录（**display 口径**：不 trim，不含压缩边界里的 `head`）
    pub messages: Vec<PersistedMessage>,
    /// 本段的压缩 / 改写边界（`compaction` 记录；只含本段）
    pub boundaries: Vec<HistoryBoundary>,
}

/// 从段记录里取出 message 记录（顺序不变）——`compaction` / `seal` 都不是展示内容。
fn messages_of(records: Vec<Record>) -> Vec<PersistedMessage> {
    records
        .into_iter()
        .filter_map(|r| match r {
            Record::Message { msg } => Some(msg),
            _ => None,
        })
        .collect()
}

/// 从段记录里取出压缩 / 改写边界（`seq` = 该段序号，`index` = 段内记录序号，0-based、不含头记录）。
///
/// `scan` 与逐段读（[`read_segment_at`]）**共用**它：同一个段在两条路径上的边界必须逐字段一致，
/// 否则「首屏边界」与「全量边界」会对不上。
fn boundaries_of(seq: u32, records: &[Record]) -> Vec<HistoryBoundary> {
    records
        .iter()
        .enumerate()
        .filter_map(|(i, rec)| match rec {
            Record::Compaction { head, source, at } => Some(HistoryBoundary {
                seq,
                index: i,
                source: source.clone(),
                head_messages: head.len(),
                at: at.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// 读一个段文件（`None` = 文件不存在 / 坏段）。**不触碰其它段**。
pub fn read_segment_at(dir: &Path, seq: u32) -> Option<SegmentRead> {
    let path = dir.join(segment_file_name(seq));
    if !path.is_file() {
        return None;
    }
    let seg = read_segment(&path, seq)?;
    let boundaries = boundaries_of(seq, &seg.records);
    Some(SegmentRead {
        info: seg.info,
        messages: messages_of(seg.records),
        boundaries,
    })
}

/// 按段向前读的结果（批2 P2 分页）：本次返回的段 + 本次走过的范围内的边界与坏段数。
#[derive(Debug, Clone, PartialEq)]
pub struct PageRead {
    /// 本次返回的段（`None` = 该侧已无可读内容：不存在 / 全是坏段 / 只有空段）
    pub segment: Option<SegmentRead>,
    /// 本次覆盖段范围内的压缩边界（**含向前翻找时跳过的空段**：边界指向「此段之后的内容」，
    /// 调用方按 `seq` 与已加载项对齐；坏段读不到内容，自然没有边界）
    pub boundaries: Vec<HistoryBoundary>,
    /// 本次向前翻找时**跳过**的坏段数（0 = 全部可读）
    pub bad_segments: usize,
}

/// 按段向前读：`before = None` 取最新段，`Some(seq)` 取**严格早于 `seq`** 的最近一段。
///
/// 坏段与「只有头 / 边界记录、没有任何消息」的段一律跳过继续向前：**坏段不影响其余段**，
/// 且页面永远有内容，不会让调用方拿到一堆空页空转；跳过的坏段数如实计入 [`PageRead::bad_segments`]
///（UI 据此提示「部分历史段损坏，已跳过」），全无可读内容时也照样报出来。
pub fn read_page(dir: &Path, before: Option<u32>) -> PageRead {
    let mut out = PageRead {
        segment: None,
        boundaries: Vec::new(),
        bad_segments: 0,
    };
    for (seq, _path) in list_segments(dir).into_iter().rev() {
        if before.is_some_and(|b| seq >= b) {
            continue;
        }
        match read_segment_at(dir, seq) {
            Some(seg) => {
                out.boundaries.extend(seg.boundaries.iter().cloned());
                if seg.messages.is_empty() {
                    continue;
                }
                out.segment = Some(seg);
                return out;
            }
            None => {
                out.bad_segments += 1;
                tracing::warn!(
                    "历史段 {seq} 不可读，已跳过（坏段只影响它自己，更早内容仍可继续加载）"
                );
            }
        }
    }
    out
}

/// 段的「时间线贡献」：段内 message 记录数（压缩边界后重算）、配对残留（未配对的
/// `tool_use`）与段指纹（`seal.sig` 同口径）。
///
/// `scan` 的每段回放与有界尾读（[`open_tail_at`]）**共用**它：封口校验、`open_tail`
/// 的状态都由同一处口径得出，两处不各写一份、不漂移。
struct SegmentContribution {
    /// 段内 message 记录数（`compaction` 记录之后重算）
    messages: usize,
    /// 仍未配对的 `tool_use` id（非空 = 不能在此封口）
    pending: HashSet<String>,
    /// 段指纹
    sig: DefaultHasher,
}

fn segment_contribution(records: &[Record]) -> SegmentContribution {
    let mut out = SegmentContribution {
        messages: 0,
        pending: HashSet::new(),
        sig: DefaultHasher::new(),
    };
    for rec in records {
        match rec {
            Record::Message { msg } => {
                out.messages += 1;
                update_pending(&mut out.pending, msg);
                feed_message_persisted(&mut out.sig, msg);
            }
            Record::Compaction { head, .. } => {
                // 压缩边界：本段的时间线贡献以 head 为基准重算
                out.messages = 0;
                out.pending = pending_of(head);
                out.sig = DefaultHasher::new();
                for m in head {
                    feed_message_persisted(&mut out.sig, m);
                }
            }
            Record::Header { .. } | Record::Seal { .. } => {}
        }
    }
    out
}

// ---------- 有界尾读（批2 回归修复：打开 / 恢复会话不再 O(全部历史字节)） ----------
//
// 落盘取消 8MB 上限后，wire 装载若整份回放（[`scan`]）就成了 O(全部历史字节)——历史越长，
// 打开会话越慢，而「大会话秒开」正是本批要保住的东西。这里把 wire 装载改成
// **只从尾部向前读到够用为止**：
//
// 1. 起点取**重置点之后的时间线**（压缩 `head` / 基线段起点 / 目录起点）：更早的内容与 wire 无关；
// 2. 读到「窗口粗估 token ≥ 门槛 + **≥ 3 轮** + 起点落在最老的 User 上 + 窗口内没有孤儿
//    tool_result」即停（[`WireWindow::ready`]）；
// 3. 后处理仍是**同一套** `repair` + `trim`（在 store 侧），并对窗口再做一次精确判据
//    （窗口自身超预算 + ≥ 2 轮）——等价性推导见 `store.rs::load_history_wire` 的注释。

/// 落盘形态的 token 估算（口径与 `util::token_est::est_tokens_message` 一致：图片落盘是 blob
/// 引用、内存是 base64，两边都记每图 1600）。
///
/// 它只服务**廉价判据**（「读够了没有」）：估小了多读一点、估大了由 store 侧的精确判据
///（对转换后的内存形态求和）兜回来——两头都不影响正确性，因此不追求逐字节同口径。
fn est_tokens_persisted(msg: &PersistedMessage) -> u64 {
    use crate::util::token_est::est_tokens_text;
    let mut total = 8; // role overhead（与 est_tokens_message 对齐）
    for c in &msg.content {
        total += match c {
            PersistedContent::Text { text } | PersistedContent::Thinking { text } => {
                est_tokens_text(text)
            }
            PersistedContent::ToolUse { name, args, .. } => {
                est_tokens_text(name) + est_tokens_text(&args.to_string())
            }
            PersistedContent::ToolResult { content, .. } => est_tokens_text(content),
            PersistedContent::ImageInline { .. } | PersistedContent::ImageBlob { .. } => 1600,
        };
    }
    total
}

/// 段尾有界读取窗口：从最后一个段向前读，[`WireWindow::ready`] 命中即停。
pub struct WireWindow {
    /// 待读段（**降序**：最新在前）
    pending: Vec<(u32, PathBuf)>,
    /// `pending` 里下一个待读下标
    cursor: usize,
    /// 已读窗口（**逆序**：最新在前）
    rev: Vec<PersistedMessage>,
    /// 已到时间线重置点（压缩边界 / 基线段起点 / 目录起点）——
    /// 此时窗口**就是**完整 wire 时间线，不再裁剪、直接走全量同路
    reset: bool,
    /// 廉价判据门槛（token 粗估）
    min_tokens: u64,
    /// 窗口 token 粗估
    approx: u64,
    /// 窗口内 User 消息数（起点必为 User，故与轮数相等）
    users: usize,
    /// 跳过的坏段数（与 `scan` 同口径：坏段只影响它自己）
    bad_segments: usize,
}

impl WireWindow {
    /// 打开窗口：读入最新一段（段目录为空时 [`Self::at_reset`] 立即为 true）。
    pub fn open(dir: &Path, min_tokens: u64) -> Self {
        let mut pending = list_segments(dir);
        pending.reverse();
        let mut w = WireWindow {
            pending,
            cursor: 0,
            rev: Vec::new(),
            reset: false,
            min_tokens,
            approx: 0,
            users: 0,
            bad_segments: 0,
        };
        w.extend();
        w
    }

    /// 是否已到时间线重置点（含「没有更早的段了」）：true ⇒ [`Self::window`] 即完整时间线。
    pub fn at_reset(&self) -> bool {
        self.reset
    }

    /// 窗口 token 粗估（[`Self::require_tokens`] 的入参由它算出）。
    pub fn approx_tokens(&self) -> u64 {
        self.approx
    }

    /// 抬高廉价判据门槛（精确判据发现 `repair` 削掉的量多于粗估时用：要求多读一点，
    /// 免得每读一段就重跑一次 `repair`）。
    pub fn require_tokens(&mut self, min_tokens: u64) {
        self.min_tokens = self.min_tokens.max(min_tokens);
    }

    /// 跳过的坏段数（与 `scan` 的 `bad_segments` 同口径）。
    pub fn bad_segments(&self) -> usize {
        self.bad_segments
    }

    /// 廉价判据 + 边界安全（后者是 O(窗口)，故只在前两条命中时才跑）。
    ///
    /// **「≥ 3 轮」不是随手取的数**：只有「窗口自身超预算 **且** ≥ 3 轮」才能保证全量那份
    /// `trim` **至少丢掉一轮**（它的条件正是「总量超预算且轮数 > keep_last = 2」）。丢掉至少
    /// 一轮，才会把「窗口之前的轮次」以及时间线开头可能存在的非 User 前缀一并丢干净，窗口上
    /// 算出的保留后缀才与全量逐条相同（推导见 `store.rs::load_history_wire`）。
    pub fn ready(&self) -> bool {
        if self.reset {
            return true;
        }
        self.approx >= self.min_tokens && self.users >= 3 && self.boundary_safe()
    }

    /// 窗口起点是否安全：
    ///
    /// - 最老的一条是 **User**（轮边界）——更早那条消息绝不可能正在与后续 `tool_result`
    ///   配对，`repair` 的配对不变量因此不会被截断改变语义；
    /// - 窗口内**没有引用窗口外 `tool_use` 的孤儿 `tool_result`**——那种结果会被窗口内的
    ///   `repair` 删掉、而全量的不会（两者一旦不同，`trim` 的后缀就可能不一致）。
    fn boundary_safe(&self) -> bool {
        let Some(i) = self.rev.iter().rposition(|m| m.role == Role::User) else {
            return false;
        };
        // `rev` 最新在前 ⇒ `rev[..=i]` 的最老一条正是 rev[i]（User）
        let win = &self.rev[..=i];
        let mut uses: HashSet<&str> = HashSet::new();
        for m in win {
            for c in &m.content {
                if let PersistedContent::ToolUse { id, .. } = c {
                    uses.insert(id.as_str());
                }
            }
        }
        win.iter().all(|m| {
            m.content.iter().all(|c| match c {
                PersistedContent::ToolResult { tool_use_id, .. } => {
                    uses.contains(tool_use_id.as_str())
                }
                _ => true,
            })
        })
    }

    /// 当前窗口（时间线顺序）。未到重置点时起点落在**最老的 User** 上（轮边界安全）；
    /// 到重置点时原样给出——重置前缀必须**完整纳入**，wire 的还原语义依赖它。
    pub fn window(&self) -> Vec<PersistedMessage> {
        if self.reset {
            return self.rev.iter().rev().cloned().collect();
        }
        match self.rev.iter().rposition(|m| m.role == Role::User) {
            Some(i) => self.rev[..=i].iter().rev().cloned().collect(),
            None => Vec::new(),
        }
    }

    /// 再往前读一段；返回 false = 没有更早的段（此后 [`Self::at_reset`] 为 true）。
    pub fn extend(&mut self) -> bool {
        while let Some((seq, path)) = self.pending.get(self.cursor).cloned() {
            self.cursor += 1;
            let Some(seg) = read_segment(&path, seq) else {
                // 坏段跳过（与 `scan` 同口径）：它自己的内容读不到，其余段照常
                self.bad_segments += 1;
                continue;
            };
            let mut hit_reset = false;
            // 记录**逆序**回放：更早的段读进来时，它的最新记录接在 `rev` 的尾部
            for rec in seg.records.iter().rev() {
                match rec {
                    Record::Header { .. } | Record::Seal { .. } => {}
                    Record::Message { msg } => {
                        if msg.role == Role::User {
                            self.users += 1;
                        }
                        self.approx += est_tokens_persisted(msg);
                        self.rev.push(msg.clone());
                    }
                    Record::Compaction { head, .. } => {
                        // 时间线重置：更早的内容一律与 wire 无关，但 **head 必须完整纳入**。
                        // `rev` 最新在前 ⇒ head 逆序接在**尾部**（它比已读到的记录都老）
                        for m in head.iter().rev() {
                            self.rev.push(m.clone());
                        }
                        self.users += head.iter().filter(|m| m.role == Role::User).count();
                        self.approx += head.iter().map(est_tokens_persisted).sum::<u64>();
                        hit_reset = true;
                        break;
                    }
                }
            }
            if hit_reset || seg.info.base || self.cursor >= self.pending.len() {
                // 压缩边界 / 基线段起点 = 时间线起点；「目录里的段已读完」同理——三种情况都让
                // 窗口等于**完整**时间线（此后不再裁剪、直接走全量同路）
                self.reset = true;
            }
            return true;
        }
        // 没有更早的段了：窗口即完整时间线
        self.reset = true;
        false
    }
}

// ---------- 写 ----------

fn record_line(rec: &Record) -> anyhow::Result<Vec<u8>> {
    let mut s = serde_json::to_string(rec)?;
    s.push('\n');
    Ok(s.into_bytes())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 追加写入器：把记录攒成批量字节落到未封口尾段；必要时封口并开新段。
struct SegmentWriter<'a> {
    dir: &'a Path,
    session: &'a str,
    state: &'a mut SaveState,
    buf: Vec<u8>,
}

impl<'a> SegmentWriter<'a> {
    fn new(dir: &'a Path, session: &'a str, state: &'a mut SaveState) -> Self {
        SegmentWriter {
            dir,
            session,
            state,
            buf: Vec::new(),
        }
    }

    fn tail_path(&self) -> Option<PathBuf> {
        self.state
            .open_tail
            .as_ref()
            .map(|t| self.dir.join(segment_file_name(t.seq)))
    }

    /// 确保有未封口的尾段（没有就新建）。
    fn ensure_open(&mut self, base: bool) -> anyhow::Result<()> {
        if self.state.open_tail.is_some() {
            return Ok(());
        }
        let seq = self.state.next_seq;
        self.state.next_seq = seq + 1;
        let header = Record::Header {
            schema: SCHEMA_VERSION,
            session: self.session.to_string(),
            seq,
            base,
            at: now_rfc3339(),
        };
        let bytes = record_line(&header)?;
        let path = self.dir.join(segment_file_name(seq));
        std::fs::create_dir_all(self.dir)?;
        // 新建段走 tmp + rename（原子）：崩溃不会留下「有内容但没头记录」的半截新段
        crate::util::atomic::atomic_write(&path, &bytes)?;
        self.state.open_tail = Some(TailState {
            seq,
            base,
            messages: 0,
            bytes: bytes.len() as u64,
            pending: HashSet::new(),
            sig: DefaultHasher::new(),
        });
        Ok(())
    }

    /// 追加一条 message 记录（记账：条数 / 配对 / 段指纹 / 字节）。
    fn push_message(&mut self, msg: &PersistedMessage) -> anyhow::Result<()> {
        let line = record_line(&Record::Message { msg: msg.clone() })?;
        let len = line.len() as u64;
        self.buf.extend_from_slice(&line);
        if let Some(t) = self.state.open_tail.as_mut() {
            t.bytes += len;
            t.messages += 1;
            update_pending(&mut t.pending, msg);
            feed_message_persisted(&mut t.sig, msg);
        }
        Ok(())
    }

    /// 追加一条 compaction 记录（该段的「时间线贡献」= head，故指纹与配对按 head 记）。
    fn push_compaction(&mut self, head: &[PersistedMessage], source: &str) -> anyhow::Result<()> {
        let rec = Record::Compaction {
            head: head.to_vec(),
            source: source.to_string(),
            at: now_rfc3339(),
        };
        let line = record_line(&rec)?;
        let len = line.len() as u64;
        self.buf.extend_from_slice(&line);
        if let Some(t) = self.state.open_tail.as_mut() {
            t.bytes += len;
            t.messages = 0;
            t.sig = DefaultHasher::new();
            for m in head {
                feed_message_persisted(&mut t.sig, m);
            }
            t.pending = pending_of(head);
        }
        Ok(())
    }

    /// 把缓冲刷进尾段文件（真追加，O(增量)）。
    fn flush(&mut self) -> anyhow::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        if self.state.open_tail.is_none() {
            // 防御：有缓冲却没有可写段（正常路径不可达，push 前必 ensure_open）——
            // 临时开一个段写掉，**绝不静默丢字节**
            self.ensure_open(false)?;
        }
        let Some(path) = self.tail_path() else {
            anyhow::bail!("段写入器状态异常：有缓冲字节却没有可写段");
        };
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)?;
        f.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }

    /// 段封口：**先把缓冲落到本段** → 取走尾段状态 → 追加封口行 → `sync_all()`。
    ///
    /// 顺序不能反：`flush()` 要靠 `open_tail` 定位文件，先 take 再 flush 会拿不到路径
    /// 而把整段缓冲丢掉（曾因此丢过整段消息）。
    fn seal(&mut self) -> anyhow::Result<()> {
        if self.state.open_tail.is_none() {
            return Ok(());
        }
        self.flush()?;
        let Some(tail) = self.state.open_tail.take() else {
            return Ok(());
        };
        let path = self.dir.join(segment_file_name(tail.seq));
        let line = record_line(&Record::Seal {
            messages: tail.messages,
            sig: format!("{:016x}", tail.sig.clone().finish()),
            at: now_rfc3339(),
        })?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)?;
        f.write_all(&line)?;
        // 封口即落稳（AC-4：崩溃最多丢最后一个未封口段的尾部）
        f.sync_all()?;
        Ok(())
    }

    /// 保存收尾：刷缓冲 + 尾段 `sync_all()`（不逐行 fsync；检查点频率低，代价可忽略）。
    fn finish(&mut self) -> anyhow::Result<()> {
        self.flush()?;
        if let Some(path) = self.tail_path() {
            // Windows：`sync_all`（FlushFileBuffers）要求句柄带**写**权限，只读句柄会报
            // 「拒绝访问（os error 5）」；这里以写权限打开但不截断、不追加，纯为落盘。
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
                f.sync_all()?;
            }
        }
        Ok(())
    }
}

/// 增量追加：把 `additions`（已转成落盘形态）写进段目录。
///
/// 常态路径只追加新字节：**既有段文件的已有部分逐字节不变**（不重写、不覆盖、不截断）。
pub fn append_messages(
    dir: &Path,
    session: &str,
    state: &mut SaveState,
    additions: &[PersistedMessage],
) -> anyhow::Result<()> {
    if additions.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let mut w = SegmentWriter::new(dir, session, state);
    for msg in additions {
        w.ensure_open(false)?;
        if w.state.open_tail.as_ref().is_some_and(|t| t.should_seal()) {
            w.seal()?;
            w.ensure_open(false)?;
        }
        w.push_message(msg)?;
        // blob 并集随写入累计（GC 判据 = 磁盘上全部保留段引用的并集）
        let mut refs = HashSet::new();
        collect_blobs(std::slice::from_ref(msg), &mut refs);
        w.state.blobs.extend(refs);
    }
    w.finish()?;
    Ok(())
}

/// 基线段（时间线重置点）：**只新增段，绝不覆盖 / 截断 / 删除既有段**。
///
/// - `compaction = Some(source)`：上下文压缩 / 前缀缩短造成的时间线重置 → 段内只放**一条**
///   `compaction` 记录（`head` = 完整的重置前缀）。既落盘了新的 wire 基线，也留下边界锚点；
/// - `compaction = None`：其它改写（例如 repair 原地修改了已有消息）→ 段内是 `message` 记录。
///
/// 两种形态在读取侧都表现为「时间线在此重置」，wire 重建规则统一。
pub fn write_base_segment(
    dir: &Path,
    session: &str,
    state: &mut SaveState,
    msgs: &[PersistedMessage],
    compaction: Option<&str>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut w = SegmentWriter::new(dir, session, state);
    // 先给未写完的尾段封口：它承载的时间线已被本次重置取代，但内容必须完整保留在磁盘上
    if w.state.open_tail.is_some() {
        w.seal()?;
    }
    w.ensure_open(true)?;
    match compaction {
        Some(source) => w.push_compaction(msgs, source)?,
        None => {
            for msg in msgs {
                if w.state.open_tail.as_ref().is_some_and(|t| t.should_seal()) {
                    w.seal()?;
                    w.ensure_open(false)?;
                }
                w.push_message(msg)?;
            }
        }
    }
    w.finish()?;
    drop(w);
    collect_blobs(msgs, &mut state.blobs);
    Ok(())
}

// ---------- 水位边车（有界冷启动） ----------
//
// 冷启动（本进程还没有该会话的水位缓存）原本要 `scan(dir, false)` 整目录回放才能拿到
// `{written, sig, blobs}`——同样是 O(全部历史字节)。这里改成一个段目录内的小边车：
// 命中即用（读一个小文件 + 必要时尾读**一个**段），对不上就回退整目录扫描。

/// 水位边车文件名（在段目录内，且**不是**段文件名：`list_segments` 只认 `NNNN.jsonl`）。
pub const META_FILE: &str = ".segmeta.json";

/// 边车 schema 版本（形状变更时递增；不符即回退全量扫描）。
pub const META_SCHEMA: u32 = 1;

/// 段目录的水位边车（`histories/<id>/.segmeta.json`）。
///
/// **为什么不让 `seal` 记录携带累积水位**：末段通常**未封口**（根本没有 seal 可读），要覆盖
/// 它就得把「时间线指纹」换成可序列化续算的流式哈希（[`sig_persisted`] 的口径随之改变），
/// 而且 blob 并集会被写进**每一个** seal——段文件随图片数量膨胀、尾部 4KB 窗口假设随之失效
/// （图 GC 的判据恰恰是本批最不能出错的一环）。边车把同一份信息放在**一处**（小文件、一次
/// 原子写），形状与危险面都小得多。
///
/// 它是**缓存而不是真相**：[`SegmentMeta::matches_dir`] 用「段集合自写下本文件以来逐字节未变」
/// 的廉价证据判定可用性——append-only 下任何一次追加都会改动段文件个数 / 最大序号 /
/// 未封口尾段的字节数，三项里必有一项对不上；对不上就回退整目录扫描（慢但一定对）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentMeta {
    /// 边车 schema（形状变更时递增；不符即回退全量扫描）
    #[serde(default)]
    pub schema: u32,
    /// 水位：当前时间线上的消息条数（`SaveState::written` 同口径）
    pub written: usize,
    /// 水位：当前时间线的指纹（`sig_messages` / `sig_persisted` 同口径）
    pub sig: u64,
    /// 磁盘上**全部保留段**引用的 blob 并集（图片 GC 判据；漏一个就是不可逆的图丢失）
    pub blobs: Vec<String>,
    /// 校验：段文件个数（含坏段）
    pub segment_count: usize,
    /// 校验：最大段序号（0 = 无段）
    pub last_seq: u32,
    /// 校验：未封口尾段序号（0 = 无未封口尾段）
    pub tail_seq: u32,
    /// 校验：未封口尾段字节数（`tail_seq` 为 0 时无意义）
    pub tail_bytes: u64,
}

impl SegmentMeta {
    /// 边车是否与段目录现状相符（判据见类型级注释的「廉价证据」）。
    pub fn matches_dir(&self, dir: &Path) -> bool {
        if self.schema != META_SCHEMA {
            return false;
        }
        let segs = list_segments(dir);
        if self.segment_count != segs.len() {
            return false;
        }
        let last = segs.last().map(|(s, _)| *s).unwrap_or(0);
        if self.last_seq != last {
            return false;
        }
        if self.tail_seq == 0 {
            // 末段已封口：写入侧封口后只新建段、不再往它里面追加，故此处的「个数 + 最大
            // 序号」一致即无从变化
            return true;
        }
        // 未封口尾段：字节数必须一模一样（任何一次追加都会改变它）
        self.tail_seq == last
            && std::fs::metadata(dir.join(segment_file_name(self.tail_seq)))
                .map(|m| m.len() == self.tail_bytes)
                .unwrap_or(false)
    }
}

/// 边车路径。
pub fn meta_path(dir: &Path) -> PathBuf {
    dir.join(META_FILE)
}

/// 写水位边车（原子写；由 `SessionStore` 在段写入成功后调用）。
///
/// 校验字段直接取自磁盘事实（段清单 + 尾段长度）：它们是下次冷启动判定「这份边车还作不作数」
/// 的全部依据，绝不能与段文件的实际状态脱节。
///
/// `blobs` 排序后落盘：并集是集合语义，排序只为让同一个集合产生同一份字节（便于对拍与排障）。
pub fn write_meta(dir: &Path, state: &SaveState) -> anyhow::Result<()> {
    let segs = list_segments(dir);
    let mut blobs: Vec<String> = state.blobs.iter().cloned().collect();
    blobs.sort();
    let meta = SegmentMeta {
        schema: META_SCHEMA,
        written: state.written,
        sig: state.sig,
        blobs,
        segment_count: segs.len(),
        last_seq: segs.last().map(|(s, _)| *s).unwrap_or(0),
        tail_seq: state.open_tail.as_ref().map(|t| t.seq).unwrap_or(0),
        tail_bytes: state.open_tail.as_ref().map(|t| t.bytes).unwrap_or(0),
    };
    crate::util::atomic::atomic_write(&meta_path(dir), &serde_json::to_vec(&meta)?)?;
    Ok(())
}

/// 读水位边车（缺失 / 损坏 → None，调用方回退全量扫描）。
pub fn read_meta(dir: &Path) -> Option<SegmentMeta> {
    let raw = std::fs::read(meta_path(dir)).ok()?;
    match serde_json::from_slice::<SegmentMeta>(&raw) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::debug!(
                "历史水位边车无法解析（{}）：{e}（回退整目录扫描）",
                meta_path(dir).display()
            );
            None
        }
    }
}

/// **有界**推导水位：边车命中即用（只读一个小文件 + 必要时尾读**一个**段）；
/// 边车缺失 / 与磁盘不符 / 尾段读不出 → None（调用方回退 [`scan`] 整目录扫描）。
pub fn state_from_meta(dir: &Path) -> Option<SaveState> {
    let meta = read_meta(dir)?;
    if !meta.matches_dir(dir) {
        tracing::debug!(
            "历史水位边车与段目录不符（{}）：回退整目录扫描",
            dir.display()
        );
        return None;
    }
    let mut state = SaveState::empty();
    state.next_seq = meta.last_seq + 1;
    state.written = meta.written;
    state.sig = meta.sig;
    state.blobs = meta.blobs.iter().cloned().collect();
    if meta.tail_seq != 0 {
        // 未封口尾段要读出来才能接着追加（段级有界：一个段 ≤ SEGMENT_MAX_BYTES 量级）
        state.open_tail = Some(open_tail_at(dir, meta.tail_seq)?);
    }
    Some(state)
}

/// 未封口尾段状态（`None` = 段不存在 / 坏段 / 已封口）。
///
/// 与 [`scan`] 同口径：`messages` / `bytes` 取段自身信息，配对残留与段指纹取
/// [`segment_contribution`]（压缩边界按 `head` 重设）。
pub fn open_tail_at(dir: &Path, seq: u32) -> Option<TailState> {
    let path = dir.join(segment_file_name(seq));
    let seg = read_segment(&path, seq)?;
    if seg.info.sealed {
        return None;
    }
    let contrib = segment_contribution(&seg.records);
    Some(TailState {
        seq,
        base: seg.info.base,
        messages: seg.info.messages,
        bytes: seg.info.bytes,
        pending: contrib.pending,
        sig: contrib.sig,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sessions::SessionStore;
    use serde_json::json;

    fn store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (SessionStore::new(dir.path().to_path_buf()), dir)
    }

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![Content::Text { text: text.into() }],
            created_at: None,
        }
    }

    /// wire 形态钉死：字段名与 `kind` 取值是磁盘契约（排障要靠文本编辑器直读），改动即破坏兼容。
    #[test]
    fn record_wire_shape_is_pinned() {
        let header = Record::Header {
            schema: 1,
            session: "s1".into(),
            seq: 1,
            base: true,
            at: "2026-09-24T00:00:00+00:00".into(),
        };
        let s = serde_json::to_string(&header).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"header","schema":1,"session":"s1","seq":1,"base":true,"at":"2026-09-24T00:00:00+00:00"}"#
        );
        assert_eq!(serde_json::from_str::<Record>(&s).unwrap(), header);

        let m = Record::Message {
            msg: PersistedMessage {
                role: Role::User,
                content: vec![PersistedContent::Text { text: "hi".into() }],
                created_at: None,
            },
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"message","msg":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#
        );

        let c = Record::Compaction {
            head: vec![],
            source: "compact".into(),
            at: "2026-09-24T00:00:00+00:00".into(),
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"compaction","head":[],"source":"compact","at":"2026-09-24T00:00:00+00:00"}"#
        );

        let seal = Record::Seal {
            messages: 3,
            sig: "00ff".into(),
            at: "2026-09-24T00:00:00+00:00".into(),
        };
        let s = serde_json::to_string(&seal).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"seal","messages":3,"sig":"00ff","at":"2026-09-24T00:00:00+00:00"}"#
        );
    }

    /// 段序号 ↔ 文件名：只认 4 位零填充的 `.jsonl`。
    #[test]
    fn segment_file_name_round_trip() {
        assert_eq!(segment_file_name(1), "0001.jsonl");
        assert_eq!(segment_file_name(1234), "1234.jsonl");
        assert_eq!(segment_seq_of("0001.jsonl"), Some(1));
        assert_eq!(segment_seq_of("0999.jsonl"), Some(999));
        for bad in [
            "0001.json.gz",
            "1.jsonl",
            "00001.jsonl",
            "000a.jsonl",
            "index.json",
            ".jsonl",
        ] {
            assert_eq!(segment_seq_of(bad), None, "{bad} 不是段文件名");
        }
    }

    /// 🔴 增量判定成立的前提：内存形态与落盘形态的指纹**同口径**。
    /// 图片尤其关键（内存是 base64、落盘是 blob 引用），不同口径会让每次检查点都退化成
    /// 整份改写（基线段），而且链路完全静默。
    #[test]
    fn memory_and_persisted_sigs_agree_for_every_content_kind() {
        let (store, _dir) = store();
        let msgs = vec![
            msg(Role::User, "q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking { text: "想".into() },
                    Content::Text { text: "答".into() },
                    Content::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        args: json!({"files": ["a.png"]}),
                    },
                ],
                created_at: Some("2026-01-01T00:00:00+00:00".into()),
            },
            Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "ok".into(),
                is_error: true,
            }]),
            Message {
                role: Role::User,
                content: vec![Content::Image {
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                }],
                created_at: None,
            },
        ];
        let (persisted, referenced) =
            crate::core::sessions::persist::to_persisted(&store, "s1", &msgs);
        assert_eq!(referenced.len(), 1, "图片已外置");
        assert_eq!(
            sig_messages(&msgs),
            sig_persisted(&persisted),
            "内存与落盘形态必须同指纹（否则增量保存恒退化成整份改写）"
        );
        // 内容变了 → 指纹必须变（否则会漏掉真正的改动）
        let mut changed = msgs.clone();
        changed[1].content[1] = Content::Text {
            text: "改了".into(),
        };
        assert_ne!(sig_messages(&msgs), sig_messages(&changed));
        // 前缀同口径同理
        assert_eq!(prefix_sig(&msgs, 2), sig_persisted(&persisted[..2]));
    }

    /// 半行 / 坏行 / 坏段三种容错：都不报错、坏段跳过、其余内容完整。
    #[test]
    fn read_segment_tolerates_half_line_bad_line_and_bad_header() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // 正常段 + 尾部半行
        let a = d.join("0001.jsonl");
        std::fs::write(
            &a,
            concat!(
                "{\"kind\":\"header\",\"schema\":1,\"session\":\"s1\",\"seq\":1,\"base\":true,\"at\":\"t\"}\n",
                "{\"kind\":\"message\",\"msg\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"a\"}]}}\n",
                "{\"kind\":\"message\",\"msg\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"b\"}]\n"
            ),
        )
        .unwrap();
        let seg = read_segment(&a, 1).expect("有合法头记录即为可读段");
        assert_eq!(seg.info.messages, 1, "半行被丢弃，完整记录保留");
        assert!(!seg.info.sealed);

        // 坏行（非末尾）跳过；完整行照常可用
        let b = d.join("0002.jsonl");
        std::fs::write(
            &b,
            concat!(
                "{\"kind\":\"header\",\"schema\":1,\"session\":\"s1\",\"seq\":2,\"base\":false,\"at\":\"t\"}\n",
                "{ 这不是 JSON }\n",
                "{\"kind\":\"message\",\"msg\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"c\"}]}}\n",
                "{\"kind\":\"seal\",\"messages\":1,\"sig\":\"x\",\"at\":\"t\"}\n"
            ),
        )
        .unwrap();
        let seg = read_segment(&b, 2).expect("坏行不影响整段");
        assert_eq!(seg.info.messages, 1);
        assert!(seg.info.sealed);

        // 坏段：头记录缺失 / 序号不符 / schema 不符 → 跳过该段
        let c = d.join("0003.jsonl");
        std::fs::write(
            &c,
            "{\"kind\":\"message\",\"msg\":{\"role\":\"user\",\"content\":[]}}\n",
        )
        .unwrap();
        assert!(read_segment(&c, 3).is_none(), "缺头记录 = 坏段");
        let e = d.join("0004.jsonl");
        std::fs::write(
            &e,
            "{\"kind\":\"header\",\"schema\":1,\"session\":\"s1\",\"seq\":9,\"base\":false,\"at\":\"t\"}\n",
        )
        .unwrap();
        assert!(read_segment(&e, 4).is_none(), "序号不符 = 坏段");
        let f = d.join("0005.jsonl");
        std::fs::write(
            &f,
            "{\"kind\":\"header\",\"schema\":99,\"session\":\"s1\",\"seq\":5,\"base\":false,\"at\":\"t\"}\n",
        )
        .unwrap();
        assert!(read_segment(&f, 5).is_none(), "schema 不符 = 坏段");

        // 扫描：坏段跳过并计数，其余段内容完整；next_seq 仍推进到最大序号之后
        let scan = scan(d, true);
        assert_eq!(scan.bad_segments, 3);
        assert_eq!(scan.state.written, 2);
        assert_eq!(scan.state.next_seq, 6, "坏段也占序号，绝不覆盖既有文件名");
        assert_eq!(scan.segments.len(), 2);
    }

    fn persisted(n: usize) -> Vec<PersistedMessage> {
        (0..n)
            .map(|i| PersistedMessage {
                role: Role::User,
                content: vec![PersistedContent::Text {
                    text: format!("m{i}"),
                }],
                created_at: None,
            })
            .collect()
    }

    /// 分页元信息：只读元数据 + 段尾（不解析整段），口径与 `scan` 一致；坏段记 0。
    #[test]
    fn summarize_and_message_count_match_scan() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut state = SaveState::empty();
        append_messages(d, "s1", &mut state, &persisted(250)).unwrap();

        // 250 条 → 段1 满 200 条封口、段2 装余下 50 条（未封口，走整段解析兜底）
        let summary = summarize(d);
        assert_eq!(summary.segment_count, 2);
        assert_eq!(summary.last_seq, Some(2));
        assert_eq!(summary.total_messages, 250);
        assert_eq!(summary.bytes, dir_bytes(d));
        assert_eq!(scan(d, true).display.len(), 250, "摘要口径与 scan 一致");
        assert!(has_readable_segments_before(d, 2));
        assert!(!has_readable_segments_before(d, 1));

        // 尾部半行（进程被杀留下）：尾读不可信 → 回退整段解析，半行不进本段消息
        let seg2 = d.join("0002.jsonl");
        let mut raw = std::fs::read_to_string(&seg2).unwrap();
        raw.push_str("{\"kind\":\"message\",\"msg\":{\"role\":\"u");
        std::fs::write(&seg2, raw).unwrap();
        assert_eq!(summarize(d).total_messages, 250);
        let page = read_page(d, None);
        let seg = page.segment.expect("最新段可读");
        assert_eq!(
            (seg.info.seq, seg.messages.len()),
            (2, 50),
            "半行不进本段消息"
        );

        // 段 1 去掉头记录 = 坏段：scan 与摘要都跳过它，段 2 照常可读
        let seg1 = d.join("0001.jsonl");
        let body: String = std::fs::read_to_string(&seg1)
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&seg1, body).unwrap();
        let scan = scan(d, true);
        assert_eq!(scan.bad_segments, 1);
        assert_eq!(scan.display.len(), 50);
        let summary = summarize(d);
        assert_eq!(summary.total_messages, 50, "坏段不计入总数（与 scan 一致）");
        assert_eq!(summary.segment_count, 2, "坏段仍占一个段文件");
        let page = read_page(d, Some(2));
        assert!(
            page.segment.is_none(),
            "更早只剩坏段：跳过它并如实报「没有更早内容」"
        );
        assert_eq!(page.bad_segments, 1, "跳过的坏段数如实上报（界面据此提示）");
        assert!(
            !has_readable_segments_before(d, 2),
            "更早只剩坏段：不得让界面挂着一个点了没反应的「加载更早」"
        );
    }

    /// 按段向前读：`before` 语义（None = 最新段 / Some = 严格更早）、空段跳过。
    #[test]
    fn read_page_walks_back_and_skips_empty_segments() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut state = SaveState::empty();
        append_messages(d, "s1", &mut state, &persisted(250)).unwrap();
        // 手写一个「只有头 + 封口、没有任何消息记录」的段（真实的压缩基线段就是这种形状）
        std::fs::write(
            d.join("0003.jsonl"),
            concat!(
                "{\"kind\":\"header\",\"schema\":1,\"session\":\"s1\",\"seq\":3,\"base\":true,\"at\":\"t\"}\n",
                "{\"kind\":\"seal\",\"messages\":0,\"sig\":\"00\",\"at\":\"t\"}\n"
            ),
        )
        .unwrap();

        let summary = summarize(d);
        assert_eq!(summary.segment_count, 3);
        assert_eq!(summary.last_seq, Some(3));
        assert_eq!(summary.total_messages, 250, "空段贡献 0 条");

        // 空段被跳过：页面永远有内容
        let page = read_page(d, None);
        let seg = page.segment.expect("跳过空段后仍有可读内容");
        assert_eq!((seg.info.seq, seg.messages.len()), (2, 50));
        assert_eq!(page.bad_segments, 0, "空段不是坏段");
        let page = read_page(d, Some(2));
        let seg = page.segment.expect("再往前一段");
        assert_eq!((seg.info.seq, seg.messages.len()), (1, 200));
        assert!(read_page(d, Some(1)).segment.is_none(), "已到最早");
        assert!(!has_readable_segments_before(d, 1));
    }

    // ---------- 有界尾读（批2 回归修复） ----------

    /// 够用即停：窗口远小于全量、不读到目录起点，起点落在**最老的 User**上（轮边界安全）。
    #[test]
    fn wire_window_stops_early_and_starts_at_round_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // 每条 ≈1.1k token（4096 字符）→ 按条数封口：12 段 × 200 条
        let big = "y".repeat(4096);
        let all: Vec<PersistedMessage> = (0..2400)
            .map(|i| PersistedMessage {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![PersistedContent::Text {
                    text: format!("m{i} {big}"),
                }],
                created_at: None,
            })
            .collect();
        let mut state = SaveState::empty();
        append_messages(d, "s1", &mut state, &all).unwrap();
        assert!(
            list_segments(d).len() >= 12,
            "本用例要足够多的段（实际 {}）",
            list_segments(d).len()
        );

        let mut w = WireWindow::open(d, 256 * 1024);
        while !w.ready() && !w.at_reset() {
            w.extend();
        }
        assert!(w.ready() && !w.at_reset(), "够用即停，不该读到目录起点");
        assert!(w.approx_tokens() >= 256 * 1024, "粗估必须已达门槛");
        assert_eq!(w.bad_segments(), 0);

        let win = w.window();
        assert!(
            win.len() * 2 < all.len(),
            "窗口应远小于全量：{} / {}",
            win.len(),
            all.len()
        );
        assert_eq!(win[0].role, Role::User, "起点必须是 User（轮边界）");
        assert_eq!(
            win,
            all[all.len() - win.len()..].to_vec(),
            "窗口必须是全量的后缀（逐条相同）"
        );
    }

    /// 压缩边界：`head` 必须**完整**纳入，且接在已读到的更新记录**之前**（顺序不能变）。
    #[test]
    fn wire_window_keeps_compaction_head_whole_and_ordered() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut state = SaveState::empty();
        let head: Vec<PersistedMessage> = (0..5)
            .map(|i| PersistedMessage {
                role: Role::User,
                content: vec![PersistedContent::Text {
                    text: format!("h{i}"),
                }],
                created_at: None,
            })
            .collect();
        write_base_segment(d, "s1", &mut state, &head, Some("compact")).unwrap();
        // 压缩后继续对话（250 条 → 按条数封口，切出后续段，逼真走「向前读到重置点」那条路）
        let after: Vec<PersistedMessage> = (0..250)
            .map(|i| PersistedMessage {
                role: Role::User,
                content: vec![PersistedContent::Text {
                    text: format!("a{i}"),
                }],
                created_at: None,
            })
            .collect();
        append_messages(d, "s1", &mut state, &after).unwrap();

        // 门槛开到不可能达到：只能靠「读到重置点」收尾
        let mut w = WireWindow::open(d, u64::MAX);
        while !w.at_reset() {
            w.extend();
        }
        let win = w.window();
        let texts: Vec<String> = win
            .iter()
            .map(|m| match &m.content[0] {
                PersistedContent::Text { text } => text.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        let mut want: Vec<String> = (0..5).map(|i| format!("h{i}")).collect();
        want.extend((0..250).map(|i| format!("a{i}")));
        assert_eq!(texts, want, "重置前缀必须完整纳入且顺序正确");
        assert!(w.ready(), "到重置点即 ready（窗口 = 完整时间线）");
    }

    /// 坏段与 `scan` 同口径：跳过它、其余段照常可读，`bad_segments` 如实计数。
    #[test]
    fn wire_window_skips_bad_segment_like_scan() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut state = SaveState::empty();
        append_messages(d, "s1", &mut state, &persisted(3)).unwrap();
        // 坏段（缺合法头记录）：序号更大 → 读窗口先撞上它
        std::fs::write(d.join("0002.jsonl"), "{ not json }\n").unwrap();
        let w = WireWindow::open(d, 0);
        assert_eq!(w.bad_segments(), 1);
        assert_eq!(w.window().len(), 3, "坏段被跳过，其余段照常");
        assert_eq!(scan(d, false).state.written, 3, "与 scan 同口径");
    }

    /// 水位边车：形状 / 校验判据 / 与全量扫描同值 / 失效时判不可用。
    #[test]
    fn meta_round_trip_matches_scan_and_rejects_stale_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mem: Vec<Message> = (0..5).map(|i| msg(Role::User, &format!("m{i}"))).collect();
        let mut state = SaveState::empty();
        append_messages(d, "s1", &mut state, &persisted(5)).unwrap();
        state.reset(&mem);
        write_meta(d, &state).unwrap();

        let meta = read_meta(d).expect("边车应可读");
        assert!(meta.matches_dir(d));
        assert_eq!(
            (
                meta.written,
                meta.segment_count,
                meta.last_seq,
                meta.tail_seq
            ),
            (5, 1, 1, 1)
        );

        let from_meta = state_from_meta(d).expect("边车应可用");
        let scanned = scan(d, false).state;
        assert_eq!(
            (from_meta.written, from_meta.sig, from_meta.next_seq),
            (scanned.written, scanned.sig, scanned.next_seq)
        );
        assert_eq!(from_meta.blobs, scanned.blobs);
        let tail = |s: &SaveState| {
            s.open_tail
                .as_ref()
                .map(|t| (t.seq, t.base, t.messages, t.bytes, t.pending.clone()))
        };
        assert_eq!(tail(&from_meta), tail(&scanned));

        // 尾部多出半行（崩溃残留）：字节数变了 → 必须判不可用
        let p = d.join("0001.jsonl");
        let mut raw = std::fs::read(&p).unwrap();
        raw.extend_from_slice(b"{\"kind\":\"mess");
        std::fs::write(&p, raw).unwrap();
        assert!(
            !read_meta(d).unwrap().matches_dir(d),
            "字节数变了必须判不可用（否则水位会拿旧值去比对前缀）"
        );
        assert!(state_from_meta(d).is_none());

        // 新增段（个数变了）→ 同样不可用
        std::fs::write(d.join("0002.jsonl"), b"{ not json }\n").unwrap();
        assert!(state_from_meta(d).is_none());
    }

    /// 末段已封口（`tail_seq == 0`）时校验只靠「段个数 + 最大序号」两项——也够用：封口之后
    /// 写入侧只新建段、绝不再往它里追加。（真实路径下封口发生在「下一次追加」之前，故末段通常
    /// 仍带未封口尾段，这种形态只能手工构造。）
    #[test]
    fn meta_with_sealed_tail_validates_by_counts_only() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut raw = String::from(
            "{\"kind\":\"header\",\"schema\":1,\"session\":\"s1\",\"seq\":1,\"base\":false,\"at\":\"t\"}\n",
        );
        for i in 0..2 {
            raw.push_str(&format!(
                "{{\"kind\":\"message\",\"msg\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"m{i}\"}}]}}}}\n"
            ));
        }
        raw.push_str("{\"kind\":\"seal\",\"messages\":2,\"sig\":\"x\",\"at\":\"t\"}\n");
        std::fs::write(d.join("0001.jsonl"), raw).unwrap();

        let scanned = scan(d, false).state;
        assert!(scanned.open_tail.is_none(), "末段已封口 → 无未封口尾段");
        write_meta(d, &scanned).unwrap();
        let meta = read_meta(d).unwrap();
        assert_eq!((meta.tail_seq, meta.tail_bytes), (0, 0));
        assert!(meta.matches_dir(d), "只有个数 / 序号作证也应判可用");

        let from_meta = state_from_meta(d).expect("边车应可用");
        assert!(from_meta.open_tail.is_none());
        assert_eq!(
            (from_meta.written, from_meta.sig, from_meta.next_seq),
            (scanned.written, scanned.sig, scanned.next_seq)
        );

        // 新增段（个数变了）→ 不可用
        std::fs::write(d.join("0002.jsonl"), b"{ not json }\n").unwrap();
        assert!(state_from_meta(d).is_none());
    }
}
