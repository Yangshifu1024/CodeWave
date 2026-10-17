//! 范围版本与新鲜度（任务 #15 拥有）。
//!
//! 一次检查的证据只在**它所观察的那份内容**上成立：文件改了，旧证据就是 stale
//! （[#15](../../../../docs/tools-and-safety.md)）。本模块是这套判定的唯一实现，
//! 禁止任何任务另写一份"判断有没有变"的逻辑。
//!
//! 三条硬规则：
//!
//! 1. **禁止 mtime-only / HEAD-only**：只比 `git status` 或 HEAD 得不出"内容是否相同"，
//!    同长度同 mtime 的内容变化必须能检出——所以摘要走内容 SHA-256。
//! 2. **读不全就说读不全**：无法读取、symlink 越出读取根、目录枚举在采集期间变化、
//!    命令前后输入漂移，一律 `completeness = incomplete/drifted`，**空哈希不等于"没变"**。
//! 3. **只采集显式范围**：不把整个用户主目录拖进哈希，也不靠 watcher 猜测变更；
//!    性能优化留到有界性测过之后。

use super::types::{PathRef, SCHEMA, VerificationScope};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 采集完整性（共同契约 §5）。只有 [`Complete`](Self::Complete) 的版本能支撑"通过"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionCompleteness {
    /// 全部声明范围成功采集。
    Complete,
    /// 部分条目读不到 / 枚举不稳定 / symlink 越界。
    Incomplete,
    /// 只读检查期间输入发生变化（前后两次采集不一致）。
    Drifted,
}

impl VersionCompleteness {
    /// 是否可支撑有效通过。
    pub fn is_valid(self) -> bool {
        matches!(self, VersionCompleteness::Complete)
    }
    /// wire 串。
    pub fn as_str(self) -> &'static str {
        match self {
            VersionCompleteness::Complete => "complete",
            VersionCompleteness::Incomplete => "incomplete",
            VersionCompleteness::Drifted => "drifted",
        }
    }
}

/// 条目种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// 常规文件。
    File,
    /// 目录（只记存在与枚举稳定性，不逐文件内容）。
    Directory,
    /// 符号链接（记目标，不跟随越界）。
    Symlink,
    /// 已删除（基线存在、当前不存在）。
    Missing,
}

/// 单个被观察条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// 所属根。
    pub root_id: String,
    /// 根内相对路径（规范化、平台无关分隔形式）。
    pub relative_path: String,
    /// 种类。
    pub kind: EntryKind,
    /// 内容摘要（目录 / 缺失 / 读不到时为 `None`——**不写空串冒充"内容为空"**）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// 字节数（仅文件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 符号链接目标（仅 `Symlink`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    /// 是否可读；`false` 时 `kind` / `sha256` 不可信。
    pub readable: bool,
}

impl VersionEntry {
    /// 排序键：`(root_id, relative_path)`——规范化根 + 相对路径的字典序，
    /// 保证同一范围在任何机器、任何枚举顺序下摘要一致。
    pub fn sort_key(&self) -> (&str, &str) {
        (self.root_id.as_str(), self.relative_path.as_str())
    }
}

/// 环境事实项（仅记录 `CheckSpec` 明确列出的工具链版本与非秘密特征哈希，**不保存完整 env**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionFact {
    /// 事实键（如 `rustc` / `node`）。
    pub key: String,
    /// 观测值（版本串或特征哈希）。
    pub value: String,
}

/// 一次范围采集的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeVersion {
    /// schema 版本（随 [`SCHEMA`]，便于后续算法升级后区分旧判据）。
    pub schema: u32,
    /// 全范围摘要（对排序后的条目 + 工具链事实计算）。
    pub digest: String,
    /// 条目（按 `sort_key` 升序）。
    #[serde(default)]
    pub entries: Vec<VersionEntry>,
    /// 工具链事实。
    #[serde(default)]
    pub toolchain: Vec<VersionFact>,
    /// 完整性。
    pub completeness: VersionCompleteness,
}

