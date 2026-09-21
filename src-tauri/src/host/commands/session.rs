use super::util::{Core, err};
use crate::core::agent::Frame;
use crate::core::types::Message;
use crate::host::events::ChannelRegistry;
use std::sync::Arc;
use tauri::State;
use tauri::ipc::Channel;

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

/// 恢复会话：读历史 + 按 meta 快照重建 runtime，并回填运行时标题。
#[tauri::command]
pub async fn load_session(
    core: Core<'_>,
    session_id: String,
    workspace: String,
) -> Result<Vec<Message>, String> {
    let msgs = core.store.load_history(&session_id).map_err(err)?;
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
        *rt.history.lock().unwrap() = msgs.clone();
    }
    // 标题回填：重开的旧会话运行时 title 为空；不回填会让 start_chat 的
    // 10 字符启发式覆盖已有标题并经 checkpoint 持久化（[docs/session-auto-title](../../../../docs/session-auto-title.md)）
    {
        let mut t = rt.title.lock().unwrap();
        if t.is_empty() {
            *t = meta_title.unwrap_or_default();
        }
    }
    Ok(msgs)
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
