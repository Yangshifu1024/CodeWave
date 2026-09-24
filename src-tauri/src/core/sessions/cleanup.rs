//! 会话保留期清理（[docs/session-cleanup](../../../../docs/session-cleanup.md)）：按「最近活动时间」收敛本地会话数据。
//!
//! 判定基准 = `max(updated_at, last_opened_at)`，按**精确时长**（当前时间 − N×24 小时）比较绝对时刻——
//! 索引里的时间戳可能混有不同时区偏移，绝不做字符串比较；任一时间戳缺失或无法解析的会话**不删**并记告警；
//! 正在运行中的会话一律跳过。
//!
//! 删除纪律（§3 第 17 条）：先删文件、最后**一次性**写索引；某条会话文件删除失败则保留其索引行
//!（下次清理再试），单条失败不中断整批；删除条数与失败数都记日志。
//!
//! 文件 IO 集中在本模块的少数几个函数里，判定与挑选都是纯函数，便于单测。

use super::store::{ArtifactKind, SessionMeta, SessionStore, is_sub_blob_owner};
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// 预览里回传的标题条数上限。
pub const PREVIEW_TITLES: usize = 5;

/// 日志里列删除编号的上限（超出只列前 N 个并说明总数）。
pub const LOG_ID_LIMIT: usize = 20;

/// 合法保留期档位（与设置页下拉一致）：1/3/7/14/30 天。
pub const ALLOWED_RETENTION_DAYS: [u32; 5] = [1, 3, 7, 14, 30];

/// 预览结果（IPC 返回结构，字段名为前端契约）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupPreview {
    /// 将删除的会话条数
    pub count: u32,
    /// 前几条会话标题（最多 `PREVIEW_TITLES` 条，按最近活动时间倒序）
    pub titles: Vec<String>,
    /// 将删除的**索引外残留**项数（历史压缩文件 + 边车文件）
    #[serde(default)]
    pub orphan_count: u32,
}

/// 一次清理的结果（IPC 返回结构，字段名为前端契约）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupOutcome {
    /// 已清理（文件已删且索引行已移除）的会话 id：前端据此关闭对应标签页
    pub ids: Vec<String>,
    /// 删除条数
    pub deleted: u32,
    /// 失败条数（文件删除失败 → 索引行保留，下次清理再试）
    pub failed: u32,
}

/// 上次清理状态（持久化在 `<数据目录>/sessions/cleanup.json`；重启后仍在）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupStatus {
    /// 上次清理时间（RFC3339；None = 从未清理过）
    #[serde(default)]
    pub last_run_at: Option<String>,
    /// 上次删除条数
    #[serde(default)]
    pub last_deleted: u32,
    /// 上次失败条数
    #[serde(default)]
    pub last_failed: u32,
}

/// 清理状态文件路径：`<数据目录>/sessions/cleanup.json`。
pub fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions").join("cleanup.json")
}

/// 读上次清理状态；文件缺失/损坏回默认（全零，绝不因状态文件而中断）。
pub fn read_status(data_dir: &Path) -> CleanupStatus {
    std::fs::read(status_path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// 原子写上次清理状态：**每次清理执行后都写**（包括删除 0 条的情况，启动清理也计入）。
pub fn write_status(data_dir: &Path, status: &CleanupStatus) {
    let path = status_path(data_dir);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("清理状态目录创建失败（{}）：{e}", parent.display());
            return;
        }
    }
    match serde_json::to_vec_pretty(status) {
        Ok(bytes) => {
            if let Err(e) = crate::util::atomic::atomic_write(&path, &bytes) {
                tracing::warn!("清理状态写入失败（{}）：{e}", path.display());
            }
        }
        Err(e) => tracing::warn!("清理状态序列化失败：{e}"),
    }
}

/// 运行中的会话 id（索引 `running` 标记 ∪ 进程内运行集合）：清理一律跳过。
/// 启动清理与命令层共用（命令层再叠加会话运行态，见 host）。
pub fn running_set(store: &SessionStore) -> HashSet<String> {
    let mut set = store.running_in_memory();
    for m in store.load_index().sessions.iter().filter(|m| m.running) {
        set.insert(m.id.clone());
    }
    set
}

/// 最近活动时间（清理判定基准）= `max(updated_at, last_opened_at)`。
/// 任一字段存在但无法解析 → 返回 None 并记告警（调用方据此**不删**该会话）。
pub fn last_activity_at(meta: &SessionMeta) -> Option<DateTime<FixedOffset>> {
    let updated = match DateTime::parse_from_rfc3339(&meta.updated_at) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "会话 {} 的 updated_at 无法解析（{e}），本次不清理该会话",
                meta.id
            );
            return None;
        }
    };
    match meta.last_opened_at.as_deref() {
        // 从未记录 = 不参与判定（不是「无法解析」）
        None => Some(updated),
        Some(raw) => match DateTime::parse_from_rfc3339(raw) {
            Ok(opened) => Some(updated.max(opened)),
            Err(e) => {
                tracing::warn!(
                    "会话 {} 的 last_opened_at 无法解析（{e}），本次不清理该会话",
                    meta.id
                );
                None
            }
        },
    }
}

/// 纯函数：截止时刻 = `now − days × 24 小时`（精确时长，不按自然日）。
/// 会话判定与「索引外残留」的文件时间判定共用它，保证两条口径完全一致。
pub fn cutoff_at(now: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    now - chrono::Duration::hours(24 * days as i64)
}

/// 过期判定：最近活动时间早于 `now − days × 24 小时`（精确时长，不是自然日）。
/// 未来时间戳不会被删（它不早于 cutoff）。
pub fn is_expired(last_activity: DateTime<FixedOffset>, now: DateTime<Utc>, days: u32) -> bool {
    last_activity.with_timezone(&Utc) < cutoff_at(now, days)
}

/// 纯函数：保留期档位是否合法（白名单 = `ALLOWED_RETENTION_DAYS`）。
/// 白名单之外的值（含 0、手改配置写进去的怪值）一律不清理——绝不拿它去删用户数据。
pub fn is_valid_retention_days(days: u32) -> bool {
    ALLOWED_RETENTION_DAYS.contains(&days)
}

/// 保留期档位的判定落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionChoice {
    /// 用这个天数清理
    Run(u32),
    /// 未设置（「不清理」）：本次什么都不做
    Skip,
    /// 档位非法（白名单之外，含 0）：本次什么都不做
    Invalid(u32),
}

/// 纯函数：解析本次清理要用的保留期——显式天数（设置页草稿值）优先，否则取已保存配置。
/// 显式值存在时**只认它**（即使已保存的值合法）：弹框里给用户看的是显式值，不能偷换。
pub fn resolve_retention(explicit: Option<u32>, saved: Option<u32>) -> RetentionChoice {
    match explicit.or(saved) {
        None => RetentionChoice::Skip,
        Some(d) if is_valid_retention_days(d) => RetentionChoice::Run(d),
        Some(d) => {
            tracing::warn!("会话保留期档位非法（{d}）：只接受 1/3/7/14/30 天，本次不清理");
            RetentionChoice::Invalid(d)
        }
    }
}

/// 内部：过期会话 + 其最近活动时刻（只解析一次，避免排序时重复解析并重复告警）。
fn expired_with_time(
    metas: &[SessionMeta],
    now: DateTime<Utc>,
    days: u32,
    running: &HashSet<String>,
) -> Vec<(SessionMeta, DateTime<FixedOffset>)> {
    metas
        .iter()
        .filter(|m| !m.running && !running.contains(&m.id))
        .filter_map(|m| last_activity_at(m).map(|t| (m.clone(), t)))
        .filter(|(_, t)| is_expired(*t, now, days))
        .collect()
}

/// 纯函数：从索引快照里挑出过期会话（跳过运行中与会话时间不可解析的）。
pub fn select_expired(
    metas: &[SessionMeta],
    now: DateTime<Utc>,
    days: u32,
    running: &HashSet<String>,
) -> Vec<SessionMeta> {
    expired_with_time(metas, now, days, running)
        .into_iter()
        .map(|(m, _)| m)
        .collect()
}

/// 纯函数：预览（保存前的确认框内容）。
///
/// 标题取**最近活动时间倒序**的前 5 条：条数只说明「要删多少」，而越新的会话越可能是用户
/// 还记得的那个——展示最新的一批比展示最旧的更能让用户判断「这些确实是可以删的会话」。
pub fn preview(
    metas: &[SessionMeta],
    now: DateTime<Utc>,
    days: u32,
    running: &HashSet<String>,
) -> CleanupPreview {
    let mut hits = expired_with_time(metas, now, days, running);
    hits.sort_by_key(|a| std::cmp::Reverse(a.1));
    CleanupPreview {
        count: hits.len() as u32,
        titles: hits
            .iter()
            .take(PREVIEW_TITLES)
            .map(|(m, _)| m.title.clone())
            .collect(),
        // 索引外残留要读盘才知道，由 `preview_all` 补上
        orphan_count: 0,
    }
}

