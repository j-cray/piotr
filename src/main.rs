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
    // let signal_service = signal::SignalService::new().await?;
    // signal_service.listen().await?;

    Ok(())
}
