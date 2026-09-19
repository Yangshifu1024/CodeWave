//! 写入后检查编排（[docs/post-write-check-plan](../../../docs/post-write-check-plan.md)）：
//! create / edit 成功后执行用户配置的检查命令，结论写进工具结果的 `outcome.data`
//! （`check` / `checks` 字段）——**必须走 `data` 通道**，只达前端的 `warnings` 模型读不到
//! （这是旧 LSP 机制的根本缺陷 B）。
//!
//! 两条路径：
//! - **检查命令**（`tools::postcheck`）：开关开启且命令非空时执行，含 `{file}` 逐文件一次、
//!   不含则整调用一次；
//! - **JSON 内置解析**：无命令时对 `.json` 文件总是执行（无需项目、无需配置）。
//!
//! 纪律：**没真的跑过，文案里绝不出现「通过」**；多文件批次逐文件成条，
//! 绝不因为批里某个文件跑过就给整批打「通过」。

use super::postcheck::{self, CheckResult, SkipReason};
use super::ToolCtx;
use crate::core::config::PostWriteCheckSettings;
use std::path::{Path, PathBuf};

/// 内置 JSON 解析结论（唯一服务 `.json` 的轻量路径；总是执行）。
#[derive(Debug, Clone)]
pub struct JsonReport {
    /// 解析是否成功
    pub ok: bool,
    /// 失败详情（成功为空串）
    pub message: String,
}

/// JSON 内置解析（解析失败回喂错误文案；读到不了的字节按空内容处理）。
pub fn json_check(path: &Path) -> JsonReport {
    let bytes = std::fs::read(path).unwrap_or_default();
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(_) => JsonReport {
            ok: true,
            message: String::new(),
        },
        Err(e) => JsonReport {
            ok: false,
            message: format!("JSON 解析失败：{e}"),
        },
    }
}

/// 一次待检查写入的目标（写入**之后**构造；不再需要写前基线）。
#[derive(Debug, Clone)]
pub struct WriteTarget {
    /// 被写文件绝对路径
    pub abs: PathBuf,
    /// 展示用相对路径（工作区相对；`{file}` 替换后即此值）
    pub rel: String,
}

impl WriteTarget {
    /// 构造。
    pub fn new(abs: PathBuf, rel: String) -> Self {
        WriteTarget { abs, rel }
    }
}

/// 带展示路径的单文件结论（`path=None` 表示命令「整个调用执行一次」，不逐文件）。
#[derive(Debug, Clone)]
pub struct CheckedFile {
    /// 展示用相对路径；`None` = 整调用一次的结果
    pub path: Option<String>,
    /// 检查结论
    pub check: CheckResult,
}

impl CheckedFile {
    /// 转为进入 `outcome.data` 的 JSON 形态：`{ "path": …, "check": { … } }`。
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "check": self.check,
        })
    }
}

/// 无命令时的回落：`.json` 走内置解析（总是执行），其余如实告知未运行原因。
///
/// 优先级说明：**用户配置了命令时命令优先**（对所有扩展名，含 `.json`）——用户显式选择用
/// 项目环境检查，就不该被内置解析顶掉；无命令时 `.json` 才有这一条零配置的兜底解析。
fn without_command(settings: &PostWriteCheckSettings, t: &WriteTarget) -> CheckResult {
    if postcheck::is_json(&t.abs) {
        let r = json_check(&t.abs);
        CheckResult::builtin_json(r.ok, r.message)
    } else if settings.enabled {
        CheckResult::skipped(SkipReason::EmptyCommand, "", "（未配置写入后检查命令）")
    } else {
        CheckResult::skipped(SkipReason::Disabled, "", "（写入后检查已在设置中关闭）")
    }
}

