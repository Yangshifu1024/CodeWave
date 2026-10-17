//! Check prerequisites are frozen with the check and evaluated at the host boundary.
use crate::core::delivery::types::CheckSpec;
pub fn unavailable(spec: &CheckSpec) -> Option<String> {
    if !spec.platforms.is_empty() && !spec.platforms.iter().any(|p| p == std::env::consts::OS) {
        return Some(format!(
            "unverified: required platforms {:?}; current platform {}",
            spec.platforms,
            std::env::consts::OS
        ));
    }
    for name in &spec.prerequisites {
        if name.is_empty() || name.contains(['/', '\\', ':']) {
            return Some("unverified: invalid executable prerequisite".into());
        }
        let found = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| {
                let p = dir.join(name);
                p.is_file()
                    || cfg!(windows)
                        && ["exe", "cmd", "bat"]
                            .iter()
                            .any(|ext| dir.join(format!("{name}.{ext}")).is_file())
            })
        });
        if !found {
            return Some(format!("unverified: executable {name} is unavailable"));
        }
    }
    None
}
