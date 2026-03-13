use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::fs;
use anyhow::{Result, Context};
use crate::ai::ReactionAnalysis;
use sqlx::PgPool;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce
};
use rand::Rng;


#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interaction {
    pub prompt: String,
    pub response: String,
    pub analysis: ReactionAnalysis,
    pub timestamp: u64,
}

#[derive(Clone)]
pub struct Memory {
    file_path: String,
    interactions: Arc<Mutex<Vec<Interaction>>>,
}

impl Memory {
    pub fn new(file_path: &str) -> Self {
        let interactions = if let Ok(content) = fs::read_to_string(file_path) {
            serde_json::from_str(&content).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };

        Self {
            file_path: file_path.to_string(),
            interactions: Arc::new(Mutex::new(interactions)),
        }
    }

    pub async fn add_interaction(&self, prompt: String, response: String, analysis: ReactionAnalysis) -> Result<()> {
        let mut guard = self.interactions.lock().await;

        if let Some(existing) = guard.iter_mut().find(|i| i.prompt == prompt && i.response == response) {
            existing.analysis = analysis;
            existing.timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        } else {
            guard.push(Interaction {
                prompt,
                response,
                analysis,
                timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64,
            });
        }

        self.save(&guard).await?;
        Ok(())
    }

    pub async fn get_relevant_examples(&self, _query: &str, limit: usize) -> Vec<Interaction> {
        let guard = self.interactions.lock().await;
        // For now, just return the highest rated recent ones
        let mut sorted: Vec<Interaction> = guard.clone();
        sorted.sort_by(|a, b| b.analysis.sentiment_score.partial_cmp(&a.analysis.sentiment_score).unwrap_or(std::cmp::Ordering::Equal));
        sorted.into_iter().take(limit).collect()
    }

