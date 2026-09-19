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
}

impl ProjectEntry {
    /// 归一化：剥掉尾部斜杠，统一形态。
    pub fn normalize(mut self) -> Self {
        while self.directory.ends_with('/') && self.directory.len() > 1 {
            self.directory.pop();
        }
        self
    }
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
fn valid_id(id: &str) -> bool {
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

/// 新建/更新单个项目：mkdir 数据目录 + 原子写元数据 + 建固定子目录 + 登记发现索引。
/// data_dir 落盘前归一化（None → <主目录>/.codewave），load 侧从此无需回落。
/// 手工迁移修正：显式传入的 data_dir 若为旧默认位置（<主目录>/.wavestudio），
/// 重归一化到新约定位置——用户手动改目录名自救时，持久化值不允许继续指向旧名。
pub fn save_project(data_dir: &Path, entry: &ProjectEntry) -> anyhow::Result<()> {
    if !valid_id(&entry.id) {
        anyhow::bail!("非法项目 id");
    }
    let mut entry = entry.clone();
    // project_data_dir 对已 Some 的值原样返回，因此旧默认位置的归一化必须在此显式重建
    let legacy_default = Path::new(&entry.directory)
        .join(LEGACY_MANAGED_DIR_NAME)
        .to_string_lossy()
        .into_owned();
    let stale_legacy = entry.data_dir.as_deref() == Some(legacy_default.as_str());
    if entry
        .data_dir
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
        || stale_legacy
    {
        entry.data_dir = Some(
            Path::new(&entry.directory)
                .join(MANAGED_DIR_NAME)
                .to_string_lossy()
                .into_owned(),
        );
    }
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
}