impl ScopeVersion {
    /// 范围是否可支撑有效通过。
    pub fn is_valid(&self) -> bool {
        self.completeness.is_valid()
    }
    /// 按 `root_id + relative_path` 查条目。
    pub fn entry(&self, root_id: &str, rel: &str) -> Option<&VersionEntry> {
        self.entries
            .iter()
            .find(|e| e.root_id == root_id && e.relative_path == rel)
    }
    /// 声明范围内是否只有这批条目发生变化（供 stale 原因定位）。
    pub fn changed_against(&self, other: &ScopeVersion) -> Vec<PathRef> {
        let mut out: Vec<PathRef> = Vec::new();
        let (left, right) = if self.entries.len() <= other.entries.len() {
            (self, other)
        } else {
            (other, self)
        };
        // 先按 key 对齐再比，避免顺序不同造成假差异。
        let left_map: BTreeMap<(&str, &str), &VersionEntry> =
            left.entries.iter().map(|e| (e.sort_key(), e)).collect();
        let right_map: BTreeMap<(&str, &str), &VersionEntry> =
            right.entries.iter().map(|e| (e.sort_key(), e)).collect();
        for (k, e) in &left_map {
            if let Some(other) = right_map.get(k) {
                if !entry_equivalent(e, other) {
                    out.push(PathRef {
                        root_id: k.0.to_string(),
                        relative_path: k.1.to_string(),
                    });
                }
            } else {
                out.push(PathRef {
                    root_id: k.0.to_string(),
                    relative_path: k.1.to_string(),
                });
            }
        }
        for (k, e) in &right_map {
            if !left_map.contains_key(k) {
                out.push(PathRef {
                    root_id: k.0.to_string(),
                    relative_path: k.1.to_string(),
                });
            }
            let _ = e;
        }
        out.sort_by(|a, b| {
            (a.root_id.as_str(), a.relative_path.as_str())
                .cmp(&(b.root_id.as_str(), b.relative_path.as_str()))
        });
        out.dedup();
        out
    }
}

/// 两条记录在"同一内容"意义上是否相等。`readable=false` 一律判不等价——
/// 读不到就承认不知道，不能因为两边都没读出来而说"相同"。
fn entry_equivalent(a: &VersionEntry, b: &VersionEntry) -> bool {
    if !a.readable || !b.readable {
        return false;
    }
    a.kind == b.kind
        && a.sha256 == b.sha256
        && a.size == b.size
        && a.symlink_target == b.symlink_target
}

/// 新鲜度判定（[#15](../../../../docs/tools-and-safety.md)）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Freshness {
    /// 相关范围未变，证据可用。
    Fresh,
    /// 相关范围已变，证据失效（派生验收状态，**不覆写原始执行记录**）。
    Stale {
        /// 发生变化的路径（最多 [`MAX_REPORTED_CHANGES`] 条）。
        changed: Vec<PathRef>,
    },
    /// 采集本身不可信（不完整 / 漂移），不能判定新鲜。
    Unknown {
        /// 原因。
        reason: String,
    },
}

impl Freshness {
    /// 是否可支撑有效通过。
    pub fn is_fresh(&self) -> bool {
        matches!(self, Freshness::Fresh)
    }
}

/// 报告变化路径的条数上限（避免把大范围差异灌进 UI / 模型上下文）。
pub const MAX_REPORTED_CHANGES: usize = 50;

/// 采集上限（共同契约 §5 的有界要求；触顶记 `incomplete` 而非静默截断）。
pub const MAX_ENTRIES: usize = 20_000;
/// 单文件参与哈希的字节上限；超限记 `incomplete`（不哈希一半再声称"相同"）。
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// 采集参数。
#[derive(Debug, Clone, Default)]
pub struct CaptureOptions {
    /// 取消令牌（长目录采集要能被打断）。
    pub cancel: Option<tokio_util::sync::CancellationToken>,
}

