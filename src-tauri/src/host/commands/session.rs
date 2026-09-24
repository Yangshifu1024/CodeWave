use super::util::{Core, err};
use crate::core::agent::Frame;
use crate::core::sessions::{
    HistoryBoundary, HistoryFormat, HistoryLoad, SessionStore, persist, segments,
};
use crate::core::types::Message;
use crate::host::events::ChannelRegistry;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;
use tauri::ipc::Channel;

/// [docs/session-restore-fidelity](../../../../docs/session-restore-fidelity.md)：按 provider 侧 tool_use id
/// 批量回读「工具结果原样 sidecar」——历史里那份模型侧文本被截断时，前端据此把卡片补回完整内容
/// （大 html、grep 大量命中、网页正文、文档读取、edit 列表、ask 载荷、子代理汇报）。
/// 缺失 / 非法键 / 超限的条目静默跳过：返回的就是实际有的那些，前端无需区分「无备份」与「读失败」。
#[tauri::command]
pub async fn load_tool_outcomes(
    core: Core<'_>,
    session_id: String,
    call_ids: Vec<String>,
) -> Result<Vec<serde_json::Value>, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    // 归属口诀与产物登记一致：子代理产生的卡片跟主会话走
    let owner = rt.root_session_id.clone().unwrap_or_else(|| rt.id.clone());
    Ok(tool_outcomes_payload(&core.store, &owner, &call_ids))
}

/// 响应体（独立成函数便于测试）：字段名与其它边车一致（snake_case）。
fn tool_outcomes_payload(
    store: &crate::core::sessions::SessionStore,
    owner: &str,
    call_ids: &[String],
) -> Vec<serde_json::Value> {
    crate::core::sessions::tool_results::load_many(store, owner, call_ids)
        .into_iter()
        .map(|(call_id, rec)| {
            serde_json::json!({
                "call_id": call_id,
                "outcome": rec.outcome,
                "duration_ms": rec.duration_ms,
            })
        })
        .collect()
}

#[cfg(test)]
mod tool_outcomes_tests {
    use super::*;

    #[test]
    fn payload_maps_records_and_skips_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::core::sessions::SessionStore::new(dir.path().to_path_buf());
        let out = crate::tools::ToolOutcome::ok(serde_json::json!({ "html": "<b>hi</b>" }));
        crate::core::sessions::tool_results::save(&store, "s1", "call-1", &out, None);

        let rows =
            tool_outcomes_payload(&store, "s1", &["call-1".to_string(), "missing".to_string()]);
        assert_eq!(rows.len(), 1, "缺失的键不进响应体");
        assert_eq!(rows[0]["call_id"], "call-1");
        // 存的是整套 ToolOutcome 信封（ok/data/...），前端据此连状态一起还原
        assert_eq!(rows[0]["outcome"]["ok"], serde_json::json!(true));
        assert_eq!(rows[0]["outcome"]["data"]["html"], "<b>hi</b>");
        assert!(rows[0]["duration_ms"].is_null());

        // 空入参 / 未知会话：空数组（不报错）
        assert!(tool_outcomes_payload(&store, "s1", &[]).is_empty());
        assert!(tool_outcomes_payload(&store, "nope", &["call-1".to_string()]).is_empty());
    }
}

/// 创建会话：项目会话（快照固化主目录）或临时会话（免目录）双形态入口。
#[tauri::command]
pub async fn create_session(
    core: Core<'_>,
    project_id: Option<String>,
    workspace: Option<String>,
) -> Result<serde_json::Value, String> {
    // 双形态（需求 1.1–1.5）：挂项目（单目录语义，数据在 <主目录>/.codewave 下）
    // 或临时会话（不传 workspace → 全局数据目录；免选目录直接开聊）
    let (primary, extra, pid, project_dir, project_entry) = if let Some(pid) = project_id {
        let project = crate::core::projects::find(&core.data_dir, &pid).ok_or("项目不存在")?;
        let primary = std::path::PathBuf::from(&project.directory);
        if !primary.is_dir() {
            return Err(format!("项目目录不存在：{}", project.directory));
        }
        let pd = crate::core::projects::project_data_dir(&core.data_dir, &project);
        crate::core::projects::save_project(&core.data_dir, &project).map_err(err)?; // 顺带确保子目录存在
        // [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：项目设置里已允许的外部目录
        // 直接作为额外根带动（只放行仍然存在的目录；目录被删掉就把这条丢掉，不让它一直躺在设置里）
        let allowed: Vec<String> = project
            .allowed_dirs
            .iter()
            .filter(|d| std::path::Path::new(d).is_dir())
            .cloned()
            .collect();
        (primary, allowed, Some(pid), Some(pd), Some(project))
    } else if let Some(ws) = workspace {
        let path = std::path::PathBuf::from(&ws);
        if !path.is_dir() {
            return Err(format!("目录不存在：{ws}"));
        }
        (path, Vec::new(), None, None, None)
    } else {
        // 临时会话免目录：挂全局数据目录（写入仍走 fence 审批，不越界）
        (core.data_dir.clone(), Vec::new(), None, None, None)
    };
    let id = uuid::Uuid::new_v4().to_string();
    let primary_str = primary.to_string_lossy().into_owned();
    let mut roots = vec![primary_str.clone()];
    roots.extend(extra.iter().cloned());
    core.get_or_create_session(&id, primary, pid.clone(), roots.clone(), project_dir, extra);
    if let Some(p) = project_entry {
        return Ok(serde_json::json!({
            "session_id": id,
            "workspace": primary_str,
            "project_id": pid,
            "project_name": p.name,
            "roots": roots,
        }));
    }
    Ok(serde_json::json!({
        "session_id": id,
        "workspace": primary_str,
        "project_id": pid,
        "roots": roots,
    }))
}

/// 列出会话索引（SessionMeta 含 project_id + roots 快照）。
#[tauri::command]
pub async fn list_sessions(
    core: Core<'_>,
) -> Result<Vec<crate::core::sessions::SessionMeta>, String> {
    Ok(core.store.list())
}

