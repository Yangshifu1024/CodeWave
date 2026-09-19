//! 进程级文件写认领表（[docs/subagent-file-isolation](../../../docs/subagent-file-isolation.md)）：
//! 非主 runtime（子代理/任务运行）经结构化写工具（create/edit/delete）写文件前必须认领目标路径；
//! 兄弟 runtime 写已认领路径（相同或祖先关系）被 E_FILE_CLAIMED 拒绝并指引「跳过 + 上报未完成」，
//! 杜绝并行子代理交叉编辑同一文件引发的版本冲突乒乓。主会话豁免——跨任务包共享的文件
//! （配置/锁文件/汇总导出/公共类型）由主代理亲自完成，这是隔离模型下共享文件的唯一合法出口。
//! 与 writelock 正交：writelock 串行化单次写操作（防 TOCTOU/后写覆盖），认领表锁定整个任务
//! 周期的文件范围（防语义冲突）。边界：shell/MCP 写不经认领表（路径无法静态归因）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static CLAIMS: OnceLock<Mutex<ClaimsTable>> = OnceLock::new();

/// 认领表：路径 → 持有者（runtime id）+ 持有者反向索引（释放用）。
#[derive(Default)]
struct ClaimsTable {
    by_path: HashMap<PathBuf, String>,
    by_owner: HashMap<String, HashSet<PathBuf>>,
}

fn table() -> &'static Mutex<ClaimsTable> {
    CLAIMS.get_or_init(|| Mutex::new(ClaimsTable::default()))
}

/// 认领冲突：被占路径 + 持有者（供错误文案指名）。
#[derive(Debug)]
pub struct ClaimConflict {
    pub path: PathBuf,
    pub holder: String,
}

/// 路径冲突判定：相同或互为祖先（组件级 starts_with 蕴含相等）——
/// 兄弟认领目录时其下文件同样不可写（delete 目录 vs 兄弟写目录内文件）。
fn path_conflicts(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// 认领 key：与 writelock 同源规范化，相对/绝对/符号链接别名落到同一条目。
fn key(p: &Path) -> PathBuf {
    crate::tools::pathutil::canonical_best_effort(p)
}

/// 为 `owner` 原子认领全部 `paths`：单锁内先全查后全插，任一路径与其他持有者的路径
/// 相同/呈祖先关系则整体失败（不产生部分认领），返回冲突清单；owner 自己已认领的
/// 路径幂等通过（同 runtime 对同一文件多次写合法）。
pub fn claim(owner: &str, paths: &[PathBuf]) -> Result<(), Vec<ClaimConflict>> {
    let keys: Vec<PathBuf> = paths.iter().map(|p| key(p)).collect();
    let mut t = table().lock().unwrap_or_else(|p| p.into_inner());
    let mut conflicts: Vec<ClaimConflict> = Vec::new();
    for k in &keys {
        if t.by_owner.get(owner).is_some_and(|own| own.contains(k)) {
            continue;
        }
        if let Some((p, holder)) = t
            .by_path
            .iter()
            .find(|(p, holder)| holder.as_str() != owner && path_conflicts(p, k))
        {
            conflicts.push(ClaimConflict {
                path: p.clone(),
                holder: holder.clone(),
            });
        }
    }
    if !conflicts.is_empty() {
        return Err(conflicts);
    }
    for k in keys {
        t.by_path.insert(k.clone(), owner.to_string());
        t.by_owner.entry(owner.to_string()).or_default().insert(k);
    }
    Ok(())
}

/// 释放 `owner` 的全部认领（drive 结束/panic unwind 时经 ReleaseGuard 调用）。
pub fn release(owner: &str) {
    let mut t = table().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(own) = t.by_owner.remove(owner) {
        for p in own {
            if t.by_path.get(&p).is_some_and(|h| h == owner) {
                t.by_path.remove(&p);
            }
        }
    }
}

/// 组装 E_FILE_CLAIMED 的模型侧文案：指名冲突文件与持有者，并给出终结性指引
/// （不得重试/等待——重试只会烧预算，跳过并上报未完成才是正确行为）。
pub fn denial_message(conflicts: &[ClaimConflict]) -> String {
    let list = conflicts
        .iter()
        .map(|c| format!("{}（持有者 {}）", c.path.display(), c.holder))
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "文件写被拒绝（严格文件隔离）：{list} 已被并行任务认领。此限制不因重试或等待解除：请把这部分从当前任务中剔除，在最终汇报「未完成」中列明，由主代理统一处理。"
    )
}

/// 认领随 drive 生命周期自动释放的 RAII 守卫：drive_agent 对非主 runtime 在开头武装，
/// 正常结束与 panic unwind 都经 Drop 释放（纯同步操作，无需 await）。
pub struct ReleaseGuard {
    owner: String,
}

impl ReleaseGuard {
    pub fn arm(owner: &str) -> Self {
        Self {
            owner: owner.to_string(),
        }
    }
}

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        release(&self.owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_conflict_atomic_and_owner_idempotent() {
        // owner 名各测试唯一：全局认领表进程级共享，release(owner) 会清空同名全部认领
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        claim("t1-owner", &[a.clone()]).unwrap();
        // 兄弟写已认领文件：整体拒绝（多路径任一冲突即全不认领）
        let err = claim("t1-other", &[b.clone(), a.clone()]).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].holder, "t1-owner");
        // 原子性：b 未被部分认领，t1-other 现在可整体认领 b
        claim("t1-other", &[b.clone()]).unwrap();
        // owner 自身重复认领幂等通过
        claim("t1-other", &[b.clone()]).unwrap();
        release("t1-owner");
        release("t1-other");
    }

    #[test]
    fn ancestor_paths_conflict_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("pkg");
        let f = d.join("mod.rs");
        claim("t2-dir", &[d.clone()]).unwrap();
        assert!(
            claim("t2-file", &[f.clone()]).is_err(),
            "认领目录后兄弟不可写其下文件"
        );
        release("t2-dir");
        claim("t2-dir", &[f.clone()]).unwrap();
        assert!(
            claim("t2-file", &[d.clone()]).is_err(),
            "兄弟不可认领包含他人文件的目录（delete 目录会波及他人文件）"
        );
        release("t2-dir");
    }

    #[test]
    fn release_allows_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.rs");
        claim("t3-a", &[f.clone()]).unwrap();
        release("t3-a");
        claim("t3-b", &[f.clone()]).unwrap();
        release("t3-b");
    }

    #[test]
    fn alias_paths_map_to_same_claim() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("y.rs");
        std::fs::write(&f, "").unwrap();
        let canonical = std::fs::canonicalize(&f).unwrap();
        claim("t4", &[f.clone()]).unwrap();
        assert!(
            claim("t4-other", &[canonical]).is_err(),
            "规范化别名应落到同一条目"
        );
        release("t4");
    }

    #[test]
    fn release_guard_drops_all_claims() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("z.rs");
        let guard = ReleaseGuard::arm("t5-guard");
        claim("t5-guard", &[f.clone()]).unwrap();
        drop(guard);
        claim("t5-other", &[f.clone()]).unwrap();
        release("t5-other");
    }

    #[test]
    fn denial_message_names_file_and_holder() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("w.rs");
        let msg = denial_message(&[ClaimConflict {
            path: f,
            holder: "sub-9".into(),
        }]);
        assert!(msg.contains("sub-9"));
        assert!(msg.contains("未完成"));
        assert!(msg.contains("不得重试") || msg.contains("不因重试"));
    }
}
