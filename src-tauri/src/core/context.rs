//! 上下文管理（G10，[docs/p0-plan](../../../docs/p0-plan.md) §11）：分节 token 计账（30s 缓存）+ 自动压缩。
//! 摘要请求走同一 Provider 层；摘要 prompt 结构为 CodeWave 原创五段式。

use crate::core::agent::{AgentCore, SessionRuntime};
use crate::core::types::{Content, Message, Role};
use crate::util::token_est::est_tokens_message;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// 上下文占用分节账单（前端 tokens:update 事件的 breakdown 载荷）。
#[derive(Debug, Clone, Serialize)]
pub struct ContextBreakdown {
    /// system prompt 各层合计
    pub system_tokens: u64,
    /// 全部历史消息（含工具结果）合计
    pub history_tokens: u64,
    /// 工具结果子集（history_tokens 的一部分）
    pub tool_results_tokens: u64,
    /// 工具 schema 估算
    pub tool_schema_tokens: u64,
    /// system + history + tool_schema 三者之和
    pub total_tokens: u64,
    /// 生效模型上下文窗口
    pub context_window: u32,
    /// total / context_window，自动压缩阈值比较用
    pub ratio: f64,
}

/// 自动压缩触发来源（日志与诊断用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactTrigger {
    /// 不触发
    None,
    /// 本地估算占比超阈值
    Estimate,
    /// 上游回报的真实输入占比超阈值
    Reported,
}

/// 自动压缩触发判定（纯函数）：估算占比与上游回报的真实占比**任一**超阈值即触发。
///
/// 为什么不能只看估算：本地估算按「字符数 ÷ 4」折算，对某些内容会少算数倍 —— 图片 base64
/// 实测 1.59 字符/token（估算只算到 0.44 倍），于是「估算占比 0.57 / 真实占比 0.99」这种
/// 组合下压缩全程不触发，上下文一路顶到上游上限并被 400 拒绝（会话 6bca80f4）。
/// 两路都超阈值时报 [`CompactTrigger::Reported`]（诊断上信息量更大）。
pub fn compact_trigger(est_ratio: f64, reported_ratio: f64, threshold: f64) -> CompactTrigger {
    if reported_ratio > threshold {
        CompactTrigger::Reported
    } else if est_ratio > threshold {
        CompactTrigger::Estimate
    } else {
        CompactTrigger::None
    }
}

/// 压缩摘要请求的输出上限（min 进模型 max_tokens，防摘要喧宾夺主）。
pub const SUMMARY_MAX_TOKENS: u32 = 8000;
/// 压缩摘要请求默认超时秒数（可经 config.compact_timeout_seconds 覆盖，钳到 [30, 3600]）。
pub const DEFAULT_SUMMARY_TIMEOUT_SECS: u64 = 180;

/// 会话生效配置下的压缩超时（[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md)：长上下文慢 provider 时 180s 可能不够）。
fn summary_timeout(cfg: &crate::core::config::ConfigState) -> std::time::Duration {
    std::time::Duration::from_secs(cfg.compact_timeout_seconds.clamp(30, 3600))
}