    fn save(&self, interactions: &[Interaction]) -> Result<()> {
        let json = serde_json::to_string_pretty(interactions)?;
        fs::write(&self.file_path, json)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String, // Hashed identifier
    pub name: Option<String>,
    pub nickname: Option<String>,
    pub personality_summary: String,
    pub interaction_style: String, // e.g. "casual", "technical"
    pub topics_of_interest: Vec<String>,
    pub last_updated: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupProfile {
    pub id: String, // Hashed group ID
    pub group_name: Option<String>,
    pub group_vibe: String, // e.g. "chaotic", "serious", "meme-heavy"
    pub inside_jokes: Vec<String>,
    pub common_topics: Vec<String>,
    pub important_memories: Vec<String>,
    pub last_updated: u64,
}

#[derive(Clone)]
pub struct DbProfileManager {
    pool: PgPool,
    encryption_key: [u8; 32],
}

impl DbProfileManager {
    pub fn new(pool: PgPool, key_hex: &str) -> Result<Self> {
        let key_bytes = hex::decode(key_hex).context("Failed to decode PROFILE_ENCRYPTION_KEY hex")?;
        if key_bytes.len() != 32 {
            anyhow::bail!("PROFILE_ENCRYPTION_KEY must be 32 bytes (64 hex chars)");
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);

        Ok(Self {
            pool,
            encryption_key: key,
        })
    }

    pub fn get_profile_id(raw_id: &str) -> String {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(raw_id.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let cipher = XChaCha20Poly1305::new(&self.encryption_key.into());
        let mut nonce_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        let ciphertext = cipher.encrypt(nonce, data)
            .map_err(|e| anyhow::anyhow!("Encryption failure: {:?}", e))?;

        // Prepend nonce to ciphertext
        let mut result = nonce_bytes.to_vec();
        result.extend(ciphertext);
        Ok(result)
    }

    fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < 24 {
            anyhow::bail!("Encrypted blob too short");
        }
        let nonce_bytes = &blob[..24];
        let ciphertext = &blob[24..];
        let nonce = XNonce::from_slice(nonce_bytes);

        let cipher = XChaCha20Poly1305::new(&self.encryption_key.into());
        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|e| anyhow::anyhow!("Decryption failure: {:?}", e))?;

        Ok(plaintext)
    }

    pub async fn get_profile(&self, raw_id: &str, current_name: Option<String>) -> Result<UserProfile> {
        let id = Self::get_profile_id(raw_id);

        // Fetch from DB
        let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT encrypted_blob FROM user_profiles WHERE user_id = $1")
            .bind(&id)
            .fetch_optional(&self.pool)
            .await?;

        let mut profile = if let Some((blob,)) = row {
            // Decrypt
            let plaintext = self.decrypt(&blob)?;
            serde_json::from_slice(&plaintext)?
        } else {
            // New Profile
            UserProfile {
                id: id.clone(),
                name: current_name.clone(),
                nickname: None,
                personality_summary: "New user".to_string(),
                interaction_style: "neutral".to_string(),
                topics_of_interest: Vec::new(),
                last_updated: 0,
            }
        };

        // Auto-update name logic
        if let Some(new_name) = current_name {
            if profile.name.as_ref() != Some(&new_name) {
                profile.name = Some(new_name);
                self.save_profile(&profile).await?;
            }
        }

        Ok(profile)
    }

    pub async fn save_profile(&self, profile: &UserProfile) -> Result<()> {
        let json = serde_json::to_vec(profile)?;
        let blob = self.encrypt(&json)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;

        sqlx::query(
            "INSERT INTO user_profiles (user_id, encrypted_blob, last_updated) VALUES ($1, $2, $3)
             ON CONFLICT(user_id) DO UPDATE SET encrypted_blob = $2, last_updated = $3"
        )
        .bind(&profile.id)
        .bind(blob)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_group_profile(&self, raw_id: &str, current_name: Option<String>) -> Result<GroupProfile> {
        let id = Self::get_profile_id(raw_id);

        let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT encrypted_blob FROM group_profiles WHERE group_id = $1")
            .bind(&id)
            .fetch_optional(&self.pool)
            .await?;

        let mut profile = if let Some((blob,)) = row {
            let plaintext = self.decrypt(&blob)?;
            serde_json::from_slice(&plaintext)?
        } else {
            GroupProfile {
                id: id.clone(),
                group_name: current_name.clone(),
                group_vibe: "Neutral".to_string(),
                inside_jokes: Vec::new(),
                common_topics: Vec::new(),
                important_memories: Vec::new(),
                last_updated: 0,
            }
        };

        if let Some(new_name) = current_name {
            if profile.group_name.as_ref() != Some(&new_name) {
                profile.group_name = Some(new_name);
                self.save_group_profile(&profile).await?;
            }
        }

        Ok(profile)
    }

    pub async fn save_group_profile(&self, profile: &GroupProfile) -> Result<()> {
        let json = serde_json::to_vec(profile)?;
        let blob = self.encrypt(&json)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;

        sqlx::query(
            "INSERT INTO group_profiles (group_id, encrypted_blob, last_updated) VALUES ($1, $2, $3)
             ON CONFLICT(group_id) DO UPDATE SET encrypted_blob = $2, last_updated = $3"
        )
        .bind(&profile.id)
        .bind(blob)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn migrate_json_profiles(&self, data_dir: &str) -> Result<()> {
        let paths = fs::read_dir(data_dir);
        if let Ok(entries) = paths {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        let content = fs::read_to_string(&path)?;
                        match serde_json::from_str::<UserProfile>(&content) {
                            Ok(profile) => {
                                tracing::info!("Migrating profile for {}", profile.id);
                                if let Err(e) = self.save_profile(&profile).await {
                                    tracing::error!("Failed to migrate profile {}: {:?}", profile.id, e);
                                } else {
                                    // Rename to .imported
                                    let new_path = path.with_extension("json.imported");
                                    let _ = fs::rename(path, new_path);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Failed to parse profile {:?}: {:?}", path, e);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_id_generation() {
        let raw_id = "+1234567890";
        let hashed_id = DbProfileManager::get_profile_id(raw_id);

        // Output should be a 64 character hex string (sha256)
        assert_eq!(hashed_id.len(), 64);

        // Same input should produce same output
        let hashed_id_2 = DbProfileManager::get_profile_id(raw_id);
        assert_eq!(hashed_id, hashed_id_2);

        // Different input should produce different output
        let different = DbProfileManager::get_profile_id("+0987654321");
        assert_ne!(hashed_id, different);
    }

    #[test]
    fn test_memory_interaction_sorting() {
        let i1 = Interaction {
            prompt: "1".to_string(),
            response: "1".to_string(),
            analysis: ReactionAnalysis {
                sentiment_score: 0.1,
                reasoning: "".to_string(),
                tags: vec![],
            },
            timestamp: 1,
        };
        let i2 = Interaction {
            prompt: "2".to_string(),
            response: "2".to_string(),
            analysis: ReactionAnalysis {
                sentiment_score: 0.9,
                reasoning: "".to_string(),
                tags: vec![],
            },
            timestamp: 2,
        };
        let i3 = Interaction {
            prompt: "3".to_string(),
            response: "3".to_string(),
            analysis: ReactionAnalysis {
                sentiment_score: -0.5,
                reasoning: "".to_string(),
                tags: vec![],
            },
            timestamp: 3,
        };

        let interactions = vec![i1, i2, i3];
        let mut sorted = interactions.clone();

        sorted.sort_by(|a, b| b.analysis.sentiment_score.partial_cmp(&a.analysis.sentiment_score).unwrap_or(std::cmp::Ordering::Equal));

        assert_eq!(sorted[0].prompt, "2"); // 0.9
        assert_eq!(sorted[1].prompt, "1"); // 0.1
        assert_eq!(sorted[2].prompt, "3"); // -0.5
    }

    // --- SECURITY TESTS ---

    fn get_test_manager() -> DbProfileManager {
        // Mock pool isn't needed for encryption isolated testing, but struct requires it.
        // We can test encrypt/decrypt methods directly if we instantiate with dummy key.
        DbProfileManager {
            pool: sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://dummy").unwrap(),
            encryption_key: [1u8; 32],
        }
    }

    #[tokio::test]
    async fn test_encryption_entropy() {
        let manager = get_test_manager();
        let data = b"Sensitive User Data";

        let run1 = manager.encrypt(data).unwrap();
        let run2 = manager.encrypt(data).unwrap();

        // Security: Nonce should ensure identical plaintext produces different ciphertext
        assert_ne!(run1, run2, "Encryption lacks entropy; same plaintext produced same ciphertext");

        // Decryption should still succeed for both
        assert_eq!(manager.decrypt(&run1).unwrap(), data);
        assert_eq!(manager.decrypt(&run2).unwrap(), data);
    }

    #[tokio::test]
    async fn test_decryption_too_short() {
        let manager = get_test_manager();
        let short_blob = vec![1u8; 23];
        let result = manager.decrypt(&short_blob);
        assert!(result.is_err(), "Decryption should fail on blobs smaller than nonce size");
    }

    #[tokio::test]
    async fn test_decryption_tampering() {
        let manager = get_test_manager();
        let data = b"Valid Payload";
        let mut encrypted = manager.encrypt(data).unwrap();

        // Tamper with the ciphertext
        encrypted[25] ^= 0x01;

        let result = manager.decrypt(&encrypted);
        assert!(result.is_err(), "Decryption should mathematically fail on tampered ciphertext");
    }

    #[tokio::test]
    async fn test_profile_id_empty_and_special() {
        let empty_hash = DbProfileManager::get_profile_id("");
        assert_eq!(empty_hash.len(), 64);

        let special_chars = "👉👈🥺 \n\r\t \x00 null byte included";
        let special_hash = DbProfileManager::get_profile_id(special_chars);
        assert_eq!(special_hash.len(), 64);
    }

    #[tokio::test]
    async fn test_memory_invalid_file_graceful_handling() {
        let mem = Memory::new("/tmp/this_file_definitely_does_not_exist_123.json");
        let examples = mem.get_relevant_examples("", 10).await;
        assert_eq!(examples.len(), 0);
    }
}
