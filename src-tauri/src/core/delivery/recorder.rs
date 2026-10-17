//! 执行回执签发器（任务 #14 拥有）。
//!
//! 它是**唯一**能把"命令真的跑了、跑了什么、退出码多少"变成不可伪造记录的入口。
//! 模型够不到它：`DeliveryService` 的签发接口只经宿主调用链抵达，没有对应工具。
//!
//! 生命周期与 [#54](../../../../docs/tools-and-safety.md) 的事务顺序一致：
//!
//! ```text
//! begin()                 请求已受理、结果未定     → ExecutionIntent
//!   ↓ 准入早返回
//! deny(code, message)     从未启动                → outcome = denied
//!   ↓ 通过准入
//! complete(...)           真实 spawn / 退出        → outcome = exited | spawn_failed | …
//! ```
//!
//! 三条纪律：
//!
//! 1. **入参解析失败不建意向**：命令都没读出来，就没有"可识别的请求"，
//!    捏造一个意向等于凭空造一条执行记录。
//! 2. **被拒记录不填 program/argv/cwd**：`denied` 走 `Requested` 形态，`resolved` / `cwd`
//!    留 `None`——不用默认目录补值，也不从 shell 文本反推未发生的子命令。
//! 3. **签发失败不改变工具真实结果**：工具已经跑完就是跑完了。写盘失败只作为
//!    `warnings` / `data.status = "unpersisted"` 上报，**绝不**把一次真实成功的命令
//!    改成失败，也绝不把写失败映射成"通过"。

use super::service::DeliveryService;
use super::types::{CommandSpec, ExecutionOutcome, ExecutionReceipt, PathRef, RunIdentity};
use super::version::ScopeVersion;

/// 回执签发器。由真实执行点（命令工具 / postcheck）构造。
pub struct ReceiptRecorder {
    /// 交付服务（`AgentCore` 持有的单实例；Arc 共享，不另建数据根）。
    svc: std::sync::Arc<DeliveryService>,
    /// 执行身份（由 [`DeliveryService::identity_of`] 从真实运行时补齐）。
    identity: RunIdentity,
    /// 操作 id：以 provider 侧 `tool_use.id` 为键（批内 `batch:index` 不可持久）。
    operation_id: String,
    /// 关联的**预登记**检查；普通命令为 `None`（不接受命令文本自称的检查身份）。
    check_id: Option<String>,
    /// 有界脱敏的请求命令。
    requested_command: String,
    /// 有界脱敏的请求 cwd（**不可**当实际 cwd 用）。
    requested_cwd: Option<String>,
    /// 请求超时。
    requested_timeout_ms: Option<u64>,
    /// 已签发回执的 id（供工具结果回填）。
    receipt_id: Option<String>,
    /// 非致命提示（截断、保存失败、软线…）。
    warnings: Vec<String>,
    binding: Option<super::types::ContractBinding>,
    capture: Option<(
        super::version::RootResolver,
        super::types::VerificationScope,
        Option<ScopeVersion>,
    )>,
}