/// 预览（会话条数/标题 + 索引外残留项数）。
///
/// 为什么要连残留一起数：执行会删「会话文件 + 索引外残留」两类东西，预览只数会话的话，
/// 「只剩残留可删」时预览返回 0，前端就不会弹确认框（用户看不到任何提示），
/// 而执行照样删——预览口径必须与执行口径一致。
/// 索引不可信时残留按 0 报（`count_orphan_files` 里已记 warn）。
pub fn preview_all(
    store: &SessionStore,
    metas: &[SessionMeta],
    now: DateTime<Utc>,
    days: u32,
    running: &HashSet<String>,
) -> CleanupPreview {
    let mut out = preview(metas, now, days, running);
    out.orphan_count = count_orphan_files(store, cutoff_at(now, days));
    out
}

/// 会话日志候选项：按路由给出主路径（项目会话在项目数据目录、临时会话在全局数据目录），
/// 再补另一侧的同名路径（会话归属变更过时旧文件不残留）；不存在的路径视为无需删除。
pub fn session_log_candidates(data_dir: &Path, meta: &SessionMeta) -> Vec<PathBuf> {
    let routed = crate::core::session_log::path_for(data_dir, meta.project_id.as_deref(), &meta.id);
    let global = crate::core::session_log::path_for(data_dir, None, &meta.id);
    if routed == global {
        vec![routed]
    } else {
        vec![routed, global]
    }
}

/// 删除一个会话的全部托管文件，返回 true = 无失败（文件本就不存在视为成功）。
/// 范围：历史 gz、两个边车、子代理过程历史目录、会话日志（两条位置）、该会话产物边车里
/// `kind == Plan` 的计划文件。
///
/// 计划文件登记的是绝对路径，仅删除**已登记归属且路径合规**的那些（见 `plan_path_allowed`）；
/// 删前做存在性判断，失败只记告警（不计入该会话的失败，避免一个已被手工删除的计划文件
/// 把整条会话卡在索引里）。
///
/// 会话编号先过白名单（`is_safe_session_id`）：下面的路径全部由编号拼出来
///（`histories/<id>.json.gz`、`sessions/<id>.*`、`histories/subs/<id>/`、`sessions/<id>.toolres/`、
/// `sessions/<id>.imgblob/`、子历史的 `sessions/<父>__<sub>.imgblob/`、`logs/<id>.log`），
/// 索引里的脏编号绝不能进拼接；编号非法时返回 false（本次不删、索引行保留、计入失败），
/// 宁可删不掉也不让脏值变成目录穿越。
pub fn delete_session_files(store: &SessionStore, data_dir: &Path, meta: &SessionMeta) -> bool {
    if !is_safe_session_id(&meta.id) {
        tracing::warn!("会话编号非法（{}），跳过该会话的清理", meta.id);
        return false;
    }
    if let Some(pid) = meta.project_id.as_deref() {
        if !is_safe_session_id(pid) {
            tracing::warn!("会话 {} 的项目编号非法（{pid}），跳过该会话的清理", meta.id);
            return false;
        }
    }

    // 计划文件清单必须先读（下一步会删掉产物边车本身）
    let plan_files: Vec<String> = store
        .load_artifacts(&meta.id)
        .into_iter()
        .filter(|a| a.kind == ArtifactKind::Plan)
        .map(|a| a.path)
        .collect();

    let mut ok = true;
    ok &= remove_file_if_exists(&store.history_path(&meta.id));
    ok &= remove_file_if_exists(&store.artifacts_path(&meta.id));
    ok &= remove_file_if_exists(&store.todos_path(&meta.id));
    // 子代理过程历史目录（histories/subs/<id>/）及其图片 blob（blob 归子历史自己，
    // owner 是 `<父会话 id>__<sub>`，不在父会话的 sessions/<id>.imgblob/ 里）——
    // 目录列必须先读，下一步就把它删了
    for sub in store.sub_history_ids(&meta.id) {
        ok &= remove_dir_if_exists(&store.sub_image_blobs_dir(&meta.id, &sub));
    }
    ok &= remove_dir_if_exists(&store.sub_histories_dir(&meta.id));
    // 工具结果原样 sidecar（[docs/session-restore-fidelity](../../../../docs/session-restore-fidelity.md)）：
    // 目录随会话级联删除（与子代理过程历史同范式：路径由编号拼出、不做登记边车）
    ok &= remove_dir_if_exists(&store.tool_results_dir(&meta.id));
    // 图片 blob（[docs/session-history-limits](../../../../docs/session-history-limits.md)）：同上
    ok &= remove_dir_if_exists(&store.image_blobs_dir(&meta.id));
    for p in session_log_candidates(data_dir, meta) {
        ok &= remove_file_if_exists(&p);
    }
    for raw in plan_files {
        if plan_path_allowed(&raw) {
            remove_plan_file(&raw);
        } else {
            tracing::warn!(
                "计划文件路径不合规（{raw}），跳过删除（必须同时满足：后缀 .md、路径含 .codewave/tasks/）"
            );
        }
    }
    ok
}

/// 清理索引之外的残留（超出索引条数上限被挤出、列表里已看不到的会话文件）：
/// 只删「id 不在索引里」且「文件修改时间早于 cutoff」的 `histories/<id>.json.gz`、
/// `sessions/<id>.{artifacts,todos}.json` 与 `sessions/<id>.{toolres,imgblob}/`。
/// **子历史的 blob 目录（`sessions/<父>__<sub>.imgblob/`）不在范围内**：它的「id」不在会话索引里、
/// 也不带 `sub_` 前缀，按「不在索引即孤儿」判定会被误删，而父会话的子历史还引用着那些图——
/// 这类目录由级联删除路径负责（见 `is_sub_blob_owner`）。
///
/// **索引不可信（缺失 / 损坏解析失败）时一个都不删**：`load_index()` 此时返回空索引，
/// 与「用户真的没有会话」在返回值上无法区分——照常扫孤儿会把全部历史文件当孤儿删掉，
/// 而预览还报 0 条（前端连确认框都不弹）。所以先问 `index_is_trusted()`，不可信就记 warn 收手。
/// 用户真的没有会话（索引能解析、只是 `sessions` 为空）不属于这一类，孤儿照样清。
///
/// **必须排除 `sub_` / `task_` 前缀**：子代理过程历史与计划任务的 id 本来就不在会话索引里，
/// 按「id 不在索引」判定会误删正在使用中的数据。不碰 `index.json`（含 `.corrupt` 备份）
/// 与 `sessions/subs/` 目录。
pub fn remove_orphan_files(store: &SessionStore, cutoff: DateTime<Utc>) -> usize {
    let mut removed = 0usize;
    for path in orphan_candidates(store, cutoff) {
        // 目录（sessions/<id>.toolres/）递归删，文件按文件删：两者共用同一份候选与预览口径
        let removed_ok = if path.is_dir() {
            remove_dir_if_exists(&path)
        } else {
            remove_file_if_exists(&path)
        };
        if removed_ok {
            tracing::info!("清理索引外残留：{}", path.display());
            removed += 1;
        }
    }
    removed
}

/// 统计本次会删掉的索引外残留项数（只读盘、不删任何文件）。
/// 与 `remove_orphan_files` 共用同一份候选列表，保证预览口径与执行口径一致。
pub fn count_orphan_files(store: &SessionStore, cutoff: DateTime<Utc>) -> u32 {
    orphan_candidates(store, cutoff).len() as u32
}

