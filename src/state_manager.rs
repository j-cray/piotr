use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use crate::ai::Content;

pub type DestinationInfo = (String, Option<String>); // (source, group_id)

pub struct ContextRequest {
    pub prompt: String,
    pub timestamp: u64,
    pub profile_key: String,
    pub source_name: Option<String>,
}

pub type SequencerMap = HashMap<
    String,
    mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>
>;

#[derive(Clone)]
pub struct StateManager {
    history: Arc<RwLock<HashMap<String, VecDeque<Content>>>>,
    sequencers: Arc<RwLock<SequencerMap>>,
    model_preferences: Arc<RwLock<HashMap<String, String>>>,
    sent_messages: Arc<RwLock<HashMap<u64, (String, String)>>>, // Timestamp -> (Prompt, Response)
}

impl StateManager {
    pub fn new() -> Self {
        Self {
            history: Arc::new(RwLock::new(HashMap::new())),
            sequencers: Arc::new(RwLock::new(HashMap::new())),
            model_preferences: Arc::new(RwLock::new(HashMap::new())),
            sent_messages: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // --- History Management ---
    pub async fn add_user_message(&self, context_key: &str, content: Content) {
        let mut history_guard = self.history.write().await;
        let chat_history = history_guard.entry(context_key.to_string()).or_insert_with(VecDeque::new);
        chat_history.push_back(content);
    }

    pub async fn add_model_message(&self, context_key: &str, content: Content) {
        let mut history_guard = self.history.write().await;
        let chat_history = history_guard.entry(context_key.to_string()).or_insert_with(VecDeque::new);
        chat_history.push_back(content);
    }

    pub async fn get_history_snapshot(&self, context_key: &str) -> Vec<Content> {
        let history_guard = self.history.read().await;
        if let Some(hist) = history_guard.get(context_key) {
            hist.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }

    pub async fn get_history_len(&self, context_key: &str) -> usize {
        let history_guard = self.history.read().await;
        history_guard.get(context_key).map_or(0, |hist| hist.len())
    }

    pub async fn clear_history(&self, context_key: &str) {
        let mut history_guard = self.history.write().await;
        history_guard.remove(context_key);
    }

    pub async fn prune_history(&self, context_key: &str, num_messages: usize) {
        let mut history_guard = self.history.write().await;
        if let Some(hist) = history_guard.get_mut(context_key) {
            for _ in 0..num_messages {
                if !hist.is_empty() {
                    hist.pop_front();
                }
            }
        }
    }

    pub async fn get_last_user_prompt(&self, context_key: &str) -> Option<String> {
        let history_guard = self.history.read().await;
        if let Some(hist) = history_guard.get(context_key) {
            // Need to find the last message with role == "user", usually it's the second to last if we just added "model"
            // But let's just search backwards for safety
            for msg in hist.iter().rev() {
                if msg.role == "user" {
                    return msg.parts.first().and_then(|p| p.text.clone());
                }
            }
        }
        None
    }

    // --- Sequencer Management ---
    pub async fn get_sequencer_tx(&self, context_key: &str) -> Option<mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>> {
        let seq_map = self.sequencers.read().await;
        seq_map.get(context_key).cloned()
    }

    pub async fn insert_sequencer_tx(&self, context_key: &str, tx: mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>) {
        let mut seq_map = self.sequencers.write().await;
        seq_map.insert(context_key.to_string(), tx);
    }

    // --- Model Preference Management ---
    pub async fn get_model_preference(&self, context_key: &str) -> Option<String> {
        let prefs = self.model_preferences.read().await;
        prefs.get(context_key).cloned()
    }

    pub async fn set_model_preference(&self, context_key: &str, model: &str) {
        let mut prefs = self.model_preferences.write().await;
        prefs.insert(context_key.to_string(), model.to_string());
    }

    pub async fn remove_model_preference(&self, context_key: &str) {
        let mut prefs = self.model_preferences.write().await;
        prefs.remove(context_key);
    }

    // --- Sent Messages Management ---
    pub async fn insert_sent_message(&self, timestamp: u64, prompt: String, response: String) {
        let mut sent_guard = self.sent_messages.write().await;
        sent_guard.insert(timestamp, (prompt, response));
    }

    pub async fn get_sent_message(&self, timestamp: u64) -> Option<(String, String)> {
        let sent_guard = self.sent_messages.read().await;
        sent_guard.get(&timestamp).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Part;

    #[tokio::test]
    async fn test_state_manager_history() {
        let state = StateManager::new();
        let ctx = "user123";

        // Initial state
        assert_eq!(state.get_history_len(ctx).await, 0);

        // Add user message
        let user_msg = Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("Hello Piotr".to_string()) }],
        };
        state.add_user_message(ctx, user_msg.clone()).await;
        assert_eq!(state.get_history_len(ctx).await, 1);

        // Add model message
        let model_msg = Content {
            role: "model".to_string(),
            parts: vec![Part { text: Some("Hello Human".to_string()) }],
        };
        state.add_model_message(ctx, model_msg).await;
        assert_eq!(state.get_history_len(ctx).await, 2);

        // Check snapshot
        let snap = state.get_history_snapshot(ctx).await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].role, "user");
        assert_eq!(snap[1].role, "model");

        // Last user prompt
        let last_prompt = state.get_last_user_prompt(ctx).await;
        assert_eq!(last_prompt.unwrap(), "Hello Piotr");

