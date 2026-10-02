use crate::ai::Content;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::info;

pub type DestinationInfo = (String, Option<String>); // (source, group_id)

pub struct ContextRequest {
    pub prompt: String,
    pub timestamp: u64,
    pub profile_key: String,
    pub source_name: Option<String>,
    pub is_explicit_interaction: bool,
}

pub type SequencerMap = HashMap<String, mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>>;

// Enum defining all operations our Actor will handle
pub enum StateCommand {
    AddUserMessage {
        context_key: String,
        content: Content,
    },
    AddModelMessage {
        context_key: String,
        content: Content,
    },
    GetHistorySnapshot {
        context_key: String,
        resp: oneshot::Sender<Vec<Content>>,
    },
    GetHistoryLen {
        context_key: String,
        resp: oneshot::Sender<usize>,
    },
    ClearHistory {
        context_key: String,
    },
    ResetSession {
        context_key: String,
    },
    PruneHistory {
        context_key: String,
        num_messages: usize,
    },
    GetLastUserPrompt {
        context_key: String,
        resp: oneshot::Sender<Option<String>>,
    },
    GetSequencerTx {
        context_key: String,
        resp: oneshot::Sender<Option<mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>>>,
    },
    InsertSequencerTx {
        context_key: String,
        tx: mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>,
    },
    GetModelPreference {
        context_key: String,
        resp: oneshot::Sender<Option<String>>,
    },
    SetModelPreference {
        context_key: String,
        model: String,
    },
    RemoveModelPreference {
        context_key: String,
    },
    InsertSentMessage {
        timestamp: u64,
        context_key: String,
        prompt: String,
        response: String,
    },
    GetSentMessage {
        timestamp: u64,
        resp: oneshot::Sender<Option<(String, String, String)>>,
    },
    GetLastSearchSources {
        context_key: String,
        resp: oneshot::Sender<Option<Vec<String>>>,
    },
    SetLastSearchSources {
        context_key: String,
        sources: Vec<String>,
    },
    GetSessionTimeout {
        resp: oneshot::Sender<Duration>,
    },
    SetSessionTimeout {
        timeout: Duration,
    },
    CleanupExpiredSessions {
        resp: oneshot::Sender<usize>,
    },
    TouchSession {
        context_key: String,
    },
}

// The internal state holding struct running in the background task
struct StateActor {
    history: HashMap<String, VecDeque<Content>>,
    history_order: VecDeque<String>, // Tracks access order for LRU eviction
    last_activity: HashMap<String, Instant>,
    session_timeout: Duration,
    sequencers: SequencerMap,
    sequencers_order: VecDeque<String>, // Tracks access order for LRU eviction
    model_preferences: HashMap<String, String>,
    sent_messages: HashMap<u64, (String, String, String)>, // Timestamp -> (ContextKey, Prompt, Response)
    sent_messages_order: VecDeque<u64>,                    // Insertion order for eviction
    search_sources: HashMap<String, Vec<String>>,
    receiver: mpsc::Receiver<StateCommand>,
}

const MAX_SENT_MESSAGES: usize = 10_000;
const MAX_SEQUENCERS: usize = 1_000;
const MAX_HISTORY_CONTEXTS: usize = 10_000;

impl StateActor {
    fn new(receiver: mpsc::Receiver<StateCommand>, session_timeout: Duration) -> Self {
        Self {
            history: HashMap::new(),
            history_order: VecDeque::new(),
            last_activity: HashMap::new(),
            session_timeout,
            sequencers: HashMap::new(),
            sequencers_order: VecDeque::new(),
            model_preferences: HashMap::new(),
            sent_messages: HashMap::new(),
            sent_messages_order: VecDeque::new(),
            search_sources: HashMap::new(),
            receiver,
        }
    }

    fn touch_history(&mut self, context_key: &str) {
        if self.history.contains_key(context_key) {
            self.history_order.retain(|x| x != context_key);
            self.history_order.push_back(context_key.to_string());
        }
    }

