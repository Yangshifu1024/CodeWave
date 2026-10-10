//! 可等待目标登记表：`wait` 工具条件等待（`until`）的后端数据面
//! （[docs/wait-conditional-wait](../../../docs/wait-conditional-wait.md)）。
//!
//! 背景：任何后台对象（service / 子代理 / 计划任务 …）都可以被 `wait` 的 `until` 等待
//! 「完成」。仓库既有表不足以直接支撑轮询：
//!
//! - 子代理完成即从 `core.subs` 移除（成功 / 失败 / panic 同一条 remove），轮询间隙
//!   反查会分不清「已完成」与「从不存在」；
//! - service 自然退出后**不移除**（仅显式 stop 才删），直接照抄会只增不减；
//! - 计划任务没有 running 中间态、没有 run id，`last_status` 只在一次 run 结束时写。
//!
//! 因此本表是**独立于三张既有表**的登记处：条目自带终态标志，由各对象**收尾路径**
//! 显式置位，`wait` 只读本表快照（轮询间隔 1s）。完成方不主动推送——`EventSink`
//! 是单向出网广播，全仓零 watch/broadcast 通道，推送式不可行。
//!
//! 内存态、不持久化：与三类目标的运行态现状一致（进程重启即全失，`wait` 跨重启无意义）。

use dashmap::DashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// 终态条目的存活时长：超过后由 `lookup` 惰性清扫。
///
/// 取值刻意宽松（默认 1 小时）：清扫只为阻止长跑应用无限增长，而**过短会让模型对同一个
/// id 的二次 `wait` 从「已完成」退化成「不存在」**。反复等同一个后台任务是常见用法。
const ENTRY_TTL: Duration = Duration::from_secs(3600);

/// 目标类别。用于 `observed.kind` 回显与诊断，**不用于按 id 前缀分派**
/// （id 前缀并不统一：service 是 `svc_`、子代理是 `sub_`、计划任务无前缀）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// 后台服务进程（`service` 工具）
    Service,
    /// 子代理（`subagent` 工具）
    Subagent,
    /// 计划任务（`scheduled_task` 工具）
    ScheduledTask,
}

impl TargetKind {
    /// 稳定字符串形态，进 `observed.kind` 与工具出参。
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetKind::Service => "service",
            TargetKind::Subagent => "subagent",
            TargetKind::ScheduledTask => "scheduled_task",
        }
    }
}

/// 目标终态。`Running` 之外都是终态（不可逆）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// 进行中
    Running,
    /// 成功终态（正常退出 / 返回成功）
    Succeeded,
    /// 失败终态（异常退出 / 返回错误 / 取消）
    Failed,
}

impl TargetState {
    /// 稳定字符串形态，进 `observed.status` 与工具出参。
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetState::Running => "running",
            TargetState::Succeeded => "succeeded",
            TargetState::Failed => "failed",
        }
    }

    /// 是否已达终态（`done` 判据的判定基础）。
    pub fn is_terminal(&self) -> bool {
        !matches!(self, TargetState::Running)
    }
}

/// 一个可等待目标。
///
/// 字段一律私有 + `&self` 方法（照抄 `ServiceTable` 范式）：读取方拿不到内部锁，
/// 跨分片锁的副作用也不会发生。
#[derive(Debug)]
pub struct WaitTarget {
    id: String,
    kind: TargetKind,
    /// 终态：由各对象收尾路径经 [`WaitTargets::finish`] 显式置位，**只前进不后退**。
    state: Mutex<TargetState>,
    /// 最近进展摘要：service = 环形日志尾，子代理 = report，任务 = last_summary。
    /// 既是 `match.text` 的判据源，也是 `observed.text` 的证据源。
    text: Mutex<Option<String>>,
    /// 登记时的状态基线。计划任务专用：开新一轮 run 时被刷新为「本轮开始前的
    /// `last_status`」，同时 `observed_status` 清空，等本轮真的写回状态。
    baseline: Mutex<Option<String>>,
    /// 本轮观测到的状态值（当前 `last_status`）。与 `baseline` **同类型**、只做
    /// 「变没变」的比较——拿 `last_summary`（自由文本）去比 `last_status` 是两种不同
    /// 类型的比较，结果恒真，会让基线守卫退化成死代码。
    observed_status: Mutex<Option<String>>,
    /// 进入终态的时刻，用于惰性淘汰。
    ended_at: Mutex<Option<SystemTime>>,
}