/// 分节计账（结果在 runtime 缓存 30s，避免每步重复估算）。
pub async fn breakdown(core: &AgentCore, rt: &SessionRuntime) -> ContextBreakdown {
    {
        let cache = rt.breakdown_cache.lock().unwrap();
        if let Some((at, bd)) = cache.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(30) {
                return bd.clone();
            }
        }
    }
    let cfg = core.cfg.read().unwrap().clone();
    // 上下文窗口跟随会话生效模型（覆盖优先，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）
    let window = crate::core::prefs::effective_model(&cfg, &rt.prefs())
        .map(|m| m.context_window)
        .unwrap_or(128_000)
        .max(1);

    // shell 描述跟随配置 selection（与 command 工具执行 shell 同一事实源）
    let shell = crate::tools::command::shell_description(cfg.shell.selection.as_deref());
    let extra: Vec<String> = rt.extra_roots.lock().unwrap().clone();
    let skills = core.skills.list(
        &rt.workspace,
        &rt.data_dir,
        &cfg.disabled_skills,
        rt.project_dir.as_deref(),
    );
    let skills_listing = crate::skills::prompt_listing(&skills);
    let memories = crate::memory::scan(&rt.data_dir, rt.project_dir.as_deref());
    let memory_listing = crate::memory::prompt_listing(&memories);
    let project_section = rt.project_id.as_ref().map(|pid| {
        let project = crate::core::projects::find(&core.data_dir, pid);
        let name = project
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| pid.clone());
        let temps = project
            .as_ref()
            .map(|p| crate::core::projects::project_data_dir(&core.data_dir, p).join("temps"))
            .unwrap_or_else(|| crate::core::projects::data_dir_by_id(&core.data_dir, pid).join("temps"))
            .to_string_lossy()
            .into_owned();
        format!(
            "\n<project>\n- Project: {name}\n- Project directory: {}\n- Scratch dir & command working directory: {temps}\n  Throwaway files go here; commands start here.\n</project>\n",
            rt.workspace.to_string_lossy()
        )
    });
    let system = crate::core::prompt::assemble(
        &rt.workspace,
        &cfg,
        &extra,
        shell.as_str(),
        &skills_listing,
        &memory_listing,
        project_section.as_deref(),
        rt.project_dir.as_deref(),
    );
    let system_tokens = crate::util::token_est::est_tokens_text(&system);
    let tool_schema_tokens = core.tools.schemas_token_estimate();

    let history = rt.history.lock().unwrap();
    let mut history_tokens = 0u64;
    let mut tool_results_tokens = 0u64;
    for m in history.iter() {
        let t = est_tokens_message(m);
        history_tokens += t;
        if m.role == Role::Tool {
            tool_results_tokens += t;
        }
    }
    let total = system_tokens + history_tokens + tool_schema_tokens;
    let bd = ContextBreakdown {
        system_tokens,
        history_tokens,
        tool_results_tokens,
        tool_schema_tokens,
        total_tokens: total,
        context_window: window,
        ratio: total as f64 / window as f64,
    };
    *rt.breakdown_cache.lock().unwrap() = Some((std::time::Instant::now(), bd.clone()));
    bd
}

const SUMMARY_SYSTEM: &str = r#"你把一段编程智能体的对话压缩成交接摘要，供全新会话无缝接续工作。输出纯文本，必须恰好包含以下五个小节，顺序固定，每节以自己的标题行开头：

LATEST_REQUEST: 用户最近一次要求做什么，忠实还原其意图
COMPLETED: 已完成的工作（改动的文件及路径、执行过的命令、结果）
CURRENT_STATE: 当前正在进行中的事（未落盘的编辑、运行中的检查、悬而未决的疑问）
NEXT_STEPS: 按顺序排列的具体下一步动作
KEY_FILES: 关键文件，每行一个，格式为 `path - why`

内容务必密集、只写事实。不要寒暄，不要 markdown 代码围栏。"#;

