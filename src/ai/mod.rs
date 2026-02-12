use serde::Deserialize;
use serde_json::json;
use anyhow::Result;
use reqwest::Client;

// Correct global endpoint base URL
const API_ENDPOINT: &str = "https://aiplatform.googleapis.com/v1";

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

    pub async fn generate_content(&self, contents: Vec<Content>, model: &str, use_search: bool) -> Result<String> {
        let token = self.get_token().await?;
        // Use global endpoint for Gemini
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "global", model
        );

        // For debugging, print the URL
        log::info!("Generating content with URL: {}", url);

        let mut body_json = json!({
            "systemInstruction": {
                "parts": [{ "text": "you are piotr, an eastern-european bot who is an eeyore-type figure, always down but always funny and witty. you are part of a group of friends in a group chat. make sure your responses are limited to 240 chars per message, you may send multiple responses in a row to get out a whole message up to 4 messages. be sparing with the jokes and aim to provide correct accurate facts when asked a question. wit is good but use it sparingly" }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": 0.5,
                "maxOutputTokens": 8192
            }
        });

        if use_search {
            if let Some(obj) = body_json.as_object_mut() {
                obj.insert("tools".to_string(), json!([{ "googleSearch": {} }]));
            }
        }

        let client = reqwest::Client::new();
        let resp = client.post(&url)
            .bearer_auth(token)
            .json(&body_json)
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
        // Imagen 3/4 likely requires regional endpoint (us-central1), NOT global AIPlatform.
        // We need to use valid regional endpoint for Imagen.
        let url = format!(
            "https://us-central1-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:predict",
            self.project_id, "us-central1", model
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
            log::error!("Vertex AI Imagen Raw Error: {}", error_text);
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
        log::info!("Classifying intent for prompt: '{}'", prompt);
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(prompt.to_string()) }],
        }];

        let token = self.get_token().await?;
        // Use gemini-3-flash-preview for classification via Global endpoint
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "global", "gemini-3-flash-preview"
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": "You are a classification router. Analyze the user's request and categorize it into one of these exact keywords:
                - IMAGE_4: If request asks for 'high quality', 'ultra realistic', '4k', or 'detailed' image/drawing/photo.
                - IMAGE_3: If request asks to 'draw', 'generate', 'create', 'sketch', or 'paint' an image/picture/photo/art/robot, OR specifically says 'generate an image'.
                - PRO: If request involves complex reasoning, coding, math, or analysis.
                - SEARCH: If request asks to 'search', 'google', 'find info', 'who is', 'what is', 'latest news', 'lookup', or contains 'search the web'.
                - FLASH: For casual chat, greetings, or simple questions.

                Input: 'draw a cat' -> Output: IMAGE_3
                Input: 'generate an image of a dog' -> Output: IMAGE_3
                Input: 'sketch a robot' -> Output: IMAGE_3
                Input: 'search for rust release' -> Output: SEARCH
                Input: 'google who won the super bowl' -> Output: SEARCH
                Input: 'find info on mars' -> Output: SEARCH
                Input: 'search the web for olympics' -> Output: SEARCH
                Input: 'hello' -> Output: FLASH
                Input: 'code a snake game' -> Output: PRO

                Output ONLY the single keyword." }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": 0.0,
                "maxOutputTokens": 256
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
