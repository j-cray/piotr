use tokio::sync::mpsc;
use tracing::{info, error, warn};
use rand::RngExt; // For random_bool

use crate::ai::{VertexClient, Content, Part, memory::{Memory, DbProfileManager}};
use crate::state_manager::{StateManager, DestinationInfo, ContextRequest};
use crate::signal::{SignalClient, Envelope};



#[derive(Debug)]
enum BotResponse {
    Text(String),
    Image(String, String), // Filename, Caption
    Error(String),
}

#[derive(Clone)]
pub struct SessionManager {
    signal_client: SignalClient,
    ai_client: VertexClient,
    state: StateManager,
    bot_number: String,
    memory: Memory,
    profile_manager: DbProfileManager,
    config: std::sync::Arc<crate::config::AppConfig>,
}

impl SessionManager {
    pub fn new(signal_client: SignalClient, ai_client: VertexClient, bot_number: String, profile_manager: DbProfileManager, config: std::sync::Arc<crate::config::AppConfig>) -> Self {
        Self {
            signal_client,
            ai_client,
            state: StateManager::new(),
            bot_number,
            memory: Memory::new("data/learned_behaviors.json"),
            profile_manager,
            config,
        }
    }

    pub async fn handle_message(&self, envelope: Envelope) {
        let source = envelope.source.clone();
        let timestamp = envelope.timestamp;

        if let Some(data) = envelope.data_message {
            info!("Data Message: {:?}", data);
            if let Some(text) = data.message {
                let is_group = data.group_info.is_some();
                let group_id = data.group_info.as_ref().map(|g| g.group_id.clone());

                // Determine Context Key (Group ID or Sender)
                let context_key = group_id.clone().unwrap_or_else(|| source.clone());

                let raw_display_name = envelope.source_name.clone()
                    .unwrap_or_else(|| envelope.source_number.clone().unwrap_or_else(|| source.clone()));
                
                // Sanitize display name to prevent prompt injection
                let display_name = sanitize_display_name(&raw_display_name);

                let history_text = if is_group {
                    format!("\"{}\": {}", display_name, text)
                } else {
                    text.clone()
                };

                // 1. Add User Message to History
                let user_content = Content {
                    role: "user".to_string(),
                    parts: vec![Part { text: Some(history_text) }],
                };
                self.state.add_user_message(&context_key, user_content).await;

                let text_lower = text.trim().to_lowercase();

                // 2. Determine if we should reply
                let is_quote_reply = if let Some(quote) = &data.quote {
                    quote.author == self.bot_number
                } else {
                    false
                };

                // Explicit triggers
                let bot_name_lower = self.config.bot.name.to_lowercase();
let mut is_explicit_interaction = !is_group || is_quote_reply || text_lower.contains(&format!("@{}", bot_name_lower)) || text_lower.contains(&bot_name_lower);

                // Also check native Signal mentions
                if let Some(mentions) = &data.mentions {
                    for m in mentions {
                        if m.number.as_deref() == Some(&self.bot_number) || m.name.as_deref().unwrap_or("").to_lowercase().contains(&bot_name_lower) {
                            is_explicit_interaction = true;
                            break;
                        }
                    }
                }

                // We always process messages; the LLM determines appropriateness to respond using intent classifier
                let profile_key = envelope.source_number.clone().unwrap_or(source.clone());
                let source_name = envelope.source_name.clone();

                // Prepend Thread Context if quoting someone else
                let mut final_prompt = text.clone();
                if let Some(quote) = &data.quote {
                    if quote.author != self.bot_number {
                        // User is replying to someone else, but triggered Piotr
                        let author_name = if quote.author == profile_key { "themselves" } else { &quote.author };
                        final_prompt = format!("(Replying to quote from {}): {}", author_name, text);
                    }
                }

                info!("Processing prompt from {}", crate::utils::anonymize(&source));
                let reply_address = envelope.source_uuid.clone().unwrap_or_else(|| source.clone());
                self.process_ai_request(reply_address, group_id, context_key, final_prompt, timestamp, profile_key, source_name, is_explicit_interaction).await;
            } else if let Some(reaction) = data.reaction {
                // Handle Reaction
                if reaction.target_author == self.bot_number {
                     // Check if we have the message context
                     if let Some((prompt, response)) = self.state.get_sent_message(reaction.target_sent_timestamp).await {
                         let prompt_clone = prompt.clone();
                         let response_clone = response.clone();
                         let emoji_clone = reaction.emoji.clone();
                         let ai_client_clone = self.ai_client.clone();
                         let memory_clone = self.memory.clone();

                         // Spawn analysis task
                         tokio::spawn(async move {
                             info!("Analyzing reaction {} for prompt", emoji_clone);
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

    async fn process_ai_request(&self, reply_address: String, group_id: Option<String>, context_key: String, prompt: String, timestamp: u64, profile_key: String, source_name: Option<String>, is_explicit_interaction: bool) {
        // Get or Create Sequencer
        let sequencer_tx = self.get_sequencer_tx(context_key.clone(), group_id.clone()).await;

        let request = ContextRequest {
            prompt,
            timestamp,
            profile_key,
            source_name,
            is_explicit_interaction,
        };

        // Send Ticket to Sequencer (Preserves Contextual Order)
        let dest_info = (reply_address, group_id);
        if let Err(e) = sequencer_tx.send((dest_info, request)) {
            error!("Failed to send request to sequencer: {}", e);
        }
    }

    async fn generate_image_response(&self, prompt: &str) -> BotResponse {
        let model = &self.config.ai.models.imagen;
        info!("Attempting to generate image with model: {} for prompt", model.name);

        match self.ai_client.generate_image(prompt, model).await {
            Ok(image_bytes) => {
                info!("Image generation successful. Bytes: {}", image_bytes.len());
                // Use a random suffix to avoid filename collisions even if clock is unusual
                let suffix = rand::rng().random::<u64>();
                let filename = format!("/tmp/piotr_img_{}.png", suffix);
                if let Err(e) = tokio::fs::write(&filename, image_bytes).await {
                    error!("Failed to write image to temp file: {:?}", e);
                    BotResponse::Error("I tried to draw something but my pencil broke (write error).".to_string())
                } else {
                    BotResponse::Image(filename, "Here is your image.".to_string())
                }
            },
            Err(e) => {
                error!("Image generation failed (LOGGED ERROR): {:?}", e);
                BotResponse::Error(format!("I could not generate that image with {}. I am sorry.", model.name))
            }
        }
    }

    async fn generate_text_response(&self, intent: &str, context_key: &str, profile_key: &str, source_name: Option<String>, override_model: Option<String>, group_id: Option<String>) -> BotResponse {
        let (model_config, use_search) = if let Some(ref m) = override_model {
             (crate::config::ModelSettings { name: m.clone(), ..Default::default() }, false) // Disable search by default for overrides
        } else if intent == "SEARCH" {
            (self.config.ai.models.classification.clone(), true)
        } else if intent == "PRO" {
            (self.config.ai.models.chat.clone(), false)
        } else {
            (self.config.ai.models.classification.clone(), false)
        };

        // Clone history to Vec for API (Snapshot)
        let history_vec: Vec<Content> = self.state.get_history_snapshot(context_key).await;

        // Inject Learned Examples if available
        let mut final_history = Vec::new();

        let target_len = self.config.bot.target_message_length_chars;
        let format_instructions = if intent == "PRO" {
            "FORMATTING INSTRUCTION: You are providing a detailed or complex response. You may write as much as necessary, using multiple paragraphs. Please format your response clearly.".to_string()
        } else {
            format!("FORMATTING INSTRUCTION: For casual conversation, aim to respond with exactly one paragraph of around {} characters. You may use subsequent paragraphs if necessary, but do so with reluctance, as each paragraph is a separate push notification. IMPORTANT: If the user explicitly asks for a long-form response (like an essay or detailed explanation), you may completely ignore this length limit and write as much as needed.", target_len)
        };
        
        final_history.push(Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(format!("SYSTEM NOTE: {}", format_instructions)) }]
        });
        final_history.push(Content {
            role: "model".to_string(),
            parts: vec![Part { text: Some("Understood. I will manage my length according to the context and user request.".to_string()) }]
        });

        // 1. Inject User Profile
        if let Ok(profile) = self.profile_manager.get_profile(profile_key, source_name.clone()).await {
            let display_name_for_profile = source_name.unwrap_or_else(|| profile_key.to_string());
            let mut profile_context = format!("User Profile for {}:\n", display_name_for_profile);
            if let Some(name) = &profile.name {
                profile_context.push_str(&format!("Name: {}\n", name));
            }
            if let Some(nickname) = &profile.nickname {
                profile_context.push_str(&format!("Nickname: {}\n", nickname));
            }
            profile_context.push_str(&format!("Personality: {}\n", profile.personality_summary));
            profile_context.push_str(&format!("Style: {}\n", profile.interaction_style));
            if !profile.topics_of_interest.is_empty() {
                profile_context.push_str(&format!("Interests: {}\n", profile.topics_of_interest.join(", ")));
            }
            profile_context.push_str("\nUse this info to personalize your response. If you know their name, use it naturally. Note: In group chats, this profile only applies to the user who just messaged you.");

            final_history.push(Content {
                role: "user".to_string(),
                parts: vec![Part { text: Some(format!("SYSTEM NOTE: {}", profile_context)) }]
            });
            final_history.push(Content {
            role: "model".to_string(),
            parts: vec![Part { text: Some("Understood. I will personalize my response based on this profile.".to_string()) }]
        });
    }

    // 2. Inject Group Profile (if applicable)
    if let Some(gid) = &group_id {
        if let Ok(group_profile) = self.profile_manager.get_group_profile(gid, None).await {
            let mut group_context = format!("Group Chat Profile for {}:\n", group_profile.group_name.as_deref().unwrap_or("this group"));
            group_context.push_str(&format!("Vibe: {}\n", group_profile.group_vibe));
            if !group_profile.inside_jokes.is_empty() {
                group_context.push_str(&format!("Inside Jokes/Memes: {}\n", group_profile.inside_jokes.join(", ")));
            }
            if !group_profile.common_topics.is_empty() {
                group_context.push_str(&format!("Common Topics: {}\n", group_profile.common_topics.join(", ")));
            }
            if !group_profile.important_memories.is_empty() {
                 group_context.push_str(&format!("Important Memories: {}\n", group_profile.important_memories.join(", ")));
            }
            group_context.push_str("\nUse this info to understand the context of the group chat. Reference inside jokes sparingly but accurately if the context fits.");

            final_history.push(Content {
                role: "user".to_string(),
                parts: vec![Part { text: Some(format!("SYSTEM NOTE: {}", group_context)) }]
            });
            final_history.push(Content {
                role: "model".to_string(),
                parts: vec![Part { text: Some("Understood. I am aware of the group's vibe and history.".to_string()) }]
            });
        }
    }

    if override_model.is_none() {
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

        match self.ai_client.generate_content(final_history, &model_config, use_search).await {
            Ok(text) => {
                info!("AI Response generated (len: {})", text.len());
                BotResponse::Text(text)
            },
            Err(e) => {
                error!("AI Error: {:?}", e);
                BotResponse::Error("I tried to think but my brain returned 404. (AI Error - check logs)".to_string())
            }
        }
    }

    async fn get_sequencer_tx(&self, context_key: String, group_id_context: Option<String>) -> mpsc::UnboundedSender<(DestinationInfo, ContextRequest)> {
        if let Some(tx) = self.state.get_sequencer_tx(&context_key).await {
            tx
        } else {
            let (tx, mut rx) = mpsc::unbounded_channel::<(DestinationInfo, ContextRequest)>();
            self.state.insert_sequencer_tx(&context_key, tx.clone()).await;

            // Spawn Sequencer Task for this context
            let signal_client_seq = self.signal_client.clone();
            let state_seq = self.state.clone();
            let context_key_seq = context_key.clone();

            let ai_client_seq = self.ai_client.clone();
            let profile_manager_seq = self.profile_manager.clone();
            let self_clone_seq = self.clone();

            tokio::spawn(async move {
                while let Some((dest_info, request)) = rx.recv().await {
                    let (reply_source, reply_group_id) = dest_info;
                    let group_id_spawn_clone = group_id_context.clone();

                    // Send Read Receipt
                    if let Err(e) = signal_client_seq.send_receipt(&reply_source, request.timestamp).await {
                        warn!("Failed to send read receipt: {:?}", e);
                    }

                    // Start Typing
                    let _ = signal_client_seq.send_typing(&reply_source, reply_group_id.as_deref()).await;

                    // Pre-Processing
                    let mut should_abort_generation = false;

                    let model_override = state_seq.get_model_preference(&context_key_seq).await;

                    let response = if let Some(model) = model_override {
                        info!("Using override model: {}", model);
                        let model_cfg = if model == self_clone_seq.config.ai.models.chat.name {
                            &self_clone_seq.config.ai.models.chat
                        } else if model == self_clone_seq.config.ai.models.imagen.name {
                            &self_clone_seq.config.ai.models.imagen
                        } else {
                            &self_clone_seq.config.ai.models.classification
                        };
                        self_clone_seq.manage_context_window(&context_key_seq, model_cfg).await;
                        self_clone_seq.generate_text_response("OVERRIDE", &context_key_seq, &request.profile_key, request.source_name.clone(), Some(model), group_id_context.clone()).await
                    } else {
                        // Intent Classification (Auto Mode)
                        let prompt_to_test = if !request.is_explicit_interaction {
                            format!("SYSTEM: Analyze this group chat message context. If it is appropriate for you to chime in unprompted to be helpful or funny, categorize the intent normally as FLASH, SEARCH, PRO, or IMAGE. Otherwise, reply IGNORE. User prompt: {}", request.prompt)
                        } else {
                            format!("SYSTEM: The user explicitly addressed you, so you MUST respond. Do NOT output IGNORE. Categorize intent normally as FLASH, SEARCH, PRO, or IMAGE. User prompt: {}", request.prompt)
                        };

                        let intent = match ai_client_seq.classify_intent(&prompt_to_test).await {
                            Ok(i) => i,
                            Err(e) => {
                                error!("Intent classification failed: {:?}", e);
                                "FLASH".to_string()
                            }
                        };
                        info!("Classified intent: {}", intent);

                        if intent.starts_with("IMAGE") {
                            self_clone_seq.generate_image_response(&request.prompt).await
                        } else if intent == "IGNORE" {
                            should_abort_generation = true;
                            // Returning an empty text avoids the signal send logic below
                            BotResponse::Error(String::new())
                        } else {
                            let model_cfg = if intent == "PRO" {
                                &self_clone_seq.config.ai.models.chat
                            } else {
                                &self_clone_seq.config.ai.models.classification
                            };
                            self_clone_seq.manage_context_window(&context_key_seq, model_cfg).await;
                            self_clone_seq.generate_text_response(&intent, &context_key_seq, &request.profile_key, request.source_name.clone(), None, group_id_context.clone()).await
                        }
                    };

                    // Only continue processing if we didn't deliberately abort (e.g., IGNORE intent)
                    if !should_abort_generation {
                        // Trigger Profile Update in background
                        if let BotResponse::Text(ref text_response) = response {
                             let prompt_clone = request.prompt.clone();
                             let text_clone = text_response.clone();
                             let profile_key_clone = request.profile_key.clone();
                             let source_name_clone = request.source_name.clone();
                             let pm = profile_manager_seq.clone();
                             let aic = ai_client_seq.clone();
                             let bot_name_inner = self_clone_seq.config.bot.name.clone();

                             tokio::spawn(async move {
                                 // 1. Update User Profile
                                 if let Ok(current_profile) = pm.get_profile(&profile_key_clone, source_name_clone.clone()).await {
                                     let history_str = format!("User: {}\nBot: {}", prompt_clone, text_clone);
                                     match aic.analyze_profile_update(&current_profile, &history_str).await {
                                         Ok(updated_profile) => {
                                             if let Err(e) = pm.save_profile(&updated_profile).await {
                                                 error!("Failed to save user profile for {}: {:?}", crate::utils::anonymize(&profile_key_clone), e);
                                             } else {
                                                 info!("Updated user profile for {}", crate::utils::anonymize(&profile_key_clone));
                                             }
                                         },
                                         Err(e) => error!("Failed to analyze user profile update: {:?}", e)
                                     }
                                 }

                                 // 2. Update Group Profile (if applicable)
                                 if let Some(gid) = &group_id_spawn_clone {
                                     if let Ok(current_group) = pm.get_group_profile(gid, None).await {
                                         // Provide a slightly richer history string for the group context
                                         let user_display = source_name_clone.unwrap_or_else(|| profile_key_clone.clone());
                                         let history_str = format!("{} (User): {}\n{} (Bot): {}", user_display, prompt_clone, bot_name_inner, text_clone);

                                         match aic.analyze_group_profile_update(&current_group, &history_str).await {
                                             Ok(updated_group) => {
                                                  if let Err(e) = pm.save_group_profile(&updated_group).await {
                                                      error!("Failed to save group profile for {}: {:?}", gid, e);
                                                  } else {
                                                      info!("Updated group profile for {}", gid);
                                                  }
                                             },
                                             Err(e) => error!("Failed to analyze group profile update: {:?}", e)
                                         }
                                     }
                                 }
                             });
                        }

                        match response {
                            BotResponse::Text(text) => {
                                // Update History with Model Response
                                let model_content = Content {
                                    role: "model".to_string(),
                                    parts: vec![Part { text: Some(text.clone()) }],
                                };
                                state_seq.add_model_message(&context_key_seq, model_content).await;



                                let paragraphs: Vec<&str> = if self_clone_seq.config.bot.enable_paragraph_splitting {
                                    text.split("\n\n").collect()
                                } else {
                                    vec![text.as_str()]
                                };

                                let is_document = text.contains("```") 
                                    || text.contains("\n# ") 
                                    || text.starts_with("# ")
                                    || text.contains("\n## ")
                                    || text.len() > 2000;

                                if !self_clone_seq.config.bot.enable_message_splitting || is_document {
                                    let trimmed = text.trim();
                                    if !trimmed.is_empty() {
                                        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                                        if let Err(e) = signal_client_seq.send_message(&reply_source, reply_group_id.as_deref(), trimmed, None).await {
                                            error!("Failed to send Signal response (long form): {:?}", e);
                                        } else {
                                            state_seq.insert_sent_message(ts, request.prompt.clone(), trimmed.to_string()).await;
                                        }
                                    }
                                } else {
                                    // Send Messages per paragraph cleanly
                                    let delay = tokio::time::Duration::from_millis(self_clone_seq.config.bot.message_delay_ms);
                                    let mut is_first = true;

                                    for paragraph in paragraphs {
                                        let trimmed = paragraph.trim();
                                        if trimmed.is_empty() { continue; }

                                        if !is_first { tokio::time::sleep(delay).await; }
                                        is_first = false;

                                        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                                        if let Err(e) = signal_client_seq.send_message(&reply_source, reply_group_id.as_deref(), trimmed, None).await {
                                            error!("Failed to send Signal response chunk: {:?}", e);
                                        } else {
                                            state_seq.insert_sent_message(ts, request.prompt.clone(), trimmed.to_string()).await;
                                        }
                                    }
                                }
                            },
                            BotResponse::Image(filename, text) => {
                                if let Err(e) = signal_client_seq.send_message(&reply_source, reply_group_id.as_deref(), &text, Some(&filename)).await {
                                    error!("Failed to send image: {:?}", e);
                                }
                                // Cleanup image after sending
                                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                                let _ = tokio::fs::remove_file(&filename).await;
                            },
                            BotResponse::Error(err_msg) => {
                                let _ = signal_client_seq.send_message(&reply_source, reply_group_id.as_deref(), &err_msg, None).await;
                            },
                        }

                        // Stop typing
                        let _ = signal_client_seq.stop_typing(&reply_source, reply_group_id.as_deref()).await;
                    }
                }
            });
            tx
        }
    }

    async fn manage_context_window(&self, context_key: &str, model_settings: &crate::config::ModelSettings) {
        let current_len = self.state.get_history_len(context_key).await;
        if current_len < 30 {
            return;
        }

        let history_snapshot = self.state.get_history_snapshot(context_key).await;

        let max_tokens = model_settings.max_input_tokens.unwrap_or(1_000_000);
        let token_limit = (max_tokens as f32 * 0.95) as i32;

        match self.ai_client.count_tokens(history_snapshot, model_settings).await {
           Ok(count) => {
               if count > token_limit {
                   info!("Context window for {} is full ({} tokens > {}). Pruning...", crate::utils::anonymize(context_key), count, token_limit);
                   let to_remove = Self::calculate_prune_amount(current_len, count, token_limit);
                   if current_len > to_remove {
                       self.state.prune_history(context_key, to_remove).await;
                   } else {
                       self.state.clear_history(context_key).await;
                   }
               }
           },
           Err(e) => error!("Failed to count tokens for {}: {:?}", crate::utils::anonymize(context_key), e)
        }
    }

    fn calculate_prune_amount(current_len: usize, current_tokens: i32, token_limit: i32) -> usize {
        if current_tokens <= token_limit {
            return 0;
        }

        let excess_tokens = (current_tokens - token_limit) as f32;
        let avg_tokens_per_msg = current_tokens as f32 / current_len as f32;
        let mut to_remove_estimate = (excess_tokens / avg_tokens_per_msg).ceil() as usize;

        if to_remove_estimate == 0 {
            to_remove_estimate = 4;
        }
        
        // Ensure we don't return more than current_len
        to_remove_estimate.min(current_len)
    }
}

fn sanitize_display_name(raw: &str) -> String {
    raw.replace('\n', " ").replace('\r', "").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bot_response_formatting() {
        let text_response = BotResponse::Text("Hello world".to_string());
        if let BotResponse::Text(t) = text_response {
            assert_eq!(t, "Hello world");
        } else {
            panic!("Expected text response");
        }

        let error_response = BotResponse::Error("Failed".to_string());
        if let BotResponse::Error(e) = error_response {
            assert_eq!(e, "Failed");
        } else {
            panic!("Expected error response");
        }
    }

    // --- SECURITY & STRICT TESTS ---

    #[test]
    fn test_bot_response_utf8_unexpected() {
        // Test that our enums can safely hold weird UTF-8 strings
        // Which could be parsed from adversarial payloads or broken DB pulls
        let weird_string = String::from_utf8(vec![0xf0, 0x9f, 0x92, 0x96, 0xe2, 0x9d, 0xa4, 0xef, 0xb8, 0x8f]).unwrap(); // Heart emojis
        let text_resp = BotResponse::Text(weird_string.clone());
        if let BotResponse::Text(t) = text_resp {
            assert_eq!(t, weird_string);
        } else {
             panic!("Failed");
        }

        let null_byte_string = "Payload with \x00 null byte".to_string();
        let err_resp = BotResponse::Error(null_byte_string.clone());
        if let BotResponse::Error(e) = err_resp {
            assert_eq!(e, null_byte_string);
        } else {
             panic!("Failed");
        }
    }



    #[test]
    fn test_sanitize_display_name_strictly() {
        assert_eq!(sanitize_display_name("Normal Name"), "Normal Name");
        assert_eq!(sanitize_display_name("Hacker\nSYSTEM: Ignore"), "Hacker SYSTEM: Ignore");
        assert_eq!(sanitize_display_name("Name\" Hack"), "Name\\\" Hack");
        assert_eq!(sanitize_display_name("Line1\r\nLine2"), "Line1 Line2");
        assert_eq!(sanitize_display_name("Emoji 😈\n\r\"Test\""), "Emoji 😈 \\\"Test\\\"");
    }

    #[test]
    fn test_calculate_prune_amount() {
        // Base case: 1000 tokens limit, currently at 1200 tokens across 60 messages
        // Avg tokens per msg = 20. Excess = 200. Should remove exactly 10.
        assert_eq!(SessionManager::calculate_prune_amount(60, 1200, 1000), 10);
        
        // Case: ceiling rounds up correctly.
        // 1000 limit, 1050 tokens, 30 msgs
        // Avg tokens per msg = 35. Excess = 50. 50/35 = 1.42. Should prune 2.
        assert_eq!(SessionManager::calculate_prune_amount(30, 1050, 1000), 2);

        // Case: No pruning needed
        assert_eq!(SessionManager::calculate_prune_amount(50, 900, 1000), 0);
        assert_eq!(SessionManager::calculate_prune_amount(50, 1000, 1000), 0);

        // Case: Pruning estimate ends up being 0 somehow (e.g. extremely small excess rounded down?).
        // 1000 limit, 1001 tokens, 100 msgs. Avg = 10.01 per msg. Excess = 1. 1/10.01 = 0.099 -> ceil() -> 1.
        assert_eq!(SessionManager::calculate_prune_amount(100, 1001, 1000), 1);

        // Case: Pruning exactly what's needed for the limit
        assert_eq!(SessionManager::calculate_prune_amount(5, 5000, 1000), 4);
    }
}
