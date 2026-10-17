//! Append-only occurrence batches with a bounded recovery intent and direct batch index.
use super::{SessionStore, ToolOccurrenceRecord};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};
const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MATCH_LOOKAHEAD: usize = 128;
fn read_bounded(path: impl AsRef<std::path::Path>) -> std::io::Result<Vec<u8>> {
    let mut bytes = vec![];
    File::open(path)?
        .take((MAX_BATCH_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "occurrence batch exceeds limit",
        ));
    }
    Ok(bytes)
}
#[derive(Clone, Serialize, Deserialize)]
struct Batch {
    actor_run: String,
    batch: String,
    entries: Vec<ToolOccurrenceRecord>,
}
#[derive(Serialize, Deserialize)]
struct Pending {
    offset: u64,
    batch: Batch,
}
pub struct ToolOccurrenceCursor {
    reader: BufReader<File>,
    queued: VecDeque<ToolOccurrenceRecord>,
    lookahead: VecDeque<ToolOccurrenceRecord>,
    exhausted: bool,
}
impl ToolOccurrenceCursor {
    fn next_record(&mut self) -> anyhow::Result<Option<ToolOccurrenceRecord>> {
        if let Some(record) = self.queued.pop_front() {
            return Ok(Some(record));
        }
        let mut line = String::new();
        let n = (&mut self.reader)
            .take((MAX_BATCH_BYTES + 1) as u64)
            .read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        anyhow::ensure!(
            n <= MAX_BATCH_BYTES && line.ends_with('\n'),
            "invalid occurrence batch tail"
        );
        let batch: Batch = serde_json::from_str(&line)?;
        self.queued = batch.entries.into();
        anyhow::ensure!(!self.queued.is_empty(), "empty occurrence batch");
        Ok(self.queued.pop_front())
    }
    /// Hold a fixed lookahead window. Unknown legacy calls do not consume any
    /// future identity and do not scan or reread the rest of the metadata log.
    /// Damaged history with a larger unmatched gap uses replay fallback IDs.
    pub fn matching(
        &mut self,
        provider: &str,
        name: &str,
    ) -> anyhow::Result<Option<ToolOccurrenceRecord>> {
        while self.lookahead.len() < MATCH_LOOKAHEAD && !self.exhausted {
            match self.next_record()? {
                Some(record) => self.lookahead.push_back(record),
                None => self.exhausted = true,
            }
        }
        let Some(index) = self
            .lookahead
            .iter()
            .position(|record| record.provider_id == provider && record.name == name)
        else {
            return Ok(None);
        };
        for _ in 0..index {
            self.lookahead.pop_front();
        }
        Ok(self.lookahead.pop_front())
    }
}
impl SessionStore {
    fn occurrence_dir(&self, session: &str) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(super::is_safe_session_id(session), "unsafe session id");
        if let Some((parent, actor)) = session.rsplit_once("__")
            && actor.starts_with("sub_")
            && super::is_safe_session_id(parent)
            && super::is_safe_session_id(actor)
        {
            return Ok(self
                .root
                .join("sessions")
                .join(format!("{parent}.toolres"))
                .join("_observations/actors")
                .join(actor));
        }
        Ok(self
            .root
            .join("sessions")
            .join(format!("{session}.toolres"))
            .join("_observations"))
    }
    pub fn tool_occurrence_cursor(
        &self,
        session: &str,
    ) -> anyhow::Result<Option<ToolOccurrenceCursor>> {
        let path = self.occurrence_dir(session)?.join("ledger.jsonl");
        match File::open(path) {
            Ok(file) => Ok(Some(ToolOccurrenceCursor {
                reader: BufReader::new(file),
                queued: VecDeque::new(),
                lookahead: VecDeque::new(),
                exhausted: false,
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    /// Explicit diagnostic/test API. Production replay uses the bounded cursor.
    pub fn tool_occurrences(&self, session: &str) -> anyhow::Result<Vec<ToolOccurrenceRecord>> {
        let Some(mut cursor) = self.tool_occurrence_cursor(session)? else {
            return Ok(vec![]);
        };
        let mut records = vec![];
        while let Some(record) = cursor.next_record()? {
            records.push(record);
        }
        Ok(records)
    }
    fn commit_occurrence_pending(&self, session: &str, pending: &Pending) -> anyhow::Result<()> {
        let dir = self.occurrence_dir(session)?;
        let path = dir.join("ledger.jsonl");
        let mut line = serde_json::to_vec(&pending.batch)?;
        line.push(b'\n');
        anyhow::ensure!(
            line.len() <= MAX_BATCH_BYTES,
            "occurrence batch exceeds limit"
        );
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        anyhow::ensure!(
            file.metadata()?.len() >= pending.offset,
            "occurrence log prefix missing"
        );
        file.seek(SeekFrom::Start(pending.offset))?;
        let mut tail = vec![];
        (&mut file)
            .take((line.len() + 1) as u64)
            .read_to_end(&mut tail)?;
        anyhow::ensure!(
            tail.len() <= line.len() && line.starts_with(&tail),
            "occurrence pending conflicts with committed log"
        );
        if tail != line {
            // Only a never-committed, incomplete tail named by our durable intent.
            file.set_len(pending.offset)?;
            file.seek(SeekFrom::Start(pending.offset))?;
            file.write_all(&line)?;
        }
        file.sync_all()?;
        let index = dir.join("batches").join(format!(
            "{}.json",
            crate::core::process_coordination::key_digest(&pending.batch.batch)
        ));
        super::atomic_write(&index, &serde_json::to_vec(&pending.batch)?)?;
        std::fs::remove_file(dir.join("pending.json"))?;
        Ok(())
    }
    pub fn register_tool_occurrences(
        &self,
        session: &str,
        actor_run: &str,
        batch: &str,
        calls: &[(String, String)],
    ) -> anyhow::Result<Vec<String>> {
        if calls.is_empty() {
            return Ok(vec![]);
        }
        let dir = self.occurrence_dir(session)?;
        let _lock = crate::core::process_coordination::DiskLock::acquire(&dir.join("ledger.lock"))?;
        let pending_path = dir.join("pending.json");
        match read_bounded(&pending_path) {
            Ok(bytes) => {
                anyhow::ensure!(
                    bytes.len() <= MAX_BATCH_BYTES,
                    "occurrence pending exceeds limit"
                );
                self.commit_occurrence_pending(
                    session,
                    &serde_json::from_slice::<Pending>(&bytes)?,
                )?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let index = dir.join("batches").join(format!(
            "{}.json",
            crate::core::process_coordination::key_digest(batch)
        ));
        let record = match read_bounded(&index) {
            Ok(bytes) => {
                anyhow::ensure!(
                    bytes.len() <= MAX_BATCH_BYTES,
                    "occurrence index exceeds limit"
                );
                serde_json::from_slice::<Batch>(&bytes)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let entries = calls
                    .iter()
                    .enumerate()
                    .map(|(index, (provider_id, name))| ToolOccurrenceRecord {
                        observation_id: format!("occ:{}", uuid::Uuid::new_v4()),
                        actor_run: actor_run.into(),
                        batch: batch.into(),
                        index,
                        provider_id: provider_id.clone(),
                        name: name.clone(),
                    })
                    .collect();
                let record = Batch {
                    actor_run: actor_run.into(),
                    batch: batch.into(),
                    entries,
                };
                let offset = match std::fs::metadata(dir.join("ledger.jsonl")) {
                    Ok(meta) => meta.len(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(e) => return Err(e.into()),
                };
                let pending = Pending {
                    offset,
                    batch: record.clone(),
                };
                let bytes = serde_json::to_vec(&pending)?;
                anyhow::ensure!(
                    bytes.len() <= MAX_BATCH_BYTES,
                    "occurrence batch exceeds limit"
                );
                super::atomic_write(&pending_path, &bytes)?;
                self.commit_occurrence_pending(session, &pending)?;
                record
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            record.actor_run == actor_run
                && record.batch == batch
                && record.entries.len() == calls.len(),
            "occurrence identity conflict"
        );
        for (index, (entry, (provider, name))) in record.entries.iter().zip(calls).enumerate() {
            anyhow::ensure!(
                entry.index == index
                    && entry.actor_run == actor_run
                    && entry.batch == batch
                    && entry.provider_id == *provider
                    && entry.name == *name,
                "occurrence identity conflict"
            );
        }
        Ok(record
            .entries
            .into_iter()
            .map(|entry| entry.observation_id)
            .collect())
    }
    pub fn tool_observation_id(&self, session: &str, batch: &str, index: usize) -> Option<String> {
        let path = self
            .occurrence_dir(session)
            .ok()?
            .join("batches")
            .join(format!(
                "{}.json",
                crate::core::process_coordination::key_digest(batch)
            ));
        let bytes = read_bounded(path).ok()?;
        if bytes.len() > MAX_BATCH_BYTES {
            return None;
        }
        let record: Batch = serde_json::from_slice(&bytes).ok()?;
        record
            .entries
            .into_iter()
            .find(|entry| entry.index == index && entry.batch == batch)
            .map(|entry| entry.observation_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().into());
        (dir, store)
    }
    #[test]
    fn append_preserves_prefix_and_cursor_rewinds_unknown_legacy_calls() {
        let (_dir, store) = fixture();
        let calls = vec![("reused".into(), "read".into())];
        let first = store
            .register_tool_occurrences("session", "run", "first", &calls)
            .unwrap();
        let path = store
            .occurrence_dir("session")
            .unwrap()
            .join("ledger.jsonl");
        let prefix = std::fs::read(&path).unwrap();
        let second = store
            .register_tool_occurrences("session", "run", "second", &calls)
            .unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
        assert_ne!(first, second);
        let mut cursor = store.tool_occurrence_cursor("session").unwrap().unwrap();
        assert!(
            cursor
                .matching("legacy-unregistered", "read")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            cursor
                .matching("reused", "read")
                .unwrap()
                .unwrap()
                .observation_id,
            first[0]
        );
        assert_eq!(
            cursor
                .matching("reused", "read")
                .unwrap()
                .unwrap()
                .observation_id,
            second[0]
        );
        assert!(cursor.queued.is_empty());
        assert_eq!(
            store
                .register_tool_occurrences("session", "run", "second", &calls)
                .unwrap(),
            second
        );
        assert_eq!(store.tool_occurrences("session").unwrap().len(), 2);
    }
    #[test]
    fn interrupted_append_recovers_only_its_uncommitted_tail_and_keeps_issued_ids() {
        let (_dir, store) = fixture();
        let calls = vec![("provider".into(), "read".into())];
        store
            .register_tool_occurrences("session", "run", "first", &calls)
            .unwrap();
        let dir = store.occurrence_dir("session").unwrap();
        let path = dir.join("ledger.jsonl");
        let prefix = std::fs::read(&path).unwrap();
        let pending = Pending {
            offset: prefix.len() as u64,
            batch: Batch {
                actor_run: "run".into(),
                batch: "interrupted".into(),
                entries: vec![ToolOccurrenceRecord {
                    observation_id: "occ:issued-before-crash".into(),
                    actor_run: "run".into(),
                    batch: "interrupted".into(),
                    index: 0,
                    provider_id: "provider".into(),
                    name: "read".into(),
                }],
            },
        };
        super::super::atomic_write(
            &dir.join("pending.json"),
            &serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        let line = serde_json::to_vec(&pending.batch).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&line[..line.len() / 2])
            .unwrap();
        let ids = store
            .register_tool_occurrences("session", "run", "interrupted", &calls)
            .unwrap();
        assert_eq!(ids, vec!["occ:issued-before-crash"]);
        assert!(std::fs::read(&path).unwrap().starts_with(&prefix));
        assert_eq!(store.tool_occurrences("session").unwrap().len(), 2);
        assert!(!dir.join("pending.json").exists());
    }
    #[test]
    fn child_ledgers_are_parent_scoped_and_deleted_with_the_parent() {
        let (_dir, store) = fixture();
        let calls = vec![("same-provider".into(), "read".into())];
        let a = store
            .register_tool_occurrences("parent-a__sub_same", "actor-a", "batch", &calls)
            .unwrap();
        let b = store
            .register_tool_occurrences("parent-b__sub_same", "actor-b", "batch", &calls)
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(
            store.tool_observation_id("parent-a__sub_same", "batch", 0),
            Some(a[0].clone())
        );
        store.remove("parent-a").unwrap();
        assert!(
            store
                .tool_observation_id("parent-a__sub_same", "batch", 0)
                .is_none()
        );
        assert_eq!(
            store.tool_observation_id("parent-b__sub_same", "batch", 0),
            Some(b[0].clone())
        );
    }

    #[test]
    fn many_unknown_legacy_calls_do_not_rescan_the_log_or_consume_future_ids() {
        let (_dir, store) = fixture();
        let calls = (0..500)
            .map(|i| (format!("provider-{i}"), "read".into()))
            .collect::<Vec<_>>();
        let ids = store
            .register_tool_occurrences("session", "run", "batch", &calls)
            .unwrap();
        let mut cursor = store.tool_occurrence_cursor("session").unwrap().unwrap();
        assert!(cursor.matching("legacy-first", "read").unwrap().is_none());
        let position = cursor.reader.stream_position().unwrap();
        for i in 0..10000 {
            assert!(
                cursor
                    .matching(&format!("unknown-legacy-{i}"), "read")
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(cursor.reader.stream_position().unwrap(), position);
        assert_eq!(cursor.lookahead.len(), MATCH_LOOKAHEAD);
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(
                cursor
                    .matching(&format!("provider-{i}"), "read")
                    .unwrap()
                    .unwrap()
                    .observation_id,
                *id
            );
        }
    }
}
