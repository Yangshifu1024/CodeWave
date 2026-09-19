//! 写后语义校验（[docs/lsp-post-write-diagnostics](../../../docs/lsp-post-write-diagnostics.md)）：
//! create / edit 成功后对已写入的文件做校验，文案经工具结果的 `warnings` 回喂模型。
//!
//! 两条路径并列：
//! - **JSON 内置解析**（`serde_json`，行为与此前一致，不涉及 LSP）；
//! - **六语言 LSP 语义校验**（`crate::lsp`）：写前基线 → 写入 → 写后差集 → 三态文案。
//!
//! 不可退让的一点：**没真的跑过，绝不说「通过」**。三态里只有跑完且无 error 的
//! `Passed` 才允许出现「通过」字样——历史实现「未运行却显示校验通过」的假阴性
//! 由此结构性消除；多文件批次逐文件成文，绝不因为批里有一个文件跑过就给整批打「通过」。

use super::ToolCtx;
use crate::core::config::ValidationSettings;
use crate::core::types::Message;
use crate::lsp::diagnostics;
use crate::lsp::{Baseline, Lang, SkipReason, ValidateRequest, ValidationOutcome};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// JSON 内置解析的单文件结果（唯一服务 `.json` 的轻量路径）。
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// 是否真的运行了校验。
    pub ran: bool,
    /// 校验是否通过（未运行时无意义）。
    pub ok: bool,
    /// 失败详情（成功为空串）。
    pub message: String,
}

/// 单文件写后校验结果：内置 JSON 与 LSP 语义两条路径的并列形态。
#[derive(Debug, Clone)]
pub enum FileCheck {
    /// JSON：内置解析（不涉及 LSP）
    Json(ValidationReport),
    /// 其余：LSP 语义校验三态（通过 / 有诊断 / 跳过带原因）
    Lsp(ValidationOutcome),
}

/// 带展示路径的单文件结果（[`summarize`] 的输入单元）。
#[derive(Debug, Clone)]
pub struct CheckedFile {
    /// 展示用路径（工作区相对路径）
    pub path: String,
    /// 校验结论
    pub check: FileCheck,
}

/// 一次待校验写入的目标（**写前**构造并跨写入持有）。
#[derive(Debug, Clone)]
pub struct WriteTarget {
    /// 被写文件绝对路径
    pub abs: PathBuf,
    /// 展示用相对路径
    pub rel: String,
    /// 写入前内容（`None` = 新建文件或读不到 → 基线视为空集）
    pub prev_content: Option<String>,
}

impl WriteTarget {
    /// 构造。
    pub fn new(abs: PathBuf, rel: String, prev_content: Option<String>) -> Self {
        WriteTarget {
            abs,
            rel,
            prev_content,
        }
    }
}

/// 路径路由：六语言走 LSP；`.json` 走内置；其余明确 `Unsupported`（**不静默**）。
enum Route {
    /// 内置 JSON 解析
    Json,
    /// 该类型本批次不覆盖
    Unsupported,
    /// LSP 语义校验
    Lsp(Lang),
}

/// 按扩展名路由（`.vue`/`.svelte` 等未覆盖类型不落到 LSP 路径，由文案如实告知）。
fn route(path: &Path) -> Route {
    if let Some(lang) = Lang::from_path(path) {
        return Route::Lsp(lang);
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "json" {
        Route::Json
    } else {
        Route::Unsupported
    }
}

/// JSON 内置解析（行为与此前一致：解析失败回喂错误文案）。
pub fn json_check(path: &Path) -> ValidationReport {
    let bytes = std::fs::read(path).unwrap_or_default();
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(_) => ValidationReport {
            ran: true,
            ok: true,
            message: String::new(),
        },
        Err(e) => ValidationReport {
            ran: true,
            ok: false,
            message: format!("JSON 解析失败：{e}"),
        },
    }
}

/// 请求装配：project_id / 根取会话快照；`new_content = None` 让 manager 自己读盘。
fn request_for(ctx: &ToolCtx, t: &WriteTarget, lang: Lang) -> ValidateRequest {
    ValidateRequest {
        project_id: ctx.rt.project_id.clone(),
        project_root: ctx.rt.workspace.clone(),
        path: t.abs.clone(),
        rel_path: t.rel.clone(),
        lang,
        new_content: None,
        prev_content: t.prev_content.clone(),
    }
}

