use dotenv::dotenv;
use std::env;

#[path = "../ai/mod.rs"]
mod ai;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    env_logger::init();

    let project_id = env::var("GCP_PROJECT_ID").unwrap_or_else(|_| "piotr-487123".to_string());
    let client = ai::VertexClient::new(&project_id);

    println!("Testing Image Generation with imagen-4.0-generate-001...");
    match client.generate_image("A cute robot holding a flower, high quality", "imagen-4.0-generate-001").await {
        Ok(bytes) => println!("Success! Generated {} bytes.", bytes.len()),
        Err(e) => println!("Error: {:?}", e),
    }

    Ok(())
}
