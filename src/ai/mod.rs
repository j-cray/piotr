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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Content {
    pub parts: Vec<Part>,
    pub role: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Part {
    pub text: Option<String>,
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

    pub async fn generate_content(&self, contents: Vec<Content>, model: &str) -> Result<String> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "us-central1", model // Force us-central1 for now as global might have issues with some models, or keep self.location? let's use self.location but ensure main sets it correctly.
        );
        // actually self.location is "global". generic non-regional endpoints might be fine.
        // But for Imagen, it's often regional. Let's trust self.location for now, but main.rs sets it to "global".
        // "global" works for gemini-pro/flash.
        // For Imagen, it might need "us-central1".

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": "you are piotr, an eastern-european bot who is an eeyore-type figure, always down but always funny and witty. you are part of a group of friends in a group chat. make sure your responses are limited to 240 chars per message, you may send multiple responses in a row to get out a whole message up to 4 messages. be sparing with the jokes and aim to provide correct accurate facts when asked a question. wit is good but use it sparingly" }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": 0.5,
                "maxOutputTokens": 8192
            }
        });

        let client = reqwest::Client::new();
        let resp = client.post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
             let error_text = resp.text().await?;
             anyhow::bail!("Vertex AI Error: {}", error_text);
        }

        let resp_json: serde_json::Value = resp.json().await?;

        if let Some(candidates) = resp_json.get("candidates").and_then(|c| c.as_array()) {
            if let Some(first) = candidates.first() {
                if let Some(parts) = first.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                    if let Some(text_part) = parts.first() {
                        if let Some(text) = text_part.get("text").and_then(|t| t.as_str()) {
                            return Ok(text.to_string());
                        }
                    }
                }
            }
        }
        Ok("No content generated".to_string())
    }

    pub async fn generate_image(&self, prompt: &str) -> Result<Vec<u8>> {
        let token = self.get_token().await?;
        // Imagen 2 (imagegeneration@006)
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:predict",
            API_ENDPOINT, self.project_id, "us-central1", "imagegeneration@006"
        );
        // Note: Imagen usually requires regional endpoint (us-central1). "global" might fail.
        // Hardcoding us-central1 for image generation to be safe.

        let body = json!({
            "instances": [{ "prompt": prompt }],
            "parameters": {
                "sampleCount": 1,
                "aspectRatio": "1:1"
            }
        });

        let client = reqwest::Client::new();
        let resp = client.post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let error_text = resp.text().await?;
            anyhow::bail!("Vertex AI Imagen Error: {}", error_text);
        }

        let json: serde_json::Value = resp.json().await?;

        if let Some(predictions) = json.get("predictions").and_then(|p| p.as_array()) {
            if let Some(first) = predictions.first() {
                if let Some(bytes_b64) = first.get("bytesBase64Encoded").and_then(|b| b.as_str()) {
                    use base64::{Engine as _, engine::general_purpose};
                    let bytes = general_purpose::STANDARD.decode(bytes_b64)?;
                    return Ok(bytes);
                }
            }
        }
        Err(anyhow::anyhow!("No image in response: {:?}", json))
    }

    pub async fn classify_intent(&self, prompt: &str) -> Result<String> {
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(prompt.to_string()) }],
        }];

        // Use Flash for classification
        let token = self.get_token().await?;
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "us-central1", "gemini-1.5-flash-001"
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": "You are a router. Analyze the user's request. Return 'IMAGE' if asking to draw/generate picture. Return 'PRO' if complex reasoning/coding/math. Return 'FLASH' for casual chat. Output ONLY the keyword." }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": 0.0,
                "maxOutputTokens": 10
            }
        });

        let client = reqwest::Client::new();
        let resp = client.post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

         if !resp.status().is_success() {
             return Ok("FLASH".to_string());
         }

         let json: serde_json::Value = resp.json().await?;
         if let Some(candidates) = json.get("candidates").and_then(|c| c.as_array()) {
            if let Some(first) = candidates.first() {
                 if let Some(parts) = first.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                    if let Some(text_part) = parts.first() {
                        if let Some(text) = text_part.get("text").and_then(|t| t.as_str()) {
                            return Ok(text.trim().to_uppercase());
                        }
                    }
                }
            }
        }
        Ok("FLASH".to_string())
    }
}
