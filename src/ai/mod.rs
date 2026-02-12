use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
// use std::sync::Arc;
// use tokio::sync::Mutex;

const API_ENDPOINT: &str = "https://aiplatform.googleapis.com/v1";
const MODEL_ID: &str = "gemini-3-flash-preview"; // As requested

#[derive(Clone)]
pub struct VertexClient {
    project_id: String,
    location: String,
    http_client: Client,
}

// Response structs
#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: Option<Content>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Content {
    parts: Vec<Part>,
    role: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Part {
    text: Option<String>,
}

impl VertexClient {
    pub fn new(project_id: &str) -> Self {
        Self {
            project_id: project_id.to_string(),
            location: "global".to_string(), // Trying global location
            http_client: Client::new(),
            // token_manager: Arc::new(Mutex::new(TokenManager {})),
        }
    }

    async fn get_token(&self) -> Result<String> {
        // Use gcloud auth print-access-token for simplicity in first pass dev
        // For production, use google-cloud-auth crate properly.
        // But since we have gcloud tool, let's try that first as a fallback if auth crate is complex.

        // Use gcloud auth print-access-token for simplicity in first pass dev
        // For production, use google-cloud-auth crate properly.
        // But since we have gcloud tool, let's try that first as a fallback if auth crate is complex.
        // Note: verify 0.13 API.
        // If 0.13 is tricky, we might need to adjust.
        // Let's assume standard google-cloud-auth usage for now.

        // Actually, let's use the simplest reliable method:
        // If GOOGLE_APPLICATION_CREDENTIALS is set, usage is easy.

        // For now, let's shell out to gcloud for dev speed, it's robust in this env.
        let output = tokio::process::Command::new("gcloud")
            .args(&["auth", "print-access-token"])
            .output()
            .await?;

        if !output.status.success() {
             anyhow::bail!("Failed to get token via gcloud: {}", String::from_utf8_lossy(&output.stderr));
        }

        let token = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(token)
    }

    pub async fn generate_content(&self, prompt: &str) -> Result<String> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, self.location, MODEL_ID
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": "you are piotr, an eastern-european bot who is an eeyore-type figure, always down but always funny and witty. you are part of a group of friends in a group chat. make sure your responses are limited to 240 chars per message, you may send multiple responses in a row to get out a whole message up to 4 messages. be sparing with the jokes and aim to provide correct accurate facts when asked a question. wit is good but use it sparingly" }]
            },
            "contents": [{
                "role": "user",
                "parts": [{ "text": prompt }]
            }],
            "generationConfig": {
                "temperature": 0.5,
                "maxOutputTokens": 8192
            }
        });

        let resp = self.http_client.post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
             let error_text = resp.text().await?;
             anyhow::bail!("Vertex AI Error: {}", error_text);
        }

        let resp_json: GenerateContentResponse = resp.json().await?;

        if let Some(candidates) = resp_json.candidates {
            if let Some(first) = candidates.first() {
                if let Some(content) = &first.content {
                   if let Some(part) = content.parts.first() {
                       if let Some(text) = &part.text {
                           return Ok(text.clone());
                       }
                   }
                }
            }
        }

        Ok("No content generated".to_string())
    }
}
