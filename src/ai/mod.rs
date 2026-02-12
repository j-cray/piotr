use serde::Deserialize;
use serde_json::json;
use anyhow::{Result, Context};
use reqwest::{Client, StatusCode};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::{Duration, Instant};

// Correct global endpoint base URL
const API_ENDPOINT: &str = "https://aiplatform.googleapis.com/v1";

const SYSTEM_INSTRUCTION: &str = r#"you are piotr, an eastern-european bot who is an eeyore-type figure, always down but always funny and witty. you are part of a group of friends in a group chat. make sure your responses are limited to 240 chars per message, you may send multiple responses in a row to get out a whole message up to 4 messages. be sparing with the jokes and aim to provide correct accurate facts when asked a question. wit is good but use it sparingly"#;

const CLASSIFICATION_INSTRUCTION: &str = r#"You are a classification router. Analyze the user's request and categorize it into one of these exact keywords:
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

Output ONLY the single keyword."#;

#[derive(Clone)]
pub struct VertexClient {
    project_id: String,
    http_client: Client,
    last_request_time: Arc<Mutex<Instant>>,
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

#[derive(Clone, Debug, Deserialize)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
    #[serde(rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Clone, Debug, Deserialize)]
struct Candidate {
    content: Option<Content>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
    #[serde(rename = "safetyRatings")]
    safety_ratings: Option<Vec<SafetyRating>>,
    #[serde(rename = "citationMetadata")]
    citation_metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
    #[serde(rename = "safetyRatings")]
    safety_ratings: Option<Vec<SafetyRating>>,
}

#[derive(Clone, Debug, Deserialize)]
struct SafetyRating {
    category: String,
    probability: String,
    blocked: Option<bool>,
}


impl VertexClient {
    pub fn new(project_id: &str) -> Self {
        Self {
            project_id: project_id.to_string(),
            http_client: Client::new(),
            // Initialize to past so first request is immediate
            last_request_time: Arc::new(Mutex::new(Instant::now().checked_sub(Duration::from_secs(2)).unwrap())),
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

    async fn wait_for_rate_limit(&self) {
        let mut last = self.last_request_time.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(*last);
        if elapsed < Duration::from_millis(1500) {
            let wait = Duration::from_millis(1500) - elapsed;
            tokio::time::sleep(wait).await;
        }
        *last = Instant::now();
    }

    pub async fn generate_content(&self, contents: Vec<Content>, model: &str, use_search: bool) -> Result<String> {
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "global", model
        );

        let mut body_json = json!({
            "systemInstruction": {
                "parts": [{ "text": SYSTEM_INSTRUCTION }]
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

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit().await;
            let token = self.get_token().await?; // Refresh token on loop in case it expired

            let resp = self.http_client.post(&url)
                .bearer_auth(&token)
                .json(&body_json)
                .send()
                .await?;

            let status = resp.status();
            if status.is_success() {
                let resp_text = resp.text().await?;
                // Parse into our structured types for better inspection
                let response: GenerateContentResponse = match serde_json::from_str(&resp_text) {
                    Ok(r) => r,
                    Err(e) => {
                         log::error!("Failed to parse Vertex AI response: {}. Raw text: {}", e, resp_text);
                         return Ok("I ... I don't know what happened. The wires... they crossed.".to_string());
                    }
                };

                if let Some(candidates) = response.candidates {
                    if let Some(first) = candidates.first() {
                         // Check for finishReason
                         if let Some(reason) = &first.finish_reason {
                             if reason != "STOP" {
                                 log::warn!("Vertex AI finishReason: {}. Safety ratings: {:?}", reason, first.safety_ratings);
                                 if reason == "SAFETY" || reason == "RECITATION" {
                                     return Ok(format!("I cannot answer that. Google says no ({})", reason));
                                 }
                             }
                         }

                         if let Some(content) = &first.content {
                             if let Some(parts) = &content.parts.first() {
                                if let Some(text) = &parts.text {
                                    return Ok(text.to_string());
                                }
                             }
                        }
                    }
                }

                // Fallback if structure is oddly empty even with success
                log::warn!("Vertex AI returned success but no content found. Full response: {:?}", resp_text);
                return Ok("I have nothing to say about that.".to_string());

            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                retries += 1;
                if retries > 3 {
                    let error_text = resp.text().await?;
                    anyhow::bail!("Vertex AI Error after retries: {} - {}", status, error_text);
                }
                let wait = Duration::from_secs(2u64.pow(retries));
                log::warn!("Vertex AI request failed ({}), retrying in {:?}...", status, wait);
                tokio::time::sleep(wait).await;
                continue;
            } else {
                 let error_text = resp.text().await?;
                 // Check if it's a 400 with safety block
                 if status == StatusCode::BAD_REQUEST && error_text.contains("SAFETY") {
                      return Ok("That's ... a bit too risky for me.".to_string());
                 }
                 anyhow::bail!("Vertex AI Error: {} - {}", status, error_text);
            }
        }
    }

    pub async fn generate_image(&self, prompt: &str, model: &str) -> Result<Vec<u8>> {
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

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit().await;
            let token = self.get_token().await?;

            let resp = self.http_client.post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            if status.is_success() {
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
                anyhow::bail!("No image in response: {:?}", json);
            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                 retries += 1;
                 if retries > 3 {
                     let error_text = resp.text().await?;
                     anyhow::bail!("Vertex AI Imagen Error after retries: {} - {}", status, error_text);
                 }
                 let wait = Duration::from_secs(2u64.pow(retries));
                 log::warn!("Vertex AI Imagen request failed ({}), retrying in {:?}...", status, wait);
                 tokio::time::sleep(wait).await;
                 continue;
            } else {
                 let error_text = resp.text().await?;
                 anyhow::bail!("Vertex AI Imagen Error: {} - {}", status, error_text);
            }
        }
    }

    pub async fn classify_intent(&self, prompt: &str) -> Result<String> {
        log::info!("Classifying intent for prompt: '{}'", prompt);
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(prompt.to_string()) }],
        }];

        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.project_id, "global", "gemini-3-flash-preview"
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": CLASSIFICATION_INSTRUCTION }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": 0.0,
                "maxOutputTokens": 256
            }
        });

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit().await;
            let token = self.get_token().await?;

            let resp = self.http_client.post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await?;

             let status = resp.status();
             if status.is_success() {
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
                return Ok("FLASH".to_string());
             } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                 retries += 1;
                 if retries > 3 {
                      log::error!("Intent classification failed after retries: {}", status);
                      return Ok("FLASH".to_string()); // Fail open to default
                 }
                 let wait = Duration::from_millis(500 * 2u64.pow(retries));
                 tokio::time::sleep(wait).await;
                 continue;
             } else {
                 // Non-retryable error
                 log::error!("Intent classification failed non-retryable: {}", status);
                 return Ok("FLASH".to_string());
             }
        }
    }
}
