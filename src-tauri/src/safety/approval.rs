//! 审批确认门（决策 D4，[docs/p0-plan](../../../docs/p0-plan.md) §7.3）：复用 ask 通道、无限等待用户应答。
//! [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：等待策略由 `auto_confirm` 决定——勾选后 5 分钟未响应
//! 自动确认推荐选项（允许）；不勾选则永不超时。run 取消（H2 修复）始终可打断，按拒绝处理。
//! 子代理审批例外：事件挂主会话下发（方案 B，emit_session_of）+ 有界超时自动拒绝（方案 C，SUB_APPROVAL_TIMEOUT）。
//! 
//! 寻址原理（2026-09-11 tester 卡死修复）：审批应答经 resolve_ask 路由到 rt.asks 表
//! （主会话未命中时扫描 core.subs，M16 已有），与事件下发寻址解耦；子代理审批事件
//! 以 sub_id 寻址会被前端 ask:opened 的「找不到桶即丢弃」守卫静默吞掉（tabs 以主会话
//! id 为键），导致审批卡永不渲染、无人能应答、审批门永久挂起——故下发统一挂主会话，
//! payload 附 sub_id 供前端标注来源。

use crate::core::agent::{EventSink, SessionRuntime};
use crate::core::session_log;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：auto_confirm 勾选时，等待这么久未响应即自动确认推荐选项
pub const AUTO_CONFIRM_AFTER: Duration = Duration::from_secs(300);

/// 子代理审批的有界等待上限（方案 C 防御性兑底）：超时自动拒绝，杜绝「审批卡因任何
/// UI/路由缺陷无法呈现 → 无人应答 → 审批门永久挂起」演变成子代理卡死。含灾难级
/// ——灾难级限的是「绝不自动允许」，反向超时拒绝不在受限之列（fail-closed）。
pub const SUB_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// 硬性策略（[docs/arithmetic-fixes-batch](../../../docs/arithmetic-fixes-batch.md)）：灾难级 Confirm 绝不搭乘 [docs/session-nav-row-states](../../../docs/session-nav-row-states.md) 的 auto_confirm
/// 超时——auto_confirm 是「放手」便利性，不得延伸到不可逆的系统级命令（fence 在关审批时
/// 对灾难级直接拦；开审批时必须用户亲自应答，与「硬 gate 不因 YOLO 放行」同理）。
pub fn effective_auto_confirm(auto_confirm: bool, is_disaster: bool) -> bool {
    auto_confirm && !is_disaster
}

/// 一次审批请求的载荷：经 ask:opened 事件下发给前端弹卡。
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// 审批标题（如命令名 / 文件路径）
    pub title: String,
    /// 审批详情正文（命令全文 / diff 预览等）
    pub detail: String,
    /// 是否提供「始终允许本项目」（仅命令审批为 true；文件写入确认 / service 命令等一律 false）
    pub allow_always: bool,
    /// [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：5 分钟未响应自动确认推荐选项（允许）；false 表示永不超时、无限等待
    pub auto_confirm: bool,
}

/// 审批结果：approved = 允许；always = 用户选择「始终允许本项目」（仅在 allow_always 时可能为真）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmOutcome {
    /// 是否允许执行
    pub approved: bool,
    /// 是否附带「始终允许本项目」
    pub always: bool,
}

/// 事件下发的寻址会话（方案 B）：主会话原样自指；子代理返回其主会话 id
/// （root_session_id 链条自带嵌套归并：子的子指向主会话）。
/// 纯函数便于单测；应答路由仍走 rt.asks + resolve_ask 的 subs 扫描，与此处无关。
fn emit_session_of(rt: &SessionRuntime) -> (String, Option<String>) {
    match &rt.root_session_id {
        Some(root) => (root.clone(), Some(rt.id.clone())),
        None => (rt.id.clone(), None),
    }
}

