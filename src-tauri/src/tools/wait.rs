//! wait 工具：可取消的 1–3600 秒等待，支持条件等待（Interactive：批次内唯一调用）。
//!
//! 两种模式：
//! - **纯计时**（省略 `until`）：`tokio::time::sleep` 到点即回，出参逐字保持
//!   `{waited_seconds, reason}` —— 旧会话历史与工具卡的兼容底线，改动不得增删字段。
//! - **条件等待**（传 `until`）：轮询 `WaitTargets` 登记表（间隔 1s，固定不可配），
//!   条件命中即提前结束；`seconds` 退化为**最长等待上限**。到点未命中返回
//!   `ok` + `outcome:"timeout"`（超时不占用错误码，是正常结果），并回显 `observed`
//!   让模型据最后观测决定下一步。
//!
//! 目标在等待过程中被回收（`remove`）视为「不存在」→ `E_NOT_FOUND`，不当成
//! 「未完成」空转到超时。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::wait_targets::{TargetState, WaitTarget};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// 条件等待的轮询间隔：固定 1 秒，不读配置、不开放为参数（1s 的粒度对
/// 「等 dev server 起来 / 等子代理收尾」这类场景足够，且避免忙轮询）。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// `observed.text` 的字节上限：超时场景最需要这段摘要，但必须限长防上下文膨胀。
const OBSERVED_TEXT_BYTES: usize = 2000;

/// wait 工具：被动等待指定秒数（如等 dev server 启动），可带条件提前结束。
/// 入参为 seconds（1–3600）+ reason + 可选 until；Interactive 分级——必须独占批次；
/// 等待期间用户可随时取消（E_CANCELLED）。
pub struct WaitTool;

/// `until` 的归一化判据。字段与 schema 一一对应，回显进出参便于模型自证条件。
#[derive(Debug, Clone)]
struct Until {
    /// 目标 id。
    target: String,
    /// 终态判据（缺省 = 任意终态）。
    state: Option<StateWant>,
    /// 摘要子串判据（大小写不敏感）；与 `state` 是 **AND** 关系。
    match_text: Option<String>,
}

/// 显式声明的终态判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateWant {
    /// 仅 `TargetState::Succeeded` 命中。
    Exited,
    /// 仅 `TargetState::Failed` 命中。
    Failed,
}

/// 轻量观测（出参 `observed` 的形状）。独立于 `TargetObservation`，因为此处
/// 需要 `text` 已被限长处理。
struct TargetObservationLite {
    kind: &'static str,
    status: String,
    text: Option<String>,
    /// 本次探测读取快照失败（锁 poisoned）时的说明。透出到出参让模型知道
    /// 这一轮是**保守判为未命中**，而不是真的确认了「还没完成」。
    probe_error: Option<String>,
}

impl Until {
    /// 出参里的 `until` 回显面（归一化后：缺省的 `state` 不补全，让模型看清
    /// 「我没写 state = 任意终态」）。
    fn echo(&self) -> Value {
        let mut v = json!({ "target": self.target });
        if let Some(s) = self.state {
            v["state"] = json!(match s {
                StateWant::Exited => "exited",
                StateWant::Failed => "failed",
            });
        }
        if let Some(t) = &self.match_text {
            v["match"] = json!({ "text": t });
        }
        v
    }

    /// 判定当前快照是否命中：state（若声明）与 match_text（若声明）**同时**满足。
    ///
    /// `state` 缺省 = 任意终态即命中（`is_completed`）；显式 `exited` 时仅
    /// `Succeeded` 命中、`Failed` **不算**（继续等到超时），显式 `failed` 反之。
    fn matches(&self, t: &WaitTarget) -> bool {
        let state_ok = match self.state {
            None => t.is_completed(),
            Some(StateWant::Exited) => t.state() == TargetState::Succeeded,
            Some(StateWant::Failed) => t.state() == TargetState::Failed,
        };
        if !state_ok {
            return false;
        }
        match &self.match_text {
            None => true,
            // 大小写不敏感的子串匹配；text 为 None 时不命中。
            Some(needle) => t
                .text()
                .is_some_and(|hay| hay.to_lowercase().contains(&needle.to_lowercase())),
        }
    }
}

