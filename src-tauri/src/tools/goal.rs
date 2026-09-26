//! goal 工具：目标模式的登记与执行期更新（合同锁定）。
//!
//! 阶段语义（[docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md) 之外的「目标档」）：
//! - 澄清期（未登记 / `status = clarify`）：允许写 `text` / `criteria` / `ledger`（登记或修订草稿），
//!   `status` 只接受 `clarify`；此阶段工作区只读由驱动层排除写工具实现（不走 fence）。
//! - 执行期（`status = executing | paused`）：**只允许**改 `criteria[].done` 与追加 `decisions` / `pending`；
//!   改 `text`、改 `criteria[].title`（含增删条目）、改 `ledger` 一律 `E_GOAL_CONTRACT_LOCKED`。
//! - 终态（`done` / `aborted`）后再登记 `text` 视为开新目标（进度类字段清零）。
//!
//! 每次成功变更：写内存态（`SessionRuntime.goal`）+ 发 `goal:update` + 落边车 + 返回纯文本汇总。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::agent::goal::{self, GoalCriterion, GoalLedger, GoalPhase, GoalState, GoalStatus};
use crate::core::agent::goal_delivery::{self, GoalDelivery, GoalEvidence};
use serde::Deserialize;
use serde_json::{Value, json};

/// 验收标准条数上限（与 plan 的 todo 上限同量级，防一次写入撑爆提示块）。
pub const MAX_CRITERIA: usize = 100;

/// 未登记时的纯文本汇总（读操作的返回体）。
pub const NOT_REGISTERED_SUMMARY: &str = "（尚未登记目标）";

/// goal 工具入参（全部可选：只给读操作留空即读当前状态）。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 目标陈述（澄清期登记/修订；执行期锁定）。
    #[serde(default)]
    text: Option<String>,
    /// 验收标准（整体替换；执行期只允许改 `done`）。
    #[serde(default)]
    criteria: Option<Vec<CriterionIn>>,
    /// 账本（整体替换；执行期锁定）。
    #[serde(default)]
    ledger: Option<LedgerIn>,
    /// 目标状态（澄清期只接受 `clarify`；执行期接受 `executing|paused|done|aborted`）。
    #[serde(default)]
    status: Option<GoalStatus>,
    /// 关键决策（执行期追加）。
    #[serde(default)]
    decisions: Option<Vec<String>>,
    /// 待办事项（执行期追加）。
    #[serde(default)]
    pending: Option<Vec<String>>,
    #[serde(default)]
    blocked: Option<Vec<String>>,
    #[serde(default)]
    sources: Option<Vec<String>>,
    #[serde(default)]
    baseline: Option<Vec<String>>,
    #[serde(default)]
    evidence: Option<Vec<GoalEvidence>>,
}

impl Args {
    /// 是否包含任何写字段（全 None = 读操作）。
    fn has_write(&self) -> bool {
        self.text.is_some()
            || self.criteria.is_some()
            || self.ledger.is_some()
            || self.status.is_some()
            || self.decisions.is_some()
            || self.pending.is_some()
            || self.blocked.is_some()
            || self.sources.is_some()
            || self.baseline.is_some()
            || self.evidence.is_some()
    }
}

/// 单条验收标准入参。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionIn {
    /// 标题（非空）。
    title: String,
    /// 是否已达成（缺省 false）。
    #[serde(default)]
    done: Option<bool>,
    #[serde(default)]
    manual: Option<bool>,
    #[serde(default)]
    verification: Option<goal_delivery::GoalCheck>,
}

/// 账本入参（两个列表均可缺省 = 空白名单）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerIn {
    /// 允许触碰的路径。
    #[serde(default)]
    paths: Option<Vec<String>>,
    /// 允许运行的程序。
    #[serde(default)]
    programs: Option<Vec<String>>,
}

/// goal 工具：读写当前会话的目标状态。Meta 分级（操作 agent 自身状态）。
pub struct GoalTool;

/// 纯函数核心：把一次工具调用应用到当前状态。
///
/// 阶段判定、合同锁定、`done` 门、账本条目校验全在这里（无 IO、无锁，便于单测）；
/// 返回 `Err((错误码, 消息))` 时调用方不得改动任何状态。
pub fn apply(current: Option<&GoalState>, args: &Args) -> Result<GoalState, (String, String)> {
    match current {
        None => register(args),
        Some(cur) => match goal::goal_phase(Some(cur)).unwrap_or(GoalPhase::Clarify) {
            GoalPhase::Clarify => revise(cur, args),
            GoalPhase::Execute => update_in_execute(cur, args),
        },
    }
}

