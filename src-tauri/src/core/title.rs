//! 会话自动命名（[docs/session-auto-title](../../../docs/session-auto-title.md)）：新会话首条用户消息之后，一次性后台 LLM 调用
//! 生成 ≤20 字的精炼标题。
//! 人设来自 agents::TITLE_BODY（title 子代理角色，绝不经 subagent 工具委派）；
//! 调用模式与 context::compact_history 同构：零工具、独立 cancel token、超时兜底。

use crate::core::agent::{AgentCore, SessionRuntime};
use tokio_util::sync::CancellationToken;

/// 标题硬上限（字符数；中英文一视同仁）
pub const MAX_TITLE_CHARS: usize = 20;
/// 生成请求超时：命名只是微型调用，超时即放弃（保留 10 字符兜底标题）——不值得久等
const TITLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// 喂给模型的用户消息截断长度（字符数）
const INPUT_MAX_CHARS: usize = 2_000;

/// 清洗模型输出：取首个非空行 → 剥成对外包裹引号与「标题：」类前缀（见 strip_title_prefix）
/// → 截断到上限；无效则返回 None。
pub fn sanitize_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let title: String = strip_title_prefix(strip_paired_quotes(line))
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// 剥离开头的中文「标题：」式标签前缀（精确匹配下方字面量变体；模型偶尔无视
/// 「不加前后缀」的约束）。
fn strip_title_prefix(s: &str) -> &str {
    let t = s.trim_start();
    for p in ["标题:", "标题："] {
        if let Some(rest) = t.strip_prefix(p) {
            return rest.trim_start();
        }
    }
    t
}

/// 剥最外层成对包裹引号（「」『’“” 及 ASCII 引号），只剥一层。
fn strip_paired_quotes(s: &str) -> &str {
    let pairs = [
        ("「", "」"),
        ("『", "』"),
        ("\u{201C}", "\u{201D}"),
        ("\u{2018}", "\u{2019}"),
        ("\"", "\""),
        ("'", "'"),
    ];
    for (open, close) in pairs {
        if let Some(inner) = s.strip_prefix(open).and_then(|r| r.strip_suffix(close)) {
            if !inner.is_empty() {
                return inner;
            }
        }
    }
    s
}

/// 覆盖守卫：仅当当前标题仍是兜底值（或为空）时自动标题才允许落盘——
/// 生成期间用户已手动改名则放弃。
fn should_replace_title(current: &str, fallback: &str) -> bool {
    current.is_empty() || current == fallback
}

