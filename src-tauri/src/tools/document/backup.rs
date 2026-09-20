//! 文档修改前的持久备份。
//!
//! 与 `edit` 工具的备份**不是一回事**：那个备份只在一次写入过程内存在、成功即删，
//! 用途是「写入中途失败时回滚」。这里的备份要**留下来**，用途是「用户事后发现改错了，
//! 能拿回改动前的原件」——保真修改动的是用户的真实文件，一次误改的代价很高。
//!
//! 存放位置与会话数据同目录（`<数据目录>/tmp/document-backup/`），随会话清理一并删除。
//! 同一份文件只保留最近若干份，避免反复修改把磁盘撑满。

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 同一份源文件保留的备份份数上限。
pub const KEEP_PER_FILE: usize = 5;

/// 备份目录。
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tmp").join("document-backup")
}

/// 源文件路径的稳定短标识（同名不同目录的文件不会互相覆盖备份）。
fn path_tag(src: &Path) -> String {
    let canonical = std::fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// 文件名里的时间戳（排序用，字典序即时间序）。
fn timestamp() -> String {
    chrono::Utc::now().format("%Y%m%d%H%M%S%3f").to_string()
}

/// 源文件名里可能有路径分隔符或奇怪的字符，压成一个安全的片段。
fn safe_basename(src: &Path) -> String {
    let raw = src
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string());
    raw.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 把 `data`（源文件当前内容）备份下来，返回备份文件路径。
///
/// 备份失败不算致命：调用方应记录告警后继续修改，而不是因为「备份没存上」就拒绝干活。
pub fn save(data_dir: &Path, src: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    let dir = dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}-{}-{}.bak",
        timestamp(),
        path_tag(src),
        safe_basename(src)
    ));
    std::fs::write(&path, data)?;
    prune(data_dir, src);
    Ok(path)
}

/// 列出某个源文件的全部备份（旧的在前）。
pub fn list_for(data_dir: &Path, src: &Path) -> Vec<PathBuf> {
    let tag = path_tag(src);
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir(data_dir))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().contains(&format!("-{tag}-")))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// 只保留最近 [`KEEP_PER_FILE`] 份。
pub fn prune(data_dir: &Path, src: &Path) {
    let all = list_for(data_dir, src);
    if all.len() <= KEEP_PER_FILE {
        return;
    }
    for old in &all[..all.len() - KEEP_PER_FILE] {
        let _ = std::fs::remove_file(old);
    }
}

/// 最近一份备份（供「回退到改动前」用）；没有备份返回 None。
pub fn latest_for(data_dir: &Path, src: &Path) -> Option<PathBuf> {
    list_for(data_dir, src).pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_creates_backup_with_original_bytes() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("表格.xlsx");
        std::fs::write(&src, b"original").unwrap();

        let path = save(data_dir.path(), &src, b"original").unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        // 文件名里保留原始扩展名，便于用户辨认
        assert!(path.to_string_lossy().ends_with(".xlsx.bak"), "{path:?}");
    }

    #[test]
    fn keeps_only_latest_five() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("a.xlsx");
        std::fs::write(&src, b"x").unwrap();

        for i in 0..8 {
            save(data_dir.path(), &src, format!("v{i}").as_bytes()).unwrap();
            // 时间戳精度到毫秒，加一点间隔保证排序稳定
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let all = list_for(data_dir.path(), &src);
        assert_eq!(all.len(), KEEP_PER_FILE, "只保留最近 5 份：{all:?}");
        // 留下的是最新的那几份
        let latest = std::fs::read(latest_for(data_dir.path(), &src).unwrap()).unwrap();
        assert_eq!(latest, b"v7");
    }

    #[test]
    fn different_files_do_not_share_backups() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let a = work.path().join("a.xlsx");
        let b = work.path().join("b.xlsx");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        save(data_dir.path(), &a, b"a-content").unwrap();
        save(data_dir.path(), &b, b"b-content").unwrap();

        assert_eq!(list_for(data_dir.path(), &a).len(), 1);
        assert_eq!(list_for(data_dir.path(), &b).len(), 1);
        assert_eq!(
            std::fs::read(latest_for(data_dir.path(), &a).unwrap()).unwrap(),
            b"a-content"
        );
    }

    #[test]
    fn no_backup_yields_none() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("never.xlsx");
        std::fs::write(&src, b"x").unwrap();
        assert!(latest_for(data_dir.path(), &src).is_none());
    }
}
