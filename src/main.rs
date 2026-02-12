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
                    info!("Processing text from {}: {}", source, text);

                    // Send Read Receipt
                    if let Err(e) = signal_client.send_receipt(&source, envelope.timestamp).await {
                        log::warn!("Failed to send read receipt: {:?}", e);
                    }

                    // Start Typing Indicator
                    if let Err(e) = signal_client.send_typing(&source).await {
                        log::warn!("Failed to send typing indicator: {:?}", e);
                    }

                    // AI Generation
                    match ai_client.generate_content(&text).await {
                        Ok(response) => {
                            info!("AI Response: {}", response);

                            // Stop Typing Indicator (optional, but good hygiene)
                            let _ = signal_client.stop_typing(&source).await;

                            if let Err(e) = signal_client.send_message(&source, &response).await {
                                log::error!("Failed to send Signal response: {:?}", e);
                            }
                        }
                        Err(e) => {
                            log::error!("AI Error: {:?}", e);
                             let _ = signal_client.stop_typing(&source).await;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