/// 首次登记（未登记 + 澄清期）：`text` / `criteria` / `ledger`，`status` 只能是 `clarify`。
fn register(args: &Args) -> Result<GoalState, (String, String)> {
    if args.decisions.is_some() || args.pending.is_some() {
        return Err((
            "E_GOAL_NOT_REGISTERED".into(),
            "目标尚未登记：先提供 text/criteria/sources 登记目标，之后才能追加 decisions/pending"
                .into(),
        ));
    }
    if let Some(s) = args.status {
        if s != GoalStatus::Clarify {
            return Err((
                "E_GOAL_NOT_REGISTERED".into(),
                format!(
                    "目标尚未登记：首次调用只能用 status=clarify 登记目标（收到 {}）",
                    s.label()
                ),
            ));
        }
    }
    if args.text.is_none() && args.criteria.is_none() && args.ledger.is_none() {
        return Err((
            "E_GOAL_NOT_REGISTERED".into(),
            "目标尚未登记：首次调用必须提供 text 和 criteria 以登记目标".into(),
        ));
    }
    let text = args.text.as_deref().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(("E_ARGS".into(), "登记目标必须提供非空 text".into()));
    }
    Ok(GoalState {
        text,
        criteria: build_criteria(args.criteria.as_deref())?,
        ledger: build_ledger(args.ledger.as_ref())?,
        status: GoalStatus::Clarify,
        decisions: Vec::new(),
        pending: Vec::new(),
        blocked: Vec::new(),
        rounds: 0,
        stall_streak: 0,
        ledger_denials: 0,
        delivery: GoalDelivery {
            sources: args.sources.as_deref().map(clean_list).unwrap_or_default(),
            baseline: args.baseline.as_deref().map(clean_list).unwrap_or_default(),
            ..Default::default()
        },
    })
}

/// 澄清期修订：`text` / `criteria` / `ledger` 可整体替换，`status` 只能是 `clarify`。
/// 终态（done/aborted）后带 `text` 的登记视为开新目标：清掉上一目标的进度类字段。
fn revise(cur: &GoalState, args: &Args) -> Result<GoalState, (String, String)> {
    if args.decisions.is_some() || args.pending.is_some() {
        return Err((
            "E_GOAL_CONTRACT_LOCKED".into(),
            "澄清期不接受 decisions/pending（进入执行期后才追加）".into(),
        ));
    }
    if let Some(s) = args.status {
        if s != GoalStatus::Clarify {
            return Err((
                "E_ARGS".into(),
                format!("澄清期 status 只接受 clarify（收到 {}）", s.label()),
            ));
        }
    }
    let restart =
        args.text.is_some() && matches!(cur.status, GoalStatus::Done | GoalStatus::Aborted);
    let mut next = if restart {
        GoalState {
            decisions: Vec::new(),
            pending: Vec::new(),
            blocked: Vec::new(),
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
            delivery: GoalDelivery::default(),
            ..cur.clone()
        }
    } else {
        cur.clone()
    };
    if let Some(t) = args.text.as_deref() {
        let t = t.trim();
        if t.is_empty() {
            return Err(("E_ARGS".into(), "text 不能为空".into()));
        }
        next.text = t.to_string();
    }
    if let Some(c) = args.criteria.as_deref() {
        next.criteria = build_criteria(Some(c))?;
    }
    if let Some(l) = args.ledger.as_ref() {
        next.ledger = build_ledger(Some(l))?;
    }
    if let Some(sources) = &args.sources {
        next.delivery.sources = clean_list(sources);
    }
    if let Some(baseline) = &args.baseline {
        next.delivery.baseline = clean_list(baseline);
    }
    next.status = GoalStatus::Clarify;
    Ok(next)
}

