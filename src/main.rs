mod signal;
mod ai;
mod bot;

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

    // Initialize Signal service
    let signal_phone = std::env::var("SIGNAL_PHONE_NUMBER").expect("SIGNAL_PHONE_NUMBER must be set in .env");

    let mut signal_client_raw = match signal::SignalClient::new(&signal_phone).await {
        Ok(client) => client,
        Err(e) => {
            log::error!("Failed to start SignalClient: {:?}", e);
            return Err(e);
        }
    };

    let mut rx = signal_client_raw.run_listener().await?;

    // Wrap in Arc<Mutex> for sharing with SessionManager
    let signal_client = Arc::new(Mutex::new(signal_client_raw));

    info!("Signal listener started. Waiting for messages...");

    // Initialize Session Manager
    let bot_number = std::env::var("SIGNAL_PHONE_NUMBER").unwrap_or_else(|_| "+12506417114".to_string());
    let session_manager = bot::SessionManager::new(signal_client.clone(), ai_client, bot_number);

    // Event Loop
    while let Some(msg) = rx.recv().await {
        info!("Received Signal Message: {:?}", msg);

        if let Some(envelope) = msg.envelope {
             // Clone SessionManager for async handling (it's cheap, just Arcs)
             let sm = session_manager.clone();
             tokio::spawn(async move {
                 sm.handle_message(envelope).await;
             });
        }
    }
    Ok(())
}