/// 首屏分页信息（`load_session` 的 `paging` 字段；键名与前端契约逐字对应，**勿改名**）。
#[derive(Debug, Clone, Serialize)]
pub struct SessionPaging {
    /// 落盘格式：`new`（段式 JSONL）/ `legacy`（旧 `.json.gz`，读兼容不迁移）
    pub format: HistoryFormat,
    /// 本次 `messages` 覆盖的最早段序号（legacy / 无段时为 0）
    pub loaded_from_seq: u32,
    /// 段文件总数（legacy 记 1）
    pub segment_count: usize,
    /// 磁盘上的消息总数（display 口径：全部 `message` 记录，**不 trim**）
    pub total_messages: usize,
    /// 该会话历史占用的字节数（段文件总和；legacy = 该文件字节数）
    pub bytes: u64,
    /// 本次向前翻找时**跳过**的坏段数（0 = 全部可读；legacy 恒 0——段此刻不是数据源）
    pub bad_segments: usize,
    /// 更早是否还有**可读**的段（false = 已到最早）
    pub has_more: bool,
}

/// `load_session` 的返回形态（首屏）。
#[derive(Debug, Clone, Serialize)]
pub struct LoadSessionPayload {
    /// 首屏消息：**最近一段**（display 口径：完整、不 trim）；legacy 回落整份
    pub messages: Vec<Message>,
    /// 压缩 / 改写边界（**只含本次覆盖段范围内**：前端按 `seq` 与已加载项对齐画分隔线）
    pub boundaries: Vec<HistoryBoundary>,
    /// 分页信息
    pub paging: SessionPaging,
}

/// `load_session_earlier` 的返回形态。
#[derive(Debug, Clone, Serialize)]
pub struct EarlierPage {
    /// 严格早于 `before_seq` 的**最近一段**的 message 记录（display 口径；无更早段时空数组）
    pub messages: Vec<Message>,
    /// 本次返回的段序号（0 = 没有更早的段，此时 `has_more` 必为 false）
    pub from_seq: u32,
    /// 更早是否还有**可读**的段（false = 已到最早）
    pub has_more: bool,
    /// 本次覆盖段范围内的压缩边界
    pub boundaries: Vec<HistoryBoundary>,
    /// 本次翻页跳过的坏段数（口径与 `paging.bad_segments` 一致）
    pub bad_segments: usize,
}

/// 恢复会话：首屏只读**最近一段**历史 + 按 meta 快照重建 runtime，并回填运行时标题。
///
/// 自批2 P2 起返回 `{ messages, paging }`：
/// - `messages` = **display 口径**的最近一段（完整转录、不 trim），更早内容由
///   [`load_session_earlier`] 按段向前加载（取消 8MB 上限后历史可以很长，整表渲染撑不住）；
/// - `paging` = 分页元信息（格式 / 已加载段序号 / 段数 / 总条数 / 字节数 / 是否还有更早）。
///
/// 取数**全部有界**（批2 P4「打开会话不再 O(全部历史字节)」）：wire 走
/// [`SessionStore::load_history_wire`]（只从段尾向前读到够用，`rt.history` 的初始化口径与旧实现
/// 逐字节一致），首屏与分页元信息走段目录（[`segments::summarize`] 的段头 / 段尾窗口 + 单段按段读）；
/// 旧格式（`.json.gz`，没有段可分页）回落整份读回，代价以该文件大小（旧上限 8MB）为界。
/// 取数入口见 [`load_for_open`]。
#[tauri::command]
pub async fn load_session(
    core: Core<'_>,
    session_id: String,
    workspace: String,
) -> Result<LoadSessionPayload, String> {
    // 有界取数：运行上下文（wire）与旧格式那份整份历史都从这一处出
    let (wire, legacy) = load_for_open(&core.store, &session_id).map_err(err)?;
    // [docs/session-cleanup](../../../../docs/session-cleanup.md)：打开即刷新「最近打开时间」（10 分钟节流，
    // 避免每次打开都重写整份索引；失败只记告警，绝不影响打开会话）
    if let Err(e) = core
        .store
        .touch_session_open(&session_id, chrono::Utc::now())
    {
        tracing::warn!("会话 {session_id} 最近打开时间落盘失败：{e}");
    }
    // 项目快照加载：project_id/roots 以 meta 为准（legacy 免目录会话自然回退单根）
    let (pid, roots, project_dir, meta_title) = match core.store.get(&session_id) {
        Some(meta) => {
            let mut roots = meta.roots;
            if roots.is_empty() {
                roots = vec![workspace.clone()];
            }
            // [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：项目设置里已允许的外部目录
            // 在重开会话时重新带上（meta 快照可能还是放行之前写的），否则用户会莫名其妙再被问一次
            if let Some(pid) = meta.project_id.as_deref()
                && let Some(entry) = crate::core::projects::find(&core.data_dir, pid)
            {
                for d in &entry.allowed_dirs {
                    if std::path::Path::new(d).is_dir() && !roots.iter().any(|r| r == d) {
                        roots.push(d.clone());
                    }
                }
            }
            let pd = meta
                .project_id
                .as_ref()
                .and_then(|pid| crate::core::projects::project_data_dir_by_id(&core.data_dir, pid));
            (meta.project_id, roots, pd, Some(meta.title))
        }
        None => (None, vec![workspace.clone()], None, None),
    };
    let path = std::path::PathBuf::from(&workspace);
    let extra: Vec<String> = roots.iter().skip(1).cloned().collect();
    let rt = core.get_or_create_session(&session_id, path, pid, roots, project_dir, extra);
    if rt.history.lock().unwrap().is_empty() {
        // 运行上下文（wire）口径与旧实现一致；**绝不把首屏那段 display 当上下文**——
        // 那会让下一次检查点把整份历史改写成「只有最近一段」的基线段（时间线重置）
        *rt.history.lock().unwrap() = wire;
    }
    // 标题回填：重开的旧会话运行时 title 为空；不回填会让 start_chat 的
    // 10 字符启发式覆盖已有标题并经 checkpoint 持久化（[docs/session-auto-title](../../../../docs/session-auto-title.md)）
    {
        let mut t = rt.title.lock().unwrap();
        if t.is_empty() {
            *t = meta_title.unwrap_or_default();
        }
    }
    Ok(first_page(&core.store, &session_id, legacy.as_ref()))
}

