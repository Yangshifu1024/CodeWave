//! 路径边界的唯一事实源（[docs/p0-plan](../../../docs/p0-plan.md) §7.1）。
//! 所有文件类工具与 fence 检查必须经由本模块，禁止散落实现。

use std::path::{Component, Path, PathBuf};

/// 写入许可根集合：工作区、数据目录、用户始终允许的外部目录三者并集构成可写边界。
#[derive(Debug, Clone)]
pub struct WriteRoots {
    /// 项目主目录（会话快照）；临时会话为全局数据目录。
    pub workspace: PathBuf,
    /// 「始终允许本项目」放行的额外外部目录。
    pub extra: Vec<PathBuf>,
    /// 本会话托管数据目录（`.codewave/`）。
    pub data_dir: PathBuf,
}

impl WriteRoots {
    /// 由工作区与数据目录构造（extra 由调用方按需补充）。
    pub fn new(workspace: PathBuf, data_dir: PathBuf) -> Self {
        WriteRoots {
            workspace,
            extra: Vec::new(),
            data_dir,
        }
    }

    /// 全部写根的规范化形态（解析符号链接）。规范化失败（不存在）的根按原样保留。
    pub fn canonical_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.workspace.clone(), self.data_dir.clone()];
        roots.extend(self.extra.iter().cloned());
        roots
            .iter()
            .filter_map(|p| std::fs::canonicalize(p).ok().or_else(|| Some(p.clone())))
            .collect()
    }
}

/// 统一的 (code, message) 错误构造辅助。
fn err(code: &str, msg: impl Into<String>) -> Result<PathBuf, (String, String)> {
    Err((code.to_string(), msg.into()))
}

/// 安全的相对路径拼接：拒绝绝对路径、`..` 逃逸、Windows 保留名、控制字符。
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, (String, String)> {
    if rel.trim().is_empty() {
        return err("E_ARGS", "路径为空");
    }
    let p = Path::new(rel);
    if p.is_absolute() || rel.starts_with('~') {
        // 绝对路径走 resolve_* 的白名单分支；join 不允许越界
        return err("E_ARGS", "工具入参必须使用相对工作区的路径");
    }
    let mut out = root.to_path_buf();
    for comp in p.components() {
        match comp {
            Component::Normal(c) => {
                let s = c.to_string_lossy();
                if s.chars().any(|ch| ch.is_control()) {
                    return err("E_ARGS", "路径含非法控制字符");
                }
                #[cfg(target_os = "windows")]
                {
                    const RESERVED: &[&str] = &[
                        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
                        "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
                        "LPT7", "LPT8", "LPT9",
                    ];
                    let upper = s.split('.').next().unwrap_or("").to_ascii_uppercase();
                    if RESERVED.contains(&upper.as_str()) {
                        return err("E_ARGS", format!("Windows 保留名：{s}"));
                    }
                }
                out.push(c);
            }
            Component::CurDir => {}
            Component::ParentDir => return err("E_PATH_OUTSIDE", "路径不允许包含 `..`"),
            other => return err("E_ARGS", format!("路径组件不支持：{other:?}")),
        }
    }
    Ok(out)
}

/// 尽力规范化路径；目标不存在时向上找到最近的现存祖先再规范化。
pub(crate) fn canonical_best_effort(p: &Path) -> PathBuf {
    match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => {
            let mut cur = p.to_path_buf();
            let mut suffix = Vec::new();
            loop {
                let parent = match cur.parent() {
                    Some(par) => par.to_path_buf(),
                    None => return p.to_path_buf(),
                };
                suffix.push(
                    cur.file_name()
                        .map(|s| s.to_os_string())
                        .unwrap_or_default(),
                );
                if let Ok(c) = std::fs::canonicalize(&parent) {
                    let mut out = c;
                    for s in suffix.iter().rev() {
                        out.push(s);
                    }
                    return out;
                }
                cur = parent;
            }
        }
    }
}

fn inside_any(cand: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| cand.starts_with(r))
}

