//! SHA-256 content hashing for manifests, snapshot equality, and the CAS.
//!
//! Snapshot equality is based on `(path, content_hash)` pairs. Size and mtime
//! are optimization metadata only: a file can change bytes while keeping the
//! same size and second-level mtime, so they must never decide equality.

use sha2::Digest as _;

/// Hex-encoded SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Hex-encoded SHA-256 of a string's bytes.
#[must_use]
pub fn sha256_hex_str(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

/// Whether `hash` is shaped like a SHA-256 hex digest (64 lowercase hex).
/// Used to harden manifest/CAS parsing against corrupt or hostile input.
#[must_use]
pub fn is_plausible_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn sha256_matches_nist_vector() {
        // NIST FIPS 180-4: SHA-256("abc").
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b"").len(), 64);
    }

    #[test]
    #[serial_test::serial]
    fn plausible_hash_validation() {
        assert!(is_plausible_hash(&sha256_hex(b"x")));
        assert!(!is_plausible_hash(""));
        assert!(!is_plausible_hash(&"0".repeat(63)));
        assert!(!is_plausible_hash(&"g".repeat(64)));
        assert!(!is_plausible_hash(&"0".repeat(65)));
    }
}
