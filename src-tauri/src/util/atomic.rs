//! Atomic write: temp file + rename; Windows keeps a .bak backup first ([docs/p0-plan](../../../docs/p0-plan.md) §3.3).

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
    tmp.as_file().sync_all()?;

    // The backup enables rollback only when rename fails; cleaned up right after success, never leaving a <name>.bak behind
    #[cfg(windows)]
    let bak = if path.exists() {
        let bak = path.with_extension("bak");
        let _ = fs::copy(path, &bak);
        Some(bak)
    } else {
        None
    };

    let result = tmp.persist(path);
    #[cfg(windows)]
    if let Some(bak) = bak {
        if result.is_ok() {
            let _ = fs::remove_file(&bak);
        } else {
            let _ = fs::copy(&bak, path);
            let _ = fs::remove_file(&bak);
        }
    }
    result.map(|_| ()).map_err(|e| e.error)
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
}