    fn check_history_capacity(&mut self) {
        if self.history.len() >= MAX_HISTORY_CONTEXTS
            && let Some(oldest) = self.history_order.pop_front()
        {
            self.history.remove(&oldest);
            self.last_activity.remove(&oldest);
            self.search_sources.remove(&oldest);
        }
    }

    fn reset_session(&mut self, context_key: &str) {
        self.history.remove(context_key);
        self.history_order.retain(|x| x != context_key);
        self.last_activity.remove(context_key);
        self.search_sources.remove(context_key);
    }

    fn is_session_expired(&self, context_key: &str) -> bool {
        if self.session_timeout.is_zero() {
            return false;
        }
        if let Some(&last_time) = self.last_activity.get(context_key) {
            Instant::now().duration_since(last_time) >= self.session_timeout
        } else {
            false
        }
    }

    fn reset_session_if_expired(&mut self, context_key: &str) -> bool {
        if self.is_session_expired(context_key) {
            info!(
                "Session expired for context {}; resetting history",
                crate::utils::anonymize(context_key)
            );
            self.reset_session(context_key);
            true
        } else {
            false
        }
    }

    fn cleanup_expired_sessions(&mut self) -> usize {
        if self.session_timeout.is_zero() {
            return 0;
        }
        let now = Instant::now();
        let expired: Vec<String> = self
            .last_activity
            .iter()
            .filter(|(_, last_time)| now.duration_since(**last_time) >= self.session_timeout)
            .map(|(k, _)| k.clone())
            .collect();

        let count = expired.len();
        for key in expired {
            info!(
                "Session reset timer fired for context {}; clearing conversation history",
                crate::utils::anonymize(&key)
            );
            self.reset_session(&key);
        }
        count
    }