/// 执行期更新：只允许改 `criteria[].done` 与追加 `decisions` / `pending`（合同锁定其余字段）。
fn update_in_execute(cur: &GoalState, args: &Args) -> Result<GoalState, (String, String)> {
    if args.text.is_some() || args.sources.is_some() || args.baseline.is_some() {
        return Err((
            "E_GOAL_CONTRACT_LOCKED".into(),
            "执行期合同锁定：不能修改目标 text（改目标须先 status=aborted，再重新登记）".into(),
        ));
    }
    if args.ledger.is_some() {
        return Err((
            "E_GOAL_CONTRACT_LOCKED".into(),
            "执行期合同锁定：不能修改账本 ledger（paths/programs）".into(),
        ));
    }
    let mut next = cur.clone();
    if let Some(ins) = args.criteria.as_deref() {
        // 只允许勾选：标题序列（长度 + 顺序 + 文本）必须与当前完全一致
        if ins.len() != cur.criteria.len()
            || ins.iter().zip(cur.criteria.iter()).any(|(a, b)| {
                a.title.trim() != b.title
                    || a.manual.is_some_and(|m| m != b.manual)
                    || a.verification
                        .as_ref()
                        .is_some_and(|v| Some(v) != b.verification.as_ref())
            })
        {
            return Err((
                "E_GOAL_CONTRACT_LOCKED".into(),
                "执行期合同锁定：不能增删或改名验收标准（只允许把已有条目的 done 置 true/false）"
                    .into(),
            ));
        }
        for (i, a) in ins.iter().enumerate() {
            if let Some(d) = a.done {
                if next.criteria[i].manual && d {
                    return Err((
                        "E_GOAL_MANUAL_ACCEPTANCE".into(),
                        "人工验收项只能由用户确认".into(),
                    ));
                }
                next.criteria[i].done = d;
            }
        }
    }
    if let Some(ds) = args.decisions.as_deref() {
        next.decisions.extend(clean_list(ds));
    }
    if let Some(ps) = args.pending.as_deref() {
        next.pending = clean_list(ps);
    }
    if let Some(bs) = &args.blocked {
        next.blocked = clean_list(bs);
    }
    if let Some(es) = &args.evidence {
        for evidence in es {
            if evidence.criterion >= next.criteria.len()
                || evidence.summary.trim().is_empty()
                || !next
                    .delivery
                    .verifications
                    .iter()
                    .any(|v| v.call_id == evidence.call_id && v.passed)
            {
                return Err((
                    "E_GOAL_EVIDENCE".into(),
                    "验收证据必须引用实际成功工具调用，criterion 为零基序号".into(),
                ));
            }
            next.delivery
                .evidence
                .retain(|e| e.criterion != evidence.criterion);
            next.delivery.evidence.push(evidence.clone());
        }
    }
    if let Some(s) = args.status {
        match s {
            GoalStatus::Done | GoalStatus::AwaitingAcceptance => {
                // done 门：以**本次调用之后**的标准集合判定（同一次调用里勾完最后一条也放行）
                let left: Vec<&str> = next
                    .criteria
                    .iter()
                    .filter(|c| !c.done && !c.manual)
                    .map(|c| c.title.as_str())
                    .collect();
                if next.criteria.is_empty() || !left.is_empty() {
                    return Err((
                        "E_GOAL_CRITERIA_PENDING".into(),
                        format!(
                            "验收标准未全部完成，不能标记 done（未完成：{}）",
                            left.join("、")
                        ),
                    ));
                }
                next.status = if next.criteria.iter().any(|c| c.manual)
                    || s == GoalStatus::AwaitingAcceptance
                {
                    GoalStatus::AwaitingAcceptance
                } else {
                    GoalStatus::Done
                };
            }
            GoalStatus::Aborted => next.status = GoalStatus::Aborted,
            GoalStatus::Executing => next.status = GoalStatus::Executing,
            GoalStatus::Paused => next.status = GoalStatus::Paused,
            GoalStatus::Clarify | GoalStatus::Stopping => {
                return Err((
                    "E_ARGS".into(),
                    "执行期不接受 status=clarify（目标已进入执行期；改目标请先 aborted）".into(),
                ));
            }
        }
    }
    Ok(next)
}

/// 验收标准构建（校验条数上限与标题非空；`done` 缺省 false）。
fn build_criteria(ins: Option<&[CriterionIn]>) -> Result<Vec<GoalCriterion>, (String, String)> {
    let Some(ins) = ins else {
        return Ok(Vec::new());
    };
    if ins.len() > MAX_CRITERIA {
        return Err(("E_ARGS".into(), format!("验收标准超过 {MAX_CRITERIA} 项")));
    }
    let mut out = Vec::with_capacity(ins.len());
    for c in ins {
        let t = c.title.trim();
        if t.is_empty() {
            return Err(("E_ARGS".into(), "criteria[].title 不能为空".into()));
        }
        out.push(GoalCriterion {
            title: t.to_string(),
            done: c.done.unwrap_or(false),
            manual: c.manual.unwrap_or(false),
            verification: c.verification.clone(),
        });
    }
    Ok(out)
}

/// 清洗旧版账本数据；字段不再出现在工具 schema，也不参与权限判定。
fn build_ledger(ins: Option<&LedgerIn>) -> Result<GoalLedger, (String, String)> {
    let Some(l) = ins else {
        return Ok(GoalLedger::default());
    };
    let paths = l.paths.as_deref().map(clean_list).unwrap_or_default();
    Ok(GoalLedger {
        paths,
        programs: l.programs.as_deref().map(clean_list).unwrap_or_default(),
    })
}

