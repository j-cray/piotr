use crate::ai::Content;
use std::collections::{HashMap, VecDeque};
use tokio::sync::{mpsc, oneshot};

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
        prompt: String,
        response: String,
    },
    GetSentMessage {
        timestamp: u64,
        resp: oneshot::Sender<Option<(String, String)>>,
    },
}

// The internal state holding struct running in the background task
struct StateActor {
    history: HashMap<String, VecDeque<Content>>,
    history_order: VecDeque<String>, // Tracks access order for LRU eviction
    sequencers: SequencerMap,
    sequencers_order: VecDeque<String>, // Tracks access order for LRU eviction
    model_preferences: HashMap<String, String>,
    sent_messages: HashMap<u64, (String, String)>, // Timestamp -> (Prompt, Response)
    sent_messages_order: VecDeque<u64>,            // Insertion order for eviction
    receiver: mpsc::Receiver<StateCommand>,
}

const MAX_SENT_MESSAGES: usize = 10_000;
const MAX_SEQUENCERS: usize = 1_000;
const MAX_HISTORY_CONTEXTS: usize = 10_000;

impl StateActor {
    fn new(receiver: mpsc::Receiver<StateCommand>) -> Self {
        Self {
            history: HashMap::new(),
            history_order: VecDeque::new(),
            sequencers: HashMap::new(),
            sequencers_order: VecDeque::new(),
            model_preferences: HashMap::new(),
            sent_messages: HashMap::new(),
            sent_messages_order: VecDeque::new(),
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
        if self.history.len() >= MAX_HISTORY_CONTEXTS {
            if let Some(oldest) = self.history_order.pop_front() {
                self.history.remove(&oldest);
            }
        }
    }

    async fn run(mut self) {
        while let Some(cmd) = self.receiver.recv().await {
            match cmd {
                StateCommand::AddUserMessage {
                    context_key,
                    content,
                } => {
                    if !self.history.contains_key(&context_key) {
                        self.check_history_capacity();
                        self.history_order.push_back(context_key.clone());
                    } else {
                        self.touch_history(&context_key);
                    }
                    let chat_history = self
                        .history
                        .entry(context_key)
                        .or_insert_with(VecDeque::new);
                    chat_history.push_back(content);
                }
                StateCommand::AddModelMessage {
                    context_key,
                    content,
                } => {
                    if !self.history.contains_key(&context_key) {
                        self.check_history_capacity();
                        self.history_order.push_back(context_key.clone());
                    } else {
                        self.touch_history(&context_key);
                    }
                    let chat_history = self
                        .history
                        .entry(context_key)
                        .or_insert_with(VecDeque::new);
                    chat_history.push_back(content);
                }
                StateCommand::GetHistorySnapshot { context_key, resp } => {
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
                    let len = if self.history.contains_key(&context_key) {
                        self.touch_history(&context_key);
                        self.history.get(&context_key).unwrap().len()
                    } else {
                        0
                    };
                    let _ = resp.send(len);
                }
                StateCommand::ClearHistory { context_key } => {
                    self.history.remove(&context_key);
                    self.history_order.retain(|x| x != &context_key);
                }
                StateCommand::PruneHistory {
                    context_key,
                    num_messages,
                } => {
                    if self.history.contains_key(&context_key) {
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
                    {
                        if let Some(oldest) = self.sequencers_order.pop_front() {
                            self.sequencers.remove(&oldest);
                        }
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
                    prompt,
                    response,
                } => {
                    // Evict oldest entry if at capacity to prevent unbounded memory growth
                    if self.sent_messages.len() >= MAX_SENT_MESSAGES {
                        if let Some(oldest_ts) = self.sent_messages_order.pop_front() {
                            self.sent_messages.remove(&oldest_ts);
                        }
                    }
                    self.sent_messages.insert(timestamp, (prompt, response));
                    self.sent_messages_order.push_back(timestamp);
                }
                StateCommand::GetSentMessage { timestamp, resp } => {
                    let msg = self.sent_messages.get(&timestamp).cloned();
                    let _ = resp.send(msg);
                }
            }
        }
    }
}

// The external API Handle
#[derive(Clone)]
pub struct StateManager {
    sender: mpsc::Sender<StateCommand>,
}

impl StateManager {
    pub fn new() -> Self {
        // Create an unbounded channel or a bounded channel with a healthy buffer.
        // A bounded channel requires `await` or `try_send`, but usually we want memory bounds.
        // For simplicity and avoiding blocking threads heavily, we can use a large buffer.
        let (sender, receiver) = mpsc::channel(1000);
        let actor = StateActor::new(receiver);
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
    pub async fn insert_sent_message(&self, timestamp: u64, prompt: String, response: String) {
        let _ = self
            .sender
            .send(StateCommand::InsertSentMessage {
                timestamp,
                prompt,
                response,
            })
            .await;
    }

    pub async fn get_sent_message(&self, timestamp: u64) -> Option<(String, String)> {
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
}
