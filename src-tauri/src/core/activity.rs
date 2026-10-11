//! 进程内运行观察摘要。只读编排事实，不参与应答、取消或历史；不依赖 UI 投递成功。
//! 更新锁仅保护有界摘要，不跨 await、IO 或事件转发；读取 runtime 真值先释放摘要锁。

use crate::core::agent::{EventSink, Frame, RunSnapshot, SessionRuntime, lock_ok};
use crate::core::types::SessionId;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

const RECENT_LIMIT: usize = 50;
const SUMMARY_CHARS: usize = 500;
const REPORT_CHARS: usize = 2000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn summary(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn field(payload: &Value, name: &str, limit: usize) -> String {
    summary(payload[name].as_str().unwrap_or_default(), limit)
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivitySnapshot {
    pub revision: u64,
    pub sampled_at_ms: u64,
    pub sessions: Vec<ActivitySession>,
    pub recent: Vec<ActivityRecent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivitySession {
    pub session_id: String,
    pub run_id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub started_at_ms: u64,
    pub activity: String,
    pub waiting: Vec<ActivityWaiting>,
    pub subs: Vec<ActivitySub>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityWaiting {
    pub ask_id: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivitySub {
    pub sub_id: String,
    pub role: String,
    pub name: Option<String>,
    pub task: String,
    pub step: u64,
    pub max_steps: u64,
    pub detail: String,
    pub last_tool: Option<String>,
    pub status: String,
    pub ended: Option<String>,
    pub report: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityRecent {
    pub id: String,
    pub kind: String,
    pub session_id: String,
    pub run_id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub status: String,
    pub ended: Option<String>,
    pub finished_at_ms: u64,
    pub sub: Option<ActivitySub>,
}

struct ObservedAsk {
    summary: ActivityWaiting,
    /// 保留原 ask:opened，仅等待期间按需回填；不收集工具参数或流式内容。
    payload: Value,
}

struct ObservedRun {
    view: ActivitySession,
    runtime: Weak<SessionRuntime>,
    sub_runtimes: HashMap<String, Weak<SessionRuntime>>,
    asks: HashMap<String, ObservedAsk>,
    /// 按完成顺序裁剪，不能丢掉刚完成的长任务。
    finished_sub_ids: VecDeque<String>,
}

#[derive(Default)]
struct State {
    revision: u64,
    runs: HashMap<String, ObservedRun>,
    recent: VecDeque<ActivityRecent>,
}

impl State {
    fn push_recent(&mut self, item: ActivityRecent) {
        // 同一终态只发布一次；run/sub 身份不可由标题或事件到达时刻替代。
        if self.recent.iter().any(|old| old.id == item.id) {
            return;
        }
        self.recent.push_front(item);
        self.recent.truncate(RECENT_LIMIT);
    }

    fn finish(&mut self, session_id: &str, run_id: &str, status: &str) {
        if !self
            .runs
            .get(session_id)
            .is_some_and(|r| r.view.run_id == run_id)
        {
            return;
        }
        let run = self.runs.remove(session_id).expect("checked run");
        for mut sub in run
            .view
            .subs
            .iter()
            .filter(|s| s.status == "running")
            .cloned()
        {
            if let Some(rt) = run.sub_runtimes.get(&sub.sub_id).and_then(Weak::upgrade) {
                // 原子计数读取无需 runtime 锁；快速取消也不依赖 800ms 轮询采样。
                sub.step = sub
                    .step
                    .max(rt.step_count.load(std::sync::atomic::Ordering::SeqCst) as u64);
            }
            sub.status = if status == "cancelled" {
                "cancelled"
            } else {
                "error"
            }
            .into();
            sub.detail = if status == "cancelled" {
                "随主会话停止"
            } else {
                "主会话已收尾"
            }
            .into();
            self.push_recent(recent_sub(
                &run.view,
                sub,
                if status == "cancelled" {
                    status
                } else {
                    "error"
                },
            ));
        }
        self.push_recent(ActivityRecent {
            id: format!("session:{session_id}:{run_id}"),
            kind: "session".into(),
            session_id: session_id.into(),
            run_id: run_id.into(),
            title: run.view.title,
            project_id: run.view.project_id,
            status: status.into(),
            ended: None,
            finished_at_ms: now_ms(),
            sub: None,
        });
        self.revision += 1;
    }
}

fn recent_sub(run: &ActivitySession, sub: ActivitySub, status: &str) -> ActivityRecent {
    ActivityRecent {
        id: format!("subagent:{}:{}:{}", run.session_id, run.run_id, sub.sub_id),
        kind: "subagent".into(),
        session_id: run.session_id.clone(),
        run_id: run.run_id.clone(),
        title: sub.name.clone().unwrap_or_else(|| sub.role.clone()),
        project_id: run.project_id.clone(),
        status: status.into(),
        ended: sub.ended.clone(),
        finished_at_ms: now_ms(),
        sub: Some(sub),
    }
}

#[derive(Default)]
pub struct ActivityState(Mutex<State>);

impl ActivityState {
    /// run 成功抢占并完成启动校验后绑定；任务 runtime 不进入主会话总览。
    pub fn begin(&self, rt: &Arc<SessionRuntime>, run_id: &str) {
        if !rt.is_main_session {
            return;
        }
        let title = summary(&lock_ok(&rt.title), SUMMARY_CHARS);
        let mut state = lock_ok(&self.0);
        state.runs.insert(
            rt.id.clone(),
            ObservedRun {
                view: ActivitySession {
                    session_id: rt.id.clone(),
                    run_id: run_id.into(),
                    title,
                    project_id: rt.project_id.clone(),
                    started_at_ms: now_ms(),
                    activity: "准备运行".into(),
                    waiting: Vec::new(),
                    subs: Vec::new(),
                },
                runtime: Arc::downgrade(rt),
                sub_runtimes: HashMap::new(),
                asks: HashMap::new(),
                finished_sub_ids: VecDeque::new(),
            },
        );
        state.revision += 1;
    }

    pub fn finish(&self, session_id: &str, run_id: &str, status: &str) {
        lock_ok(&self.0).finish(session_id, run_id, status);
    }

    pub fn register_sub(
        &self,
        parent: &SessionRuntime,
        sub: &Arc<SessionRuntime>,
        run_id: Option<&str>,
    ) {
        let mut state = lock_ok(&self.0);
        if let Some(run) = state.runs.get_mut(&parent.id)
            && Some(run.view.run_id.as_str()) == run_id
            && run
                .view
                .subs
                .iter()
                .any(|s| s.sub_id == sub.id && s.status == "running")
        {
            run.sub_runtimes.insert(sub.id.clone(), Arc::downgrade(sub));
        }
    }

    /// 在转发前读取现有事件的摘要字段；不解析/保存流式文本、工具入参或整个出参。
    fn observe(&self, session: &str, event: &str, payload: &Value) {
        let mut state = lock_ok(&self.0);
        if event == "session:title" {
            let title = field(payload, "title", SUMMARY_CHARS);
            if let Some(run) = state.runs.get_mut(session) {
                run.view.title = title.clone();
            }
            for item in state
                .recent
                .iter_mut()
                .filter(|r| r.session_id == session && r.kind == "session")
            {
                item.title = title.clone();
            }
            state.revision += 1;
            return;
        }
        let Some(run) = state.runs.get_mut(session) else {
            return;
        };
        if let Some(id) = payload["run_id"].as_str()
            && id != run.view.run_id
        {
            return;
        }
        let mut terminal_sub = None;
        match event {
            "run:start" => run.view.activity = "运行中".into(),
            "tool:start" => run.view.activity = format!("执行工具 {}", field(payload, "tool", 100)),
            "tool:result" | "tool:error" => run.view.activity = "运行中".into(),
            "run:retry" => run.view.activity = "正在重试".into(),
            "ask:opened" => {
                // 子代理审批只能归到本轮实际登记的子 runtime。
                if let Some(sub) = payload["sub_id"].as_str()
                    && !run.sub_runtimes.contains_key(sub)
                {
                    return;
                }
                let Some(id) = payload["ask_id"].as_str() else {
                    return;
                };
                let kind = if payload["kind"] == "approval" || payload["approval"] == true {
                    "approval"
                } else {
                    "ask"
                };
                let first_question = &payload["questions"][0];
                let title = payload["title"]
                    .as_str()
                    .or_else(|| first_question["header"].as_str())
                    .unwrap_or(if kind == "approval" {
                        "等待审批"
                    } else {
                        "等待回答"
                    });
                let detail = payload["detail"]
                    .as_str()
                    .or_else(|| first_question["question"].as_str())
                    .unwrap_or_default();
                run.asks.insert(
                    id.into(),
                    ObservedAsk {
                        summary: ActivityWaiting {
                            ask_id: id.into(),
                            kind: kind.into(),
                            title: summary(title, SUMMARY_CHARS),
                            detail: summary(detail, SUMMARY_CHARS),
                        },
                        payload: payload.clone(),
                    },
                );
            }
            "ask:closed" => {
                run.asks
                    .remove(payload["ask_id"].as_str().unwrap_or_default());
            }
            "sub:spawn" => {
                let Some(id) = payload["sub_id"].as_str() else {
                    return;
                };
                if run.view.subs.iter().any(|s| s.sub_id == id) {
                    return;
                }
                run.view.subs.push(ActivitySub {
                    sub_id: id.into(),
                    role: field(payload, "role", 100),
                    name: payload["name"].as_str().map(|s| summary(s, 100)),
                    task: field(payload, "task", SUMMARY_CHARS),
                    step: 0,
                    max_steps: payload["max_steps"].as_u64().unwrap_or_default(),
                    detail: "准备运行".into(),
                    last_tool: None,
                    status: "running".into(),
                    ended: None,
                    report: None,
                });
            }
            "sub:step" | "sub:report" | "sub:done" | "sub:error" => {
                let id = payload["sub_id"].as_str().unwrap_or_default();
                let Some(sub) = run
                    .view
                    .subs
                    .iter_mut()
                    .find(|s| s.sub_id == id && s.status == "running")
                else {
                    return;
                };
                match event {
                    "sub:step" => {
                        sub.step = sub.step.max(payload["step"].as_u64().unwrap_or_default());
                        sub.detail = field(payload, "detail", SUMMARY_CHARS);
                        sub.last_tool = payload["tool"].as_str().map(|s| summary(s, 100));
                    }
                    "sub:report" => sub.report = Some(field(payload, "report", REPORT_CHARS)),
                    _ => {
                        sub.status = if event == "sub:done" {
                            "done"
                        } else if payload["cancelled"] == true {
                            "cancelled"
                        } else {
                            "error"
                        }
                        .into();
                        sub.step = sub
                            .step
                            .max(payload["steps_used"].as_u64().unwrap_or_default());
                        sub.ended = payload["ended"]
                            .as_str()
                            .filter(|e| matches!(*e, "report" | "partial" | "budget" | "no_report"))
                            .map(str::to_owned);
                        if event == "sub:error" {
                            sub.detail = field(payload, "error", SUMMARY_CHARS);
                        }
                        let status = if payload["cancelled"] == true {
                            "cancelled".into()
                        } else {
                            sub.status.clone()
                        };
                        let sub = sub.clone();
                        terminal_sub = Some(recent_sub(&run.view, sub, &status));
                        run.sub_runtimes.remove(id);
                        run.asks
                            .retain(|_, ask| ask.payload["sub_id"].as_str() != Some(id));
                        run.finished_sub_ids.push_back(id.into());
                        if run.finished_sub_ids.len() > RECENT_LIMIT
                            && let Some(expired) = run.finished_sub_ids.pop_front()
                        {
                            run.view.subs.retain(|s| s.sub_id != expired);
                        }
                    }
                }
            }
            _ => return,
        }
        if let Some(item) = terminal_sub {
            state.push_recent(item);
        }
        state.revision += 1;
    }

    /// runtime 与 responder 是真值；先读取它们，再在摘要锁内按 run 身份合并。
    /// 单次快照不是所有 runtime 的线性一致读取，但绝不把旧轮数据拼进新轮。
    fn reconcile(&self) {
        let observations: Vec<_> = {
            let state = lock_ok(&self.0);
            state
                .runs
                .iter()
                .map(|(id, run)| {
                    (
                        id.clone(),
                        run.view.run_id.clone(),
                        run.runtime.clone(),
                        run.sub_runtimes.clone(),
                        run.asks
                            .keys()
                            .cloned()
                            .collect::<std::collections::HashSet<_>>(),
                    )
                })
                .collect()
        };
        for (id, run_id, weak, subs, sampled_asks) in observations {
            let Some(rt) = weak.upgrade() else {
                self.finish(&id, &run_id, "error");
                continue;
            };
            let snapshot = rt.run_snapshot();
            let title = summary(&lock_ok(&rt.title), SUMMARY_CHARS);
            let mut valid = live_asks(&rt);
            for sub in subs.values().filter_map(Weak::upgrade) {
                valid.extend(live_asks(&sub));
            }
            let mut state = lock_ok(&self.0);
            if !state.runs.get(&id).is_some_and(|r| r.view.run_id == run_id) {
                continue;
            }
            if !snapshot.running || snapshot.run_id.as_deref() != Some(run_id.as_str()) {
                let status = terminal_status(&snapshot, &run_id);
                state.finish(&id, &run_id, status);
                continue;
            }
            let run = state.runs.get_mut(&id).expect("checked run");
            let before = run.asks.len();
            // 对账期间新开的询问不属于这次采样，留到下一次验证，防误删原 payload。
            run.asks
                .retain(|key, _| !sampled_asks.contains(key) || valid.contains(key));
            let changed = before != run.asks.len() || run.view.title != title;
            run.view.title = title;
            if changed {
                state.revision += 1;
            }
        }
    }

    pub fn snapshot(&self) -> ActivitySnapshot {
        self.reconcile();
        let state = lock_ok(&self.0);
        let mut sessions: Vec<_> = state
            .runs
            .values()
            .map(|run| {
                let mut view = run.view.clone();
                view.waiting = run.asks.values().map(|ask| ask.summary.clone()).collect();
                view.waiting.sort_by(|a, b| a.ask_id.cmp(&b.ask_id));
                view
            })
            .collect();
        sessions.sort_by(|a, b| {
            a.started_at_ms
                .cmp(&b.started_at_ms)
                .then(a.session_id.cmp(&b.session_id))
        });
        ActivitySnapshot {
            revision: state.revision,
            sampled_at_ms: now_ms(),
            sessions,
            recent: state.recent.iter().cloned().collect(),
        }
    }

    /// 返回原 payload 前再次验证运行身份与实际 waiter（包含子代理审批）。
    pub fn ask(&self, session: &str, run_id: &str, ask_id: &str) -> Option<Value> {
        self.reconcile();
        let (payload, root, owner) = {
            let state = lock_ok(&self.0);
            let run = state
                .runs
                .get(session)
                .filter(|r| r.view.run_id == run_id)?;
            let ask = run.asks.get(ask_id)?;
            let owner = match ask.payload["sub_id"].as_str() {
                Some(id) => run.sub_runtimes.get(id)?.clone(),
                None => run.runtime.clone(),
            };
            (ask.payload.clone(), run.runtime.clone(), owner)
        };
        let root = root.upgrade()?;
        let owner = owner.upgrade()?;
        let snapshot = root.run_snapshot();
        if !snapshot.running || snapshot.run_id.as_deref() != Some(run_id) {
            return None;
        }
        let waiter = lock_ok(&owner.asks);
        if !waiter.get(ask_id).is_some_and(|tx| !tx.is_closed()) {
            return None;
        }
        Some(payload)
    }
}

fn live_asks(rt: &SessionRuntime) -> std::collections::HashSet<String> {
    lock_ok(&rt.asks)
        .iter()
        .filter(|(_, tx)| !tx.is_closed())
        .map(|(id, _)| id.clone())
        .collect()
}

fn terminal_status(snapshot: &RunSnapshot, run_id: &str) -> &'static str {
    snapshot
        .terminal
        .as_ref()
        .filter(|t| t.run_id == run_id)
        .map(|t| match t.kind {
            crate::core::agent::RunTerminalKind::Done => "done",
            crate::core::agent::RunTerminalKind::Error => "error",
            crate::core::agent::RunTerminalKind::Cancelled => "cancelled",
        })
        .unwrap_or("error")
}

/// 所有转发入口保持透明；摘要在 emit 前更新，因此无通道/投递失败不影响观察。
pub struct ActivitySink {
    pub activity: Arc<ActivityState>,
    pub downstream: Arc<dyn EventSink>,
}

impl EventSink for ActivitySink {
    fn channel_frame(&self, session: &SessionId, frame: &Frame) {
        self.downstream.channel_frame(session, frame);
    }
    fn emit(&self, session: &SessionId, event: &str, payload: Value) {
        self.activity.observe(session, event, &payload);
        self.downstream.emit(session, event, payload);
    }
    fn bind_sub_channel(&self, parent: &SessionId, sub: &SessionId) {
        self.downstream.bind_sub_channel(parent, sub);
    }
    fn unbind_sub_channel(&self, sub: &SessionId) {
        self.downstream.unbind_sub_channel(sub);
    }
}