/// 写前基线（**必须在写入之前调用**，此刻盘上还是旧内容）。
///
/// 返回值与同一次写入的写后校验配对；`None` = 该文件本轮不做 LSP 语义校验
/// （未覆盖类型 / 临时会话 / 开关关闭 / server 未就绪）——此时**不回喂任何诊断**。
pub async fn baseline(
    ctx: &ToolCtx,
    t: &WriteTarget,
    cfg: &ValidationSettings,
) -> Option<Baseline> {
    let Route::Lsp(lang) = route(&t.abs) else {
        return None;
    };
    let mgr = ctx.core.lsp.clone();
    // 开关快照同步：manager 内部持一份 ValidationSettings，不投喂则用户改的开关不生效
    mgr.set_settings(cfg);
    mgr.baseline(&request_for(ctx, t, lang), &cfg.lsp).await
}

/// 写后校验单个文件（写前必须已取 [`baseline`] 并配对传入）。
///
/// `Skipped{ServerLoading}`（基线可信、只是诊断迟到）时顺带 spawn 异步补条；
/// `Skipped{ServerNotReady}`（基线不可信）**不补条**——补发同样拿不到可信基线，
/// 文案也不得承诺「稍后补发诊断」；
/// `Skipped{NoServer}`（含「默认关闭」语言）时发 `lsp:server_missing` 引导事件。
pub async fn check(
    ctx: &ToolCtx,
    t: &WriteTarget,
    base: Option<&Baseline>,
    cfg: &ValidationSettings,
) -> CheckedFile {
    let check = match route(&t.abs) {
        Route::Json => {
            if cfg.json {
                FileCheck::Json(json_check(&t.abs))
            } else {
                // JSON 开关关闭：与 LSP 语言同一套「未运行如实告知」的文案路径
                FileCheck::Lsp(ValidationOutcome::Skipped {
                    lang: None,
                    reason: SkipReason::Disabled,
                })
            }
        }
        Route::Unsupported => FileCheck::Lsp(ValidationOutcome::Skipped {
            lang: None,
            reason: SkipReason::Unsupported,
        }),
        Route::Lsp(lang) => {
            let mgr = ctx.core.lsp.clone();
            mgr.set_settings(cfg);
            let req = request_for(ctx, t, lang);
            let outcome = mgr.validate(&req, base, &cfg.lsp).await;
            emit_hint_if_needed(ctx, lang, &outcome, cfg).await;
            if let ValidationOutcome::Skipped {
                lang: Some(l),
                reason: SkipReason::ServerLoading,
            } = &outcome
            {
                // 同步窗口没等到诊断：交给异步补条（前缀标注「异步补发」）
                let rel = req.rel_path.clone();
                spawn_async_supplement(ctx, *l, rel, req, base.cloned(), cfg.clone());
            }
            FileCheck::Lsp(outcome)
        }
    };
    CheckedFile {
        path: t.rel.clone(),
        check,
    }
}

/// LSP 三态 → 文案（同步结果与异步补条共用同一套措辞，不另起说法）。
///
/// `ran() == false` 的场合文案里绝不允许出现「校验通过」——跳过必须如实带原因。
pub fn outcome_text(path: &str, outcome: &ValidationOutcome, max_chars: usize) -> String {
    match outcome {
        ValidationOutcome::Passed { lang } => format!(
            "（写入后语义校验通过：{}）",
            lang.display_name()
        ),
        ValidationOutcome::Diagnosed {
            items,
            removed,
            truncated,
            ..
        } => diagnostics::format_feedback(items, *removed, *truncated, max_chars),
        ValidationOutcome::Skipped { lang, reason } => match reason {
            SkipReason::ServerLoading => format!(
                "（{} 语义校验未就绪：server 正在启动，稍后补发诊断）",
                lang_label(*lang)
            ),
            // 无基线/预热中：**不得**承诺「稍后补发诊断」（异步补条拿不到可信基线，本轮就是不回喂）
            SkipReason::ServerNotReady => format!(
                "（{} 语义校验未就绪：server 正在预热，本轮未回喂诊断）",
                lang_label(*lang)
            ),
            SkipReason::NoServer { server } => format!(
                "（未对 {} 做语义校验：未找到 {server}——本次写入未被校验，不要假装它通过了）",
                lang_label(*lang)
            ),
            SkipReason::Disabled => {
                format!("（{} 语义校验已在设置中关闭）", lang_label(*lang))
            }
            SkipReason::NoProject => "（临时会话不做语义校验）".to_string(),
            SkipReason::Unsupported => {
                format!("（该文件类型不做语义校验：{}）", file_kind(path))
            }
            SkipReason::FileTooLarge { bytes } => {
                format!("（文件过大，已跳过语义校验：{bytes} 字节）")
            }
        },
    }
}

