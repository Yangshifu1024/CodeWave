//! 只读 git 集成（G8/D5）：git2-rs（libgit2）。P0 仅覆盖 status；diff/log 随 P1 落地。

use serde::Serialize;
use std::path::Path;

/// 单个文件的 git 状态条目：index 与 worktree 两个维度的新增/修改/删除各自独立标记。
#[derive(Debug, Clone, Serialize)]
pub struct GitStatusEntry {
    /// 仓库内相对路径
    pub path: String,
    /// index 中新增（已暂存的新文件）
    pub index_new: bool,
    /// index 中修改
    pub index_modified: bool,
    /// index 中删除
    pub index_deleted: bool,
    /// worktree 中新增（未跟踪文件）
    pub worktree_new: bool,
    /// worktree 中修改
    pub worktree_modified: bool,
    /// worktree 中删除
    pub worktree_deleted: bool,
    /// 重命名（index 或 worktree 任一维度）
    pub renamed: bool,
}

/// git status（≤500 条）。目录不是仓库时返回 Ok(None)。
#[derive(Debug, Clone, Serialize)]
pub struct StatusInfo {
    /// 当前分支（HEAD 分离时为短哈希）
    pub branch: String,
    /// 状态条目列表（未跟踪目录递归展开、排除子模块与忽略文件）
    pub entries: Vec<GitStatusEntry>,
}

/// 读取工作区的 git status：非仓库（无 .git）返回 None，其余经 git2 全量扫描。
pub fn status(workspace: &Path) -> anyhow::Result<Option<StatusInfo>> {
    if !workspace.join(".git").exists() {
        return Ok(None);
    }
    let repo =
        git2::Repository::open(workspace).map_err(|e| anyhow::anyhow!("打开仓库失败：{e}"))?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(|s| s.to_string()))
        .unwrap_or_else(|| "HEAD".into());
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut out = Vec::new();
    for entry in statuses.iter().take(500) {
        let s = entry.status();
        let path = entry.path().map(|p| p.to_string()).unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        out.push(GitStatusEntry {
            path,
            index_new: s.contains(git2::Status::INDEX_NEW),
            index_modified: s.contains(git2::Status::INDEX_MODIFIED),
            index_deleted: s.contains(git2::Status::INDEX_DELETED),
            worktree_new: s.contains(git2::Status::WT_NEW),
            worktree_modified: s.contains(git2::Status::WT_MODIFIED),
            worktree_deleted: s.contains(git2::Status::WT_DELETED),
            renamed: s.intersects(git2::Status::INDEX_RENAMED | git2::Status::WT_RENAMED),
        });
    }
    Ok(Some(StatusInfo {
        branch,
        entries: out,
    }))
}

// ---------- P1：diff / 最近 log（[docs/p1-plan](../../../docs/p1-plan.md) §6.4） ----------

/// 单文件 diff 摘要：状态 + 增删行数 + 完整 patch 文本。
#[derive(Debug, Clone, Serialize)]
pub struct GitDiffFile {
    /// 文件路径（优先新路径，缺失时退回旧路径）
    pub path: String,
    /// 变更状态（added / deleted / renamed / modified）
    pub status: String,
    /// 新增行数
    pub additions: usize,
    /// 删除行数
    pub deletions: usize,
    /// 完整 patch 文本（含行首 origin 字符）
    pub patch: String,
    /// 多根项目：归属的根目录（单根/临时会话为 None）
    #[serde(default)]
    pub root: Option<String>,
}

/// 单条提交记录（最近 log 视图）。
#[derive(Debug, Clone, Serialize)]
pub struct GitLogEntry {
    /// 短哈希（前 7 位）
    pub short_id: String,
    /// 提交摘要（首行）
    pub summary: String,
    /// 作者名
    pub author: String,
    /// 提交时间（Unix 秒）
    pub time: i64,
    /// 多根项目：归属的根目录（单根/临时会话为 None）
    #[serde(default)]
    pub root: Option<String>,
}

/// diff 文件数上限（防御超大仓库拖垮 UI）。
const MAX_FILES: usize = 100;

/// worktree 对 HEAD 的 diff（含 index 暂存变更），最多返回 MAX_FILES 个文件。
pub fn diff_workspace(workspace: &Path) -> anyhow::Result<Vec<GitDiffFile>> {
    let repo =
        git2::Repository::open(workspace).map_err(|e| anyhow::anyhow!("打开仓库失败：{e}"))?;
    let head_tree = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .and_then(|c| c.tree())
        .ok();
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3)
        .include_untracked(true)
        .recurse_untracked_dirs(true);
    let diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
    let mut out = Vec::new();
    let n = diff.deltas().len();
    for i in 0..n.min(MAX_FILES) {
        let delta = diff
            .get_delta(i)
            .ok_or_else(|| anyhow::anyhow!("delta 不存在"))?;
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let status = match delta.status() {
            git2::Delta::Added | git2::Delta::Untracked => "added",
            git2::Delta::Deleted => "deleted",
            git2::Delta::Renamed => "renamed",
            _ => "modified",
        };
        let mut additions = 0usize;
        let mut deletions = 0usize;
        let mut patch_text = String::new();
        if let Ok(Some(mut p)) = git2::Patch::from_diff(&diff, i) {
            let mut buf = String::new();
            // line_stats：(上下文行数, 新增, 删除)
            if let Ok((_ctx, add, del)) = p.line_stats() {
                additions = add;
                deletions = del;
            }
            p.print(&mut |_d, _h, line: git2::DiffLine| -> bool {
                buf.push(line.origin());
                buf.push_str(&String::from_utf8_lossy(line.content()));
                true
            })
            .ok();
            patch_text = buf;
        }
        out.push(GitDiffFile {
            path,
            status: status.into(),
            additions,
            deletions,
            patch: patch_text,
            root: None,
        });
    }
    Ok(out)
}

