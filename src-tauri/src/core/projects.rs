//! 项目注册表：项目数据存于项目目录下的 `.codewave/`（数据随项目走）。
//! project.json = 元数据；temps/logs/memory/skills/tasks = 项目级托管数据。
//! 发现机制：`projects/directories.json` 索引（id → 项目主目录）+ 经会话索引的自愈查找
//! ——项目散落在用户代码目录里，全局扫描代价高；索引 + 快照查找双保险。
//! 会话加载是快照语义：create 时目录 → runtime roots；此后项目变更不影响已打开会话。

use super::config::{LEGACY_MANAGED_DIR_NAME, MANAGED_DIR_NAME};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 项目注册表条目（project.json 的载体）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// 项目 id（uuid 生成，兼作数据目录名）
    pub id: String,
    /// 显示名
    pub name: String,
    /// 项目主目录（单目录语义；需求 1.4 已移除多根）。
    pub directory: String,
    /// 项目数据目录（`<directory>/.codewave`）；缺省时 project_data_dir 回落
    /// `<directory>/.codewave`。
    #[serde(default)]
    pub data_dir: Option<String>,
    /// 创建时间（RFC3339）
    pub created_at: String,
    /// 已允许访问的外部目录（[docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)）：
    /// 用户把项目目录外的文件拖进来时选「始终允许这个目录」后记在这里，以后不再询问。
    /// 子目录一并放行；目录移动或改名后按路径匹配自然失效。
    #[serde(default)]
    pub allowed_dirs: Vec<String>,
}

impl ProjectEntry {
    /// 归一化：剥掉尾部斜杠，统一形态。
    pub fn normalize(mut self) -> Self {
        while self.directory.ends_with('/') && self.directory.len() > 1 {
            self.directory.pop();
        }
        self.allowed_dirs = normalize_allowed_dirs(self.allowed_dirs);
        self
    }

    /// 追加一个已允许目录（去重 + 归一化）；已存在时返回 false。
    pub fn allow_dir(&mut self, dir: &str) -> bool {
        let d = strip_trailing_slash(dir);
        if d.is_empty() || self.allowed_dirs.iter().any(|x| x == &d) {
            return false;
        }
        self.allowed_dirs.push(d);
        true
    }
}

/// `save_project` 的**入参**形态（仅 IPC 边界用，不落盘）。
///
/// 与落盘 DTO `ProjectEntry` 分开的唯一原因是 `allowed_dirs` 的三态语义：
/// - `None`（wire 上缺该键 / null）= 前端本次未携带该字段 → 保存时**沿用磁盘旧值**；
/// - `Some(v)` = 前端显式给了值 → 用 `v`（`Some(vec![])` 即显式清空）。
///
/// 落盘 DTO 保持 `Vec<String>` 不变，project.json 格式零变化。
/// 反过来说：若沿用 `ProjectEntry` 当入参，「字段缺省」与「显式清空」不可区分，
/// 前端少抄一个键就会把用户「始终允许」的外部目录静默清空（正是本 DTO 要堵的缺陷）。
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectSaveInput {
    pub id: String,
    pub name: String,
    pub directory: String,
    #[serde(default)]
    pub data_dir: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub allowed_dirs: Option<Vec<String>>,
}

/// 剥掉尾部斜杠（保留根 `/`）。
fn strip_trailing_slash(d: &str) -> String {
    let mut s = d.to_string();
    while s.ends_with('/') && s.len() > 1 {
        s.pop();
    }
    s
}

/// 已允许目录列表归一化：逐项去尾斜杠、去空、去重，保持原顺序。
pub fn normalize_allowed_dirs(dirs: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in dirs {
        let s = strip_trailing_slash(&d);
        if s.is_empty() || out.iter().any(|x| x == &s) {
            continue;
        }
        out.push(s);
    }
    out
}

// ---------- id → 目录索引（新位置项目的发现锚点） ----------

/// id → 项目主目录 的持久化索引（发现锚点，save_project 时登记）。
#[derive(Debug, Default, Serialize, Deserialize)]
struct DirectoryIndex {
    /// id → 项目主目录（不含 .codewave）
    entries: std::collections::HashMap<String, String>,
}

/// 索引文件路径：projects/directories.json。
fn index_path(data_dir: &Path) -> PathBuf {
    data_dir.join("projects").join("directories.json")
}

