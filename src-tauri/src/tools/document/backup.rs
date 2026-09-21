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
/// 用纳秒精度而不是毫秒：备份名就是时间戳，毫秒下「同一毫秒内两次备份」
/// 会算出同一个名字，后一份直接盖掉前一份——那等于静默丢掉一个可回退的版本。
fn timestamp() -> String {
    chrono::Utc::now().format("%Y%m%d%H%M%S%9f").to_string()
}

/// 源文件名里可能有路径分隔符或奇怪的字符，压成一个安全的片段。
fn safe_basename(src: &Path) -> String {
    let raw = src
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string());
    raw.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 把 `data`（源文件当前内容）备份下来，返回备份文件路径。
///
/// 备份失败不算致命：调用方应记录告警后继续修改，而不是因为「备份没存上」就拒绝干活。
pub fn save(data_dir: &Path, src: &Path, data: &[u8]) -> std::io::Result<PathBuf> {
    let dir = dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let path = free_path(&dir, src);
    std::fs::write(&path, data)?;
    prune(data_dir, src);
    Ok(path)
}

/// 取一个还没被占用的备份路径。
///
/// 时间戳已经细到纳秒，撞名实际上不会发生；但**丢掉一个备份是无法接受的**（使用者会少一个
/// 可回退的版本，而且这件事没有任何提示），所以这里再兜一层：名字被占就在标识段上递增，
/// 直到拿到空位。递增段插在标识里，列表匹配与排序都不受影响。
fn free_path(dir: &Path, src: &Path) -> PathBuf {
    let base = path_tag(src);
    let name = safe_basename(src);
    for attempt in 0..100 {
        let tag = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        let p = dir.join(format!("{}-{}-{}.bak", timestamp(), tag, name));
        if !p.exists() {
            return p;
        }
    }
    // 极端情况下（同目录一万份同名备份）也不报错：用一个几乎不可能撞的名字兵底
    dir.join(format!(
        "{}-{}-{}.bak",
        timestamp(),
        uuid::Uuid::new_v4(),
        name
    ))
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

/// 一份备份的描述（供界面列出「可以回退到哪几个版本」）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupInfo {
    /// 备份文件路径（回退时原样传回来）
    pub path: String,
    /// 备份时刻（RFC3339；名字里的时间戳解读不出来时回落文件修改时间）
    pub at: String,
    /// 备份文件的字节数
    pub size: u64,
}

/// 从备份文件名里读出时刻；读不出来返回 None（调用方回落文件修改时间）。
///
/// 不用修改时间做主依据：回退时会把备份文件拷来拷去，修改时间会被碰。
/// 小数位数不写死：时间戳精度或格式以后要调时，这里不必跟着改。
fn time_of(name: &str) -> Option<String> {
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    let (secs, frac) = digits.split_at(digits.len().min(14));
    if secs.len() != 14 {
        return None;
    }
    let (value, fmt) = if frac.is_empty() {
        (secs.to_string(), "%Y%m%d%H%M%S")
    } else {
        (format!("{secs}.{frac}"), "%Y%m%d%H%M%S%f")
    };
    chrono::NaiveDateTime::parse_from_str(&value, fmt)
        .ok()
        .map(|d| d.and_utc().to_rfc3339())
}

/// 某个源文件的全部备份描述，**新的在前**（界面默认选第一个，也就是最新那份）。
pub fn describe(data_dir: &Path, src: &Path) -> Vec<BackupInfo> {
    let mut out: Vec<BackupInfo> = list_for(data_dir, src)
        .into_iter()
        .map(|p| {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            let at = p
                .file_name()
                .and_then(|n| time_of(&n.to_string_lossy()))
                .or_else(|| {
                    std::fs::metadata(&p)
                        .and_then(|m| m.modified())
                        .ok()
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                })
                .unwrap_or_default();
            BackupInfo {
                path: p.to_string_lossy().into_owned(),
                at,
                size,
            }
        })
        .collect();
    out.reverse();
    out
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

    /// 描述列表按「新的在前」排，时间戳从文件名解读成 RFC3339（带毫秒），
    /// 体积与磁盘上的实际字节数一致。
    #[test]
    fn describe_lists_newest_first_with_parsed_time() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("表.xlsx");
        std::fs::write(&src, b"x").unwrap();

        save(data_dir.path(), &src, b"one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        save(data_dir.path(), &src, b"two-longer").unwrap();

        let list = describe(data_dir.path(), &src);
        assert_eq!(list.len(), 2);
        // 新的在前：第一份是「two-longer」
        assert_eq!(list[0].size, b"two-longer".len() as u64);
        assert_eq!(list[1].size, b"one".len() as u64);
        // 时间能解读且递减
        assert!(list[0].at.starts_with("20"), "{}", list[0].at);
        assert!(list[0].at > list[1].at, "{} vs {}", list[0].at, list[1].at);
        // 路径就是备份文件本身
        assert!(std::path::Path::new(&list[0].path).exists());
    }

    #[test]
    fn describe_is_empty_without_backups() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("a.xlsx");
        std::fs::write(&src, b"x").unwrap();
        assert!(describe(data_dir.path(), &src).is_empty());
    }

    /// 同一瞬间连做两次备份，两份都得在。
    /// 曾经的时间戳只到毫秒：同一毫秒内两次备份算出同一个名字，后一份直接盖掉前一份，
    /// 而这件事没有任何提示——使用者只是发现「能回退的版本少了一个」。
    #[test]
    fn two_backups_in_the_same_instant_both_survive() {
        let data_dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("a.xlsx");
        std::fs::write(&src, b"x").unwrap();

        let first = save(data_dir.path(), &src, b"state-0").unwrap();
        let second = save(data_dir.path(), &src, b"state-1").unwrap();
        assert_ne!(first, second, "两次备份必须落在不同文件上");
        assert_eq!(std::fs::read(&first).unwrap(), b"state-0");
        assert_eq!(std::fs::read(&second).unwrap(), b"state-1");
        assert_eq!(list_for(data_dir.path(), &src).len(), 2);
        // 两份都能解读出时间，且排序把后写的排在后面
        let list = describe(data_dir.path(), &src);
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|b| !b.at.is_empty()), "{list:?}");
        assert!(list[0].at >= list[1].at, "{:?}", list);
    }
}
