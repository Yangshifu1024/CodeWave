//! 交付与验收域（[docs/tools-and-safety](../../../../docs/tools-and-safety.md)）。
//!
//! 这一层回答一个此前无处安放的问题：**「任务做完了吗」凭什么说了算**。
//! 答案不是运行终态、不是子代理的 `<report>`、不是进程退出 0、也不是 wait 命中，而是
//! 逐条需求在**宿主已采纳的不可变证据**上成立与否。
//!
//! 模块分工（与任务文档的所有权约定一致，**不重复定义**）：
//!
//! | 模块 | 归属 | 职责 |
//! |---|---|---|
//! | [`types`] | #54 | 公共模型：契约 / 需求 / 检查 / 回执 / 交付结果 / 验收快照 |
//! | [`store`] | #54 | 版本化边车、不可变记录、CAS 索引与崩溃对账 |
//! | [`acceptance`] | #54 | 逐项求值与整体归约 |
//! | [`version`] | #15 | 范围版本采集与新鲜度判定 |
//! | [`artifacts`] | #12 | 声明与真实改动的逐条核对 |
//! | [`recorder`] | #14 | 执行回执签发（唯一可签发入口） |
//! | [`responsibility`] | #9 / #10 / #11 | 职责分离、独立审查身份、干净验证上下文 |
//! | [`regression`] | #13 | 修复前失败证据（RED / GREEN 配对） |
//! | [`integration`] | #22 | 集成后验收（合体后重跑整体检查） |
//! | [`validation`] | #18 | 子交付宿主校验（固定七步顺序） |
//! | [`completion`] | #16 | 完成门裁决与有界修复预算 |
//! | [`service`] | #54 / #14 / #17 | 生产接线：建契约、签发回执、提交交付、求值 |
//!
//! 跨层纪律：`core/` 不依赖 tauri；[`service`] 的写接口只经 `host/commands/` 暴露，
//! 且**不提供 set_passed**——通过只能由求值推导。

pub mod acceptance;
pub mod artifacts;
pub mod completion;
pub mod integration;
pub mod recorder;
pub mod regression;
pub mod responsibility;
pub mod service;
pub mod store;
pub mod types;
pub mod validation;
pub mod version;

#[allow(unused_imports)]
pub use acceptance::{EvalContext, EvalOutcome, Ledger, evaluate, validate_contract_shape};
#[allow(unused_imports)]
pub use completion::{CompletionDecision, RepairBudget, gate};
#[allow(unused_imports)]
pub use recorder::ReceiptRecorder;
#[allow(unused_imports)]
pub use service::DeliveryService;
#[allow(unused_imports)]
pub use store::{DeliveryError, DeliveryIndex, DeliveryStore, WriteReceipt};
#[allow(unused_imports)]
pub use types::{
    AcceptanceItem, AcceptanceSnapshot, AcceptanceStatus, AcceptanceStatusKind, ArtifactConstraint,
    ArtifactKind, ArtifactRequirement, Blocker, BuiltinValidationRecord, ChangeKind, ChangeRef,
    CheckExpectation, CheckKind, CheckSpec, CommandSpec, DeliveryResult, EndedKind, EvidenceKind,
    EvidenceRef, ExecutionIntent, ExecutionOutcome, ExecutionReceipt, IncompleteItem, PathRef,
    Reason, ReceiptIssuer, Requirement, ReviewPolicy, ReviewResult, ReviewVerdict, RunIdentity,
    ScopeChange, ScopeChangeKind, SubmissionState, TaskContract, UserDecision, UserDecisionKind,
    ValidationFinding, ValidationOutcome, VerificationScope,
};
#[allow(unused_imports)]
pub use validation::{DeliveryValidation, DeliveryVerdict, validate_delivery};
#[allow(unused_imports)]
pub use version::{Freshness, RootResolver, ScopeVersion, VersionCompleteness, VersionEntry};

/// 契约创建时宿主为任务分配的 id 前缀（id 全部宿主生成，路径不拼用户输入）。
pub const TASK_ID_PREFIX: &str = "task";