/// 索引外残留候选（内部）：索引不可信时返回空（已记 warn）。
///
/// 从文件名解析出的编号一律再过一遭 `is_safe_session_id`（纵深防御）：这里的候选虽然只用于
/// `remove_file_if_exists`（删的是 `read_dir` 拿到的真实目录项，不拿编号拼路径），但编号是
/// 磁盘上的任意文件名，脏值不参与任何后续判定更安全；解析失败/非法就跳过（宁可留着不删）。
fn orphan_candidates(store: &SessionStore, cutoff: DateTime<Utc>) -> Vec<PathBuf> {
    if !store.index_is_trusted() {
        tracing::warn!("会话索引不可读（缺失或损坏），本次跳过索引外残留清理：宁可不删也不误删");
        return Vec::new();
    }
    let known: HashSet<String> = store
        .load_index()
        .sessions
        .into_iter()
        .map(|m| m.id)
        .collect();
    let mut out: Vec<PathBuf> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(store.histories_dir()) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json.gz") else {
                continue;
            };
            if !is_safe_session_id(id) || known.contains(id) || is_non_session_id(id) {
                continue;
            }
            let path = entry.path();
            if modified_before(&path, cutoff) {
                out.push(path);
            }
        }
    }

    if let Ok(rd) = std::fs::read_dir(store.sessions_dir()) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name
                .strip_suffix(".artifacts.json")
                .or_else(|| name.strip_suffix(".todos.json"));
            let Some(id) = id else {
                continue;
            };
            if !is_safe_session_id(id) || known.contains(id) || is_non_session_id(id) {
                continue;
            }
            let path = entry.path();
            if modified_before(&path, cutoff) {
                out.push(path);
            }
        }
    }

    // 按会话分桶的托管目录（sessions/<id>.toolres/、sessions/<id>.imgblob/）：
    // 会话被挤出索引后整个目录随之成为残留（`remove_orphan_files` 按 `path.is_dir()` 分流）
    if let Ok(rd) = std::fs::read_dir(store.sessions_dir()) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name
                .strip_suffix(".toolres")
                .or_else(|| name.strip_suffix(".imgblob"));
            let Some(id) = id else {
                continue;
            };
            // `is_sub_blob_owner`：子历史的 blob 目录（sessions/<父>__<sub>.imgblob/）
            // 既不在会话索引里也不带 `sub_` 前缀，不得当孤儿删（父会话还引用着那些图）
            if !is_safe_session_id(id)
                || known.contains(id)
                || is_non_session_id(id)
                || is_sub_blob_owner(id)
            {
                continue;
            }
            let path = entry.path();
            if modified_before(&path, cutoff) {
                out.push(path);
            }
        }
    }

    out
}

/// 执行一次清理：先删文件、最后**一次性**写索引；随后扫索引外残留并写状态文件。
///
/// `candidates` 由调用方（`select_expired`）在同一时刻挑好：启动清理要求「先同步挑候选、
/// 再异步删除」，避免与前端恢复标签页的先后顺序导致结果随机。
pub fn execute(
    store: &SessionStore,
    data_dir: &Path,
    days: u32,
    candidates: &[SessionMeta],
) -> CleanupOutcome {
    let now = Utc::now();
    let mut ok_ids: Vec<String> = Vec::new();
    let mut failed = 0u32;
    for meta in candidates {
        if delete_session_files(store, data_dir, meta) {
            ok_ids.push(meta.id.clone());
        } else {
            failed += 1;
            tracing::warn!(
                "会话 {} 本次未完成清理（文件删除失败，或编号非法被跳过），索引行保留（下次清理再试）",
                meta.id
            );
        }
    }

    // 索引写入失败不能按「删了 0 个」报：文件已删、索引行还在，受影响的会话要计入失败
    //（下次清理会再次命中，重删文件视为成功——自愈）。
    let mut index_written = true;
    let removed = match store.remove_many(&ok_ids) {
        Ok(removed) => removed,
        Err(e) => {
            index_written = false;
            failed += ok_ids.len() as u32;
            tracing::warn!(
                "会话索引批量删除写入失败：文件已删、索引未更新，下次清理自愈（受影响的会话计入失败 {} 个）：{e}",
                ok_ids.len()
            );
            Vec::new()
        }
    };
    if index_written && removed.len() != ok_ids.len() {
        tracing::warn!(
            "会话索引批量删除条数不符（期望 {}，实际 {}）：并发改动导致索引里已没有这些条目",
            ok_ids.len(),
            removed.len()
        );
    }

    let orphans = remove_orphan_files(store, cutoff_at(now, days));

    let outcome = CleanupOutcome {
        deleted: removed.len() as u32,
        ids: removed,
        failed,
    };
    tracing::info!(
        "会话保留期清理完成：删除 {} 个，失败 {} 个，索引外残留 {} 个",
        outcome.deleted,
        outcome.failed,
        orphans
    );
    if !outcome.ids.is_empty() {
        // 启动那次清理没有界面可问，至少要留下「删了哪些会话」的痕迹（命令与启动共用本函数）
        tracing::info!("已删除的会话编号：{}", format_deleted_ids(&outcome.ids));
    }
    write_status(
        data_dir,
        &CleanupStatus {
            last_run_at: Some(now.to_rfc3339()),
            last_deleted: outcome.deleted,
            last_failed: outcome.failed,
        },
    );
    outcome
}

/// 一次到位的清理（挑候选 + 执行）：命令层与启动清理之外的场景用。
pub fn run(store: &SessionStore, data_dir: &Path, days: u32) -> CleanupOutcome {
    let now = Utc::now();
    let running = running_set(store);
    let candidates = select_expired(&store.load_index().sessions, now, days, &running);
    execute(store, data_dir, days, &candidates)
}

/// 子代理过程历史（`sub_`）与计划任务（`task_`）的 id 前缀：它们本就不在会话索引里，
/// 绝不参与「索引外残留」清理。
fn is_non_session_id(id: &str) -> bool {
    id.starts_with("sub_") || id.starts_with("task_")
}

/// 会话编号合法性（删除入口拼路径前统一校验）：非空、不含路径分隔符（`/` `\`）、
/// 不含 `.`（因而也不含 `..`）、首尾无空白。与项目 id 共用同一条白名单（`projects::valid_id`），
/// 不另立一套规则。
///
/// 为什么要这道校验：删除路径全由编号拼出来（`histories/<id>.json.gz`、`histories/subs/<id>/`、
/// `logs/<id>.log`），索引是磁盘上的普通 JSON，一条含 `..` 的脏编号就能把删除操作引到数据目录之外。
pub fn is_safe_session_id(id: &str) -> bool {
    crate::core::projects::valid_id(id)
}

/// 纯函数：计划文件路径是否合规（删除前白名单）。
/// 必须**同时**满足三条：
/// ① 后缀 `.md`；② 路径里含 `.codewave/tasks/`；③ 路径分段里没有 `..`。
/// 边车可被外部工具改写，绝不能让一条脏登记把任意文件变成清理目标——单靠 ①+② 挡不住
/// `/ws/.codewave/tasks/../../../../etc/hosts.md` 这类先满足字面条件、再靠 `..` 回退出去的路径，
/// 所以按 `/` 切段逐段比 `..`（`a..b.md` 这类含两个点的**文件名**不算，只有独立成段才拒绝）。
/// Windows 分隔符先归一成 `/` 再判断。
fn plan_path_allowed(raw: &str) -> bool {
    let normalized = raw.replace('\\', "/");
    normalized.ends_with(".md")
        && normalized.contains(".codewave/tasks/")
        && !normalized.split('/').any(|seg| seg == "..")
}

/// 纯函数：被删会话编号拼成一行日志文本（超过 `LOG_ID_LIMIT` 时截断但说明总数）。
pub fn format_deleted_ids(ids: &[String]) -> String {
    if ids.len() <= LOG_ID_LIMIT {
        return ids.join(", ");
    }
    format!(
        "{} …（共 {} 个）",
        ids[..LOG_ID_LIMIT].join(", "),
        ids.len()
    )
}

/// 文件删除：不存在 = 成功（项目已删除、已手工清理都算成功）。
fn remove_file_if_exists(p: &Path) -> bool {
    match std::fs::remove_file(p) {
        Ok(()) => true,
        Err(e) if e.kind() == ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!("清理会话文件失败（{}）：{e}", p.display());
            false
        }
    }
}

/// 目录级联删除：不存在 = 成功。
fn remove_dir_if_exists(p: &Path) -> bool {
    match std::fs::remove_dir_all(p) {
        Ok(()) => true,
        Err(e) if e.kind() == ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!("清理会话目录失败（{}）：{e}", p.display());
            false
        }
    }
}

/// 计划文件删除：绝对路径，删前存在性判断，失败只记告警（不影响该会话的删除判定）。
fn remove_plan_file(raw: &str) {
    let path = Path::new(raw);
    if !path.exists() {
        return;
    }
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!("清理计划文件失败（{raw}）：{e}");
    }
}