/// 根解析器：`root_id` → 真实路径。由宿主从 `SessionMeta.roots` 快照构造，
/// 契约与回执只带 `root_id`，绝不带可信绝对路径（[#12](../../../../docs/tools-and-safety.md)）。
pub type RootResolver = BTreeMap<String, PathBuf>;

/// 采集失败的可定位原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    /// 稳定码。
    pub code: &'static str,
    /// 说明。
    pub message: String,
}

fn cap_err(code: &'static str, message: impl Into<String>) -> CaptureError {
    CaptureError {
        code,
        message: message.into(),
    }
}

/// 采集一次范围版本。
///
/// `scope` 决定看什么（显式 roots/inputs/outputs/dependency_files），`roots` 决定怎么看
/// （受控解析，不信任入参绝对路径）。返回的 `ScopeVersion` 即使 `completeness != Complete`
/// 也有真实条目——它要告诉验收器"哪里没读成"，而不是塌成一个空摘要。
pub fn capture_scope(
    roots: &RootResolver,
    scope: &VerificationScope,
    opts: &CaptureOptions,
) -> Result<ScopeVersion, CaptureError> {
    let mut entries: BTreeMap<(String, String), VersionEntry> = BTreeMap::new();
    let mut completeness = VersionCompleteness::Complete;
    let mut problems: Vec<String> = Vec::new();
    let mut touched = 0usize;

    // 只读根（防 symlink 越界）：所有 relative_path 必须落在其声明根内。
    let read_roots: Vec<PathBuf> = roots.values().cloned().collect();
    let host_roots = roots
        .get("data_dir")
        .map(|data| crate::tools::pathutil::WriteRoots {
            workspace: roots.get("workspace").unwrap_or(data).clone(),
            data_dir: data.clone(),
            project_dir: roots.get("project_dir").cloned(),
            extra: Vec::new(),
        });
    let ignored = |path: &Path| {
        host_roots.as_ref().is_some_and(|host| {
        matches!(crate::core::protected_state::check_direct_write(host, path), Err((code, _)) if code == crate::core::protected_state::ERROR_CODE)
            || path.starts_with(host.data_dir.join("logs"))
            || host.project_dir.as_ref().is_some_and(|project| path.starts_with(project.join("logs")))
    })
    };

    let mut targets: Vec<PathRef> = Vec::new();
    for group in [&scope.inputs, &scope.outputs, &scope.dependency_files] {
        for p in group {
            targets.push(p.clone());
        }
    }

    let mut position = 0;
    while position < targets.len() {
        let t = targets[position].clone();
        position += 1;
        if opts.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            return Err(cap_err("E_CAPTURE_CANCELLED", "范围采集被取消"));
        }
        touched += 1;
        if touched > MAX_ENTRIES {
            completeness = VersionCompleteness::Incomplete;
            problems.push(format!("声明条目超过上限 {MAX_ENTRIES}，未全部采集"));
            break;
        }
        let Some(root) = roots.get(&t.root_id) else {
            completeness = VersionCompleteness::Incomplete;
            problems.push(format!("未知根 `{}`", t.root_id));
            continue;
        };
        // 规范化相对路径：与写路径共用 safe_join 语义（拒绝对路径 / `..` / 控制字符）。
        let joined = match crate::tools::pathutil::safe_join(root, &t.relative_path) {
            Ok(p) => p,
            Err((code, msg)) => {
                completeness = VersionCompleteness::Incomplete;
                problems.push(format!("路径非法 `{}`（{code}: {msg}）", t.relative_path));
                continue;
            }
        };
        let entry = capture_one(&t.root_id, &t.relative_path, &joined, &read_roots, &ignored);
        // **只按 readable 判完整性**：`Missing` 与 `Directory` 本来就没有内容摘要，
        // 拿"无 sha256"当读不到会把「文件被删了」误判成采集失败（删除声明因此永远不成立）。
        if !entry.readable {
            completeness = VersionCompleteness::Incomplete;
            problems.push(entry.reason_if_any());
        }
        if entry.kind == EntryKind::Directory {
            match std::fs::read_dir(&joined) {
                Ok(children) => {
                    for child in children {
                        match child {
                            Ok(child) => {
                                if ignored(&child.path()) {
                                    continue;
                                }
                                if targets.len() >= MAX_ENTRIES {
                                    completeness = VersionCompleteness::Incomplete;
                                    break;
                                }
                                let relative_path = std::path::Path::new(&t.relative_path)
                                    .join(child.file_name())
                                    .to_string_lossy()
                                    .replace('\\', "/");
                                let child = PathRef {
                                    root_id: t.root_id.clone(),
                                    relative_path,
                                };
                                if !targets.contains(&child) {
                                    targets.push(child);
                                }
                            }
                            Err(_) => completeness = VersionCompleteness::Incomplete,
                        }
                    }
                }
                Err(_) => completeness = VersionCompleteness::Incomplete,
            }
        }
        entries.insert((t.root_id.clone(), t.relative_path.clone()), entry);
    }

    // 目录型范围要重复枚举确认稳定；不稳定 = incomplete。
    for group in [&scope.inputs, &scope.outputs, &scope.dependency_files] {
        for t in group {
            if opts.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                return Err(cap_err("E_CAPTURE_CANCELLED", "范围采集被取消"));
            }
            let Some(root) = roots.get(&t.root_id) else {
                continue;
            };
            let Ok(joined) = crate::tools::pathutil::safe_join(root, &t.relative_path) else {
                continue;
            };
            let Ok(p1) = std::fs::read_dir(&joined) else {
                continue;
            };
            let mut first: BTreeSet<String> = BTreeSet::new();
            for e in p1.flatten() {
                if ignored(&e.path()) {
                    continue;
                }
                first.insert(e.file_name().to_string_lossy().into_owned());
            }
            let Ok(p2) = std::fs::read_dir(&joined) else {
                completeness = VersionCompleteness::Incomplete;
                problems.push(format!("目录 `{}` 二次枚举失败", t.relative_path));
                continue;
            };
            let mut second: BTreeSet<String> = BTreeSet::new();
            for e in p2.flatten() {
                if ignored(&e.path()) {
                    continue;
                }
                second.insert(e.file_name().to_string_lossy().into_owned());
            }
            if first != second {
                completeness = VersionCompleteness::Incomplete;
                problems.push(format!("目录 `{}` 枚举期间发生变化", t.relative_path));
            }
        }
    }

    let toolchain = capture_toolchain(&scope.environment_keys);
    let mut entries: Vec<VersionEntry> = entries.into_values().collect();
    entries.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let digest = digest_of(&entries, &toolchain);

    Ok(ScopeVersion {
        schema: SCHEMA,
        digest,
        entries,
        toolchain,
        completeness,
    })
    .map(|mut v| {
        if !problems.is_empty() && v.completeness == VersionCompleteness::Complete {
            v.completeness = VersionCompleteness::Incomplete;
        }
        v
    })
}

