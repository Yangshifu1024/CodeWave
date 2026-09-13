//! Optimistic concurrency tokens for the edit tool: first 6 chars of a SHA-256 digest in Crockford Base32
//! (ambiguous characters I L O U excluded, consistent with [docs/p0-plan](../../../docs/p0-plan.md) §6.3).

use sha2::{Digest, Sha256};

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub fn version_token(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut n = u64::from_be_bytes(digest[..8].try_into().unwrap());
    let mut out = [0u8; 6];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[(n % 32) as usize];
        n /= 32;
    }
    String::from_utf8(out.to_vec()).unwrap()
}

/// Normalize user/model-supplied tokens: lowercase mapped back to Crockford uppercase (l→1, o→0, i→1, u→v).
pub fn normalize_token(s: &str) -> String {
    s.trim()
        .to_ascii_uppercase()
        .chars()
        .map(|c| match c {
            'L' | 'I' => '1',
            'O' => '0',
            'U' => 'V',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_alphabet() {
        let a = version_token(b"hello");
        let b = version_token(b"hello");
        let c = version_token(b"hello!");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 6);
        assert!(a.bytes().all(|b| CROCKFORD.contains(&b)));
    }

    #[test]
    fn normalize() {
        // O→0, I→1, L→1, U→V
        assert_eq!(normalize_token("oilst"), "011ST");
    }
}