impl TargetObservationLite {
    /// 从目标快照取观测（`text` 限长）。锁 poisoned 时**不 panic**：`WaitTarget`
    /// 内部用 `lock().unwrap()`，一旦某次探测 panic 中断了持锁线程，本工具不能
    /// 跟着崩。`kind` 是字段读取（无锁）始终可靠。
    fn of(t: &WaitTarget) -> Self {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (t.state().as_str().to_string(), t.text())
        })) {
            Ok((status, text)) => TargetObservationLite {
                kind: t.kind().as_str(),
                status,
                text: text.map(|s| truncate_bytes(&s, OBSERVED_TEXT_BYTES)),
                probe_error: None,
            },
            Err(_) => TargetObservationLite {
                kind: t.kind().as_str(),
                status: "unknown".to_string(),
                text: None,
                probe_error: Some("目标快照读取失败（锁 poisoned），本轮保守判为未命中".into()),
            },
        }
    }

    /// 出参里的 `observed` 面。
    fn to_json(&self) -> Value {
        let mut v = json!({ "kind": self.kind, "status": self.status });
        if let Some(t) = &self.text {
            v["text"] = json!(t);
        }
        if let Some(e) = &self.probe_error {
            v["probe_error"] = json!(e);
        }
        v
    }
}

/// 按**字节**上限截断字符串，落在合法 UTF-8 边界上（尾部补省略标记）。
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[已截断]", &s[..end])
}

/// 探测一次：返回 `(是否命中, 观测快照)`。
///
/// 判据与取快照都可能被锁 poison 打断，两者各自 `catch_unwind`：判据失败→保守
/// 判未命中（宁可多等一轮，也不能把「没看到」当成「已完成」）。
fn probe(until: &Until, target: &Arc<WaitTarget>) -> (bool, TargetObservationLite) {
    let hit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| until.matches(target)))
        .unwrap_or(false);
    (hit, TargetObservationLite::of(target))
}

#[async_trait::async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &'static str {
        "wait"
    }
    fn description(&self) -> &'static str {
        "等待工具，两种模式：（1）省略 until = 纯计时等 1–3600 秒（如等 dev server 启动）；（2）给 until = 条件等待，盯住本会话 service / subagent / scheduled_task 工具回执里的 id，达成终态（或进展摘要命中 match）即提前结束，seconds 退化为最长等待上限。凡目标有 id 且有可判定终态/日志标志，必须用 until，禁止用大 seconds 盲等。超时不是错误：返回 ok + outcome:\"timeout\" + observed，请据 observed 决定下一步（继续等、换条件或直接读日志）。until 不启动任何任务，target 只能取自本会话既有回执 id。始终是所在轮次中唯一的工具调用。"
    }
    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["seconds", "reason"],
  "properties": {
    "seconds": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "最长等待秒数。省略 until 时为纯计时等待时长；给 until 时为条件等待的上限（条件命中即提前返回）"},
    "reason": {"type": "string", "description": "在等什么"},
    "until": {
      "type": "object",
      "additionalProperties": false,
      "required": ["target"],
      "description": "可选：条件等待。命中条件即提前结束等待；省略它 = 纯计时等待。目标必须已有可等待的 id（本会话 service / subagent / scheduled_task 工具回执），wait 不会启动任何任务。",
      "properties": {
        "target": {"type": "string", "description": "目标 id：service / subagent / scheduled_task 工具回执里的 id"},
        "state": {"type": "string", "enum": ["done", "exited", "failed"], "description": "等待哪种终态；缺省 done = 任意终态（成功或失败都算结束）"},
        "match": {
          "type": "object",
          "additionalProperties": false,
          "required": ["text"],
          "description": "可选：额外要求最近进展摘要（service 为日志尾、子代理为 report、计划任务为上次摘要）包含指定子串，大小写不敏感。与 state 是 AND 关系（两个都写则必须同时满足）",
          "properties": {"text": {"type": "string", "description": "最近进展摘要（service 为日志尾）包含该子串即命中；大小写不敏感"}}
        }
      }
    }
  }
}"#
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Interactive
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let secs = match args["seconds"].as_u64() {
            Some(s) if (1..=3600).contains(&s) => s,
            _ => return ToolOutcome::err("E_ARGS", "seconds 必须在 1–3600"),
        };
        let reason = args["reason"].as_str().unwrap_or_default().to_string();
        match parse_until(&args) {
            // 纯计时模式：省略 until 时**不引入任何轮询分支**，出参逐字不变
            // （旧会话历史与工具卡兼容底线）。
            UntilArg::Absent => {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {
                        super::ToolOutcome::ok(json!({ "waited_seconds": secs, "reason": reason }))
                    }
                    _ = ctx.cancel.cancelled() => {
                        super::ToolOutcome::err("E_CANCELLED", "等待被用户取消")
                    }
                }
            }
            UntilArg::Invalid(msg) => ToolOutcome::err("E_ARGS", msg),
            UntilArg::Ok(u) => self.run_conditional(ctx, secs, reason, u).await,
        }
    }
}

