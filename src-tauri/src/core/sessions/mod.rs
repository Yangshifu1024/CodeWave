//! 会话持久化（[docs/p0-plan](../../../../docs/p0-plan.md) §8）：纯文件，不引入数据库。
//! 保存 = sanitize → repair → **分段 append-only JSONL**（不 trim、不整份改写）；
//! 加载 = 段回放 → repair → trim（wire 侧裁剪与落盘解耦）。

pub mod cleanup;
pub mod interrupt;
pub mod repair;
// 工具结果原样 sidecar（历史里只有模型侧瘦身文本，恢复需要完整出参）
pub mod tool_results;
// 图片外置存储与历史落盘 DTO（[docs/session-history-limits](../../../../docs/session-history-limits.md)）：
// 历史文件里只留 blob 引用，base64 原文落到 `sessions/<owner>.imgblob/`
pub(crate) mod image_blobs;
pub(crate) mod persist;
// 分段 append-only JSONL 历史存储（会话保存与恢复优化 · 批2 P1）：
// 段格式 / 增量水位与基线段 / 容错读取（半行、坏行、坏段一律跳过而不报错）
pub(crate) mod segments;

mod store;

pub use cleanup::{CleanupOutcome, CleanupPreview, CleanupStatus};
// `SaveReport` / `HistoryStatus` 是调用方契约（`core/agent/drive.rs` 的 checkpoint 返回值、
// 前端 `SessionMeta.history_status` 的载荷）：定义在 store.rs、经本模块转出。
// 转出是必要的（`mod store` 私有，外部只能走这里），而本 crate 的非测试代码尚未引用它们
// （由调用方接线时使用），故显式 allow，不新增一条「未使用导入」噪音。
#[allow(unused_imports)]
pub use segments::{HistoryBoundary, HistoryFormat, SegmentInfo};
#[allow(unused_imports)]
pub use store::{
    ArtifactKind, ArtifactOp, HistoryLoad, HistoryStatus, SaveReport, SessionMeta, SessionStore,
};