impl ReceiptRecorder {
    /// 构造签发器。**不**写任何东西——调 [`Self::begin`] 才落 intent。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        svc: std::sync::Arc<DeliveryService>,
        identity: RunIdentity,
        provider_call_id: &str,
        requested_command: &str,
        requested_cwd: Option<String>,
        requested_timeout_ms: Option<u64>,
        check_id: Option<String>,
    ) -> Self {
        ReceiptRecorder {
            svc,
            identity: identity.clone(),
            operation_id: format!(
                "cmd_{}_{}_{}",
                identity.root_run_id, identity.actor_id, provider_call_id
            ),
            check_id,
            requested_command: bound(requested_command),
            requested_cwd: requested_cwd.as_deref().map(bound),
            requested_timeout_ms,
            receipt_id: None,
            warnings: Vec::new(),
            capture: None,
            binding: None,
        }
    }

    pub fn bind_contract(&mut self, contract: &super::types::TaskContract) {
        self.binding = Some(super::types::ContractBinding::of(contract));
    }

    /// 显式关联一个**已登记**的检查 id。
    ///
    /// 只有宿主核验过调用上下文才可传；模型在命令文本里自称"这是 check X"不构成依据
    /// ——本方法不接受来自工具入参的值。
    #[must_use]
    pub fn with_check(mut self, check_id: impl Into<String>) -> Self {
        self.check_id = Some(check_id.into());
        self
    }

    /// Attach a host-validated check scope before command admission.
    pub fn observe(
        &mut self,
        roots: super::version::RootResolver,
        scope: super::types::VerificationScope,
    ) {
        let before = super::version::capture_scope(&roots, &scope, &Default::default()).ok();
        self.capture = Some((roots, scope, before));
    }

    /// 执行身份（调用方需要把它并进工具结果）。
    pub fn identity(&self) -> &RunIdentity {
        &self.identity
    }

    /// 操作 id（幂等键）。
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// 已签发的回执 id（尚未签发为 `None`）。
    pub fn receipt_id(&self) -> Option<&str> {
        self.receipt_id.as_deref()
    }

    /// 取走并清空提示。
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// 落执行意向。写失败只记提示——意向本身不构成"命令已执行"。
    pub fn begin(&mut self) {
        if let Err(e) = self.svc.begin_execution_intent(
            &self.identity,
            &self.requested_command,
            self.requested_cwd.as_deref(),
            self.requested_timeout_ms,
            self.check_id.as_deref(),
            &self.operation_id,
        ) {
            self.warnings.push(format!(
                "执行意向未能落盘（{}），不影响本次工具结果",
                e.code()
            ));
        }
    }

    /// 准入早返回：命令**从未启动**。
    ///
    /// 记录里 `resolved` / `cwd` 保持 `None`，原因落 `reason`——
    /// 调用者据此区分 `invalid_cwd` / `fence_block` / `approval_denied`，
    /// 不会把"没跑"记成"跑了但失败"。
    pub fn deny(&mut self, reason: &str) {
        let requested = CommandSpec::Requested {
            command_text: self.requested_command.clone(),
            shell_hint: None,
        };
        let message = format!("{reason}: {}", self.requested_command);
        match self.svc.finish_execution_bound(
            &self.identity,
            &self.operation_id,
            requested,
            None,
            None,
            ExecutionOutcome::Denied,
            None,
            None,
            None,
            None,
            None,
            Some(message),
            self.check_id.clone().as_deref(),
            self.binding.clone(),
        ) {
            Ok((r, w)) => {
                self.receipt_id = Some(r.id);
                self.warnings.extend(w.warnings);
            }
            Err(e) => self.warnings.push(format!(
                "拒绝记录未能落盘（{}）：命令未执行，不构成任何通过证据",
                e.code()
            )),
        }
    }

    /// 真实执行之后落回执。
    ///
    /// `resolved` 只有真实解析成功才给；`cwd` 只有成功解析才给。`output` 是原始输出，
    /// 落盘预算与截断标记由 [`DeliveryService::finish_execution`] 处理。
    #[allow(clippy::too_many_arguments)]
    pub fn complete(
        &mut self,
        resolved: Option<CommandSpec>,
        cwd: Option<PathRef>,
        outcome: ExecutionOutcome,
        exit_code: Option<i32>,
        signal: Option<String>,
        output: Option<Vec<u8>>,
        before: Option<ScopeVersion>,
        after: Option<ScopeVersion>,
        reason: Option<String>,
    ) -> Option<ExecutionReceipt> {
        let (before, after) = if let Some((roots, scope, captured)) = &self.capture {
            (
                captured.clone(),
                super::version::capture_scope(roots, scope, &Default::default()).ok(),
            )
        } else {
            (before, after)
        };
        let requested = CommandSpec::Requested {
            command_text: self.requested_command.clone(),
            shell_hint: resolved.as_ref().and_then(|r| match r {
                CommandSpec::Resolved { shell_family, .. } => shell_family.clone(),
                CommandSpec::Requested { .. } => None,
            }),
        };
        match self.svc.finish_execution_bound(
            &self.identity,
            &self.operation_id,
            requested,
            resolved,
            cwd,
            outcome,
            exit_code,
            signal,
            output.as_deref(),
            before,
            after,
            reason,
            self.check_id.clone().as_deref(),
            self.binding.clone(),
        ) {
            Ok((r, w)) => {
                self.receipt_id = Some(r.id.clone());
                self.warnings.extend(w.warnings);
                Some(r)
            }
            Err(e) => {
                // 工具结果**已经发生**：如实上报未保存，不改判成败，更不当成通过。
                self.warnings.push(format!(
                    "执行回执未能落盘（{}）：本次执行事实保留在工具结果中，但不可作为已验收证据",
                    e.code()
                ));
                None
            }
        }
    }
}

/// Remove labelled credentials before evidence reaches disk. Unlabelled secrets still need callers to avoid printing them.
pub fn redact(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    for label in [
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "access-token",
        "token",
        "password",
        "secret",
        "authorization",
    ] {
        for (offset, _) in lower.match_indices(label) {
            let mut pos = offset + label.len();
            let bytes = text.as_bytes();
            while pos < bytes.len()
                && (bytes[pos].is_ascii_whitespace() || matches!(bytes[pos], b'"' | b'\''))
            {
                pos += 1;
            }
            if pos >= bytes.len() || !matches!(bytes[pos], b':' | b'=') {
                continue;
            }
            pos += 1;
            while pos < bytes.len()
                && (bytes[pos].is_ascii_whitespace() || matches!(bytes[pos], b'"' | b'\''))
            {
                pos += 1;
            }
            if label == "authorization" && lower[pos..].starts_with("bearer ") {
                pos += 7;
            }
            let start = pos;
            while pos < bytes.len()
                && !bytes[pos].is_ascii_whitespace()
                && !matches!(bytes[pos], b'"' | b'\'' | b',' | b';')
            {
                pos += 1;
            }
            if pos > start {
                spans.push((start, pos));
            }
        }
    }
    spans.sort_unstable();
    spans.dedup();
    let mut out = String::new();
    let mut cursor = 0;
    for (start, end) in spans {
        if start >= cursor {
            out.push_str(&text[cursor..start]);
            out.push_str("[REDACTED]");
            cursor = end;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// 有界脱敏（与 store 同口径：入参文本进边车前必须截断）。
fn bound(s: &str) -> String {
    let redacted = redact(s);
    let s = redacted.as_str();
    const MAX: usize = 512;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX).collect();
    out.push('…');
    out
}

#[cfg(test)]
#[path = "recorder_tests.rs"]
mod tests;
