use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::env;

type HmacSha256 = Hmac<Sha256>;

/// Anonymizes a unique identifier (like a phone number or group ID) by hashing it.
/// Uses HMAC-SHA256 with a secret key from the ANONYMIZE_KEY environment variable
/// to avoid reversible, unsalted hashes for low-entropy identifiers (e.g., phone numbers).
/// Returns a truncated hex-encoded prefix of the MAC for readability.
pub fn anonymize(s: &str) -> String {
    let key = env::var("ANONYMIZE_KEY").expect("ANONYMIZE_KEY must be set for anonymization");
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC-SHA256 can take key of any size");
    mac.update(s.as_bytes());
    let result = mac.finalize().into_bytes();
    // Use the first 16 bytes (32 hex chars) for readability while remaining collision-resistant.
    let truncated = &result[..16];
    hex::encode(truncated)
}