impl WaitTarget {
    /// 构造进行中的目标条目。
    fn new(id: String, kind: TargetKind, baseline: Option<String>) -> Arc<Self> {
        Arc::new(WaitTarget {
            id,
            kind,
            state: Mutex::new(TargetState::Running),
            text: Mutex::new(None),
            baseline: Mutex::new(baseline),
            observed_status: Mutex::new(None),
            ended_at: Mutex::new(None),
        })
    }

    /// 目标 id。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 目标类别。
    pub fn kind(&self) -> TargetKind {
        self.kind
    }

    /// 当前状态快照（锁内取值后立即释放，调用方不持锁）。
    pub fn state(&self) -> TargetState {
        *self.state.lock().unwrap()
    }

    /// 最近进展摘要快照（`match.text` 判据与 `observed.text` 都读它）。
    pub fn text(&self) -> Option<String> {
        self.text.lock().unwrap().clone()
    }

    /// 登记时的状态基线（计划任务专用）。
    pub fn baseline(&self) -> Option<String> {
        self.baseline.lock().unwrap().clone()
    }

    /// 进入终态的时刻。
    pub fn ended_at(&self) -> Option<SystemTime> {
        *self.ended_at.lock().unwrap()
    }

    /// 是否已达成「本次运行已完成」。
    ///
    /// 计划任务的 `last_status` 在 run 期间是**上一次的旧值**，没有 running 中间态，
    /// 所以光看终态不够：上一轮跑完的终态会在下一轮开跑时依旧留在条目里。仅靠终态
    /// 判定会让**二次运行立即误报已完成**（`every:30 m` 级任务必中）。`register`
    /// 开新一轮时把 `observed_status` 清成 `None`（`Running` 本就没有状态值），
    /// 必须等本轮真的写回状态才判完成。
    ///
    /// `observed_status` 缺失时**保守判为未完成**：宁可多等一轮也不能提前收工。
    pub fn is_completed(&self) -> bool {
        let state = self.state();
        if !state.is_terminal() {
            return false;
        }
        match self.baseline() {
            // 无基线（service / 子代理）：终态即完成
            None => true,
            // 有基线（计划任务）：本轮状态必须与登记时的基线不同
            Some(base) => self.observed_status().is_some_and(|cur| cur != base),
        }
    }

    /// 本轮观测到的状态值（计划任务 = 当前 `last_status`；其余恒为 None）。
    fn observed_status(&self) -> Option<String> {
        self.observed_status.lock().unwrap().clone()
    }

    /// 开新一轮（仅计划任务）：重置回进行中、刷新基线、清空本轮观测值与终态时刻。
    ///
    /// 上一轮遗留的终态与 `observed_status` 必须一起清——只清终态的话，
    /// `observed_status` 仍是上一轮的 `last_status`，`is_completed` 会拿它与新基线比，
    /// 在两次运行结果相同时（都是 `ok`）漏判完成。
    fn begin_round(&self, baseline: Option<String>) {
        *self.state.lock().unwrap() = TargetState::Running;
        *self.text.lock().unwrap() = None;
        *self.observed_status.lock().unwrap() = None;
        *self.ended_at.lock().unwrap() = None;
        *self.baseline.lock().unwrap() = baseline;
    }

    /// 供 `wait` 的 `match.text` 判据使用的观察面（状态 + 摘要）。
    pub fn observed(&self) -> TargetObservation {
        TargetObservation {
            kind: self.kind,
            status: self.state().as_str().to_string(),
            text: self.text(),
        }
    }
}

/// `wait` 出参里的 `observed` 面：等待结束时模型看到的最后状态。
#[derive(Debug, Clone)]
pub struct TargetObservation {
    /// 目标类别。
    pub kind: TargetKind,
    /// 状态字符串（running / succeeded / failed）。
    pub status: String,
    /// 最近进展摘要（可能为 None，也可能被调用方截断）。
    pub text: Option<String>,
}

/// 可等待目标登记表：`id` → 目标条目。
///
/// 照抄 `ServiceTable`（`tools/service.rs`）的范式：`DashMap` + 私有字段 + `&self`
/// 方法，`get` 在方法内 clone 出 `Arc` 让调用方脱离分片锁。默认构造（`Default`），
/// 由 `AgentCore::new` 一次性建好——全仓无 `OnceLock` / `lazy_static` 先例。
#[derive(Default)]
pub struct WaitTargets {
    targets: DashMap<String, Arc<WaitTarget>>,
}