/// 读索引；缺失/损坏一律回空索引（绝不因索引坏而炸列表）。
fn load_index(data_dir: &Path) -> DirectoryIndex {
    std::fs::read(index_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// 原子写回索引。
fn save_index(data_dir: &Path, idx: &DirectoryIndex) {
    if let Ok(bytes) = serde_json::to_vec_pretty(idx) {
        let _ = crate::util::atomic::atomic_write(&index_path(data_dir), &bytes);
    }
}

/// 登记/更新一条 id → 主目录 索引。
fn index_remember(data_dir: &Path, id: &str, directory: &str) {
    let mut idx = load_index(data_dir);
    idx.entries.insert(id.to_string(), directory.to_string());
    save_index(data_dir, &idx);
}

/// 删除一条索引项（存在才写回，避免无谓落盘）。
fn index_forget(data_dir: &Path, id: &str) {
    let mut idx = load_index(data_dir);
    if idx.entries.remove(id).is_some() {
        save_index(data_dir, &idx);
    }
}

/// 全局 projects 根目录（projects/<id>/ 形态的历史位置与兜底根）。
pub fn projects_root(data_dir: &Path) -> PathBuf {
    data_dir.join("projects")
}

/// 项目数据目录：显式 data_dir 优先（语义 = <主目录>/.codewave），
/// 否则由主目录推导。
pub fn project_data_dir(_data_dir: &Path, entry: &ProjectEntry) -> PathBuf {
    if let Some(d) = &entry.data_dir {
        return PathBuf::from(d);
    }
    PathBuf::from(&entry.directory).join(MANAGED_DIR_NAME)
}

/// 按 id 查数据目录（供命令层把 project_id 解析为项目数据目录）。
pub fn project_data_dir_by_id(data_dir: &Path, id: &str) -> Option<PathBuf> {
    find(data_dir, id).map(|e| project_data_dir(data_dir, &e))
}

/// 只持有 id 的调用方（如计划任务日志）解析项目数据目录；回落 projects_root/<id>。
pub fn data_dir_by_id(data_dir: &Path, id: &str) -> PathBuf {
    project_data_dir_by_id(data_dir, id).unwrap_or_else(|| projects_root(data_dir).join(id))
}

/// 项目目录内的固定子目录（随项目创建；id 必须已是安全标识符——由 uuid 生成）
pub const SUBDIRS: &[&str] = &["temps", "logs", "memory", "skills", "tasks"];

/// 项目元数据文件路径：<数据目录>/project.json。
fn project_file(dir: &Path) -> PathBuf {
    dir.join("project.json")
}

/// id 白名单：非空、不含路径分隔符与点、首尾无空白（防目录穿越）。
/// 会话保留期清理删文件前也走这一条（[docs/session-cleanup](../../../docs/session-cleanup.md)：
/// 会话编号同样要拿去拼 `histories/<id>.json.gz` 等路径）。
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', '.']) && id == id.trim()
}

/// 聚合两处来源的 project.json：
/// ① directories.json 索引指向的 <主目录>/.codewave/；
/// ② 索引整体缺失时经会话快照自愈找回。
/// 单个目录损坏/不可解析即跳过（绝不炸掉整个列表）。
pub fn load(data_dir: &Path) -> Vec<ProjectEntry> {
    let mut out: Vec<ProjectEntry> = Vec::new();
    let mut upsert = |e: ProjectEntry| {
        if let Some(pos) = out.iter().position(|p| p.id == e.id) {
            out[pos] = e;
        } else {
            out.push(e);
        }
    };
    // ① directories.json 索引指向的项目（权威来源）
    let idx = load_index(data_dir);
    for (id, dir_str) in &idx.entries {
        let dir = Path::new(dir_str).join(MANAGED_DIR_NAME);
        if let Some(mut fresh) = read_project_json(&dir) {
            fresh = fresh.normalize();
            if fresh.id != *id {
                continue; // 索引与内容不一致，跳过
            }
            fresh.data_dir = Some(dir.to_string_lossy().into_owned());
            upsert(fresh);
        } else if find_in_sessions(data_dir, id).is_none() {
            // 项目目录没了且无会话引用 → 清掉死索引项
            index_forget(data_dir, id);
        }
    }
    // ② 自愈：索引整个丢失（如 directories.json 被删）时，经会话快照找回项目
    if load_index(data_dir).entries.is_empty() {
        for (id, dir_str) in discover_from_sessions(data_dir) {
            if let Some(mut fresh) = read_project_json(&Path::new(&dir_str).join(MANAGED_DIR_NAME))
            {
                fresh = fresh.normalize();
                if fresh.id == id {
                    fresh.data_dir = Some(
                        Path::new(&dir_str)
                            .join(MANAGED_DIR_NAME)
                            .to_string_lossy()
                            .into_owned(),
                    );
                    upsert(fresh);
                    index_remember(data_dir, &id, &dir_str);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 遍历会话索引：project_id → roots[0]（其下存在 .codewave/project.json 才有效）。
fn discover_from_sessions(data_dir: &Path) -> Vec<(String, String)> {
    let mut found: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let Ok(bytes) = std::fs::read(data_dir.join("sessions").join("index.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    if let Some(arr) = v.get("sessions").and_then(|x| x.as_array()) {
        for s in arr {
            let Some(pid) = s.get("project_id").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(root) = s
                .get("roots")
                .and_then(|r| r.as_array())
                .and_then(|r| r.first())
                .and_then(|x| x.as_str())
            else {
                continue;
            };
            if read_project_json(&Path::new(root).join(MANAGED_DIR_NAME)).is_some() {
                found.insert(pid.to_string(), root.to_string());
            }
        }
    }
    found.into_iter().collect()
}

/// 单个 id 的会话快照查找（索引项失效时判断项目是否仍存活）。
fn find_in_sessions(data_dir: &Path, id: &str) -> Option<String> {
    discover_from_sessions(data_dir)
        .into_iter()
        .find(|(pid, _)| pid == id)
        .map(|(_, dir)| dir)
}

/// 读取并解析单个 project.json；缺失/损坏返回 None。
fn read_project_json(dir: &Path) -> Option<ProjectEntry> {
    let bytes = std::fs::read(project_file(dir)).ok()?;
    serde_json::from_slice::<ProjectEntry>(&bytes).ok()
}

/// 按 id 查找项目（全量 load 后过滤；调用频率低，简单可靠优先）。
pub fn find(data_dir: &Path, id: &str) -> Option<ProjectEntry> {
    load(data_dir).into_iter().find(|p| p.id == id)
}

/// 比较用的路径归一：能 canonicalize 就用其绝对形态，否则退回原样。
///
/// canonicalize 失败（路径不存在 / 无权限）不报错——迁移判定只是启发式，
/// 判不出来就保持 data_dir 原值（宁可不迁移，也不能把能用的条目改坏）。
fn comparable_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// data_dir 归一化（三条分支，抽出来供 save_project 与 save_project_input 共用）：
/// ① 空 / 全空白 → `<主目录>/.codewave`；
/// ② 指向旧默认位置 `<主目录>/.wavestudio` → 重算到新约定位置
///    （用户手动改名自救时，持久化值不允许继续指向旧名）；
/// ③ **父目录与当前主目录不一致**（即主目录被改过）→ 重算到 `<新主目录>/.codewave`。
///
/// ③ 是「项目从左栏消失」的根因：前端编辑项目时会把**旧** data_dir 原样回传，
/// 于是索引被改指到新主目录、project.json 却仍留在旧目录下 → `load()` 读不到，
/// 项目既不可见也删不掉。跟随主目录重算才是一致行为。
///
/// 比较时两侧都先 `comparable_path`：Windows 上 `canonicalize` 返回 `\\?\` verbatim 路径，
/// 与普通字符串直接比较永远不相等；不存在的路径则退回 `Path` 直接比较。
fn normalize_data_dir(entry: &mut ProjectEntry) {
    let new_default = |directory: &str| {
        Path::new(directory)
            .join(MANAGED_DIR_NAME)
            .to_string_lossy()
            .into_owned()
    };
    let current = entry.data_dir.as_deref().map(str::trim).unwrap_or("");
    if current.is_empty() {
        entry.data_dir = Some(new_default(&entry.directory));
        return;
    }
    // project_data_dir 对已 Some 的值原样返回，因此旧默认位置与「主目录已变」的归一化必须在此显式重建
    let stale_legacy = current == new_default_of(&entry.directory, LEGACY_MANAGED_DIR_NAME);
    let follows_directory = Path::new(current)
        .parent()
        .map(|p| comparable_path(p) == comparable_path(Path::new(&entry.directory)))
        .unwrap_or(false);
    if stale_legacy || !follows_directory {
        entry.data_dir = Some(new_default(&entry.directory));
    }
}

/// <主目录>/<托管目录名> 的字符串形态（Windows 上必须经 Path::join：
/// canonicalize 得到的 `\\?\` verbatim 路径与正斜杠字符串直接拼接会 ERROR_INVALID_NAME）。
fn new_default_of(directory: &str, managed_dir_name: &str) -> String {
    Path::new(directory)
        .join(managed_dir_name)
        .to_string_lossy()
        .into_owned()
}

/// 读磁盘上已存在的那一份 entry（按 id）：索引反查旧主目录 → 只读那一个 project.json。
///
/// 刻意**不走** `find()`/`load()`：后者是全量 load（内部还会写索引，见 index_forget/index_remember），
/// 而本函数处在 `create_session` 的 find+save 热路径上。
/// 必须经索引反查：编辑时用户可能改了主目录，旧 project.json 在**旧**目录之下。
///
/// id 不匹配（索引指向的 project.json 里是另一个项目）即视为无旧值——防御性：
/// 索引与内容不一致时宁可让 allowed_dirs 回到空列表，也不把别的项目的目录放行进来。
fn read_existing(data_dir: &Path, id: &str) -> Option<ProjectEntry> {
    let dir = load_index(data_dir).entries.get(id)?.clone();
    let e = read_project_json(&Path::new(&dir).join(MANAGED_DIR_NAME))?;
    (e.id == id).then_some(e)
}

/// 新建/更新单个项目：mkdir 数据目录 + 原子写元数据 + 建固定子目录 + 登记发现索引。
/// data_dir 落盘前归一化（None → <主目录>/.codewave；主目录变更 → 跟随迁移），
/// load 侧从此无需回落。
pub fn save_project(data_dir: &Path, entry: &ProjectEntry) -> anyhow::Result<()> {
    if !valid_id(&entry.id) {
        anyhow::bail!("非法项目 id");
    }
    let mut entry = entry.clone();
    normalize_data_dir(&mut entry);
    let dir = project_data_dir(data_dir, &entry);
    std::fs::create_dir_all(&dir)?;
    for sub in SUBDIRS {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    let bytes = serde_json::to_vec_pretty(&entry)?;
    crate::util::atomic::atomic_write(&project_file(&dir), &bytes)?;
    // 登记发现索引（重启后 load 依赖它再发现）
    if !entry.directory.is_empty() {
        index_remember(data_dir, &entry.id, &entry.directory);
    }
    Ok(())
}

/// `save_project` 的入参校验（用户输入准入）。纯函数、无 IO，供 host 命令层调用。
/// 与 core 层的结构性兜底分工：这里管**用户输入准入**（名称/主目录不能为空），
/// core 兜底管**结构性非法**（防 CWD 污染 / 目录穿越）。
///
/// 刻意只兜空白、不裁剪落盘值：保存路径上不做任何静默改写。
pub fn validate_save_input(name: &str, directory: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("项目名称不能为空");
    }
    if directory.trim().is_empty() {
        anyhow::bail!("项目主目录不能为空");
    }
    Ok(())
}

/// IPC 入口（读-合并-写）：`allowed_dirs` 缺省时沿用磁盘旧值。
pub fn save_project_input(data_dir: &Path, input: &ProjectSaveInput) -> anyhow::Result<()> {
    // ① id 合法性必须最先：否则 "../evil" 会触发针对非法 id 的文件读取
    if !valid_id(&input.id) {
        anyhow::bail!("非法项目 id");
    }
    // ② 结构性兜底：主目录为空会让 project_data_dir 落到相对路径 ".codewave"，
    //    create_dir_all 将在**进程工作目录**下建目录，且索引被跳过 → 项目永久不可见也删不掉。
    //    这一层刻意只兜 directory（不兜 name）：name 的用户输入准入在 host 层做
    //    （validate_save_input），刻意不在 core 硬拦，否则磁盘上的历史空名项目
    //    经 create_session 回写（host/commands/session.rs）会失败、建不出会话。
    if input.directory.trim().is_empty() {
        anyhow::bail!("项目主目录不能为空");
    }
    let mut entry = ProjectEntry {
        id: input.id.clone(),
        name: input.name.clone(),
        directory: input.directory.clone(),
        data_dir: input.data_dir.clone(),
        created_at: input.created_at.clone(),
        // ③ 合并：未提供则沿用磁盘旧值（read_existing 读不到 = 新项目 = 空列表）
        allowed_dirs: match &input.allowed_dirs {
            Some(v) => v.clone(),
            None => read_existing(data_dir, &input.id)
                .map(|e| e.allowed_dirs)
                .unwrap_or_default(),
        },
    };
    // ④ 主目录变更时 data_dir 跟随迁移（与 save_project 同一份归一化逻辑）
    normalize_data_dir(&mut entry);
    save_project(data_dir, &entry)
}

/// 删除项目：整个项目数据目录（含 temps/logs/memory/skills/tasks 托管数据）。
/// 用户代码目录永不动（只移除其内部的 .codewave/ 数据目录，其余全部保留）。
pub fn delete_project(data_dir: &Path, id: &str) -> anyhow::Result<()> {
    if !valid_id(id) {
        anyhow::bail!("非法项目 id");
    }
    if let Some(entry) = find(data_dir, id) {
        let dir = project_data_dir(data_dir, &entry);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    index_forget(data_dir, id);
    Ok(())
}

/// 项目 logs/<session-id>.log 路径（W5 日志路由）。
pub fn session_log_path(data_dir: &Path, project_id: &str, session_id: &str) -> PathBuf {
    data_dir_by_id(data_dir, project_id)
        .join("logs")
        .join(format!("{session_id}.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dd: &Path, id: &str, name: &str) -> ProjectEntry {
        ProjectEntry {
            id: id.into(),
            name: name.into(),
            directory: dd.join(format!("proj-{id}")).to_string_lossy().into_owned(),
            data_dir: None, // 保存时归一化 → <directory>/.codewave
            created_at: "2026-08-30T00:00:00Z".into(),
            allowed_dirs: vec![],
        }
    }

    #[test]
    fn dir_per_project_roundtrip() {
        let dd = tempfile::tempdir().unwrap();
        save_project(dd.path(), &entry(dd.path(), "p1", "CodeWave")).unwrap();
        save_project(dd.path(), &entry(dd.path(), "p2", "DocsWave")).unwrap();
        // 固定子目录已创建
        for sub in SUBDIRS {
            assert!(data_dir_by_id(dd.path(), "p1").join(sub).is_dir());
        }
        let all = load(dd.path());
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|p| p.name == "CodeWave"));
        // 更新 = 重写
        let updated = entry(dd.path(), "p1", "CodeWave2");
        save_project(dd.path(), &updated).unwrap();
        assert_eq!(
            load(dd.path()).iter().find(|p| p.id == "p1").unwrap().name,
            "CodeWave2"
        );
    }

    #[test]
    fn corrupt_dir_skipped_and_delete_cascades_files() {
        let dd = tempfile::tempdir().unwrap();
        // 损坏的项目目录：目录在但 project.json 坏了
        let bad = data_dir_by_id(dd.path(), "bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(project_file(&bad), b"not json").unwrap();
        save_project(dd.path(), &entry(dd.path(), "good", "G")).unwrap();
        assert_eq!(load(dd.path()).len(), 1); // bad 被跳过
        // 删除 = 整个目录（含子目录文件）消失
        std::fs::write(
            data_dir_by_id(dd.path(), "good").join("logs").join("s.log"),
            b"x",
        )
        .unwrap();
        delete_project(dd.path(), "good").unwrap();
        assert!(!data_dir_by_id(dd.path(), "good").exists());
        assert!(load(dd.path()).is_empty());
    }

    #[test]
    fn invalid_id_rejected() {
        let dd = tempfile::tempdir().unwrap();
        assert!(save_project(dd.path(), &entry(dd.path(), "../evil", "E")).is_err());
        assert!(delete_project(dd.path(), "..").is_err());
    }

    #[test]
    fn stale_legacy_data_dir_renormalized_on_save() {
        // 手工迁移场景：用户把 <主目录>/.wavestudio 改名 .codewave 后，
        // 残留旧默认位置 data_dir 的条目保存时必须重归一化到新约定位置
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(ws.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut e = entry(dd.path(), "p-mig", "迁移项目");
        e.directory = dir.clone();
        e.data_dir = Some(
            std::path::PathBuf::from(&dir)
                .join(LEGACY_MANAGED_DIR_NAME)
                .to_string_lossy()
                .into_owned(),
        );
        save_project(dd.path(), &e).unwrap();
        let found = find(dd.path(), "p-mig").unwrap();
        let expected = std::path::PathBuf::from(&dir)
            .join(MANAGED_DIR_NAME)
            .to_string_lossy()
            .into_owned();
        assert_eq!(found.data_dir.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn relocatable_project_survives_reload() {
        // 回归：新语义项目（数据在项目目录下）重启后必须仍可发现（BUG：项目消失）
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(ws.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut e = entry(dd.path(), "p-new", "新项目");
        e.directory = dir.clone();
        // Windows：canonicalize 返回 \\?\ verbatim 路径，与正斜杠字符串直接拼接会得到
        // ERROR_INVALID_NAME（os error 123），必须经 Path::join
        let data_abs = std::path::PathBuf::from(&dir)
            .join(MANAGED_DIR_NAME)
            .to_string_lossy()
            .into_owned();
        e.data_dir = Some(data_abs.clone());
        save_project(dd.path(), &e).unwrap();
        // 模拟重启：重载必须能再发现（索引锚点）
        let found = find(dd.path(), "p-new").expect("新位置项目必须可发现");
        assert_eq!(found.directory, dir);
        assert_eq!(found.data_dir.as_deref(), Some(data_abs.as_str()));
        // 索引整个丢失时：会话快照自愈（造一个引用该项目的会话索引）
        index_forget(dd.path(), "p-new");
        let sessions_dir = dd.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let snap = serde_json::json!({
            "version": 1,
            "sessions": [{
                "id": "s1", "title": "t", "workspace": dir,
                "project_id": "p-new", "roots": [dir],
                "created_at": "", "updated_at": "", "message_count": 0,
            }]
        });
        std::fs::write(
            sessions_dir.join("index.json"),
            serde_json::to_vec(&snap).unwrap(),
        )
        .unwrap();
        let found2 = find(dd.path(), "p-new").expect("会话快照自愈发现");
        assert_eq!(found2.directory, dir);
    }

    // ---------- save_project_input（读-合并-写 + 主目录迁移） ----------

    /// 取可比较/可拼接的绝对路径：Windows 上 canonicalize 返回 `\\?\` verbatim 路径
    /// （与正斜杠字符串直接拼接会 ERROR_INVALID_NAME，必须经 Path::join）。
    fn abs_dir(p: &Path) -> String {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    /// 读回磁盘上落盘的那份 project.json（按索引反查）。
    /// 刻意不用 `load()`/`find()` 断言 save 侧行为：load 会无条件用索引重算 data_dir，
    /// 那样断言会假绿（写错位置也测不出来）。
    fn read_saved(dd: &Path, id: &str) -> serde_json::Value {
        let dir = load_index(dd)
            .entries
            .get(id)
            .expect("项目必须在索引里")
            .clone();
        let bytes = std::fs::read(project_file(&Path::new(&dir).join(MANAGED_DIR_NAME)))
            .expect("project.json");
        serde_json::from_slice(&bytes).unwrap()
    }

    fn save_input(id: &str, name: &str, directory: &str) -> ProjectSaveInput {
        ProjectSaveInput {
            id: id.into(),
            name: name.into(),
            directory: directory.into(),
            data_dir: None,
            created_at: "2026-08-30T00:00:00Z".into(),
            allowed_dirs: None,
        }
    }

    /// 键集合硬锚点：project.json 落盘的键精确等于这 6 个。
    /// 多一个字段 = 前端「按已知字段回抄」的前提被打破（一旦前端漏抄，新字段仍会被覆盖写清空，
    /// 正是本批次在修的缺陷）；少一个字段 = 数据丢失。改结构必须先改这里。
    #[test]
    fn project_json_wire_field_set_contract() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ext = tempfile::tempdir().unwrap();
        let mut e = entry(dd.path(), "p-keys", "键集合");
        e.directory = abs_dir(ws.path());
        e.allowed_dirs = vec![abs_dir(ext.path())];
        save_project(dd.path(), &e).unwrap();
        let obj = read_saved(dd.path(), "p-keys")
            .as_object()
            .expect("project.json 必须是对象")
            .clone();
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "allowed_dirs",
                "created_at",
                "data_dir",
                "directory",
                "id",
                "name"
            ],
            "project.json 键集合变了：新增字段必须同步前端回抄 + 本断言"
        );
    }

    /// 主缺陷护栏：前端漏抄 allowed_dirs（wire 上无该键 → None）时，
    /// 整份覆盖写不得把用户「始终允许」的外部目录清空。
    #[test]
    fn allowed_dirs_omitted_keeps_previous() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ext = tempfile::tempdir().unwrap();
        let dir = abs_dir(ws.path());
        let ext_dir = abs_dir(ext.path());
        let mut e = entry(dd.path(), "p-keep", "保留");
        e.directory = dir.clone();
        e.allowed_dirs = vec![ext_dir.clone()];
        save_project(dd.path(), &e).unwrap();

        // 前端只改了名字，不带 allowed_dirs 键
        let input = save_input("p-keep", "改名", &dir);
        assert!(input.allowed_dirs.is_none(), "缺省 = 本次未携带");
        save_project_input(dd.path(), &input).unwrap();

        let saved = read_saved(dd.path(), "p-keep");
        assert_eq!(
            saved["name"],
            serde_json::json!("改名"),
            "其余字段照常采用本次值"
        );
        assert_eq!(saved["allowed_dirs"], serde_json::json!([ext_dir]));
    }

    /// 三态里的第三态必须仍然可用：显式 Some([]) = 用户主动清空，不得被合并回旧值。
    #[test]
    fn allowed_dirs_explicit_empty_clears() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ext = tempfile::tempdir().unwrap();
        let dir = abs_dir(ws.path());
        let mut e = entry(dd.path(), "p-clear", "清空");
        e.directory = dir.clone();
        e.allowed_dirs = vec![abs_dir(ext.path())];
        save_project(dd.path(), &e).unwrap();

        let mut input = save_input("p-clear", "清空", &dir);
        input.allowed_dirs = Some(vec![]);
        save_project_input(dd.path(), &input).unwrap();

        assert_eq!(
            read_saved(dd.path(), "p-clear")["allowed_dirs"],
            serde_json::json!([])
        );
    }

    /// 合并只认同一个 id：A 的允许目录绝不能被 B 继承（继承错 = 越权放行）。
    /// 后半段额外钉住 `read_existing` 的 id 匹配防御：索引指向的 project.json 里是另一个
    /// 项目 id 时，一律当作「无旧值」（allowed_dirs 回落空列表）而不是继承。
    #[test]
    fn merge_never_crosses_ids() {
        let dd = tempfile::tempdir().unwrap();
        let ws_a = tempfile::tempdir().unwrap();
        let ws_b = tempfile::tempdir().unwrap();
        let ext_a = tempfile::tempdir().unwrap();
        let ext_b = tempfile::tempdir().unwrap();
        let (dir_a, dir_b) = (abs_dir(ws_a.path()), abs_dir(ws_b.path()));
        let (a_dir, b_dir) = (abs_dir(ext_a.path()), abs_dir(ext_b.path()));
        for (id, dir, ext) in [("p-a", &dir_a, &a_dir), ("p-b", &dir_b, &b_dir)] {
            let mut e = entry(dd.path(), id, id);
            e.directory = dir.clone();
            e.allowed_dirs = vec![ext.clone()];
            save_project(dd.path(), &e).unwrap();
        }

        // 用 B 的 id 保存且不带 allowed_dirs → 只继承 B 自己的旧值
        save_project_input(dd.path(), &save_input("p-b", "B改", &dir_b)).unwrap();
        let saved = read_saved(dd.path(), "p-b");
        assert_eq!(saved["allowed_dirs"], serde_json::json!([b_dir]));
        // 否定断言必须**逐项等值比对**：子串匹配（to_string().contains）在路径互为前缀时
        // （如 `D:\out` 与 `D:\outside`）会误判，而这里是「越权放行」护栏，属安全语义。
        let allowed: Vec<String> = serde_json::from_value(saved["allowed_dirs"].clone())
            .expect("allowed_dirs 必须是字符串数组");
        assert!(
            !allowed.iter().any(|d| d == &a_dir),
            "不得拿到 A 的允许目录：{allowed:?}"
        );

        // 防御：B 的位置放一份 id = p-a 的 project.json（索引与内容不一致）
        let mut forged = entry(dd.path(), "p-a", "A");
        forged.directory = dir_b.clone();
        forged.allowed_dirs = vec![a_dir.clone()];
        std::fs::write(
            project_file(&Path::new(&dir_b).join(MANAGED_DIR_NAME)),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        save_project_input(dd.path(), &save_input("p-b", "B再改", &dir_b)).unwrap();
        let saved = read_saved(dd.path(), "p-b");
        assert_eq!(
            saved["allowed_dirs"],
            serde_json::json!([]),
            "索引与内容 id 不一致时不得继承别人的允许目录"
        );
    }

    /// N1 护栏（一次覆盖两个缺陷）：改主目录时
    /// (a) allowed_dirs 缺省仍沿用旧值；(b) data_dir 跟随新主目录（project.json 落在 B 下）；
    /// (c) 重启后 load() 仍能发现该项目——原缺陷是「项目从左栏消失且删不掉」。
    /// 旧目录下的数据目录**故意保留不删**（自动删用户目录是危险行为）。
    #[test]
    fn directory_change_moves_data_dir_and_keeps_project() {
        let dd = tempfile::tempdir().unwrap();
        let ws_a = tempfile::tempdir().unwrap();
        let ws_b = tempfile::tempdir().unwrap();
        let ext = tempfile::tempdir().unwrap();
        let (dir_a, dir_b) = (abs_dir(ws_a.path()), abs_dir(ws_b.path()));
        let ext_dir = abs_dir(ext.path());
        let mut e = entry(dd.path(), "p-move", "搬家");
        e.directory = dir_a.clone();
        e.allowed_dirs = vec![ext_dir.clone()];
        save_project(dd.path(), &e).unwrap();

        // 前端把**旧** data_dir 原样回传（ProjectNav.tsx 的实际形状）+ 换了主目录
        let input = ProjectSaveInput {
            id: "p-move".into(),
            name: "搬家".into(),
            directory: dir_b.clone(),
            data_dir: Some(
                Path::new(&dir_a)
                    .join(MANAGED_DIR_NAME)
                    .to_string_lossy()
                    .into_owned(),
            ),
            created_at: e.created_at.clone(),
            allowed_dirs: None,
        };
        save_project_input(dd.path(), &input).unwrap();

        // (a) 允许目录没丢
        assert_eq!(
            read_saved(dd.path(), "p-move")["allowed_dirs"],
            serde_json::json!([ext_dir])
        );
        // (b) project.json 落到新主目录下，data_dir 也指向它
        assert!(
            project_file(&Path::new(&dir_b).join(MANAGED_DIR_NAME)).is_file(),
            "换主目录后必须写到新位置"
        );
        assert_eq!(
            read_saved(dd.path(), "p-move")["data_dir"],
            serde_json::json!(Path::new(&dir_b).join(MANAGED_DIR_NAME).to_string_lossy())
        );
        // (c) 重启后仍可发现
        let found = find(dd.path(), "p-move").expect("换主目录后项目必须仍可发现");
        assert_eq!(found.directory, dir_b);
        assert_eq!(found.allowed_dirs, vec![ext_dir]);
    }

    /// 空主目录必须在**任何 mkdir 之前**被拒：否则 project_data_dir 落到相对路径
    /// ".codewave"，会在**进程工作目录**下建目录且索引被跳过 → 项目不可见也删不掉。
    ///
    /// 可观测面只有「报错 + 无任何落盘副作用」：`.codewave` 若真被建在进程 CWD 下，
    /// 本用例无从观测（Rust 测试并行执行，改进程 CWD 会与其他用例竞争，
    /// 故不采用 set_current_dir + guard 还原的写法）。因此以
    /// 「三种空白输入全部 Err + 未写发现索引 + 无可见项目」作为等价可观测证据。
    #[test]
    fn empty_directory_rejected_without_creating_dirs() {
        let dd = tempfile::tempdir().unwrap();
        for bad in ["", "   ", "\t\n"] {
            assert!(
                save_project_input(dd.path(), &save_input("p-empty", "空目录", bad)).is_err(),
                "空/纯空白主目录必须拒绝：{bad:?}"
            );
        }
        assert!(
            load_index(dd.path()).entries.is_empty(),
            "拒绝路径不得写发现索引"
        );
        assert!(load(dd.path()).is_empty(), "拒绝路径不得留下可见项目");
    }

    /// N4 兼容护栏：磁盘上历史空名项目必须仍能经**内部** save_project 回写
    /// （create_session 走这条路，见 host/commands/session.rs）——
    /// 若把空名校验下沉到 core，存量用户会直接建不出会话。
    #[test]
    fn legacy_empty_name_project_still_savable() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let mut e = entry(dd.path(), "p-noname", "");
        e.directory = abs_dir(ws.path());
        save_project(dd.path(), &e).expect("内部回写不得拦空名");
        assert_eq!(
            read_saved(dd.path(), "p-noname")["name"],
            serde_json::json!("")
        );
        assert!(find(dd.path(), "p-noname").is_some());
    }

    /// 用户输入准入（host 命令层用的纯函数）：空/纯空白名称与主目录均拒，正常值通过。
    /// 末尾斜杠/前后空格不算空（项目主目录合法）。
    #[test]
    fn validate_save_input_rejects_empty_name_and_directory() {
        assert!(validate_save_input("", "/tmp/a").is_err(), "空名");
        assert!(validate_save_input("   ", "/tmp/a").is_err(), "纯空白名");
        assert!(validate_save_input("名字", "").is_err(), "空主目录");
        assert!(validate_save_input("名字", " \t ").is_err(), "纯空白主目录");
        assert!(validate_save_input("名字", "/tmp/a").is_ok());
        assert!(
            validate_save_input("名字", " /tmp/a/ ").is_ok(),
            "带空白与尾斜杠仍是合法输入"
        );
    }

    /// id 合法性必须先于任何读盘/写盘：("../evil" 会把 project.json 拼成目录穿越路径）。
    /// 可观测面是「无任何写入副作用 + 报错」；读取本身无副作用，故读序不可直接观测，
    /// 实现上 id 校验就是 save_project_input 的第一条语句。
    #[test]
    fn save_project_input_rejects_invalid_id_before_any_read() {
        let dd = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let missing = PathBuf::from(abs_dir(ws.path())).join("never-created");
        let input = save_input("../evil", "穿越", &missing.to_string_lossy());
        assert!(
            save_project_input(dd.path(), &input).is_err(),
            "非法 id 必须拒"
        );
        assert!(!missing.exists(), "非法 id 不得创建任何目录");
        assert!(!index_path(dd.path()).exists(), "非法 id 不得写索引文件");
        assert!(load(dd.path()).is_empty());
    }
}
