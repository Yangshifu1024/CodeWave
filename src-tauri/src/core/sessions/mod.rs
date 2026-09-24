//! 会话持久化（[docs/p0-plan](../../../../docs/p0-plan.md) §8）：纯文件 + gzip，不引入数据库。
//! 保存 = sanitize → trim → repair → 原子写；加载 = gunzip → repair → trim。

pub mod cleanup;
pub mod interrupt;
pub mod repair;
// 工具结果原样 sidecar（历史里只有模型侧瘦身文本，恢复需要完整出参）
pub mod tool_results;

mod store;

pub use cleanup::{CleanupOutcome, CleanupPreview, CleanupStatus};
pub use store::{ArtifactKind, ArtifactOp, SessionMeta, SessionStore};
