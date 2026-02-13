use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::fs;
use anyhow::Result;
use crate::ai::ReactionAnalysis;

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

        // check if exists (simple dedup based on prompt+response)
        if let Some(existing) = guard.iter_mut().find(|i| i.prompt == prompt && i.response == response) {
            // Update analysis if new one is "stronger" or just overwrite?
            // For now, let's overwrite with the latest reaction analysis
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

        self.save(&guard)?;
        Ok(())
    }

    pub async fn get_relevant_examples(&self, _query: &str, limit: usize) -> Vec<Interaction> {
        let guard = self.interactions.lock().await;
        // For now, just return the highest rated recent ones
        // In a real system, we'd use embeddings or keyword matching
        let mut sorted: Vec<Interaction> = guard.clone();
        // Sort by sentiment score descending
        sorted.sort_by(|a, b| b.analysis.sentiment_score.partial_cmp(&a.analysis.sentiment_score).unwrap_or(std::cmp::Ordering::Equal));
        sorted.into_iter().take(limit).collect()
    }

    fn save(&self, interactions: &[Interaction]) -> Result<()> {
        let json = serde_json::to_string_pretty(interactions)?;
        fs::write(&self.file_path, json)?;
        Ok(())
    }
    }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_memory_analysis_persistence() -> Result<()> {
        let temp_file = NamedTempFile::new()?;
        let path = temp_file.path().to_str().unwrap();
        let memory = Memory::new(path);

        let analysis1 = ReactionAnalysis {
            sentiment_score: 0.8,
            reasoning: "Good job".to_string(),
            tags: vec!["positive".to_string()]
        };

        memory.add_interaction("Prompt1".to_string(), "Response1".to_string(), analysis1).await?;

        let analysis2 = ReactionAnalysis {
            sentiment_score: -0.5,
            reasoning: "Bad job".to_string(),
            tags: vec!["negative".to_string(), "sarcasm".to_string()]
        };

        memory.add_interaction("Prompt2".to_string(), "Response2".to_string(), analysis2).await?;

        // Retrieve
        let examples = memory.get_relevant_examples("", 10).await;
        assert_eq!(examples.len(), 2);

        // Sorting check (0.8 > -0.5)
        assert_eq!(examples[0].prompt, "Prompt1");
        assert_eq!(examples[0].analysis.sentiment_score, 0.8);
        assert_eq!(examples[1].prompt, "Prompt2");
        assert_eq!(examples[1].analysis.tags.len(), 2);

        Ok(())
    }
}


