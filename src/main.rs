mod signal;
mod ai;
mod bot;
mod db;
mod utils;
mod state_manager;

use dotenv::dotenv;
use log::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    env_logger::init();

    info!("Starting Signal Bot...");

    // Initialize Database
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db = db::Database::new(&database_url).await?;
    db.run_migrations().await?;

    // Initialize Profile Manager
    let encryption_key = std::env::var("PROFILE_ENCRYPTION_KEY").expect("PROFILE_ENCRYPTION_KEY must be set");
    let profile_manager = ai::memory::DbProfileManager::new(db.pool.clone(), &encryption_key)?;

    // Migrate existing profiles (if any)
    if let Err(e) = profile_manager.migrate_json_profiles("data/profiles").await {
        log::warn!("Failed to migrate profiles: {:?}", e);
        // Continue anyway, maybe folder doesn't exist
    }

    // Initialize AI client
    let project_id = std::env::var("GCP_PROJECT_ID").unwrap_or_else(|_| "piotr-487123".to_string());
    let ai_client = ai::VertexClient::new(&project_id);

    // Initialize Signal service - Auto-detect linked number
    let accounts_json = std::fs::read_to_string("data/signal-cli/data/accounts.json")
        .expect("Failed to read accounts.json. Did you run the linking script?");
    let accounts: serde_json::Value = serde_json::from_str(&accounts_json).expect("Invalid accounts.json format");
    let signal_phone = accounts["accounts"][0]["number"].as_str().expect("Could not find number in accounts.json").to_string();

    let (signal_client, mut rx) = match signal::SignalClient::new(&signal_phone).await {
        Ok(res) => res,
        Err(e) => {
            log::error!("Failed to start SignalClient: {:?}", e);
            return Err(e);
        }
    };

    info!("Signal listener started. Waiting for messages...");

    // Initialize Session Manager
    // Reuse the phone number we got earlier for the signal client
    let bot_number = signal_phone.clone();
    let session_manager = bot::SessionManager::new(signal_client, ai_client, bot_number, profile_manager);

    // Event Loop
    while let Some(msg) = rx.recv().await {
        if let Some(source) = msg.envelope.as_ref().map(|e| &e.source) {
            info!("Received Signal Message from: {}", crate::utils::anonymize(source));
        } else {
            info!("Received Signal Message (unknown source)");
        }

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
