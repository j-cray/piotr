use dotenv::dotenv;
use std::env;

use piotr::ai;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    env_logger::init();

    let project_id = env::var("GCP_PROJECT_ID").expect("GCP_PROJECT_ID must be set for this test");
    let client = ai::VertexClient::new(&project_id);

    println!("Testing Image Generation with imagen-4.0-generate-001...");
    match client.generate_image("A cute robot holding a flower, high quality", "imagen-4.0-generate-001").await {
        Ok(bytes) => println!("Success! Generated {} bytes.", bytes.len()),
        Err(e) => println!("Error: {:?}", e),
    }

    Ok(())
}
