//! Provenance describes loaded content; it is not an execution or author-trust grant.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Bound source reads before parsing or hashing; a truncated file has no version.
pub fn read_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Resource is not a regular file",
        ));
    }
    if metadata.len() > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Resource exceeds 1 MiB",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Resource changed to a non-regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Resource exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSource {
    Builtin,
    UserClaude,
    UserAgents,
    WorkspaceClaude,
    WorkspaceAgents,
    WorkspaceFile,
    GlobalManaged,
    ProjectManaged,
}

impl ResourceSource {
    pub fn priority(self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::UserClaude => 1,
            Self::UserAgents => 2,
            Self::WorkspaceClaude => 3,
            Self::WorkspaceAgents => 4,
            Self::WorkspaceFile => 4,
            Self::GlobalManaged => 5,
            Self::ProjectManaged => 6,
        }
    }

    pub fn managed(self) -> bool {
        matches!(self, Self::GlobalManaged | Self::ProjectManaged)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourceOrigin {
    pub source: ResourceSource,
    /// Canonical file identity, or a namespaced built-in identity.
    pub identity: String,
    /// SHA-256 of raw file bytes or registered built-in template data.
    pub version: String,
}

impl ResourceOrigin {
    pub fn builtin(kind: &str, name: &str, bytes: &[u8]) -> Self {
        Self {
            source: ResourceSource::Builtin,
            identity: format!("builtin:{kind}:{name}"),
            version: content_version(bytes),
        }
    }

    pub fn file(source: ResourceSource, canonical: &Path, bytes: &[u8]) -> Self {
        Self {
            source,
            identity: format!("file:{}", canonical.display()),
            version: content_version(bytes),
        }
    }
}

pub fn content_version(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}
