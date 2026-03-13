use sha2::{Digest, Sha256};

/// Anonymizes a unique identifier (such as a phone number or group ID) by hashing it.
/// The input is hashed using SHA-256 and the full hash is returned
/// as a lowercase hexadecimal string.
pub fn anonymize(s: &str) -> String {
    let mut hasher = Sha256::new();
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}
