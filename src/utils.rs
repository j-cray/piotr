use sha2::{Digest, Sha256};

/// Anonymizes a unique identifier (like a phone number or group ID) by hashing it.
/// Returns a short prefix of the hash for readability (e.g., first 8 chars) or full hash?
/// User asked for "hashed uuid", implies full or substantial part.
/// Let's return the full hex hash to be safe and consistent with profile IDs.
pub fn anonymize(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}
