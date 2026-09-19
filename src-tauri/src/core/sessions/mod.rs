//! 会话持久化（[docs/p0-plan](../../../../docs/p0-plan.md) §8）：纯文件 + gzip，不引入数据库。
//! 保存 = sanitize → trim → repair → 原子写；加载 = gunzip → repair → trim。

pub mod cleanup;
pub mod interrupt;
pub mod repair;

mod store;

pub use cleanup::{CleanupOutcome, CleanupPreview, CleanupStatus};
pub use store::{ArtifactKind, ArtifactOp, SessionMeta, SessionStore};