    async fn run(mut self) {
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(60));
        cleanup_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                cmd_opt = self.receiver.recv() => {
                    match cmd_opt {
                        Some(cmd) => self.handle_command(cmd),
                        None => break,
                    }
                }
                _ = cleanup_interval.tick() => {
                    self.cleanup_expired_sessions();
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: StateCommand) {
        match cmd {
            StateCommand::AddUserMessage {
                context_key,
                content,
            } => {
                self.reset_session_if_expired(&context_key);
                self.last_activity
                    .insert(context_key.clone(), Instant::now());
                if !self.history.contains_key(&context_key) {
                    self.check_history_capacity();
                    self.history_order.push_back(context_key.clone());
                } else {
                    self.touch_history(&context_key);
                }
                let chat_history = self.history.entry(context_key).or_default();
                chat_history.push_back(content);
            }
            StateCommand::AddModelMessage {
                context_key,
                content,
            } => {
                self.last_activity
                    .insert(context_key.clone(), Instant::now());
                if !self.history.contains_key(&context_key) {
                    self.check_history_capacity();
                    self.history_order.push_back(context_key.clone());
                } else {
                    self.touch_history(&context_key);
                }
                let chat_history = self.history.entry(context_key).or_default();
                chat_history.push_back(content);
            }
            StateCommand::GetHistorySnapshot { context_key, resp } => {
                self.reset_session_if_expired(&context_key);
                let snapshot = if self.history.contains_key(&context_key) {
                    self.touch_history(&context_key);
                    self.history
                        .get(&context_key)
                        .unwrap()
                        .iter()
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                let _ = resp.send(snapshot);
            }
            StateCommand::GetHistoryLen { context_key, resp } => {
                self.reset_session_if_expired(&context_key);
                let len = if self.history.contains_key(&context_key) {
                    self.touch_history(&context_key);
                    self.history.get(&context_key).unwrap().len()
                } else {
                    0
                };
                let _ = resp.send(len);
            }
            StateCommand::ClearHistory { context_key }
            | StateCommand::ResetSession { context_key } => {
                self.reset_session(&context_key);
            }
            StateCommand::PruneHistory {
                context_key,
                num_messages,
            } => {
                if !self.reset_session_if_expired(&context_key)
                    && self.history.contains_key(&context_key)
                {
                    self.touch_history(&context_key);
                    let hist = self.history.get_mut(&context_key).unwrap();
                    for _ in 0..num_messages {
                        if !hist.is_empty() {
                            hist.pop_front();
                        }
                    }
                }
            }
            StateCommand::GetLastUserPrompt { context_key, resp } => {
                self.reset_session_if_expired(&context_key);
                let mut result = None;
                if self.history.contains_key(&context_key) {
                    self.touch_history(&context_key);
                    let hist = self.history.get(&context_key).unwrap();
                    for msg in hist.iter().rev() {
                        if msg.role == "user" {
                            result = msg.parts.first().and_then(|p| p.text.clone());
                            break;
                        }
                    }
                }
                let _ = resp.send(result);
            }
            StateCommand::GetSequencerTx { context_key, resp } => {
                let mut is_closed = false;
                let tx = if let Some(sender) = self.sequencers.get(&context_key) {
                    if sender.is_closed() {
                        is_closed = true;
                        None
                    } else {
                        Some(sender.clone())
                    }
                } else {
                    None
                };

                if is_closed {
                    self.sequencers.remove(&context_key);
                    self.sequencers_order.retain(|x| x != &context_key);
                } else if tx.is_some() {
                    // Mark as recently used (LRU)
                    self.sequencers_order.retain(|x| x != &context_key);
                    self.sequencers_order.push_back(context_key.clone());
                }

                let _ = resp.send(tx);
            }
            StateCommand::InsertSequencerTx { context_key, tx } => {
                // Cleanup any closed sequencers first to save capacity
                self.sequencers.retain(|_, sender| !sender.is_closed());
                self.sequencers_order
                    .retain(|k| self.sequencers.contains_key(k));

                if !self.sequencers.contains_key(&context_key)
                    && self.sequencers.len() >= MAX_SEQUENCERS
                    && let Some(oldest) = self.sequencers_order.pop_front()
                {
                    self.sequencers.remove(&oldest);
                }

                if !self.sequencers.contains_key(&context_key) {
                    self.sequencers_order.push_back(context_key.clone());
                } else {
                    // Move to back as it was just updated (LRU)
                    self.sequencers_order.retain(|x| x != &context_key);
                    self.sequencers_order.push_back(context_key.clone());
                }

                self.sequencers.insert(context_key, tx);
            }
            StateCommand::GetModelPreference { context_key, resp } => {
                let pref = self.model_preferences.get(&context_key).cloned();
                let _ = resp.send(pref);
            }
            StateCommand::SetModelPreference { context_key, model } => {
                self.model_preferences.insert(context_key, model);
            }
            StateCommand::RemoveModelPreference { context_key } => {
                self.model_preferences.remove(&context_key);
            }
            StateCommand::InsertSentMessage {
                timestamp,
                context_key,
                prompt,
                response,
            } => {
                // Evict oldest entry if at capacity to prevent unbounded memory growth
                if self.sent_messages.len() >= MAX_SENT_MESSAGES
                    && let Some(oldest_ts) = self.sent_messages_order.pop_front()
                {
                    self.sent_messages.remove(&oldest_ts);
                }
                self.sent_messages
                    .insert(timestamp, (context_key, prompt, response));
                self.sent_messages_order.push_back(timestamp);
            }
            StateCommand::GetSentMessage { timestamp, resp } => {
                let msg = self.sent_messages.get(&timestamp).cloned();
                let _ = resp.send(msg);
            }
            StateCommand::GetLastSearchSources { context_key, resp } => {
                let sources = self.search_sources.get(&context_key).cloned();
                let _ = resp.send(sources);
            }
            StateCommand::SetLastSearchSources {
                context_key,
                sources,
            } => {
                self.search_sources.insert(context_key, sources);
            }
            StateCommand::GetSessionTimeout { resp } => {
                let _ = resp.send(self.session_timeout);
            }
            StateCommand::SetSessionTimeout { timeout } => {
                self.session_timeout = timeout;
            }
            StateCommand::CleanupExpiredSessions { resp } => {
                let count = self.cleanup_expired_sessions();
                let _ = resp.send(count);
            }
            StateCommand::TouchSession { context_key } => {
                if !self.reset_session_if_expired(&context_key) {
                    self.last_activity.insert(context_key, Instant::now());
                }
            }
        }
    }
}

pub const DEFAULT_SESSION_RESET_TIMEOUT: Duration = Duration::from_secs(7200);

// The external API Handle
#[derive(Clone)]
pub struct StateManager {
    sender: mpsc::Sender<StateCommand>,
}

impl Default for StateManager {
    fn default() -> Self {
        Self::with_session_timeout(DEFAULT_SESSION_RESET_TIMEOUT)
    }
}

impl StateManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_session_timeout(session_timeout: Duration) -> Self {
        // Create an unbounded channel or a bounded channel with a healthy buffer.
        // A bounded channel requires `await` or `try_send`, but usually we want memory bounds.
        // For simplicity and avoiding blocking threads heavily, we can use a large buffer.
        let (sender, receiver) = mpsc::channel(1000);
        let actor = StateActor::new(receiver, session_timeout);
        tokio::spawn(async move {
            actor.run().await;
        });

