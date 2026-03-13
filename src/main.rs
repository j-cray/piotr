use dotenv::dotenv;
use tracing::{info, error, Instrument};
use std::time::Duration;
use anyhow::Context;
use piotr::{ai, bot, db, signal, utils};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    info!("Starting Signal Bot...");

    // Initialize Database
    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL environment variable not set")?;
    let db = db::Database::new(&database_url).await?;
    db.run_migrations().await?;

    // Initialize Profile Manager
    let encryption_key = std::env::var("PROFILE_ENCRYPTION_KEY")
        .context("PROFILE_ENCRYPTION_KEY environment variable not set")?;
    let profile_manager = ai::memory::DbProfileManager::new(db.pool.clone(), &encryption_key)?;

    // Migrate existing profiles (if any)
    if let Err(e) = profile_manager.migrate_json_profiles("data/profiles").await {
        tracing::warn!("Failed to migrate profiles: {:?}", e);
        // Continue anyway, maybe folder doesn't exist
    }

    // Initialize AI client
    let project_id = std::env::var("GCP_PROJECT_ID").context("GCP_PROJECT_ID must be set")?;
    let ai_client = ai::VertexClient::new(&project_id);

    // Initialize Signal service - Auto-detect linked number
    let accounts_json = std::fs::read_to_string("data/signal-cli/data/accounts.json")
        .context("Failed to read accounts.json - did you run the linking script?")?;
    let accounts: serde_json::Value = serde_json::from_str(&accounts_json)
        .context("accounts.json is not valid JSON")?;
    let signal_phone = accounts["accounts"]
        .get(0)
        .and_then(|v| v.get("number"))
        .and_then(|v| v.as_str())
        .context("Could not find accounts[0].number in accounts.json - check the file structure")?
        .to_string();

    let (signal_client, mut rx) = match signal::SignalClient::new(&signal_phone).await {
        Ok(res) => {
            info!("SignalClient initialized for {}", res.0.user_phone());
            res
        },
        Err(e) => {
            tracing::error!("Failed to start SignalClient: {:?}", e);
            return Err(e);
        }
    };

    info!("Signal listener started. Waiting for messages...");

    // Initialize Session Manager
    // Reuse the phone number we got earlier for the signal client
    let bot_number = signal_phone.clone();
    let session_manager = bot::SessionManager::new(signal_client, ai_client, bot_number, profile_manager);

    // Event Loop with Backpressure
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(100));

    while let Some(msg) = rx.recv().await {
        if let Some(source) = msg.envelope.as_ref().map(|e| &e.source) {
            info!("Received Signal Message from: {}", utils::anonymize(source));
        } else {
            info!("Received Signal Message (unknown source)");
        }

        if let Some(envelope) = msg.envelope {
             // Clone SessionManager for async handling (it's cheap, just Arcs)
             let sm = session_manager.clone();

             // Extract some info for the span
             let source_for_span = envelope.source.clone();
             let source_for_closure = envelope.source.clone();
             let ts = envelope.timestamp;

             // Acquire a permit before spawning. This blocks if there are 100 concurrent tasks already.
             let permit = match semaphore.clone().acquire_owned().await {
                 Ok(p) => p,
                 Err(e) => {
                     error!("Failed to acquire semaphore permit: {}", e);
                     continue;
                 }
             };

             let span = tracing::info_span!("handle_message", source = %utils::anonymize(&source_for_span), ts = ts);

             tokio::spawn(async move {
                 // The permit is held for the duration of this task, limiting concurrency.
                 let _permit = permit;

                 let result = tokio::time::timeout(
                     Duration::from_secs(60),
                     sm.handle_message(envelope)
                 ).await;

                 if let Err(_) = result {
                     error!(
                         "Message processing timed out after 60 seconds (Source: {}, TS: {})",
                         utils::anonymize(&source_for_closure),
                         ts
                     );
                 }
             }.instrument(span));
        }
    }
    Ok(())
}