impl VersionEntry {
    /// 读不到时的可定位说明（供采集问题列表用）。
    fn reason_if_any(&self) -> String {
        format!(
            "条目 `{}/{}`（{:?}）未能完整读取",
            self.root_id, self.relative_path, self.kind
        )
    }
}

/// 采集单个条目。**symlink 不跟随越出读取根**，越界即 `readable=false`。
fn capture_one(
    root_id: &str,
    relative_path: &str,
    abs: &Path,
    read_roots: &[PathBuf],
    ignored: &dyn Fn(&Path) -> bool,
) -> VersionEntry {
    let mut e = VersionEntry {
        root_id: root_id.to_string(),
        relative_path: normalize_rel(relative_path),
        kind: EntryKind::Missing,
        sha256: None,
        size: None,
        symlink_target: None,
        readable: true,
    };
    let meta = match std::fs::symlink_metadata(abs) {
        Ok(m) => m,
        Err(error) => {
            e.readable = error.kind() == std::io::ErrorKind::NotFound;
            return e;
        }
    };
    if meta.is_symlink() {
        e.kind = EntryKind::Symlink;
        let target = std::fs::read_link(abs)
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        e.symlink_target = target.clone();
        // 目标解析后必须仍在读取根内，否则不采内容。
        // 目标解析后必须仍在读取根内，否则不采内容（越界链接不该被"读到了"当成产物）。
        let resolved = std::fs::canonicalize(abs);
        let inside = match resolved {
            Ok(resolved) => read_roots.iter().any(|r| {
                std::fs::canonicalize(r)
                    .map(|c| resolved.starts_with(&c))
                    .unwrap_or(false)
            }),
            Err(_) => false,
        };
        if !inside {
            e.readable = false;
            e.sha256 = None;
            return e;
        }
        if let Ok(bytes) = std::fs::read(abs) {
            if bytes.len() as u64 > MAX_FILE_BYTES {
                e.readable = false;
                return e;
            }
            e.sha256 = Some(sha256_hex(&bytes));
            e.size = Some(bytes.len() as u64);
        } else {
            e.readable = false;
        }
        return e;
    }
    if meta.is_dir() {
        e.kind = EntryKind::Directory;
        let mut remaining = MAX_ENTRIES;
        match directory_digest(abs, read_roots, &mut remaining, ignored) {
            Ok(digest) => e.sha256 = Some(digest),
            Err(_) => e.readable = false,
        }
        return e;
    }
    e.kind = EntryKind::File;
    e.size = Some(meta.len());
    if meta.len() > MAX_FILE_BYTES {
        e.readable = false;
        e.sha256 = None;
        return e;
    }
    match std::fs::read(abs) {
        Ok(bytes) => e.sha256 = Some(sha256_hex(&bytes)),
        Err(_) => {
            e.readable = false;
            e.sha256 = None;
        }
    }
    e
}