        Self { sender }
    }

    // --- History Management ---
    pub async fn add_user_message(&self, context_key: &str, content: Content) {
        let _ = self
            .sender
            .send(StateCommand::AddUserMessage {
                context_key: context_key.to_string(),
                content,
            })
            .await;
    }

    pub async fn add_model_message(&self, context_key: &str, content: Content) {
        let _ = self
            .sender
            .send(StateCommand::AddModelMessage {
                context_key: context_key.to_string(),
                content,
            })
            .await;
    }

    pub async fn get_history_snapshot(&self, context_key: &str) -> Vec<Content> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetHistorySnapshot {
                context_key: context_key.to_string(),
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    pub async fn get_history_len(&self, context_key: &str) -> usize {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetHistoryLen {
                context_key: context_key.to_string(),
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(0)
        } else {
            0
        }
    }

    pub async fn clear_history(&self, context_key: &str) {
        let _ = self
            .sender
            .send(StateCommand::ClearHistory {
                context_key: context_key.to_string(),
            })
            .await;
    }

    pub async fn prune_history(&self, context_key: &str, num_messages: usize) {
        let _ = self
            .sender
            .send(StateCommand::PruneHistory {
                context_key: context_key.to_string(),
                num_messages,
            })
            .await;
    }

    pub async fn get_last_user_prompt(&self, context_key: &str) -> Option<String> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetLastUserPrompt {
                context_key: context_key.to_string(),
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(None)
        } else {
            None
        }
    }

