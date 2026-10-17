//! Ordinary tools cannot replace host-owned transaction state, even in FullAccess.
//! Scope comes from runtime snapshots; a directory merely named `.codewave` has no authority.

use crate::tools::pathutil::{WriteRoots, canonical_best_effort};
use std::path::{Component, Path, PathBuf};

pub const ERROR_CODE: &str = "E_INTERNAL_STATE";

const GLOBAL_FILES: &[&str] = &[
    "config.json",
    "config.bak",
    "credential-commits.json",
    "mcp.json",
    "running.marker",
    "ui-state.json",
    "ui-state.json.corrupt",
    "projects/directories.json",
];
const GLOBAL_TREES: &[&str] = &[
    "sessions",
    "histories",
    "stats",
    "locks",
    "instances",
    "trust",
    "credential-leases",
    "workflow_trust",
    "coord",
];
const PROJECT_FILES: &[&str] = &["project.json", "mcp.json"];
const PROJECT_TREES: &[&str] = &["locks", "instances", "trust", "credential-leases", "coord"];

/// Reject the target itself, descendants and ancestors that could replace/delete
/// protected state. This checks both named entries and their current symlink
/// destinations; it does not grant write access or change ordinary read boundaries.
pub fn check_write(roots: &WriteRoots, path: &Path) -> Result<(), (String, String)> {
    check(roots, path, true)
}

/// For an operation proven to preserve its directory container (for example
/// mkdir of an existing workspace). Callers must still check every actual child
/// destination; this is not a recursive-copy or rename exemption.
pub fn check_direct_write(roots: &WriteRoots, path: &Path) -> Result<(), (String, String)> {
    check(roots, path, false)
}

fn check(roots: &WriteRoots, path: &Path, include_ancestors: bool) -> Result<(), (String, String)> {
    let target = if path.is_absolute() {
        path.to_path_buf()
    } else {
        roots.workspace.join(path)
    };
    let targets = forms(&target)?;
    let mut reserved: Vec<PathBuf> = GLOBAL_FILES
        .iter()
        .chain(GLOBAL_TREES)
        .map(|name| roots.data_dir.join(name))
        .collect();
    collect_versioned_ui_state(&roots.data_dir, &target, &mut reserved);
    if let Some(project) = &roots.project_dir {
        reserved.extend(
            PROJECT_FILES
                .iter()
                .chain(PROJECT_TREES)
                .map(|name| project.join(name)),
        );
        // Scheduler owns direct <task-id>.json entries. Plans and other artifacts
        // under tasks/ remain writable, including JSON in artifact subdirectories.
        let task_dir = project.join("tasks");
        collect_task_state(&task_dir, &target, &mut reserved);
    }
    for protected in reserved {
        for entry in forms(&protected)? {
            if targets
                .iter()
                .any(|target| overlaps(target, &entry, include_ancestors))
            {
                return Err((
                    ERROR_CODE.into(),
                    format!(
                        "拒绝修改宿主内部状态：{}；请使用对应设置或业务工具",
                        protected.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn collect_versioned_ui_state(root: &Path, target: &Path, reserved: &mut Vec<PathBuf>) {
    collect_entries(root, target, reserved, |name| {
        name.strip_prefix("ui-state.json.v")
            .and_then(|n| n.strip_suffix(".bak"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
}

fn collect_task_state(root: &Path, target: &Path, reserved: &mut Vec<PathBuf>) {
    collect_entries(root, target, reserved, |name| {
        name.strip_suffix(".json")
            .is_some_and(crate::core::projects::valid_id)
    });
}

fn collect_entries(
    root: &Path,
    target: &Path,
    reserved: &mut Vec<PathBuf>,
    accepts: impl Fn(&str) -> bool,
) {
    // Existing entries are necessary to also protect aliases pointing at an
    // otherwise ordinary destination (including symlinked scheduler state).
    if let Ok(entries) = std::fs::read_dir(root) {
        reserved.extend(
            entries
                .filter_map(Result::ok)
                .filter(|e| accepts(&comparable(&PathBuf::from(e.file_name())).to_string_lossy()))
                .map(|e| e.path()),
        );
    }
    // Missing state must be protected before creation. Compare within each root
    // namespace so an allowed alias cannot create a new scheduler entry.
    for root_form in forms(root).unwrap_or_default() {
        for target_form in forms(target).unwrap_or_default() {
            let target_form = comparable(&target_form);
            let root_form = comparable(&root_form);
            if let Ok(relative) = target_form.strip_prefix(&root_form)
                && let Some(Component::Normal(name)) = relative.components().next()
                && accepts(&name.to_string_lossy())
            {
                reserved.push(root.join(name));
            }
        }
    }
}

fn forms(path: &Path) -> Result<Vec<PathBuf>, (String, String)> {
    let lexical = normalize(path);
    let resolved = resolve_links(path).map_err(|e| {
        (
            ERROR_CODE.into(),
            format!("无法确认内部状态路径，拒绝写入：{}: {e}", path.display()),
        )
    })?;
    Ok(vec![
        lexical,
        canonical_best_effort(path),
        canonical_best_effort(&resolved),
    ])
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            component => out.push(component.as_os_str()),
        }
    }
    out
}

/// canonicalize alone cannot resolve a dangling symlink. Walking links also
/// protects a missing state target, with a bounded cycle check.
fn resolve_links(path: &Path) -> std::io::Result<PathBuf> {
    let mut pending = path.to_path_buf();
    for _ in 0..40 {
        let mut prefix = PathBuf::new();
        let mut components = pending.components();
        let mut replaced = false;
        while let Some(component) = components.next() {
            prefix.push(component.as_os_str());
            if std::fs::symlink_metadata(&prefix).is_ok_and(|m| m.file_type().is_symlink()) {
                let link = std::fs::read_link(&prefix)?;
                let destination = if link.is_absolute() {
                    link
                } else {
                    prefix.parent().unwrap_or(Path::new("")).join(link)
                };
                // Preserve `link/..` until the link has been expanded: lexical
                // removal of `..` would change the kernel's destination.
                pending = destination.join(components.as_path());
                replaced = true;
                break;
            }
        }
        if !replaced {
            return Ok(normalize(&pending));
        }
    }
    Err(std::io::Error::other(
        "symbolic link cycle or excessive depth",
    ))
}

fn overlaps(a: &Path, b: &Path, include_ancestors: bool) -> bool {
    let (a, b) = (comparable(a), comparable(b));
    a.starts_with(&b) || include_ancestors && b.starts_with(&a)
}

fn comparable(path: &Path) -> PathBuf {
    // Missing entries cannot be canonicalized to their filesystem spelling.
    // macOS deliberately reserves case variants even on case-sensitive volumes.
    #[cfg(windows)]
    {
        PathBuf::from(
            crate::tools::pathutil::strip_verbatim_prefix(&path.to_string_lossy()).to_lowercase(),
        )
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
#[path = "protected_state_tests.rs"]
mod tests;