/// 去空白 + 去空条目（列表类入参统一清洗）。
fn clean_list(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[async_trait::async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &'static str {
        "goal"
    }
    fn description(&self) -> &'static str {
        "登记文档驱动的目标合同(text/criteria/sources/baseline)，执行中维护验收证据与进度。预算由用户在界面设置。criteria.manual=true为人工验收项，不能由模型勾选。evidence引用真实command调用id与criterion零基序号。pending/blocked整体替换当前未解决列表（[]表示已解决）；decisions追加。全部机器项完成并有当前版本独立审查后status=done，有人工项自动进入待验收。{}读取状态。"
    }
    fn schema(&self) -> &'static str {
        r#"{
          "type":"object","additionalProperties":false,"properties":{
            "text":{"type":"string","minLength":1},
            "criteria":{"type":"array","maxItems":100,"items":{"type":"object","additionalProperties":false,"required":["title"],"properties":{"title":{"type":"string"},"done":{"type":"boolean"},"manual":{"type":"boolean"},"verification":{"type":"object","additionalProperties":false,"required":["command"],"properties":{"command":{"type":"string","minLength":1},"cwd":{"type":"string"}}}}}},
            "sources":{"type":"array","items":{"type":"string"},"description":"需求与技术文档路径，执行期锁定"},
            "baseline":{"type":"array","items":{"type":"string"},"description":"开工已有的失败与已知问题，执行期锁定"},
            "status":{"type":"string","enum":["clarify","executing","paused","awaiting_acceptance","done","aborted"]},
            "decisions":{"type":"array","items":{"type":"string"}},
            "pending":{"type":"array","items":{"type":"string"},"description":"整体替换当前未完成工作；清空表示已解决"},
            "blocked":{"type":"array","items":{"type":"string"},"description":"整体替换当前阻塞；清空表示已解决"},
            "evidence":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["criterion","call_id","summary"],"properties":{"criterion":{"type":"integer","minimum":0},"call_id":{"type":"string"},"summary":{"type":"string"}}}}
          }
        }"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Meta
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        let current = ctx.rt.goal_snapshot();
        if !args.has_write() {
            let rendered = match &current {
                Some(s) => goal::render_goal_summary(s),
                None => NOT_REGISTERED_SUMMARY.to_string(),
            };
            return ToolOutcome::ok(json!({ "goal": current, "rendered": rendered }));
        }
        let stamp = if args
            .status
            .is_some_and(|s| matches!(s, GoalStatus::Done | GoalStatus::AwaitingAcceptance))
        {
            let Some(state) = &current else {
                return ToolOutcome::err("E_GOAL_NOT_REGISTERED", "目标尚未登记");
            };
            if ctx
                .core
                .subs
                .iter()
                .any(|s| s.root_session_id.as_deref() == Some(ctx.rt.id.as_str()))
            {
                return ToolOutcome::err(
                    "E_GOAL_VERIFY_BUSY",
                    "仍有子代理在运行，请等待所有任务结束后单独验收",
                );
            }
            match goal_delivery::fingerprint(&ctx.rt, state).await {
                Ok(s) => Some(s),
                Err(e) => return ToolOutcome::err("E_GOAL_EVIDENCE", e),
            }
        } else {
            None
        };
        // Reapply under the shared lock: subagent usage and verification records must not be lost.
        let mut guard = ctx.rt.goal.lock().unwrap();
        let next = match apply(guard.as_ref(), &args) {
            Ok(n) => n,
            Err((code, msg)) => return ToolOutcome::err(&code, msg),
        };
        if let Some(stamp) = &stamp {
            if let Some(e) = goal_delivery::completion_error(&next, stamp, false) {
                return ToolOutcome::err("E_GOAL_EVIDENCE", e);
            }
        }
        if let Err(e) = ctx.core.store.save_goal(&ctx.rt.id, &Some(next.clone())) {
            return ToolOutcome::err("E_GOAL_SAVE", e.to_string());
        }
        *guard = Some(next.clone());
        drop(guard);
        goal_delivery::persist(&ctx.core, &ctx.rt);
        ToolOutcome::ok(json!({"goal": next, "rendered": goal::render_goal_summary(&next)}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{AgentCore, EventSink, Frame};
    use crate::core::types::SessionId;
    use std::sync::{Arc, Mutex};

    /// 记录事件的测试 sink：断言 `goal:update` 的键名与载荷（其余事件不关心）。
    #[derive(Default)]
    struct RecSink {
        events: Mutex<Vec<(String, String, Value)>>,
    }

    impl EventSink for RecSink {
        fn channel_frame(&self, _s: &SessionId, _f: &Frame) {}
        fn emit(&self, s: &SessionId, e: &str, p: Value) {
            self.events
                .lock()
                .unwrap()
                .push((s.clone(), e.to_string(), p));
        }
    }

    /// 测试夹具：真实 store（边车落临时目录）+ 记录事件的 sink + 主会话 runtime。
    struct Harness {
        ctx: ToolCtx,
        sink: Arc<RecSink>,
        data_dir: std::path::PathBuf,
        _ws: tempfile::TempDir,
        _dd: tempfile::TempDir,
    }

    fn harness() -> Harness {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let cfg = crate::core::config::ConfigState::default();
        let sink = Arc::new(RecSink::default());
        let store = Arc::new(crate::core::sessions::SessionStore::new(
            dd.path().to_path_buf(),
        ));
        let core = Arc::new(AgentCore::new(
            cfg,
            sink.clone(),
            store,
            reqwest::Client::new(),
            dd.path().to_path_buf(),
        ));
        let rt = core.get_or_create_session(
            "goal-tool-test",
            std::fs::canonicalize(ws.path()).unwrap(),
            None,
            vec![],
            None,
            vec![],
        );
        Harness {
            ctx: ToolCtx {
                core,
                rt,
                batch_id: "b".into(),
                call_index: 0,
                call_key: "b:0".into(),
                cancel: tokio_util::sync::CancellationToken::new(),
            },
            sink,
            data_dir: dd.path().to_path_buf(),
            _ws: ws,
            _dd: dd,
        }
    }

    async fn call(h: &Harness, args: Value) -> ToolOutcome {
        GoalTool.run(&h.ctx, args).await
    }

    fn code(out: &ToolOutcome) -> String {
        out.error
            .as_ref()
            .map(|e| e.code.clone())
            .unwrap_or_else(|| "（无错误）".into())
    }

    fn goal_events(h: &Harness) -> Vec<Value> {
        h.sink
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e, _)| e == "goal:update")
            .map(|(_, _, p)| p.clone())
            .collect()
    }

    fn state_of(h: &Harness) -> GoalState {
        h.ctx.rt.goal_snapshot().expect("目标应已登记")
    }

    fn register_args() -> Value {
        json!({
            "text": "把 X 改成 Y",
            "criteria": [{"title": "改完 X"}, {"title": "测试通过"}],
            "ledger": {"paths": ["/work/proj/src"], "programs": ["cargo"]}
        })
    }

    /// 登记后把状态推到执行期（驱动层的切换动作在测试里手工模拟）。
    fn enter_execute(h: &Harness) {
        let mut s = state_of(h);
        s.status = GoalStatus::Executing;
        h.ctx.rt.set_goal(Some(s));
    }

    #[test]
    fn schema_is_strict_object() {
        let s: Value = serde_json::from_str(GoalTool.schema()).unwrap();
        assert_eq!(s["type"], "object");
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(GoalTool.name(), "goal");
        assert_eq!(GoalTool.kind(), ToolKind::Meta);
    }

    #[tokio::test]
    async fn read_before_registration_returns_empty_state() {
        let h = harness();
        let out = call(&h, json!({})).await;
        assert!(out.ok, "{:?}", out.error);
        assert!(out.data["goal"].is_null());
        assert_eq!(out.data["rendered"], json!(NOT_REGISTERED_SUMMARY));
        assert!(goal_events(&h).is_empty(), "读操作不发事件");
    }

    #[tokio::test]
    async fn registration_persists_emits_and_returns_summary() {
        let h = harness();
        let out = call(&h, register_args()).await;
        assert!(out.ok, "{:?}", out.error);
        let s = state_of(&h);
        assert_eq!(s.text, "把 X 改成 Y");
        assert_eq!(s.criteria.len(), 2);
        assert_eq!(s.status, GoalStatus::Clarify, "登记后保持澄清期");
        assert_eq!(s.ledger.programs, vec!["cargo".to_string()]);
        // 事件：键名 + 载荷（session 与 goal）
        let ev = goal_events(&h);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["session"], json!("goal-tool-test"));
        assert_eq!(ev[0]["goal"]["text"], json!("把 X 改成 Y"));
        // 边车落盘并可读回
        let sidecar = h.data_dir.join("sessions/goal-tool-test.goal.json");
        assert!(sidecar.exists(), "边车必须落盘");
        let back = h.ctx.core.store.load_goal("goal-tool-test").unwrap();
        assert_eq!(back, s);
        // 返回值带纯文本汇总
        let rendered = out.data["rendered"].as_str().unwrap();
        assert!(rendered.contains("状态：澄清中"), "{rendered}");
        assert!(rendered.contains("改完 X"), "{rendered}");
    }

    #[tokio::test]
    async fn registration_requires_text() {
        let h = harness();
        let out = call(&h, json!({"criteria": [{"title": "a"}]})).await;
        assert!(!out.ok);
        assert_eq!(code(&out), "E_ARGS");
        assert!(h.ctx.rt.goal_snapshot().is_none(), "失败不得留下状态");
        assert!(goal_events(&h).is_empty(), "失败不发事件");
    }

    #[tokio::test]
    async fn unregistered_rejects_non_registration_writes() {
        let h = harness();
        let out = call(&h, json!({"decisions": ["x"]})).await;
        assert_eq!(code(&out), "E_GOAL_NOT_REGISTERED");
        let out = call(&h, json!({"pending": ["x"]})).await;
        assert_eq!(code(&out), "E_GOAL_NOT_REGISTERED");
        let out = call(&h, json!({"status": "executing"})).await;
        assert_eq!(code(&out), "E_GOAL_NOT_REGISTERED");
        // 只给 status=clarify 而无任何登记内容 → 仍算未登记
        let out = call(&h, json!({"status": "clarify"})).await;
        assert_eq!(code(&out), "E_GOAL_NOT_REGISTERED");
        assert!(h.ctx.rt.goal_snapshot().is_none());
    }

    #[tokio::test]
    async fn clarify_phase_revises_draft() {
        let h = harness();
        assert!(call(&h, register_args()).await.ok);
        let out = call(
            &h,
            json!({
                "text": "把 X 改成 Z",
                "criteria": [{"title": "改完 Z", "done": true}],
                "ledger": {"paths": ["/work/proj/src/z.rs"]}
            }),
        )
        .await;
        assert!(out.ok, "{:?}", out.error);
        let s = state_of(&h);
        assert_eq!(s.text, "把 X 改成 Z");
        assert_eq!(s.criteria.len(), 1);
        assert!(s.criteria[0].done, "澄清期允许带 done 的整体替换");
        assert_eq!(s.ledger.paths, vec!["/work/proj/src/z.rs".to_string()]);
        assert!(s.ledger.programs.is_empty(), "整体替换：未给的程序清空");
        assert_eq!(s.status, GoalStatus::Clarify);
        // 澄清期不接受 decisions/pending 与非 clarify 的 status
        assert_eq!(
            code(&call(&h, json!({"decisions": ["x"]})).await),
            "E_GOAL_CONTRACT_LOCKED"
        );
        assert_eq!(
            code(&call(&h, json!({"status": "executing"})).await),
            "E_ARGS"
        );
    }

    #[tokio::test]
    async fn execute_phase_locks_text_ledger_and_titles() {
        let h = harness();
        assert!(call(&h, register_args()).await.ok);
        enter_execute(&h);
        let before = state_of(&h);
        // text 锁定
        let out = call(&h, json!({"text": "换个目标"})).await;
        assert_eq!(code(&out), "E_GOAL_CONTRACT_LOCKED");
        // ledger 锁定
        let out = call(&h, json!({"ledger": {"paths": ["/work/proj/src"]}})).await;
        assert_eq!(code(&out), "E_GOAL_CONTRACT_LOCKED");
        // 改标题锁定
        let out = call(
            &h,
            json!({"criteria": [{"title": "改完 X2"}, {"title": "测试通过"}]}),
        )
        .await;
        assert_eq!(code(&out), "E_GOAL_CONTRACT_LOCKED");
        // 增删条目锁定
        let out = call(&h, json!({"criteria": [{"title": "改完 X"}]})).await;
        assert_eq!(code(&out), "E_GOAL_CONTRACT_LOCKED");
        // 全部失败路径都不得改动状态，也不发事件
        assert_eq!(state_of(&h), before);
        assert!(goal_events(&h).len() == 1, "只有登记那次事件");
    }

    #[tokio::test]
    async fn execute_phase_allows_done_flags_and_appends() {
        let h = harness();
        assert!(call(&h, register_args()).await.ok);
        enter_execute(&h);
        let out = call(
            &h,
            json!({
                "criteria": [{"title": "改完 X", "done": true}, {"title": "测试通过", "done": false}],
                "decisions": ["用 A 方案"],
                "pending": ["补测试"]
            }),
        )
        .await;
        assert!(out.ok, "{:?}", out.error);
        let s = state_of(&h);
        assert!(s.criteria[0].done && !s.criteria[1].done);
        assert_eq!(s.decisions, vec!["用 A 方案".to_string()]);
        assert_eq!(s.pending, vec!["补测试".to_string()]);
        assert_eq!(s.status, GoalStatus::Executing, "只改 done 不切状态");
        // 追加语义（不是整体替换）
        assert!(call(&h, json!({"decisions": ["再记一条"]})).await.ok);
        assert_eq!(state_of(&h).decisions.len(), 2);
    }

    #[tokio::test]
    async fn done_requires_all_criteria() {
        let h = harness();
        assert!(call(&h, register_args()).await.ok);
        enter_execute(&h);
        let out = call(&h, json!({"status": "done"})).await;
        assert_eq!(code(&out), "E_GOAL_CRITERIA_PENDING");
        assert!(out.error.unwrap().message.contains("改完 X"));
        assert_eq!(state_of(&h).status, GoalStatus::Executing, "拒绝不得切状态");
        // 同一次调用里勾完最后一条 + status=done → 放行（按本次调用之后的标准集合判定）
        let out = call(
            &h,
            json!({
                "criteria": [{"title": "改完 X", "done": true}, {"title": "测试通过", "done": true}],
                "status": "done"
            }),
        )
        .await;
        assert_eq!(code(&out), "E_GOAL_EVIDENCE", "仅勾选不能证明完成");
        assert_eq!(state_of(&h).status, GoalStatus::Executing);
    }

    #[tokio::test]
    async fn abort_is_allowed_in_execute_and_status_clarify_is_not() {
        let h = harness();
        assert!(call(&h, register_args()).await.ok);
        enter_execute(&h);
        assert_eq!(
            code(&call(&h, json!({"status": "clarify"})).await),
            "E_ARGS"
        );
        let out = call(&h, json!({"status": "aborted"})).await;
        assert!(out.ok, "{:?}", out.error);
        assert_eq!(state_of(&h).status, GoalStatus::Aborted);
        // 中止后回到澄清期语义：可重新登记新目标，进度类字段清零
        let mut s = state_of(&h);
        s.rounds = 4;
        s.stall_streak = 2;
        s.decisions.push("旧决策".into());
        h.ctx.rt.set_goal(Some(s));
        let out = call(&h, json!({"text": "新目标"})).await;
        assert!(out.ok, "{:?}", out.error);
        let s = state_of(&h);
        assert_eq!(s.status, GoalStatus::Clarify);
        assert_eq!(s.text, "新目标");
        assert_eq!((s.rounds, s.stall_streak), (0, 0));
        assert!(s.decisions.is_empty(), "开新目标清掉上一目标的决策");
    }

    #[tokio::test]
    async fn empty_criteria_cannot_be_completed() {
        let h = harness();
        assert!(call(&h, json!({"text": "只改一行"})).await.ok);
        enter_execute(&h);
        let out = call(&h, json!({"status": "done"})).await;
        assert_eq!(code(&out), "E_GOAL_CRITERIA_PENDING");
        assert_eq!(state_of(&h).status, GoalStatus::Executing);
    }

    #[tokio::test]
    async fn tool_backed_evidence_and_human_acceptance_form_a_complete_lifecycle() {
        let h = harness();
        assert!(call(&h,json!({"text":"完整交付", "criteria":[
            {"title":"构建测试通过","verification":{"command":"test"}}, {"title":"人工操作界面", "manual":true}
        ]})).await.ok);
        h.ctx
            .rt
            .mutate_goal(|g| goal_delivery::prepare_contract(g, &h.ctx.rt).unwrap());
        enter_execute(&h);
        let stamp =
            goal_delivery::verification_start(&h.ctx, "command", &json!({"command":"test"})).await;
        goal_delivery::record_tool_result(
            &h.ctx,
            "test-1",
            "command",
            &json!({"command":"test"}),
            &ToolOutcome::ok(json!({"exit_code":0})),
            stamp,
        )
        .await;
        let stamp =
            goal_delivery::verification_start(&h.ctx, "subagent", &json!({"role":"reviewer"}))
                .await;
        goal_delivery::record_tool_result(
            &h.ctx,
            "review-1",
            "subagent",
            &json!({"role":"reviewer"}),
            &ToolOutcome::ok(json!({"ended":"report","report":"已核对原文\n[GOAL_REVIEW_PASS]"})),
            stamp,
        )
        .await;
        let out = call(&h,json!({"criteria":[{"title":"构建测试通过","done":true},{"title":"人工操作界面","manual":true}],
            "evidence":[{"criterion":0,"call_id":"test-1","summary":"构建与测试通过"}],"status":"done"})).await;
        assert!(out.ok, "{:?}", out.error);
        assert_eq!(state_of(&h).status, GoalStatus::AwaitingAcceptance);
        assert!(!state_of(&h).criteria[1].done);
        let done = h.ctx.core.accept_goal(&h.ctx.rt, true, None).await.unwrap();
        assert_eq!(done.status, GoalStatus::Done);
        assert!(done.criteria.iter().all(|c| c.done));
        assert_eq!(
            h.ctx.core.store.load_goal(&h.ctx.rt.id).unwrap().status,
            GoalStatus::Done
        );
    }

    #[tokio::test]
    async fn changes_during_verification_and_failed_reviews_cannot_supply_evidence() {
        let h = harness();
        assert!(call(&h,json!({"text":"test", "criteria":[{"title":"test","verification":{"command":"test"}}]})).await.ok);
        h.ctx
            .rt
            .mutate_goal(|g| goal_delivery::prepare_contract(g, &h.ctx.rt).unwrap());
        enter_execute(&h);
        let stamp =
            goal_delivery::verification_start(&h.ctx, "command", &json!({"command":"test"})).await;
        std::fs::write(h.ctx.rt.workspace.join("changed.rs"), "changed during test").unwrap();
        goal_delivery::record_tool_result(
            &h.ctx,
            "stale",
            "command",
            &json!({"command":"test"}),
            &ToolOutcome::ok(json!({"exit_code":0})),
            stamp,
        )
        .await;
        assert!(!state_of(&h).delivery.verifications[0].passed);
        let stamp =
            goal_delivery::verification_start(&h.ctx, "subagent", &json!({"role":"reviewer"}))
                .await;
        goal_delivery::record_tool_result(
            &h.ctx,
            "review",
            "subagent",
            &json!({"role":"reviewer"}),
            &ToolOutcome::ok(json!({"ended":"report","report":"[GOAL_REVIEW_FAIL] 缺少核心功能"})),
            stamp,
        )
        .await;
        assert!(!state_of(&h).delivery.verifications[1].passed);
        let out = call(
            &h,
            json!({"evidence":[{"criterion":0,"call_id":"stale","summary":"通过"}]}),
        )
        .await;
        assert_eq!(code(&out), "E_GOAL_EVIDENCE");
    }

    #[tokio::test]
    async fn blocked_items_can_be_resolved_and_manual_flags_cannot_be_lowered() {
        let h = harness();
        assert!(
            call(
                &h,
                json!({"text":"交付","criteria":[{"title":"真实账号验收","manual":true}]})
            )
            .await
            .ok
        );
        enter_execute(&h);
        assert!(
            call(&h, json!({"blocked":["缺账号"],"pending":["待接入"]}))
                .await
                .ok
        );
        assert!(call(&h, json!({"blocked":[],"pending":[]})).await.ok);
        assert!(state_of(&h).blocked.is_empty());
        let out = call(
            &h,
            json!({"criteria":[{"title":"真实账号验收","manual":false,"done":true}]}),
        )
        .await;
        assert_eq!(code(&out), "E_GOAL_CONTRACT_LOCKED");
        let out = call(
            &h,
            json!({"criteria":[{"title":"真实账号验收","done":true}]}),
        )
        .await;
        assert_eq!(code(&out), "E_GOAL_MANUAL_ACCEPTANCE");
    }

    // ---------- 纯函数：合同锁定（不经工具与 runtime）----------

    fn pure_state(status: GoalStatus) -> GoalState {
        GoalState {
            text: "目标".into(),
            criteria: vec![GoalCriterion {
                title: "a".into(),
                done: false,
                manual: false,
                verification: None,
            }],
            ledger: GoalLedger::default(),
            status,
            decisions: vec![],
            pending: vec![],
            blocked: vec![],
            rounds: 0,
            stall_streak: 0,
            ledger_denials: 0,
            delivery: Default::default(),
        }
    }

    fn args_of(v: Value) -> Args {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn pure_contract_lock_matrix() {
        let cur = pure_state(GoalStatus::Executing);
        // 锁定字段
        for v in [
            json!({"text": "改"}),
            json!({"ledger": {"paths": ["/w"]}}),
            json!({"criteria": [{"title": "b"}]}),
            json!({"criteria": [{"title": "a"}, {"title": "b"}]}),
        ] {
            let e = apply(Some(&cur), &args_of(v.clone())).unwrap_err();
            assert_eq!(e.0, "E_GOAL_CONTRACT_LOCKED", "{v}");
        }
        // 允许字段
        let next = apply(
            Some(&cur),
            &args_of(json!({"criteria": [{"title": "a", "done": true}]})),
        )
        .unwrap();
        assert!(next.criteria[0].done);
        assert_eq!(next.text, "目标", "允许的调用不得动 text");
        // 澄清期可改标题
        let next = apply(
            Some(&pure_state(GoalStatus::Clarify)),
            &args_of(json!({"criteria": [{"title": "b", "done": true}]})),
        )
        .unwrap();
        assert_eq!(next.criteria[0].title, "b");
    }

    #[test]
    fn pure_registration_and_not_registered_rules() {
        // 未登记：登记字段放行
        let next = apply(
            None,
            &args_of(json!({"text": " t ", "criteria": [{"title": " a "}]})),
        )
        .unwrap();
        assert_eq!(next.text, "t", "登记时 trim");
        assert_eq!(next.criteria[0].title, "a");
        assert_eq!(next.status, GoalStatus::Clarify);
        // 未登记 + 追加/切档 → E_GOAL_NOT_REGISTERED
        for v in [
            json!({"decisions": ["x"]}),
            json!({"pending": ["x"]}),
            json!({"status": "done"}),
        ] {
            let e = apply(None, &args_of(v.clone())).unwrap_err();
            assert_eq!(e.0, "E_GOAL_NOT_REGISTERED", "{v}");
        }
        // 空标题 / 超限
        assert_eq!(
            apply(
                None,
                &args_of(json!({"text": "t", "criteria": [{"title": "  "}]}))
            )
            .unwrap_err()
            .0,
            "E_ARGS"
        );
        let many: Vec<Value> = (0..=MAX_CRITERIA)
            .map(|i| json!({"title": format!("c{i}")}))
            .collect();
        assert_eq!(
            apply(None, &args_of(json!({"text": "t", "criteria": many})))
                .unwrap_err()
                .0,
            "E_ARGS"
        );
    }

    #[test]
    fn read_only_args_are_detected() {
        assert!(!args_of(json!({})).has_write());
        assert!(args_of(json!({"status": "clarify"})).has_write());
        assert!(args_of(json!({"text": "t"})).has_write());
    }
}