impl WaitTargets {
    /// 登记一个可等待目标（`baseline` 仅计划任务需要）。返回 `Arc` 供调用方后续置终态。
    ///
    /// 幂等分两种语义，由 `baseline` 是否为 `Some` 区分：
    ///
    /// - **有基线（计划任务）= 开新一轮**：`every:30 m` 的任务每轮都会调到这里，
    ///   上一轮遗留的终态必须重置回 `Running`、基线刷新为本轮开始前的 `last_status`、
    ///   `observed_status` 清空。不重置的话，二次运行会被上一轮的终态直接判为已完成。
    /// - **无基线（service / 子代理）= 只登记一次**：重复调用返回已有条目，
    ///   **不覆盖**（覆盖会丢掉已记录的终态，让 wait 反而等不到结束）。
    pub fn register(
        &self,
        id: impl Into<String>,
        kind: TargetKind,
        baseline: Option<String>,
    ) -> Arc<WaitTarget> {
        let id = id.into();
        let restart = baseline.is_some();
        if let Some(existing) = self.targets.get(&id) {
            let arc = existing.value().clone();
            drop(existing); // 先释放分片读锁再返回值
            if restart {
                arc.begin_round(baseline);
            }
            return arc;
        }
        let entry = WaitTarget::new(id.clone(), kind, baseline);
        self.targets.insert(id, entry.clone());
        entry
    }

    /// 置终态。由各对象的**收尾路径**调用（必须早于该对象从其既有表中被移除，
    /// 否则 `wait` 轮询会错过事件）。终态只前进不后退：已是终态时不再改写。
    ///
    /// `status` 仅计划任务传（当前 `last_status`），供 `is_completed` 与基线比对。
    pub fn finish(
        &self,
        id: &str,
        state: TargetState,
        text: Option<String>,
        status: Option<String>,
    ) {
        let Some(entry) = self.targets.get(id) else {
            return;
        };
        let arc = entry.value().clone();
        drop(entry);
        if let Some(t) = text {
            *arc.text.lock().unwrap() = Some(t);
        }
        if let Some(s) = status {
            *arc.observed_status.lock().unwrap() = Some(s);
        }
        let mut slot = arc.state.lock().unwrap();
        if slot.is_terminal() {
            return; // 终态不可回退
        }
        *slot = state;
        *arc.ended_at.lock().unwrap() = Some(SystemTime::now());
    }

    /// 只更新最近进展摘要，**不动状态**。
    ///
    /// service 的活日志就靠它：`finish` 只在进程退出时才调，而「等 dev server 日志出现
    /// ready」要在**进程还活着**时就命中——没有这个方法，`match.text` 在服务运行期间
    /// 永远不命中，且 `observed.text` 也是空，模型超时时拿不到任何证据。
    /// 由 service 已有的 1s ticker 调用（与日志推送同一个口径）。
    pub fn update_text(&self, id: &str, text: impl Into<String>) {
        let Some(entry) = self.targets.get(id) else {
            return;
        };
        let arc = entry.value().clone();
        drop(entry);
        *arc.text.lock().unwrap() = Some(text.into());
    }

    /// 按 id 取目标。不存在返回 None（调用方据此报 `E_NOT_FOUND`）。
    ///
    /// 顺带惰性清扫：终态且超过 TTL 的条目在此被回收——**必须在这里做而不能靠调用方**，
    /// 因为 service 自然退出、计划任务运行、子代理收尾三条路径都不 remove 本表条目
    /// （它们各自的对象生命周期不同，靠表自己淘汰才不会只增不减）。
    pub fn lookup(&self, id: &str) -> Option<Arc<WaitTarget>> {
        self.sweep_expired();
        self.targets.get(id).map(|e| e.value().clone())
    }

    /// 移除目标。由各对象既有回收路径调用（service stop / 任务删除）。
    pub fn remove(&self, id: &str) -> Option<Arc<WaitTarget>> {
        self.targets.remove(id).map(|(_, v)| v)
    }