/// 最近 n 条提交（按时间倒序）。
pub fn recent_log(workspace: &Path, n: usize) -> anyhow::Result<Vec<GitLogEntry>> {
    let repo =
        git2::Repository::open(workspace).map_err(|e| anyhow::anyhow!("打开仓库失败：{e}"))?;
    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    walk.set_sorting(git2::Sort::TIME)?;
    let mut out = Vec::new();
    for oid in walk.take(n.min(500)) {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        out.push(GitLogEntry {
            short_id: oid.to_string()[..7].to_string(),
            summary: commit.summary().ok().flatten().unwrap_or("").to_string(),
            author: commit.author().name().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            root: None,
        });
    }
    Ok(out)
}

/// git 提交身份（左下角身份条的数据源，只读）
#[derive(Debug, Clone, Serialize)]
pub struct GitUserInfo {
    /// user.name（未配置为 None）
    pub name: Option<String>,
    /// user.email（未配置为 None）
    pub email: Option<String>,
}

/// 读取 git 提交身份（user.name / user.email，只读）。workspace 为 Some 时打开该仓库的 config
/// 快照（repo > global > system 叠加，与 `git config` 生效语义一致）；None / 非仓库 / 打开失败
/// 回退全局默认 config（`Config::open_default`）。键缺失或读取失败返回 None，
/// 绝不向上抛错——左下角身份条静默降级（「未配置」占位）。
pub fn user_info(workspace: Option<&Path>) -> (Option<String>, Option<String>) {
    let config = match workspace {
        Some(dir) => git2::Repository::open(dir)
            .and_then(|repo| repo.config())
            .or_else(|_| git2::Config::open_default()),
        None => git2::Config::open_default(),
    };
    let read = |key: &str| -> Option<String> {
        config
            .as_ref()
            .ok()
            .and_then(|cfg| cfg.get_string(key).ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    (read("user.name"), read("user.email"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_on_temp_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("hello.txt"), b"hi").unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "t").unwrap();
        cfg.set_str("user.email", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("hello.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        // worktree 修改 + 新增文件
        std::fs::write(dir.path().join("hello.txt"), b"changed").unwrap();
        std::fs::write(dir.path().join("new.txt"), b"new").unwrap();

        let info = status(dir.path()).unwrap().unwrap();
        assert!(!info.branch.is_empty());
        assert!(info
            .entries
            .iter()
            .any(|e| e.path == "hello.txt" && e.worktree_modified));
        assert!(info
            .entries
            .iter()
            .any(|e| e.path == "new.txt" && e.worktree_new));
    }

    #[test]
    fn non_repo_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(status(dir.path()).unwrap().is_none());
    }

    #[test]
    fn diff_and_log_on_temp_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"line1\nline2\n").unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "t").unwrap();
        cfg.set_str("user.email", "t@t").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        // 修改 + 新增
        std::fs::write(dir.path().join("a.txt"), b"line1\nline2-changed\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"new file\n").unwrap();

        let files = diff_workspace(dir.path()).unwrap();
        assert!(
            files
                .iter()
                .any(|f| f.path == "a.txt" && f.additions == 1 && f.deletions == 1),
            "{files:?}"
        );
        assert!(files
            .iter()
            .any(|f| f.path == "b.txt" && f.status == "added"));
        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert!(a.patch.contains("-line2"));
        assert!(a.patch.contains("+line2-changed"));

        let log = recent_log(dir.path(), 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].summary, "init");
        assert_eq!(log[0].short_id.len(), 7);
    }

    #[test]
    fn user_info_reads_repo_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "张三").unwrap();
        cfg.set_str("user.email", "zhang@example.com").unwrap();

        let (name, email) = user_info(Some(dir.path()));
        assert_eq!(name.as_deref(), Some("张三"));
        assert_eq!(email.as_deref(), Some("zhang@example.com"));
    }

    #[test]
    fn user_info_silent_on_missing() {
        // 非 git 目录（回退全局 config）与全局路径：键缺失/配置缺失不得 panic 或报错（静默降级基线）
        let dir = tempfile::tempdir().unwrap();
        let _ = user_info(Some(dir.path()));
        let _ = user_info(None);
    }
}