/// 文件修改时间是否早于 cutoff（取不到修改时间 → 不删，保守）。
fn modified_before(path: &Path, cutoff: DateTime<Utc>) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let at: DateTime<Utc> = modified.into();
    at < cutoff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sessions::store::ArtifactOp;
    use chrono::TimeZone;

    fn meta(id: &str, updated_at: &str) -> SessionMeta {
        SessionMeta {
            id: id.into(),
            title: format!("t-{id}"),
            workspace: ".".into(),
            model_id: None,
            created_at: updated_at.into(),
            updated_at: updated_at.into(),
            message_count: 0,
            project_id: None,
            roots: vec!["/ws".into()],
            running: false,
            interrupted: None,
            last_opened_at: None,
            history_status: None,
        }
    }

    /// 固定时刻，纯函数测试里不用 `Utc::now()`（否则「边界」用例会随执行时刻漂移）。
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 19, 12, 0, 0).unwrap()
    }

    fn ago(secs: i64) -> String {
        (now() - chrono::Duration::seconds(secs)).to_rfc3339()
    }

    /// 真实时钟的 N 秒前：走 `run` / `execute` 的用例必须用它——这两个函数内部取真实当前时间，
    /// 用固定时钟造时间戳会让「过期」判定随执行时刻漂移。
    fn real_ago(secs: i64) -> String {
        (Utc::now() - chrono::Duration::seconds(secs)).to_rfc3339()
    }

    /// 把文件的修改时间改到 N 天前（模拟陈旧残留：索引外残留按文件修改时间判定）。
    fn age_file(path: &Path, days: i64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let t = std::time::SystemTime::now()
            - std::time::Duration::from_secs((days.max(0) as u64) * 86_400);
        f.set_modified(t).unwrap();
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    fn store_in(dir: &Path) -> SessionStore {
        SessionStore::new(dir.to_path_buf())
    }

    /// 测试用：一条只含图片的 user 消息。
    fn image(data: &str) -> crate::core::types::Message {
        crate::core::types::Message {
            role: crate::core::types::Role::User,
            content: vec![crate::core::types::Content::Image {
                media_type: "image/png".into(),
                data: data.into(),
            }],
            created_at: None,
        }
    }

    // ---------- 判定 ----------

    /// 精确时长判定（不是自然日）：正好 24 小时前**不删**，早 1 秒**删**。
    #[test]
    fn expired_uses_exact_duration_with_day_boundary() {
        let boundary = meta("boundary", &ago(24 * 3600));
        let just_over = meta("over", &ago(24 * 3600 + 1));
        let fresh = meta("fresh", &ago(3600));
        let metas = vec![boundary, just_over, fresh];

        let picked: Vec<String> = select_expired(&metas, now(), 1, &none())
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(
            picked,
            vec!["over".to_string()],
            "1 天档位只删超过 24 小时的"
        );

        // 3 天档位：24 小时前的还在保留期内
        assert!(select_expired(&metas, now(), 3, &none()).is_empty());
    }

    /// 未来时间戳不会被删（时钟误差/异常数据都不得误删）。
    #[test]
    fn future_timestamp_never_deleted() {
        let future = meta("future", &(now() + chrono::Duration::hours(1)).to_rfc3339());
        assert!(select_expired(&[future], now(), 1, &none()).is_empty());
    }

    /// 判定基准取 updated_at 与 last_opened_at 的**较新者**。
    #[test]
    fn last_activity_takes_newer_of_updated_and_opened() {
        // 很久没更新、但刚刚打开过 → 保留
        let mut opened_recently = meta("opened", &ago(30 * 24 * 3600));
        opened_recently.last_opened_at = Some(ago(60));
        // 刚刚更新过、很久没打开 → 保留
        let mut updated_recently = meta("updated", &ago(60));
        updated_recently.last_opened_at = Some(ago(30 * 24 * 3600));
        // 两个都旧 → 删
        let mut both_old = meta("old", &ago(30 * 24 * 3600));
        both_old.last_opened_at = Some(ago(20 * 24 * 3600));

        let picked: Vec<String> = select_expired(
            &[opened_recently, updated_recently, both_old],
            now(),
            7,
            &none(),
        )
        .into_iter()
        .map(|m| m.id)
        .collect();
        assert_eq!(picked, vec!["old".to_string()]);

        // last_opened_at 参与判定：打开时间较新（+08:00 偏移写法）时保留
        let mut tz_opened = meta("tz", &ago(30 * 24 * 3600));
        tz_opened.last_opened_at = Some("2026-09-19T19:00:00+08:00".into()); // = 11:00Z，1 小时前
        assert!(select_expired(&[tz_opened], now(), 7, &none()).is_empty());
    }

    /// 不同时区偏移混用：按绝对时刻比较（字符串比较会得出错误结论）。
    #[test]
    fn mixed_timezone_offsets_compared_as_instants() {
        // 同一时刻的两种写法：UTC 与 +08:00
        let utc = "2026-09-01T04:00:00+00:00";
        let shifted = "2026-09-01T12:00:00+08:00";
        let a = last_activity_at(&meta("a", utc)).unwrap();
        let b = last_activity_at(&meta("b", shifted)).unwrap();
        assert_eq!(a, b, "同一时刻的两种偏移必须解析成同一时刻");

        // 字符串比较会把 shifted 判成「更晚」（"12" > "04"）——按时刻比较则两者一起过期
        let picked = select_expired(&[meta("a", utc), meta("b", shifted)], now(), 7, &none());
        assert_eq!(picked.len(), 2);
        // 7 天以内（8 月 30 日之后）的两种写法都不删
        let recent_utc = "2026-09-15T04:00:00+00:00";
        let recent_shifted = "2026-09-15T12:00:00+08:00";
        assert!(
            select_expired(
                &[meta("c", recent_utc), meta("d", recent_shifted)],
                now(),
                7,
                &none()
            )
            .is_empty()
        );
    }

    /// 时间戳不可解析（或缺失）→ 不删，且记告警（此处断言不删）。
    #[test]
    fn unparsable_timestamps_are_never_deleted() {
        // updated_at 空（旧/手造索引）→ 不删
        assert!(select_expired(&[meta("empty", "")], now(), 1, &none()).is_empty());
        // updated_at 乱码 → 不删
        assert!(select_expired(&[meta("bad", "yesterday")], now(), 1, &none()).is_empty());
        // last_opened_at 乱码 → 不删（即使 updated_at 很旧）
        let mut broken_opened = meta("broken-open", &ago(400 * 24 * 3600));
        broken_opened.last_opened_at = Some("not-a-time".into());
        assert!(select_expired(&[broken_opened], now(), 1, &none()).is_empty());
        // last_activity_at 本身也如实返回 None
        assert!(last_activity_at(&meta("bad2", "2026-13-45T99:99:99Z")).is_none());
    }

    /// 运行中会话一律跳过：索引 `running` 标记与进程内运行集合两条口径都生效。
    #[test]
    fn running_sessions_are_skipped() {
        let mut indexed = meta("indexed-running", &ago(30 * 24 * 3600));
        indexed.running = true;
        let in_process = meta("memory-running", &ago(30 * 24 * 3600));
        let idle = meta("idle", &ago(30 * 24 * 3600));

        let mut running = HashSet::new();
        running.insert("memory-running".to_string());
        let picked: Vec<String> =
            select_expired(&[indexed, in_process, idle.clone()], now(), 1, &running)
                .into_iter()
                .map(|m| m.id)
                .collect();
        assert_eq!(picked, vec!["idle".to_string()]);

        // running_set 同时汇总两条口径（进程内集合直接来自 store 的运行状态）
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.upsert_meta(meta("r-index", &ago(0))).unwrap();
        store.mark_running("r-index", true).unwrap();
        store.mark_running("r-mem", true).unwrap();
        let set = running_set(&store);
        assert!(set.contains("r-index"));
        assert!(set.contains("r-mem"));
    }

    /// 预览：条数正确 + 标题按最近活动时间倒序取前 5。
    #[test]
    fn preview_counts_and_takes_newest_titles() {
        let mut metas = Vec::new();
        // 7 个过期会话，活动时间从新到旧 t1（最新）… t7（最旧）
        for i in 1..=7 {
            metas.push(meta(&format!("s{i}"), &ago(8 * 24 * 3600 + i * 3600)));
        }
        metas.push(meta("fresh", &ago(60)));
        let p = preview(&metas, now(), 7, &none());
        assert_eq!(p.count, 7);
        assert_eq!(p.titles.len(), PREVIEW_TITLES);
        assert_eq!(
            p.titles,
            vec!["t-s1", "t-s2", "t-s3", "t-s4", "t-s5"],
            "标题按最近活动时间倒序（最新的在前，最可能被用户认出）"
        );
    }

    /// 保留期档位白名单：只接受 1/3/7/14/30；未设置 = 不清理；0 与其它怪值 = 非法（一律不清理）。
    #[test]
    fn retention_whitelist_accepts_only_known_tiers() {
        for d in ALLOWED_RETENTION_DAYS {
            assert!(is_valid_retention_days(d), "{d} 是合法档位");
            assert_eq!(resolve_retention(Some(d), None), RetentionChoice::Run(d));
            assert_eq!(resolve_retention(None, Some(d)), RetentionChoice::Run(d));
        }

        // 空值 = 不清理（未设置，不是非法）
        assert_eq!(resolve_retention(None, None), RetentionChoice::Skip);

        // 0 = 非法（不是「不清理」的写法，而是手改/旧数据里的怪值）
        assert!(!is_valid_retention_days(0));
        assert_eq!(
            resolve_retention(Some(0), None),
            RetentionChoice::Invalid(0)
        );
        assert_eq!(
            resolve_retention(None, Some(0)),
            RetentionChoice::Invalid(0)
        );

        // 其它怪值 = 非法
        for bad in [2u32, 6, 31, 365, u32::MAX] {
            assert!(!is_valid_retention_days(bad));
            assert_eq!(
                resolve_retention(Some(bad), None),
                RetentionChoice::Invalid(bad)
            );
        }

        // 显式值优先（设置页草稿值就是弹框里给用户看的那个）
        assert_eq!(
            resolve_retention(Some(7), Some(30)),
            RetentionChoice::Run(7)
        );
        // 显式值非法时即使存量值合法也不清理：不能偷换成用户没看过的档位
        assert_eq!(
            resolve_retention(Some(2), Some(7)),
            RetentionChoice::Invalid(2)
        );
    }

    // ---------- 文件删除 ----------

    /// 会话日志两条路径都能删：项目会话在项目数据目录、临时会话在全局数据目录。
    #[test]
    fn session_logs_deleted_on_both_routes() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let proj_dir = tempfile::tempdir().unwrap();
        let entry = crate::core::projects::ProjectEntry {
            id: "p1".into(),
            name: "P1".into(),
            directory: proj_dir.path().to_string_lossy().into_owned(),
            data_dir: None,
            created_at: ago(0),
            allowed_dirs: vec![],
        };
        crate::core::projects::save_project(dd.path(), &entry).unwrap();

        // 项目会话：项目数据目录 logs/<id>.log
        let mut proj_meta = meta("proj-sess", &ago(30 * 24 * 3600));
        proj_meta.project_id = Some("p1".into());
        let proj_log = crate::core::session_log::path_for(dd.path(), Some("p1"), "proj-sess");
        std::fs::create_dir_all(proj_log.parent().unwrap()).unwrap();
        std::fs::write(&proj_log, b"log").unwrap();
        assert!(proj_log.starts_with(proj_dir.path()));

        // 临时会话：全局数据目录 logs/<id>.log
        let temp_meta = meta("temp-sess", &ago(30 * 24 * 3600));
        let temp_log = crate::core::session_log::path_for(dd.path(), None, "temp-sess");
        std::fs::create_dir_all(temp_log.parent().unwrap()).unwrap();
        std::fs::write(&temp_log, b"log").unwrap();

        assert!(delete_session_files(&store, dd.path(), &proj_meta));
        assert!(delete_session_files(&store, dd.path(), &temp_meta));
        assert!(!proj_log.exists(), "项目会话日志应随会话删除");
        assert!(!temp_log.exists(), "临时会话日志应随会话删除");

        // 文件不存在（含项目已删除、解析到不存在的路径）同样算成功
        let mut ghost = meta("ghost-proj", &ago(30 * 24 * 3600));
        ghost.project_id = Some("no-such-project".into());
        assert!(delete_session_files(&store, dd.path(), &ghost));
    }

    /// 计划文件：边车 `kind == plan` 解析正确、随会话连带删除；普通产物文件不动。
    #[test]
    fn plan_files_are_deleted_and_plain_artifacts_kept() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let ws = tempfile::tempdir().unwrap();

        // 临时会话的真实形态：工作区 = 全局数据目录 → 计划文件是双层路径
        let plan_path = dd
            .path()
            .join(".codewave")
            .join("tasks")
            .join("plan-20260919-120000-abcdef.md");
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, "# 计划").unwrap();
        let user_file = ws.path().join("code.rs");
        std::fs::write(&user_file, b"fn main() {}").unwrap();

        let session = "temp-session";
        store
            .append_artifact_kind(
                session,
                &plan_path.to_string_lossy(),
                ArtifactOp::Create,
                ArtifactKind::Plan,
            )
            .unwrap();
        store
            .append_artifact(session, &user_file.to_string_lossy(), ArtifactOp::Create)
            .unwrap();

        // 边车解析：kind 读回正确；右栏「文件」数据源过滤掉计划文件
        let all = store.load_artifacts(session);
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter()
                .find(|a| a.path == plan_path.to_string_lossy())
                .unwrap()
                .kind,
            ArtifactKind::Plan
        );
        let files = store.load_file_artifacts(session);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, user_file.to_string_lossy());

        let m = meta(session, &ago(30 * 24 * 3600));
        assert!(delete_session_files(&store, dd.path(), &m));
        assert!(!plan_path.exists(), "计划文件必须随会话删除");
        assert!(user_file.exists(), "用户项目文件绝不动");
        assert!(!store.artifacts_path(session).exists());
        assert!(!store.todos_path(session).exists());
        assert!(!store.history_path(session).exists());
    }

    /// 工具结果原样 sidecar 目录（sessions/<id>.toolres/）随会话删除，且会被索引外扫描认作残留。
    #[test]
    fn tool_results_dir_removed_with_session() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let session = "with-toolres";
        let dir = store.tool_results_dir(session);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("call-1.json"), b"{}").unwrap();
        assert!(dir.exists());

        let m = meta(session, &ago(30 * 24 * 3600));
        assert!(delete_session_files(&store, dd.path(), &m));
        assert!(!dir.exists(), "工具结果 sidecar 目录必须随会话删除");
        // 会话还在索引里时不算残留（避免把正在用的数据当孤儿删掉）
        assert!(
            !orphan_candidates(&store, cutoff_at(Utc::now(), 7))
                .iter()
                .any(|p| p.ends_with("with-toolres.toolres"))
        );
    }

    /// 图片 blob 目录（[docs/session-history-limits](../../../../../docs/session-history-limits.md)）：
    /// 会话自己的与子历史的（blob 归子历史自己）都随会话级联删除；会话不在索引里时
    /// `.imgblob` 目录会被索引外扫描认作残留（与 `.toolres` 同口径）。
    #[test]
    fn image_blob_dirs_cascade_and_are_scanned_as_orphans() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let session = "with-blobs";
        store
            .upsert_meta(meta(session, &ago(30 * 24 * 3600)))
            .unwrap();
        // 会话自己的 blob（带图保存）+ 子历史（带自己的 blob）
        store
            .save_history(
                session,
                "t",
                ".",
                None,
                None,
                &["/ws".into()],
                &[crate::core::types::Message::user_text("q"), image("AAAA")],
            )
            .unwrap();
        store
            .save_sub_history(session, "sub_1", &[image("BBBB")])
            .unwrap();
        assert!(store.image_blobs_dir(session).is_dir());
        assert!(store.sub_image_blobs_dir(session, "sub_1").is_dir());

        assert!(delete_session_files(
            &store,
            dd.path(),
            &meta(session, &ago(0))
        ));
        assert!(
            !store.image_blobs_dir(session).exists(),
            "会话的 blob 目录必须随会话删除"
        );
        assert!(
            !store.sub_image_blobs_dir(session, "sub_1").exists(),
            "子历史的 blob 目录必须随父会话删除"
        );

        // 索引外残留扫描：`.imgblob` 目录被认（会话还在索引里时不算）
        let ghost = "ghost-blobs";
        let dir = store.image_blobs_dir(ghost);
        std::fs::create_dir_all(&dir).unwrap();
        let mtime: DateTime<Utc> = std::fs::metadata(&dir).unwrap().modified().unwrap().into();
        assert!(
            !orphan_candidates(&store, cutoff_at(mtime, 0))
                .iter()
                .any(|p| p.ends_with("ghost-blobs.imgblob")),
            "刚创建的目录不算残留（mtime 不早于 cutoff）"
        );
        let after = mtime + chrono::Duration::seconds(1);
        assert!(
            orphan_candidates(&store, cutoff_at(after, 0))
                .iter()
                .any(|p| p.ends_with("ghost-blobs.imgblob")),
            "索引外的 .imgblob 目录应被认作残留"
        );
        assert_eq!(remove_orphan_files(&store, cutoff_at(after, 0)), 1);
        assert!(!dir.exists(), "残留的 .imgblob 目录应被递归删除");
    }

    /// 历史 / 子代理过程历史目录都随会话删除。
    #[test]
    fn history_and_sub_histories_removed() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let id = "sess-h";
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        std::fs::write(store.history_path(id), b"x").unwrap();
        let subs = store.sub_histories_dir(id).join("sub_1");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(subs.join("sub_1.json.gz"), b"x").unwrap();

        assert!(delete_session_files(&store, dd.path(), &meta(id, &ago(0))));
        assert!(!store.history_path(id).exists());
        assert!(!store.sub_histories_dir(id).exists());
    }

    /// 计划文件删除的路径白名单：必须同时是 `.md` 且位于托管的 `.codewave/tasks/` 下；
    /// 边车被外部改写成别的路径时只记 warn 不删（绝不让一条脏登记变成任意文件删除入口）。
    #[test]
    fn plan_file_deletion_requires_md_under_managed_tasks_dir() {
        // 纯函数本身：两条同时满足才放行（Windows 分隔符先归一）
        assert!(plan_path_allowed("/ws/.codewave/tasks/plan-1.md"));
        assert!(plan_path_allowed("C:\\ws\\.codewave\\tasks\\plan-1.md"));
        assert!(!plan_path_allowed("/ws/README.md"), "不在托管 tasks 目录");
        assert!(
            !plan_path_allowed("/ws/.codewave/tasks/plan-1.txt"),
            "后缀不是 .md"
        );
        assert!(!plan_path_allowed("/ws/.codewave/other/plan-1.md"));
        assert!(!plan_path_allowed(""));
        // 分段里的 `..` 一律拒绝：先满足字面条件（.md + .codewave/tasks/）再靠 `..` 回退出去的路径
        assert!(
            !plan_path_allowed("/ws/.codewave/tasks/../../../../tmp/evil.md"),
            "含 `..` 分段的路径必须拒绝（否则能删到托管目录之外）"
        );
        assert!(
            !plan_path_allowed("C:\\ws\\.codewave\\tasks\\..\\evil.md"),
            "Windows 分隔符归一后同样拒绝 `..` 分段"
        );
        // 反向边界：文件名里恰好连着两个点不算分段，仍在托管目录内，照常放行
        assert!(
            plan_path_allowed("/ws/.codewave/tasks/v1.2..3.md"),
            "`a..b.md` 是普通文件名，不得误拒"
        );

        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let ws = tempfile::tempdir().unwrap();
        let session = "sess-plan-guard";

        // 伪造一条不合规的 Plan 登记：指向用户的 markdown 文件
        let user_md = ws.path().join("README.md");
        std::fs::write(&user_md, "# hi").unwrap();
        store
            .append_artifact_kind(
                session,
                &user_md.to_string_lossy(),
                ArtifactOp::Create,
                ArtifactKind::Plan,
            )
            .unwrap();

        // 合规的计划文件（托管 tasks 目录 + .md）
        let ok_plan = dd
            .path()
            .join(".codewave/tasks/plan-20260919-120000-abcd.md");
        std::fs::create_dir_all(ok_plan.parent().unwrap()).unwrap();
        std::fs::write(&ok_plan, "# 计划").unwrap();
        store
            .append_artifact_kind(
                session,
                &ok_plan.to_string_lossy(),
                ArtifactOp::Create,
                ArtifactKind::Plan,
            )
            .unwrap();

        assert!(delete_session_files(
            &store,
            dd.path(),
            &meta(session, &ago(30 * 24 * 3600))
        ));
        assert!(user_md.exists(), "不合规的计划登记绝不能删掉用户文件");
        assert!(!ok_plan.exists(), "合规的计划文件仍要随会话删除");
    }

    /// 会话编号合法性：索引里混进含 `..` 的脏编号时，绝不拿它拼路径——
    /// 数据目录**外面**的文件不能被牵连删除，`histories/` 与 `histories/subs/` 结构完好。
    #[test]
    fn unsafe_session_id_is_skipped_without_touching_dirs() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("data");
        let store = store_in(&root);
        // 受害文件：脏编号 "../../evil" 拼出的历史路径正好指向它（root/histories/../../evil.json.gz）
        let victim = outer.path().join("evil.json.gz");
        std::fs::write(&victim, "x").unwrap();
        // 正常的目录结构（不能被破坏）
        let subs = store.histories_dir().join("subs").join("ok-sess");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(subs.join("sub_1.json.gz"), "x").unwrap();
        let ok_hist = store.histories_dir().join("ok-sess.json.gz");
        std::fs::write(&ok_hist, "x").unwrap();

        let mut bad = meta("../../evil", &real_ago(30 * 86_400));
        bad.running = false;
        assert!(!is_safe_session_id(&bad.id));
        for good in ["8f2c1a9e-1234-4abc-9def-001122334455", "sess-1"] {
            assert!(is_safe_session_id(good), "{good} 是合法编号");
        }
        for bad_id in ["", ".", "..", "a/b", "a\\b", "a.b", " lead"] {
            assert!(!is_safe_session_id(bad_id), "{bad_id} 是非法编号");
        }

        let outcome = execute(&store, &root, 1, &[bad]);
        assert_eq!(outcome.deleted, 0);
        assert_eq!(
            outcome.failed, 1,
            "非法编号跳过该会话并计入失败（索引行保留）"
        );
        assert!(outcome.ids.is_empty());
        assert!(victim.exists(), "数据目录外的文件绝不能被脏编号牵连删除");
        assert!(ok_hist.exists());
        assert!(subs.join("sub_1.json.gz").exists());
        assert!(store.histories_dir().join("subs").is_dir());
        assert!(store.histories_dir().is_dir());
    }

    // ---------- 批量执行 ----------

    /// 单条失败不中断整批；失败会话索引行保留；索引只写一次；状态文件每次都写。
    #[test]
    fn single_failure_keeps_row_and_writes_index_once() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());

        // 两条都过期：good 正常，bad 的历史路径被占成**非空目录**（remove_file 必失败）
        store
            .save_history(
                "good",
                "t",
                ".",
                None,
                None,
                &["/ws".into()],
                &[crate::core::types::Message::user_text("x")],
            )
            .unwrap();
        // 检查点会把 updated_at 刷成当前时间，重新写回旧时间才是「过期会话」
        store
            .upsert_meta(meta("good", &real_ago(30 * 86_400)))
            .unwrap();
        store
            .upsert_meta(meta("bad", &real_ago(30 * 86_400)))
            .unwrap();
        std::fs::create_dir_all(store.history_path("bad").join("inner")).unwrap();

        let candidates = select_expired(&store.load_index().sessions, Utc::now(), 1, &none());
        assert_eq!(candidates.len(), 2, "两条都应入选（磁盘状态不影响判定）");

        let before = store.index_write_count();
        let outcome = execute(&store, dd.path(), 1, &candidates);
        assert_eq!(outcome.deleted, 1);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.ids, vec!["good".to_string()]);
        // 整批只写一次索引（含移除 good 的那一次）
        assert_eq!(store.index_write_count(), before + 1, "索引只写一次");

        // 失败的会话索引行保留（下次清理再试），成功的消失
        assert!(store.get("bad").is_some());
        assert!(store.get("good").is_none());

        // 状态文件：每次执行都写（本次删除 1 失败 1）
        let status = read_status(dd.path());
        assert!(status.last_run_at.is_some());
        assert_eq!(status.last_deleted, 1);
        assert_eq!(status.last_failed, 1);
    }

    /// 删除 0 条也要写状态（设置页要显示「上次清理」）。
    #[test]
    fn status_written_even_with_zero_deletions() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        store.upsert_meta(meta("fresh", &real_ago(60))).unwrap();
        let outcome = run(&store, dd.path(), 1);
        assert_eq!(outcome.deleted, 0);
        assert_eq!(outcome.failed, 0);
        let status = read_status(dd.path());
        assert!(status.last_run_at.is_some());
        assert_eq!(status.last_deleted, 0);
        assert_eq!(status.last_failed, 0);
    }

    /// 一次性清理：过期会话消失、新鲜会话与运行中会话保留、被删 id 回传。
    #[test]
    fn run_deletes_expired_and_reports_ids() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        store
            .upsert_meta(meta("old", &real_ago(30 * 86_400)))
            .unwrap();
        store.upsert_meta(meta("new", &real_ago(60))).unwrap();
        store
            .upsert_meta(meta("running", &real_ago(30 * 86_400)))
            .unwrap();
        // running 标记只能经 mark_running 置位（upsert_meta 会按内存集合/索引现值裁决）
        store.mark_running("running", true).unwrap();

        let outcome = run(&store, dd.path(), 7);
        assert_eq!(outcome.deleted, 1);
        assert_eq!(outcome.ids, vec!["old".to_string()]);
        assert_eq!(outcome.failed, 0);
        let ids: Vec<String> = store.list().into_iter().map(|m| m.id).collect();
        assert!(ids.contains(&"new".to_string()));
        assert!(ids.contains(&"running".to_string()));
        assert!(!ids.contains(&"old".to_string()));
    }

    /// 状态文件损坏/缺失 → 读回默认值（绝不因状态文件炸掉命令）。
    #[test]
    fn status_read_is_defensive() {
        let dd = tempfile::tempdir().unwrap();
        assert!(read_status(dd.path()).last_run_at.is_none());
        std::fs::create_dir_all(dd.path().join("sessions")).unwrap();
        std::fs::write(status_path(dd.path()), b"not json").unwrap();
        assert!(read_status(dd.path()).last_run_at.is_none());
        // 原子写后可读回
        write_status(
            dd.path(),
            &CleanupStatus {
                last_run_at: Some("2026-09-19T00:00:00+00:00".into()),
                last_deleted: 3,
                last_failed: 1,
            },
        );
        let s = read_status(dd.path());
        assert_eq!(s.last_deleted, 3);
        assert_eq!(s.last_failed, 1);
    }

    /// 索引写入失败时的报告不得失真：文件已删、索引未更新 → `deleted` 为 0、
    /// 受影响的会话计入 `failed`（下次清理自愈），状态文件与之一致。
    #[test]
    fn index_write_failure_counts_into_failed() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        let id = "gone";
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        std::fs::write(store.history_path(id), b"x").unwrap();
        // 让索引写入必定失败：index.json 被占成**目录**（原子写的 rename 无法覆盖目录）
        std::fs::create_dir_all(store.sessions_dir().join("index.json")).unwrap();

        let outcome = execute(&store, dd.path(), 1, &[meta(id, &real_ago(30 * 86_400))]);
        assert!(!store.history_path(id).exists(), "文件确实已删");
        assert_eq!(outcome.deleted, 0, "索引没更新成功就不能报已删除");
        assert!(outcome.ids.is_empty());
        assert_eq!(outcome.failed, 1, "索引写失败要把受影响的会话计入失败");
        let status = read_status(dd.path());
        assert!(status.last_run_at.is_some());
        assert_eq!(status.last_deleted, 0);
        assert_eq!(status.last_failed, 1);
    }

    /// 日志里的删除清单：数量少时列全，超过上限时截断但说明总数
    ///（启动那次清理没有界面，日志是唯一痕迹）。
    #[test]
    fn deleted_ids_log_line_truncates_but_keeps_total() {
        let ids = |n: usize| (0..n).map(|i| format!("s{i}")).collect::<Vec<_>>();
        assert_eq!(format_deleted_ids(&ids(3)), "s0, s1, s2");
        assert_eq!(
            format_deleted_ids(&ids(LOG_ID_LIMIT)).split(", ").count(),
            LOG_ID_LIMIT
        );

        let line = format_deleted_ids(&ids(LOG_ID_LIMIT + 5));
        assert!(line.starts_with("s0, s1, s2, "), "{line}");
        assert!(
            !line.contains(&format!("s{},", LOG_ID_LIMIT)),
            "超出上限的编号不再逐个列出：{line}"
        );
        assert!(
            line.contains(&format!("共 {} 个", LOG_ID_LIMIT + 5)),
            "必须说明总数：{line}"
        );
    }

    // ---------- 索引外残留 ----------

    /// 索引外残留清理：命中孤儿、不误伤 `sub_`/`task_` 前缀、不碰 index.json、不删索引内的会话文件。
    #[test]
    fn orphan_cleanup_hits_orphans_only() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        std::fs::create_dir_all(store.histories_dir().join("subs")).unwrap();
        std::fs::create_dir_all(store.sessions_dir().join("subs")).unwrap();

        let old = |name: &str| {
            let p = dd.path().join(name);
            std::fs::write(&p, "x").unwrap();
            age_file(&p, 30);
            p
        };
        let orphan_gz = old("histories/orphan-1.json.gz");
        let orphan_todos = old("sessions/orphan-1.todos.json");
        let orphan_artifacts = old("sessions/orphan-2.artifacts.json");
        let known_gz = old("histories/known.json.gz");
        let sub_gz = old("histories/sub_deadbeef.json.gz");
        let task_gz = old("histories/task_daily-1.json.gz");
        let task_todos = old("sessions/task_daily-1.todos.json");

        // 索引写在最后（否则会被上面的占位文件覆盖）：known 在索引里 → 它不是孤儿
        store.upsert_meta(meta("known", &real_ago(0))).unwrap();

        let cutoff = Utc::now() - chrono::Duration::hours(24);
        let removed = remove_orphan_files(&store, cutoff);
        assert_eq!(removed, 3, "孤儿 gz + 两个孤儿边车");
        assert!(!orphan_gz.exists());
        assert!(!orphan_todos.exists());
        assert!(!orphan_artifacts.exists());
        assert!(known_gz.exists(), "索引里的会话不得被当孤儿删");
        assert!(sub_gz.exists(), "sub_ 前缀（子代理过程历史）不得误删");
        assert!(task_gz.exists(), "task_ 前缀（计划任务）不得误删");
        assert!(task_todos.exists());
        assert!(
            store.sessions_dir().join("index.json").exists(),
            "清理绝不碰 index.json"
        );
        assert!(
            store.sessions_dir().join("subs").is_dir(),
            "sessions/subs 目录不碰"
        );
        assert!(store.histories_dir().join("subs").is_dir());
    }

    /// 修改时间未早于 cutoff 的孤儿**不删**（保留期是硬门槛）。
    #[test]
    fn orphan_cleanup_keeps_recent_files() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        // 先写一份可解析的索引（索引不可信时根本不会走到孤儿扫描）
        store.upsert_meta(meta("known", &real_ago(0))).unwrap();
        let fresh = dd.path().join("histories/orphan-fresh.json.gz");
        std::fs::write(&fresh, b"x").unwrap();
        let cutoff = Utc::now() - chrono::Duration::hours(24);
        assert_eq!(remove_orphan_files(&store, cutoff), 0);
        assert!(fresh.exists());
    }

    /// 🔴 索引不可读时**绝不**扫索引外残留：`load_index()` 在「文件缺失 / 读失败 / 解析失败」
    /// 三种情况下都返回空索引，而空索引与「用户真的没有会话」无法区分——照常扫孤儿会把
    /// 全部历史文件当孤儿删掉，且预览还报 0 条（前端连确认框都不弹）。
    #[test]
    fn orphan_cleanup_is_skipped_when_index_is_untrusted() {
        let cases: [(&str, Option<&[u8]>); 2] = [
            ("索引文件不存在", None),
            ("索引是坏 JSON", Some(&b"{ not json"[..])),
        ];
        for (label, index_bytes) in cases {
            let dd = tempfile::tempdir().unwrap();
            let store = store_in(dd.path());
            std::fs::create_dir_all(store.histories_dir()).unwrap();
            std::fs::create_dir_all(store.sessions_dir()).unwrap();
            let old_hist = dd.path().join("histories/x.json.gz");
            std::fs::write(&old_hist, "x").unwrap();
            age_file(&old_hist, 30);
            let old_todos = dd.path().join("sessions/x.todos.json");
            std::fs::write(&old_todos, "[]").unwrap();
            age_file(&old_todos, 30);
            if let Some(bytes) = index_bytes {
                std::fs::write(store.sessions_dir().join("index.json"), bytes).unwrap();
            }

            let cutoff = Utc::now() - chrono::Duration::hours(24);
            assert_eq!(
                remove_orphan_files(&store, cutoff),
                0,
                "{label}：一个文件都不删"
            );
            assert!(old_hist.exists(), "{label}：旧历史必须原样保留");
            assert!(old_todos.exists(), "{label}：旧边车必须原样保留");
            // 预览与执行同一口径：不可信时报 0，绝不会出现「预览 0 条、执行却全删」
            assert_eq!(count_orphan_files(&store, cutoff), 0, "{label}");
            // 走完整清理路径同理（execute 里的孤儿扫描也受这道守卫保护）
            execute(&store, dd.path(), 1, &[]);
            assert!(old_hist.exists(), "{label}：execute 路径也不得删");
            assert!(old_todos.exists(), "{label}");
        }
    }

    /// 🔴 真实启动序列（不是手工构造 store）：磁盘索引损坏 → 既有启动清扫 `purge_non_session_entries()`
    /// 走 `mutate_index` → `load_index()` 把损坏索引改名成 `index.json.corrupt` 并回默认，
    /// 同一函数**必定**把一份合法空索引写回 `index.json` → 紧随其后的清理 `execute()` 里
    /// 「文件存在且能解析」又变成真。
    /// 若判据只看当下这一份文件，守卫就被同一轮启动序列顶掉：一份空索引把 30 天前的历史与边车
    /// 当孤儿删光（判据还被偷换成文件修改时间，用户近期打开过、很久没写入的会话照样中招，
    /// 而且启动路径没有确认框可问）。
    /// 修法：`index.json.corrupt` 存在 = 索引曾损坏未复原 = 持久不可信信号，本次一概不扫残留。
    #[test]
    fn orphan_cleanup_skipped_after_startup_rebuilds_corrupt_index() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        std::fs::create_dir_all(store.sessions_dir()).unwrap();

        // 用户数据：60 天前的历史与边车（远超 30 天保留期；“很久没写入但近期打开过”的会话即此形态）
        let hist = dd.path().join("histories/x.json.gz");
        std::fs::write(&hist, "x").unwrap();
        age_file(&hist, 60);
        let todos = dd.path().join("sessions/x.todos.json");
        std::fs::write(&todos, "[]").unwrap();
        age_file(&todos, 60);

        // ① 磁盘上索引损坏
        std::fs::write(store.sessions_dir().join("index.json"), b"{ not json").unwrap();

        // ② 真实启动序列里的既有清扫：改名保全 → 写回合法空索引（0 条非会话条目可清）
        assert_eq!(store.purge_non_session_entries(), 0);
        assert!(
            store.corrupt_index_backup_path().exists(),
            "损坏索引必须已改名保全（持久信号已落盘）"
        );
        assert!(
            store.load_index().sessions.is_empty(),
            "此刻 index.json 已是一份可解析的空索引——正是守卫会被顶掉的那一刻"
        );
        assert!(
            !store.index_is_trusted(),
            "索引曾损坏未复原 → 不可信（不能只看 index.json 当下是否可解析）"
        );

        // ③ 紧随其后的启动清理：一个文件都不许删
        const DAYS: u32 = 30;
        assert_eq!(
            count_orphan_files(&store, cutoff_at(Utc::now(), DAYS)),
            0,
            "预览口径也必须是 0（启动路径没有确认框，非 0 才有机会提醒）"
        );
        let outcome = execute(&store, dd.path(), DAYS, &[]);
        assert_eq!(outcome.deleted, 0);
        assert_eq!(outcome.failed, 0);
        assert!(hist.exists(), "索引曾损坏 → 旧历史不得当孤儿删");
        assert!(todos.exists(), "索引曾损坏 → 旧边车不得当孤儿删");
        assert!(
            store.corrupt_index_backup_path().exists(),
            "保全证据不得被清理顺手删掉"
        );
    }

    /// 索引可信且里面确实没有这个会话时，同一个孤儿仍要被删（不因上面的守卫而退化）。
    #[test]
    fn orphan_cleanup_still_deletes_when_index_is_trusted() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        // 用户真的没有会话：索引能解析、sessions 为空
        store
            .upsert_meta(meta("ghost-in-index", &real_ago(0)))
            .unwrap();
        store.remove_many(&["ghost-in-index".to_string()]).unwrap();
        assert!(store.index_is_trusted(), "索引仍是可解析的空列表");
        assert!(store.load_index().sessions.is_empty());

        let orphan = dd.path().join("histories/y.json.gz");
        std::fs::write(&orphan, "x").unwrap();
        age_file(&orphan, 30);
        let cutoff = Utc::now() - chrono::Duration::hours(24);
        assert_eq!(count_orphan_files(&store, cutoff), 1);
        assert_eq!(remove_orphan_files(&store, cutoff), 1);
        assert!(!orphan.exists());
    }

    /// 预览口径与执行口径对齐：只有索引外残留可删时，`orphan_count` 也必须是非 0。
    #[test]
    fn preview_reports_orphan_count() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        store.upsert_meta(meta("fresh", &real_ago(60))).unwrap();
        let orphan = dd.path().join("histories/orphan-p.json.gz");
        std::fs::write(&orphan, "x").unwrap();
        age_file(&orphan, 30);

        let p = preview_all(&store, &store.load_index().sessions, Utc::now(), 1, &none());
        assert_eq!(p.count, 0, "会话条数仍只算会话");
        assert!(p.titles.is_empty());
        assert_eq!(p.orphan_count, 1, "索引外残留要计进预览");

        // 执行后与预览一致：残留没了，会话条数也没变
        let outcome = execute(&store, dd.path(), 1, &[]);
        assert_eq!(outcome.deleted, 0);
        assert!(!orphan.exists());
        assert_eq!(
            count_orphan_files(&store, Utc::now() - chrono::Duration::hours(24)),
            0
        );
    }

    /// 索引外的孤儿随 `execute` 一起清掉，且索引行数与状态文件一致。
    #[test]
    fn execute_also_cleans_orphans() {
        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        // 索引可解析（只是里面没有这个孤儿）：它按「不在索引里」被清掉
        store.upsert_meta(meta("kept", &real_ago(60))).unwrap();
        let orphan = dd.path().join("histories/orphan-9.json.gz");
        std::fs::write(&orphan, "x").unwrap();
        age_file(&orphan, 30);
        let outcome = execute(&store, dd.path(), 1, &[]);
        assert_eq!(outcome.deleted, 0);
        assert!(!orphan.exists());
        assert!(read_status(dd.path()).last_run_at.is_some());
    }

    /// 旧边车（无 `kind`）透明读为普通产物（serde default，不做迁移）。
    #[test]
    fn legacy_sidecar_without_kind_loads_as_file() {
        let dd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dd.path().join("sessions")).unwrap();
        std::fs::write(
            dd.path().join("sessions/old.artifacts.json"),
            br#"[{"path":"/ws/a.md","first_op":"create","last_op":"edit","first_at":"2026-01-01T00:00:00+00:00","last_at":"2026-01-02T00:00:00+00:00","count":2}]"#,
        )
        .unwrap();
        let store = store_in(dd.path());
        let items = store.load_artifacts("old");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ArtifactKind::File);
        // 缺 kind 的条目不进计划文件删除范围 → 会话删除不会连带删它
        assert_eq!(store.load_file_artifacts("old").len(), 1);
    }

    /// 🔴 子历史的 blob 目录（sessions/<父>__<sub>.imgblob/）绝不参与索引外残留清理。
    ///
    /// 它的「id」（`<父>__<sub>`）既不在会话索引里、也不带 `sub_` 前缀，按「不在索引即孤儿」
    /// 的旧判据会被当残留删掉——而父会话仍活着、其子历史还引用着那些图。
    /// 子 blob 目录一律由级联删除路径负责（`remove` / `delete_session_files`）。
    #[test]
    fn sub_history_blob_dirs_are_never_orphans() {
        // 判据本身
        assert!(is_sub_blob_owner("parent-a__sub_deadbeef"));
        assert!(is_sub_blob_owner("parent-a__task_daily-1"));
        assert!(!is_sub_blob_owner("parent-a"), "没有分隔符 = 普通会话 id");
        assert!(!is_sub_blob_owner("8f2c1a9e-1234-4abc-9def-001122334455"));
        assert!(!is_sub_blob_owner("__sub_x"), "父会话编号为空不算");
        assert!(
            !is_sub_blob_owner("parent-a__other"),
            "后缀不是 sub_/task_ 不算"
        );

        let dd = tempfile::tempdir().unwrap();
        let store = store_in(dd.path());
        store.upsert_meta(meta("parent-a", &ago(0))).unwrap();
        store
            .save_sub_history("parent-a", "sub_deadbeef", &[image(&"A".repeat(64))])
            .unwrap();
        let blob_dir = store.sub_image_blobs_dir("parent-a", "sub_deadbeef");
        assert!(blob_dir.is_dir());

        // cutoff 取目录 mtime 之后 1 秒：若无守卫，这个目录正是「索引外 + 够旧」的残留
        let mtime: DateTime<Utc> = std::fs::metadata(&blob_dir)
            .unwrap()
            .modified()
            .unwrap()
            .into();
        let cutoff = cutoff_at(mtime, 0) + chrono::Duration::seconds(1);
        assert!(
            !orphan_candidates(&store, cutoff)
                .iter()
                .any(|p| p == &blob_dir),
            "子历史的 blob 目录不得被认作索引外残留"
        );
        assert_eq!(count_orphan_files(&store, cutoff), 0);
        assert_eq!(remove_orphan_files(&store, cutoff), 0);
        assert!(blob_dir.is_dir(), "子历史的 blob 目录不得被残留清理删掉");
    }
}
