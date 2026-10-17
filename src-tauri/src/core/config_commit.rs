//! Serialized configuration commits. Secrets are prepared in immutable keyring generations;
//! replacing config.json is the only commit point. No cfg lock is held during blocking I/O.
use super::config::{ConfigState, KEYRING_PLACEHOLDER, ProviderConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const MAX_RECORDS: usize = 512;
// 64 KiB persistent UUID-only tombstones. Never rotate/clear automatically: doing so would
// re-admit an expired request. False positives reject safely; 50% occupancy fails closed.
const TOMBSTONE_WORDS: usize = 8192;
const TOMBSTONE_BITS: usize = TOMBSTONE_WORDS * 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigVersion {
    pub boot_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigCommitError {
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for ConfigCommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ConfigCommitError {}
fn error(code: &str, message: &str) -> ConfigCommitError {
    ConfigCommitError {
        code: code.into(),
        message: message.into(),
    }
}

// Deliberately no Debug: this is an input DTO containing new secrets, never a disk DTO.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialEdit {
    Keep,
    Append { new_keys: Vec<String> },
    Replace { new_keys: Vec<String> },
    Clear,
}

/// JSON objects must not silently overwrite duplicate provider intents during deserialization.
/// Keep this input type separate from the persistable configuration model.
pub struct CredentialEdits(pub HashMap<String, CredentialEdit>);
impl<'de> Deserialize<'de> for CredentialEdits {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = CredentialEdits;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("unique provider credential operations")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut edits = HashMap::new();
                while let Some((provider, edit)) = access.next_entry::<String, CredentialEdit>()? {
                    if edits.insert(provider, edit).is_some() {
                        return Err(serde::de::Error::custom(
                            "duplicate provider credential operation",
                        ));
                    }
                }
                Ok(CredentialEdits(edits))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSnapshot {
    pub config: ConfigState,
    pub version: ConfigVersion,
    pub page_version: u64,
    pub role_versions: HashMap<String, u64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigSaveResult {
    #[serde(flatten)]
    pub snapshot: ConfigSnapshot,
    pub cleanup: Option<super::sessions::CleanupOutcome>,
    pub replayed: bool,
    /// Only the original executor runs these post-commit effects; never serialized to clients.
    #[serde(skip)]
    pub effects: CommitEffects,
}
#[derive(Debug, Clone, Default)]
pub struct CommitEffects {
    pub providers_changed: bool,
    pub proxy_changed: bool,
    pub log_changed: bool,
    pub cleanup_days: Option<u32>,
}

/// The host supplies the OS adapter; tests use deterministic fakes, never user accounts.
pub trait CredentialStore: Send + Sync {
    fn read(&self, account: &str) -> Result<Vec<String>, ConfigCommitError>;
    fn write(&self, account: &str, keys: &[String]) -> Result<(), ConfigCommitError>;
    fn delete(&self, account: &str) -> Result<(), ConfigCommitError>;
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitJournal {
    records: Vec<CommitRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    expired_operations: Vec<u64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitRecord {
    operation_id: String,
    accounts: Vec<String>,
    committed: bool,
    post_processing: String,
    #[serde(default)]
    version: Option<ConfigVersion>,
}

pub trait ConfigFileStore: Send + Sync {
    fn read_config(&self) -> Result<Option<ConfigState>, ConfigCommitError>;
    fn write_config(&self, cfg: &ConfigState) -> Result<Vec<String>, ConfigCommitError>;
    fn read_journal(&self) -> Result<CommitJournal, ConfigCommitError>;
    fn write_journal(&self, journal: &CommitJournal) -> Result<(), ConfigCommitError>;
}

pub struct DiskConfigStore {
    root: PathBuf,
}
impl DiskConfigStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

/// This config-specific replacement NEVER copies the previous file to .bak, including Windows.
/// NamedTempFile::persist replaces the target atomically. Directory sync is best-effort after
/// the commit point: failure is a saved-with-warning result, never a false pre-commit failure.
fn replace_without_backup(path: &Path, bytes: &[u8]) -> Result<Vec<String>, ConfigCommitError> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| error("ConfigWriteFailed", "Invalid configuration location"))?;
    std::fs::create_dir_all(parent)
        .map_err(|_| error("ConfigWriteFailed", "Cannot create configuration directory"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| error("ConfigWriteFailed", "Cannot prepare configuration file"))?;
    tmp.write_all(bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| error("ConfigWriteFailed", "Cannot write configuration file"))?;
    tmp.persist(path).map_err(|_| {
        error(
            "ConfigWriteFailed",
            "Cannot replace configuration file; previous configuration is unchanged",
        )
    })?;
    let mut warnings = Vec::new();
    #[cfg(unix)]
    if std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .is_err()
    {
        warnings.push("Configuration saved, but directory durability could not be verified".into());
    }
    Ok(warnings)
}
impl ConfigFileStore for DiskConfigStore {
    fn read_config(&self) -> Result<Option<ConfigState>, ConfigCommitError> {
        match std::fs::read(self.root.join("config.json")) {
            Ok(bytes) => {
                let cfg: ConfigState = serde_json::from_slice(&bytes).map_err(|_| {
                    error(
                        "ConfigUnreadable",
                        "Configuration is unreadable; credential cleanup is disabled",
                    )
                })?;
                if cfg.schema_version != 2 {
                    return Err(error(
                        "ConfigUnreadable",
                        "Unknown configuration format; credential cleanup is disabled",
                    ));
                }
                Ok(Some(cfg))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(error(
                "ConfigUnreadable",
                "Configuration cannot be read; credential cleanup is disabled",
            )),
        }
    }
    fn write_config(&self, cfg: &ConfigState) -> Result<Vec<String>, ConfigCommitError> {
        assert_persistable(cfg)?;
        let bytes = serde_json::to_vec_pretty(cfg)
            .map_err(|_| error("ConfigWriteFailed", "Cannot serialize configuration"))?;
        replace_without_backup(&self.root.join("config.json"), &bytes)
    }
    fn read_journal(&self) -> Result<CommitJournal, ConfigCommitError> {
        match std::fs::read(self.root.join("credential-commits.json")) {
            Ok(bytes) => {
                let mut journal: CommitJournal = serde_json::from_slice(&bytes).map_err(|_| {
                    error(
                        "JournalUnreadable",
                        "Credential recovery record cannot be read",
                    )
                })?;
                if journal
                    .records
                    .iter()
                    .any(|record| uuid::Uuid::parse_str(&record.operation_id).is_err())
                    || (!journal.expired_operations.is_empty()
                        && journal.expired_operations.len() != TOMBSTONE_WORDS)
                {
                    return Err(error(
                        "JournalUnreadable",
                        "Operation recovery history has an unsupported format",
                    ));
                }
                let mut ids = HashSet::new();
                for record in &mut journal.records {
                    record.operation_id = uuid::Uuid::parse_str(&record.operation_id)
                        .unwrap()
                        .to_string();
                    if !ids.insert(record.operation_id.clone()) {
                        return Err(error(
                            "JournalUnreadable",
                            "Duplicate operation recovery record",
                        ));
                    }
                }
                Ok(journal)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CommitJournal::default()),
            Err(_) => Err(error(
                "JournalUnreadable",
                "Credential recovery record cannot be read",
            )),
        }
    }
    fn write_journal(&self, journal: &CommitJournal) -> Result<(), ConfigCommitError> {
        let bytes = serde_json::to_vec(journal).map_err(|_| {
            error(
                "JournalWriteFailed",
                "Cannot prepare credential recovery record",
            )
        })?;
        let warnings = replace_without_backup(&self.root.join("credential-commits.json"), &bytes)
            .map_err(|_| {
            error(
                "JournalWriteFailed",
                "Cannot persist credential recovery record; no new credentials were created",
            )
        })?;
        if !warnings.is_empty() {
            return Err(error(
                "JournalWriteFailed",
                "Credential recovery record durability is unverified; no new credentials were created",
            ));
        }
        Ok(())
    }
}

struct CommitState {
    boot_id: String,
    sequence: u64,
    page_version: u64,
    role_versions: HashMap<String, u64>,
    warnings: Vec<String>,
    recovery_block: Option<ConfigCommitError>,
}

pub struct ConfigCoordinator {
    state: Mutex<CommitState>,
    credentials: Arc<dyn CredentialStore>,
    files: Arc<dyn ConfigFileStore>,
    /// #36-C0 跨进程 owner（`config:<root>` 租约）。`None` = 未接入协调的宿主
    /// （桌面单实例、旧测试）：行为与接入前完全一致。
    coord: Option<crate::core::process_coordination::LeaseStore>,
    /// 本进程**最后一次从磁盘读到**的配置摘要。提交前若磁盘摘要与它不同，
    /// 说明另一个进程改过 config.json——此时继续写就是静默覆盖别人的设置。
    ///
    /// 比"直接比对内存与磁盘"更稳：记录来自真实磁盘读，首次提交时建立，
    /// 每次成功写后刷新，因此不会出现"同一份内容序列化差异"造成的假阳性。
    last_disk_digest: Mutex<Option<String>>,
}

pub enum ConfigPatch {
    Page {
        config: Box<ConfigState>,
        expected: ConfigVersion,
        edits: HashMap<String, CredentialEdit>,
        skip_cleanup: bool,
    },
    Role {
        role: String,
        model_id: Option<String>,
        expected: ConfigVersion,
    },
    Fonts {
        sans: String,
        content: String,
        mono: String,
        /// sans 文本字号（px）；与 `sans` 字体族同槽提交。
        font_size_sans: f32,
        /// 正文字号（px）；与 `content` 字体族同槽提交。
        font_size_content: f32,
        /// mono 文本字号（px）；与 `mono` 字体族同槽提交。
        font_size_mono: f32,
    },
    Skill {
        name: String,
        disabled: bool,
    },
    AllowCommand {
        command: String,
    },
    SecureExisting,
}

impl ConfigCoordinator {
    pub fn new(credentials: Arc<dyn CredentialStore>, files: Arc<dyn ConfigFileStore>) -> Self {
        Self {
            state: Mutex::new(CommitState {
                boot_id: uuid::Uuid::new_v4().to_string(),
                sequence: 0,
                page_version: 0,
                role_versions: HashMap::new(),
                warnings: Vec::new(),
                recovery_block: None,
            }),
            credentials,
            files,
            coord: None,
            last_disk_digest: Mutex::new(None),
        }
    }

    /// 接入跨进程协调（#36-C0）：`data_root` 是全局数据根，协调根为 `<data_root>/coord`。
    ///
    /// 接入前后**语义不变**，只是多了两道：提交期间持 `config:<root>` 租约（不与
    /// history 事务嵌套），以及提交前检查磁盘是否被别的进程改过。
    pub fn with_coordination(mut self, data_root: &std::path::Path) -> Self {
        *self.last_disk_digest.lock().unwrap() = self
            .files
            .read_config()
            .ok()
            .flatten()
            .as_ref()
            .map(Self::config_digest);
        self.coord = Some(crate::core::process_coordination::LeaseStore::new(
            &data_root.join("coord"),
        ));
        self
    }

    /// 配置域的租约键。与 history / 项目根各占一把，互不误判。
    fn coord_resource() -> &'static str {
        "config:root"
    }

    fn config_digest(cfg: &ConfigState) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(cfg).unwrap_or_default());
        h.finalize()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Called only while the cross-process configuration transaction lock is held.
    fn check_no_external_change(&self) -> Result<(), ConfigCommitError> {
        if self.coord.is_none() {
            return Ok(());
        }
        let disk = self.files.read_config()?;
        let digest = disk.as_ref().map(Self::config_digest);
        let recorded = self.last_disk_digest.lock().unwrap();
        if *recorded != digest {
            return Err(error(
                "ConfigChangedExternally",
                "Configuration was changed by another CodeWave instance. Reload the settings before saving again",
            ));
        }
        Ok(())
    }

    /// 成功写入后刷新"我见过什么"记录。
    fn note_disk_digest(&self, written: &ConfigState) {
        if self.coord.is_none() {
            return;
        }
        if let Ok(mut g) = self.last_disk_digest.lock() {
            *g = Some(Self::config_digest(written));
        }
    }
    /// ConfigState::load is tolerant for display, never authority to replace an unreadable file.
    /// Latch failures until a new coordinator is built from an explicitly recovered startup load.
    fn readable_config(
        &self,
        state: &mut CommitState,
    ) -> Result<Option<ConfigState>, ConfigCommitError> {
        if let Some(block) = &state.recovery_block {
            return Err(block.clone());
        }
        match self.files.read_config() {
            Ok(config) => Ok(config),
            Err(_) => {
                let block = error(
                    "ConfigUnreadable",
                    "Configuration cannot be safely read or uses an unsupported format. Saving and credential cleanup are blocked. Repair or restore the configuration, then restart the application",
                );
                if !state.warnings.contains(&block.message) {
                    state.warnings.push(block.message.clone());
                }
                state.recovery_block = Some(block.clone());
                Err(block)
            }
        }
    }
    fn snapshot_locked(&self, state: &CommitState, cfg: &ConfigState) -> ConfigSnapshot {
        ConfigSnapshot {
            config: cfg.sanitized(),
            version: ConfigVersion {
                boot_id: state.boot_id.clone(),
                sequence: state.sequence,
            },
            page_version: state.page_version,
            role_versions: state.role_versions.clone(),
            warnings: state.warnings.clone(),
        }
    }
    pub fn snapshot(&self, cfg: &RwLock<ConfigState>) -> ConfigSnapshot {
        let state = self.state.lock().unwrap();
        self.snapshot_locked(&state, &cfg.read().unwrap())
    }
    pub fn warn(&self, message: &str) {
        self.state.lock().unwrap().warnings.push(message.into());
    }
    fn check_version(
        &self,
        state: &CommitState,
        expected: &ConfigVersion,
        sequence: u64,
    ) -> Result<(), ConfigCommitError> {
        if expected.boot_id != state.boot_id || expected.sequence != sequence {
            return Err(error(
                "ConfigConflict",
                "Settings changed elsewhere. Refresh and review your draft before saving again",
            ));
        }
        Ok(())
    }
    fn replay(
        &self,
        state: &CommitState,
        current: &ConfigState,
        journal: &CommitJournal,
        op: &str,
    ) -> Result<Option<ConfigSaveResult>, ConfigCommitError> {
        let record = journal.records.iter().find(|r| r.operation_id == op);
        if current
            .last_operation_id
            .as_deref()
            .is_some_and(|id| same_operation(id, op))
            || record.is_some_and(|r| r.committed)
        {
            let mut snapshot = self.snapshot_locked(state, current);
            if record.is_none_or(|r| r.post_processing != "complete") {
                snapshot.warnings.push("Configuration was saved. Previous post-processing may be incomplete; it will not be repeated automatically".into());
            }
            return Ok(Some(ConfigSaveResult {
                snapshot,
                cleanup: None,
                replayed: true,
                effects: CommitEffects::default(),
            }));
        }
        if record.is_some() {
            return Err(error(
                "OperationUncertain",
                "This operation did not commit or its outcome is uncertain. Refresh the configuration and review before starting a new operation",
            ));
        }
        if tombstones_saturated(journal) {
            return Err(history_full());
        }
        if expired_operation(journal, op) {
            return Err(error(
                "OperationExpired",
                "This operation may have an expired receipt. It was not executed again. Reload and review current settings before explicitly starting a new operation",
            ));
        }
        Ok(None)
    }
    pub fn operation(
        &self,
        cfg: &RwLock<ConfigState>,
        op: &str,
    ) -> Result<ConfigSaveResult, ConfigCommitError> {
        let op = uuid::Uuid::parse_str(op)
            .map_err(|_| error("InvalidOperation", "An operation UUID is required"))?
            .to_string();
        let state = self.state.lock().unwrap();
        let current = cfg.read().unwrap().clone();
        self.replay(&state, &current, &self.files.read_journal()?, &op)?.ok_or_else(|| error("OperationUnknown", "Operation receipt has expired or is unknown. Refresh and review; do not automatically resubmit"))
    }
    /// Cleanup is only run at startup, before any model snapshots can exist. If config cannot be
    /// decoded, NOTHING is deleted. Only syntactically application-generated generation accounts
    /// registered before creation/retirement are eligible; accounts are never enumerated.
    pub fn recover(&self) -> Result<(), ConfigCommitError> {
        let _disk = self
            .coord
            .as_ref()
            .map(|store| {
                crate::core::process_coordination::DiskLock::acquire(
                    &store.coord_root().join("config-transaction.lock"),
                )
                .map_err(|e| error("ConfigBusy", &e.to_string()))
            })
            .transpose()?;
        let mut state = self.state.lock().unwrap();
        let cfg = self.readable_config(&mut state)?.unwrap_or_default();
        let referenced: HashSet<_> = cfg.providers.iter().filter_map(provider_account).collect();
        let mut journal = self.files.read_journal()?;
        for record in &mut journal.records {
            if cfg
                .last_operation_id
                .as_deref()
                .is_some_and(|id| same_operation(id, &record.operation_id))
            {
                record.committed = true;
            }
            let mut keep = Vec::new();
            for account in &record.accounts {
                if referenced.contains(account.as_str()) || !valid_cleanup_account(account) {
                    keep.push(account.clone());
                } else if self.credentials.delete(account).is_err() {
                    keep.push(account.clone());
                    let warning =
                        "Credential cleanup needs retry; current saved credentials remain valid"
                            .to_string();
                    if !state.warnings.contains(&warning) {
                        state.warnings.push(warning);
                    }
                }
            }
            record.accounts = keep;
        }
        compact_journal(&mut journal)?;
        self.files.write_journal(&journal)
    }
    pub fn mark_post_processed(&self, operation_id: &str) -> Result<(), ConfigCommitError> {
        let _disk = self
            .coord
            .as_ref()
            .map(|store| {
                crate::core::process_coordination::DiskLock::acquire(
                    &store.coord_root().join("config-transaction.lock"),
                )
                .map_err(|e| error("ConfigBusy", &e.to_string()))
            })
            .transpose()?;
        let _state = self.state.lock().unwrap();
        let mut journal = self.files.read_journal()?;
        if let Some(r) = journal
            .records
            .iter_mut()
            .find(|r| r.operation_id == operation_id)
        {
            r.post_processing = "complete".into();
        }
        self.files.write_journal(&journal)
    }
    pub fn commit(
        &self,
        cfg: &RwLock<ConfigState>,
        patch: ConfigPatch,
        operation_id: &str,
    ) -> Result<ConfigSaveResult, ConfigCommitError> {
        self.commit_with(cfg, patch, operation_id, |_, _| {})
    }
    pub fn commit_with(
        &self,
        cfg: &RwLock<ConfigState>,
        patch: ConfigPatch,
        operation_id: &str,
        apply: impl FnOnce(&ConfigState, &CommitEffects),
    ) -> Result<ConfigSaveResult, ConfigCommitError> {
        let normalized_operation = uuid::Uuid::parse_str(operation_id)
            .map_err(|_| error("InvalidOperation", "An operation UUID is required"))?
            .to_string();
        let operation_id = normalized_operation.as_str();
        // 磁盘事实检查在**取进程内锁之前**：别的进程改过配置时应当立刻拒绝，
        // 而不是先把所有提交堵在 mutex 上再失败。
        let _disk = self
            .coord
            .as_ref()
            .map(|store| {
                crate::core::process_coordination::DiskLock::try_acquire(
                    &store.coord_root().join("config-transaction.lock"),
                )
                .map_err(|e| error("ConfigBusy", &format!("Configuration lock failed: {e}")))?
                .ok_or_else(|| {
                    error(
                        "ConfigBusy",
                        "Configuration is being changed by another CodeWave instance",
                    )
                })
            })
            .transpose()?;
        self.check_no_external_change()?;
        let mut state = self.state.lock().unwrap();
        self.readable_config(&mut state)?;
        let current = cfg.read().unwrap().clone();
        let mut journal = self.files.read_journal()?;
        if let Some(result) = self.replay(&state, &current, &journal, operation_id)? {
            return Ok(result);
        }
        let mut updated = current.clone();
        let mut edits = HashMap::new();
        let mut skip_cleanup = true;
        let mut page_input = false;
        match patch {
            ConfigPatch::Page {
                config,
                expected,
                edits: input_edits,
                skip_cleanup: skip,
            } => {
                self.check_version(&state, &expected, state.page_version)?;
                updated = *config;
                page_input = true;
                preserve_instant_fields(&mut updated, &current);
                edits = input_edits;
                skip_cleanup = skip;
            }
            ConfigPatch::Role {
                role,
                model_id,
                expected,
            } => {
                let role = crate::agents::find(&role)
                    .map(|agent| agent.name.to_string())
                    .ok_or_else(|| error("InvalidRole", "Unknown delegable role"))?;
                if !crate::agents::DELEGABLE_ROLES.contains(&role.as_str()) {
                    return Err(error("InvalidRole", "Unknown delegable role"));
                }
                self.check_version(
                    &state,
                    &expected,
                    *state.role_versions.get(&role).unwrap_or(&0),
                )?;
                if model_id
                    .as_deref()
                    .is_some_and(|id| current.find_model(id).is_none())
                {
                    return Err(error(
                        "ModelNotFound",
                        "The selected model is no longer saved. Refresh the model list",
                    ));
                }
                updated.subagent_models.insert(role, model_id);
            }
            ConfigPatch::Fonts {
                sans,
                content,
                mono,
                font_size_sans,
                font_size_content,
                font_size_mono,
            } => {
                updated.ui.font_sans = sans.trim().into();
                updated.ui.font_content = content.trim().into();
                updated.ui.font_mono = mono.trim().into();
                updated.ui.font_size_sans = font_size_sans;
                updated.ui.font_size_content = font_size_content;
                updated.ui.font_size_mono = font_size_mono;
            }
            ConfigPatch::Skill { name, disabled } => {
                updated.disabled_skills.retain(|s| s != &name);
                if disabled {
                    updated.disabled_skills.push(name);
                }
            }
            ConfigPatch::AllowCommand { command } => {
                if !updated.approval.command_allowlist.contains(&command) {
                    updated.approval.command_allowlist.push(command);
                }
            }
            ConfigPatch::SecureExisting => {}
        }
        validate_config(&updated, &edits)?;
        let mut warnings = Vec::new();
        let model_ids: HashSet<String> = updated
            .providers
            .iter()
            .flat_map(|p| p.models.iter().map(|m| m.id.clone()))
            .collect();
        for (role, model_id) in &mut updated.subagent_models {
            if model_id.as_ref().is_some_and(|id| !model_ids.contains(id)) {
                *model_id = None;
                warnings.push(format!(
                    "Role {role} now inherits its parent model because its saved model was removed"
                ));
            }
        }
        // Validate/resolve EVERY provider before creating ANY account (append read failure is atomic).
        let mut prepared: Vec<(usize, String, Vec<String>)> = Vec::new();
        for (i, provider) in updated.providers.iter_mut().enumerate() {
            let old = current.providers.iter().find(|p| p.id == provider.id);
            if provider.credential_ref != old.and_then(|p| p.credential_ref.clone()) {
                return Err(error(
                    "InvalidCredentialReference",
                    "Credential references are assigned by the application",
                ));
            }
            let edit = match edits.remove(&provider.id) {
                Some(edit) => edit,
                None if page_input
                    && !old.is_some_and(|old| {
                        provider.keys == old.keys
                            || provider.keys
                                == old
                                    .keys
                                    .iter()
                                    .map(|key| {
                                        if key == KEYRING_PLACEHOLDER {
                                            key.clone()
                                        } else {
                                            ConfigState::mask(key)
                                        }
                                    })
                                    .collect::<Vec<_>>()
                    }) =>
                {
                    legacy_credential_edit(old, &provider.keys)?
                }
                None => CredentialEdit::Keep,
            };
            let pool = match edit {
                CredentialEdit::Keep => {
                    if let Some(old) = old {
                        provider.keys = old.keys.clone();
                        provider.credential_ref = old.credential_ref.clone();
                        provider.credential_count = old.credential_count;
                        if old
                            .keys
                            .iter()
                            .any(|k| !k.is_empty() && k != KEYRING_PLACEHOLDER)
                        {
                            Some(resolve_provider_with(self.credentials.as_ref(), old)?)
                        } else {
                            None
                        }
                    } else {
                        if !provider.keys.is_empty() {
                            return Err(error(
                                "CredentialIntentRequired",
                                "New credentials require an explicit append or replace operation",
                            ));
                        }
                        provider.keys.clear();
                        provider.credential_ref = None;
                        provider.credential_count = Some(0);
                        None
                    }
                }
                CredentialEdit::Append { new_keys } => {
                    let new = normalize_new_keys(new_keys)?;
                    if new.is_empty() {
                        if let Some(old) = old {
                            provider.keys = old.keys.clone();
                            provider.credential_ref = old.credential_ref.clone();
                            provider.credential_count = old.credential_count;
                        }
                        if provider
                            .keys
                            .iter()
                            .any(|k| !k.is_empty() && k != KEYRING_PLACEHOLDER)
                        {
                            Some(resolve_provider_with(self.credentials.as_ref(), provider)?)
                        } else {
                            None
                        }
                    } else {
                        let mut pool = old
                            .map(|p| resolve_provider_with(self.credentials.as_ref(), p))
                            .transpose()?
                            .unwrap_or_default();
                        extend_unique(&mut pool, new);
                        Some(pool)
                    }
                }
                CredentialEdit::Replace { new_keys } => Some(normalize_new_keys(new_keys)?),
                CredentialEdit::Clear => Some(Vec::new()),
            };
            if let Some(pool) = pool {
                if pool.is_empty() {
                    provider.keys.clear();
                    provider.credential_ref = None;
                    provider.credential_count = Some(0);
                } else {
                    let account = format!("{}:credential:{}", provider.id, uuid::Uuid::new_v4());
                    provider.keys = vec![KEYRING_PLACEHOLDER.into()];
                    provider.credential_ref = Some(account.clone());
                    provider.credential_count = Some(pool.len());
                    prepared.push((i, account, pool));
                }
            }
            if provider.keys.is_empty() {
                provider.credential_ref = None;
            }
        }
        assert_persistable(&updated)?;
        let page_changed = page_comparable(&updated) != page_comparable(&current);
        // Discard old non-account receipts during normal operation too. Live generation records
        // remain until startup, because an in-flight model snapshot may still reference them.
        compact_journal(&mut journal)?;
        if journal.records.len() >= MAX_RECORDS {
            return Err(error(
                "RecoveryQueueFull",
                "Credential recovery queue is full. Restart to retry safe cleanup before saving",
            ));
        }
        let version = ConfigVersion {
            boot_id: state.boot_id.clone(),
            sequence: state.sequence + 1,
        };
        let mut accounts: Vec<String> = prepared.iter().map(|(_, a, _)| a.clone()).collect();
        let next_accounts: HashSet<&str> = updated
            .providers
            .iter()
            .filter_map(provider_account)
            .collect();
        // Retire known current references even if their original creation receipt is no longer
        // present. Register before replacement, so a crash keeps every still-current account.
        // Current v2 UUID accounts are known app-owned; nonstandard historical names are retained.
        for account in current.providers.iter().filter_map(provider_account) {
            if !next_accounts.contains(account)
                && valid_cleanup_account(account)
                && !journal
                    .records
                    .iter()
                    .any(|record| record.accounts.iter().any(|a| a == account))
                && !accounts.iter().any(|a| a == account)
            {
                accounts.push(account.into());
            }
        }
        journal.records.push(CommitRecord {
            operation_id: operation_id.into(),
            accounts,
            committed: false,
            post_processing: "pending".into(),
            version: Some(version),
        });
        // Record intent before any secret reaches a new account; failure aborts all preparation.
        self.files.write_journal(&journal)?;
        for (_, account, pool) in &prepared {
            if let Err(e) = self.credentials.write(account, pool).and_then(|_| {
                let readback = self.credentials.read(account)?;
                if readback == *pool {
                    Ok(())
                } else {
                    Err(error(
                        "CredentialVerificationFailed",
                        "Stored credentials could not be verified. Configuration was not changed",
                    ))
                }
            }) {
                self.discard_candidates(&prepared);
                return Err(e);
            }
        }
        updated.last_operation_id = Some(operation_id.into());
        match self.files.write_config(&updated) {
            Ok(durability) => {
                warnings.extend(durability);
                self.note_disk_digest(&updated);
            }
            Err(e) => {
                self.discard_candidates(&prepared);
                return Err(e);
            }
        }
        // Commit point passed: publish exactly what was written before any fallible post-processing.
        state.sequence += 1;
        if page_changed {
            state.page_version += 1;
        }
        for role in crate::agents::DELEGABLE_ROLES {
            if current.subagent_models.get(*role).and_then(|m| m.as_ref())
                != updated.subagent_models.get(*role).and_then(|m| m.as_ref())
            {
                *state.role_versions.entry((*role).into()).or_default() += 1;
            }
        }
        let effects = CommitEffects {
            providers_changed: serde_json::to_value(&current.providers).ok()
                != serde_json::to_value(&updated.providers).ok(),
            proxy_changed: serde_json::to_value(&current.proxy).ok()
                != serde_json::to_value(&updated.proxy).ok(),
            log_changed: current.log.level != updated.log.level,
            cleanup_days: if !skip_cleanup
                && current.sessions.retention_days != updated.sessions.retention_days
            {
                updated
                    .sessions
                    .retention_days
                    .filter(|d| super::sessions::cleanup::is_valid_retention_days(*d))
            } else {
                None
            },
        };
        *cfg.write().unwrap() = updated.clone();
        apply(&updated, &effects);
        if let Some(record) = journal.records.last_mut() {
            record.committed = true;
            if effects.cleanup_days.is_none() {
                record.post_processing = "complete".into();
            }
        }
        if self.files.write_journal(&journal).is_err() {
            warnings.push("Configuration saved; recovery receipt update needs retry".into());
        }
        state
            .warnings
            .retain(|w| !w.starts_with("Existing configuration contains"));
        let mut snapshot = self.snapshot_locked(&state, &updated);
        snapshot.warnings.extend(warnings);
        Ok(ConfigSaveResult {
            snapshot,
            cleanup: None,
            replayed: false,
            effects,
        })
    }
    fn discard_candidates(&self, prepared: &[(usize, String, Vec<String>)]) {
        for (_, account, _) in prepared {
            let _ = self.credentials.delete(account);
        }
        // Failed deletions remain registered in the journal and are retried on next startup.
    }
}

pub fn preserve_instant_fields(updated: &mut ConfigState, current: &ConfigState) {
    updated.ui.font_sans = current.ui.font_sans.clone();
    updated.ui.font_content = current.ui.font_content.clone();
    updated.ui.font_mono = current.ui.font_mono.clone();
    updated.ui.font_size_sans = current.ui.font_size_sans;
    updated.ui.font_size_content = current.ui.font_size_content;
    updated.ui.font_size_mono = current.ui.font_size_mono;
    updated.subagent_models = current.subagent_models.clone();
    updated.last_operation_id = current.last_operation_id.clone();
}
fn history_full() -> ConfigCommitError {
    error(
        "OperationHistoryFull",
        "Operation recovery history is full. Configuration saving is blocked to prevent replay. Keep the recovery record; an application update with explicit history recovery is required. Generating another operation ID will not fix this",
    )
}
fn same_operation(a: &str, b: &str) -> bool {
    uuid::Uuid::parse_str(a)
        .ok()
        .zip(uuid::Uuid::parse_str(b).ok())
        .is_some_and(|(a, b)| a == b)
}

fn operation_positions(operation: &str) -> [usize; 4] {
    let id =
        uuid::Uuid::parse_str(operation).expect("journal only contains validated operation UUIDs");
    let digest = Sha256::digest(id.as_bytes());
    std::array::from_fn(|i| {
        let bytes: [u8; 4] = digest[i * 4..i * 4 + 4].try_into().unwrap();
        u32::from_le_bytes(bytes) as usize % TOMBSTONE_BITS
    })
}
fn expired_operation(journal: &CommitJournal, operation: &str) -> bool {
    !journal.expired_operations.is_empty()
        && operation_positions(operation).iter().all(|bit| {
            journal
                .expired_operations
                .get(bit / 64)
                .is_some_and(|word| word & (1u64 << (bit % 64)) != 0)
        })
}
fn tombstones_saturated(journal: &CommitJournal) -> bool {
    journal
        .expired_operations
        .iter()
        .map(|word| word.count_ones() as usize)
        .sum::<usize>()
        >= TOMBSTONE_BITS / 2
}
fn compact_journal(journal: &mut CommitJournal) -> Result<(), ConfigCommitError> {
    if tombstones_saturated(journal) {
        return Err(history_full());
    }
    let len = journal.records.len();
    let mut index = 0;
    let mut expired = Vec::new();
    journal.records.retain(|record| {
        let keep = !record.accounts.is_empty() || index + 128 >= len;
        if !keep {
            expired.push(record.operation_id.clone());
        }
        index += 1;
        keep
    });
    if !expired.is_empty() && journal.expired_operations.is_empty() {
        journal.expired_operations.resize(TOMBSTONE_WORDS, 0);
    }
    for operation in expired {
        for bit in operation_positions(&operation) {
            journal.expired_operations[bit / 64] |= 1u64 << (bit % 64);
        }
    }
    if tombstones_saturated(journal) {
        return Err(history_full());
    }
    Ok(())
}

fn page_comparable(cfg: &ConfigState) -> serde_json::Value {
    let mut cfg = cfg.clone();
    cfg.last_operation_id = None;
    cfg.subagent_models.clear();
    cfg.ui.font_sans.clear();
    cfg.ui.font_content.clear();
    cfg.ui.font_mono.clear();
    cfg.ui.font_size_sans = super::config::DEFAULT_TEXT_FONT_SIZE;
    cfg.ui.font_size_content = super::config::DEFAULT_TEXT_FONT_SIZE;
    cfg.ui.font_size_mono = super::config::DEFAULT_CODE_FONT_SIZE;
    serde_json::to_value(cfg).unwrap_or_default()
}
fn validate_config(
    cfg: &ConfigState,
    edits: &HashMap<String, CredentialEdit>,
) -> Result<(), ConfigCommitError> {
    if cfg.schema_version != 2 {
        return Err(error(
            "InvalidConfiguration",
            "Unknown configuration schema",
        ));
    }
    if !super::config::is_valid_font_size(cfg.ui.font_size_sans)
        || !super::config::is_valid_font_size(cfg.ui.font_size_content)
        || !super::config::is_valid_font_size(cfg.ui.font_size_mono)
    {
        return Err(error(
            "InvalidConfiguration",
            "Font sizes must be whole pixels from 12 to 20",
        ));
    }
    let mut providers = HashSet::new();
    let mut models = HashSet::new();
    for p in &cfg.providers {
        if p.id.is_empty() || !providers.insert(p.id.as_str()) {
            return Err(error(
                "InvalidProvider",
                "Provider identifiers must be nonempty and unique",
            ));
        }
        // UUID prevents path/namespace confusion in generated credential account names.
        if p.id.contains(":credential:") {
            return Err(error("InvalidProvider", "Invalid provider identifier"));
        }
        super::config::validate_request_headers(&p.headers)
            .map_err(|_| error("InvalidHeaders", "Provider request headers are invalid"))?;
        for m in &p.models {
            if m.id.is_empty() || !models.insert(m.id.as_str()) {
                return Err(error(
                    "InvalidModel",
                    "Model identifiers must be nonempty and unique",
                ));
            }
        }
    }
    if edits.keys().any(|id| !providers.contains(id.as_str())) {
        return Err(error(
            "InvalidCredentialIntent",
            "Credential operation names an unknown provider",
        ));
    }
    Ok(())
}
fn normalize_new_keys(keys: Vec<String>) -> Result<Vec<String>, ConfigCommitError> {
    let mut out = Vec::new();
    for key in keys {
        let key = key.trim();
        if key.contains(['\n', '\r']) || key == KEYRING_PLACEHOLDER || key.starts_with("***") {
            return Err(error(
                "InvalidCredential",
                "Enter new API keys, not saved-state markers or multiline keys",
            ));
        }
        if !key.is_empty() && !out.iter().any(|v| v == key) {
            out.push(key.into());
        }
    }
    Ok(out)
}
fn extend_unique(keys: &mut Vec<String>, additions: Vec<String>) {
    for key in additions {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
}
pub fn resolve_provider_with(
    store: &dyn CredentialStore,
    p: &ProviderConfig,
) -> Result<Vec<String>, ConfigCommitError> {
    if p.keys.is_empty() {
        return Ok(Vec::new());
    }
    // Keep the existing mixed-format effective order (plain first), distinct from explicit append.
    let mut keys = Vec::new();
    extend_unique(
        &mut keys,
        p.keys
            .iter()
            .filter(|k| !k.is_empty() && k.as_str() != KEYRING_PLACEHOLDER)
            .cloned()
            .collect(),
    );
    if p.keys.iter().any(|k| k == KEYRING_PLACEHOLDER) {
        extend_unique(
            &mut keys,
            store.read(p.credential_ref.as_deref().unwrap_or(&p.id))?,
        );
    }
    Ok(keys)
}
fn assert_persistable(cfg: &ConfigState) -> Result<(), ConfigCommitError> {
    if cfg
        .providers
        .iter()
        .any(|p| p.keys.iter().any(|k| k != KEYRING_PLACEHOLDER))
    {
        return Err(error(
            "UnsafeCredentialStorage",
            "API keys must be stored securely before configuration can be written",
        ));
    }
    Ok(())
}
fn provider_account(provider: &ProviderConfig) -> Option<&str> {
    provider
        .keys
        .iter()
        .any(|key| key == KEYRING_PLACEHOLDER)
        .then(|| provider.credential_ref.as_deref().unwrap_or(&provider.id))
}
fn valid_cleanup_account(account: &str) -> bool {
    valid_generated_account(account) || uuid::Uuid::parse_str(account).is_ok()
}

fn valid_generated_account(account: &str) -> bool {
    account
        .rsplit_once(":credential:")
        .is_some_and(|(provider, generation)| {
            !provider.is_empty()
                && !provider.contains(":credential:")
                && uuid::Uuid::parse_str(generation).is_ok()
        })
}

/// One-release adapter for tested internal callers of the old string editor. Unknown/ambiguous
/// masks are conflicts, not silent deletions. The new UI never uses this protocol.
pub fn legacy_credential_edit(
    old: Option<&ProviderConfig>,
    incoming: &[String],
) -> Result<CredentialEdit, ConfigCommitError> {
    let mut new_keys = Vec::new();
    let mut preserve = false;
    for key in incoming {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        if key == KEYRING_PLACEHOLDER {
            if old.is_none_or(|p| !p.keys.iter().any(|k| k == KEYRING_PLACEHOLDER)) {
                return Err(error(
                    "CredentialConflict",
                    "Saved credential marker does not belong to this provider",
                ));
            }
            preserve = true;
        } else if key.starts_with("***") {
            let matches: Vec<_> = old
                .into_iter()
                .flat_map(|p| &p.keys)
                .filter(|k| ConfigState::mask(k) == key && !k.starts_with("***"))
                .collect();
            if matches.len() != 1 {
                return Err(error(
                    "CredentialConflict",
                    "Saved credential mask is stale or ambiguous; refresh before editing",
                ));
            }
            new_keys.push(matches[0].clone());
        } else {
            new_keys.push(key.into());
        }
    }
    Ok(if preserve {
        if new_keys.is_empty() {
            CredentialEdit::Keep
        } else {
            CredentialEdit::Append { new_keys }
        }
    } else if new_keys.is_empty() {
        CredentialEdit::Clear
    } else {
        CredentialEdit::Replace { new_keys }
    })
}

#[cfg(test)]
mod tests {
    use super::super::config::ProviderModel;
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    pub(super) struct FakeCredentials {
        pools: Mutex<HashMap<String, Vec<String>>>,
        reads: AtomicUsize,
        writes: AtomicUsize,
        deletes: AtomicUsize,
        fail_read: AtomicBool,
        fail_write_at: AtomicUsize,
        fail_delete: AtomicBool,
        wrong_readback: AtomicBool,
    }

    impl FakeCredentials {
        /// 已发生的钥匙串写次数。跨模块夹具（`coordination_c0`）要断言「被拒的轮换
        /// 根本没碰过钥匙串」，而计数器字段本身保持私有。
        pub(super) fn write_count(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        /// 预置一个账户里已有的密钥（模拟系统钥匙串里本来就有东西）。
        pub(super) fn seed(&self, account: &str, keys: &[&str]) {
            self.pools
                .lock()
                .unwrap()
                .insert(account.into(), keys.iter().map(|k| (*k).into()).collect());
        }
    }

    impl CredentialStore for FakeCredentials {
        fn read(&self, account: &str) -> Result<Vec<String>, ConfigCommitError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.fail_read.load(Ordering::SeqCst) {
                return Err(error("CredentialReadFailed", "test read failure"));
            }
            if self.wrong_readback.load(Ordering::SeqCst) && account.contains(":credential:") {
                return Ok(vec!["incorrect".into()]);
            }
            self.pools
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .ok_or_else(|| error("CredentialReadFailed", "missing test account"))
        }
        fn write(&self, account: &str, keys: &[String]) -> Result<(), ConfigCommitError> {
            let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_write_at.load(Ordering::SeqCst) == n {
                return Err(error("CredentialWriteFailed", "test write failure"));
            }
            self.pools
                .lock()
                .unwrap()
                .insert(account.into(), keys.to_vec());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), ConfigCommitError> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(error("CredentialCleanupFailed", "test cleanup failure"));
            }
            self.pools.lock().unwrap().remove(account);
            Ok(())
        }
    }
    #[derive(Default)]
    struct FakeFiles {
        config: Mutex<Option<ConfigState>>,
        journal: Mutex<CommitJournal>,
        captured: Mutex<Vec<Vec<u8>>>,
        fail_config: AtomicBool,
        fail_read_config: AtomicBool,
        fail_journal_at: AtomicUsize,
        journals: AtomicUsize,
    }
    impl ConfigFileStore for FakeFiles {
        fn read_config(&self) -> Result<Option<ConfigState>, ConfigCommitError> {
            if self.fail_read_config.load(Ordering::SeqCst) {
                return Err(error("ConfigUnreadable", "test corrupt file"));
            }
            Ok(self.config.lock().unwrap().clone())
        }
        fn write_config(&self, cfg: &ConfigState) -> Result<Vec<String>, ConfigCommitError> {
            assert_persistable(cfg)?;
            self.captured
                .lock()
                .unwrap()
                .push(serde_json::to_vec(cfg).unwrap());
            if self.fail_config.load(Ordering::SeqCst) {
                return Err(error("ConfigWriteFailed", "test replacement failure"));
            }
            *self.config.lock().unwrap() = Some(cfg.clone());
            Ok(Vec::new())
        }
        fn read_journal(&self) -> Result<CommitJournal, ConfigCommitError> {
            Ok(self.journal.lock().unwrap().clone())
        }
        fn write_journal(&self, journal: &CommitJournal) -> Result<(), ConfigCommitError> {
            let n = self.journals.fetch_add(1, Ordering::SeqCst) + 1;
            self.captured
                .lock()
                .unwrap()
                .push(serde_json::to_vec(journal).unwrap());
            if self.fail_journal_at.load(Ordering::SeqCst) == n {
                return Err(error("JournalWriteFailed", "test journal failure"));
            }
            *self.journal.lock().unwrap() = journal.clone();
            Ok(())
        }
    }
    struct Fixture {
        cfg: RwLock<ConfigState>,
        credentials: Arc<FakeCredentials>,
        files: Arc<FakeFiles>,
        coordinator: ConfigCoordinator,
    }
    impl Fixture {
        fn new() -> Self {
            let credentials = Arc::new(FakeCredentials::default());
            credentials.pools.lock().unwrap().insert(
                "p1".into(),
                vec!["test-secret-A".into(), "test-secret-B".into()],
            );
            let mut cfg = ConfigState::default();
            cfg.providers.push(ProviderConfig {
                id: "p1".into(),
                keys: vec![KEYRING_PLACEHOLDER.into()],
                models: vec![
                    ProviderModel {
                        id: "m1".into(),
                        model: "one".into(),
                        ..Default::default()
                    },
                    ProviderModel {
                        id: "m2".into(),
                        model: "two".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            });
            let files = Arc::new(FakeFiles::default());
            *files.config.lock().unwrap() = Some(cfg.clone());
            let coordinator = ConfigCoordinator::new(credentials.clone(), files.clone());
            Self {
                cfg: RwLock::new(cfg),
                credentials,
                files,
                coordinator,
            }
        }
        fn snapshot(&self) -> ConfigSnapshot {
            self.coordinator.snapshot(&self.cfg)
        }
        fn page(&self, edit: CredentialEdit) -> ConfigPatch {
            let s = self.snapshot();
            ConfigPatch::Page {
                config: Box::new(s.config),
                expected: ConfigVersion {
                    boot_id: s.version.boot_id,
                    sequence: s.page_version,
                },
                edits: HashMap::from([("p1".into(), edit)]),
                skip_cleanup: true,
            }
        }
        fn commit(&self, patch: ConfigPatch) -> Result<ConfigSaveResult, ConfigCommitError> {
            self.coordinator
                .commit(&self.cfg, patch, &uuid::Uuid::new_v4().to_string())
        }
        fn pool(&self) -> Vec<String> {
            resolve_provider_with(
                self.credentials.as_ref(),
                &self.cfg.read().unwrap().providers[0],
            )
            .unwrap()
        }
    }
    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).into()).collect()
    }

    #[test]
    fn credentials_never_enter_config_writes() {
        let f = Fixture::new();
        let result = f
            .commit(f.page(CredentialEdit::Append {
                new_keys: keys(&["test-secret-C"]),
            }))
            .unwrap();
        assert_eq!(
            f.pool(),
            keys(&["test-secret-A", "test-secret-B", "test-secret-C"])
        );
        assert_eq!(
            result.snapshot.config.providers[0].credential_count,
            Some(3)
        );
        for bytes in f.files.captured.lock().unwrap().iter() {
            let text = String::from_utf8_lossy(bytes);
            for secret in ["test-secret-A", "test-secret-B", "test-secret-C"] {
                assert!(!text.contains(secret));
            }
        }
        assert_eq!(
            f.credentials.pools.lock().unwrap().get("p1").unwrap(),
            &keys(&["test-secret-A", "test-secret-B"])
        );
    }
    #[test]
    fn append_preserves_existing_pool_and_stable_deduplication() {
        let f = Fixture::new();
        for _ in 0..2 {
            f.commit(f.page(CredentialEdit::Append {
                new_keys: keys(&[" test-secret-B ", "test-secret-C", "test-secret-C", " "]),
            }))
            .unwrap();
        }
        assert_eq!(
            f.pool(),
            keys(&["test-secret-A", "test-secret-B", "test-secret-C"])
        );
    }
    #[test]
    fn keep_does_not_read_or_rewrite_secure_pool() {
        let f = Fixture::new();
        f.credentials.fail_read.store(true, Ordering::SeqCst);
        f.commit(f.page(CredentialEdit::Keep)).unwrap();
        assert_eq!(f.credentials.reads.load(Ordering::SeqCst), 0);
        assert_eq!(f.credentials.writes.load(Ordering::SeqCst), 0);
        assert_eq!(f.cfg.read().unwrap().providers[0].credential_ref, None);
    }
    #[test]
    fn append_read_failure_never_overwrites() {
        let f = Fixture::new();
        f.credentials.fail_read.store(true, Ordering::SeqCst);
        let error = f
            .commit(f.page(CredentialEdit::Append {
                new_keys: keys(&["C"]),
            }))
            .unwrap_err();
        assert_eq!(error.code, "CredentialReadFailed");
        assert_eq!(f.snapshot().version.sequence, 0);
        assert_eq!(f.credentials.writes.load(Ordering::SeqCst), 0);
        assert!(f.files.captured.lock().unwrap().is_empty());
    }
    #[test]
    fn replace_and_clear_are_explicit_and_old_snapshot_survives() {
        let f = Fixture::new();
        f.credentials.fail_read.store(true, Ordering::SeqCst);
        // Replacement does not need old account access (readback of the new account still required).
        f.credentials.fail_read.store(false, Ordering::SeqCst);
        f.credentials.pools.lock().unwrap().remove("p1");
        f.commit(f.page(CredentialEdit::Replace {
            new_keys: keys(&["C"]),
        }))
        .unwrap();
        let old = f.cfg.read().unwrap().providers[0].clone();
        f.commit(f.page(CredentialEdit::Clear)).unwrap();
        assert!(f.pool().is_empty());
        assert!(f.cfg.read().unwrap().providers[0].credential_ref.is_none());
        assert_eq!(
            resolve_provider_with(f.credentials.as_ref(), &old).unwrap(),
            keys(&["C"])
        );
    }
    #[test]
    fn failed_file_commit_keeps_old_credentials_and_memory() {
        let f = Fixture::new();
        f.files.fail_config.store(true, Ordering::SeqCst);
        assert_eq!(
            f.commit(f.page(CredentialEdit::Replace {
                new_keys: keys(&["C"])
            }))
            .unwrap_err()
            .code,
            "ConfigWriteFailed"
        );
        assert_eq!(f.pool(), keys(&["test-secret-A", "test-secret-B"]));
        assert_eq!(f.snapshot().version.sequence, 0);
        assert_eq!(
            f.files.config.lock().unwrap().as_ref().unwrap().providers[0].credential_ref,
            None
        );
    }
    #[test]
    fn partial_prepare_never_publishes() {
        let f = Fixture::new();
        let mut snapshot = f.snapshot();
        snapshot.config.providers.push(ProviderConfig {
            id: "p2".into(),
            ..Default::default()
        });
        f.credentials.fail_write_at.store(2, Ordering::SeqCst);
        let patch = ConfigPatch::Page {
            config: Box::new(snapshot.config),
            expected: snapshot.version,
            edits: HashMap::from([
                (
                    "p1".into(),
                    CredentialEdit::Replace {
                        new_keys: keys(&["C"]),
                    },
                ),
                (
                    "p2".into(),
                    CredentialEdit::Replace {
                        new_keys: keys(&["D"]),
                    },
                ),
            ]),
            skip_cleanup: true,
        };
        assert_eq!(f.commit(patch).unwrap_err().code, "CredentialWriteFailed");
        assert_eq!(f.cfg.read().unwrap().providers.len(), 1);
        assert_eq!(f.pool(), keys(&["test-secret-A", "test-secret-B"]));
        assert_eq!(f.credentials.pools.lock().unwrap().len(), 1);
    }
    #[test]
    fn failed_verification_and_journal_never_publish() {
        let f = Fixture::new();
        f.credentials.wrong_readback.store(true, Ordering::SeqCst);
        assert_eq!(
            f.commit(f.page(CredentialEdit::Replace {
                new_keys: keys(&["C"])
            }))
            .unwrap_err()
            .code,
            "CredentialVerificationFailed"
        );
        assert_eq!(f.snapshot().version.sequence, 0);
        let f = Fixture::new();
        f.files.fail_journal_at.store(1, Ordering::SeqCst);
        assert_eq!(
            f.commit(f.page(CredentialEdit::Replace {
                new_keys: keys(&["C"])
            }))
            .unwrap_err()
            .code,
            "JournalWriteFailed"
        );
        assert_eq!(f.credentials.writes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn startup_uses_same_secure_commit_and_mixed_effective_order() {
        let f = Fixture::new();
        f.cfg.write().unwrap().providers[0].keys =
            keys(&[KEYRING_PLACEHOLDER, "plain-test-C", "test-secret-B"]);
        f.commit(ConfigPatch::SecureExisting).unwrap();
        assert_eq!(
            f.pool(),
            keys(&["plain-test-C", "test-secret-B", "test-secret-A"])
        );
        for bytes in f.files.captured.lock().unwrap().iter() {
            assert!(!String::from_utf8_lossy(bytes).contains("plain-test-C"));
        }
    }
    #[test]
    fn failed_startup_or_font_write_never_copies_existing_plaintext() {
        for is_font in [false, true] {
            let f = Fixture::new();
            f.cfg.write().unwrap().providers[0].keys = keys(&["plain-test-original"]);
            f.credentials.fail_write_at.store(1, Ordering::SeqCst);
            let patch = if is_font {
                ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "S".into(),
                    mono: "M".into(),
                    font_size_sans: 15.0,
                    font_size_content: 14.0,
                    font_size_mono: 15.0,
                }
            } else {
                ConfigPatch::SecureExisting
            };
            assert!(f.commit(patch).is_err());
            assert_eq!(
                f.cfg.read().unwrap().providers[0].keys,
                keys(&["plain-test-original"])
            );
            for bytes in f.files.captured.lock().unwrap().iter() {
                assert!(!String::from_utf8_lossy(bytes).contains("plain-test-original"));
            }
        }
    }
    #[test]
    fn restart_preserves_referenced_accounts_and_collects_orphans() {
        let f = Fixture::new();
        f.commit(f.page(CredentialEdit::Replace {
            new_keys: keys(&["first"]),
        }))
        .unwrap();
        let first = f.cfg.read().unwrap().providers[0]
            .credential_ref
            .clone()
            .unwrap();
        f.commit(f.page(CredentialEdit::Replace {
            new_keys: keys(&["second"]),
        }))
        .unwrap();
        let second = f.cfg.read().unwrap().providers[0]
            .credential_ref
            .clone()
            .unwrap();
        assert!(f.credentials.pools.lock().unwrap().contains_key(&first));
        let restarted = ConfigCoordinator::new(f.credentials.clone(), f.files.clone());
        restarted.recover().unwrap();
        assert!(!f.credentials.pools.lock().unwrap().contains_key(&first));
        assert!(f.credentials.pools.lock().unwrap().contains_key(&second));
        assert_eq!(f.pool(), keys(&["second"]));
    }
    #[test]
    fn corrupt_configuration_prevents_destructive_recovery() {
        let f = Fixture::new();
        f.commit(f.page(CredentialEdit::Replace {
            new_keys: keys(&["C"]),
        }))
        .unwrap();
        f.files.fail_read_config.store(true, Ordering::SeqCst);
        assert!(f.coordinator.recover().is_err());
        assert_eq!(f.credentials.deletes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn lost_response_replays_receipt_without_credentials_or_cleanup() {
        let f = Fixture::new();
        let op = uuid::Uuid::new_v4().to_string();
        let result = f
            .coordinator
            .commit(
                &f.cfg,
                f.page(CredentialEdit::Append {
                    new_keys: keys(&["C"]),
                }),
                &op,
            )
            .unwrap();
        let writes = f.credentials.writes.load(Ordering::SeqCst);
        let replay = f
            .coordinator
            .commit(&f.cfg, f.page(CredentialEdit::Clear), &op)
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.snapshot.version, result.snapshot.version);
        assert!(replay.effects.cleanup_days.is_none());
        assert_eq!(f.credentials.writes.load(Ordering::SeqCst), writes);
        assert_eq!(f.pool(), keys(&["test-secret-A", "test-secret-B", "C"]));
    }
    #[test]
    fn crash_after_file_commit_reconciles_using_config_operation_id() {
        let f = Fixture::new();
        f.files.fail_journal_at.store(2, Ordering::SeqCst);
        let op = uuid::Uuid::new_v4().to_string();
        let saved = f
            .coordinator
            .commit(
                &f.cfg,
                f.page(CredentialEdit::Replace {
                    new_keys: keys(&["C"]),
                }),
                &op,
            )
            .unwrap();
        assert!(!saved.snapshot.warnings.is_empty());
        let restarted = ConfigCoordinator::new(f.credentials.clone(), f.files.clone());
        let replay = restarted.operation(&f.cfg, &op).unwrap();
        assert!(replay.replayed);
        assert_eq!(f.pool(), keys(&["C"]));
    }
    #[test]
    fn stale_page_conflicts_but_roles_and_fonts_merge() {
        let f = Fixture::new();
        let mut stale = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, .. } = &mut stale {
            config.log.level = "debug".into();
        }
        let token = f.snapshot().version;
        f.commit(ConfigPatch::Role {
            role: "backend-dev".into(),
            model_id: Some("m2".into()),
            expected: token,
        })
        .unwrap();
        f.commit(ConfigPatch::Fonts {
            content: "Content".into(),
            sans: "Sans".into(),
            mono: "Mono".into(),
            font_size_sans: 19.0,
            font_size_content: 18.0,
            font_size_mono: 13.0,
        })
        .unwrap();
        assert_eq!(f.snapshot().page_version, 0);
        let saved = f.commit(stale).unwrap();
        assert_eq!(
            saved.snapshot.config.subagent_models["backend-dev"].as_deref(),
            Some("m2")
        );
        assert_eq!(saved.snapshot.config.ui.font_sans, "Sans");
        assert_eq!(saved.snapshot.config.ui.font_content, "Content");
        assert_eq!(saved.snapshot.config.ui.font_mono, "Mono");
        assert_eq!(saved.snapshot.config.ui.font_size_sans, 19.0);
        assert_eq!(saved.snapshot.config.ui.font_size_content, 18.0);
        assert_eq!(saved.snapshot.config.ui.font_size_mono, 13.0);
        let stale = f.page(CredentialEdit::Keep);
        f.commit(ConfigPatch::AllowCommand {
            command: "cwd\u{1}git status".into(),
        })
        .unwrap();
        assert_eq!(f.commit(stale).unwrap_err().code, "ConfigConflict");
    }
    #[test]
    fn invalid_font_sizes_never_commit() {
        for size in [
            0.0,
            -1.0,
            11.0,
            21.0,
            12.5,
            100000.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ] {
            for (sans_size, content_size, mono_size) in
                [(size, 14.0, 12.0), (14.0, size, 12.0), (14.0, 14.0, size)]
            {
                let f = Fixture::new();
                let result = f.commit(ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "Sans".into(),
                    mono: "Mono".into(),
                    font_size_sans: sans_size,
                    font_size_content: content_size,
                    font_size_mono: mono_size,
                });
                assert_eq!(result.unwrap_err().code, "InvalidConfiguration");
                assert_eq!(f.snapshot().version.sequence, 0);
                assert!(f.snapshot().config.ui.font_sans.is_empty());
                assert!(f.snapshot().config.ui.font_content.is_empty());
                assert!(f.snapshot().config.ui.font_mono.is_empty());
                assert_eq!(f.snapshot().config.ui.font_size_sans, 14.0);
                assert_eq!(f.snapshot().config.ui.font_size_content, 14.0);
                assert_eq!(f.snapshot().config.ui.font_size_mono, 12.0);
                assert!(f.files.captured.lock().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn all_font_size_options_and_reset_commit_without_advancing_page_version() {
        let f = Fixture::new();
        for size in super::super::config::VALID_FONT_SIZES {
            let saved = f
                .commit(ConfigPatch::Fonts {
                    sans: "Sans".into(),
                    content: "Content".into(),
                    mono: "Mono".into(),
                    font_size_sans: size,
                    font_size_content: size,
                    font_size_mono: size,
                })
                .unwrap();
            assert_eq!(saved.snapshot.page_version, 0);
            assert_eq!(saved.snapshot.config.ui.font_size_sans, size);
            assert_eq!(saved.snapshot.config.ui.font_size_content, size);
            assert_eq!(saved.snapshot.config.ui.font_size_mono, size);
        }
        let defaults = super::super::config::UiPrefs::default();
        let reset = f
            .commit(ConfigPatch::Fonts {
                sans: defaults.font_sans.clone(),
                content: defaults.font_content.clone(),
                mono: defaults.font_mono.clone(),
                font_size_sans: defaults.font_size_sans,
                font_size_content: defaults.font_size_content,
                font_size_mono: defaults.font_size_mono,
            })
            .unwrap();
        assert_eq!(reset.snapshot.page_version, 0);
        assert_eq!(
            serde_json::to_value(&reset.snapshot.config.ui).unwrap(),
            serde_json::to_value(defaults).unwrap(),
        );
    }

    #[test]
    fn font_prefs_persist_and_replayed_operation_keeps_newer_three_slots() {
        let f = Fixture::new();
        let operation = uuid::Uuid::new_v4().to_string();
        let original = || ConfigPatch::Fonts {
            sans: "  First Sans  ".into(),
            content: "  First Content  ".into(),
            mono: "  First Mono  ".into(),
            font_size_sans: 13.0,
            font_size_content: 17.0,
            font_size_mono: 15.0,
        };
        let saved = f
            .coordinator
            .commit(&f.cfg, original(), &operation)
            .unwrap();
        assert_eq!(saved.snapshot.config.ui.font_sans, "First Sans");
        assert_eq!(saved.snapshot.config.ui.font_content, "First Content");
        assert_eq!(saved.snapshot.config.ui.font_mono, "First Mono");
        let disk_dir = tempfile::tempdir().unwrap();
        let disk = DiskConfigStore::new(disk_dir.path().to_path_buf());
        disk.write_config(&saved.snapshot.config).unwrap();
        let persisted = disk.read_config().unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&persisted.ui).unwrap(),
            serde_json::to_value(&saved.snapshot.config.ui).unwrap(),
        );

        let newest = f
            .commit(ConfigPatch::Fonts {
                sans: "Latest Sans".into(),
                content: "Latest Content".into(),
                mono: "Latest Mono".into(),
                font_size_sans: 20.0,
                font_size_content: 18.0,
                font_size_mono: 12.0,
            })
            .unwrap();
        let writes = f.files.captured.lock().unwrap().len();
        let replay = f
            .coordinator
            .commit(&f.cfg, original(), &operation)
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            replay.snapshot.version.sequence,
            newest.snapshot.version.sequence
        );
        assert_eq!(
            serde_json::to_value(&replay.snapshot.config.ui).unwrap(),
            serde_json::to_value(&newest.snapshot.config.ui).unwrap(),
        );
        assert_eq!(f.files.captured.lock().unwrap().len(), writes);
    }

    #[test]
    fn failed_font_write_keeps_all_six_saved_fields() {
        let f = Fixture::new();
        let saved = f
            .commit(ConfigPatch::Fonts {
                sans: "Saved Sans".into(),
                content: "Saved Content".into(),
                mono: "Saved Mono".into(),
                font_size_sans: 16.0,
                font_size_content: 18.0,
                font_size_mono: 13.0,
            })
            .unwrap();
        f.files.fail_config.store(true, Ordering::SeqCst);
        let failed = f
            .commit(ConfigPatch::Fonts {
                sans: "Failed Sans".into(),
                content: "Failed Content".into(),
                mono: "Failed Mono".into(),
                font_size_sans: 20.0,
                font_size_content: 19.0,
                font_size_mono: 12.0,
            })
            .unwrap_err();
        assert_eq!(failed.code, "ConfigWriteFailed");
        assert_eq!(
            f.snapshot().version.sequence,
            saved.snapshot.version.sequence
        );
        assert_eq!(
            serde_json::to_value(&f.snapshot().config.ui).unwrap(),
            serde_json::to_value(&saved.snapshot.config.ui).unwrap(),
        );
        assert_eq!(
            serde_json::to_value(&f.files.read_config().unwrap().unwrap().ui).unwrap(),
            serde_json::to_value(&saved.snapshot.config.ui).unwrap(),
        );
    }

    #[test]
    fn different_role_concurrency_merges_and_same_role_conflicts() {
        let f = Arc::new(Fixture::new());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let token = f.snapshot().version;
        let handles: Vec<_> = ["backend-dev", "frontend-dev"]
            .into_iter()
            .map(|role| {
                let f = f.clone();
                let barrier = barrier.clone();
                let token = token.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    f.commit(ConfigPatch::Role {
                        role: role.into(),
                        model_id: Some("m2".into()),
                        expected: token,
                    })
                    .unwrap();
                })
            })
            .collect();
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(f.cfg.read().unwrap().subagent_models.len(), 2);
        assert_eq!(
            f.commit(ConfigPatch::Role {
                role: "backend-dev".into(),
                model_id: None,
                expected: token
            })
            .unwrap_err()
            .code,
            "ConfigConflict"
        );
    }
    #[test]
    fn concurrent_append_uses_domain_conflict_instead_of_last_writer_wins() {
        let f = Arc::new(Fixture::new());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let patches = [
            f.page(CredentialEdit::Append {
                new_keys: keys(&["C"]),
            }),
            f.page(CredentialEdit::Append {
                new_keys: keys(&["D"]),
            }),
        ];
        let handles: Vec<_> = patches
            .into_iter()
            .map(|patch| {
                let f = f.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    f.commit(patch)
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results.iter().find_map(|r| r.as_ref().err()).unwrap().code,
            "ConfigConflict"
        );
        assert_eq!(&f.pool()[..2], &keys(&["test-secret-A", "test-secret-B"]));
    }
    #[test]
    fn model_deletion_normalizes_role_and_bumps_only_its_domain() {
        let f = Fixture::new();
        let token = f.snapshot().version;
        f.commit(ConfigPatch::Role {
            role: "backend-dev".into(),
            model_id: Some("m2".into()),
            expected: token,
        })
        .unwrap();
        let mut patch = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, .. } = &mut patch {
            config.providers[0].models.retain(|m| m.id != "m2");
        }
        let saved = f.commit(patch).unwrap();
        assert_eq!(saved.snapshot.config.subagent_models["backend-dev"], None);
        assert_eq!(saved.snapshot.role_versions["backend-dev"], 2);
        assert!(!saved.snapshot.warnings.is_empty());
        assert!(!saved.effects.proxy_changed);
    }
    #[test]
    fn boot_tokens_reject_old_clients_and_unknown_operations() {
        let f = Fixture::new();
        let stale = f.page(CredentialEdit::Keep);
        let restarted = ConfigCoordinator::new(f.credentials.clone(), f.files.clone());
        assert_eq!(
            restarted
                .commit(&f.cfg, stale, &uuid::Uuid::new_v4().to_string())
                .unwrap_err()
                .code,
            "ConfigConflict"
        );
        assert_eq!(
            restarted
                .operation(&f.cfg, &uuid::Uuid::new_v4().to_string())
                .unwrap_err()
                .code,
            "OperationUnknown"
        );
    }
    #[test]
    fn provider_identity_and_credential_references_cannot_be_forged() {
        let f = Fixture::new();
        let mut patch = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, .. } = &mut patch {
            config.providers[0].credential_ref = Some("another-provider".into());
        }
        assert_eq!(
            f.commit(patch).unwrap_err().code,
            "InvalidCredentialReference"
        );
        let mut patch = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, .. } = &mut patch {
            config.providers.push(config.providers[0].clone());
        }
        assert_eq!(f.commit(patch).unwrap_err().code, "InvalidProvider");
    }
    #[test]
    fn legacy_masks_reject_ambiguity_and_sentinels_cannot_create_pools() {
        let old = ProviderConfig {
            keys: keys(&["first1234", "other1234"]),
            ..Default::default()
        };
        assert!(legacy_credential_edit(Some(&old), &keys(&["***1234"])).is_err());
        assert!(legacy_credential_edit(None, &keys(&[KEYRING_PLACEHOLDER])).is_err());
        assert!(normalize_new_keys(keys(&[KEYRING_PLACEHOLDER])).is_err());
        assert!(normalize_new_keys(keys(&["a\nb"])).is_err());
    }
    #[test]
    fn disk_store_rejects_plaintext_and_never_creates_backups() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiskConfigStore::new(dir.path().to_path_buf());
        let mut cfg = ConfigState::default();
        cfg.providers.push(ProviderConfig {
            keys: keys(&["sentinel-secret"]),
            ..Default::default()
        });
        assert_eq!(
            store.write_config(&cfg).unwrap_err().code,
            "UnsafeCredentialStorage"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        std::fs::write(
            dir.path().join("config.json"),
            b"existing plaintext original",
        )
        .unwrap();
        cfg.providers[0].keys.clear();
        store.write_config(&cfg).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(
            !String::from_utf8_lossy(&std::fs::read(dir.path().join("config.json")).unwrap())
                .contains("plaintext")
        );
    }
    #[test]
    fn page_role_font_writers_share_one_linearized_commit_point() {
        let f = Arc::new(Fixture::new());
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut page = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, .. } = &mut page {
            config.log.level = "debug".into();
        }
        let token = f.snapshot().version;
        let patches = [
            page,
            ConfigPatch::Role {
                role: "backend-dev".into(),
                model_id: Some("m2".into()),
                expected: token,
            },
            ConfigPatch::Fonts {
                content: "Content".into(),
                sans: "Concurrent Sans".into(),
                mono: "Concurrent Mono".into(),
                font_size_sans: 15.0,
                font_size_content: 14.0,
                font_size_mono: 15.0,
            },
        ];
        let handles: Vec<_> = patches
            .into_iter()
            .map(|patch| {
                let f = f.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    f.commit(patch).unwrap();
                })
            })
            .collect();
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }
        let saved = f.snapshot();
        assert_eq!(saved.config.log.level, "debug");
        assert_eq!(
            saved.config.subagent_models["backend-dev"].as_deref(),
            Some("m2")
        );
        assert_eq!(saved.config.ui.font_sans, "Concurrent Sans");
        assert_eq!(saved.config.ui.font_content, "Content");
        assert_eq!(saved.config.ui.font_mono, "Concurrent Mono");
        assert_eq!(saved.config.ui.font_size_sans, 15.0);
        assert_eq!(saved.config.ui.font_size_content, 14.0);
        assert_eq!(saved.config.ui.font_size_mono, 15.0);
        assert_eq!(saved.page_version, 1);
        assert_eq!(saved.version.sequence, 3);
        let disk = f.files.config.lock().unwrap().clone().unwrap();
        assert_eq!(
            serde_json::to_value(&disk).unwrap(),
            serde_json::to_value(f.cfg.read().unwrap().clone()).unwrap()
        );
    }

    #[test]
    fn failed_allowlist_commit_does_not_expand_persistent_authorization() {
        let f = Fixture::new();
        f.files.fail_config.store(true, Ordering::SeqCst);
        assert!(
            f.commit(ConfigPatch::AllowCommand {
                command: "cwd\u{1}write".into()
            })
            .is_err()
        );
        assert!(f.cfg.read().unwrap().approval.command_allowlist.is_empty());
        assert!(
            f.files
                .config
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .approval
                .command_allowlist
                .is_empty()
        );
        let f = Fixture::new();
        for _ in 0..2 {
            f.commit(ConfigPatch::AllowCommand {
                command: "cwd\u{1}write".into(),
            })
            .unwrap();
        }
        assert_eq!(f.cfg.read().unwrap().approval.command_allowlist.len(), 1);
        assert_eq!(f.snapshot().page_version, 1);
    }

    #[test]
    fn failed_gc_is_visible_and_retried_without_reviving_credentials() {
        let f = Fixture::new();
        f.commit(f.page(CredentialEdit::Replace {
            new_keys: keys(&["old-generation"]),
        }))
        .unwrap();
        f.commit(f.page(CredentialEdit::Clear)).unwrap();
        f.credentials.fail_delete.store(true, Ordering::SeqCst);
        f.coordinator.recover().unwrap();
        assert!(!f.snapshot().warnings.is_empty());
        assert!(f.pool().is_empty());
        f.credentials.fail_delete.store(false, Ordering::SeqCst);
        f.coordinator.recover().unwrap();
        assert!(f.pool().is_empty());
        assert_eq!(f.credentials.pools.lock().unwrap().len(), 1); // legacy provider account is conservative
    }

    #[test]
    fn real_save_adapter_preserves_append_and_rejects_stale_masks() {
        let f = Fixture::new();
        let mut page = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, edits, .. } = &mut page {
            edits.clear();
            config.providers[0].keys.push("C".into());
        }
        f.commit(page).unwrap();
        assert_eq!(f.pool(), keys(&["test-secret-A", "test-secret-B", "C"]));
        let mut page = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { config, edits, .. } = &mut page {
            edits.clear();
            config.providers[0].keys = keys(&["***missing"]);
        }
        assert_eq!(f.commit(page).unwrap_err().code, "CredentialConflict");
    }

    // Persistent fake keyring permits child-process crashes without OS/user credentials. Its
    // files deliberately represent the keyring, and are never used as a production fallback.
    struct ProcessCredentials {
        root: PathBuf,
        crash_after_write: bool,
    }
    impl ProcessCredentials {
        fn path(&self, account: &str) -> PathBuf {
            self.root.join(
                account
                    .as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            )
        }
    }
    impl CredentialStore for ProcessCredentials {
        fn read(&self, account: &str) -> Result<Vec<String>, ConfigCommitError> {
            std::fs::read(self.path(account))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or_else(|| error("CredentialReadFailed", "fake process store missing"))
        }
        fn write(&self, account: &str, keys: &[String]) -> Result<(), ConfigCommitError> {
            std::fs::create_dir_all(&self.root).unwrap();
            replace_without_backup(&self.path(account), &serde_json::to_vec(keys).unwrap())?;
            if self.crash_after_write && account.contains(":credential:") {
                std::process::exit(87);
            }
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), ConfigCommitError> {
            match std::fs::remove_file(self.path(account)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(error(
                    "CredentialCleanupFailed",
                    "fake process store deletion failed",
                )),
            }
        }
    }
    struct CrashFiles {
        disk: DiskConfigStore,
        phase: String,
    }
    impl ConfigFileStore for CrashFiles {
        fn read_config(&self) -> Result<Option<ConfigState>, ConfigCommitError> {
            self.disk.read_config()
        }
        fn write_config(&self, cfg: &ConfigState) -> Result<Vec<String>, ConfigCommitError> {
            let result = self.disk.write_config(cfg)?;
            if self.phase == "after_config" {
                std::process::exit(87);
            }
            Ok(result)
        }
        fn read_journal(&self) -> Result<CommitJournal, ConfigCommitError> {
            self.disk.read_journal()
        }
        fn write_journal(&self, journal: &CommitJournal) -> Result<(), ConfigCommitError> {
            self.disk.write_journal(journal)?;
            if self.phase == "after_intent" {
                std::process::exit(87);
            }
            Ok(())
        }
    }
    #[test]
    fn crash_recovery_worker() {
        let Some(root) = std::env::var_os("CODEWAVE_TEST_COMMIT_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let phase = std::env::var("CODEWAVE_TEST_COMMIT_CRASH_PHASE").unwrap();
        let files = Arc::new(CrashFiles {
            disk: DiskConfigStore::new(root.clone()),
            phase: phase.clone(),
        });
        let cfg = RwLock::new(files.read_config().unwrap().unwrap());
        let coordinator = ConfigCoordinator::new(
            Arc::new(ProcessCredentials {
                root: root.join("fake-keyring"),
                crash_after_write: phase == "after_account",
            }),
            files,
        );
        let snapshot = coordinator.snapshot(&cfg);
        coordinator
            .commit(
                &cfg,
                ConfigPatch::Page {
                    config: Box::new(snapshot.config),
                    expected: snapshot.version,
                    edits: HashMap::from([(
                        "p1".into(),
                        CredentialEdit::Replace {
                            new_keys: keys(&["crash-new-secret"]),
                        },
                    )]),
                    skip_cleanup: true,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        panic!("crash phase did not terminate process");
    }
    #[test]
    fn process_crashes_before_and_after_commit_recover_whole_pools() {
        for phase in ["after_intent", "after_account", "after_config"] {
            let dir = tempfile::tempdir().unwrap();
            let files = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
            let credentials = Arc::new(ProcessCredentials {
                root: dir.path().join("fake-keyring"),
                crash_after_write: false,
            });
            credentials
                .write("p1", &keys(&["crash-old-secret"]))
                .unwrap();
            let mut cfg = ConfigState::default();
            cfg.providers.push(ProviderConfig {
                id: "p1".into(),
                keys: keys(&[KEYRING_PLACEHOLDER]),
                ..Default::default()
            });
            files.write_config(&cfg).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "core::config_commit::tests::crash_recovery_worker",
                    "--nocapture",
                ])
                .env("CODEWAVE_TEST_COMMIT_CRASH_ROOT", dir.path())
                .env("CODEWAVE_TEST_COMMIT_CRASH_PHASE", phase)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(87));
            let coordinator = ConfigCoordinator::new(credentials.clone(), files.clone());
            coordinator.recover().unwrap();
            let recovered = files.read_config().unwrap().unwrap();
            let expected = if phase == "after_config" {
                "crash-new-secret"
            } else {
                "crash-old-secret"
            };
            assert_eq!(
                resolve_provider_with(credentials.as_ref(), &recovered.providers[0]).unwrap(),
                keys(&[expected])
            );
            for file in ["config.json", "credential-commits.json"] {
                let bytes = std::fs::read(dir.path().join(file)).unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
            }
            assert!(!dir.path().join("config.bak").exists());
        }
    }
    #[test]
    fn input_dto_rejects_duplicate_or_unknown_credential_operations() {
        assert!(
            serde_json::from_str::<CredentialEdits>(
                r#"{"p1":{"kind":"keep"},"p1":{"kind":"clear"}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<CredentialEdits>(
                r#"{"p1":{"kind":"replace","new_keys":["new"],"account":"other"}}"#
            )
            .is_err()
        );
        let f = Fixture::new();
        let mut page = f.page(CredentialEdit::Keep);
        if let ConfigPatch::Page { edits, .. } = &mut page {
            edits.insert("missing".into(), CredentialEdit::Clear);
        }
        assert_eq!(f.commit(page).unwrap_err().code, "InvalidCredentialIntent");
        assert!(f.files.captured.lock().unwrap().is_empty());
    }
    #[test]
    fn ordinary_writes_compact_receipts_without_losing_live_generations() {
        let f = Fixture::new();
        f.commit(f.page(CredentialEdit::Replace {
            new_keys: keys(&["live-secret"]),
        }))
        .unwrap();
        let live = f.cfg.read().unwrap().providers[0]
            .credential_ref
            .clone()
            .unwrap();
        for i in 0..600 {
            f.commit(ConfigPatch::Fonts {
                content: "Content".into(),
                sans: format!("Font {i}"),
                mono: String::new(),
                font_size_sans: 15.0,
                font_size_content: 14.0,
                font_size_mono: 15.0,
            })
            .unwrap();
        }
        let journal = f.files.journal.lock().unwrap();
        assert!(journal.records.len() <= 130);
        assert!(
            journal
                .records
                .iter()
                .any(|record| record.accounts.contains(&live))
        );
        assert_eq!(f.pool(), keys(&["live-secret"]));
    }
    #[test]
    fn retired_current_uuid_accounts_wait_for_restart_and_failed_commit_keeps_them() {
        for fail_commit in [false, true] {
            let f = Fixture::new();
            let id = uuid::Uuid::new_v4().to_string();
            f.cfg.write().unwrap().providers[0].id = id.clone();
            f.credentials
                .pools
                .lock()
                .unwrap()
                .insert(id.clone(), keys(&["uuid-old-secret"]));
            *f.files.config.lock().unwrap() = Some(f.cfg.read().unwrap().clone());
            f.files.fail_config.store(fail_commit, Ordering::SeqCst);
            let snapshot = f.snapshot();
            let result = f.commit(ConfigPatch::Page {
                config: Box::new(snapshot.config),
                expected: snapshot.version,
                edits: HashMap::from([(
                    id.clone(),
                    CredentialEdit::Replace {
                        new_keys: keys(&["uuid-new-secret"]),
                    },
                )]),
                skip_cleanup: true,
            });
            assert_eq!(result.is_err(), fail_commit);
            assert!(f.credentials.pools.lock().unwrap().contains_key(&id));
            f.coordinator.recover().unwrap();
            assert_eq!(
                f.credentials.pools.lock().unwrap().contains_key(&id),
                fail_commit
            );
            assert_eq!(
                f.pool(),
                keys(&[if fail_commit {
                    "uuid-old-secret"
                } else {
                    "uuid-new-secret"
                }])
            );
        }
    }
    #[test]
    fn unreadable_and_future_config_block_all_writers_and_preserve_credentials() {
        for bad_bytes in [
            b"{ invalid json".as_slice(),
            br#"{"schema_version":99,"providers":[]}"#.as_slice(),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let files = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
            let credentials = Arc::new(FakeCredentials::default());
            let account = format!("p1:credential:{}", uuid::Uuid::new_v4());
            credentials
                .pools
                .lock()
                .unwrap()
                .insert(account.clone(), keys(&["protected-secret"]));
            let mut authoritative = ConfigState::default();
            authoritative.providers.push(ProviderConfig {
                id: "p1".into(),
                credential_ref: Some(account.clone()),
                keys: keys(&[KEYRING_PLACEHOLDER]),
                ..Default::default()
            });
            files.write_config(&authoritative).unwrap();
            files
                .write_journal(&CommitJournal {
                    records: vec![CommitRecord {
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        accounts: vec![account.clone()],
                        committed: true,
                        post_processing: "complete".into(),
                        version: None,
                    }],
                    ..Default::default()
                })
                .unwrap();
            std::fs::write(dir.path().join("config.json"), bad_bytes).unwrap();
            // Same fallback shape as tolerant ConfigState::load: it must NEVER authorize a write.
            let fallback = RwLock::new(ConfigState::default());
            let coordinator = ConfigCoordinator::new(credentials.clone(), files.clone());
            assert_eq!(coordinator.recover().unwrap_err().code, "ConfigUnreadable");
            let token = coordinator.snapshot(&fallback).version;
            let patches = [
                ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "font-migration".into(),
                    mono: String::new(),
                    font_size_sans: 15.0,
                    font_size_content: 14.0,
                    font_size_mono: 15.0,
                },
                ConfigPatch::Skill {
                    name: "skill".into(),
                    disabled: true,
                },
                ConfigPatch::AllowCommand {
                    command: "cmd".into(),
                },
                ConfigPatch::SecureExisting,
                ConfigPatch::Page {
                    config: Box::default(),
                    expected: token,
                    edits: HashMap::new(),
                    skip_cleanup: true,
                },
            ];
            for patch in patches {
                assert_eq!(
                    coordinator
                        .commit(&fallback, patch, &uuid::Uuid::new_v4().to_string())
                        .unwrap_err()
                        .code,
                    "ConfigUnreadable"
                );
                assert_eq!(
                    std::fs::read(dir.path().join("config.json")).unwrap(),
                    bad_bytes
                );
            }
            assert_eq!(credentials.deletes.load(Ordering::SeqCst), 0);
            assert!(credentials.pools.lock().unwrap().contains_key(&account));
            // Repair alone must not permit stale fallback memory to overwrite the recovered file.
            files.write_config(&authoritative).unwrap();
            assert!(
                coordinator
                    .commit(
                        &fallback,
                        ConfigPatch::Fonts {
                            content: "Content".into(),
                            sans: "stale".into(),
                            mono: String::new(),
                            font_size_sans: 15.0,
                            font_size_content: 14.0,
                            font_size_mono: 15.0,
                        },
                        &uuid::Uuid::new_v4().to_string()
                    )
                    .is_err()
            );
            let restarted = ConfigCoordinator::new(credentials.clone(), files.clone());
            restarted.recover().unwrap();
            assert!(credentials.pools.lock().unwrap().contains_key(&account));
            let loaded = RwLock::new(files.read_config().unwrap().unwrap());
            restarted
                .commit(
                    &loaded,
                    ConfigPatch::Fonts {
                        content: "Content".into(),
                        sans: "recovered".into(),
                        mono: String::new(),
                        font_size_sans: 15.0,
                        font_size_content: 14.0,
                        font_size_mono: 15.0,
                    },
                    &uuid::Uuid::new_v4().to_string(),
                )
                .unwrap();
            assert_eq!(
                resolve_provider_with(credentials.as_ref(), &loaded.read().unwrap().providers[0])
                    .unwrap(),
                keys(&["protected-secret"])
            );
        }
    }

    #[test]
    fn file_becoming_unreadable_after_startup_blocks_even_before_recover_is_called() {
        let f = Fixture::new();
        f.files.fail_read_config.store(true, Ordering::SeqCst);
        assert_eq!(
            f.commit(ConfigPatch::Fonts {
                content: "Content".into(),
                sans: "blocked".into(),
                mono: String::new(),
                font_size_sans: 15.0,
                font_size_content: 14.0,
                font_size_mono: 15.0,
            })
            .unwrap_err()
            .code,
            "ConfigUnreadable"
        );
        f.files.fail_read_config.store(false, Ordering::SeqCst);
        assert_eq!(
            f.commit(ConfigPatch::AllowCommand {
                command: "blocked".into()
            })
            .unwrap_err()
            .code,
            "ConfigUnreadable"
        );
        assert!(f.files.captured.lock().unwrap().is_empty());
        assert_eq!(f.snapshot().version.sequence, 0);
        assert!(!f.snapshot().warnings.is_empty());
    }
    #[test]
    fn expired_font_operation_never_replays_after_receipt_compaction_or_restart() {
        let f = Fixture::new();
        let expired = uuid::Uuid::new_v4().to_string();
        f.coordinator
            .commit(
                &f.cfg,
                ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "old font".into(),
                    mono: String::new(),
                    font_size_sans: 15.0,
                    font_size_content: 14.0,
                    font_size_mono: 15.0,
                },
                &expired,
            )
            .unwrap();
        for i in 0..132 {
            f.commit(ConfigPatch::Fonts {
                content: "Content".into(),
                sans: format!("new font {i}"),
                mono: String::new(),
                font_size_sans: 15.0,
                font_size_content: 14.0,
                font_size_mono: 15.0,
            })
            .unwrap();
        }
        let retry = || ConfigPatch::Fonts {
            content: "Content".into(),
            sans: "old font".into(),
            mono: String::new(),
            font_size_sans: 15.0,
            font_size_content: 14.0,
            font_size_mono: 15.0,
        };
        assert_eq!(
            f.coordinator.operation(&f.cfg, &expired).unwrap_err().code,
            "OperationExpired"
        );
        assert_eq!(
            f.coordinator
                .commit(&f.cfg, retry(), &expired)
                .unwrap_err()
                .code,
            "OperationExpired"
        );
        assert_eq!(f.cfg.read().unwrap().ui.font_sans, "new font 131");
        let dir = tempfile::tempdir().unwrap();
        let disk = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
        disk.write_config(&f.cfg.read().unwrap()).unwrap();
        disk.write_journal(&f.files.journal.lock().unwrap())
            .unwrap();
        let restarted = ConfigCoordinator::new(f.credentials.clone(), disk);
        restarted.recover().unwrap();
        assert_eq!(
            restarted
                .commit(&f.cfg, retry(), &expired.to_uppercase())
                .unwrap_err()
                .code,
            "OperationExpired"
        );
        assert_eq!(f.cfg.read().unwrap().ui.font_sans, "new font 131");
    }

    #[test]
    fn tombstone_false_positive_is_a_safe_rejection_and_new_reviewed_id_can_commit() {
        let f = Fixture::new();
        let fresh = uuid::Uuid::new_v4().to_string();
        {
            let mut journal = f.files.journal.lock().unwrap();
            journal.expired_operations.resize(TOMBSTONE_WORDS, 0);
            for bit in operation_positions(&fresh) {
                journal.expired_operations[bit / 64] |= 1u64 << (bit % 64);
            }
        }
        let retry = || ConfigPatch::Fonts {
            content: "Content".into(),
            sans: "reviewed new font".into(),
            mono: String::new(),
            font_size_sans: 15.0,
            font_size_content: 14.0,
            font_size_mono: 15.0,
        };
        assert_eq!(
            f.coordinator
                .commit(&f.cfg, retry(), &fresh)
                .unwrap_err()
                .code,
            "OperationExpired"
        );
        assert_eq!(f.snapshot().version.sequence, 0);
        assert!(f.files.captured.lock().unwrap().is_empty());
        f.commit(retry()).unwrap();
        assert_eq!(f.cfg.read().unwrap().ui.font_sans, "reviewed new font");
    }

    #[test]
    fn saturated_tombstones_block_fresh_ids_and_never_clear_on_restart() {
        let f = Fixture::new();
        f.files.journal.lock().unwrap().expired_operations = vec![u64::MAX; TOMBSTONE_WORDS];
        for _ in 0..2 {
            assert_eq!(
                f.commit(ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "blocked".into(),
                    mono: String::new(),
                    font_size_sans: 15.0,
                    font_size_content: 14.0,
                    font_size_mono: 15.0,
                })
                .unwrap_err()
                .code,
                "OperationHistoryFull"
            );
        }
        let restarted = ConfigCoordinator::new(f.credentials.clone(), f.files.clone());
        assert_eq!(
            restarted.recover().unwrap_err().code,
            "OperationHistoryFull"
        );
        assert_eq!(
            restarted
                .commit(
                    &f.cfg,
                    ConfigPatch::AllowCommand {
                        command: "blocked".into()
                    },
                    &uuid::Uuid::new_v4().to_string()
                )
                .unwrap_err()
                .code,
            "OperationHistoryFull"
        );
        assert_eq!(
            f.files.journal.lock().unwrap().expired_operations,
            vec![u64::MAX; TOMBSTONE_WORDS]
        );
        assert!(f.cfg.read().unwrap().approval.command_allowlist.is_empty());
        assert!(f.files.captured.lock().unwrap().is_empty());
    }
    #[test]
    fn corrupt_journal_uuid_is_rejected_without_poisoning_coordinator() {
        let dir = tempfile::tempdir().unwrap();
        let disk = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
        let credentials = Arc::new(FakeCredentials::default());
        disk.write_config(&ConfigState::default()).unwrap();
        std::fs::write(dir.path().join("credential-commits.json"), br#"{"records":[{"operation_id":"broken-receipt","accounts":[],"committed":true,"post_processing":"complete"}]}"#).unwrap();
        let coordinator = ConfigCoordinator::new(credentials, disk.clone());
        let cfg = RwLock::new(ConfigState::default());
        assert_eq!(coordinator.recover().unwrap_err().code, "JournalUnreadable");
        for _ in 0..2 {
            assert_eq!(
                coordinator
                    .commit(
                        &cfg,
                        ConfigPatch::Fonts {
                            content: "Content".into(),
                            sans: "safe".into(),
                            mono: String::new(),
                            font_size_sans: 15.0,
                            font_size_content: 14.0,
                            font_size_mono: 15.0,
                        },
                        &uuid::Uuid::new_v4().to_string()
                    )
                    .unwrap_err()
                    .code,
                "JournalUnreadable"
            );
        }
        assert_eq!(coordinator.snapshot(&cfg).version.sequence, 0);
        disk.write_journal(&CommitJournal::default()).unwrap();
        coordinator
            .commit(
                &cfg,
                ConfigPatch::Fonts {
                    content: "Content".into(),
                    sans: "recovered".into(),
                    mono: String::new(),
                    font_size_sans: 15.0,
                    font_size_content: 14.0,
                    font_size_mono: 15.0,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        assert_eq!(cfg.read().unwrap().ui.font_sans, "recovered");
    }
}

/// #36-C0 夹具：配置提交必须挡住"另一个实例已经改过磁盘配置"的静默覆盖。
///
/// 回归背景：`CommitState` 里的 `page_version` / `role_versions` 都是**进程内**状态，
/// 跨进程完全看不见。两个实例各改一项设置时，两边的 CAS 都会通过，后写的把前写的
/// 静默抹掉——用户表现为"我刚改的设置又变回去了"。
#[cfg(test)]
mod coordination_c0 {
    // 复用既有夹具：真实 `DiskConfigStore` 才能测出"另一个进程改了磁盘文件"这件事。
    use super::tests::FakeCredentials;
    use super::*;

    fn creds() -> Arc<dyn CredentialStore> {
        Arc::new(FakeCredentials::default())
    }

    fn patch_font(sans: &str) -> ConfigPatch {
        ConfigPatch::Fonts {
            sans: sans.into(),
            content: "Content".into(),
            mono: "Mono".into(),
            font_size_sans: 16.0,
            font_size_content: 16.0,
            font_size_mono: 12.0,
        }
    }

    #[test]
    fn 未接入协调的宿主行为不变_首提交不会被自己的磁盘状态挡住() {
        // 没有 `with_coordination` 的调用方（桌面旧路径、既有测试）不该被新检查误伤。
        let dir = tempfile::tempdir().unwrap();
        let cfg = RwLock::new(ConfigState::default());
        let f = ConfigCoordinator::new(
            creds(),
            Arc::new(DiskConfigStore::new(dir.path().to_path_buf())),
        );
        let saved = f
            .commit(&cfg, patch_font("Sans"), &uuid::Uuid::new_v4().to_string())
            .expect("首提交");
        assert_eq!(saved.snapshot.config.ui.font_sans, "Sans");
        // 第二次提交同样不该被"自己写的磁盘内容"判成外部修改。
        f.commit(
            &cfg,
            patch_font("Sans 2"),
            &uuid::Uuid::new_v4().to_string(),
        )
        .expect("二次提交");
    }

    #[test]
    fn 别的实例改过磁盘配置_提交必须被拒而不是覆盖() {
        let dir = tempfile::tempdir().unwrap();
        let files = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
        let cfg = RwLock::new(ConfigState::default());
        let f = ConfigCoordinator::new(creds(), files.clone()).with_coordination(dir.path());

        // 第一次提交：建立"我见过什么"的记录。
        f.commit(&cfg, patch_font("Mine"), &uuid::Uuid::new_v4().to_string())
            .expect("首提交");

        // 另一个实例直接改磁盘（绕过本进程）。
        let mut theirs = files.read_config().unwrap().unwrap();
        theirs.ui.font_sans = "Theirs".into();
        files.write_config(&theirs).unwrap();

        let e = f
            .commit(
                &cfg,
                patch_font("Mine 2"),
                &uuid::Uuid::new_v4().to_string(),
            )
            .expect_err("磁盘已被他人改过，必须拒绝");
        assert_eq!(e.code, "ConfigChangedExternally");

        // 磁盘上必须还是别人的那份：拒绝意味着**没有**覆盖。
        let on_disk = files.read_config().unwrap().unwrap();
        assert_eq!(on_disk.ui.font_sans, "Theirs", "被拒的提交不得写盘");
    }

    #[test]
    fn 配置域与_history_域各占一把_不互相误判() {
        assert_eq!(ConfigCoordinator::coord_resource(), "config:root");
        assert_ne!(
            ConfigCoordinator::coord_resource(),
            "history:s1",
            "配置与历史不得共用一把租约"
        );
    }

    /// #36-C0 回归：提交**不得**在盘上留下本进程的配置租约。
    ///
    /// `check_no_external_change` 曾写成 `let _lease = store.acquire(..)`——租约拿到了却
    /// 连 `Result` 一起丢掉，于是每次提交都在 `<data>/coord/leases` 留下一个属于本进程的
    /// `config:root` 文件且永不释放。本进程察觉不到（同持有者重复申请走幂等分支），
    /// 只表现为「另一个实例从此再也配不动配置」，而且没有任何报错。
    #[test]
    fn 提交后不得留下配置租约_否则别的实例永远配不动() {
        use crate::core::process_coordination::LeaseStore;
        let dir = tempfile::tempdir().unwrap();
        let files = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
        let cfg = RwLock::new(ConfigState::default());
        let f = ConfigCoordinator::new(creds(), files).with_coordination(dir.path());

        let leases = LeaseStore::new(&dir.path().join("coord"));
        for i in 0..3 {
            f.commit(
                &cfg,
                patch_font(&format!("Sans {i}")),
                &uuid::Uuid::new_v4().to_string(),
            )
            .expect("提交");
            assert!(
                leases.read(ConfigCoordinator::coord_resource()).is_none(),
                "第 {i} 次提交后仍占着 config:root：别的实例将永远拿不到这把锁"
            );
        }
    }

    /// #36-C0：凭据轮换期间配置被**别的实例改过**时必须被拒，且不得已经动过钥匙串。
    ///
    /// 轮换不是普通配置写：它要同时改系统钥匙串与配置文件。真正的保护不是租约
    /// （配置写刻意**不**因租约被占而阻断，见 `check_no_external_change`），
    /// 而是提交前的磁盘事实比对——那一刻若发现磁盘内容不是「我上次见到的」就必须停手，
    /// 否则会拿一份过期的密钥清单去覆盖别人刚改的配置。
    #[test]
    fn 轮换期间配置被别的实例改过_必须被拒且不动钥匙串() {
        use crate::core::config::{KEYRING_PLACEHOLDER, ProviderConfig};
        let dir = tempfile::tempdir().unwrap();
        let files = Arc::new(DiskConfigStore::new(dir.path().to_path_buf()));
        let creds = Arc::new(FakeCredentials::default());
        let cfg = RwLock::new(ConfigState::default());
        let f = ConfigCoordinator::new(creds.clone(), files.clone()).with_coordination(dir.path());

        // 首提交：建立「我见过什么」的基准。
        f.commit(&cfg, patch_font("Mine"), &uuid::Uuid::new_v4().to_string())
            .expect("首提交");
        cfg.write().unwrap().providers.push(ProviderConfig {
            id: "p1".into(),
            keys: vec![KEYRING_PLACEHOLDER.into(), "plain-rotate-me".into()],
            ..Default::default()
        });

        // 另一个实例绕过本进程直接改了磁盘。
        let mut theirs = files.read_config().unwrap().unwrap();
        theirs.ui.font_mono = "TheirMono".into();
        files.write_config(&theirs).unwrap();

        let e = f
            .commit(
                &cfg,
                ConfigPatch::SecureExisting,
                &uuid::Uuid::new_v4().to_string(),
            )
            .expect_err("配置已被他人改过时，轮换必须停手");
        assert_eq!(e.code, "ConfigChangedExternally", "{}", e.code);
        assert_eq!(creds.write_count(), 0, "被拒的轮换不得已经动过钥匙串");

        // 重新加载到最新磁盘事实后，轮换照常进行——检查是拦冲突，不是永久封禁。
        creds.seed("p1", &["already-in-keyring"]);
        f.note_disk_digest(&theirs);
        f.commit(
            &cfg,
            ConfigPatch::SecureExisting,
            &uuid::Uuid::new_v4().to_string(),
        )
        .expect("对齐磁盘后轮换应可进行");
        assert!(creds.write_count() > 0, "轮换应真的写了钥匙串");
    }
}