impl WaitTool {
    /// 条件等待：轮询登记表直到命中 / 目标消失 / 取消 / 到点。
    ///
    /// 判据顺序 **取消 > 条件 > 超时**，三处各有一道闸：
    /// 1. t=0 首探前先看 cancel（预取消的 token 必须赢过已满足的条件）；
    /// 2. `select!` 用 `biased` 把 cancel 分支排在最前；
    /// 3. 每轮落地后**再查一次** cancel——即便 deadline 抢先就绪，条件恰好在到点
    ///    瞬间达成时也先排掉取消，再判条件命中而非超时。
    async fn run_conditional(
        &self,
        ctx: &ToolCtx,
        secs: u64,
        reason: String,
        until: Until,
    ) -> ToolOutcome {
        let started = tokio::time::Instant::now();
        // 判据顺序 **取消 > 条件**：预先取消的 token 必须抢在 t=0 首探之前生效，
        // 否则「已取消 + 条件已满足」会误报为命中。
        if ctx.cancel.is_cancelled() {
            return ToolOutcome::err("E_CANCELLED", "等待被用户取消");
        }
        // t=0 立即首探（不等第一个 tick，否则每次白等一个轮询周期）。
        let Some(target) = ctx.core.wait_targets.lookup(&until.target) else {
            return not_found(ctx, &until);
        };
        let (hit, mut last) = probe(&until, &target);
        if hit {
            return finish_ok(started, reason, &until, last, "condition_met");
        }
        // 固定 1s 轮询；`Delay` 策略避免探测耗时累积导致的追帧突发。
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval` 首次 tick 立即就绪，上面已首探，这里吞掉那次空转。
        ticker.tick().await;
        // deadline 必须在循环外算好：在循环里重建 `sleep` 会每轮重置计时。
        let deadline = started + Duration::from_secs(secs);
        loop {
            let timed_out = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    return ToolOutcome::err("E_CANCELLED", "等待被用户取消");
                }
                _ = ticker.tick() => false,
                _ = tokio::time::sleep_until(deadline) => true,
            };
            // 取消优先于条件：条件恰好在到点 / tick 同瞬满足时，仍以取消为最高判据。
            if ctx.cancel.is_cancelled() {
                return ToolOutcome::err("E_CANCELLED", "等待被用户取消");
            }
            // 目标可能在等待期间被回收（service stop / 任务删除）→ 视为不存在，
            // 不当成「未完成」空转到超时。与超时同瞬被回收的也走这里。
            let Some(alive) = ctx.core.wait_targets.lookup(&until.target) else {
                return not_found(ctx, &until);
            };
            let (hit, obs) = probe(&until, &alive);
            if hit {
                return finish_ok(started, reason, &until, obs, "condition_met");
            }
            last = obs;
            if timed_out {
                // 超时是正常结果（ok），不占用错误码。
                return finish_ok(started, reason, &until, last, "timeout");
            }
        }
    }
}

/// 目标不存在：附当前可用 id 帮模型自纠。`ids()` 仅用于提示，不触发清扫——
/// 清扫只发生在 `lookup`，避免拼提示的副作用不可预期。
fn not_found(ctx: &ToolCtx, until: &Until) -> ToolOutcome {
    let known = ctx.core.wait_targets.ids();
    let hint = if known.is_empty() {
        "当前没有任何可等待的目标".to_string()
    } else {
        format!("可等待的目标：{}", known.join(", "))
    };
    ToolOutcome::err(
        "E_NOT_FOUND",
        format!("目标不存在：{}（{hint}）", until.target),
    )
}

/// 条件 / 超时的统一出参构造：`ok` + `outcome` + 归一化 `until` 回显 + `observed`。
fn finish_ok(
    started: tokio::time::Instant,
    reason: String,
    until: &Until,
    observed: TargetObservationLite,
    outcome: &str,
) -> ToolOutcome {
    // 实测秒数向下取整（虚拟时钟下 elapsed 常是 0.xxx 秒）。
    let waited = started.elapsed().as_secs();
    ToolOutcome::ok(json!({
        "waited_seconds": waited,
        "reason": reason,
        "outcome": outcome,
        "until": until.echo(),
        "observed": observed.to_json(),
    }))
}

/// `until` 入参的三态：未传（纯计时）/ 传了但非法（`E_ARGS`）/ 合法。
enum UntilArg {
    Absent,
    Invalid(&'static str),
    Ok(Until),
}

/// 解析并校验 `until`。校验顺序与错误文案按「先结构、再 target、再判据」，
/// 让模型能从单条错误里知道改哪里。
fn parse_until(args: &Value) -> UntilArg {
    let Some(raw) = args.get("until") else {
        return UntilArg::Absent;
    };
    let Some(u) = raw.as_object() else {
        return UntilArg::Invalid("until 必须是对象");
    };
    let target = u.get("target").and_then(Value::as_str).unwrap_or_default();
    if target.trim().is_empty() {
        return UntilArg::Invalid(
            "until.target 不能为空（须为本会话 service / subagent / scheduled_task 回执里的 id）",
        );
    }
    let state = match u.get("state") {
        None => None,
        Some(Value::String(s)) if s == "done" => None, // done = 缺省语义
        Some(Value::String(s)) if s == "exited" => Some(StateWant::Exited),
        Some(Value::String(s)) if s == "failed" => Some(StateWant::Failed),
        Some(_) => return UntilArg::Invalid("until.state 只能是 done / exited / failed"),
    };
    let match_text = match u.get("match") {
        None => None,
        Some(m) => match m
            .as_object()
            .and_then(|o| o.get("text"))
            .and_then(Value::as_str)
        {
            Some(t) if !t.is_empty() => Some(t.to_string()),
            _ => return UntilArg::Invalid("until.match.text 必须是非空字符串"),
        },
    };
    UntilArg::Ok(Until {
        target: target.to_string(),
        state,
        match_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> ToolCtx {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = super::super::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = crate::core::agent::test_support::make_core(&roots);
        let rt = core.get_or_create_session(
            "wait-test",
            roots.workspace.clone(),
            None,
            vec![],
            None,
            vec![],
        );
        ToolCtx {
            core,
            rt,
            batch_id: "b".into(),
            call_index: 0,
            call_key: "b:0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// 暂停的虚拟时钟下 1–3600s 等待瞬时完成。
    #[tokio::test(start_paused = true)]
    async fn waits_full_duration_and_reports_reason() {
        let ctx = ctx();
        let out = WaitTool
            .run(&ctx, json!({"seconds": 3600, "reason": "dev server boot"}))
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["waited_seconds"], 3600);
        assert_eq!(out.data["reason"], "dev server boot");
    }

    #[tokio::test]
    async fn reason_defaults_to_empty_string() {
        let ctx = ctx();
        let out = WaitTool.run(&ctx, json!({"seconds": 1})).await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["reason"], "");
    }

    #[tokio::test]
    async fn seconds_outside_1_3600_rejected() {
        let ctx = ctx();
        for bad in [0u64, 3601, 9999] {
            let out = WaitTool.run(&ctx, json!({"seconds": bad})).await;
            assert_eq!(out.error.unwrap().code, "E_ARGS", "seconds={bad}");
        }
        // 缺失 / 非数值的 seconds
        let out = WaitTool.run(&ctx, json!({"reason": "x"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
        let out = WaitTool.run(&ctx, json!({"seconds": "5"})).await;
        assert_eq!(out.error.unwrap().code, "E_ARGS");
    }

    #[tokio::test]
    async fn cancel_beats_the_wait() {
        let ctx = ctx();
        ctx.cancel.cancel();
        let out = WaitTool
            .run(&ctx, json!({"seconds": 3600, "reason": "never finishes"}))
            .await;
        let err = out.error.unwrap();
        assert_eq!(err.code, "E_CANCELLED");
    }

    /// 等待进行中的取消同样生效（不只是预先取消的 token）。
    #[tokio::test(start_paused = true)]
    async fn cancel_mid_wait_interrupts() {
        let ctx = ctx();
        let cancel = ctx.cancel.clone();
        let task = tokio::spawn(async move {
            WaitTool
                .run(&ctx, json!({"seconds": 3600, "reason": "interrupt me"}))
                .await
        });
        cancel.cancel();
        let out = task.await.unwrap();
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
    }

    // ── 条件等待（until）────────────────────────────────────────────────

    use crate::core::wait_targets::TargetKind;

    /// 条件满足提前结束：目标 5s 后完成、上限 3600s → 明显早退。
    #[tokio::test(start_paused = true)]
    async fn condition_met_ends_wait_early() {
        let ctx = ctx();
        // `WaitTargets` 未实现 Clone（内部 DashMap），跨线程传递时克隆外层 Arc。
        let core = ctx.core.clone();
        core.wait_targets
            .register("svc_a", TargetKind::Service, None);
        let _task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            core.wait_targets.finish(
                "svc_a",
                TargetState::Succeeded,
                Some("listening :5173".into()),
                None,
            );
        });
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 3600, "reason": "等 dev server", "until": {"target": "svc_a"}}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["outcome"], "condition_met");
        let waited = out.data["waited_seconds"].as_u64().unwrap();
        assert!(waited <= 6, "应远早于 3600s 上限，实际等了 {waited}s");
        assert_eq!(out.data["until"]["target"], "svc_a");
        assert_eq!(out.data["observed"]["kind"], "service");
        assert_eq!(out.data["observed"]["status"], "succeeded");
        assert_eq!(out.data["observed"]["text"], "listening :5173");
    }

    /// t=0 首探：目标登记时已终态 → 一个轮询周期都不等，waited_seconds == 0。
    #[tokio::test(start_paused = true)]
    async fn terminal_target_is_probed_at_t0() {
        let ctx = ctx();
        let targets = &ctx.core.wait_targets;
        targets.register("sub_a", TargetKind::Subagent, None);
        targets.finish("sub_a", TargetState::Succeeded, Some("report".into()), None);
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 600, "reason": "子代理收尾", "until": {"target": "sub_a"}}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["outcome"], "condition_met");
        assert_eq!(
            out.data["waited_seconds"], 0,
            "t=0 首探应立即命中，不等第一个 tick"
        );
        assert_eq!(out.data["observed"]["kind"], "subagent");
    }

    /// 超时是**正常结果**：ok + outcome:"timeout" + observed，不得占错误码。
    #[tokio::test(start_paused = true)]
    async fn timeout_is_ok_with_observed() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_b", TargetKind::Service, None);
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 3, "reason": "等永远不会出现的日志", "until": {"target": "svc_b"}}),
            )
            .await;
        assert!(out.ok, "超时不得是错误：{out:?}");
        assert_eq!(out.data["outcome"], "timeout");
        assert_eq!(out.data["observed"]["status"], "running");
        assert_eq!(out.data["waited_seconds"], 3);
        assert_eq!(out.data["reason"], "等永远不会出现的日志");
        assert_eq!(out.data["until"]["target"], "svc_b");
        assert!(out.data["observed"].get("text").is_none());
    }

    /// 目标不存在 → `E_NOT_FOUND`，并列举可用 id 帮模型自纠。
    #[tokio::test]
    async fn unknown_target_is_not_found() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_known", TargetKind::Service, None);
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 5, "reason": "等一个不存在的目标", "until": {"target": "svc_ghost"}}),
            )
            .await;
        assert!(!out.ok);
        let err = out.error.unwrap();
        assert_eq!(err.code, "E_NOT_FOUND");
        assert!(
            err.message.contains("svc_ghost") && err.message.contains("svc_known"),
            "错误应点名缺失 id 并列举可用 id：{err:?}"
        );
    }

    /// 非法 `until` 一律 `E_ARGS`（空 target / 非法 state / 非法 match / 非对象）。
    #[tokio::test]
    async fn invalid_until_is_args_error() {
        let ctx = ctx();
        let bad: Vec<Value> = vec![
            json!({"target": ""}),
            json!({"target": "   "}),
            json!({}),
            json!({"target": "svc_a", "state": "weird"}),
            json!({"target": "svc_a", "state": 1}),
            json!({"target": "svc_a", "match": {}}),
            json!({"target": "svc_a", "match": {"text": ""}}),
            json!({"target": "svc_a", "match": "ready"}),
            json!("svc_a"),
        ];
        for u in bad {
            let out = WaitTool
                .run(&ctx, json!({"seconds": 5, "reason": "x", "until": u}))
                .await;
            assert_eq!(out.error.map(|e| e.code), Some("E_ARGS".into()), "{u}");
        }
    }

    /// 取消优先于条件：预先取消的 token + 已满足条件 → `E_CANCELLED`。
    #[tokio::test(start_paused = true)]
    async fn cancel_beats_satisfied_condition() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("sub_c", TargetKind::Subagent, None);
        ctx.core
            .wait_targets
            .finish("sub_c", TargetState::Succeeded, Some("done".into()), None);
        ctx.cancel.cancel();
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 60, "reason": "条件已满足但用户已取消", "until": {"target": "sub_c"}}),
            )
            .await;
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
    }

    /// 条件等待中途的取消同样生效。
    #[tokio::test(start_paused = true)]
    async fn cancel_mid_conditional_wait_interrupts() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_c", TargetKind::Service, None);
        let cancel = ctx.cancel.clone();
        let task = tokio::spawn(async move {
            WaitTool
                .run(
                    &ctx,
                    json!({"seconds": 3600, "reason": "中途取消", "until": {"target": "svc_c"}}),
                )
                .await
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
        cancel.cancel();
        let out = task.await.unwrap();
        assert_eq!(out.error.unwrap().code, "E_CANCELLED");
    }

    /// `match.text` 大小写不敏感子串命中。
    #[tokio::test]
    async fn match_text_is_case_insensitive_substring() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_d", TargetKind::Service, None);
        ctx.core.wait_targets.finish(
            "svc_d",
            TargetState::Succeeded,
            Some("Server Ready On Port 5173".into()),
            None,
        );
        let out = WaitTool
            .run(
                &ctx,
                json!({
                    "seconds": 60,
                    "reason": "等 ready 日志",
                    "until": {"target": "svc_d", "match": {"text": "ready on"}}
                }),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["outcome"], "condition_met");
        assert_eq!(out.data["until"]["match"]["text"], "ready on");
        assert!(
            out.data["until"].get("state").is_none(),
            "缺省的 state 不应在回显里补全"
        );
    }

    /// state 与 match 是 AND 关系：终态到了但摘要没命中 → 走超时。
    #[tokio::test(start_paused = true)]
    async fn state_and_match_are_anded() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_e", TargetKind::Service, None);
        ctx.core.wait_targets.finish(
            "svc_e",
            TargetState::Succeeded,
            Some("port 5173".into()),
            None,
        );
        let out = WaitTool
            .run(
                &ctx,
                json!({
                    "seconds": 2,
                    "reason": "终态到了但摘要不含 ready",
                    "until": {"target": "svc_e", "match": {"text": "ready"}}
                }),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(
            out.data["outcome"], "timeout",
            "AND 语义：只满足一半不算命中"
        );
        assert_eq!(out.data["observed"]["text"], "port 5173");
    }

    /// 显式 `state:"exited"` + 目标 Failed → **不命中**（继续等到超时）。
    #[tokio::test(start_paused = true)]
    async fn explicit_exited_does_not_match_failed_target() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_f", TargetKind::Service, None);
        ctx.core.wait_targets.finish(
            "svc_f",
            TargetState::Failed,
            Some("port in use".into()),
            None,
        );
        let out = WaitTool
            .run(
                &ctx,
                json!({
                    "seconds": 2,
                    "reason": "只要正常退出",
                    "until": {"target": "svc_f", "state": "exited"}
                }),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["outcome"], "timeout", "Failed 不应命中显式 exited");
        assert_eq!(out.data["observed"]["status"], "failed");
        assert_eq!(out.data["until"]["state"], "exited");
    }

    /// 显式 `state:"failed"` 只在 Failed 时命中（对照面）。
    #[tokio::test]
    async fn explicit_failed_matches_failed_target() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_g", TargetKind::Service, None);
        ctx.core
            .wait_targets
            .finish("svc_g", TargetState::Failed, Some("crashed".into()), None);
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 60, "reason": "等崩溃", "until": {"target": "svc_g", "state": "failed"}}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["outcome"], "condition_met");
        assert_eq!(out.data["waited_seconds"], 0);
    }

    /// `state:"done"` 与省略等价（任意终态均命中）。
    #[tokio::test]
    async fn explicit_done_matches_any_terminal_state() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_j", TargetKind::Service, None);
        ctx.core
            .wait_targets
            .finish("svc_j", TargetState::Failed, Some("boom".into()), None);
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 60, "reason": "任意终态即可", "until": {"target": "svc_j", "state": "done"}}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        assert_eq!(out.data["outcome"], "condition_met");
    }

    /// 等待中目标被 remove（service stop / 任务删除）→ `E_NOT_FOUND`，
    /// 不得当成「未完成」空转到超时。
    #[tokio::test(start_paused = true)]
    async fn target_removed_mid_wait_is_not_found() {
        let ctx = ctx();
        let core = ctx.core.clone();
        core.wait_targets
            .register("svc_h", TargetKind::Service, None);
        let _task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            core.wait_targets.remove("svc_h");
        });
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 3600, "reason": "等一个会被 stop 的服务", "until": {"target": "svc_h"}}),
            )
            .await;
        assert!(!out.ok);
        assert_eq!(out.error.unwrap().code, "E_NOT_FOUND");
    }

    /// `observed.text` 超长时限长到 2000 字节（防上下文膨胀），且不 panic。
    #[tokio::test(start_paused = true)]
    async fn observed_text_is_capped_at_2000_bytes() {
        let ctx = ctx();
        ctx.core
            .wait_targets
            .register("svc_i", TargetKind::Service, None);
        let big = "x".repeat(9000);
        ctx.core
            .wait_targets
            .finish("svc_i", TargetState::Succeeded, Some(big), None);
        let out = WaitTool
            .run(
                &ctx,
                json!({"seconds": 60, "reason": "限长", "until": {"target": "svc_i"}}),
            )
            .await;
        assert!(out.ok, "{out:?}");
        let text = out.data["observed"]["text"].as_str().unwrap();
        assert!(
            text.len() <= OBSERVED_TEXT_BYTES + "…[已截断]".len(),
            "限长失效：{} 字节",
            text.len()
        );
        assert!(text.ends_with("[已截断]"));
    }

    /// 纯计时模式的出参**逐字**不变（兼容底线）：不得多出 outcome/until/observed。
    #[tokio::test(start_paused = true)]
    async fn plain_mode_data_has_no_extra_fields() {
        let ctx = ctx();
        let out = WaitTool
            .run(&ctx, json!({"seconds": 30, "reason": "compat"}))
            .await;
        assert_eq!(out.data, json!({"waited_seconds": 30, "reason": "compat"}));
    }

    /// schema 必须是合法 JSON，且 until 为可选、additionalProperties 关闭。
    #[test]
    fn schema_is_valid_json_with_optional_until() {
        let s: Value = serde_json::from_str(WaitTool.schema()).expect("schema 必须是合法 JSON");
        assert_eq!(s["required"], json!(["seconds", "reason"]));
        assert_eq!(s["additionalProperties"], false);
        let u = &s["properties"]["until"];
        assert_eq!(u["type"], "object");
        assert_eq!(u["additionalProperties"], false);
        assert_eq!(u["required"], json!(["target"]));
        assert_eq!(
            u["properties"]["state"]["enum"],
            json!(["done", "exited", "failed"])
        );
        assert_eq!(u["properties"]["match"]["required"], json!(["text"]));
    }

    /// 多字节文本在 2000 字节边界被截断时不得切坏 UTF-8。
    #[test]
    fn truncate_bytes_respects_utf8_boundary() {
        let s = "日".repeat(1000); // 3000 字节
        let t = truncate_bytes(&s, 2000);
        assert!(t.len() <= 2000 + "…[已截断]".len());
        assert!(t.ends_with("[已截断]"));
        // 2000 字节不是 3 的倍数 → 必须回退到合法边界
        assert!(s.starts_with(t.trim_end_matches("…[已截断]")));
    }
}