        // Clear
        state.clear_history(ctx).await;
        assert_eq!(state.get_history_len(ctx).await, 0);
    }

    #[tokio::test]
    async fn test_state_manager_pruning() {
        let state = StateManager::new();
        let ctx = "group_test";

        for i in 0..10 {
            let msg = Content {
                role: "user".to_string(),
                parts: vec![Part { text: Some(format!("msg {}", i)) }],
            };
            state.add_user_message(ctx, msg).await;
        }

        assert_eq!(state.get_history_len(ctx).await, 10);
        state.prune_history(ctx, 4).await;
        assert_eq!(state.get_history_len(ctx).await, 6);

        let snap = state.get_history_snapshot(ctx).await;
        assert_eq!(snap[0].parts[0].text.as_deref().unwrap(), "msg 4");
    }

    #[tokio::test]
    async fn test_state_manager_preferences() {
        let state = StateManager::new();
        let ctx = "user_pref";

        assert_eq!(state.get_model_preference(ctx).await, None);

        state.set_model_preference(ctx, "gemini-3-pro-preview").await;
        assert_eq!(state.get_model_preference(ctx).await.unwrap(), "gemini-3-pro-preview");

        state.remove_model_preference(ctx).await;
        assert_eq!(state.get_model_preference(ctx).await, None);
    }

    #[tokio::test]
    async fn test_state_manager_sent_messages() {
        let state = StateManager::new();
        let ts = 123456789;

        assert_eq!(state.get_sent_message(ts).await, None);

        state.insert_sent_message(ts, "prompt?".to_string(), "response!".to_string()).await;
        let (p, r) = state.get_sent_message(ts).await.unwrap();
        assert_eq!(p, "prompt?");
        assert_eq!(r, "response!");
    }

    #[tokio::test]
    async fn test_state_manager_sequencer() {
        let state = StateManager::new();
        let ctx = "seq_test";

        assert!(state.get_sequencer_tx(ctx).await.is_none());

        let (tx, _rx) = mpsc::unbounded_channel();
        state.insert_sequencer_tx(ctx, tx).await;

        assert!(state.get_sequencer_tx(ctx).await.is_some());
    }

    #[tokio::test]
    async fn test_state_manager_get_last_user_prompt_edge_cases() {
        let state = StateManager::new();
        let ctx = "edge_cases";

        // Empty history
        assert_eq!(state.get_last_user_prompt(ctx).await, None);

        // Only model messages
        let model_msg = Content {
            role: "model".to_string(),
            parts: vec![Part { text: Some("Model text".to_string()) }],
        };
        state.add_model_message(ctx, model_msg).await;
        assert_eq!(state.get_last_user_prompt(ctx).await, None);

        // Multiple user messages, ensure we get the LAST one
        let user_msg1 = Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("First".to_string()) }],
        };
        let user_msg2 = Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("Second".to_string()) }],
        };
        state.add_user_message(ctx, user_msg1).await;
        state.add_user_message(ctx, user_msg2).await;

        let last_prompt = state.get_last_user_prompt(ctx).await;
        assert_eq!(last_prompt.unwrap(), "Second");
    }

    #[tokio::test]
    async fn test_state_manager_prune_edge_cases() {
        let state = StateManager::new();
        let ctx = "prune_edge";

        // Pruning empty history shouldn't panic
        state.prune_history(ctx, 5).await;
        assert_eq!(state.get_history_len(ctx).await, 0);

        let msg = Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("msg".to_string()) }],
        };
        state.add_user_message(ctx, msg).await;

        // Pruning more than existing
        state.prune_history(ctx, 10).await;
        assert_eq!(state.get_history_len(ctx).await, 0);

        // Pruning exactly 0
        let msg2 = Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("msg2".to_string()) }],
        };
        state.add_user_message(ctx, msg2).await;
        state.prune_history(ctx, 0).await;
        assert_eq!(state.get_history_len(ctx).await, 1);
    }

    #[tokio::test]
    async fn test_state_manager_concurrency() {
        // Test that StateManager handles concurrent updates correctly
        let state = Arc::new(StateManager::new());
        let ctx = "concurrent";
        let mut handles = vec![];

        for i in 0..100 {
            let state_clone = Arc::clone(&state);
            handles.push(tokio::spawn(async move {
                let msg = Content {
                    role: "user".to_string(),
                    parts: vec![Part { text: Some(format!("msg_{}", i)) }],
                };
                state_clone.add_user_message(ctx, msg).await;
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(state.get_history_len(ctx).await, 100);
    }

    #[tokio::test]
    async fn test_state_manager_clear_history_non_existent() {
        let state = StateManager::new();
        let ctx = "non_existent";
        // Should not panic
        state.clear_history(ctx).await;
        assert_eq!(state.get_history_len(ctx).await, 0);
    }

    #[tokio::test]
    async fn test_state_manager_get_history_len_empty() {
        let state = StateManager::new();
        let ctx = "empty_len";
        assert_eq!(state.get_history_len(ctx).await, 0);
    }

    #[tokio::test]
    async fn test_state_manager_remove_model_preference_non_existent() {
        let state = StateManager::new();
        let ctx = "non_existent_pref";
        // Should not panic
        state.remove_model_preference(ctx).await;
        assert_eq!(state.get_model_preference(ctx).await, None);
    }

    #[tokio::test]
    async fn test_state_manager_get_sent_message_non_existent() {
        let state = StateManager::new();
        // Should return None
        assert_eq!(state.get_sent_message(999999).await, None);
    }
}
