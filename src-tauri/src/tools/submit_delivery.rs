//! `submit_delivery` 工具（任务 #17，[docs/subagents](../../../../docs/subagents.md)）。
//!
//! 子代理用**结构化草稿**声明"做完了什么、还差什么、动了哪些文件、被什么挡住"。
//! 旧 `<report>` 文本**保留**用于收尾展示，但**不再独立满足结构化交付要求**——
//! 文本说"全部完成"而没有结构化记录，就是没完成。
//!
//! 三条硬边界：
//!
//! 1. **仅子 runtime 可提交自己的 root_run**：根身份调用直接拒绝；跨 run 的迟到提交
//!    因 `root_run_id` 不匹配被拒（[`DeliveryService::submit_delivery`] 内校验）。
//! 2. **草稿只落草稿，落盘由宿主在子代理终态统一执行**。`ended` 由
//!    [`ended_of`](crate::tools::subagent::ended_of) 在收尾那一刻按真实步数与报告标记判定，
//!    `owner` / `id` / `task_id` / `contract_revision` 同理——本工具一个都不接受模型指定。
//! 3. **完整性状态独立于 `SubEnded` 四象限**：预算尾部生成的有效草稿**不会**把
//!    `partial` 升格为完整交付；完整性由 [`SubmissionState`] 单独表达。

use super::{Tool, ToolCtx, ToolKind, ToolOutcome};
use crate::core::delivery::types::{ChangeKind, ChangeRef, IncompleteItem, PathRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 结构化交付草稿（工具写入、宿主在终态消费）。
///
/// 存在**子 runtime**上而不是全局：草稿属本次子任务，不跨 run、不跨子代理。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeliveryDraft {
    /// 绑定的契约修订（声明时所见）。
    pub contract_revision: u64,
    /// 声称完成的 id。
    #[serde(default)]
    pub completed: Vec<String>,
    /// 未完成项。
    #[serde(default)]
    pub incomplete: Vec<IncompleteItem>,
    /// 声明的变更。
    #[serde(default)]
    pub changes: Vec<ChangeRef>,
    /// 自述用到的证据 id。
    #[serde(default)]
    pub evidence: Vec<String>,
    /// 阻塞。
    #[serde(default)]
    pub blockers: Vec<crate::core::delivery::types::Blocker>,
}

/// 入参（全部可选：只交 completed 而不写 blockers 是合法收尾）。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Args {
    /// 声称已完成的需求 / 产物 id 列表。
    #[serde(default)]
    completed: Vec<String>,
    /// 未完成项及其原因。
    #[serde(default)]
    incomplete: Vec<IncompleteIn>,
    /// 声明的变更（宿主会拿回真实文件系统核对，模型给的哈希不作证明）。
    #[serde(default)]
    changes: Vec<ChangeIn>,
    /// 自述用到的证据 id（必须是宿主已采纳的记录，否则验收器不认）。
    #[serde(default)]
    evidence: Vec<String>,
    /// 阻塞。
    #[serde(default)]
    blockers: Vec<BlockerIn>,
    /// 声明时的契约修订；与派发绑定的不一致时按过期拒绝。
    #[serde(default)]
    contract_revision: Option<u64>,
}

/// 未完成项入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncompleteIn {
    /// 目标 id。
    target_id: String,
    /// 原因。
    reason: String,
}

/// 变更入参（`rootId` 必须命中会话快照根，否则拒绝解析）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeIn {
    /// 根 id。
    root_id: String,
    /// 根内相对路径。
    path: String,
    /// 变更类别：`created` / `modified` / `deleted`。
    kind: String,
    /// 变更前哈希（删除声明必填）。
    #[serde(default)]
    before_hash: Option<String>,
    /// 变更后哈希。
    #[serde(default)]
    after_hash: Option<String>,
}

/// 阻塞入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockerIn {
    /// 阻塞目标 id。
    target_id: String,
    /// 阻塞类别。
    code: String,
    /// 说明。
    message: String,
}

/// 规模上限（子代理声明不是无限清单）。
pub const MAX_ITEMS: usize = 100;

/// 工具名常量：工具自身的 `name()` 与主会话排除集共用，避免两处拼写分叉。
pub const SUBMIT_DELIVERY_TOOL: &str = "submit_delivery";

/// `submit_delivery` 工具。
pub struct SubmitDeliveryTool;