/// 弹出对话框并等待用户决定。关闭/拒绝 = 不允许；勾选 auto_confirm 时超时自动允许。
pub async fn confirm(
    rt: &Arc<SessionRuntime>,
    sink: &Arc<dyn EventSink>,
    req: ApprovalRequest,
    cancel: &tokio_util::sync::CancellationToken,
) -> ConfirmOutcome {
    let ask_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt.open_ask(&ask_id, tx);
    // 应答路由仍挂 rt 自己的 asks 表（resolve_ask 主会话未命中会扫描 subs），
    // 只有事件下发改挂父会话——两个寻址解耦，前端无感知子代理 id。
    session_log::info(
        rt,
        &format!(
            "审批提出 [{ask_id}] {}：{}",
            req.title,
            session_log::trunc(&req.detail, 400)
        ),
    );
    let (emit_session, from_sub) = emit_session_of(rt);
    if from_sub.is_some() {
        session_log::info(
            rt,
            &format!("审批 [{ask_id}] 来自子代理 {}，事件挂主会话 {emit_session} 下发", rt.id),
        );
    }
    sink.emit(
        &emit_session,
        "ask:opened",
        json!({
            "session": emit_session, "ask_id": ask_id, "kind": "approval",
            "title": req.title, "detail": req.detail,
            "allow_always": req.allow_always,
            "sub_id": from_sub,
        }),
    );
    // H2 修复：等待审批期间 run 取消可打断（取消 = 拒绝），不再永久挂起
    // [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：auto_confirm=false 永不超时；true 时超时自动确认推荐选项（允许，绝不做「始终允许」）
    // 子代理审批（方案 C）：恒有界等待，超时自动拒绝——审批门不得成为永久挂起点
    let mut auto_confirmed = false;
    let mut sub_timed_out = false;
    let result = if from_sub.is_some() {
        tokio::select! {
            _ = cancel.cancelled() => None,
            r = tokio::time::timeout(SUB_APPROVAL_TIMEOUT, rx) => match r {
                Ok(answer) => Some(answer),
                Err(_) => {
                    sub_timed_out = true;
                    None
                }
            },
        }
    } else if req.auto_confirm {
        tokio::select! {
            _ = cancel.cancelled() => None,
            r = tokio::time::timeout(AUTO_CONFIRM_AFTER, rx) => match r {
                Ok(answer) => Some(answer),
                Err(_) => {
                    auto_confirmed = true;
                    None
                }
            },
        }
    } else {
        tokio::select! {
            _ = cancel.cancelled() => None,
            r = rx => Some(r),
        }
    };
    // asks 表收口（评审修复，测试 sub_approval_times_out_to_deny 抓到的泄漏）：
    // resolve_ask 应答路径会 remove 条目；但取消 / 超时路径此前从不清理——悬空 waiter
    // 与 open_ask 的插入不对称，留着只会误导后续 resolve_ask 误报「已应答」。统一 remove。
    rt.asks.lock().unwrap().remove(&ask_id);
    sink.emit(
        &emit_session,
        "ask:closed",
        json!({ "session": emit_session, "ask_id": ask_id }),
    );
    let (approved, always) = if auto_confirmed {
        (true, false)
    } else {
        match &result {
            Some(Ok(v)) => (
                v["approved"].as_bool().unwrap_or(false),
                req.allow_always && v["always"].as_bool().unwrap_or(false),
            ),
            _ => (false, false),
        }
    };
    let outcome = ConfirmOutcome { approved, always };
    if auto_confirmed {
        session_log::info(
            rt,
            &format!(
                "审批 [{ask_id}] {} {}s 未响应，自动确认推荐选项（允许）",
                req.title,
                AUTO_CONFIRM_AFTER.as_secs()
            ),
        );
    } else if sub_timed_out {
        session_log::info(
            rt,
            &format!(
                "审批 [{ask_id}] {} 子代理 {}s 未响应，超时自动拒绝",
                req.title,
                SUB_APPROVAL_TIMEOUT.as_secs()
            ),
        );
    } else if cancel.is_cancelled() {
        session_log::info(
            rt,
            &format!("审批 [{ask_id}] {} 随 run 取消按拒绝处理", req.title),
        );
    } else if approved {
        session_log::info(
            rt,
            &format!(
                "审批 [{ask_id}] {} → 批准{}",
                req.title,
                if always {
                    "（始终允许本项目）"
                } else {
                    ""
                }
            ),
        );
    } else {
        session_log::info(rt, &format!("审批 [{ask_id}] {} → 拒绝", req.title));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::test_support::make_core;
    use serde_json::json;
    use std::sync::Arc;

    type Core = Arc<crate::core::agent::AgentCore>;
    type Rt = Arc<crate::core::agent::SessionRuntime>;

    /// 共享脚手架：临时工作区 + 数据目录 + core/会话 runtime（返回 TempDirs 以保活）
    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Core, Rt) {
        let ws = tempfile::tempdir().unwrap();
        let dd = tempfile::tempdir().unwrap();
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: std::fs::canonicalize(ws.path()).unwrap(),
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap(),
        };
        let core = make_core(&roots);
        let rt =
            core.get_or_create_session("ap", roots.workspace.clone(), None, vec![], None, vec![]);
        (ws, dd, core, rt)
    }

    /// 启动 confirm 任务并等待审批进入 pending（asks 表出现条目）；返回 (任务, ask_id)。
    /// cancel 按值传入（tokio::spawn 要求 'static）；等待用轮询时钟，start_paused 下即时推进。
    async fn spawn_confirm(
        core: &Core,
        rt: &Rt,
        req: ApprovalRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> (tokio::task::JoinHandle<ConfirmOutcome>, String) {
        let c2 = core.clone();
        let rt2 = rt.clone();
        let task = tokio::spawn(async move { confirm(&rt2, &c2.sink, req, &cancel).await });
        let ask_id = loop {
            if let Some(k) = rt.asks.lock().unwrap().keys().next().cloned() {
                break k;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        (task, ask_id)
    }

    /// [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：confirm 解析前端载荷 {approved, always}；always 仅在 allow_always=true 时生效
    ///（服务端钳制防客户端伪造）；allow_always=false 时 always 恒为 false。
    #[tokio::test]
    async fn confirm_parses_always_with_server_side_gate() {
        let (_ws, _dd, core, rt) = setup();

        let run_case = |allow_always: bool, payload: serde_json::Value| {
            let core = core.clone();
            let rt = rt.clone();
            async move {
                let cancel = tokio_util::sync::CancellationToken::new();
                let req = ApprovalRequest {
                    title: "t".into(),
                    detail: "d".into(),
                    allow_always,
                    auto_confirm: false,
                };
                let (task, ask_id) = spawn_confirm(&core, &rt, req, cancel).await;
                rt.resolve_ask(&ask_id, payload);
                task.await.unwrap()
            }
        };

        let out = run_case(true, json!({ "approved": true, "always": true })).await;
        assert!(out.approved && out.always, "{out:?}");
        let out = run_case(false, json!({ "approved": true, "always": true })).await;
        assert!(
            out.approved && !out.always,
            "allow_always=false 时 always 必须被服务端钳制：{out:?}"
        );
        let out = run_case(true, json!({ "approved": false })).await;
        assert!(!out.approved && !out.always, "{out:?}");
    }

    /// [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：勾选 auto_confirm——5 分钟未响应自动确认推荐选项（允许，绝不做「始终允许」）
    #[tokio::test(start_paused = true)]
    async fn confirm_auto_confirms_recommended_after_timeout_when_enabled() {
        let (_ws, _dd, core, rt) = setup();
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: true,
            auto_confirm: true,
        };
        let (task, _ask_id) = spawn_confirm(&core, &rt, req, cancel).await;
        // start_paused：await 即挂起、虚拟时钟自动推进；300s 超时到期释放任务
        let out = task.await.unwrap();
        assert!(out.approved && !out.always, "超时应自动允许且不做「始终允许」：{out:?}");
    }

    /// [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：不勾选 auto_confirm——永不超时；虚拟时间推进远超 300s 仍 pending，应答后按应答返回
    #[tokio::test(start_paused = true)]
    async fn confirm_waits_indefinitely_when_auto_confirm_disabled() {
        let (_ws, _dd, core, rt) = setup();
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: true,
            auto_confirm: false,
        };
        let (task, ask_id) = spawn_confirm(&core, &rt, req, cancel).await;
        // 虚拟时间推进 1 小时：审批必须仍处 pending（asks 条目仍在、任务未结束）
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        assert!(
            rt.asks.lock().unwrap().contains_key(&ask_id),
            "auto_confirm=false 时审批不得因超时收口"
        );
        rt.resolve_ask(&ask_id, json!({ "approved": true }));
        let out = task.await.unwrap();
        assert!(out.approved && !out.always, "{out:?}");
    }

    /// H2 取消语义：即便 auto_confirm 开启，run 取消也把审批按拒绝收口
    ///（绝不能落在自动允许的结果上）。
    #[tokio::test(start_paused = true)]
    async fn cancel_beats_auto_confirm_timeout() {
        let (_ws, _dd, core, rt) = setup();
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: true,
            auto_confirm: true,
        };
        let (task, _ask_id) = spawn_confirm(&core, &rt, req, cancel.clone()).await;
        cancel.cancel();
        let out = task.await.unwrap();
        assert!(
            !out.approved && !out.always,
            "run cancel must count as rejection, not auto-allow: {out:?}"
        );
    }

    /// [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：auto_confirm 开启时，5 分钟窗口内到达的显式应答优先于之后的自动确认
    ///（拒绝仍是拒绝）。
    #[tokio::test(start_paused = true)]
    async fn explicit_answer_before_timeout_wins_over_auto_confirm() {
        let (_ws, _dd, core, rt) = setup();
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: true,
            auto_confirm: true,
        };
        let (task, ask_id) = spawn_confirm(&core, &rt, req, cancel).await;
        rt.resolve_ask(&ask_id, json!({ "approved": false }));
        let out = task.await.unwrap();
        assert!(!out.approved && !out.always, "explicit deny must be honored: {out:?}");
    }

    /// [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：畸形应答载荷 fail closed（approved 兜底为 false）。
    #[tokio::test]
    async fn malformed_answer_fails_closed() {
        let (_ws, _dd, core, rt) = setup();
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: true,
            auto_confirm: false,
        };
        let (task, ask_id) = spawn_confirm(&core, &rt, req, cancel).await;
        rt.resolve_ask(&ask_id, json!({ "bogus": "payload" }));
        let out = task.await.unwrap();
        assert!(!out.approved && !out.always, "{out:?}");
    }

    /// 方案 B：主会话审批事件挂自身下发，无 sub_id；子代理审批事件挂主会话下发，
    /// 携带 sub_id——嵌套时归并到主会话（root_session_id 链）。
    #[test]
    fn emit_session_of_routes_sub_to_root() {
        let (_ws, _dd, _core, rt) = setup();
        assert_eq!(super::emit_session_of(&rt), (rt.id.clone(), None), "主会话自指");

        let (_ws2, _dd2, core2, rt2) = setup();
        let sub = crate::core::agent::SessionRuntime::new_sub(&rt2, "sub_route1".into());
        assert_eq!(
            super::emit_session_of(&sub),
            (rt2.id.clone(), Some("sub_route1".into())),
            "一级子代理挂父会话"
        );
        let subsub = crate::core::agent::SessionRuntime::new_sub(&sub, "sub_route2".into());
        assert_eq!(
            super::emit_session_of(&subsub),
            (rt2.id.clone(), Some("sub_route2".into())),
            "嵌套子代理归并到主会话"
        );
        let _ = core2;
    }

    /// 方案 C：子代理审批恒有界等待——即便 auto_confirm 关闭，超过 SUB_APPROVAL_TIMEOUT
    /// 未响应也自动拒绝收口（start_paused 虚拟时钟免真实等待）。
    #[tokio::test(start_paused = true)]
    async fn sub_approval_times_out_to_deny_when_no_answer() {
        let (_ws, _dd, core, rt) = setup();
        let sub = crate::core::agent::SessionRuntime::new_sub(&rt, "sub_timeo1".into());
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: false,
            auto_confirm: false, // 主会话不勾 auto_confirm = 永不超时；子代理例外
        };
        let (task, ask_id) = spawn_confirm(&core, &sub, req, cancel).await;
        // 虚拟时钟推进到超时点之后（600s < 601s）；无应答 → 自动拒绝
        tokio::time::sleep(std::time::Duration::from_secs(601)).await;
        assert!(
            !sub.asks.lock().unwrap().contains_key(&ask_id),
            "超时后 asks 表必须收口，不留悬空 waiter"
        );
        let out = task.await.unwrap();
        assert!(!out.approved && !out.always, "子代理审批超时必须拒绝：{out:?}");
    }

    /// 方案 C 边界：子代理审批在超时窗口内收到应答，应答优先于超时（拒绝仍是拒绝）。
    #[tokio::test(start_paused = true)]
    async fn sub_approval_answer_within_window_wins() {
        let (_ws, _dd, core, rt) = setup();
        let sub = crate::core::agent::SessionRuntime::new_sub(&rt, "sub_timeo2".into());
        let cancel = tokio_util::sync::CancellationToken::new();
        let req = ApprovalRequest {
            title: "t".into(),
            detail: "d".into(),
            allow_always: false,
            auto_confirm: false,
        };
        let (task, ask_id) = spawn_confirm(&core, &sub, req, cancel).await;
        sub.resolve_ask(&ask_id, json!({ "approved": false }));
        let out = task.await.unwrap();
        assert!(!out.approved && !out.always, "{out:?}");
    }

    /// [docs/arithmetic-fixes-batch](../../../docs/arithmetic-fixes-batch.md)：auto-confirm 超时绝不适用于灾难级 Confirm——审批开启时
    /// `mkfs`/`dd of=/dev/` 即便勾了 auto_confirm 也要等用户亲自应答。
    #[test]
    fn effective_auto_confirm_never_applies_to_disaster() {
        assert!(super::effective_auto_confirm(true, false));
        assert!(!super::effective_auto_confirm(false, false));
        assert!(
            !super::effective_auto_confirm(true, true),
            "灾难级 Confirm 不得因超时被自动允许"
        );
        assert!(!super::effective_auto_confirm(false, true));
    }
}
