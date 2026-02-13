use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use log::{info, error, warn};
use rand::RngExt; // For random_bool

use crate::ai::{self, VertexClient, Content, Part, memory::{Memory, ProfileManager}};
use crate::signal::{SignalClient, Envelope};
use std::time::SystemTime;

// Type aliases for cleaner signatures
type DestinationInfo = (String, Option<String>); // (source, group_id)
// Sequencer map stores a sender to a task that processes responses sequentially for a context
type SequencerMap = HashMap<
    String,
    mpsc::UnboundedSender<(DestinationInfo, oneshot::Receiver<BotResponse>)>
>;

#[derive(Debug)]
enum BotResponse {
    Text(String),
    Image(String, String), // Filename, Caption
    Error(String),
}

#[derive(Clone)]
pub struct SessionManager {
    signal_client: Arc<Mutex<SignalClient>>,
    ai_client: VertexClient,
    history: Arc<Mutex<HashMap<String, VecDeque<Content>>>>,
    sequencers: Arc<Mutex<SequencerMap>>,
    model_preferences: Arc<Mutex<HashMap<String, String>>>,
    bot_number: String,
    memory: Memory,
    profile_manager: ProfileManager,
    sent_messages: Arc<Mutex<HashMap<u64, (String, String)>>>, // Timestamp -> (Prompt, Response)
}