/// 单文件文案（空串 = 无需回喂）。
pub fn single_text(path: &str, check: &FileCheck, cfg: &ValidationSettings) -> String {
    match check {
        FileCheck::Lsp(outcome) => outcome_text(path, outcome, cfg.lsp.max_chars),
        FileCheck::Json(r) => {
            if !r.ran {
                return String::new();
            }
            if r.ok {
                return "（写入后语义校验通过：JSON）".to_string();
            }
            format!(
                "（写入后语义校验发现 1 个错误，请用 edit 修复：\n{path} {}\n）",
                r.message
            )
        }
    }
}

/// 把逐文件结果折叠为回喂文本：**每个文件各自成篇**——跑过的说跑过的结论，
/// 跳过的说跳过的原因，绝不因为批里某个文件跑过就给整批打「通过」。
pub fn summarize(files: &[CheckedFile], cfg: &ValidationSettings) -> String {
    let parts: Vec<String> = files
        .iter()
        .map(|f| single_text(&f.path, &f.check, cfg))
        .filter(|s| !s.is_empty())
        .collect();
    parts.join("\n")
}

/// 语言展示名（`Skipped{lang: None}` 只可能来自 JSON 开关关闭，故回落为 JSON）。
fn lang_label(lang: Option<Lang>) -> String {
    match lang {
        Some(l) => l.display_name().to_string(),
        None => "JSON".to_string(),
    }
}

/// 文件类型标签：优先扩展名，其次文件名，最后「未知类型」（文案里必须看得见是什么被跳过了）。
fn file_kind(path: &str) -> String {
    let p = Path::new(path);
    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        if !ext.is_empty() {
            return ext.to_ascii_lowercase();
        }
    }
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未知类型".to_string())
}

/// 发 `lsp:server_missing` 引导事件（事件面第 29 键，前端 `LspGuideCard` 的数据源）。
///
/// 两种触发场景（同一 `ServerStatus.install` 数据源）：
/// - `Skipped{NoServer}`：该语言**已启用但没找到 server**（kind 取解析结果；
///   Java = `manual` + prerequisite「需 JDK 21+」）；
/// - `Skipped{Disabled}` 且该语言**默认就是关的**（Java）：kind = `confirm_enable`，
///   前端卡片给「启用」按钮（其余语言是用户自己关的，不该弹 Java 专属文案的卡片）。
///
/// 同一次写入的同一文件最多一条；跨文件同名事件由前端按 (language, project_id) 去重。
async fn emit_hint_if_needed(
    ctx: &ToolCtx,
    lang: Lang,
    outcome: &ValidationOutcome,
    cfg: &ValidationSettings,
) {
    let ValidationOutcome::Skipped { reason, .. } = outcome else {
        return;
    };
    let confirmed_disabled = match reason {
        SkipReason::NoServer { .. } => false,
        // 「默认就关」的语言（Java）才弹启用引导：用户自己关掉的别的语言不该收到
        // 这张 Java 专属文案的卡片（前端 confirm_enable 景的标题/代价说明按 Java 写）
        SkipReason::Disabled => !lang.enabled_in(&ValidationSettings::default()),
        _ => return,
    };
    let status = ctx.core.lsp.status_cached(cfg).await;
    let Some(st) = status.iter().find(|s| s.language == lang.id()) else {
        tracing::debug!(lang = lang.id(), "引导事件跳过：状态查询未命中该语言");
        return;
    };
    let hint = st.install.clone();
    let (kind, command, docs_url, prerequisite) = match &hint {
        Some(h) => (
            h.kind,
            h.command.clone(),
            h.docs_url.clone(),
            h.prerequisite.clone(),
        ),
        None => (
            // 找不到引导信息（理论上不该发生）→ 最保守形态：只给官方地址
            crate::lsp::InstallKind::Manual,
            None,
            Some(crate::lsp::server_spec::spec(lang).install.docs_url.to_string()),
            None,
        ),
    };
    // 默认关闭的启用引导：理由文案明说「默认关闭」；未找到则是解析结果里的 detail
    let reason_text = if confirmed_disabled {
        format!("{} 语义校验默认关闭，启用后才会校验", lang.display_name())
    } else {
        match reason {
            SkipReason::NoServer { server } => format!("未找到 {server}"),
            _ => format!("{} 语义校验未运行", lang.display_name()),
        }
    };
    ctx.core.sink.emit(
        &ctx.rt.id,
        "lsp:server_missing",
        serde_json::json!({
            "language": lang.id(),
            "project_id": ctx.rt.project_id.clone(),
            "kind": kind,
            "server": crate::lsp::server_spec::spec(lang).server_name,
            "command": command,
            "docs_url": docs_url,
            "prerequisite": prerequisite,
            "reason": reason_text,
        }),
    );
}