    // --- Sequencer Management ---
    pub async fn get_sequencer_tx(
        &self,
        context_key: &str,
    ) -> Option<mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetSequencerTx {
                context_key: context_key.to_string(),
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(None)
        } else {
            None
        }
    }

    pub async fn insert_sequencer_tx(
        &self,
        context_key: &str,
        tx: mpsc::UnboundedSender<(DestinationInfo, ContextRequest)>,
    ) {
        let _ = self
            .sender
            .send(StateCommand::InsertSequencerTx {
                context_key: context_key.to_string(),
                tx,
            })
            .await;
    }

    // --- Model Preference Management ---
    pub async fn get_model_preference(&self, context_key: &str) -> Option<String> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetModelPreference {
                context_key: context_key.to_string(),
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(None)
        } else {
            None
        }
    }

    pub async fn set_model_preference(&self, context_key: &str, model: &str) {
        let _ = self
            .sender
            .send(StateCommand::SetModelPreference {
                context_key: context_key.to_string(),
                model: model.to_string(),
            })
            .await;
    }

    pub async fn remove_model_preference(&self, context_key: &str) {
        let _ = self
            .sender
            .send(StateCommand::RemoveModelPreference {
                context_key: context_key.to_string(),
            })
            .await;
    }

    // --- Sent Messages Management ---
    pub async fn insert_sent_message(
        &self,
        timestamp: u64,
        context_key: String,
        prompt: String,
        response: String,
    ) {
        let _ = self
            .sender
            .send(StateCommand::InsertSentMessage {
                timestamp,
                context_key,
                prompt,
                response,
            })
            .await;
    }

    pub async fn get_sent_message(&self, timestamp: u64) -> Option<(String, String, String)> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetSentMessage {
                timestamp,
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(None)
        } else {
            None
        }
    }

    // --- Search Sources Management ---
    pub async fn get_last_search_sources(&self, context_key: &str) -> Option<Vec<String>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetLastSearchSources {
                context_key: context_key.to_string(),
                resp: resp_tx,
            })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(None)
        } else {
            None
        }
    }

    pub async fn set_last_search_sources(&self, context_key: &str, sources: Vec<String>) {
        let _ = self
            .sender
            .send(StateCommand::SetLastSearchSources {
                context_key: context_key.to_string(),
                sources,
            })
            .await;
    }

    // --- Session Reset Management ---
    pub async fn reset_session(&self, context_key: &str) {
        let _ = self
            .sender
            .send(StateCommand::ResetSession {
                context_key: context_key.to_string(),
            })
            .await;
    }

    pub async fn get_session_timeout(&self) -> Duration {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::GetSessionTimeout { resp: resp_tx })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or_default()
        } else {
            Duration::default()
        }
    }

    pub async fn set_session_timeout(&self, timeout: Duration) {
        let _ = self
            .sender
            .send(StateCommand::SetSessionTimeout { timeout })
            .await;
    }

    pub async fn cleanup_expired_sessions(&self) -> usize {
        let (resp_tx, resp_rx) = oneshot::channel();
        if self
            .sender
            .send(StateCommand::CleanupExpiredSessions { resp: resp_tx })
            .await
            .is_ok()
        {
            resp_rx.await.unwrap_or(0)
        } else {
            0
        }
    }

    pub async fn touch_session(&self, context_key: &str) {
        let _ = self
            .sender
            .send(StateCommand::TouchSession {
                context_key: context_key.to_string(),
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::Part;

    #[tokio::test]
    async fn test_sent_messages_context_tracking() {
        let state = StateManager::new();

        state
            .insert_sent_message(
                100,
                "group_chat_1".to_string(),
                "Prompt 1".to_string(),
                "Response 1".to_string(),
            )
            .await;

        state
            .insert_sent_message(
                200,
                "group_chat_2".to_string(),
                "Prompt 2".to_string(),
                "Response 2".to_string(),
            )
            .await;

        let msg1 = state.get_sent_message(100).await.unwrap();
        assert_eq!(msg1.0, "group_chat_1");
        assert_eq!(msg1.1, "Prompt 1");
        assert_eq!(msg1.2, "Response 1");

        let msg2 = state.get_sent_message(200).await.unwrap();
        assert_eq!(msg2.0, "group_chat_2");
        assert_eq!(msg2.1, "Prompt 2");
        assert_eq!(msg2.2, "Response 2");

        let non_existent = state.get_sent_message(999).await;
        assert!(non_existent.is_none());
    }

    #[tokio::test]
    async fn test_search_sources_tracking() {
        let state = StateManager::new();
        assert!(state.get_last_search_sources("chat_1").await.is_none());

        let sources = vec![
            "• [Article 1](https://example.com/1)".to_string(),
            "• [Article 2](https://example.com/2)".to_string(),
        ];
        state
            .set_last_search_sources("chat_1", sources.clone())
            .await;

        let retrieved = state.get_last_search_sources("chat_1").await.unwrap();
        assert_eq!(retrieved, sources);
        assert!(state.get_last_search_sources("chat_2").await.is_none());
    }

    #[tokio::test]
    async fn test_session_reset_on_timeout() {
        // Short timeout for fast testing
        let state = StateManager::with_session_timeout(Duration::from_millis(50));
        let user_content = Content {
            role: "user".to_string(),
            parts: vec![Part::text("Hello")],
        };
        state.add_user_message("user_123", user_content).await;

        let model_content = Content {
            role: "model".to_string(),
            parts: vec![Part::text("Hi there!")],
        };
        state.add_model_message("user_123", model_content).await;

        assert_eq!(state.get_history_len("user_123").await, 2);

        // Wait beyond session timeout
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Querying history snapshot or len should detect expiry and reset
        assert_eq!(state.get_history_len("user_123").await, 0);
        let snapshot = state.get_history_snapshot("user_123").await;
        assert!(snapshot.is_empty());
    }

    #[tokio::test]
    async fn test_session_reset_on_new_user_message() {
        let state = StateManager::with_session_timeout(Duration::from_millis(50));

        let msg1 = Content {
            role: "user".to_string(),
            parts: vec![Part::text("Message 1")],
        };
        state.add_user_message("user_abc", msg1).await;
        assert_eq!(state.get_history_len("user_abc").await, 1);

        // Wait beyond timeout
        tokio::time::sleep(Duration::from_millis(80)).await;

        // New message arrives after timeout
        let msg2 = Content {
            role: "user".to_string(),
            parts: vec![Part::text("Message 2")],
        };
        state.add_user_message("user_abc", msg2).await;

        // Previous session was cleared; only message 2 is present
        let snapshot = state.get_history_snapshot("user_abc").await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].parts[0].text.as_deref(), Some("Message 2"));
    }

    #[tokio::test]
    async fn test_session_not_reset_within_timeout() {
        let state = StateManager::with_session_timeout(Duration::from_millis(200));

        let msg1 = Content {
            role: "user".to_string(),
            parts: vec![Part::text("Message 1")],
        };
        state.add_user_message("user_def", msg1).await;

        // Sleep within timeout window
        tokio::time::sleep(Duration::from_millis(60)).await;

        let model_msg = Content {
            role: "model".to_string(),
            parts: vec![Part::text("Response 1")],
        };
        state.add_model_message("user_def", model_msg).await;

        // Sleep again within refreshed window
        tokio::time::sleep(Duration::from_millis(60)).await;

        let msg2 = Content {
            role: "user".to_string(),
            parts: vec![Part::text("Message 2")],
        };
        state.add_user_message("user_def", msg2).await;

        // All 3 messages should be preserved
        let snapshot = state.get_history_snapshot("user_def").await;
        assert_eq!(snapshot.len(), 3);
    }

    #[tokio::test]
    async fn test_cleanup_expired_sessions() {
        let state = StateManager::with_session_timeout(Duration::from_millis(50));

        state
            .add_user_message(
                "chat_x",
                Content {
                    role: "user".to_string(),
                    parts: vec![Part::text("Hello X")],
                },
            )
            .await;

        state
            .add_user_message(
                "chat_y",
                Content {
                    role: "user".to_string(),
                    parts: vec![Part::text("Hello Y")],
                },
            )
            .await;

        tokio::time::sleep(Duration::from_millis(80)).await;

        let cleaned = state.cleanup_expired_sessions().await;
        assert_eq!(cleaned, 2);
        assert_eq!(state.get_history_len("chat_x").await, 0);
        assert_eq!(state.get_history_len("chat_y").await, 0);
    }

    #[tokio::test]
    async fn test_session_reset_timer_disabled_when_zero() {
        let state = StateManager::with_session_timeout(Duration::ZERO);

        state
            .add_user_message(
                "chat_perm",
                Content {
                    role: "user".to_string(),
                    parts: vec![Part::text("Always here")],
                },
            )
            .await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should not be cleared
        assert_eq!(state.get_history_len("chat_perm").await, 1);
        let cleaned = state.cleanup_expired_sessions().await;
        assert_eq!(cleaned, 0);
    }

    #[tokio::test]
    async fn test_reset_session_clears_search_sources() {
        let state = StateManager::new();

        state
            .set_last_search_sources("chat_src", vec!["Source A".to_string()])
            .await;
        state
            .add_user_message(
                "chat_src",
                Content {
                    role: "user".to_string(),
                    parts: vec![Part::text("Search query")],
                },
            )
            .await;

        assert!(state.get_last_search_sources("chat_src").await.is_some());
        state.reset_session("chat_src").await;

        assert!(state.get_last_search_sources("chat_src").await.is_none());
        assert_eq!(state.get_history_len("chat_src").await, 0);
    }
}
