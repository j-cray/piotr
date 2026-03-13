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
    process_message_stream(
        &mut rx,
        config.performance.max_concurrent_requests,
        config.performance.message_processing_timeout_secs,
        move |envelope| {
            let sm = session_manager.clone();
            async move {
                sm.handle_message(envelope).await
            }
        }
    ).await;

    Ok(())
}

pub async fn process_message_stream<F, Fut>(
    rx: &mut tokio::sync::mpsc::Receiver<piotr::signal::SignalMessage>,
    max_concurrent_requests: usize,
    timeout_secs: u64,
    handler: F,
) where
    F: Fn(piotr::signal::Envelope) -> Fut + Send + Sync + 'static + Clone,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrent_requests));

    while let Some(msg) = rx.recv().await {
        if let Some(source) = msg.envelope.as_ref().map(|e| &e.source) {
            info!("Received Signal Message from: {}", utils::anonymize(source));
        } else {
            info!("Received Signal Message (unknown source)");
        }

        if let Some(envelope) = msg.envelope {
            let source_for_span = envelope.source.clone();
            let source_for_closure = envelope.source.clone();
            let ts = envelope.timestamp;

            // Acquire a permit before spawning. This blocks if max concurrent tasks are running.
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to acquire semaphore permit: {}", e);
                    continue;
                }
            };

            let span = tracing::info_span!("handle_message", source = %utils::anonymize(&source_for_span), ts = ts);
            
            let handler_clone = handler.clone();
            
            tokio::spawn(async move {
                // The permit is held for the duration of this task, limiting concurrency.
                let _permit = permit;

                let result = tokio::time::timeout(
                    Duration::from_secs(timeout_secs),
                    handler_clone(envelope)
                ).await;

                if let Err(_) = result {
                    error!(
                        "Message processing timed out after {} seconds (Source: {}, TS: {})",
                        timeout_secs,
                        utils::anonymize(&source_for_closure),
                        ts
                    );
                }
            }.instrument(span));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piotr::signal::{SignalMessage, Envelope};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn test_process_message_stream_concurrency() {
        unsafe { std::env::set_var("ANONYMIZE_KEY", "test_key"); }
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let handler_active = active_tasks.clone();
        let handler_max = max_observed.clone();

        let handler = move |_env: Envelope| {
            let active = handler_active.clone();
            let max = handler_max.clone();
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                
                // Update max optionally
                let mut current_max = max.load(Ordering::SeqCst);
                while current > current_max {
                    if max.compare_exchange(current_max, current, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                        break;
                    }
                    current_max = max.load(Ordering::SeqCst);
                }

                tokio::time::sleep(Duration::from_millis(50)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            }
        };

        // Send 5 concurrent messages quickly
        for i in 0..5 {
            let env = Envelope {
                source: "test".to_string(),
                source_number: None,
                source_uuid: None,
                timestamp: i,
                source_name: None,
                data_message: None,
            };
            tx.send(SignalMessage { envelope: Some(env) }).await.unwrap();
        }
        
        // Drop tx so the stream finishes processing
        drop(tx);

        let max_concurrent = 2; // Test constraining to 2 things simultaneously out of the 5 requests
        process_message_stream(&mut rx, max_concurrent, 5, handler).await;

        // Ensure we actually processed them concurrently, but not more than max_concurrent
        assert_eq!(max_observed.load(Ordering::SeqCst), max_concurrent, "Did not properly restrict concurrency");
    }

    #[tokio::test]
    async fn test_process_message_stream_timeout() {
        unsafe { std::env::set_var("ANONYMIZE_KEY", "test_key"); }
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let completed = Arc::new(AtomicUsize::new(0));

        let handler_completed = completed.clone();
        let handler = move |_env: Envelope| {
            let comp = handler_completed.clone();
            async move {
                // Sleep for way longer than the timeout
                tokio::time::sleep(Duration::from_secs(5)).await;
                comp.fetch_add(1, Ordering::SeqCst);
            }
        };

        let env = Envelope {
            source: "test".to_string(),
            source_number: None,
            source_uuid: None,
            timestamp: 1,
            source_name: None,
            data_message: None,
        };
        tx.send(SignalMessage { envelope: Some(env) }).await.unwrap();
        drop(tx);

        // Run with 1 second timeout
        process_message_stream(&mut rx, 10, 1, handler).await;
        
        // Sleep a bit more to ensure background spawns had time to either finish or timeout
        tokio::time::sleep(Duration::from_secs(2)).await;

        // If the timeout worked, the handler task was aborted, so `completed` should still be 0
        assert_eq!(completed.load(Ordering::SeqCst), 0, "Handler should have been timed out before completion");
    }
}