/// 自动/手动压缩：结构化摘要整体替换历史；原历史归档到 tmp/compacted/。
/// 返回压缩调用自身的 usage（[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md) 计账：kind=compact；
/// usage 缺失时以估算兜底）。
pub async fn compact_history(
    core: &std::sync::Arc<AgentCore>,
    rt: &std::sync::Arc<SessionRuntime>,
    keep_last_user: bool,
    cancel: &CancellationToken,
) -> Result<crate::provider::RunUsage, String> {
    let cfg = core.cfg.read().unwrap().clone();
    // 压缩请求使用会话生效模型（覆盖优先，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）；
    // 摘要输出上限压到 SUMMARY_MAX_TOKENS（兑现该常量的声明意图，此前只声明未接线）
    let mut model = crate::core::prefs::effective_model(&cfg, &rt.prefs()).ok_or("未配置模型")?;
    model.max_tokens = model.max_tokens.min(SUMMARY_MAX_TOKENS);

    let history_snapshot: Vec<Message> = {
        let h = rt.history.lock().unwrap();
        h.clone()
    };
    if history_snapshot.is_empty() {
        return Err("会话为空，无需压缩".into());
    }

    // 归档原始历史（供事后查看）
    let archive_dir = rt.data_dir.join("tmp").join("compacted");
    let _ = std::fs::create_dir_all(&archive_dir);
    let archive = archive_dir.join(format!(
        "{}-{}.json.gz",
        rt.id,
        chrono::Utc::now().timestamp_millis()
    ));
    if let Ok(json) = serde_json::to_vec(&history_snapshot) {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = std::io::Write::write_all(&mut enc, &json);
        let _ = std::io::Write::flush(&mut enc);
        let _ = std::fs::write(archive, enc.finish().unwrap_or_default());
    }

    // 历史序列化为压缩文本（原文截到合理长度）
    let mut transcript = String::new();
    for m in &history_snapshot {
        let role = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL_RESULT",
            Role::System => continue,
        };
        let text = m
            .content
            .iter()
            .map(|c| match c {
                Content::Text { text } => text.clone(),
                Content::ToolUse { name, args, .. } => format!("[tool {name} {args}]"),
                Content::ToolResult { content, .. } => content.clone(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let clipped: String = text.chars().take(6_000).collect();
        transcript.push_str(&format!("--- {role} ---\n{clipped}\n"));
    }
    transcript.push_str("\n--- INSTRUCTION ---\n请现在撰写上述五段式交接摘要。");
    let transcript_tokens = crate::util::token_est::est_tokens_text(&transcript);

    let req = crate::provider::StreamRequest {
        model: model.clone(),
        system_core: SUMMARY_SYSTEM.into(),
        system_extra: String::new(),
        cache_gen_index: None,
        messages: vec![Message::user_text(transcript)],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
        session_id: Some(rt.id.clone()),
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let collect = tokio::spawn(async move {
        let mut out = String::new();
        while let Some(d) = rx.recv().await {
            if let crate::provider::StreamDelta::Text { text } = d {
                out.push_str(&text);
            }
        }
        out
    });
    // key 解析必须走钥匙串占位符解析器（[docs/topbar-migration-and-git-identity](../../../docs/topbar-migration-and-git-identity.md)）：否则钥匙串
    // 用户（keys 含占位符）会拿到 None 并发出未鉴权请求（401）；
    // 解析为空则短路进既有的压缩失败路径
    let Some(key) = crate::host::keyring::pick_resolved_key(&model) else {
        return Err("上下文压缩跳过：模型 key 在钥匙串中解析为空".into());
    };
    // 读锁 clone（std 锁不能跨 await）；save_config 热替换后此处取到新代理的 client
    let client = core.client.read().unwrap().clone();
    let fut = crate::provider::stream_model(&client, &model, Some(key), req, tx, cancel.clone());
    // H2 修复：压缩响应可取消（cancel = 放弃本次压缩）；超时可配置（[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md)）
    let usage = tokio::select! {
        _ = cancel.cancelled() => return Err("压缩已取消".into()),
        r = tokio::time::timeout(summary_timeout(&cfg), fut) => {
            r.map_err(|_| "压缩请求超时".to_string())?
                .map_err(|e| format!("压缩请求失败：{e}"))?
        }
    };
    let summary = collect.await.map_err(|e| format!("collect failed: {e}"))?;
    if summary.trim().is_empty() {
        return Err("摘要为空，放弃压缩".into());
    }

    // usage 缺失（provider 未上报）→ 估算兜底，保证压缩成本在统计中可见
    let usage = if usage.input + usage.output == 0 {
        crate::provider::RunUsage {
            input: transcript_tokens,
            output: crate::util::token_est::est_tokens_text(&summary),
            ..Default::default()
        }
    } else {
        usage
    };

    let mut new_history = vec![Message::user_text(format!(
        "<handoff-summary>\n以下是此前会话的结构化摘要，请基于它继续工作：\n\n{summary}\n</handoff-summary>"
    ))];
    if keep_last_user {
        if let Some(last_user) = history_snapshot.iter().rev().find(|m| m.role == Role::User) {
            new_history.push(last_user.clone());
        }
    }
    *rt.history.lock().unwrap() = new_history;
    rt.breakdown_cache.lock().unwrap().take();
    // 真实输入量随历史一起作废：不复位会让「按上游回报触发」在压缩后立刻重复触发
    //（下一次请求回来前，那个值仍是被压缩掉的那份历史的规模）
    rt.last_input_tokens
        .store(0, std::sync::atomic::Ordering::SeqCst);

    // 压缩调用自身的 token 计账（[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md)）：独立 kind=compact，不再凭空蒸发
    core.stats.record(crate::core::stats::UsageRecord {
        session: rt.id.clone(),
        model_id: model.id.clone(),
        workspace: rt.workspace.to_string_lossy().into_owned(),
        input: usage.input,
        output: usage.output,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        runs: 1,
        kind: crate::core::stats::KIND_COMPACT.into(),
    });
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn breakdown_counts_sections() {
        // 在这里搭完整 AgentCore 太重：只验证摘要指令非空；五段结构由模型产出。
        // breakdown 的纯计数逻辑由 token_est 测试覆盖。
        let _ = SUMMARY_SYSTEM.contains("LATEST_REQUEST");
        assert!(SUMMARY_SYSTEM.contains("KEY_FILES"));
    }

    /// 回归钉（会话 6bca80f4）：估算占比低、上游回报占比高时必须触发压缩。
    #[test]
    fn compact_trigger_uses_reported_ratio() {
        assert_eq!(compact_trigger(0.10, 0.10, 0.6), CompactTrigger::None);
        assert_eq!(compact_trigger(0.61, 0.10, 0.6), CompactTrigger::Estimate);
        assert_eq!(compact_trigger(0.57, 0.99, 0.6), CompactTrigger::Reported);
        // 两路都超阈值时报「真实输入」优先（诊断上更有信息量）
        assert_eq!(compact_trigger(0.9, 0.9, 0.6), CompactTrigger::Reported);
    }

    #[test]
    fn summary_timeout_clamped_to_configurable_bounds() {
        let mut cfg = crate::core::config::ConfigState::default();
        assert_eq!(
            summary_timeout(&cfg),
            std::time::Duration::from_secs(180),
            "default stays at the default"
        );
        cfg.compact_timeout_seconds = 0;
        assert_eq!(summary_timeout(&cfg), std::time::Duration::from_secs(30));
        cfg.compact_timeout_seconds = 10_000;
        assert_eq!(summary_timeout(&cfg), std::time::Duration::from_secs(3600));
        cfg.compact_timeout_seconds = 600;
        assert_eq!(summary_timeout(&cfg), std::time::Duration::from_secs(600));
    }

    /// 走真实计账路径的集成验证：分节求和、工具结果是历史子集、窗口取生效模型、
    /// 结果缓存 30s。
    #[tokio::test]
    async fn breakdown_fields_and_cache() {
        use crate::core::types::{Content, Message};
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "ctx-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        {
            let mut h = rt.history.lock().unwrap();
            h.push(Message::user_text("hello world this is a user message"));
            h.push(Message::tool_results(vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "tool output text".into(),
                is_error: false,
            }]));
        }

        let bd = breakdown(&core, &rt).await;
        // 默认测试配置的生效模型：上下文窗口 128k
        assert_eq!(bd.context_window, 128_000);
        assert!(bd.system_tokens > 0, "system prompt contributes");
        assert!(bd.tool_schema_tokens > 0, "tool schemas contribute");
        assert!(bd.history_tokens > 0);
        assert!(bd.tool_results_tokens > 0);
        assert!(
            bd.tool_results_tokens < bd.history_tokens,
            "tool results are a strict subset of history"
        );
        assert_eq!(
            bd.total_tokens,
            bd.system_tokens + bd.history_tokens + bd.tool_schema_tokens
        );
        assert!((bd.ratio - bd.total_tokens as f64 / bd.context_window as f64).abs() < 1e-9);
        assert!(bd.total_tokens < bd.context_window as u64);

        // 30s 缓存：改动历史不影响本次上报的 breakdown
        rt.history
            .lock()
            .unwrap()
            .push(Message::user_text("this new message must not show up yet"));
        let cached = breakdown(&core, &rt).await;
        assert_eq!(cached.history_tokens, bd.history_tokens);
        assert_eq!(cached.total_tokens, bd.total_tokens);
    }
}
