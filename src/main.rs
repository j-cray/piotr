use dotenv::dotenv;
use tracing::{info, error, Instrument};
use std::time::Duration;
use anyhow::Context;
use piotr::{ai, bot, db, signal, utils, config::AppConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();
    info!("Starting Signal Bot...");

    // Load Configuration
    let config = Arc::new(AppConfig::load().context("Failed to load configuration from config.json5 or environment variables")?);

    // Initialize Database
    let db = db::Database::new(&config.database.url).await?;
    db.run_migrations().await?;

    // Initialize Profile Manager
    let profile_manager = ai::memory::DbProfileManager::new(db.pool.clone(), &config.security.profile_encryption_key)?;

    // Migrate existing profiles (if any)
    if let Err(e) = profile_manager.migrate_json_profiles("data/profiles").await {
        tracing::warn!("Failed to migrate profiles: {:?}", e);
        // Continue anyway, maybe folder doesn't exist
    }

    // Initialize AI client
    let ai_client = ai::VertexClient::new(config.clone());

    // Initialize Signal service - Auto-detect linked number or use configured
    let signal_phone = match &config.signal.phone_number {
        Some(num) => num.clone(),
        None => {
            let accounts_path = std::path::PathBuf::from(&config.signal.data_path).join("data").join("accounts.json");
            let accounts_json = std::fs::read_to_string(&accounts_path)
                .context(format!("Failed to read {} - did you run the linking script?", accounts_path.display()))?;
            let accounts: serde_json::Value = serde_json::from_str(&accounts_json)
                .context("accounts.json is not valid JSON")?;
            accounts["accounts"]
                .get(0)
                .and_then(|v| v.get("number"))
                .and_then(|v| v.as_str())
                .context("Could not find accounts[0].number in accounts.json - check the file structure")?
                .to_string()
        }
    };

    let (signal_client, mut rx) = match signal::SignalClient::new(&signal_phone, &config.signal.data_path).await {
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
    let session_manager = bot::SessionManager::new(signal_client, ai_client, bot_number, profile_manager, config.clone());

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