/// 去掉 Windows `canonicalize` 产生的 verbatim 前缀：`\\?\C:\x` → `C:\x`、`\\?\UNC\srv\share` → `\\srv\share`。
///
/// **只用于展示与「交给模型的引用写法」**（[docs/composer-file-ref-chips](../../../docs/composer-file-ref-chips.md)）：
/// 边界比较、extra 根存储等内部形态一律保持 canonical（见 `safety/fence/check.rs` 的命名空间注释），
/// 否则包含性判断会失真。去前缀后的路径在 Windows 上仍可被 `read`/`write` 接受——
/// `resolve_read/resolve_write` 会先 `canonical_best_effort` 再比根，又回到同一命名空间。
/// 幂等；仅剥离盘符（`X:`）与 `UNC` 两种形态，Unix 上把 `\\?\` 当普通文件名的相对路径不受影响。
pub fn strip_verbatim_prefix(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        if !rest.is_empty() {
            return format!(r"\\{rest}");
        }
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return rest.to_string();
        }
    }
    s.to_string()
}

/// fence 使用的包含性检查：候选路径是否落在任一根之内。
pub(crate) fn inside_roots(cand: &Path, roots: &[PathBuf]) -> bool {
    inside_any(cand, roots)
}

/// 读路径解析：绝对路径（白名单根内的规范形态）或相对路径。
/// 相对路径跨根解析：先试主根，未命中依次试 extra 根（首个存在者胜出）；
/// 都不存在时回退主根候选（随后的打开操作会给出明确的 not-found 错误）。
pub fn resolve_read(roots: &WriteRoots, raw: &str) -> Result<PathBuf, (String, String)> {
    let cand = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        let primary = safe_join(&roots.workspace, raw)?;
        if primary.exists() {
            return Ok(primary);
        }
        for extra in &roots.extra {
            let cand = safe_join(Path::new(extra), raw)?;
            if cand.exists() {
                return Ok(cand);
            }
        }
        primary
    };
    let canonical = canonical_best_effort(&cand);
    if !inside_any(&canonical, &roots.canonical_roots()) {
        return err("E_PATH_OUTSIDE", format!("路径超出可读边界：{raw}"));
    }
    Ok(cand)
}

/// 写路径解析：规范化（含中间符号链接）后必须落在白名单根内。
pub fn resolve_write(roots: &WriteRoots, raw: &str) -> Result<PathBuf, (String, String)> {
    let cand = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        safe_join(&roots.workspace, raw)?
    };
    let canonical = canonical_best_effort(&cand);
    if !inside_any(&canonical, &roots.canonical_roots()) {
        return err("E_PATH_OUTSIDE", format!("路径超出可写边界：{raw}"));
    }
    Ok(cand)
}

/// 危险删除守卫：拒绝文件系统根 / 任一工作区根 / 任一 `.git` / 家目录 / 系统目录。
/// 评审 H2：extra 根与其 `.git` 此前不在保护名单内，AI 曾可删掉整个 extra 根。
pub fn is_dangerous_delete(p: &Path, workspace: &Path) -> bool {
    is_dangerous_delete_multi(p, &[workspace])
}

