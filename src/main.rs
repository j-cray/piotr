mod signal;
mod ai;

use dotenv::dotenv;
use log::info;

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
    match ai_client.generate_content(vec![test_content], "gemini-3-flash-preview").await {
        Ok(response) => info!("Received response: {}", response),
        Err(e) => info!("Error querying Vertex AI: {:?}", e),
    }

    // Initialize Signal service
    // Phone number must be configured via env (see .env)
    let signal_phone = std::env::var("SIGNAL_PHONE_NUMBER").expect("SIGNAL_PHONE_NUMBER must be set in .env");

    let mut signal_client = match signal::SignalClient::new(&signal_phone).await {
        Ok(client) => client,
        Err(e) => {
            log::error!("Failed to start SignalClient: {:?}", e);
            return Err(e);
        }
    };

    let mut rx = signal_client.run_listener().await?;

    info!("Signal listener started. Waiting for messages...");

    // Conversation History: GroupID/Source -> VecDeque of Content
    let mut history: std::collections::HashMap<String, std::collections::VecDeque<ai::Content>> = std::collections::HashMap::new();
    // Re-read signal phone for quote check
    let bot_number = std::env::var("SIGNAL_PHONE_NUMBER").unwrap_or_else(|_| "+12506417114".to_string());

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

                    // Add User Message to History
                    let user_content = ai::Content {
                        role: "user".to_string(),
                        parts: vec![ai::Part { text: Some(text.clone()) }],
                    };

                    let chat_history = history.entry(context_key.clone()).or_insert_with(|| std::collections::VecDeque::new());
                    chat_history.push_back(user_content);
                    if chat_history.len() > 20 { chat_history.pop_front(); }

                    let text_lower = text.trim().to_lowercase();

                    // Check for Quote Trigger
                    let is_quote_reply = if let Some(quote) = &data.quote {
                        quote.author == bot_number
                    } else {
                        false
                    };

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

                        let group_id = data.group_info.as_ref().map(|g| g.group_id.as_str());

                        // Send Read Receipt
                        if let Err(e) = signal_client.send_receipt(&source, envelope.timestamp).await {
                            log::warn!("Failed to send read receipt: {:?}", e);
                        }

                        // Start Typing Indicator
                        if let Err(e) = signal_client.send_typing(&source, group_id).await {
                            log::warn!("Failed to send typing indicator: {:?}", e);
                        }

                        // Intent Classification
                        let intent = match ai_client.classify_intent(&prompt).await {
                            Ok(i) => i,
                            Err(e) => {
                                log::error!("Intent classification failed: {:?}", e);
                                "FLASH".to_string()
                            }
                        };
                         info!("Classified intent: {}", intent);

                        if intent.starts_with("IMAGE") {
                             // Image Generation
                             let model = if intent == "IMAGE_4" { "imagen-4.0-generate-001" } else { "imagen-3.0-generate-001" };

                             match ai_client.generate_image(&prompt, model).await {
                                 Ok(image_bytes) => {
                                     // Save to temp file
                                     let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                                     let filename = format!("/tmp/piotr_img_{}.png", timestamp);
                                     if let Err(e) = std::fs::write(&filename, image_bytes) {
                                         log::error!("Failed to write image to temp file: {:?}", e);
                                         let _ = signal_client.send_message(&source, group_id, "I tried to draw something but my pencil broke (write error).", None).await;
                                     } else {
                                         // Send with attachment
                                         if let Err(e) = signal_client.send_message(&source, group_id, "Here is your image.", Some(&filename)).await {
                                             log::error!("Failed to send image: {:?}", e);
                                         }
                                         // Cleanup
                                         let _ = std::fs::remove_file(&filename);
                                     }
                                 },
                                 Err(e) => {
                                     log::error!("Image generation failed: {:?}", e);
                                     let _ = signal_client.send_message(&source, group_id, &format!("I could not generate that image with {}. I am sorry.", model), None).await;
                                 }
                             }
                             // Stop typing
                             let _ = signal_client.stop_typing(&source, group_id).await;

                        } else {
                            // Text Generation (Flash/Pro)
                            // Use gemini-3-flash-preview for FLASH, gemini-3-pro-preview for PRO
                            let model_id = if intent == "PRO" { "gemini-3-pro-preview" } else { "gemini-3-flash-preview" };

                            // Clone history to Vec for API
                            let history_vec: Vec<ai::Content> = chat_history.iter().cloned().collect();

                            match ai_client.generate_content(history_vec, model_id).await {
                                Ok(response) => {
                                    info!("AI Response: {}", response);

                                    // Add Model Response to History
                                    let model_content = ai::Content {
                                        role: "model".to_string(),
                                        parts: vec![ai::Part { text: Some(response.clone()) }],
                                    };
                                    if let Some(hist) = history.get_mut(&context_key) {
                                         hist.push_back(model_content);
                                         if hist.len() > 20 { hist.pop_front(); }
                                    }

                                    // Stop Typing Indicator
                                    let _ = signal_client.stop_typing(&source, group_id).await;

                                    // Split and send up to 4 messages
                                    let chunks = textwrap::wrap(&response, 240);
                                    for (i, chunk) in chunks.iter().take(4).enumerate() {
                                        if let Err(e) = signal_client.send_message(&source, group_id, &chunk, None).await {
                                            log::error!("Failed to send Signal response part {}: {:?}", i + 1, e);
                                        }
                                        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
                                    }
                                }
                                Err(e) => {
                                    log::error!("AI Error: {:?}", e);
                                     let _ = signal_client.stop_typing(&source, group_id).await;
                                     let _ = signal_client.send_message(&source, group_id, "I tried to think but my brain returned 404. (AI Error - check logs)", None).await;
                                }
                            }
                        }
                    } else {
                        info!("Ignoring message from {}: {} (No trigger)", source, text);
                    }
                }
            }
        }
    }

    Ok(())
}