impl SessionManager {
    pub fn new(signal_client: Arc<Mutex<SignalClient>>, ai_client: VertexClient, bot_number: String) -> Self {
        Self {
            signal_client,
            ai_client,
            history: Arc::new(Mutex::new(HashMap::new())),
            sequencers: Arc::new(Mutex::new(HashMap::new())),
            model_preferences: Arc::new(Mutex::new(HashMap::new())),
            bot_number,
            memory: Memory::new("data/learned_behaviors.json"),
            profile_manager: ProfileManager::new("data/profiles"),
            sent_messages: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn handle_message(&self, envelope: Envelope) {
        let source = envelope.source.clone();
        let timestamp = envelope.timestamp;

        if let Some(data) = envelope.data_message {
            if let Some(text) = data.message {
                let is_group = data.group_info.is_some();
                let group_id = data.group_info.as_ref().map(|g| g.group_id.clone());

                // Determine Context Key (Group ID or Sender)
                let context_key = group_id.clone().unwrap_or_else(|| source.clone());

                // 1. Add User Message to History
                let user_content = Content {
                    role: "user".to_string(),
                    parts: vec![Part { text: Some(text.clone()) }],
                };
                {
                    let mut history_guard = self.history.lock().await;
                    let chat_history = history_guard.entry(context_key.clone()).or_insert_with(VecDeque::new);
                    chat_history.push_back(user_content);
                    if chat_history.len() > 20 { chat_history.pop_front(); }
                }

                let text_lower = text.trim().to_lowercase();

                // 2. Check for Commands
                if text_lower == "/reset" {
                    self.handle_reset(&context_key, &source, group_id.as_deref()).await;
                    return;
                }
                if text_lower == "/help" {
                    self.handle_help(&source, group_id.as_deref()).await;
                    return;
                }
                if text_lower.starts_with("/model") {
                    self.handle_model(&context_key, &source, group_id.as_deref(), &text).await;
                    return;
                }

                // 3. Determine if we should reply
                let is_quote_reply = if let Some(quote) = &data.quote {
                    quote.author == self.bot_number
                } else {
                    false
                };

                let (should_reply, prompt) = if is_group {
                    if text_lower.starts_with("piotr") || text_lower.starts_with("hey piotr") || is_quote_reply {
                        (true, text.clone())
                    } else {
                        // Random joke logic
                        let mut rng = rand::rng();
                        if rng.random_bool(0.02) {
                            (true, "Tell me a short, clean joke.".to_string())
                        } else {
                            (false, String::new())
                        }
                    }
                } else {
                    (true, text.clone())
                };

                if should_reply {
                    let profile_key = envelope.source_number.clone().unwrap_or(source.clone());
                    info!("Processing prompt from {}: {}", source, prompt);
                    self.process_ai_request(source, group_id, context_key, prompt, timestamp, profile_key).await;
                } else {
                    info!("Ignoring message from {}: {} (No trigger)", source, text);
                }
            } else if let Some(reaction) = data.reaction {
                // Handle Reaction
                if reaction.target_author == self.bot_number {
                     // Check if we have the message context
                     let sent_guard = self.sent_messages.lock().await;
                     if let Some((prompt, response)) = sent_guard.get(&reaction.target_sent_timestamp) {
                         let prompt_clone = prompt.clone();
                         let response_clone = response.clone();
                         let emoji_clone = reaction.emoji.clone();
                         let ai_client_clone = self.ai_client.clone();
                         let memory_clone = self.memory.clone();

                         // Spawn analysis task
                         tokio::spawn(async move {
                             info!("Analyzing reaction {} for prompt: {}", emoji_clone, prompt_clone);
                             match ai_client_clone.analyze_reaction(&prompt_clone, &response_clone, &emoji_clone).await {
                                 Ok(analysis) => {
                                     info!("Reaction Analysis: {:?}", analysis);
                                     if let Err(e) = memory_clone.add_interaction(prompt_clone, response_clone, analysis).await {
                                         error!("Failed to save interaction: {:?}", e);
                                     }
                                 },
                                 Err(e) => {
                                     error!("Failed to analyze reaction: {:?}", e);
                                 }
                             }
                         });
                     } else {
                         warn!("Received reaction for unknown message timestamp: {}", reaction.target_sent_timestamp);
                     }
                }
            }
        }
    }

    async fn handle_reset(&self, context_key: &str, reply_source: &str, reply_group_id: Option<&str>) {
        {
            let mut history_guard = self.history.lock().await;
            history_guard.remove(context_key);
        }
        let mut sc = self.signal_client.lock().await;
        let _ = sc.send_message(reply_source, reply_group_id, "Conversation history cleared.", None).await;
    }

    async fn handle_help(&self, reply_source: &str, reply_group_id: Option<&str>) {
         let help_msg = "I am Piotr. I can chat, generate images, and search the web.\n\n\
                        Commands:\n\
                        /reset - Clear our conversation history\n\
                        /model [list|auto|<name>] - Select AI model\n\
                        /help - Show this message\n\n\
                        Triggers:\n\
                        - Mention 'Piotr' or reply to me in groups.\n\
                        - DM me directly.\n\
                        - Ask for 'image', 'draw', 'sketch' for images.";
        let mut sc = self.signal_client.lock().await;
        let _ = sc.send_message(reply_source, reply_group_id, help_msg, None).await;
    }

    async fn handle_model(&self, context_key: &str, reply_source: &str, reply_group_id: Option<&str>, command_text: &str) {
        let parts: Vec<&str> = command_text.split_whitespace().collect();
        let arg = parts.get(1).map(|s| s.to_lowercase());

        let response = match arg.as_deref() {
            Some("list") => {
                "Available Models:\n\
                 - auto (Default/Smart Intent)\n\
                 - gemini-3-flash-preview (Fast)\n\
                 - gemini-3-pro-preview (Smart)\n\
                 - imagen-3.0-generate-001 (Images)".to_string()
            },
            Some("auto") => {
                let mut prefs = self.model_preferences.lock().await;
                prefs.remove(context_key);
                "Model set to AUTO (I will decide best model based on intent).".to_string()
            },
            Some(model) => {
                // Basic validation could be added here, but open-ended is fine for now
                let mut prefs = self.model_preferences.lock().await;
                prefs.insert(context_key.to_string(), model.to_string());
                format!("Model set to: {}. I will use this for all text responses.", model)
            },
            None => {
                 let prefs = self.model_preferences.lock().await;
                 let current = prefs.get(context_key).map(|s| s.as_str()).unwrap_or("auto");
                 format!("Current model: {}. Use '/model list' to see options.", current)
            }
        };

        let mut sc = self.signal_client.lock().await;
        let _ = sc.send_message(reply_source, reply_group_id, &response, None).await;
    }

    async fn process_ai_request(&self, source: String, group_id: Option<String>, context_key: String, prompt: String, timestamp: u64, profile_key: String) {
        // Get or Create Sequencer
        let sequencer_tx = self.get_sequencer_tx(context_key.clone()).await;

        // Create Result Channel
        let (worker_tx, worker_rx) = oneshot::channel::<BotResponse>();

        // Send Ticket to Sequencer (Preserves Order)
        let dest_info = (source.clone(), group_id.clone());
        if let Err(e) = sequencer_tx.send((dest_info, worker_rx)) {
            error!("Failed to send to sequencer: {}", e);
            return;
        }

        // Check for Model Preference
        let model_override = {
            let prefs = self.model_preferences.lock().await;
            prefs.get(&context_key).cloned()
        };

        // Spawn Worker Task
        let self_clone = self.clone();
        tokio::spawn(async move {
            // Send Read Receipt
            {
                let mut sc = self_clone.signal_client.lock().await;
                if let Err(e) = sc.send_receipt(&source, timestamp).await {
                    warn!("Failed to send read receipt: {:?}", e);
                }
            }

            // Start Typing
            {
                let mut sc = self_clone.signal_client.lock().await;
                let _ = sc.send_typing(&source, group_id.as_deref()).await;
            }

            let response = if let Some(model) = model_override {
                // If model is overridden, skip intent classification
                info!("Using override model: {}", model);
                self_clone.generate_text_response("OVERRIDE", &context_key, &profile_key, Some(model)).await
            } else {
                // Intent Classification (Auto Mode)
                let intent = match self_clone.ai_client.classify_intent(&prompt).await {
                    Ok(i) => i,
                    Err(e) => {
                        error!("Intent classification failed: {:?}", e);
                        "FLASH".to_string()
                    }
                };
                info!("Classified intent: {}", intent);

                if intent.starts_with("IMAGE") {
                    self_clone.generate_image_response(&intent, &prompt).await
                } else {
                    self_clone.generate_text_response(&intent, &context_key, &profile_key, None).await
                }
            };

            // Trigger Profile Update
            if let BotResponse::Text(ref text_response) = response {
                 let prompt_clone = prompt.clone();
                 let text_clone = text_response.clone();
                 let profile_key_clone = profile_key.clone();
                 let profile_manager = self_clone.profile_manager.clone();
                 let ai_client = self_clone.ai_client.clone();

                 tokio::spawn(async move {
                     if let Ok(current_profile) = profile_manager.get_profile(&profile_key_clone) {
                         let history_str = format!("User: {}\nBot: {}", prompt_clone, text_clone);
                         match ai_client.analyze_profile_update(&current_profile, &history_str).await {
                             Ok(updated_profile) => {
                                 if let Err(e) = profile_manager.save_profile(&updated_profile) {
                                     error!("Failed to save profile for {}: {:?}", profile_key_clone, e);
                                 } else {
                                     info!("Updated profile for {}", profile_key_clone);
                                 }
                             },
                             Err(e) => error!("Failed to analyze profile update: {:?}", e)
                         }
                     }
                 });
            }

            // Send result to Sequencer
            let _ = worker_tx.send(response);
        });
    }

    async fn generate_image_response(&self, intent: &str, prompt: &str) -> BotResponse {
        let model = if intent == "IMAGE_4" { "imagen-4.0-generate-001" } else { "imagen-3.0-generate-001" };
        info!("Attempting to generate image with model: {} for prompt: {}", model, prompt);

        match self.ai_client.generate_image(prompt, model).await {
            Ok(image_bytes) => {
                info!("Image generation successful. Bytes: {}", image_bytes.len());
                let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                let filename = format!("/tmp/piotr_img_{}.png", timestamp);
                if let Err(e) = std::fs::write(&filename, image_bytes) {
                    error!("Failed to write image to temp file: {:?}", e);
                    BotResponse::Error("I tried to draw something but my pencil broke (write error).".to_string())
                } else {
                    BotResponse::Image(filename, "Here is your image.".to_string())
                }
            },
            Err(e) => {
                error!("Image generation failed (LOGGED ERROR): {:?}", e);
                BotResponse::Error(format!("I could not generate that image with {}. I am sorry.", model))
            }
        }
    }

    async fn generate_text_response(&self, intent: &str, context_key: &str, profile_key: &str, override_model: Option<String>) -> BotResponse {
        let (model_id, use_search) = if let Some(ref m) = override_model {
             (m.clone(), false) // Disable search by default for overrides
        } else if intent == "SEARCH" {
            ("gemini-3-flash-preview".to_string(), true)
        } else if intent == "PRO" {
            ("gemini-3-pro-preview".to_string(), false)
        } else {
            ("gemini-3-flash-preview".to_string(), false)
        };

        // Clone history to Vec for API (Snapshot)
        let history_vec: Vec<Content> = {
            let history_guard = self.history.lock().await;
            if let Some(hist) = history_guard.get(context_key) {
                hist.iter().cloned().collect()
            } else {
                Vec::new()
            }
        };

        // Inject Learned Examples if available
        let mut final_history = Vec::new();

        // 1. Inject User Profile
        if let Ok(profile) = self.profile_manager.get_profile(profile_key) {
            let mut profile_context = format!("User Profile for {}:\n", profile_key);
            if let Some(name) = &profile.name {
                profile_context.push_str(&format!("Name: {}\n", name));
            }
            profile_context.push_str(&format!("Personality: {}\n", profile.personality_summary));
            profile_context.push_str(&format!("Style: {}\n", profile.interaction_style));
            if !profile.topics_of_interest.is_empty() {
                profile_context.push_str(&format!("Interests: {}\n", profile.topics_of_interest.join(", ")));
            }
            profile_context.push_str("\nUse this info to personalize your response. If you know their name, use it naturally.");

            final_history.push(Content {
                role: "user".to_string(),
                parts: vec![Part { text: Some(format!("SYSTEM NOTE: {}", profile_context)) }]
            });
            final_history.push(Content {
                role: "model".to_string(),
                parts: vec![Part { text: Some("Understood. I will personalize my response based on this profile.".to_string()) }]
            });
        }

        if !override_model.is_some() {
             // Retrieve relevant examples (simple latest/best for now)
             let examples = self.memory.get_relevant_examples("", 3).await;
             if !examples.is_empty() {
                 let mut examples_text = String::from("Here are some examples of your best past responses that people liked:\n");
                 for ex in examples {
                     examples_text.push_str(&format!("User: {}\nYou: {}\n---\n", ex.prompt, ex.response));
                 }
                 final_history.push(Content {
                     role: "user".to_string(),
                     parts: vec![Part { text: Some(examples_text) }]
                 });
                 final_history.push(Content {
                     role: "model".to_string(),
                     parts: vec![Part { text: Some("Understood. I will try to be as witty and helpful as those examples.".to_string()) }]
                 });
             }
        }
        final_history.extend(history_vec);

        match self.ai_client.generate_content(final_history, &model_id, use_search).await {
            Ok(text) => {
                info!("AI Response: {}", text);
                BotResponse::Text(text)
            },
            Err(e) => {
                error!("AI Error: {:?}", e);
                BotResponse::Error("I tried to think but my brain returned 404. (AI Error - check logs)".to_string())
            }
        }
    }

    async fn get_sequencer_tx(&self, context_key: String) -> mpsc::UnboundedSender<(DestinationInfo, oneshot::Receiver<BotResponse>)> {
        let mut seq_map = self.sequencers.lock().await;
        if let Some(tx) = seq_map.get(&context_key) {
            tx.clone()
        } else {
            let (tx, mut rx) = mpsc::unbounded_channel::<(DestinationInfo, oneshot::Receiver<BotResponse>)>();
            seq_map.insert(context_key.clone(), tx.clone());

            // Spawn Sequencer Task for this context
            let history_seq = self.history.clone();
            let signal_client_seq = self.signal_client.clone();
            let sent_messages_seq = self.sent_messages.clone();
            let context_key_seq = context_key.clone();

            tokio::spawn(async move {
                while let Some((dest_info, result_rx)) = rx.recv().await {
                    let (reply_source, reply_group_id) = dest_info;

                    // Wait for result from worker
                    if let Ok(response) = result_rx.await {
                        match response {
                            BotResponse::Text(text) => {
                                // Update History with Model Response
                                let model_content = Content {
                                    role: "model".to_string(),
                                    parts: vec![Part { text: Some(text.clone()) }],
                                };
                                {
                                    let mut history_guard = history_seq.lock().await;
                                    let hist = history_guard.entry(context_key_seq.clone()).or_insert_with(VecDeque::new);
                                    hist.push_back(model_content.clone());
                                    if hist.len() > 20 { hist.pop_front(); }

                                    // Retrieve the LAST user prompt to store in sent_messages
                                    // This is a bit tricky because we just pushed the response.
                                    // The user prompt is the one before.
                                    if hist.len() >= 2 {
                                        if let Some(last_user) = hist.get(hist.len() - 2) {
                                             if last_user.role == "user" {
                                                 if let Some(user_text) = last_user.parts.first().and_then(|p| p.text.clone()) {
                                                     // We have (User Prompt, Bot Response)
                                                     // We need the timestamp of the *response* we are about to send.
                                                     // Signal sends timestamps in ms.
                                                     let now_ts = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                                                      {
                                                          let mut sent_guard = sent_messages_seq.lock().await;
                                                          sent_guard.insert(now_ts, (user_text, text.clone()));
                                                          // Cleanup old sent messages (optional, skipping for brevity but good practice)
                                                      }
                                                 }
                                             }
                                        }
                                    }
                                }

                                // Send Split Messages
                                let chunks = textwrap::wrap(&text, 240);
                                for (i, chunk) in chunks.iter().take(4).enumerate() {
                                    let mut sc = signal_client_seq.lock().await;
                                    if let Err(e) = sc.send_message(&reply_source, reply_group_id.as_deref(), &chunk, None).await {
                                        error!("Failed to send Signal response part {}: {:?}", i + 1, e);
                                    }
                                    tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
                                }
                            },
                            BotResponse::Image(filename, text) => {
                                let mut sc = signal_client_seq.lock().await;
                                if let Err(e) = sc.send_message(&reply_source, reply_group_id.as_deref(), &text, Some(&filename)).await {
                                    error!("Failed to send image: {:?}", e);
                                }
                                // Cleanup image after sending
                                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                                let _ = std::fs::remove_file(&filename);
                            },
                            BotResponse::Error(err_msg) => {
                                let mut sc = signal_client_seq.lock().await;
                                let _ = sc.send_message(&reply_source, reply_group_id.as_deref(), &err_msg, None).await;
                            },
                        }

                        // Stop typing
                        {
                            let mut sc = signal_client_seq.lock().await;
                            let _ = sc.stop_typing(&reply_source, reply_group_id.as_deref()).await;
                        }
                    }
                }
            });
            tx
        }
    }
}
