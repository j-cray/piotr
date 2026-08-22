use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::env;
use std::sync::OnceLock;

type HmacSha256 = Hmac<Sha256>;

static ANONYMIZE_KEY: OnceLock<String> = OnceLock::new();

pub fn init_anonymize_key(key: &str) {
    if !key.is_empty() {
        let _ = ANONYMIZE_KEY.set(key.to_string());
    }
}

/// Anonymizes a unique identifier (like a phone number or group ID) by hashing it.
/// Uses HMAC-SHA256 with a secret key from configuration or environment variable
/// to avoid reversible, unsalted hashes for low-entropy identifiers (e.g., phone numbers).
/// Returns a truncated hex-encoded prefix of the MAC for readability.
pub fn anonymize(s: &str) -> String {
    let key = ANONYMIZE_KEY.get_or_init(|| {
        env::var("ANONYMIZE_KEY")
            .or_else(|_| env::var("PIOTR_ANONYMIZE_KEY"))
            .expect("ANONYMIZE_KEY or PIOTR_ANONYMIZE_KEY must be set for anonymization")
    });
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC-SHA256 can take key of any size");
    mac.update(s.as_bytes());
    let result = mac.finalize().into_bytes();
    // Use the first 16 bytes (32 hex chars) for readability while remaining collision-resistant.
    let truncated = &result[..16];
    hex::encode(truncated)
}
