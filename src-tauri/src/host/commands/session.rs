use super::util::{err, Core};
use crate::core::agent::Frame;
use crate::host::events::ChannelRegistry;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::State;
use crate::core::types::Message;

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
        (primary, Vec::new(), Some(pid), Some(pd), Some(project))
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
    // 项目快照加载：project_id/roots 以 meta 为准（legacy 免目录会话自然回退单根）
    let (pid, roots, project_dir, meta_title) = match core.store.get(&session_id) {
        Some(meta) => {
            let mut roots = meta.roots;
            if roots.is_empty() {
                roots = vec![workspace.clone()];
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
    core.store.load_sub_history(&session_id, &sub_id).map_err(err)
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
    match core.session(&session_id) { Some(rt) => {
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
    } _ => {
        core.store
            .rename_in_index(&session_id, &title)
            .map_err(err)?;
    }}
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
    channels.map.insert(session_id, on_event);
    core.start_chat(rt, text, images.unwrap_or_default())
        .map_err(err)
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
    match core.subs.get(&sub_id) { Some(sub) => {
        sub.cancel_active();
        Ok(())
    } _ => {
        Err("子代理不存在或已结束".into())
    }}
}

// ---------- Run 日志（[docs/session-logging-report](../../../../docs/session-logging-report.md)；校验与读取在 core/logging，host 只转调）----------