#[async_trait::async_trait]
impl Tool for SubmitDeliveryTool {
    fn name(&self) -> &'static str {
        SUBMIT_DELIVERY_TOOL
    }

    fn description(&self) -> &'static str {
        "结构化提交本次任务的交付草稿（仅子代理可用）：声明完成项、未完成项、实际改动、证据与阻塞。\
         宿主会逐项核对契约引用与真实文件状态；引用不合法或状态矛盾时立刻拒绝，\
         不会因为这里写了 completed 就判定任务完成。文本 <report> 仍用于收尾展示，但不单独满足交付验收。"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "completed": {"type": "array", "items": {"type": "string"}, "description": "声称完成的需求/产物 id（须在本次派发绑定的契约内）"},
    "incomplete": {"type": "array", "description": "未完成项，必须写明原因", "items": {"type": "object", "additionalProperties": false, "properties": {"targetId": {"type": "string"}, "reason": {"type": "string"}}}},
    "changes": {"type": "array", "description": "本轮实际改动（宿主按真实内容核对）", "items": {"type": "object", "additionalProperties": false, "properties": {"rootId": {"type": "string"}, "path": {"type": "string"}, "kind": {"type": "string", "enum": ["created", "modified", "deleted"]}, "beforeHash": {"type": "string"}, "afterHash": {"type": "string"}}}},
    "evidence": {"type": "array", "items": {"type": "string"}, "description": "用到的证据记录 id（receipt_id / 校验记录 id）"},
    "blockers": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {"targetId": {"type": "string"}, "code": {"type": "string"}, "message": {"type": "string"}}}},
    "contractRevision": {"type": "integer", "description": "声明时看到的契约修订；与派发绑定的不一致会被判过期"}
  }
}"#
    }

    fn kind(&self) -> ToolKind {
        // Meta：操作 agent 自身状态（交付草稿），不触工作区、不触网络。
        ToolKind::Meta
    }

    async fn run(&self, ctx: &ToolCtx, args: Value) -> ToolOutcome {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutcome::err("E_ARGS", format!("参数解析失败：{e}")),
        };
        // 仅子 runtime：根身份没有 root_run 可归属，结构化提交无意义。
        let task_id = ctx.rt.delivery_task_id.lock().unwrap().clone();
        let Some(task_id) = task_id else {
            return ToolOutcome::err(
                "E_DELIVERY_NOT_AVAILABLE",
                "本次派发未绑定交付契约（问答 / 机械修改 / 旧任务），结构化交付不可用；按 <report> 收尾即可",
            );
        };
        let svc = ctx.core.delivery.clone();
        let identity = crate::core::delivery::DeliveryService::identity_of(&ctx.rt);
        let Some(contract) = svc.read_contract(&identity.session_id, &task_id) else {
            return ToolOutcome::err(
                "E_DELIVERY_NOT_FOUND",
                format!("绑定的契约 {task_id} 不存在或未入索引"),
            );
        };
        // 修订漂移：按旧修订交付的结果不能满足新契约。
        if let Some(rev) = args.contract_revision
            && rev != contract.revision
        {
            return ToolOutcome::err(
                "E_CONTRACT_STALE",
                format!(
                    "提交声明的修订 {rev} 与当前契约修订 {} 不一致，请按当前契约重新提交",
                    contract.revision
                ),
            );
        }
        if args.completed.len() > MAX_ITEMS
            || args.incomplete.len() > MAX_ITEMS
            || args.changes.len() > MAX_ITEMS
        {
            return ToolOutcome::err(
                "E_CONTRACT_INVALID",
                format!("提交项超过上限 {MAX_ITEMS}，请精简后重试"),
            );
        }
        // 引用闭包：完成 / 未完成的 id 必须在契约内，且不能既完成又未完成。
        let mut problems: Vec<String> = Vec::new();
        let known =
            |id: &str| contract.requirement(id).is_some() || contract.artifact(id).is_some();
        for id in &args.completed {
            if !known(id) {
                problems.push(format!("完成项 `{id}` 不在契约内"));
            }
        }
        for i in &args.incomplete {
            if !known(&i.target_id) {
                problems.push(format!("未完成项 `{}` 不在契约内", i.target_id));
            }
            if args.completed.contains(&i.target_id) {
                problems.push(format!("`{}` 同时列为完成与未完成", i.target_id));
            }
            if i.reason.trim().is_empty() {
                problems.push(format!("未完成项 `{}` 缺少原因", i.target_id));
            }
        }
        let mut changes: Vec<ChangeRef> = Vec::with_capacity(args.changes.len());
        for c in &args.changes {
            let Some(kind) = change_kind(&c.kind) else {
                return ToolOutcome::err(
                    "E_ARGS",
                    format!("未知变更类别 `{}`（created/modified/deleted）", c.kind),
                );
            };
            changes.push(ChangeRef {
                path: PathRef {
                    root_id: c.root_id.clone(),
                    relative_path: c.path.clone(),
                },
                before_hash: c.before_hash.clone(),
                after_hash: c.after_hash.clone(),
                kind,
            });
        }
        if !problems.is_empty() {
            return ToolOutcome::err("E_DELIVERY_INVALID", problems.join("；"));
        }
        let draft = DeliveryDraft {
            contract_revision: contract.revision,
            completed: args.completed,
            incomplete: args
                .incomplete
                .into_iter()
                .map(|i| IncompleteItem {
                    target_id: i.target_id,
                    reason: i.reason,
                })
                .collect(),
            changes,
            evidence: args.evidence,
            blockers: args
                .blockers
                .into_iter()
                .map(|b| crate::core::delivery::types::Blocker {
                    target_id: b.target_id,
                    code: b.code,
                    message: b.message,
                })
                .collect(),
        };
        // 草稿尚未落盘：完整性此刻是 unknown，不是 valid。
        // 宿主在收尾时按真实 `ended` 复核后才落成不可变记录（`SubmissionState`）。
        let uncovered: Vec<&str> = contract
            .requirements
            .iter()
            .filter(|r| r.required)
            .filter(|r| {
                !draft.completed.contains(&r.id)
                    && !draft.incomplete.iter().any(|i| i.target_id == r.id)
            })
            .map(|r| r.id.as_str())
            .collect();
        *ctx.rt.delivery_draft.lock().unwrap() = Some(draft);
        ToolOutcome::ok(json!({
            "accepted": true,
            "task_id": task_id,
            "contract_revision": contract.revision,
            // 明确告诉模型：草稿不等于完成，落盘与验收由宿主决定。
            "submission_state": "draft_recorded",
            "uncovered_required": uncovered,
        }))
    }
}

/// 变更类别解析（未知类型拒绝，不静默忽略）。
fn change_kind(s: &str) -> Option<ChangeKind> {
    match s {
        "created" => Some(ChangeKind::Created),
        "modified" => Some(ChangeKind::Modified),
        "deleted" => Some(ChangeKind::Deleted),
        _ => None,
    }
}

#[cfg(test)]
#[path = "submit_delivery_tests.rs"]
mod tests;