/// 打开会话的**有界**取数（批2 P4）：返回 `(运行上下文 wire, 旧格式那份整份历史)`。
///
/// - 有段目录（新格式）：wire 走 [`SessionStore::load_history_wire`]（只从段尾向前读到够用），
///   首屏与分页元信息由 [`first_page`] 按段现读（旧格式那份传 `None`）；
/// - 无段目录（旧 `.json.gz`）：整份读回（本来就有界，旧上限 8MB），首屏直接用它的
///   `display` / `bytes`；此时 `wire` 多一份 clone——旧格式两边同源（`wire == display`），
///   且历史有旧上限，代价可忽略。
///
/// 错误语义与旧实现（直接 `load_history_full`）一致：两种格式都没落盘时返回 `Err`。
fn load_for_open(
    store: &SessionStore,
    id: &str,
) -> anyhow::Result<(Vec<Message>, Option<HistoryLoad>)> {
    if store.reads_new_format(id) {
        return Ok((store.load_history_wire(id)?, None));
    }
    // 无段目录 ⇒ `load_history_full` 必走旧格式回落（读兼容、不迁移）
    let full = store.load_history_full(id)?;
    Ok((full.wire.clone(), Some(full)))
}

/// 首屏分页载荷（独立成函数便于测试）：新格式**只读最近一段**；旧格式回落整份。
///
/// `legacy` = 旧格式（无段目录）那份整份历史，新格式传 `None`——旧格式没有段可分页，首屏即整份
///（与旧行为一致）。新格式**只读**：段目录摘要（段头 / 段尾窗口，不整段解析）+ 最新一个可读段。
///
/// `messages` 是 **display 口径**（不 trim）：wire 是给模型看的（最后一个重置点之后 +
/// repair + trim），把它交给界面会让已被压缩掉的轮次凭空消失。
fn first_page(store: &SessionStore, id: &str, legacy: Option<&HistoryLoad>) -> LoadSessionPayload {
    // 旧格式（`.json.gz`）没有段：整份照旧给出（与旧行为一致），没有更早内容
    if let Some(full) = legacy {
        return LoadSessionPayload {
            messages: full.display.clone(),
            boundaries: Vec::new(),
            paging: SessionPaging {
                format: HistoryFormat::Legacy,
                loaded_from_seq: 0,
                segment_count: 1,
                total_messages: full.display.len(),
                bytes: full.bytes,
                bad_segments: 0,
                has_more: false,
            },
        };
    }
    let dir = store.history_dir(id);
    let summary = segments::summarize(&dir);
    let meta = |loaded_from_seq: u32, has_more: bool, bad_segments: usize| SessionPaging {
        format: HistoryFormat::New,
        loaded_from_seq,
        segment_count: summary.segment_count,
        total_messages: summary.total_messages,
        bytes: summary.bytes,
        bad_segments,
        has_more,
    };
    let segments::PageRead {
        segment,
        boundaries,
        bad_segments,
    } = segments::read_page(&dir, None);
    match segment {
        Some(seg) => {
            let seq = seg.info.seq;
            LoadSessionPayload {
                messages: persist::from_persisted(store, id, seg.messages),
                boundaries,
                paging: meta(
                    seq,
                    segments::has_readable_segments_before(&dir, seq),
                    bad_segments,
                ),
            }
        }
        // 目录存在却一个可读段都没有（新建后即崩溃 / 全是坏段）：空首屏，
        // 如实报「没有更早内容」+ 本次跳过的坏段数（界面据此提示，plan §5-P2）
        None => LoadSessionPayload {
            messages: Vec::new(),
            boundaries,
            paging: meta(0, false, bad_segments),
        },
    }
}

/// 加载更早一段历史（独立成函数便于测试）：**只读一个段文件**，不扫描其它段。
///
/// 越界 / 段不存在 / 该段损坏（跳过它继续向前）→ 空数组 + `has_more = false` + `from_seq = 0`，
/// **不报错**（翻页失败不该弹错，界面只需知道「没有更早内容了」）。
///
/// `from_seq = 0` 是刻意的：0 不是合法段序号（段号从 1 起），调用方据此一眼看出本次没有加载到段。
fn earlier_page(store: &SessionStore, id: &str, before_seq: u32) -> EarlierPage {
    let dir = store.history_dir(id);
    // 旧格式（此刻新格式不是权威数据源）没有段可分页；新目录缺失同理：没有更早内容
    if !store.reads_new_format(id) {
        return EarlierPage {
            messages: Vec::new(),
            from_seq: 0,
            has_more: false,
            boundaries: Vec::new(),
            bad_segments: 0,
        };
    }
    let segments::PageRead {
        segment,
        boundaries,
        bad_segments,
    } = segments::read_page(&dir, Some(before_seq));
    match segment {
        Some(seg) => {
            let seq = seg.info.seq;
            EarlierPage {
                messages: persist::from_persisted(store, id, seg.messages),
                from_seq: seq,
                has_more: segments::has_readable_segments_before(&dir, seq),
                boundaries,
                bad_segments,
            }
        }
        None => EarlierPage {
            messages: Vec::new(),
            from_seq: 0,
            has_more: false,
            boundaries,
            bad_segments,
        },
    }
}

/// 加载更早一段历史（分页向前翻）。
///
/// 只读**一个**段文件（坏段跳过继续向前），翻页代价与已加载内容量无关；
/// 越界 / 没有更早内容时返回空数组 + `has_more = false`（不报错）。
#[tauri::command]
pub async fn load_session_earlier(
    core: Core<'_>,
    session_id: String,
    before_seq: u32,
) -> Result<EarlierPage, String> {
    Ok(earlier_page(&core.store, &session_id, before_seq))
}

/// 读取子代理过程历史（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)）：会话恢复后，过程抽屉按需重建完整消息流。
/// 旧会话 / 无落盘时返回空数组（前端降级为任务 + 最终汇报展示）。
#[tauri::command]
pub async fn load_subagent_history(
    core: Core<'_>,
    session_id: String,
    sub_id: String,
) -> Result<Vec<Message>, String> {
    core.store
        .load_sub_history(&session_id, &sub_id)
        .map_err(err)
}

/// 删除会话：运行中拒绝；先标 zombie 再移除（防迟到写入复活）。
#[tauri::command]
pub async fn delete_session(core: Core<'_>, session_id: String) -> Result<(), String> {
    // M14 修复：删除运行中的会话会被后续 checkpoint 复活 → 拒绝
    if let Some(rt) = core.session(&session_id) {
        if rt.running.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("会话正在运行，请先停止再删除".into());
        }
    }
    // H5 同根因（[docs/session-auto-title](../../../../docs/session-auto-title.md) 评审修复）：run 已结束但自动命名后台任务（最长 20s）可能仍在途；
    // 标 zombie 让迟到的 session_log 写入 / 统计记账无法复活已删除会话
    if let Some(rt) = core.session(&session_id) {
        rt.zombie.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    core.sessions.remove(&session_id);
    core.store.remove(&session_id).map_err(err)
}