/// 后台生成并落盘标题：成功 → 更新 rt.title + 索引 + session:title 事件；
/// 任何失败都静默保留兜底标题。由 run_chat 在首条用户消息后 spawn，与主 run 并行、不阻塞。
pub async fn generate_and_apply(
    core: std::sync::Arc<AgentCore>,
    rt: std::sync::Arc<SessionRuntime>,
    user_text: String,
    fallback_title: String,
) {
    if rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let started = std::time::Instant::now();
    let cfg = core.cfg.read().unwrap().clone();
    // 与压缩一致：命名请求使用会话生效模型（覆盖优先，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）
    let Some(model) = crate::core::prefs::effective_model(&cfg, &rt.prefs()) else {
        return;
    };
    let clipped: String = user_text.chars().take(INPUT_MAX_CHARS).collect();
    let req = crate::provider::StreamRequest {
        model: model.clone(),
        system: crate::agents::TITLE_BODY.into(),
        messages: vec![crate::core::types::Message::user_text(format!(
            "{clipped}\n\n---\n请为以上用户消息发起的会话生成标题。"
        ))],
        tools: vec![],
        cache_key: None,
        reasoning_effort: None,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let collect = tokio::spawn(async move {
        let mut out = String::new();
        while let Some(d) = rx.recv().await {
            if let crate::provider::StreamDelta::Text { text } = d {
                out.push_str(&text);
            }
        }
        out
    });
    // key 解析必须走钥匙串占位符解析器（[docs/topbar-migration-and-git-identity](../../../docs/topbar-migration-and-git-identity.md)）：钥匙串用户的
    // 配置 keys 原样是 ["__keyring__"]，朴素的 filter 挑选会得到 None，
    // 发出未鉴权请求（静默 401 失败）
    let Some(key) = crate::host::keyring::pick_resolved_key(&model) else {
        crate::core::session_log::warn(
            &rt,
            "自动命名跳过：模型 key 在钥匙串中解析为空（keys 含占位符但回读失败）",
        );
        return;
    };
    // 独立 cancel token：用户停止 run 不得连带取消命名；超时另行兜底
    let cancel = CancellationToken::new();
    // 读锁 clone（std 锁不能跨 await）；save_config 热替换后此处取到新代理的 client
    let client = core.client.read().unwrap().clone();
    let usage = match tokio::time::timeout(
        TITLE_TIMEOUT,
        crate::provider::stream_model(&client, &model, Some(key), req, tx, cancel),
    )
    .await
    {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => {
            crate::core::session_log::warn(&rt, &format!("自动命名失败：{e}"));
            return;
        }
        Err(_) => {
            crate::core::session_log::warn(&rt, "自动命名超时，保留默认标题");
            return;
        }
    };
    let raw = collect.await.unwrap_or_default();

    let Some(title) = sanitize_title(&raw) else {
        crate::core::session_log::warn(&rt, "自动命名输出无效，保留默认标题");
        return;
    };
    // 覆盖守卫 + 删除复查：生成期间用户已手动改名或会话已删除则放弃
    if rt.zombie.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    {
        // check+write 同一临界区完成（lock_ok 防锁中毒）：与 rename_session 的写互斥，
        // 防止迟到的自动标题在首条消息后静默覆盖手动改名
        let mut t = crate::core::agent::lock_ok(&rt.title);
        if !should_replace_title(&t, &fallback_title) {
            drop(t);
            crate::core::session_log::info(&rt, "生成期间会话已被手动重命名，放弃覆盖");
            return;
        }
        *t = title.clone();
    }
    // 已建索引 → 轻量改名（仅索引，不重写历史）；未建索引 → 由 run 结束检查点兜底落盘。
    // 注意：与并发检查点的「读标题 → gzip → 写」交错时索引可能短暂回落兜底值；
    // 后续任一检查点自愈（首条消息处历史极小、窗口微秒级）——可接受。
    if core.store.get(&rt.id).is_some() {
        if let Err(e) = core.store.rename_in_index(&rt.id, &title) {
            tracing::warn!("自动命名索引更新失败：{e}");
        }
    }
    // 计账（kind=title，[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md)：消耗不得凭空消失；usage 缺失时估算）
    let usage = if usage.input + usage.output == 0 {
        crate::provider::RunUsage {
            input: crate::util::token_est::est_tokens_text(&clipped),
            output: crate::util::token_est::est_tokens_text(&title),
            ..Default::default()
        }
    } else {
        usage
    };
    core.stats.record(crate::core::stats::UsageRecord {
        session: rt.id.clone(),
        model_id: model.id.clone(),
        workspace: rt.workspace.to_string_lossy().into_owned(),
        input: usage.input,
        output: usage.output,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        runs: 1,
        kind: crate::core::stats::KIND_TITLE.into(),
    });
    crate::core::session_log::info(
        &rt,
        &format!(
            "自动命名：{title}（耗时 {}ms）",
            started.elapsed().as_millis()
        ),
    );
    core.sink.emit(
        &rt.id,
        "session:title",
        serde_json::json!({ "session": rt.id, "title": title }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_takes_first_line_and_truncates() {
        assert_eq!(
            sanitize_title("修复登录超时\n多余行").unwrap(),
            "修复登录超时"
        );
        let long = "一".repeat(30);
        assert_eq!(sanitize_title(&long).unwrap(), "一".repeat(MAX_TITLE_CHARS));
    }

    #[test]
    fn sanitize_strips_wrapping_quotes() {
        assert_eq!(sanitize_title("「修复登录超时」").unwrap(), "修复登录超时");
        assert_eq!(
            sanitize_title("\"Fix login bug\"").unwrap(),
            "Fix login bug"
        );
        assert_eq!(
            sanitize_title("  ‘重构会话存储’  ").unwrap(),
            "重构会话存储"
        );
    }

    #[test]
    fn sanitize_strips_title_prefix() {
        assert_eq!(
            sanitize_title("标题：修复登录超时").unwrap(),
            "修复登录超时"
        );
        assert_eq!(sanitize_title("标题: Fix login").unwrap(), "Fix login");
    }

    #[test]
    fn sanitize_invalid_inputs_yield_none() {
        assert!(sanitize_title("").is_none());
        assert!(sanitize_title("   \n \t ").is_none());
    }

    #[test]
    fn replace_guard_protects_manual_rename() {
        assert!(should_replace_title("", "兜底"));
        assert!(should_replace_title("兜底", "兜底"));
        assert!(!should_replace_title("手动名", "兜底"));
    }

    #[test]
    fn registry_body_shares_single_source() {
        assert_eq!(
            crate::agents::find("title").map(|d| d.body),
            Some(crate::agents::TITLE_BODY)
        );
    }
}
