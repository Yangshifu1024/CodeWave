//! Goal delivery contract, user-selected budgets and tool-backed verification records.
use super::{
    AgentCore, SessionRuntime,
    goal::{GoalState, GoalStatus, goal_gate_rt},
};
use crate::tools::{ToolCtx, ToolOutcome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{path::Path, sync::Arc};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalBudget {
    #[serde(default)]
    pub token_limit: Option<u64>,
    #[serde(default)]
    pub time_limit_ms: Option<u64>,
    #[serde(default)]
    pub unlimited: bool,
}
impl GoalBudget {
    pub fn validate(&self) -> Result<(), String> {
        if self.token_limit == Some(0)
            || self.time_limit_ms == Some(0)
            || self.unlimited == (self.token_limit.is_some() || self.time_limit_ms.is_some())
        {
            return Err("请选择正数预算或明确选择不设上限，两者不能同时设置".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalEvidence {
    /// Zero-based index into the approved criterion list.
    pub criterion: usize,
    pub call_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCheck {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalSourceDocument {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub snapshot_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalVerification {
    pub call_id: String,
    pub tool: String,
    pub summary: String,
    pub passed: bool,
    pub fingerprint: String,
    pub review: bool,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalDelivery {
    /// Only the user-facing IPC can choose a budget. None never means unlimited.
    pub budget: Option<GoalBudget>,
    pub used_tokens: u64,
    pub elapsed_ms: u64,
    pub sources: Vec<String>,
    pub baseline: Vec<String>,
    pub evidence: Vec<GoalEvidence>,
    pub verifications: Vec<GoalVerification>,
    pub source_fingerprint: Option<String>,
    pub source_documents: Vec<GoalSourceDocument>,
}

impl GoalDelivery {
    pub fn budget_exhausted(&self) -> Option<String> {
        let Some(budget) = &self.budget else {
            return Some("请先明确设置目标预算或选择不设上限".into());
        };
        if let Err(e) = budget.validate() {
            return Some(e);
        }
        if budget
            .token_limit
            .is_some_and(|limit| self.used_tokens >= limit)
        {
            return Some("目标累计 token 预算已用完，请增加预算后继续".into());
        }
        if budget
            .time_limit_ms
            .is_some_and(|limit| self.elapsed_ms >= limit)
        {
            return Some("目标累计执行时长预算已用完，请增加预算后继续".into());
        }
        None
    }
    pub fn add_tokens(&mut self, tokens: u64) {
        self.used_tokens = self.used_tokens.saturating_add(tokens);
    }
}

/// Content fingerprint (not mtimes): external edits, deletions and new files invalidate evidence.
/// Build caches and goal's own persistence must not invalidate a verification that just finished.
pub fn workspace_fingerprint(workspace: &Path, sources: &[String]) -> Result<String, String> {
    let mut paths = Vec::new();
    let managed_root = workspace.file_name().is_some_and(|n| n == ".codewave");
    let walker = ignore::WalkBuilder::new(workspace)
        .hidden(false)
        .git_ignore(false)
        .ignore(false)
        .parents(false)
        .git_exclude(false)
        .git_global(false)
        .filter_entry(move |entry| {
            if managed_root
                && entry.depth() == 1
                && matches!(
                    entry.file_name().to_str(),
                    Some(
                        "sessions"
                            | "histories"
                            | "logs"
                            | "stats"
                            | "config.json"
                            | "memory"
                            | "skills"
                            | "tasks"
                            | "tmp"
                    )
                )
            {
                return false;
            }
            entry.depth() == 0
                || (!matches!(
                    entry.file_name().to_str(),
                    Some(".git" | ".codewave" | "node_modules" | ".venv" | "__pycache__")
                ) && !(entry.path().parent().is_some_and(|p| {
                    (entry.file_name() == "target" && p.join("Cargo.toml").exists())
                        || (matches!(
                            entry.file_name().to_str(),
                            Some("dist" | "build" | ".next" | "coverage")
                        ) && p.join("package.json").exists())
                })))
        })
        .build();
    for entry in walker {
        let entry = entry.map_err(|e| format!("无法建立验收快照：{e}"))?;
        if entry
            .file_type()
            .is_some_and(|t| t.is_file() || t.is_symlink())
        {
            paths.push(entry.into_path());
            if paths.len() > 50_000 {
                return Err("验收快照超过 50000 个文件，请缩小项目范围".into());
            }
        }
    }
    // An output-like name must never hide tracked source. Git remains strictly read-only.
    if let Ok(repo) = git2::Repository::discover(workspace) {
        if let (Some(workdir), Ok(index)) = (repo.workdir(), repo.index()) {
            for entry in index.iter() {
                if let Ok(relative) = std::str::from_utf8(&entry.path) {
                    let path = workdir.join(relative);
                    if path.starts_with(workspace) && path.is_file() {
                        paths.push(path);
                    }
                }
            }
        }
    }
    for source in sources {
        let p = Path::new(source);
        paths.push(if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace.join(p)
        });
    }
    paths.sort();
    paths.dedup();
    let mut hash = Sha256::new();
    for path in paths {
        let mut file = std::fs::File::open(&path)
            .map_err(|e| format!("无法读取验收快照 {}：{e}", path.display()))?;
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(
            file.metadata()
                .map_err(|e| e.to_string())?
                .len()
                .to_le_bytes(),
        );
        let mut buffer = [0u8; 65536];
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn source_fingerprint(workspace: &Path, sources: &[String]) -> Result<String, String> {
    let mut hash = Sha256::new();
    for source in sources {
        let path = Path::new(source);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            workspace.join(path)
        };
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("需求文档 {} 无法读取：{e}", path.display()))?;
        hash.update(source.as_bytes());
        hash.update([0]);
        hash.update(bytes);
        hash.update([0]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn source_documents(
    rt: &SessionRuntime,
    sources: &[String],
) -> Result<Vec<GoalSourceDocument>, String> {
    let mut documents = Vec::new();
    if !crate::core::sessions::cleanup::is_safe_session_id(&rt.id) {
        return Err("会话编号非法".into());
    }
    let dir = rt
        .data_dir
        .join("sessions")
        .join(format!("{}.goal.sources", rt.id));
    for source in sources {
        let path = Path::new(source);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            rt.workspace.join(path)
        };
        let metadata =
            std::fs::metadata(&path).map_err(|e| format!("需求文档 {}：{e}", path.display()))?;
        if metadata.len() > 64 * 1024 * 1024 {
            return Err("单个需求文档超过64MB，请拆分合同来源".into());
        }
        let bytes =
            std::fs::read(&path).map_err(|e| format!("需求快照 {}：{e}", path.display()))?;
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("txt");
        let snapshot = dir.join(format!("{digest}.{extension}"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        crate::util::atomic::atomic_write(&snapshot, &bytes).map_err(|e| e.to_string())?;
        let content = match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => text.chars().take(12_000).collect(),
            _ => {
                let preview = crate::tools::document::read::read_for_preview(
                    source,
                    &snapshot,
                    None,
                    None,
                    None,
                    Some("1-10"),
                );
                if preview.ok {
                    serde_json::to_string(&preview.data)
                        .unwrap_or_default()
                        .chars()
                        .take(12_000)
                        .collect()
                } else {
                    "二进制原文已完整保存在 snapshot_path；请使用合适的文档工具读取原始版本。"
                        .into()
                }
            }
        };
        documents.push(GoalSourceDocument {
            path: source.clone(),
            content,
            snapshot_path: Some(snapshot.to_string_lossy().to_string()),
        });
    }
    Ok(documents)
}

pub fn check_directory(workspace: &Path, cwd: Option<&str>) -> String {
    let path = cwd.map(Path::new).unwrap_or(workspace);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    let path = crate::tools::pathutil::canonical_best_effort(&path)
        .to_string_lossy()
        .to_string();
    #[cfg(windows)]
    let path = path.to_lowercase();
    path
}

pub fn prepare_contract(state: &mut GoalState, rt: &SessionRuntime) -> Result<(), String> {
    for (index, criterion) in state.criteria.iter_mut().enumerate() {
        if criterion.manual {
            continue;
        }
        let check = criterion.verification.as_mut().ok_or_else(|| {
            format!(
                "验收项 {} 缺少批准时的验证命令，请补充 verification.command/cwd",
                index + 1
            )
        })?;
        if check.command.trim().is_empty() {
            return Err("验收命令不能为空".into());
        }
        check.command = check.command.trim().into();
        check.cwd = Some(check_directory(&rt.workspace, check.cwd.as_deref()));
    }
    state.delivery.source_documents = source_documents(rt, &state.delivery.sources)?;
    Ok(())
}

pub async fn verification_start(
    ctx: &ToolCtx,
    name: &str,
    args: &serde_json::Value,
) -> Option<String> {
    if !matches!(name, "command" | "subagent") {
        return None;
    }
    let root = goal_gate_rt(&ctx.core, &ctx.rt);
    let state = root
        .goal_snapshot()
        .filter(|s| s.status == GoalStatus::Executing)?;
    if name == "command" {
        let command = args["command"].as_str()?.trim();
        let cwd = actual_command_directory(ctx, args);
        if !state
            .criteria
            .iter()
            .filter_map(|c| c.verification.as_ref())
            .any(|check| {
                check.command.trim() == command
                    && check_directory(&ctx.rt.workspace, check.cwd.as_deref()) == cwd
            })
        {
            return None;
        }
    } else if !matches!(args["role"].as_str(), Some("reviewer" | "code-reviewer")) {
        return None;
    }
    fingerprint(&root, &state).await.ok()
}

fn actual_command_directory(ctx: &ToolCtx, args: &serde_json::Value) -> String {
    let default_cwd = ctx.rt.project_dir.as_ref().unwrap_or(&ctx.rt.workspace);
    let cwd = args["cwd"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| default_cwd.to_string_lossy().to_string());
    check_directory(&ctx.rt.workspace, Some(&cwd))
}

pub async fn fingerprint(rt: &SessionRuntime, state: &GoalState) -> Result<String, String> {
    let ws = rt.workspace.clone();
    let sources = state.delivery.sources.clone();
    let additional: Vec<_> = state
        .criteria
        .iter()
        .filter_map(|c| c.verification.as_ref())
        .filter_map(|c| c.cwd.clone())
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut roots = vec![crate::tools::pathutil::canonical_best_effort(&ws)];
        for directory in additional {
            let path = crate::tools::pathutil::canonical_best_effort(Path::new(&directory));
            if !roots.iter().any(|r| path.starts_with(r)) {
                roots.push(path);
            }
        }
        roots.sort();
        roots.dedup();
        let mut hash = Sha256::new();
        for root in roots {
            hash.update(workspace_fingerprint(&root, &[])?.as_bytes());
        }
        hash.update(source_fingerprint(&ws, &sources)?.as_bytes());
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn review_passed(report: &str) -> bool {
    report
        .lines()
        .filter(|line| !line.trim().is_empty())
        .next_back()
        .is_some_and(|line| line.trim() == "[GOAL_REVIEW_PASS]")
        && !report.contains("[GOAL_REVIEW_FAIL]")
}

pub fn completion_error(state: &GoalState, fingerprint: &str, human: bool) -> Option<String> {
    if state.criteria.is_empty() {
        return Some("尚未定义验收标准".into());
    }
    for (i, criterion) in state.criteria.iter().enumerate() {
        if criterion.manual {
            if !human {
                continue;
            }
        } else if !criterion.done
            || !state.delivery.evidence.iter().any(|e| {
                e.criterion == i
                    && state.delivery.verifications.iter().any(|v| {
                        v.call_id == e.call_id
                            && v.tool == "command"
                            && v.passed
                            && !v.fingerprint.is_empty()
                            && v.fingerprint == fingerprint
                            && criterion.verification.as_ref().is_some_and(|c| {
                                v.command.as_deref() == Some(c.command.trim()) && v.cwd == c.cwd
                            })
                    })
            })
        {
            return Some(format!("验收项 {} 缺少当前文件版本的成功验证证据", i + 1));
        }
    }
    if !state
        .delivery
        .verifications
        .iter()
        .any(|v| v.review && v.passed && v.fingerprint == fingerprint && !v.fingerprint.is_empty())
    {
        return Some("缺少针对当前文件版本的独立审查，请调用 reviewer/code-reviewer 子代理核对原始需求并处理发现的问题".into());
    }
    if !state.blocked.is_empty() || !state.pending.is_empty() {
        return Some("仍有未解决的阻塞或待办，不能标记完成".into());
    }
    None
}

/// Called by the tool batch with the provider call id, never from model-supplied goal data.
pub async fn record_tool_result(
    ctx: &ToolCtx,
    call_id: &str,
    name: &str,
    args: &serde_json::Value,
    out: &ToolOutcome,
    started_at: Option<String>,
) {
    if started_at.is_none() {
        return;
    }
    let root = goal_gate_rt(&ctx.core, &ctx.rt);
    let Some(state) = root
        .goal_snapshot()
        .filter(|s| s.status == GoalStatus::Executing)
    else {
        return;
    };
    if !matches!(name, "command" | "subagent") {
        return;
    }
    let role = args
        .get("role")
        .or_else(|| args.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let review = name == "subagent" && matches!(role, "reviewer" | "code-reviewer");
    if name == "subagent" && !review {
        return;
    }
    let passed = out.ok
        && if name == "command" {
            out.data["exit_code"].as_i64() == Some(0)
        } else {
            out.data["ended"].as_str() == Some("report")
                && out.data["report"].as_str().is_some_and(review_passed)
        };
    let stamp = fingerprint(&root, &state).await.unwrap_or_default();
    let passed = passed && !stamp.is_empty() && started_at.as_deref() == Some(stamp.as_str());
    let summary = if name == "command" {
        format!(
            "{} (exit {})",
            args["command"].as_str().unwrap_or("command"),
            out.data["exit_code"]
        )
    } else {
        out.data["report"]
            .as_str()
            .unwrap_or("独立审查已返回；请处理报告中所有阻塞问题")
            .chars()
            .take(2000)
            .collect()
    };
    root.mutate_goal(|s| {
        if s.status != GoalStatus::Executing {
            return;
        }
        s.delivery.verifications.retain(|v| v.call_id != call_id);
        let command = if name == "command" {
            args["command"].as_str().map(|s| s.trim().to_owned())
        } else {
            None
        };
        let cwd = if name == "command" {
            Some(actual_command_directory(ctx, args))
        } else {
            None
        };
        s.delivery.verifications.push(GoalVerification {
            call_id: call_id.into(),
            tool: name.into(),
            summary,
            passed,
            fingerprint: stamp,
            review,
            command,
            cwd,
        });
        // Keep referenced evidence and the latest 200 unreferenced results.
        if s.delivery.verifications.len() > 300 {
            if let Some(index) = s
                .delivery
                .verifications
                .iter()
                .position(|v| !s.delivery.evidence.iter().any(|e| e.call_id == v.call_id))
            {
                s.delivery.verifications.remove(index);
            }
        }
    });
    persist(&ctx.core, &root);
}

pub fn persist(core: &AgentCore, rt: &SessionRuntime) {
    let state = rt.goal.lock().unwrap();
    if let Err(e) = core.store.save_goal(&rt.id, &state) {
        crate::core::session_log::info(rt, &format!("目标检查点保存失败：{e}"));
    }
    core.sink.emit(
        &rt.id,
        "goal:update",
        serde_json::json!({"session":rt.id,"goal":*state}),
    );
}

impl AgentCore {
    pub fn set_goal_budget(
        &self,
        rt: &Arc<SessionRuntime>,
        budget: GoalBudget,
    ) -> Result<GoalState, String> {
        budget.validate()?;
        if rt.running.load(std::sync::atomic::Ordering::SeqCst)
            && rt
                .goal_snapshot()
                .is_some_and(|s| s.status != GoalStatus::Clarify)
        {
            return Err("请先暂停目标再调整预算".into());
        }
        let mut guard = rt.goal.lock().unwrap();
        let state = guard.as_ref().ok_or("请先登记目标")?;
        if !matches!(
            state.status,
            GoalStatus::Clarify | GoalStatus::Paused | GoalStatus::AwaitingAcceptance
        ) {
            return Err("当前目标状态不能修改预算".into());
        }
        let mut result = state.clone();
        result.delivery.budget = Some(budget);
        self.store
            .save_goal(&rt.id, &Some(result.clone()))
            .map_err(|e| format!("预算保存失败：{e}"))?;
        *guard = Some(result.clone());
        drop(guard);
        persist(self, rt);
        Ok(result)
    }

    pub async fn accept_goal(
        self: &Arc<Self>,
        rt: &Arc<SessionRuntime>,
        accepted: bool,
        feedback: Option<String>,
    ) -> Result<GoalState, String> {
        let _run = rt
            .run_lock
            .clone()
            .try_lock_owned()
            .map_err(|_| "目标仍在停止，请稍后验收")?;
        if rt.running.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("目标仍在运行，请稍后验收".into());
        }
        let mut state = rt.goal_snapshot().ok_or("目标不存在")?;
        let original = state.clone();
        if state.status != GoalStatus::AwaitingAcceptance {
            return Err("当前目标不在待验收状态".into());
        }
        if accepted {
            let stamp = fingerprint(rt, &state).await?;
            if let Some(e) = completion_error(&state, &stamp, true) {
                return Err(e);
            }
            for criterion in &mut state.criteria {
                criterion.done = true;
            }
            state.status = GoalStatus::Done;
        } else {
            let feedback = feedback.unwrap_or_default();
            if feedback.trim().is_empty() {
                return Err("请填写需要修复的问题".into());
            }
            state
                .blocked
                .push(format!("人工验收反馈：{}", feedback.trim()));
            for criterion in &mut state.criteria {
                criterion.done = false;
            }
            state.delivery.evidence.clear();
            state.status = GoalStatus::Paused;
        }
        let mut guard = rt.goal.lock().unwrap();
        if guard.as_ref() != Some(&original) {
            return Err("目标状态已变化，请刷新后重试验收".into());
        }
        self.store
            .save_goal(&rt.id, &Some(state.clone()))
            .map_err(|e| format!("验收结果保存失败：{e}"))?;
        *guard = Some(state.clone());
        drop(guard);
        persist(self, rt);
        if accepted {
            super::apply_goal_fallback(self, rt);
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_command_output_does_not_invalidate_checks_but_project_tmp_does() {
        let root = tempfile::tempdir().unwrap();
        let managed = root.path().join(".codewave");
        std::fs::create_dir_all(&managed).unwrap();
        let before = workspace_fingerprint(&managed, &[]).unwrap();
        std::fs::create_dir(managed.join("tmp")).unwrap();
        std::fs::write(managed.join("tmp/output.txt"), vec![b'x'; 100_000]).unwrap();
        assert_eq!(before, workspace_fingerprint(&managed, &[]).unwrap());
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let before = workspace_fingerprint(&project, &[]).unwrap();
        std::fs::create_dir(project.join("tmp")).unwrap();
        std::fs::write(project.join("tmp/source.txt"), "source").unwrap();
        assert_ne!(before, workspace_fingerprint(&project, &[]).unwrap());
    }
    #[test]
    fn budgets_require_explicit_choice_and_count_across_runs() {
        assert!(GoalBudget::default().validate().is_err());
        let mut delivery = GoalDelivery {
            budget: Some(GoalBudget {
                token_limit: Some(100),
                ..Default::default()
            }),
            ..Default::default()
        };
        delivery.add_tokens(60);
        delivery.add_tokens(40);
        assert!(delivery.budget_exhausted().is_some());
        delivery.budget = Some(GoalBudget {
            unlimited: true,
            ..Default::default()
        });
        assert!(delivery.budget_exhausted().is_none());
    }
    #[test]
    fn fingerprint_tracks_content_new_files_and_external_documents() {
        let ws = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let doc = docs.path().join("requirements.md");
        std::fs::write(&doc, "v1").unwrap();
        let sources = vec![doc.to_string_lossy().to_string()];
        let first = workspace_fingerprint(ws.path(), &sources).unwrap();
        std::fs::write(ws.path().join("main.rs"), "fn main() {}").unwrap();
        let second = workspace_fingerprint(ws.path(), &sources).unwrap();
        assert_ne!(first, second);
        std::fs::write(&doc, "v2").unwrap();
        assert_ne!(second, workspace_fingerprint(ws.path(), &sources).unwrap());
    }

    fn verified_state() -> GoalState {
        let mut state: GoalState = serde_json::from_value(serde_json::json!({
            "text":"完成应用", "criteria":[{"title":"登录端到端通过","done":true,"verification":{"command":"test","cwd":"/project"}}],
            "ledger":{}, "status":"executing"
        })).unwrap();
        state.delivery.evidence.push(GoalEvidence {
            criterion: 0,
            call_id: "test-1".into(),
            summary: "实际登录流程测试".into(),
        });
        state.delivery.verifications = vec![
            GoalVerification {
                call_id: "test-1".into(),
                tool: "command".into(),
                summary: "test exit 0".into(),
                passed: true,
                fingerprint: "v1".into(),
                command: Some("test".into()),
                cwd: Some("/project".into()),
                review: false,
            },
            GoalVerification {
                call_id: "review-1".into(),
                tool: "subagent".into(),
                summary: "checked original requirements".into(),
                passed: true,
                fingerprint: "v1".into(),
                command: None,
                cwd: None,
                review: true,
            },
        ];
        state
    }

    #[test]
    fn completion_rejects_stale_failed_or_fabricated_evidence() {
        let state = verified_state();
        assert!(completion_error(&state, "v1", false).is_none());
        assert!(completion_error(&state, "v2", false).is_some());
        let mut failed = state.clone();
        failed.delivery.verifications[0].passed = false;
        assert!(completion_error(&failed, "v1", false).is_some());
        let mut fabricated = state.clone();
        fabricated.delivery.evidence[0].call_id = "invented".into();
        assert!(completion_error(&fabricated, "v1", false).is_some());
        let mut no_review = state.clone();
        no_review.delivery.verifications.pop();
        assert!(completion_error(&no_review, "v1", false).is_some());
        let mut blocked = state;
        blocked.blocked.push("缺少真实服务验证".into());
        assert!(completion_error(&blocked, "v1", false).is_some());
    }

    #[test]
    fn stale_review_is_not_revalidated_by_a_new_test() {
        let mut state = verified_state();
        state.delivery.verifications[0].fingerprint = "v2".into();
        assert!(
            completion_error(&state, "v2", false)
                .unwrap()
                .contains("独立审查")
        );
    }

    #[test]
    fn unrelated_success_command_or_review_cannot_replace_an_approved_check() {
        let mut state = verified_state();
        state.delivery.verifications[0].command = Some("echo ok".into());
        assert!(completion_error(&state, "v1", false).is_some());
        state.delivery.verifications[0].command = Some("test".into());
        state.delivery.verifications[0].cwd = Some("/another-project".into());
        assert!(completion_error(&state, "v1", false).is_some());
        state.delivery.evidence[0].call_id = "review-1".into();
        assert!(completion_error(&state, "v1", false).is_some());
    }

    #[test]
    fn quoted_review_pass_is_not_a_verdict() {
        assert!(!review_passed("修复问题后才能输出 [GOAL_REVIEW_PASS]"));
        assert!(!review_passed("[GOAL_REVIEW_PASS]\n但仍有关键缺陷"));
        assert!(!review_passed("[GOAL_REVIEW_FAIL]\n[GOAL_REVIEW_PASS]"));
        assert!(review_passed(
            "已核对原文与真实验证结果\n[GOAL_REVIEW_PASS]\n"
        ));
    }

    #[test]
    fn ignore_rules_cannot_hide_source_changes() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join(".ignore"), "src/").unwrap();
        std::fs::create_dir_all(ws.path().join("src/build")).unwrap();
        let source = ws.path().join("src/build/compiler.rs");
        std::fs::write(&source, "v1").unwrap();
        let before = workspace_fingerprint(ws.path(), &[]).unwrap();
        std::fs::write(&source, "v2").unwrap();
        assert_ne!(before, workspace_fingerprint(ws.path(), &[]).unwrap());
    }

    #[test]
    fn ignored_build_outputs_do_not_invalidate_source_checks() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("app.rs"), "v1").unwrap();
        std::fs::write(ws.path().join("Cargo.toml"), "[package]").unwrap();
        let before = workspace_fingerprint(ws.path(), &[]).unwrap();
        std::fs::create_dir(ws.path().join("target")).unwrap();
        std::fs::write(ws.path().join("target/output"), "generated").unwrap();
        assert_eq!(before, workspace_fingerprint(ws.path(), &[]).unwrap());
        std::fs::remove_file(ws.path().join("app.rs")).unwrap();
        assert_ne!(before, workspace_fingerprint(ws.path(), &[]).unwrap());
    }
}
