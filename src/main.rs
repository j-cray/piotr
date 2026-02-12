mod signal;
mod ai;

use dotenv::dotenv;
use log::info;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    env_logger::init();

    info!("Starting Signal Bot...");

    // Initialize AI client
    let project_id = std::env::var("GCP_PROJECT_ID").unwrap_or_else(|_| "piotr-487123".to_string());
    let ai_client = ai::VertexClient::new(&project_id);

    info!("Sending test prompt to Vertex AI...");
    let test_content = ai::Content {
        role: "user".to_string(),
        parts: vec![ai::Part { text: Some("Hello! Are you working?".to_string()) }],
    };
    // Use Gemini 3 Flash for test
    match ai_client.generate_content(vec![test_content], "gemini-3-flash-preview", false).await {
        Ok(response) => info!("Received response: {}", response),
        Err(e) => info!("Error querying Vertex AI: {:?}", e),
    }

    // Initialize Signal service
    // Phone number must be configured via env (see .env)
    let signal_phone = std::env::var("SIGNAL_PHONE_NUMBER").expect("SIGNAL_PHONE_NUMBER must be set in .env");

    let mut signal_client_raw = match signal::SignalClient::new(&signal_phone).await {
        Ok(client) => client,
        Err(e) => {
            log::error!("Failed to start SignalClient: {:?}", e);
            return Err(e);
        }
    };

    let mut rx = signal_client_raw.run_listener().await?;

    // Wrap in Arc<Mutex> for sharing with typing task
    let signal_client = Arc::new(Mutex::new(signal_client_raw));

    info!("Signal listener started. Waiting for messages...");

    // Conversation History: GroupID/Source -> VecDeque of Content
    let history: Arc<Mutex<std::collections::HashMap<String, std::collections::VecDeque<ai::Content>>>> = Arc::new(Mutex::new(std::collections::HashMap::new()));
    // Re-read signal phone for quote check
    let bot_number = std::env::var("SIGNAL_PHONE_NUMBER").unwrap_or_else(|_| "+12506417114".to_string());

    // Sequencer Map: ContextKey -> mpsc::Sender<(DestinationInfo, oneshot::Receiver<BotResponse>)>
    // We use UnboundedSender for simplicity, as we don't expect massive spam.
    type DestinationInfo = (String, Option<String>); // (source, group_id)
    let sequencers: Arc<Mutex<std::collections::HashMap<String, tokio::sync::mpsc::UnboundedSender<(DestinationInfo, tokio::sync::oneshot::Receiver<BotResponse>)>>>> = Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Event Loop
    while let Some(msg) = rx.recv().await {
        info!("Received Signal Message: {:?}", msg);

        if let Some(envelope) = msg.envelope {
            let source = envelope.source.clone();
            if let Some(data) = envelope.data_message {
                if let Some(text) = data.message {
                    let is_group = data.group_info.is_some();

                    // Determine Context Key for History (Group ID or Sender)
                    let context_key = data.group_info.as_ref()
                        .map(|g| g.group_id.clone())
                        .unwrap_or_else(|| source.clone());

                    // Add User Message to History (Locking)
                    let user_content = ai::Content {
                        role: "user".to_string(),
                        parts: vec![ai::Part { text: Some(text.clone()) }],
                    };
                    {
                        let mut history_guard = history.lock().await;
                        let chat_history = history_guard.entry(context_key.clone()).or_insert_with(|| std::collections::VecDeque::new());
                        chat_history.push_back(user_content);
                        if chat_history.len() > 20 { chat_history.pop_front(); }
                    }

                    let text_lower = text.trim().to_lowercase();

                    // Check for Quote Trigger
                    let is_quote_reply = if let Some(quote) = &data.quote {
                        quote.author == bot_number
                    } else {
                        false
                    };

                    // Determine if we should reply
                    let (should_reply, prompt) = if is_group {
                        if text_lower.starts_with("piotr") || text_lower.starts_with("hey piotr") || is_quote_reply {
                            (true, text.clone())
                        } else {
                            // Random joke logic
                            use rand::RngExt; // Import RngExt for random_bool
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
                         info!("Processing prompt from {}: {}", source, prompt);

                         let group_id = data.group_info.as_ref().map(|g| g.group_id.clone());

                         // Get or Create Sequencer for this Context
                         let sequencer_tx = {
                             let mut seq_map = sequencers.lock().await;
                             if let Some(tx) = seq_map.get(&context_key) {
                                 tx.clone()
                             } else {
                                 let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(DestinationInfo, tokio::sync::oneshot::Receiver<BotResponse>)>();
                                 seq_map.insert(context_key.clone(), tx.clone());

                                 // Spawn Sequencer Task
                                 let history_seq = history.clone();
                                 let signal_client_seq = signal_client.clone();
                                 let context_key_seq = context_key.clone();

                                 let tx_clone = tx.clone(); // For return

                                 tokio::spawn(async move {
                                     while let Some((dest_info, result_rx)) = rx.recv().await {
                                         let (reply_source, reply_group_id) = dest_info;

                                         // Wait for result
                                         if let Ok(response) = result_rx.await {
                                             match response {
                                                 BotResponse::Text(text) => {
                                                     // Update History
                                                     let model_content = ai::Content {
                                                         role: "model".to_string(),
                                                         parts: vec![ai::Part { text: Some(text.clone()) }],
                                                     };
                                                     {
                                                         let mut history_guard = history_seq.lock().await;
                                                         if let Some(hist) = history_guard.get_mut(&context_key_seq) {
                                                              hist.push_back(model_content);
                                                              if hist.len() > 20 { hist.pop_front(); }
                                                         }
                                                     }

                                                     // Send Split Messages
                                                     let chunks = textwrap::wrap(&text, 240);
                                                     for (i, chunk) in chunks.iter().take(4).enumerate() {
                                                         let mut sc = signal_client_seq.lock().await;
                                                         if let Err(e) = sc.send_message(&reply_source, reply_group_id.as_deref(), &chunk, None).await {
                                                             log::error!("Failed to send Signal response part {}: {:?}", i + 1, e);
                                                         }
                                                         tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
                                                     }
                                                 },
                                                 BotResponse::Image(filename, text) => {
                                                     let mut sc = signal_client_seq.lock().await;
                                                     if let Err(e) = sc.send_message(&reply_source, reply_group_id.as_deref(), &text, Some(&filename)).await {
                                                         log::error!("Failed to send image: {:?}", e);
                                                     }
                                                     // Cleanup image after sending
                                                     tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                                                     let _ = std::fs::remove_file(&filename);
                                                 },
                                                 BotResponse::Error(err_msg) => {
                                                     let mut sc = signal_client_seq.lock().await;
                                                     let _ = sc.send_message(&reply_source, reply_group_id.as_deref(), &err_msg, None).await;
                                                 },
                                                 BotResponse::None => {} // Ignore
                                             }

                                             // Stop typing (Worker started it, Sequencer stops it after sending)
                                             {
                                                  let mut sc = signal_client_seq.lock().await;
                                                  let _ = sc.stop_typing(&reply_source, reply_group_id.as_deref()).await;
                                             }
                                         }
                                     }
                                 });
                                 tx_clone
                             }
                         };

                         // Create Result Channel
                         let (worker_tx, worker_rx) = tokio::sync::oneshot::channel::<BotResponse>();

                         // Send Ticket to Sequencer (Preserves Order)
                         let dest_info = (source.clone(), group_id.clone());
                         let _ = sequencer_tx.send((dest_info, worker_rx));

                         // Clone for Worker
                         let signal_client_task = signal_client.clone();
                         let ai_client_task = ai_client.clone();
                         let history_task = history.clone();
                         let source_task = source.clone();
                         let group_id_task = group_id.clone();
                         let context_key_task = context_key.clone();
                         let prompt_task: String = prompt.clone();
                         let timestamp_task = envelope.timestamp;

                         tokio::spawn(async move {
                             // Send Read Receipt
                             {
                                 let mut sc = signal_client_task.lock().await;
                                 if let Err(e) = sc.send_receipt(&source_task, timestamp_task).await {
                                     log::warn!("Failed to send read receipt: {:?}", e);
                                 }
                             }

                             // Start Typing (Worker manages start)
                             {
                                 let mut sc = signal_client_task.lock().await;
                                 let _ = sc.send_typing(&source_task, group_id_task.as_deref()).await;
                             }
                             // We don't need a loop for typing anymore if the Sequencer stops it accurately?
                             // A loop is better because generation can take time and intent classification might be fast.
                             // Let's keep a weak heartbeat task or just rely on the initial send?
                             // Signal typing indicators expire after ~10-15s.
                             // For now, simple single send.

                             // Intent Classification
                             let intent = match ai_client_task.classify_intent(&prompt_task).await {
                                 Ok(i) => i,
                                 Err(e) => {
                                     log::error!("Intent classification failed: {:?}", e);
                                     "FLASH".to_string()
                                 }
                             };
                             info!("Classified intent: {}", intent);

                             let response = if intent.starts_with("IMAGE") {
                                  // Image Generation
                                  let model = if intent == "IMAGE_4" { "imagen-4.0-generate-001" } else { "imagen-3.0-generate-001" };
                                  info!("Attempting to generate image with model: {} for prompt: {}", model, prompt_task);

                                  match ai_client_task.generate_image(&prompt_task, model).await {
                                      Ok(image_bytes) => {
                                          info!("Image generation successful. Bytes: {}", image_bytes.len());
                                          let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                                          let filename = format!("/tmp/piotr_img_{}.png", timestamp);
                                          if let Err(e) = std::fs::write(&filename, image_bytes) {
                                              log::error!("Failed to write image to temp file: {:?}", e);
                                              BotResponse::Error("I tried to draw something but my pencil broke (write error).".to_string())
                                          } else {
                                              BotResponse::Image(filename, "Here is your image.".to_string())
                                          }
                                      },
                                      Err(e) => {
                                          log::error!("Image generation failed (LOGGED ERROR): {:?}", e);
                                          BotResponse::Error(format!("I could not generate that image with {}. I am sorry.", model))
                                      }
                                  }
                             } else {
                                 // Text Generation (Flash/Pro/Search)
                                 let (model_id, use_search) = if intent == "SEARCH" {
                                     ("gemini-3-flash-preview", true)
                                 } else if intent == "PRO" {
                                     ("gemini-3-pro-preview", false)
                                 } else {
                                     ("gemini-3-flash-preview", false)
                                 };

                                 // Clone history to Vec for API (Snapshot)
                                 let history_vec: Vec<ai::Content> = {
                                     let history_guard = history_task.lock().await;
                                     if let Some(hist) = history_guard.get(&context_key_task) {
                                         hist.iter().cloned().collect()
                                     } else {
                                         Vec::new()
                                     }
                                 };

                                 match ai_client_task.generate_content(history_vec, model_id, use_search).await {
                                     Ok(text) => {
                                         info!("AI Response: {}", text);
                                         BotResponse::Text(text)
                                     },
                                     Err(e) => {
                                         log::error!("AI Error: {:?}", e);
                                         BotResponse::Error("I tried to think but my brain returned 404. (AI Error - check logs)".to_string())
                                     }
                                 }
                             };

                             // Send result to Sequencer
                             let _ = worker_tx.send(response);
                         });

                    } else {
                        info!("Ignoring message from {}: {} (No trigger)", source, text);
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
enum BotResponse {
    Text(String),
    Image(String, String), // Filename, Caption
    Error(String),
    None,
}