/// 执行写入后检查（编排入口）。
///
/// - 开关关闭 / 命令为空：`.json` 内置解析，其余按 `disabled` / `empty-command` 跳过；
/// - 开关开启且命令非空：命令优先（含 `.json`），临时会话整批 `no-project` 跳过；
/// - 命令含 `{file}`：每个被写文件执行一次、串行、逐文件成条；
/// - 命令不含 `{file}`：整个工具调用执行一次（单条，`path=None`）。
pub async fn run(
    ctx: &ToolCtx,
    settings: &PostWriteCheckSettings,
    targets: &[WriteTarget],
) -> Vec<CheckedFile> {
    let cmd = settings.command.trim();
    if !settings.enabled || cmd.is_empty() {
        return targets
            .iter()
            .map(|t| CheckedFile {
                path: Some(t.rel.clone()),
                check: without_command(settings, t),
            })
            .collect();
    }

    if ctx.rt.project_id.is_none() {
        return targets
            .iter()
            .map(|t| CheckedFile {
                path: Some(t.rel.clone()),
                check: CheckResult::skipped(
                    SkipReason::NoProject,
                    cmd,
                    "（临时会话不执行写入后检查）",
                ),
            })
            .collect();
    }

    if postcheck::has_file_placeholder(cmd) {
        let mut out = Vec::with_capacity(targets.len());
        for t in targets {
            let check = postcheck::run_command(
                ctx,
                cmd,
                Some(&t.rel),
                settings.timeout_seconds,
                settings.tail_chars,
            )
            .await;
            out.push(CheckedFile {
                path: Some(t.rel.clone()),
                check,
            });
        }
        out
    } else {
        let check = postcheck::run_command(
            ctx,
            cmd,
            None,
            settings.timeout_seconds,
            settings.tail_chars,
        )
        .await;
        vec![CheckedFile { path: None, check }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn without_command_falls_back_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("a.json");
        std::fs::write(&json, b"{\"x\":1}").unwrap();
        let rs = dir.path().join("main.rs");
        std::fs::write(&rs, b"fn main() {}\n").unwrap();
        let jt = WriteTarget::new(json, "a.json".into());
        let rt = WriteTarget::new(rs, "src/main.rs".into());

        // 开关关闭：json 仍走内置解析；rust 如实报 disabled
        let s = PostWriteCheckSettings::default();
        let jc = without_command(&s, &jt);
        assert!(jc.ran && jc.ok && jc.skipped.is_none());
        assert_eq!(jc.command, "builtin:json-parse");
        let rc = without_command(&s, &rt);
        assert!(!rc.ran);
        assert_eq!(rc.skipped, Some(SkipReason::Disabled));

        // 开关开启但命令为空：json 仍走内置解析；rust 报 empty-command
        let s = PostWriteCheckSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(without_command(&s, &jt).ok);
        assert_eq!(
            without_command(&s, &rt).skipped,
            Some(SkipReason::EmptyCommand)
        );
    }

    #[test]
    fn check_result_wire_shape() {
        let r = CheckResult::builtin_json(true, "");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["ran"], serde_json::json!(true));
        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v["command"], serde_json::json!("builtin:json-parse"));
        assert_eq!(v["skipped"], serde_json::Value::Null);

        let s = CheckResult::skipped(SkipReason::EmptyCommand, "", "");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["skipped"], serde_json::json!("empty-command"));
        assert_eq!(v["ran"], serde_json::json!(false));
    }

    #[test]
    fn checked_file_json_carries_path_and_check() {
        let f = CheckedFile {
            path: Some("ui/src/x.ts".into()),
            check: CheckResult::builtin_json(false, "boom"),
        };
        let v = f.to_json();
        assert_eq!(v["path"], serde_json::json!("ui/src/x.ts"));
        assert_eq!(v["check"]["ok"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn temp_session_skips_command_but_json_still_runs() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session("t", roots.workspace.clone(), None, vec![], None, vec![]);
        let ctx = ToolCtx {
            core: core.clone(),
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let json = roots.workspace.join("a.json");
        std::fs::write(&json, b"{\"x\":1}").unwrap();
        let rs = roots.workspace.join("main.rs");
        std::fs::write(&rs, b"fn main() {}\n").unwrap();

        // 临时会话 + 已配置命令：全部 no-project 跳过（命令不执行）
        let enabled = PostWriteCheckSettings {
            enabled: true,
            command: "echo hi {file}".into(),
            ..Default::default()
        };
        let out = run(
            &ctx,
            &enabled,
            &[
                WriteTarget::new(json.clone(), "a.json".into()),
                WriteTarget::new(rs.clone(), "main.rs".into()),
            ],
        )
        .await;
        assert_eq!(out.len(), 2);
        assert!(out
            .iter()
            .all(|c| c.check.skipped == Some(SkipReason::NoProject)));

        // 无命令：json 内置解析照样给结论（不依赖项目）
        let off = PostWriteCheckSettings::default();
        let out = run(&ctx, &off, &[WriteTarget::new(json, "a.json".into())]).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].check.ran && out[0].check.ok);
    }

    /// 命令不含 `{file}`：**整个工具调用执行一次**（单条结果，`path=None`），不逐文件重复跑。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_without_placeholder_runs_once_for_whole_call() {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        core.cfg.write().unwrap().shell.selection = Some("sh".into());
        let settings = PostWriteCheckSettings {
            enabled: true,
            command: "echo whole-call-once".into(),
            timeout_seconds: 10,
            tail_chars: 1000,
        };
        let rt = core.get_or_create_session(
            "t",
            roots.workspace.clone(),
            Some("proj".into()),
            vec![],
            None,
            vec![],
        );
        rt.set_prefs(crate::core::prefs::SessionPrefs {
            approval_mode: crate::core::prefs::ApprovalMode::AutoEdit,
            model_id: None,
            reasoning_effort: None,
        });
        let ctx = ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        let out = run(
            &ctx,
            &settings,
            &[
                WriteTarget::new(roots.workspace.join("a.ts"), "a.ts".into()),
                WriteTarget::new(roots.workspace.join("b.ts"), "b.ts".into()),
            ],
        )
        .await;
        assert_eq!(out.len(), 1, "不含 {{file}} 时整调用只跑一次：{out:?}");
        assert!(out[0].path.is_none(), "整调用结果不绑定单文件");
        assert!(out[0].check.ran && out[0].check.ok);
        assert!(out[0].check.output.contains("whole-call-once"));
    }
}
