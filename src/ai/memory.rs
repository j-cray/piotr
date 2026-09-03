use crate::ai::ReactionAnalysis;
use anyhow::{Context, Result};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

fn encrypt_blob(encryption_key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(encryption_key.into());
    let mut nonce_bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, data)
        .map_err(|e| anyhow::anyhow!("Encryption failure: {:?}", e))?;

    // Prepend nonce to ciphertext
    let mut result = nonce_bytes.to_vec();
    result.extend(ciphertext);
    Ok(result)
}

fn decrypt_blob(encryption_key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 24 {
        anyhow::bail!("Encrypted blob too short");
    }
    let nonce_bytes = &blob[..24];
    let ciphertext = &blob[24..];
    let nonce = XNonce::from_slice(nonce_bytes);

    let cipher = XChaCha20Poly1305::new(encryption_key.into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failure: {:?}", e))?;

    Ok(plaintext)
}

fn compute_interaction_hash(prompt: &str, response: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    hasher.update(b":::");
    hasher.update(response.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interaction {
    #[serde(default)]
    pub context_key: String,
    pub prompt: String,
    pub response: String,
    pub analysis: ReactionAnalysis,
    pub timestamp: u64,
}

#[derive(Clone)]
pub struct Memory {
    pool: SqlitePool,
    encryption_key: [u8; 32],
}

impl Memory {
    pub fn new(pool: SqlitePool, key_hex: &str) -> Result<Self> {
        let key_bytes =
            hex::decode(key_hex).context("Failed to decode PROFILE_ENCRYPTION_KEY hex")?;
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

    pub fn from_key(pool: SqlitePool, encryption_key: [u8; 32]) -> Self {
        Self {
            pool,
            encryption_key,
        }
    }

    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        encrypt_blob(&self.encryption_key, data)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>> {
        decrypt_blob(&self.encryption_key, blob)
    }

    pub async fn add_interaction(
        &self,
        context_key: &str,
        prompt: String,
        response: String,
        analysis: ReactionAnalysis,
    ) -> Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as i64;

        let interaction_hash = compute_interaction_hash(&prompt, &response);
        let sentiment_score = analysis.sentiment_score;

        let interaction = Interaction {
            context_key: context_key.to_string(),
            prompt,
            response,
            analysis,
            timestamp: timestamp as u64,
        };

        let json = serde_json::to_vec(&interaction)?;
        let encrypted_blob = self.encrypt(&json)?;

        sqlx::query(
            "INSERT INTO learned_behaviors (context_key, interaction_hash, sentiment_score, encrypted_blob, timestamp)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(context_key, interaction_hash) DO UPDATE SET
                 sentiment_score = excluded.sentiment_score,
                 encrypted_blob = excluded.encrypted_blob,
                 timestamp = excluded.timestamp"
        )
        .bind(context_key)
        .bind(&interaction_hash)
        .bind(sentiment_score)
        .bind(encrypted_blob)
        .bind(timestamp)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_relevant_examples(
        &self,
        context_key: &str,
        _query: &str,
        limit: usize,
    ) -> Vec<Interaction> {
        let rows: Result<Vec<(Vec<u8>, i64)>, _> = sqlx::query_as(
            "SELECT encrypted_blob, timestamp FROM learned_behaviors
             WHERE context_key = ?
             ORDER BY sentiment_score DESC
             LIMIT ?"
        )
        .bind(context_key)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await;

        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to fetch relevant examples: {:?}", e);
                return Vec::new();
            }
        };

        let mut examples = Vec::new();
        for (blob, timestamp) in rows {
            match self.decrypt(&blob) {
                Ok(plaintext) => match serde_json::from_slice::<Interaction>(&plaintext) {
                    Ok(mut interaction) => {
                        interaction.context_key = context_key.to_string();
                        interaction.timestamp = timestamp as u64;
                        examples.push(interaction);
                    }
                    Err(e) => {
                        tracing::error!("Failed to deserialize interaction payload: {:?}", e);
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to decrypt interaction blob: {:?}", e);
                }
            }
        }

        examples
    }

    pub async fn migrate_json_file(&self, file_path: &str) -> Result<()> {
        let path = std::path::Path::new(file_path);
        if !path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(path).await?;
        let interactions: Vec<Interaction> = match serde_json::from_str(&content) {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!("Could not parse {:?} as JSON interaction list: {:?}", path, e);
                return Ok(());
            }
        };

        tracing::info!(
            "Migrating {} learned behaviors from {:?} to SQLite",
            interactions.len(),
            path
        );
        for item in interactions {
            if let Err(e) = self
                .add_interaction(
                    &item.context_key,
                    item.prompt,
                    item.response,
                    item.analysis,
                )
                .await
            {
                tracing::error!("Failed to migrate learned behavior item: {:?}", e);
            }
        }

        let new_path = path.with_extension("json.imported");
        let _ = tokio::fs::rename(path, new_path).await;
        tracing::info!(
            "Successfully migrated learned behaviors and renamed to {:?}",
            path.with_extension("json.imported")
        );
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
    pool: SqlitePool,
    encryption_key: [u8; 32],
}

impl DbProfileManager {
    pub fn new(pool: SqlitePool, key_hex: &str) -> Result<Self> {
        let key_bytes =
            hex::decode(key_hex).context("Failed to decode PROFILE_ENCRYPTION_KEY hex")?;
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
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(raw_id.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        encrypt_blob(&self.encryption_key, data)
    }

    fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>> {
        decrypt_blob(&self.encryption_key, blob)
    }

    pub async fn get_profile(
        &self,
        raw_id: &str,
        current_name: Option<String>,
    ) -> Result<UserProfile> {
        let id = Self::get_profile_id(raw_id);

        // Fetch from DB
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT encrypted_blob FROM user_profiles WHERE user_id = ?")
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
        if let Some(new_name) = current_name
            && profile.name.as_ref() != Some(&new_name)
        {
            profile.name = Some(new_name);
            self.save_profile(&profile).await?;
        }

        Ok(profile)
    }

    pub async fn save_profile(&self, profile: &UserProfile) -> Result<()> {
        let json = serde_json::to_vec(profile)?;
        let blob = self.encrypt(&json)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO user_profiles (user_id, encrypted_blob, last_updated) VALUES (?, ?, ?)
             ON CONFLICT(user_id) DO UPDATE SET encrypted_blob = excluded.encrypted_blob, last_updated = excluded.last_updated"
        )
        .bind(&profile.id)
        .bind(blob)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_group_profile(
        &self,
        raw_id: &str,
        current_name: Option<String>,
    ) -> Result<GroupProfile> {
        let id = Self::get_profile_id(raw_id);

        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT encrypted_blob FROM group_profiles WHERE group_id = ?")
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

        if let Some(new_name) = current_name
            && profile.group_name.as_ref() != Some(&new_name)
        {
            profile.group_name = Some(new_name);
            self.save_group_profile(&profile).await?;
        }

        Ok(profile)
    }

    pub async fn save_group_profile(&self, profile: &GroupProfile) -> Result<()> {
        let json = serde_json::to_vec(profile)?;
        let blob = self.encrypt(&json)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO group_profiles (group_id, encrypted_blob, last_updated) VALUES (?, ?, ?)
             ON CONFLICT(group_id) DO UPDATE SET encrypted_blob = excluded.encrypted_blob, last_updated = excluded.last_updated"
        )
        .bind(&profile.id)
        .bind(blob)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn migrate_json_profiles(&self, data_dir: &str) -> Result<()> {
        let paths = tokio::fs::read_dir(data_dir).await;
        if let Ok(mut entries) = paths {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    let content = tokio::fs::read_to_string(&path).await?;
                    match serde_json::from_str::<UserProfile>(&content) {
                        Ok(profile) => {
                            tracing::info!("Migrating profile for {}", profile.id);
                            if let Err(e) = self.save_profile(&profile).await {
                                tracing::error!(
                                    "Failed to migrate profile {}: {:?}",
                                    profile.id,
                                    e
                                );
                            } else {
                                // Rename to .imported
                                let new_path = path.with_extension("json.imported");
                                let _ = tokio::fs::rename(path, new_path).await;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse profile {:?}: {:?}", path, e);
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
            context_key: "chat1".to_string(),
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
            context_key: "chat1".to_string(),
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
            context_key: "chat1".to_string(),
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

        sorted.sort_by(|a, b| {
            b.analysis
                .sentiment_score
                .partial_cmp(&a.analysis.sentiment_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        assert_eq!(sorted[0].prompt, "2"); // 0.9
        assert_eq!(sorted[1].prompt, "1"); // 0.1
        assert_eq!(sorted[2].prompt, "3"); // -0.5
    }

    // --- SECURITY TESTS ---

    fn get_test_manager() -> DbProfileManager {
        // Mock pool isn't needed for encryption isolated testing, but struct requires it.
        // We can test encrypt/decrypt methods directly if we instantiate with dummy key.
        // Note: any test making actual DB calls through this pool will fail at runtime
        // rather than at setup. Given none of the encryption tests use the pool, this is acceptable.
        DbProfileManager {
            pool: sqlx::sqlite::SqlitePoolOptions::new()
                .connect_lazy("sqlite::memory:")
                .unwrap(),
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
        assert_ne!(
            run1, run2,
            "Encryption lacks entropy; same plaintext produced same ciphertext"
        );

        // Decryption should still succeed for both
        assert_eq!(manager.decrypt(&run1).unwrap(), data);
        assert_eq!(manager.decrypt(&run2).unwrap(), data);
    }

    #[tokio::test]
    async fn test_decryption_too_short() {
        let manager = get_test_manager();
        let short_blob = vec![1u8; 23];
        let result = manager.decrypt(&short_blob);
        assert!(
            result.is_err(),
            "Decryption should fail on blobs smaller than nonce size"
        );
    }

    #[tokio::test]
    async fn test_decryption_tampering() {
        let manager = get_test_manager();
        let data = b"Valid Payload";
        let mut encrypted = manager.encrypt(data).unwrap();

        // Tamper with the ciphertext
        encrypted[25] ^= 0x01;

        let result = manager.decrypt(&encrypted);
        assert!(
            result.is_err(),
            "Decryption should mathematically fail on tampered ciphertext"
        );
    }

    #[tokio::test]
    async fn test_profile_id_empty_and_special() {
        let empty_hash = DbProfileManager::get_profile_id("");
        assert_eq!(empty_hash.len(), 64);

        let special_chars = "👉👈🥺 \n\r\t \x00 null byte included";
        let special_hash = DbProfileManager::get_profile_id(special_chars);
        assert_eq!(special_hash.len(), 64);
    }

    /// Ensure that migrations and profile persistence work end-to-end on SQLite.
    #[tokio::test]
    async fn test_db_profile_round_trip() {
        // Set up an in-memory SQLite database and run migrations.
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        // Create a DbProfileManager using the migrated pool.
        let manager = DbProfileManager {
            pool,
            encryption_key: [1u8; 32],
        };

        // Use a stable raw identifier and derive the stored profile ID.
        let raw_id = "+15555550000";
        let profile_id = DbProfileManager::get_profile_id(raw_id);

        // Create a UserProfile for round-trip testing.
        let profile = UserProfile {
            id: profile_id.clone(),
            name: Some("Alice".to_string()),
            nickname: None,
            personality_summary: "Test profile".to_string(),
            interaction_style: "testing".to_string(),
            topics_of_interest: vec!["rust".to_string()],
            last_updated: 1234567890,
        };

        // Save the profile and then read it back.
        manager.save_profile(&profile).await.unwrap();
        let loaded = manager.get_profile(raw_id, None).await.unwrap();

        assert_eq!(loaded.id, profile.id, "loaded profile ID does not match");
        assert_eq!(
            loaded.name, profile.name,
            "loaded profile name does not match"
        );
        assert_eq!(
            loaded.personality_summary, profile.personality_summary,
            "loaded profile personality does not match"
        );
        assert_eq!(
            loaded.interaction_style, profile.interaction_style,
            "loaded profile style does not match"
        );
        assert_eq!(
            loaded.topics_of_interest, profile.topics_of_interest,
            "loaded profile topics do not match"
        );
    }

    #[tokio::test]
    async fn test_memory_empty_db_graceful_handling() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        let mem = Memory::from_key(pool, [1u8; 32]);
        let examples = mem.get_relevant_examples("chat1", "", 10).await;
        assert_eq!(examples.len(), 0);
    }

    #[tokio::test]
    async fn test_memory_context_isolation() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        let mem = Memory::from_key(pool.clone(), [1u8; 32]);

        let analysis_chat1 = ReactionAnalysis {
            sentiment_score: 0.9,
            reasoning: "Great answer in chat 1".to_string(),
            tags: vec!["helpful".to_string()],
        };
        let analysis_chat2 = ReactionAnalysis {
            sentiment_score: 0.8,
            reasoning: "Good answer in chat 2".to_string(),
            tags: vec!["funny".to_string()],
        };

        // Add interaction to group chat A
        mem.add_interaction(
            "group_a",
            "Secret from Group A".to_string(),
            "Answer A".to_string(),
            analysis_chat1,
        )
        .await
        .unwrap();

        // Add interaction to group chat B
        mem.add_interaction(
            "group_b",
            "Secret from Group B".to_string(),
            "Answer B".to_string(),
            analysis_chat2,
        )
        .await
        .unwrap();

        // Query examples for Group A: should ONLY contain Group A interaction
        let examples_a = mem.get_relevant_examples("group_a", "", 10).await;
        assert_eq!(examples_a.len(), 1);
        assert_eq!(examples_a[0].prompt, "Secret from Group A");
        assert_eq!(examples_a[0].context_key, "group_a");

        // Query examples for Group B: should ONLY contain Group B interaction
        let examples_b = mem.get_relevant_examples("group_b", "", 10).await;
        assert_eq!(examples_b.len(), 1);
        assert_eq!(examples_b[0].prompt, "Secret from Group B");
        assert_eq!(examples_b[0].context_key, "group_b");

        // Query examples for unknown group C: should be empty
        let examples_c = mem.get_relevant_examples("group_c", "", 10).await;
        assert_eq!(examples_c.len(), 0);

        // Verify persistence with a new instance sharing the same pool preserves isolation
        let reloaded_mem = Memory::from_key(pool, [1u8; 32]);
        let reloaded_a = reloaded_mem.get_relevant_examples("group_a", "", 10).await;
        assert_eq!(reloaded_a.len(), 1);
        assert_eq!(reloaded_a[0].prompt, "Secret from Group A");

        let reloaded_b = reloaded_mem.get_relevant_examples("group_b", "", 10).await;
        assert_eq!(reloaded_b.len(), 1);
        assert_eq!(reloaded_b[0].prompt, "Secret from Group B");
    }

    #[tokio::test]
    async fn test_memory_upsert_deduplication() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        let mem = Memory::from_key(pool, [1u8; 32]);

        let analysis_v1 = ReactionAnalysis {
            sentiment_score: 0.5,
            reasoning: "Okay response".to_string(),
            tags: vec!["meh".to_string()],
        };
        mem.add_interaction(
            "chat1",
            "What is 2+2?".to_string(),
            "4".to_string(),
            analysis_v1,
        )
        .await
        .unwrap();

        let analysis_v2 = ReactionAnalysis {
            sentiment_score: 1.0,
            reasoning: "Loved it".to_string(),
            tags: vec!["perfect".to_string()],
        };
        mem.add_interaction(
            "chat1",
            "What is 2+2?".to_string(),
            "4".to_string(),
            analysis_v2,
        )
        .await
        .unwrap();

        let examples = mem.get_relevant_examples("chat1", "", 10).await;
        assert_eq!(examples.len(), 1);
        assert_eq!(examples[0].analysis.sentiment_score, 1.0);
        assert_eq!(examples[0].analysis.reasoning, "Loved it");
    }

    #[tokio::test]
    async fn test_memory_json_migration() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        let mem = Memory::from_key(pool, [1u8; 32]);

        let temp_dir = tempfile::tempdir().unwrap();
        let json_file = temp_dir.path().join("learned_behaviors.json");
        let sample_json = r#"[
            {
                "context_key": "imported_chat",
                "prompt": "Hello",
                "response": "Hi there!",
                "analysis": {
                    "sentiment_score": 0.95,
                    "reasoning": "Friendly greeting",
                    "tags": ["friendly"]
                },
                "timestamp": 1234567890
            }
        ]"#;
        tokio::fs::write(&json_file, sample_json).await.unwrap();

        mem.migrate_json_file(json_file.to_str().unwrap()).await.unwrap();

        // Check that json was renamed to .json.imported
        assert!(!json_file.exists());
        assert!(temp_dir.path().join("learned_behaviors.json.imported").exists());

        // Check that the interaction was imported into SQLite
        let examples = mem.get_relevant_examples("imported_chat", "", 10).await;
        assert_eq!(examples.len(), 1);
        assert_eq!(examples[0].prompt, "Hello");
        assert_eq!(examples[0].response, "Hi there!");
        assert_eq!(examples[0].analysis.sentiment_score, 0.95);
    }
}
