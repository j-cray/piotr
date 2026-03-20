use dotenv::dotenv;
use std::sync::Arc;

use anyhow::Context;
use piotr::{ai, config::AppConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    tracing_subscriber::fmt::init();

    let config = Arc::new(AppConfig::load().context("Failed to load configuration")?);
    let client = ai::VertexClient::new(config);

    println!("Testing Image Generation with imagen-4.0-generate-001...");
    match client
        .generate_image(
            "A cute robot holding a flower, high quality",
            &piotr::config::ModelSettings {
                name: "imagen-4.0-generate-001".to_string(),
                ..Default::default()
            },
        )
        .await
    {
        Ok(bytes) => println!("Success! Generated {} bytes.", bytes.len()),
        Err(e) => println!("Error: {:?}", e),
    }

    Ok(())
}