/// 多根变体：给每个根本身与其 `.git` 同等保护。
pub fn is_dangerous_delete_multi(p: &Path, roots: &[&Path]) -> bool {
    let resolved = canonical_best_effort(p);
    let home = dirs::home_dir();
    let system_roots: Vec<PathBuf> = [
        Some(PathBuf::from("/")),
        #[cfg(target_os = "macos")]
        Some(PathBuf::from("/System")),
        #[cfg(target_os = "macos")]
        Some(PathBuf::from("/usr")),
        #[cfg(target_os = "macos")]
        Some(PathBuf::from("/etc")),
        #[cfg(target_os = "windows")]
        Some(
            std::env::var("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_default(),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    for sr in system_roots {
        if resolved == sr {
            return true;
        }
    }
    if let Some(h) = home {
        if resolved == h {
            return true;
        }
    }
    for root in roots {
        if resolved == canonical_best_effort(root) {
            return true;
        }
        // 指向根内 .git 的路径（.git 目录及其内容）
        if resolved.starts_with(canonical_best_effort(&root.join(".git"))) {
            return true;
        }
    }
    // 根级挂载点（Unix 根的首层特殊目录未纳入检查；普通子目录保守放行）
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(ws: &tempfile::TempDir) -> WriteRoots {
        let ws_canon = std::fs::canonicalize(ws.path()).unwrap();
        let dd = tempfile::tempdir().unwrap();
        WriteRoots {
            workspace: ws_canon,
            extra: vec![],
            data_dir: std::fs::canonicalize(dd.path()).unwrap_or_else(|_| dd.path().to_path_buf()),
        }
    }

    #[test]
    fn verbatim_prefix_stripped_for_display() {
        // Windows canonicalize 产物 → 去前缀（仅展示与引用写法；内部边界仍用 canonical）
        assert_eq!(
            strip_verbatim_prefix(r"\\?\C:\Users\x\a.xlsx"),
            r"C:\Users\x\a.xlsx"
        );
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\srv\share\a.xlsx"),
            r"\\srv\share\a.xlsx"
        );
        // 幂等：已剥过的路径再过一次不变
        assert_eq!(
            strip_verbatim_prefix(&strip_verbatim_prefix(r"\\?\C:\x")),
            r"C:\x"
        );
        // 不误伤：POSIX 绝对路径、UNC 直接形式、以及 Unix 上把 `\\?\` 当普通名字的相对路径
        for s in [
            "/tmp/ws/a.xlsx",
            r"\\srv\share",
            r"\\?\weird",
            "report.xlsx",
            "",
        ] {
            assert_eq!(strip_verbatim_prefix(s), s, "{s}");
        }
    }

    #[test]
    fn safe_join_rejects_escape() {
        let ws = tempfile::tempdir().unwrap();
        let r = roots(&ws);
        assert!(safe_join(&r.workspace, "a/b.txt").is_ok());
        assert!(safe_join(&r.workspace, "../x").is_err());
        assert!(safe_join(&r.workspace, "/etc/passwd").is_err());
        assert!(safe_join(&r.workspace, "").is_err());
        assert!(safe_join(&r.workspace, "a\u{0}b").is_err());
    }

    #[test]
    fn read_write_containment() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("in.txt"), b"x").unwrap();
        let r = roots(&ws);
        assert!(resolve_read(&r, "in.txt").is_ok());
        assert!(resolve_write(&r, "sub/new.txt").is_ok());
        // 写根之外已存在的文件（独立 tempdir，避免被 data_dir 遮蔽）
        let out = tempfile::tempdir().unwrap();
        let outside = out.path().join("f.txt");
        std::fs::write(&outside, b"y").unwrap();
        assert_eq!(
            resolve_read(&r, outside.to_str().unwrap()).unwrap_err().0,
            "E_PATH_OUTSIDE"
        );
        assert_eq!(
            resolve_write(&r, outside.to_str().unwrap()).unwrap_err().0,
            "E_PATH_OUTSIDE"
        );
    }

    #[test]
    fn data_dir_readable_and_writable() {
        let ws = tempfile::tempdir().unwrap();
        let mut r = roots(&ws);
        let dd = tempfile::tempdir().unwrap();
        r.data_dir = std::fs::canonicalize(dd.path()).unwrap();
        assert!(resolve_write(&r, dd.path().join("x.json").to_str().unwrap()).is_ok());
    }

    #[test]
    fn symlink_escape_blocked() {
        #[cfg(unix)]
        {
            let ws = tempfile::tempdir().unwrap();
            let out = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(out.path(), ws.path().join("leak")).unwrap();
            let r = roots(&ws);
            assert_eq!(
                resolve_write(&r, "leak/evil.txt").unwrap_err().0,
                "E_PATH_OUTSIDE"
            );
        }
    }
}