/// 异步补条：同步窗口内没等到诊断（`Skipped{ServerLoading}`）时接管，拿到结论后经
/// **既有注入通道**（`SessionRuntime::inject_tx`，与 `inject_run_message` 同一通道）
/// 注入一条合成消息，前缀标注「异步补发的语义校验结果」。
///
/// 纪律：会话已结束 / 已取消 / 已删除（zombie）时静默丢弃并记日志；注入失败只记日志，
/// 绝不影响工具结果（也不 panic）。
fn spawn_async_supplement(
    ctx: &ToolCtx,
    lang: Lang,
    rel: String,
    req: ValidateRequest,
    base: Option<Baseline>,
    cfg: ValidationSettings,
) {
    let mgr = ctx.core.lsp.clone();
    let rt = ctx.rt.clone();
    let cancel = ctx.cancel.clone();
    tokio::spawn(async move {
        let outcome = match mgr.validate_async(req, base, cfg.lsp.clone()).await {
            Some(o) => o,
            None => {
                tracing::debug!(lang = lang.id(), "异步补条未取得可信结论（无基线/未就绪），不回喂");
                return;
            }
        };
        // 会话已结束（run 收尾）/ 已取消 / 已删除：注入无意义且会污染下一轮，静默丢弃
        if rt.zombie.load(Ordering::SeqCst)
            || !rt.running.load(Ordering::SeqCst)
            || cancel.is_cancelled()
        {
            tracing::info!(lang = lang.id(), "异步补条丢弃：会话已结束或已取消");
            return;
        }
        let text = format!(
            "（异步补发的语义校验结果）{}",
            outcome_text(&rel, &outcome, cfg.lsp.max_chars)
        );
        match rt.inject_tx.try_send(Message::user_text(text)) {
            Ok(()) => tracing::info!(lang = lang.id(), path = %rel, "异步补条已注入"),
            Err(e) => tracing::warn!(lang = lang.id(), "异步补条注入失败：{e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::test_support::make_core;
    use crate::lsp::DiagnosticItem;

    fn vcfg() -> ValidationSettings {
        ValidationSettings::default()
    }

    #[test]
    fn json_builtin_check() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("a.json");
        std::fs::write(&good, b"{\"x\":1}").unwrap();
        let bad = dir.path().join("b.json");
        std::fs::write(&bad, b"{broken").unwrap();
        assert!(json_check(&good).ok);
        let r = json_check(&bad);
        assert!(!r.ok);
        assert!(r.message.contains("JSON"));
    }

    #[test]
    fn passed_text_names_the_language() {
        let t = outcome_text(
            "src/main.rs",
            &ValidationOutcome::Passed { lang: Lang::Rust },
            4000,
        );
        assert_eq!(t, "（写入后语义校验通过：Rust）");
    }

    #[test]
    fn diagnosed_text_uses_diagnostics_renderer() {
        let items = vec![DiagnosticItem {
            path: "ui/src/x.ts".into(),
            line: 3,
            col: 5,
            code: "TS2322".into(),
            message: "Type 'string' is not assignable to type 'number'.".into(),
            fingerprint: "f".into(),
        }];
        let out = ValidationOutcome::Diagnosed {
            lang: Lang::TypeScript,
            items,
            removed: 0,
            truncated: 0,
        };
        let t = outcome_text("ui/src/x.ts", &out, 4000);
        assert!(
            t.starts_with("（写入后语义校验发现 1 个错误，请用 edit 修复：\n")
        );
        assert!(t.contains("ui/src/x.ts:3:5 TS2322"));
        assert!(t.ends_with('）'));
    }

    #[test]
    fn skipped_texts_are_explicit_and_never_claim_success() {
        let cases: Vec<(ValidationOutcome, &str)> = vec![
            (
                ValidationOutcome::Skipped {
                    lang: Some(Lang::Rust),
                    reason: SkipReason::ServerLoading,
                },
                "server 正在启动，稍后补发诊断",
            ),
            (
                ValidationOutcome::Skipped {
                    lang: Some(Lang::Rust),
                    reason: SkipReason::ServerNotReady,
                },
                "语义校验未就绪：server 正在预热，本轮未回喂诊断",
            ),
            (
                ValidationOutcome::Skipped {
                    lang: Some(Lang::Rust),
                    reason: SkipReason::NoServer {
                        server: "rust-analyzer".into(),
                    },
                },
                "未对 Rust 做语义校验：未找到 rust-analyzer——本次写入未被校验，不要假装它通过了",
            ),
            (
                ValidationOutcome::Skipped {
                    lang: Some(Lang::Rust),
                    reason: SkipReason::Disabled,
                },
                "Rust 语义校验已在设置中关闭",
            ),
            (
                ValidationOutcome::Skipped {
                    lang: None,
                    reason: SkipReason::NoProject,
                },
                "临时会话不做语义校验",
            ),
            (
                ValidationOutcome::Skipped {
                    lang: None,
                    reason: SkipReason::Unsupported,
                },
                "该文件类型不做语义校验：vue",
            ),
            (
                ValidationOutcome::Skipped {
                    lang: Some(Lang::Go),
                    reason: SkipReason::FileTooLarge { bytes: 2048 },
                },
                "文件过大，已跳过语义校验：2048 字节",
            ),
        ];
        for (outcome, needle) in cases {
            let t = outcome_text("ui/src/App.vue", &outcome, 4000);
            assert!(!outcome.ran(), "Skipped 的 ran() 必须为 false：{t}");
            assert!(t.contains(needle), "期望含「{needle}」，实得：{t}");
            assert!(
                !t.contains("校验通过") && !t.contains("语义校验通过"),
                "未运行的场合绝不允许出现「通过」：{t}"
            );
        }
    }

    #[test]
    fn unsupported_kind_falls_back_to_file_name() {
        let out = ValidationOutcome::Skipped {
            lang: None,
            reason: SkipReason::Unsupported,
        };
        assert!(outcome_text("Dockerfile", &out, 4000).contains("Dockerfile"));
    }

    #[test]
    fn json_disabled_reads_as_disabled_not_as_passed() {
        let t = single_text(
            "a.json",
            &FileCheck::Lsp(ValidationOutcome::Skipped {
                lang: None,
                reason: SkipReason::Disabled,
            }),
            &vcfg(),
        );
        assert_eq!(t, "（JSON 语义校验已在设置中关闭）");
    }

    #[test]
    fn summarize_keeps_per_file_verdicts() {
        let files = vec![
            CheckedFile {
                path: "src/main.rs".into(),
                check: FileCheck::Lsp(ValidationOutcome::Passed { lang: Lang::Rust }),
            },
            CheckedFile {
                path: "ui/src/App.vue".into(),
                check: FileCheck::Lsp(ValidationOutcome::Skipped {
                    lang: None,
                    reason: SkipReason::Unsupported,
                }),
            },
        ];
        let s = summarize(&files, &vcfg());
        assert!(s.contains("（写入后语义校验通过：Rust）"), "{s}");
        assert!(s.contains("该文件类型不做语义校验：vue"), "{s}");
        // 整批不得因某个文件跑过就打「通过」——只有该文件自己的那句才说通过
        assert_eq!(s.matches("校验通过").count(), 1, "{s}");
    }

    #[test]
    fn summarize_all_skipped_says_nothing_about_passing() {
        let files = vec![
            CheckedFile {
                path: "a.json".into(),
                check: FileCheck::Json(ValidationReport {
                    ran: true,
                    ok: false,
                    message: "JSON 解析失败：expected value".into(),
                }),
            },
            CheckedFile {
                path: "b.vue".into(),
                check: FileCheck::Lsp(ValidationOutcome::Skipped {
                    lang: None,
                    reason: SkipReason::Unsupported,
                }),
            },
        ];
        let s = summarize(&files, &vcfg());
        assert!(s.contains("JSON 解析失败"), "{s}");
        assert!(s.contains("该文件类型不做语义校验：vue"), "{s}");
        assert!(!s.contains("校验通过"), "{s}");
    }

    #[test]
    fn route_covers_json_lsp_and_unsupported() {
        assert!(matches!(route(Path::new("a.json")), Route::Json));
        assert!(matches!(route(Path::new("a.ts")), Route::Lsp(Lang::TypeScript)));
        assert!(matches!(route(Path::new("a.rs")), Route::Lsp(Lang::Rust)));
        assert!(matches!(route(Path::new("b.vue")), Route::Unsupported));
        assert!(matches!(route(Path::new("b.svelte")), Route::Unsupported));
        assert!(matches!(route(Path::new("noext")), Route::Unsupported));
    }

    /// 端到端接线（不启动任何 server）：临时会话 + 已关闭语言 + 未覆盖类型三条
    /// 早退路径必须各自给出如实文案，且都不触发 LSP 进程。
    #[tokio::test]
    async fn wiring_short_circuits_without_spawning_servers() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = make_core(&roots);
        let ctx = ToolCtx {
            core: core.clone(),
            rt: core.get_or_create_session(
                "t",
                roots.workspace.clone(),
                None,
                vec![],
                None,
                vec![],
            ),
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let abs = roots.workspace.join("src/main.rs");
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, "fn main() {}\n").unwrap();
        let target = WriteTarget::new(abs.clone(), "src/main.rs".into(), Some(String::new()));
        // 临时会话（project_id = None）→ NoProject，且不碰 server
        let cfg = vcfg();
        assert!(baseline(&ctx, &target, &cfg).await.is_none());
        let checked = check(&ctx, &target, None, &cfg).await;
        assert_eq!(checked.path, "src/main.rs");
        let text = single_text(&checked.path, &checked.check, &cfg);
        assert_eq!(text, "（临时会话不做语义校验）");
        assert_eq!(core.lsp.server_count(), 0, "早退路径不得拉起 server");

        // 开关关闭的语言 → Disabled（同样不碰 server）
        let mut cfg2 = vcfg();
        cfg2.rust = false;
        let rt2 = core.get_or_create_session(
            "p",
            roots.workspace.clone(),
            Some("proj".into()),
            vec![],
            None,
            vec![],
        );
        let ctx2 = ToolCtx {
            core: core.clone(),
            rt: rt2,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let checked = check(&ctx2, &target, None, &cfg2).await;
        assert_eq!(
            single_text(&checked.path, &checked.check, &cfg2),
            "（Rust 语义校验已在设置中关闭）"
        );

        // 未覆盖类型 → Unsupported（`.vue` 不在 Lang 里）
        let vue = roots.workspace.join("ui/App.vue");
        std::fs::create_dir_all(vue.parent().unwrap()).unwrap();
        std::fs::write(&vue, "<template/>").unwrap();
        let t2 = WriteTarget::new(vue, "ui/App.vue".into(), None);
        let checked = check(&ctx2, &t2, None, &cfg).await;
        assert_eq!(
            single_text(&checked.path, &checked.check, &cfg),
            "（该文件类型不做语义校验：vue）"
        );
        // JSON 走内置（真解析）
        let js = roots.workspace.join("data.json");
        std::fs::write(&js, b"{broken").unwrap();
        let t3 = WriteTarget::new(js, "data.json".into(), None);
        let checked = check(&ctx2, &t3, None, &cfg).await;
        let text = single_text(&checked.path, &checked.check, &cfg);
        assert!(text.contains("JSON 解析失败"), "{text}");
        assert_eq!(core.lsp.server_count(), 0);
    }
}
