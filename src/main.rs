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
    match ai_client.generate_content("Hello! Are you working?").await {
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

    // Event Loop
    while let Some(msg) = rx.recv().await {
        info!("Received Signal Message: {:?}", msg);

        if let Some(envelope) = msg.envelope {
            let source = envelope.source.clone();
            if let Some(data) = envelope.data_message {
                if let Some(text) = data.message {
                    let is_group = data.group_info.is_some();
                    let text_lower = text.trim().to_lowercase(); // Define text_lower here for use in closure/logic

                    let (should_reply, prompt): (bool, String) = if is_group {
                        if text_lower.starts_with("piotr") || text_lower.starts_with("hey piotr") {
                            (true, text.clone())
                        } else {
                            // Random joke logic
                            use rand::RngExt; // Import RngExt for random_bool
                            let mut rng = rand::rng();
                            if rng.random_bool(0.02) {
                                // Actually, user requested "occasionally". Let's set it to 2%
                                // But for testing I might want it higher? sticking to 2%
                                (true, "Tell me a short, clean joke.".to_string())
                            } else {
                                (false, String::new())
                            }
                        }
                    } else {
                        (true, text.clone())
                    };

                    if should_reply {
                         info!("Processing prompt from {}: {}", source, prompt); // Use 'prompt' instead of 'text'

                        let group_id = data.group_info.as_ref().map(|g| g.group_id.as_str());

                        // Send Read Receipt (Always to the source/sender for now, or use group logic?
                        // signal-cli sendReceipt takes recipient. It might suffice to send to source.)
                        if let Err(e) = signal_client.send_receipt(&source, envelope.timestamp).await {
                            log::warn!("Failed to send read receipt: {:?}", e);
                        }

                        // Start Typing Indicator
                        if let Err(e) = signal_client.send_typing(&source, group_id).await {
                            log::warn!("Failed to send typing indicator: {:?}", e);
                        }

                        // AI Generation
                        match ai_client.generate_content(&prompt).await {
                            Ok(response) => {
                                info!("AI Response: {}", response);

                                // Stop Typing Indicator
                                let _ = signal_client.stop_typing(&source, group_id).await;

                                if let Err(e) = signal_client.send_message(&source, group_id, &response).await {
                                    log::error!("Failed to send Signal response: {:?}", e);
                                }
                            }
                            Err(e) => {
                                log::error!("AI Error: {:?}", e);
                                 let _ = signal_client.stop_typing(&source, group_id).await;
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
