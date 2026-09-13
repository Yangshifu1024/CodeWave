use crate::core::types::Message;
use crate::core::sessions::repair;
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
}

impl SessionStore {
    /// 以给定数据根构造存储（不立即建目录；写路径各自兜底）。
    pub fn new(data_root: PathBuf) -> Self {
        SessionStore {
            root: data_root,
            index_lock: std::sync::Mutex::new(()),
            artifacts_lock: std::sync::Mutex::new(()),
        }
    }

    /// 索引文件路径：sessions/index.json。
    fn index_path(&self) -> PathBuf {
        self.root.join("sessions").join("index.json")
    }

    /// 会话历史文件路径：histories/<id>.json.gz。
    fn history_path(&self, id: &str) -> PathBuf {
        self.root.join("histories").join(format!("{id}.json.gz"))
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
                let backup = self.root.join("sessions").join("index.json.corrupt");
                let _ = std::fs::rename(self.index_path(), &backup);
                tracing::warn!("会话索引损坏，已备份到 {}：{e}", backup.display());
                SessionIndex::default()
            }
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
    pub fn upsert_meta(&self, meta: SessionMeta) -> anyhow::Result<()> {
        self.mutate_index(|idx| {
            // M7 修复：保留原 created_at（此前每次检查点都会重置创建时间）
            let created_at = idx
                .sessions
                .iter()
                .find(|s| s.id == meta.id)
                .map(|s| s.created_at.clone())
                .unwrap_or_else(|| meta.created_at.clone());
            idx.sessions.retain(|s| s.id != meta.id);
            let mut meta = meta.clone();
            meta.created_at = created_at;
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
    /// 为剥图后再存。
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
        // 大图片历史可能顶到上限：剥离图片 payload 后重试
        // （附件退化为占位文本，但会话仍可保存）
        let gz = if gz.len() > MAX_HISTORY_BYTES {
            let stripped = repair::sanitize(&mut prepared);
            let gz2 = gzip_history(&prepared)?;
            if gz2.len() > MAX_HISTORY_BYTES {
                anyhow::bail!("历史超过 8MB 上限，请新开会话");
            }
            tracing::warn!("会话 {id} 历史含图片超上限，已剥离图片 payload 保存：{stripped:?}");
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

    /// 子代理历史目录：histories/subs/<parent>/。
    fn sub_histories_dir(&self, parent: &str) -> PathBuf {
        self.root.join("histories").join("subs").join(parent)
    }

    /// 子代理历史文件路径：histories/subs/<parent>/<sub>.json.gz。
    fn sub_history_path(&self, parent: &str, sub: &str) -> PathBuf {
        self.sub_histories_dir(parent).join(format!("{sub}.json.gz"))
    }

    /// 持久化子代理的完整执行历史（drive_agent 结束/异常路径调用）。
    /// 复用主历史的 sanitize/trim/gzip 管线，但绝不触碰索引。
    pub fn save_sub_history(&self, parent: &str, sub: &str, msgs: &[Message]) -> anyhow::Result<()> {
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

    /// todos 边车文件路径：sessions/<id>.todos.json。
    fn todos_path(&self, id: &str) -> PathBuf {
        self.root.join("sessions").join(format!("{id}.todos.json"))
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

    /// 产物边车文件路径：sessions/<id>.artifacts.json。
    fn artifacts_path(&self, id: &str) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("{id}.artifacts.json"))
    }

    /// 登记一次写入：按路径去重/合并（首记 create/edit；重复则累加 count 并刷新 last_*）。
    pub fn append_artifact(&self, id: &str, path: &str, op: ArtifactOp) -> anyhow::Result<()> {
        let _guard = self.artifacts_lock.lock().unwrap();
        let mut items = self.load_artifacts(id);
        let now = Utc::now().to_rfc3339();
        if let Some(a) = items.iter_mut().find(|a| a.path == path) {
            a.last_op = op;
            a.last_at = now;
            a.count = a.count.saturating_add(1);
        } else {
            items.push(SessionArtifact {
                path: path.to_string(),
                first_op: op,
                last_op: op,
                first_at: now.clone(),
                last_at: now,
                count: 1,
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