fn directory_digest(
    path: &Path,
    roots: &[PathBuf],
    remaining: &mut usize,
    ignored: &dyn Fn(&Path) -> bool,
) -> std::io::Result<String> {
    fn walk(
        path: &Path,
        roots: &[PathBuf],
        budget: &mut usize,
        hash: &mut Sha256,
        depth: usize,
        ignored: &dyn Fn(&Path) -> bool,
    ) -> std::io::Result<()> {
        if depth > 64 {
            return Err(std::io::Error::other("directory scope too deep"));
        }
        let mut children = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            if ignored(&child.path()) {
                continue;
            }
            *budget = budget
                .checked_sub(1)
                .ok_or_else(|| std::io::Error::other("directory scope exceeds budget"))?;
            let name = child.file_name().to_string_lossy().into_owned();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name.as_bytes());
            let meta = std::fs::symlink_metadata(child.path())?;
            if meta.is_dir() {
                hash.update(b"directory");
                walk(&child.path(), roots, budget, hash, depth + 1, ignored)?;
            } else {
                let entry = capture_one("", &name, &child.path(), roots, ignored);
                if !entry.readable {
                    return Err(std::io::Error::other("unreadable directory member"));
                }
                hash.update(serde_json::to_vec(&entry).map_err(std::io::Error::other)?);
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    walk(path, roots, remaining, &mut hash, 0, ignored)?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// 相对路径归一：统一为 `/` 分隔、去掉前导 `./`，便于跨平台摘要一致
/// （Windows 的 `\` 与 macOS 的 `/` 不能让同一份内容算出两个 digest）。
fn normalize_rel(rel: &str) -> String {
    let mut s = rel.replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s
}

/// 内容摘要（`sha256:<hex>` 统一格式，与 trust.rs 的裸 hex 区分开：这里带前缀便于日志辨识）。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 单份内容的裸摘要（`[#12]` 核对声明哈希与产物观测用同一算法，
/// 否则"声明的哈希"与"读到的哈希"永不相等）。
pub fn digest_of_bytes(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// 全范围摘要：对排序后的条目与工具链事实依次喂入哈希。
/// 条目顺序、字段顺序固定 → 同一内容恒等摘要。
pub fn digest_of(entries: &[VersionEntry], toolchain: &[VersionFact]) -> String {
    let mut h = Sha256::new();
    for e in entries {
        h.update(e.root_id.as_bytes());
        h.update([0u8]);
        h.update(e.relative_path.as_bytes());
        h.update([0u8]);
        h.update(format!("{:?}", e.kind).as_bytes());
        h.update([0u8]);
        h.update(e.sha256.clone().unwrap_or_else(|| "-".into()).as_bytes());
        h.update([0u8]);
        h.update(
            e.size
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into())
                .as_bytes(),
        );
        h.update([0u8]);
        h.update(
            e.symlink_target
                .clone()
                .unwrap_or_else(|| "-".into())
                .as_bytes(),
        );
        h.update([0u8]);
        h.update(if e.readable { b"1" } else { b"0" });
        h.update([0u8]);
    }
    for t in toolchain {
        h.update(t.key.as_bytes());
        h.update([0u8]);
        h.update(t.value.as_bytes());
        h.update([0u8]);
    }
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 只采集 `environment_keys` 明确列出的工具链版本；查不到记 `unknown`（不是"空"）。
fn capture_toolchain(keys: &[String]) -> Vec<VersionFact> {
    let mut out = Vec::new();
    for k in keys {
        out.push(VersionFact {
            key: k.clone(),
            value: probe_toolchain(k),
        });
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// 单个工具链键的版本探测。只查常见工具的 `--version` 首行，不读环境变量、不落任何 env 值。
fn probe_toolchain(key: &str) -> String {
    let (prog, arg) = match key {
        "rustc" => ("rustc", "--version"),
        "cargo" => ("cargo", "--version"),
        "node" => ("node", "--version"),
        "python" => ("python3", "--version"),
        "go" => ("go", "version"),
        _ => return "unknown".to_string(),
    };
    let Ok(out) = std::process::Command::new(prog).arg(arg).output() else {
        return "unknown".to_string();
    };
    if !out.status.success() {
        return "unknown".to_string();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        "unknown".to_string()
    } else {
        // 只留有界首行（避免把整段 banner 落盘）。
        first.chars().take(120).collect()
    }
}

/// 比较记录版本与当前版本。
///
/// **记录侧不可信时一律 Unknown**：采集不完整/漂移的版本不能支撑通过，也不能被当成 stale
/// 去"乐观地"改判——它本来就没资格作结论。
pub fn compare_scope(recorded: &ScopeVersion, current: &ScopeVersion) -> Freshness {
    if !recorded.is_valid() {
        return Freshness::Unknown {
            reason: format!("记录侧范围采集不完整（{}）", recorded.completeness.as_str()),
        };
    }
    if !current.is_valid() {
        return Freshness::Unknown {
            reason: format!("当前范围采集不完整（{}）", current.completeness.as_str()),
        };
    }
    let changed = recorded.changed_against(current);
    if changed.is_empty() && recorded.toolchain == current.toolchain {
        Freshness::Fresh
    } else {
        Freshness::Stale {
            changed: changed.into_iter().take(MAX_REPORTED_CHANGES).collect(),
        }
    }
}

/// Output generation may change declared outputs; execution drift concerns inputs and dependencies.
pub fn compare_execution_inputs(
    before: &ScopeVersion,
    after: &ScopeVersion,
    scope: &VerificationScope,
) -> Freshness {
    let project = |version: &ScopeVersion| {
        let mut value = version.clone();
        value.entries.retain(|entry| {
            scope
                .inputs
                .iter()
                .chain(&scope.dependency_files)
                .any(|path| {
                    path.root_id == entry.root_id
                        && (path.relative_path == "."
                            || Path::new(&entry.relative_path)
                                .starts_with(path.relative_path.replace('\\', "/")))
                })
        });
        value.digest = digest_of(&value.entries, &value.toolchain);
        value
    };
    compare_scope(&project(before), &project(after))
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