/// 重命名会话：运行时有 runtime 则同步标题并全量重写历史；否则仅改索引。
#[tauri::command]
pub async fn rename_session(
    core: Core<'_>,
    session_id: String,
    title: String,
) -> Result<(), String> {
    match core.session(&session_id) {
        Some(rt) => {
            *rt.title.lock().unwrap() = title.clone();
            let ws = rt.workspace.to_string_lossy().into_owned();
            let history = rt.history.lock().unwrap().clone();
            let model_id = {
                let cfg = core.cfg.read().unwrap();
                crate::core::prefs::effective_model(&cfg, &rt.prefs()).map(|m| m.id.clone())
            };
            core.store
                .save_history(
                    &session_id,
                    &title,
                    &ws,
                    model_id.as_deref(),
                    rt.project_id.as_deref(),
                    &rt.roots,
                    &history,
                )
                .map_err(err)?;
        }
        _ => {
            core.store
                .rename_in_index(&session_id, &title)
                .map_err(err)?;
        }
    }
    Ok(())
}

// ---------- 运行 ----------

/// 发起一轮对话：注册前端 channel 后转调 core 启动 run。
#[tauri::command]
pub async fn start_chat(
    core: Core<'_>,
    channels: State<'_, Arc<ChannelRegistry>>,
    session_id: String,
    text: String,
    images: Option<Vec<crate::core::prefs::ImageIn>>,
    on_event: Channel<Frame>,
) -> Result<String, String> {
    let rt = core
        .session(&session_id)
        .ok_or_else(|| "会话不存在，请先创建会话".to_string())?;
    channels.map.insert(session_id.clone(), on_event);
    let run_id = core
        .start_chat(rt.clone(), text, images.unwrap_or_default())
        .map_err(err)?;
    // 批1：run 起点落 running 标记（崩溃恢复据此判定上次未正常收尾的会话）。
    // run 可能在落盘前就已收尾（极快的失败/取消）：按内存标志补一次清除，避免留下永久 running。
    if let Err(e) = core.store.mark_running(&session_id, true) {
        tracing::warn!("会话 {session_id} running 标记落盘失败：{e}");
    } else if !rt.running.load(std::sync::atomic::Ordering::SeqCst) {
        if let Err(e) = core.store.mark_running(&session_id, false) {
            tracing::warn!("会话 {session_id} running 标记回补失败：{e}");
        }
    }
    Ok(run_id)
}

/// 会话级运行偏好（权限档/模型/推理力度；全量替换，前端为准）。
#[tauri::command]
pub async fn set_session_prefs(
    core: Core<'_>,
    session_id: String,
    prefs: crate::core::prefs::SessionPrefs,
) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    if let Some(id) = &prefs.model_id {
        let exists = core.cfg.read().unwrap().find_model(id).is_some();
        if !exists {
            return Err(format!("模型不存在：{id}"));
        }
    }
    rt.set_prefs(prefs);
    Ok(())
}

/// 读取会话级运行偏好。
#[tauri::command]
pub async fn get_session_prefs(
    core: Core<'_>,
    session_id: String,
) -> Result<crate::core::prefs::SessionPrefs, String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    Ok(rt.prefs())
}

/// 取消当前 run（含子代理与审批等待）。
#[tauri::command]
pub async fn cancel_run(core: Core<'_>, session_id: String) -> Result<(), String> {
    core.session(&session_id)
        .ok_or("会话不存在")?
        .cancel_active();
    Ok(())
}

/// 前端 M4：重开会话时探测 run 是否仍在跑（同进程关 Tab 不打断 run）。
#[tauri::command]
pub async fn session_running(core: Core<'_>, session_id: String) -> Result<bool, String> {
    Ok(core
        .session(&session_id)
        .map(|rt| rt.running.load(std::sync::atomic::Ordering::SeqCst))
        .unwrap_or(false))
}

/// 清除会话中断标记（批1）：前端「已读 / 续跑」后调用，幂等；会话不存在时静默成功。
#[tauri::command]
pub async fn clear_session_interrupt(core: Core<'_>, session_id: String) -> Result<(), String> {
    core.store.clear_interrupted(&session_id).map_err(err)?;
    Ok(())
}

/// 运行中注入用户消息（不打断当前回合，下一轮生效）。
#[tauri::command]
pub async fn inject_run_message(
    core: Core<'_>,
    session_id: String,
    text: String,
) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    rt.inject_tx
        .try_send(Message::user_text(text))
        .map_err(|e| format!("注入失败：{e}"))
}

/// 解决一次 ask（审批/询问应答）：主会话未命中则扫描子代理 runtime。
#[tauri::command]
pub async fn resolve_ask(
    core: Core<'_>,
    session_id: String,
    ask_id: String,
    value: serde_json::Value,
) -> Result<(), String> {
    // M16 修复：主会话未命中时扫描子代理 runtime（此前子代理的审批永远无法批准）
    if let Some(rt) = core.session(&session_id) {
        if rt.resolve_ask(&ask_id, value.clone()) {
            return Ok(());
        }
    }
    for e in core.subs.iter() {
        if e.value().resolve_ask(&ask_id, value.clone()) {
            return Ok(());
        }
    }
    Err("ask 不存在或已关闭".into())
}

