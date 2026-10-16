//! Provenance describes loaded content; it is not an execution or author-trust grant.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSource {
    Builtin,
    UserClaude,
    UserAgents,
    WorkspaceClaude,
    WorkspaceAgents,
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
