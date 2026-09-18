//! 工作区归属：扫描项目的语言根（一项目 × 一语言多实例）、按文件反查所属根。

use super::Lang;
use super::server_spec;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// 扫描默认深度。
pub const DEFAULT_MAX_DEPTH: usize = 3;

/// 扫描时跳过的目录名（构建产物、依赖、VCS、工具目录）。
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".codewave",
    ".dart_tool",
    ".venv",
    "venv",
    "out",
    ".idea",
];

/// 该目录是否含某语言的 manifest。
pub fn has_manifest(dir: &Path, lang: Lang) -> bool {
    server_spec::spec(lang)
        .manifest
        .iter()
        .any(|m| dir.join(m).is_file())
}

/// BFS 扫描项目根，返回 `(语言, 语言根)` 清单（同一目录同一语言只记一次）。
///
/// **一项目 × 一语言多实例是常态**：本仓库自身就是 `Cargo.toml` 在根、
/// `package.json`+`tsconfig.json` 在 `ui/`，两个根都要给出。
pub fn scan_roots(project_root: &Path, max_depth: usize) -> Vec<(Lang, PathBuf)> {
    let mut found: BTreeSet<(Lang, PathBuf)> = BTreeSet::new();
    let mut queue: Vec<(PathBuf, usize)> = vec![(project_root.to_path_buf(), 0)];
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some((dir, depth)) = queue.pop() {
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !visited.insert(canonical) {
            continue;
        }
        for lang in Lang::all() {
            if has_manifest(&dir, lang) {
                found.insert((lang, dir.clone()));
            }
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            // 只递归真实目录（不跟着符号链接兜圈）
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            queue.push((entry.path(), depth + 1));
        }
    }
    found.into_iter().collect()
}

/// 文件所属的语言根：祖先链上最近的、含该语言 manifest 的目录；找不到回落项目根。
pub fn root_for_file(project_root: &Path, file: &Path, lang: Lang) -> PathBuf {
    let start = if file.is_dir() {
        Some(file)
    } else {
        file.parent()
    };
    let mut cur = start.map(|p| p.to_path_buf());
    while let Some(dir) = cur {
        if has_manifest(&dir, lang) {
            return dir;
        }
        if dir == project_root {
            break;
        }
        let parent = dir.parent().map(|p| p.to_path_buf());
        match parent {
            // 已越过项目根（或在根之外）就停，避免向上无限找
            Some(p) if p.starts_with(project_root) || dir.starts_with(project_root) => {
                cur = Some(p)
            }
            _ => break,
        }
    }
    project_root.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造 `root/{Cargo.toml,src/x.rs}` + `root/ui/{package.json,tsconfig.json,src/y.ts}`。
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/x.rs"), "fn main(){}").unwrap();
        let ui = root.join("ui");
        std::fs::create_dir_all(ui.join("src")).unwrap();
        std::fs::write(ui.join("package.json"), "{}").unwrap();
        std::fs::write(ui.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(ui.join("src/y.ts"), "export {}").unwrap();
        // 依赖目录里的 manifest 必须被跳过
        let nm = ui.join("node_modules").join("left-pad");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("package.json"), "{}").unwrap();
        tmp
    }

    #[test]
    fn scan_finds_multiple_language_roots() {
        let tmp = fixture();
        let roots = scan_roots(tmp.path(), DEFAULT_MAX_DEPTH);
        assert!(
            roots.contains(&(Lang::Rust, tmp.path().to_path_buf())),
            "{roots:?}"
        );
        let ui = tmp.path().join("ui");
        assert!(roots.contains(&(Lang::TypeScript, ui.clone())), "{roots:?}");
        assert!(
            !roots
                .iter()
                .any(|(_, p)| p.to_string_lossy().contains("node_modules")),
            "依赖目录内的 manifest 不得成为语言根：{roots:?}"
        );
        assert_eq!(
            roots.iter().filter(|(l, _)| *l == Lang::TypeScript).count(),
            1
        );
    }

    #[test]
    fn scan_respects_max_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("Cargo.toml"), "[package]").unwrap();
        assert!(scan_roots(tmp.path(), 1).is_empty());
        assert_eq!(scan_roots(tmp.path(), 4).len(), 1);
    }

    #[test]
    fn root_for_file_walks_up_to_nearest_manifest() {
        let tmp = fixture();
        let root = tmp.path();
        assert_eq!(
            root_for_file(root, &root.join("src/x.rs"), Lang::Rust),
            root.to_path_buf()
        );
        assert_eq!(
            root_for_file(root, &root.join("ui/src/y.ts"), Lang::TypeScript),
            root.join("ui")
        );
    }

    #[test]
    fn root_for_file_falls_back_to_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("x").join("y");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            root_for_file(tmp.path(), &nested.join("z.py"), Lang::Python),
            tmp.path().to_path_buf()
        );
    }

    #[test]
    fn scan_skips_dot_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let hidden = tmp.path().join(".hidden-ui");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("tsconfig.json"), "{}").unwrap();
        assert!(scan_roots(tmp.path(), DEFAULT_MAX_DEPTH).is_empty());
    }
}
