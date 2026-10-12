//! Same-directory staged writes. Replacement never touches a user-owned `.bak`.

use std::fs;
use std::io::Write;
use std::path::Path;

pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent dir"))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;

    // tempfile uses rename on Unix and MoveFileExW(MOVEFILE_REPLACE_EXISTING)
    // on Windows. Never copy/delete the destination first, and never touch a
    // user-owned sibling .bak. A failed replacement is reported directly; an
    // unverified rollback copy must never be written over the destination.
    tmp.persist(path).map(|_| ()).map_err(|e| e.error)
}

/// Publish a complete file only if no destination exists, including when another
/// process creates it after validation. No check-then-overwrite fallback.
pub fn atomic_create_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent dir"))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist_noclobber(path).map(|_| ()).map_err(|e| e.error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/c.json");
        atomic_write(&p, b"v1").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"v1");
        atomic_write(&p, b"v2").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"v2");
        // No leftover tmp files expected
        let leftovers: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[test]
    fn empty_content_creates_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.txt");
        atomic_write(&p, b"").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"");
        assert_eq!(fs::metadata(&p).unwrap().len(), 0);
    }

    #[test]
    fn large_binary_content_round_trips_intact() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        // 1 MiB pseudo-random-ish binary (NUL bytes included) survives the temp+rename path
        let content: Vec<u8> = (0u32..(1024 * 1024))
            .map(|i| ((i.wrapping_mul(2654435761u32)) >> 13) as u8)
            .collect();
        atomic_write(&p, &content).unwrap();
        assert_eq!(fs::read(&p).unwrap(), content);
        // Overwrite with a different length; no .bak or temp leftovers remain
        atomic_write(&p, b"tiny").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"tiny");
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "only the target file remains");
    }
    #[test]
    fn document_atomic_preserves_user_bak_on_success_and_failure() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.docx");
        let bak = p.with_extension("bak");
        fs::write(&bak, b"user backup").unwrap();
        atomic_write(&p, b"first").unwrap();
        atomic_write(&p, b"second").unwrap();
        assert_eq!(fs::read(&bak).unwrap(), b"user backup");
        // A directory is a deterministic persist failure on every platform.
        let blocked = dir.path().join("blocked.docx");
        fs::create_dir(&blocked).unwrap();
        let blocked_bak = blocked.with_extension("bak");
        fs::write(&blocked_bak, b"other user backup").unwrap();
        assert!(atomic_write(&blocked, b"not written").is_err());
        assert!(blocked.is_dir());
        assert_eq!(fs::read(&blocked_bak).unwrap(), b"other user backup");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
    }

    #[test]
    fn document_atomic_create_new_is_no_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.docx");
        atomic_create_new(&p, b"first").unwrap();
        assert_eq!(
            atomic_create_new(&p, b"second").unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&p).unwrap(), b"first");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn document_same_stem_writes_never_share_rollback_files() {
        let dir = tempfile::tempdir().unwrap();
        let bak = dir.path().join("a.bak");
        fs::write(&bak, b"owned by user").unwrap();
        std::thread::scope(|s| {
            for ext in ["docx", "xlsx", "xlsm"] {
                let p = dir.path().join(format!("a.{ext}"));
                s.spawn(move || {
                    for _ in 0..10 {
                        atomic_write(&p, ext.as_bytes()).unwrap();
                    }
                });
            }
        });
        assert_eq!(fs::read(bak).unwrap(), b"owned by user");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
    }

    #[cfg(windows)]
    #[test]
    fn document_windows_atomic_preserves_user_bak_when_target_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.docx");
        let bak = p.with_extension("bak");
        fs::write(&p, b"original").unwrap();
        fs::write(&bak, b"user backup").unwrap();
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&p)
            .unwrap();
        assert!(atomic_write(&p, b"replacement").is_err());
        drop(handle);
        assert_eq!(fs::read(&p).unwrap(), b"original");
        assert_eq!(fs::read(&bak).unwrap(), b"user backup");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
