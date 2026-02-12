use serde::Deserialize;
use serde_json::json;
use anyhow::Result;
use reqwest::Client;

const API_ENDPOINT: &str = "https://us-central1-aiplatform.googleapis.com/v1";

#[derive(Clone)]
pub struct VertexClient {
    project_id: String,
    location: String,
    http_client: Client,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct Content {
    pub role: String,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct Part {
    pub text: Option<String>,
}

impl VertexClient {
    pub fn new(project_id: &str) -> Self {
        Self {
            project_id: project_id.to_string(),
            location: "global".to_string(), // Default, but overridden in methods
            http_client: Client::new(),
        }
    }

    async fn get_token(&self) -> Result<String> {
        // Use gcloud auth print-access-token
        let output = tokio::process::Command::new("gcloud")
            .args(&["auth", "print-access-token"])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("Failed to get gcloud token: {:?}", String::from_utf8(output.stderr));
        }

        let token = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(token)
    }

    pub async fn generate_content(&self, contents: Vec<Content>, model: &str) -> Result<String> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "us-central1", model
        );

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

    pub async fn generate_image(&self, prompt: &str, model: &str) -> Result<Vec<u8>> {
        let token = self.get_token().await?;
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:predict",
            API_ENDPOINT, self.project_id, "us-central1", model
        );

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

        let token = self.get_token().await?;
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "global", "gemini-1.5-flash-001"
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": "You are a router. Analyze the user's request.
                - Return 'IMAGE_4' if the user specifically asks for 'imagen 4', 'high quality', 'ultra realistic', '4k', or 'detailed' image.
                - Return 'IMAGE_3' if the user asks to draw/generate an image but it is standard/fast/simple or asks for 'imagen 3'.
                - Return 'PRO' if complex reasoning/coding/math.
                - Return 'FLASH' for casual chat.
                Output ONLY the keyword." }]
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