/// 手动触发上下文压缩：RAII 槽位防并发 + 前后事件通知（与自动压缩同一套事件）。
#[tauri::command]
pub async fn compact_session(core: Core<'_>, session_id: String) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    use std::sync::atomic::Ordering;
    // TOCTOU 修复（[docs/tool-optimizations-port](../../../../docs/tool-optimizations-port.md)）：先原子占据槽位再复查 running——原
    // check-then-act 实现会在检查后让 start_chat 插入，压缩整体替换历史时
    // 丢掉并发写入的新消息。
    // 槽位为 RAII guard：compact_history panic 展开时 Drop 仍会复位 compacting（评审修复）。
    let Some(_compact_guard) = crate::core::agent::CompactingGuard::acquire(&rt) else {
        return Err("压缩已在进行中".into());
    };
    if rt.running.load(Ordering::SeqCst) {
        return Err("运行中无法压缩，请先停止".into());
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    // 手动压缩也发进度事件（缺陷修复）：此前只有自动压缩发
    // run:compacting/compacted，手动压缩对前端不可见（按钮无 loading、
    // 聊天窗无提示）。tokens_before 经 breakdown 与自动压缩对齐。
    let tokens_before = crate::core::context::breakdown(&core, &rt)
        .await
        .total_tokens;
    core.sink.emit(
        &rt.id,
        "run:compacting",
        serde_json::json!({ "tokens_before": tokens_before, "messages": rt.history.lock().unwrap().len(), "timeout_ms": core.cfg.read().unwrap().compact_timeout_seconds.clamp(30, 3600) * 1000 }),
    );
    let result = crate::core::context::compact_history(&core, &rt, true, &cancel).await;
    match &result {
        Ok(_) => core.sink.emit(
            &rt.id,
            "run:compacted",
            serde_json::json!({ "tokens_before": tokens_before }),
        ),
        Err(e) => core.sink.emit(
            &rt.id,
            "run:compact_failed",
            serde_json::json!({ "error": e }),
        ),
    }
    result.map(|_| ()).map_err(err)
}

// ---------- 工作区 ----------

/// 停止一个子代理（取消其 run）。
#[tauri::command]
pub async fn stop_subagent(
    core: Core<'_>,
    session_id: String,
    sub_id: String,
) -> Result<(), String> {
    let rt = core.session(&session_id).ok_or("会话不存在")?;
    let _ = rt;
    match core.subs.get(&sub_id) {
        Some(sub) => {
            sub.cancel_active();
            Ok(())
        }
        _ => Err("子代理不存在或已结束".into()),
    }
}

// ---------- Run 日志（[docs/session-logging-report](../../../../docs/session-logging-report.md)；校验与读取在 core/logging，host 只转调）----------

// ---------- 会话保留期清理（[docs/session-cleanup](../../../../docs/session-cleanup.md)）----------

/// 预览清理：返回将删除的会话条数与前几条标题，外加索引外残留项数（保存前的确认框内容）。
/// `days` 缺省时用已保存的保留期；保留期为「不清理」时报错（前端按钮同样禁用）。
/// 配置里的档位非法（白名单之外，手改配置/旧值）时返回**空预览**并记 warn：一律不清理。
#[tauri::command]
pub async fn preview_session_cleanup(
    core: Core<'_>,
    days: Option<u32>,
) -> Result<crate::core::sessions::CleanupPreview, String> {
    let Some(days) = resolve_retention(&core, days)? else {
        return Ok(crate::core::sessions::CleanupPreview::default());
    };
    Ok(crate::core::sessions::cleanup::preview_all(
        &core.store,
        &core.store.load_index().sessions,
        chrono::Utc::now(),
        days,
        &running_ids(&core),
    ))
}

/// 立即清理：用**已保存的**保留期跑一次，返回被删会话 id 列表（前端据此关标签页并刷新列表）。
/// 配置里的档位非法时什么都不删（返回空结果，已记 warn）。
#[tauri::command]
pub async fn run_session_cleanup(
    core: Core<'_>,
) -> Result<crate::core::sessions::CleanupOutcome, String> {
    let Some(days) = resolve_retention(&core, None)? else {
        return Ok(crate::core::sessions::CleanupOutcome::default());
    };
    Ok(execute_cleanup(&core, days))
}

/// 上次清理状态（存在后端，重启后仍在；启动时的自动清理也计入）。
#[tauri::command]
pub async fn get_cleanup_status(
    core: Core<'_>,
) -> Result<crate::core::sessions::CleanupStatus, String> {
    Ok(crate::core::sessions::cleanup::read_status(&core.data_dir))
}

// ---------- 「清理旧格式历史」（P5；[docs/session-cleanup](../../../../docs/session-cleanup.md)）----------
// 命令层只做转调：扫描 / 判定 / 删除全在 `core::sessions::cleanup`（纯函数化、有单测）。

/// 预览「清理旧格式历史」：可回收的会话数 / 字节数 + **必须保留**的会话数（只读，不删任何文件）。
///
/// 「必须保留」= 只有旧 `.json.gz`、没有新格式段数据的会话：那是该会话历史的唯一副本，绝不删。
#[tauri::command]
pub async fn preview_legacy_history_cleanup(
    core: Core<'_>,
) -> Result<crate::core::sessions::cleanup::LegacyCleanupPreview, String> {
    Ok(crate::core::sessions::cleanup::preview_legacy_histories(
        &core.store,
    ))
}

/// 执行「清理旧格式历史」：**只删已有新格式数据**的旧文件；返回回收统计与被保留（跳过）的条数。
#[tauri::command]
pub async fn run_legacy_history_cleanup(
    core: Core<'_>,
) -> Result<crate::core::sessions::cleanup::LegacyCleanupOutcome, String> {
    Ok(crate::core::sessions::cleanup::run_legacy_cleanup(
        &core.store,
    ))
}

/// 解析本次清理要用的保留期：显式天数优先（设置页的草稿值），否则取已保存配置。
/// - `Ok(Some(days))`：合法档位（1/3/7/14/30），照常清理；
/// - `Ok(None)`：配置里的值不是合法档位（白名单之外，含 0）——一律不清理，已记 warn；
/// - `Err`：当前是「不清理」（未设置）——报错，前端按钮同样禁用。
fn resolve_retention(
    core: &crate::core::agent::AgentCore,
    days: Option<u32>,
) -> Result<Option<u32>, String> {
    use crate::core::sessions::cleanup::RetentionChoice;
    let saved = core.cfg.read().unwrap().sessions.retention_days;
    match crate::core::sessions::cleanup::resolve_retention(days, saved) {
        RetentionChoice::Run(d) => Ok(Some(d)),
        RetentionChoice::Invalid(_) => Ok(None),
        RetentionChoice::Skip => Err("未设置会话保留期（当前为「不清理」）".into()),
    }
}

/// 运行中的会话 id：索引标记 ∪ 进程内运行集合（core 存储侧）∪ 会话运行态（运行时注册表）。
/// 宁可不删也不误删——运行中的会话被删后，迟到写入会把索引行“复活”。
fn running_ids(core: &crate::core::agent::AgentCore) -> std::collections::HashSet<String> {
    use std::sync::atomic::Ordering;
    let mut set = crate::core::sessions::cleanup::running_set(&core.store);
    for e in core.sessions.iter() {
        if e.value().running.load(Ordering::SeqCst) {
            set.insert(e.key().clone());
        }
    }
    set
}

/// 执行一次清理：同步挑候选 → 删文件 → 一次性写索引 → 扫索引外残留 → 写状态文件；
/// 随后把被删会话的运行时对象从内存注册表移除（复用 delete_session 的收尾方式：先标 zombie，
/// 让迟到的检查点/日志写入无法复活已删除会话）。
/// 「立即清理」命令与保存配置（保留期变化）共用。
pub(crate) fn execute_cleanup(
    core: &crate::core::agent::AgentCore,
    days: u32,
) -> crate::core::sessions::CleanupOutcome {
    use std::sync::atomic::Ordering;
    let running = running_ids(core);
    let candidates = crate::core::sessions::cleanup::select_expired(
        &core.store.load_index().sessions,
        chrono::Utc::now(),
        days,
        &running,
    );
    // 先标 zombie（在删文件之前）：候选会话的运行时若还在内存里，任何在途写入都不得让它复活
    for m in &candidates {
        if let Some(rt) = core.session(&m.id) {
            rt.zombie.store(true, Ordering::SeqCst);
        }
    }
    let outcome =
        crate::core::sessions::cleanup::execute(&core.store, &core.data_dir, days, &candidates);
    for id in &outcome.ids {
        if let Some(rt) = core.session(id) {
            rt.zombie.store(true, Ordering::SeqCst);
        }
        core.sessions.remove(id);
    }
    outcome
}

/// 批2 P2 的分页读用例（测试对象是命令层的两个纯函数 `first_page` / `earlier_page`）。
#[cfg(test)]
mod paging_tests {
    use super::*;
    use crate::core::types::{Content, Role};

    /// 带 250 条历史的 store：段式落盘后 = 段1（200 条，已封口）+ 段2（50 条，未封口）。
    fn store_with_segments() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let msgs: Vec<Message> = (0..250)
            .map(|i| Message {
                role: Role::User,
                content: vec![Content::Text {
                    text: format!("m{i}"),
                }],
                created_at: None,
            })
            .collect();
        store
            .save_history("s1", "t", ".", None, None, &["/ws".into()], &msgs)
            .unwrap();
        (store, dir)
    }

    fn gzip_bytes(s: &str) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(s.as_bytes()).unwrap();
        enc.finish().unwrap()
    }

    fn texts(msgs: &[Message]) -> Vec<String> {
        msgs.iter().map(|m| m.text_joined()).collect()
    }

    /// ① 首屏只含最近 1 段的消息，分页元信息口径正确。
    #[test]
    fn first_page_reads_only_latest_segment() {
        let (store, _dir) = store_with_segments();
        // 基线只用于对拍 display 口径：打开会话的取数路径（`load_for_open` + `first_page`）不读全量
        let full = store.load_history_full("s1").unwrap();
        let page = first_page(&store, "s1", None);

        assert_eq!(page.messages.len(), 50, "首屏 = 最近一段");
        assert_eq!(texts(&page.messages)[0], "m200");
        assert!(matches!(page.paging.format, HistoryFormat::New));
        assert_eq!(page.paging.loaded_from_seq, 2);
        assert_eq!(page.paging.segment_count, 2);
        assert_eq!(page.paging.total_messages, 250);
        assert_eq!(page.paging.bytes, store.session_history_bytes("s1"));
        assert!(page.paging.has_more, "还有更早一段");
        // display 口径：首屏是整份 display 的尾部（wire 会被 trim，不能当展示数据源）
        assert_eq!(page.messages, full.display[full.display.len() - 50..]);
    }

    /// ② 逐段向前直到 `has_more = false`：拼接结果与 `load_history_full().display` 完全一致。
    #[test]
    fn paging_backwards_reproduces_full_display() {
        let (store, _dir) = store_with_segments();
        let full = store.load_history_full("s1").unwrap();
        let first = first_page(&store, "s1", None);

        let mut acc = first.messages.clone();
        let (mut cursor, mut has_more) = (first.paging.loaded_from_seq, first.paging.has_more);
        let mut rounds = 0;
        while has_more {
            let page = earlier_page(&store, "s1", cursor);
            assert!(!page.messages.is_empty(), "有更早段时不得返回空页");
            assert!(
                page.from_seq < cursor,
                "段序号必须严格向前推进（防界面空转）"
            );
            acc.splice(0..0, page.messages.iter().cloned());
            cursor = page.from_seq;
            has_more = page.has_more;
            rounds += 1;
            assert!(rounds < 8, "段数有限，不该无限翻页");
        }
        assert_eq!(rounds, 1);
        assert_eq!(acc, full.display, "逐段拼接必须与整份 display 逐条一致");
    }

    /// ③ 旧格式回落：整份（与旧行为一致）、`legacy` 标记、没有更早内容。
    #[test]
    fn legacy_history_loads_whole_and_has_no_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        let json = serde_json::to_string(&serde_json::json!([{
            "role": "user",
            "content": [{ "type": "text", "text": "旧内容" }],
        }]))
        .unwrap();
        std::fs::write(store.history_path("s-legacy"), gzip_bytes(&json)).unwrap();

        // 旧格式（无段目录）走 legacy 回落：整份由 `load_for_open` 的第二个返回值给出
        let full = store.load_history_full("s-legacy").unwrap();
        let page = first_page(&store, "s-legacy", Some(&full));
        assert!(matches!(page.paging.format, HistoryFormat::Legacy));
        assert_eq!(texts(&page.messages), vec!["旧内容".to_string()]);
        assert!(!page.paging.has_more);
        assert_eq!(page.paging.loaded_from_seq, 0);
        assert_eq!(page.paging.segment_count, 1);
        assert_eq!(page.paging.total_messages, 1);
        assert_eq!(page.paging.bytes, full.bytes);
        assert_eq!(
            serde_json::to_value(&page).unwrap()["paging"]["format"],
            serde_json::json!("legacy"),
            "旧格式在 wire 上是 `legacy`"
        );

        let earlier = earlier_page(&store, "s-legacy", 7);
        assert!(earlier.messages.is_empty());
        assert!(!earlier.has_more);
        assert_eq!(earlier.from_seq, 0);
    }

    /// 新旧并存：以新格式为权威（分页也只看段，不被旧文件带偏）。
    #[test]
    fn new_format_wins_over_legacy_file_for_paging() {
        let (store, _dir) = store_with_segments();
        std::fs::create_dir_all(store.histories_dir()).unwrap();
        let json = serde_json::to_string(&serde_json::json!([{
            "role": "user",
            "content": [{ "type": "text", "text": "旧内容" }],
        }]))
        .unwrap();
        std::fs::write(store.history_path("s1"), gzip_bytes(&json)).unwrap();

        // 全量侧同样以新格式为权威；打开会话的取数路径以「段目录在不在」分派（在 ⇒ 不是 legacy）
        let full = store.load_history_full("s1").unwrap();
        assert!(matches!(full.format, HistoryFormat::New));
        let page = first_page(&store, "s1", None);
        assert!(matches!(page.paging.format, HistoryFormat::New));
        assert_eq!(page.messages.len(), 50);
        let earlier = earlier_page(&store, "s1", 2);
        assert_eq!(earlier.from_seq, 1);
        assert_eq!(earlier.messages.len(), 200);
        assert!(!earlier.has_more);
    }

    /// ④ `before_seq` 越界 / 已到最早 / 目录为空：空数组 + `has_more = false`，**不报错**。
    #[test]
    fn earlier_page_out_of_range_returns_empty() {
        let (store, _dir) = store_with_segments();
        for seq in [0u32, 1] {
            let page = earlier_page(&store, "s1", seq);
            assert!(page.messages.is_empty(), "before_seq={seq} 不该返回消息");
            assert!(!page.has_more);
            assert_eq!(page.from_seq, 0, "0 = 没有更早的段");
        }
        // 会话压根没落盘：同样只回空（分页不弹错）
        assert!(earlier_page(&store, "missing", 3).messages.is_empty());

        // 目录存在但一个段文件都没有（新建后即崩溃）：仍走新格式（不是 legacy 回落），如实报空
        let empty = store.history_dir("s-empty");
        std::fs::create_dir_all(&empty).unwrap();
        let page = first_page(&store, "s-empty", None);
        assert!(page.messages.is_empty());
        assert!(!page.paging.has_more);
        assert_eq!(page.paging.segment_count, 0);
        assert_eq!(page.paging.loaded_from_seq, 0);
    }

    /// ⑤ 坏段 / 半行：只影响它自己，其余段照常分页，且不会把界面困在「翻不动」的状态。
    #[test]
    fn bad_or_half_written_segment_does_not_break_paging() {
        let (store, _dir) = store_with_segments();
        let dir = store.history_dir("s1");
        // 段1 去掉头记录 → 坏段；段2 尾部追加半行（崩溃残留）
        let seg1 = dir.join("0001.jsonl");
        let body: String = std::fs::read_to_string(&seg1)
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&seg1, body).unwrap();
        let seg2 = dir.join("0002.jsonl");
        let mut raw = std::fs::read_to_string(&seg2).unwrap();
        raw.push_str("{\"kind\":\"messa");
        std::fs::write(&seg2, raw).unwrap();

        let full = store.load_history_full("s1").unwrap();
        let page = first_page(&store, "s1", None);
        assert_eq!(page.messages.len(), 50, "半行被丢弃，完整记录照常可用");
        assert_eq!(
            page.messages, full.display,
            "display 与首屏同源，坏段不在其中"
        );
        assert!(
            !page.paging.has_more,
            "更早只剩坏段：如实报「没有更早内容」"
        );
        assert_eq!(page.paging.segment_count, 2, "坏段仍占一个段文件");
        assert_eq!(
            page.paging.total_messages, 50,
            "坏段不计入总数（与 display 同口径）"
        );
        let earlier = earlier_page(&store, "s1", 2);
        assert!(earlier.messages.is_empty());
        assert!(!earlier.has_more);
    }

    /// 前端契约钉死：键名与取值即 wire 契约（改键名就破坏前端，必须两端同步改）。
    #[test]
    fn payload_wire_shape_is_pinned() {
        let (store, _dir) = store_with_segments();
        // 直接钉**打开会话的取数路径**（有界）产出的 wire 形态，不经过全量扫描
        let page = serde_json::to_value(first_page(&store, "s1", None)).unwrap();
        let mut keys: Vec<&String> = page.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["boundaries", "messages", "paging"]);
        let mut pkeys: Vec<&String> = page["paging"].as_object().unwrap().keys().collect();
        pkeys.sort();
        assert_eq!(
            pkeys,
            vec![
                "bad_segments",
                "bytes",
                "format",
                "has_more",
                "loaded_from_seq",
                "segment_count",
                "total_messages",
            ]
        );
        assert_eq!(page["paging"]["format"], serde_json::json!("new"));
        assert_eq!(page["paging"]["loaded_from_seq"], serde_json::json!(2));
        assert_eq!(page["paging"]["segment_count"], serde_json::json!(2));
        assert_eq!(page["paging"]["total_messages"], serde_json::json!(250));
        assert!(page["paging"]["bytes"].as_u64().unwrap() > 0);
        assert_eq!(page["paging"]["has_more"], serde_json::json!(true));
        // messages 仍是前端既有的 Message 形态（role + content）
        assert_eq!(page["messages"][0]["role"], serde_json::json!("user"));
        assert_eq!(
            page["messages"][0]["content"][0]["text"],
            serde_json::json!("m200")
        );

        let earlier = serde_json::to_value(earlier_page(&store, "s1", 2)).unwrap();
        let mut keys: Vec<&String> = earlier.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "bad_segments",
                "boundaries",
                "from_seq",
                "has_more",
                "messages"
            ]
        );
        assert_eq!(earlier["from_seq"], serde_json::json!(1));
        assert_eq!(earlier["has_more"], serde_json::json!(false));
        assert_eq!(earlier["messages"].as_array().unwrap().len(), 200);
    }

    /// 压缩边界（真实的基线段：段内只有 compaction 记录、没有 message）：
    /// 分页跳过它，逐段拼接仍与 display 一致（压缩点之前的轮次照样能翻到）。
    #[test]
    fn compaction_base_segment_is_skipped_by_paging() {
        let (store, _dir) = store_with_segments();
        // 一次「压缩后」的保存：条数变少 → 新基线段（既有段一律保留，不丢历史）
        let shortened: Vec<Message> = (0..3)
            .map(|i| Message::user_text(format!("摘要{i}")))
            .collect();
        store
            .save_history("s1", "t", ".", None, None, &["/ws".into()], &shortened)
            .unwrap();

        let full = store.load_history_full("s1").unwrap();
        assert_eq!(full.segments.len(), 3, "压缩只新增段，旧段不删");
        assert_eq!(full.display.len(), 250, "display 保留压缩前的完整转录");
        assert!(full.wire.len() <= 3, "wire 只含压缩后的时间线");
        assert_eq!(full.boundaries.len(), 1, "压缩记一条边界");

        let first = first_page(&store, "s1", None);
        assert_eq!(first.messages.len(), 50, "只有 compaction 记录的段不进页面");
        assert_eq!(first.paging.loaded_from_seq, 2);
        assert!(first.paging.has_more);
        let earlier = earlier_page(&store, "s1", first.paging.loaded_from_seq);
        assert_eq!(earlier.messages.len(), 200);
        assert!(
            !earlier.has_more,
            "基线段没有消息记录：跳过它后已无更早内容"
        );

        let mut acc = first.messages.clone();
        acc.splice(0..0, earlier.messages.clone());
        assert_eq!(acc, full.display, "分页拼接仍等于完整转录");
    }

    // ---------- 打开会话的读取量有界（批2 P4） ----------

    /// 每条消息正文的字符数：ASCII 1024 字符 ≈ 290 token（chars/4 + 10% headroom + role 开销）——
    /// 段粒度够大（每段 ≈ 5.8 万 token），「读到够用即停」（预算 256k token）才会落在少量段上。
    const FAT_CHARS: usize = 1024;

    /// 跨段胖历史：每段装 `SEGMENT_MAX_MESSAGES` 条（按条数封口），user / assistant 交替。
    fn fat_msgs(segments_n: usize) -> Vec<Message> {
        let n = segments_n * segments::SEGMENT_MAX_MESSAGES;
        let fat = "y".repeat(FAT_CHARS);
        (0..n)
            .map(|i| Message {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![Content::Text {
                    text: format!("m{i} {fat}"),
                }],
                created_at: None,
            })
            .collect()
    }

    /// 打开会话读了多少：`stats` 只统计**整段解析**（有界方案里最贵的那部分），
    /// 分页元信息的段头 1KB / 段尾 4KB 窗口另计在 `window_bytes`。
    struct OpenReads {
        stats: segments::ReadStats,
        /// 该会话历史占用的总字节（段文件总和）
        total_bytes: u64,
        /// 段文件个数
        segment_count: usize,
    }

    /// 走一遍**打开会话的取数路径**（`load_for_open` + `first_page`）并量出读取量。
    ///
    /// 载荷校验一律对**输入的那份历史**做（不跑全量基线）：`load_history_full` 本身的代价与
    /// 历史总量成正比（它要重建完整 `display`），放进度量里会喧宾夺主。有界装载与全量的逐字节
    /// 对拍在 `core/sessions/store/tests.rs::bounded_wire_*` 与下面的 `load_for_open_matches_full_scan`。
    fn measure_open(segments_n: usize) -> (tempfile::TempDir, OpenReads) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().to_path_buf());
        let msgs = fat_msgs(segments_n);
        store
            .save_history("s-open", "t", ".", None, None, &["/ws".into()], &msgs)
            .unwrap();
        let d = store.history_dir("s-open");
        let total_bytes = segments::dir_bytes(&d);
        let segment_count = segments::list_segments(&d).len();

        segments::reset_read_stats();
        let (wire, legacy) = load_for_open(&store, "s-open").unwrap();
        let page = first_page(&store, "s-open", legacy.as_ref());
        let stats = segments::read_stats();

        assert!(legacy.is_none(), "有段目录 ⇒ 不走 legacy 回落");
        assert_eq!(
            page.messages.len(),
            segments::SEGMENT_MAX_MESSAGES,
            "首屏 = 最近一段"
        );
        assert_eq!(
            page.messages,
            msgs[msgs.len() - page.messages.len()..].to_vec(),
            "首屏 = 输入历史的尾部（display 口径）"
        );
        assert_eq!(page.paging.total_messages, msgs.len(), "总数仍是全量口径");
        assert_eq!(page.paging.segment_count, segment_count);
        assert_eq!(page.paging.bytes, total_bytes);
        assert_eq!(
            wire.last().unwrap().text_joined(),
            msgs.last().unwrap().text_joined(),
            "wire 的右端就是时间线末尾"
        );
        (
            dir,
            OpenReads {
                stats,
                total_bytes,
                segment_count,
            },
        )
    }

    /// 打开会话的取数路径（`load_for_open`）产出的 wire 与全量扫描**逐条一致**。
    #[test]
    fn load_for_open_matches_full_scan() {
        let (store, _dir) = store_with_segments();
        let full = store.load_history_full("s1").unwrap();
        let (wire, legacy) = load_for_open(&store, "s1").unwrap();
        assert!(legacy.is_none());
        assert_eq!(wire, full.wire, "有界打开的 wire 必须与全量扫描一致");
    }

    /// ⑦ 打开会话的读取量**有界**：段数翻倍，读的段数 / 字节数基本不变（不随历史总量增长）。
    ///
    /// 度量用 `segments` 里仅测试可见的读取计数钩子（thread-local；本装载全在当前线程完成）。
    /// 复位后走的正是 `load_session` 的取数路径，只省掉 Core / runtime 那层装配。
    #[test]
    fn open_session_reads_stay_bounded_as_history_grows() {
        let (_d1, small) = measure_open(20);
        let (_d2, big) = measure_open(40);
        println!(
            "打开会话读取量：{} 段（全量 {} 字节）→ {:?}；{} 段（全量 {} 字节）→ {:?}",
            small.segment_count,
            small.total_bytes,
            small.stats,
            big.segment_count,
            big.total_bytes,
            big.stats
        );
        assert!(
            small.segment_count >= 20 && big.segment_count >= 40,
            "本用例要求足够多的段：{} / {}",
            small.segment_count,
            big.segment_count
        );

        // 核心性质：历史翻倍，读取量**不跟着翻倍**（有界 = 常数级）
        assert!(
            big.stats.full_segments <= small.stats.full_segments + 1,
            "读的段数不该随历史总量增长：{:?} → {:?}",
            small.stats,
            big.stats
        );
        assert!(
            big.stats.full_bytes <= small.stats.full_bytes * 3 / 2,
            "读的字节不该随历史总量增长：{} → {}",
            small.stats.full_bytes,
            big.stats.full_bytes
        );
        // 且都远小于全量
        assert!(
            small.stats.full_segments * 2 < small.segment_count as u64,
            "读的段数必须远小于段总数：{} / {}",
            small.stats.full_segments,
            small.segment_count
        );
        assert!(
            big.stats.full_bytes * 3 < big.total_bytes,
            "读的字节必须远小于全量：{} / {}",
            big.stats.full_bytes,
            big.total_bytes
        );
    }
}