    /// 全部目标的 id 列表（按字典序，保证可复现）。
    ///
    /// **不做清扫**：调用方只用它列举可等待 id 拼错误提示，清扫会引入不可预期的突变。
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.targets.iter().map(|e| e.key().clone()).collect();
        ids.sort();
        ids
    }

    /// 惰性清扫超 TTL 的终态条目（进行中的永不淘汰）。
    ///
    /// 淘汰终态而非一律淘汰：条目一旦被清掉，模型对同一个 id 的二次 `wait` 会从
    /// 「已完成」退化成 `E_NOT_FOUND`，把「跑完了」与「记错 id」混为一谈。TTL 取得足够宽
    /// （默认 1 小时），只为阻止长时间运行的应用无限增长。
    fn sweep_expired(&self) {
        let now = SystemTime::now();
        let stale: Vec<String> = self
            .targets
            .iter()
            .filter(|e| match (e.value().state(), e.value().ended_at()) {
                // 进行中：永不淘汰（否则等一个正在跑的任务会被凭空判成「不存在」）
                (TargetState::Running, _) => false,
                // 终态但没有时刻（理论上不可达）：保守保留
                (_, None) => false,
                (_, Some(at)) => now
                    .duration_since(at)
                    .map(|d| d >= ENTRY_TTL)
                    .unwrap_or(false),
            })
            .map(|e| e.key().clone())
            .collect();
        for id in stale {
            self.targets.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_lookup_returns_running_target() {
        let t = WaitTargets::default();
        let e = t.register("svc_a", TargetKind::Service, None);
        assert_eq!(e.id(), "svc_a");
        assert_eq!(e.kind(), TargetKind::Service);
        assert_eq!(e.state(), TargetState::Running);
        assert!(!e.is_completed());
        let got = t.lookup("svc_a").expect("已登记目标应可查");
        assert_eq!(got.id(), "svc_a");
        assert!(t.lookup("svc_missing").is_none());
    }

    #[test]
    fn finish_marks_completed_and_keeps_text() {
        let t = WaitTargets::default();
        t.register("svc_a", TargetKind::Service, None);
        t.finish(
            "svc_a",
            TargetState::Succeeded,
            Some("ready in 3s".into()),
            None,
        );
        let e = t.lookup("svc_a").unwrap();
        assert_eq!(e.state(), TargetState::Succeeded);
        assert!(e.is_completed());
        assert_eq!(e.text().as_deref(), Some("ready in 3s"));
        assert!(e.ended_at().is_some(), "终态必须记录时刻供惰性淘汰");
    }

    #[test]
    fn terminal_state_never_regresses() {
        let t = WaitTargets::default();
        t.register("sub_a", TargetKind::Subagent, None);
        t.finish("sub_a", TargetState::Failed, Some("boom".into()), None);
        // 后到的成功置位不得覆盖已记录的失败终态
        t.finish("sub_a", TargetState::Succeeded, None, None);
        let e = t.lookup("sub_a").unwrap();
        assert_eq!(e.state(), TargetState::Failed);
        // text 仍可更新（摘要不是判据）
        t.finish("sub_a", TargetState::Failed, Some("tail".into()), None);
        assert_eq!(t.lookup("sub_a").unwrap().text().as_deref(), Some("tail"));
    }

    /// 🔴 回归（审查发现）：`update_text` 必须能在**进程还活着**时把日志尾写进登记表。
    /// 没有它，`match.text` 在 service 运行期间永远不命中，且 `observed.text` 也是空——
    /// 「等 dev server 日志出现 ready」这个旗舰场景直接失效。
    #[test]
    fn update_text_publishes_live_log_while_still_running() {
        let t = WaitTargets::default();
        t.register("svc_dev", TargetKind::Service, None);
        // 未退出：状态仍是 Running，但日志尾必须可见
        t.update_text("svc_dev", "ready in 812ms");
        let e = t.lookup("svc_dev").unwrap();
        assert_eq!(e.state(), TargetState::Running);
        assert!(!e.is_completed(), "进程还活着就不算完成");
        assert_eq!(e.text().as_deref(), Some("ready in 812ms"));
        assert!(e.observed().text.is_some());
    }

    #[test]
    fn update_text_on_unknown_id_is_noop() {
        let t = WaitTargets::default();
        t.update_text("ghost", "whatever");
        assert!(t.lookup("ghost").is_none());
    }

    #[test]
    fn register_is_idempotent_and_does_not_clobber_terminal() {
        let t = WaitTargets::default();
        t.register("svc_a", TargetKind::Service, None);
        t.finish("svc_a", TargetState::Succeeded, None, None);
        // 重复登记（无基线 = 只登记一次）：返回同一条目，终态不被重置
        t.register("svc_a", TargetKind::Service, None);
        assert_eq!(t.lookup("svc_a").unwrap().state(), TargetState::Succeeded);
        assert_eq!(t.ids().len(), 1);
    }

    #[test]
    fn finish_on_unknown_id_is_noop() {
        let t = WaitTargets::default();
        t.finish("ghost", TargetState::Succeeded, None, None);
        assert!(t.lookup("ghost").is_none());
    }

    /// 计划任务没有 running 中间态：`last_status` 在 run 期间是上一次的旧值，
    /// 故「完成」要求本轮状态 ≠ 登记基线，否则登记后立刻误判完成。
    #[test]
    fn scheduled_task_needs_status_change_from_baseline() {
        let t = WaitTargets::default();
        t.register("task_a", TargetKind::ScheduledTask, Some("ok".into()));
        // run 结束：终态 + 状态写回（基线 "ok" → 本轮 "error"）
        t.finish(
            "task_a",
            TargetState::Failed,
            Some("本轮摘要".into()),
            Some("error".into()),
        );
        let e = t.lookup("task_a").unwrap();
        assert!(e.is_completed(), "状态已变化 → 本次 run 完成");
    }

    /// 摘要（自由文本）与基线（状态串）**不可互相比较**：本轮摘要字面上就写着 "ok"，
    /// 而基线也是 "ok"——拿 text 比 baseline 会在两次运行都成功时永远判不出变化。
    #[test]
    fn scheduled_task_compares_status_not_summary() {
        let t = WaitTargets::default();
        t.register("task_c", TargetKind::ScheduledTask, Some("ok".into()));
        // 本轮状态**没变**（仍是 ok），只是摘要里恰好出现 "ok" 字样
        t.finish(
            "task_c",
            TargetState::Succeeded,
            Some("all ok, 12 tests passed".into()),
            Some("ok".into()),
        );
        assert!(
            !t.lookup("task_c").unwrap().is_completed(),
            "状态未变 → 不是本次 run 的完成"
        );
    }

    /// 🔴 回归（审查发现）：计划任务**二次运行**不得被上一轮的终态直接判为已完成。
    ///
    /// 复现序列：`every:30 m` 任务第一轮跑完（终态 + status 写回），第二轮开跑时
    /// 再次 `register`——若不重置条目，`is_completed()` 会拿上一轮的 `observed_status`
    /// 与新基线比较而立即命中，`wait` 每次都秒回，任务等于没被等到。
    #[test]
    fn second_scheduled_task_round_resets_previous_terminal() {
        let t = WaitTargets::default();
        // 第一轮：基线 None（首次运行）→ 跑完写回 ok
        t.register("task_d", TargetKind::ScheduledTask, None);
        t.finish(
            "task_d",
            TargetState::Succeeded,
            Some("一轮".into()),
            Some("ok".into()),
        );
        assert!(t.lookup("task_d").unwrap().is_completed());

        // 第二轮开跑：基线是本轮开始前的 last_status（"ok"）
        t.register("task_d", TargetKind::ScheduledTask, Some("ok".into()));
        let e = t.lookup("task_d").unwrap();
        assert_eq!(e.state(), TargetState::Running, "新一轮必须重置回进行中");
        assert!(!e.is_completed(), "第二轮开跑时不得被上一轮终态判为已完成");
        assert_eq!(e.baseline().as_deref(), Some("ok"));

        // 第二轮跑完（这次是 error，与基线不同）
        t.finish(
            "task_d",
            TargetState::Failed,
            Some("二轮".into()),
            Some("error".into()),
        );
        assert!(t.lookup("task_d").unwrap().is_completed());
    }

    /// 同理：第二轮**结果与第一轮相同**（都是 ok）时，基线与观测值相等 → 不判完成。
    /// 这是 `begin_round` 必须清空 `observed_status` 的理由——只清终态会漏判。
    #[test]
    fn second_round_with_same_status_is_not_completed() {
        let t = WaitTargets::default();
        t.register("task_e", TargetKind::ScheduledTask, None);
        t.finish("task_e", TargetState::Succeeded, None, Some("ok".into()));
        t.register("task_e", TargetKind::ScheduledTask, Some("ok".into()));
        t.finish("task_e", TargetState::Succeeded, None, Some("ok".into()));
        assert!(
            !t.lookup("task_e").unwrap().is_completed(),
            "状态与基线相同 → 本次尚未观测到变化"
        );
    }

    /// service / 子代理（无基线）重复登记**不重置**：它们一生命周期只登记一次，
    /// 重置会把已记录的终态抹掉，让 wait 反过来等不到结束。
    #[test]
    fn re_register_without_baseline_keeps_terminal() {
        let t = WaitTargets::default();
        t.register("svc_f", TargetKind::Service, None);
        t.finish("svc_f", TargetState::Succeeded, None, None);
        t.register("svc_f", TargetKind::Service, None);
        assert_eq!(t.lookup("svc_f").unwrap().state(), TargetState::Succeeded);
        assert!(t.lookup("svc_f").unwrap().is_completed());
    }

    #[test]
    fn remove_and_ids_are_deterministic() {
        let t = WaitTargets::default();
        t.register("svc_c", TargetKind::Service, None);
        t.register("svc_a", TargetKind::Service, None);
        t.register("sub_b", TargetKind::Subagent, None);
        assert_eq!(t.ids(), vec!["sub_b", "svc_a", "svc_c"]);
        assert!(t.remove("svc_a").is_some());
        assert!(t.remove("svc_a").is_none());
        assert_eq!(t.ids(), vec!["sub_b", "svc_c"]);
    }

    #[test]
    fn observation_exposes_kind_status_text() {
        let t = WaitTargets::default();
        t.register("svc_a", TargetKind::Service, None);
        t.finish("svc_a", TargetState::Failed, Some("err".into()), None);
        let obs = t.lookup("svc_a").unwrap().observed();
        assert_eq!(obs.kind, TargetKind::Service);
        assert_eq!(obs.kind.as_str(), "service");
        assert_eq!(obs.status, "failed");
        assert_eq!(obs.text.as_deref(), Some("err"));
        assert_eq!(TargetState::Running.as_str(), "running");
        assert_eq!(TargetKind::Subagent.as_str(), "subagent");
        assert_eq!(TargetKind::ScheduledTask.as_str(), "scheduled_task");
        assert!(!TargetState::Running.is_terminal());
        assert!(TargetState::Failed.is_terminal());
    }

    /// 惰性清扫：超 TTL 的终态条目被淘汰，进行中的永不淘汰。
    ///
    /// 进行中不淘汰是硬要求：否则等一个正在跑的任务会被凭空判成「不存在」而报
    /// `E_NOT_FOUND`——比多等一轮严重得多。
    #[test]
    fn sweep_reaps_expired_terminal_but_keeps_running() {
        let t = WaitTargets::default();
        t.register("svc_done", TargetKind::Service, None);
        t.finish("svc_done", TargetState::Succeeded, None, None);
        t.register("svc_live", TargetKind::Service, None);
        // 把终态条目的时刻推到 TTL 之前
        {
            let e = t.lookup("svc_done").unwrap();
            *e.ended_at.lock().unwrap() =
                Some(SystemTime::now() - ENTRY_TTL - Duration::from_secs(1));
        }
        let got = t.lookup("svc_done");
        assert!(got.is_none(), "超 TTL 终态条目应被 lookup 清扫");
        assert!(t.lookup("svc_live").is_some(), "进行中条目永不淘汰");
    }

    /// 未超 TTL 的终态条目必须留着：反复等同一个任务是常见用法，
    /// 过早清理会把「已完成」退化成「不存在」。
    #[test]
    fn sweep_keeps_fresh_terminal_entry() {
        let t = WaitTargets::default();
        t.register("svc_a", TargetKind::Service, None);
        t.finish("svc_a", TargetState::Succeeded, None, None);
        assert!(t.lookup("svc_a").is_some());
        assert_eq!(t.ids(), vec!["svc_a"]);
    }

    /// ids() 不触发清扫（它只用于拼错误提示，突变会不可预期）。
    #[test]
    fn ids_does_not_sweep() {
        let t = WaitTargets::default();
        t.register("svc_old", TargetKind::Service, None);
        t.finish("svc_old", TargetState::Succeeded, None, None);
        {
            let e = t.lookup("svc_old").unwrap();
            *e.ended_at.lock().unwrap() =
                Some(SystemTime::now() - ENTRY_TTL - Duration::from_secs(1));
        }
        assert_eq!(t.ids(), vec!["svc_old"], "ids 不应清扫");
        assert!(t.lookup("svc_old").is_none(), "随后的 lookup 才清扫");
    }
}
