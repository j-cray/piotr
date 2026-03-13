pub mod memory;

use serde::Deserialize;
use serde_json::json;
use anyhow::Result;
use reqwest::{Client, StatusCode};
use std::sync::Arc;
use tokio::sync::Mutex;
use std::time::{Duration, Instant};
use google_cloud_auth::credentials::{Builder as AuthBuilder, AccessTokenCredentials};
use tokio::sync::OnceCell;

// Correct global endpoint base URL
const API_ENDPOINT: &str = "https://aiplatform.googleapis.com/v1";

const CLASSIFICATION_INSTRUCTION: &str = r#"You are a classification router. Analyze the user's request and categorize it into one of these exact keywords:
- IGNORE: If the user is mentioning you but clearly talking to someone else in the group chat and not expecting you to reply, or if the SYSTEM prompt instructs you to output IGNORE.
- IMAGE: If request asks to 'draw', 'generate', 'create', 'sketch', or 'paint' an image/picture/photo/art/robot, OR specifically says 'generate an image', 'high quality', 'ultra realistic', '4k', or 'detailed'.
- PRO: If request involves complex reasoning, coding, math, or analysis.
- SEARCH: If request asks to 'search', 'google', 'find info', 'who is', 'what is', 'latest news', 'lookup', or contains 'search the web'.
- FLASH: For casual chat, greetings, or simple questions.

Input: 'draw a cat' -> Output: IMAGE
Input: 'generate an image of a dog' -> Output: IMAGE
Input: 'sketch a robot' -> Output: IMAGE
Input: 'search for rust release' -> Output: SEARCH
Input: 'google who won the super bowl' -> Output: SEARCH
Input: 'find info on mars' -> Output: SEARCH
Input: 'search the web for olympics' -> Output: SEARCH
Input: 'hello' -> Output: FLASH
Input: 'code a snake game' -> Output: PRO
Input: 'I think @Piotr is broken' -> Output: IGNORE

Output ONLY the single keyword."#;

#[derive(Clone)]
pub struct VertexClient {
    config: Arc<crate::config::AppConfig>,
    http_client: Client,
    rate_limiters: Arc<EndpointRateLimiters>,
    token_provider: Arc<OnceCell<AccessTokenCredentials>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointType {
    GenerateContent,
    GenerateImage,
    ClassifyIntent,
    AnalyzeReaction,
    AnalyzeProfileUpdate,
    AnalyzeGroupProfileUpdate,
    CountTokens,
}

pub struct EndpointRateLimiters {
    pub generate_content: Mutex<Instant>,
    pub generate_image: Mutex<Instant>,
    pub classify_intent: Mutex<Instant>,
    pub analyze_reaction: Mutex<Instant>,
    pub analyze_profile_update: Mutex<Instant>,
    pub analyze_group_profile_update: Mutex<Instant>,
    pub count_tokens: Mutex<Instant>,
}

impl EndpointRateLimiters {
    pub fn new() -> Self {
        let past = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
        Self {
            generate_content: Mutex::new(past),
            generate_image: Mutex::new(past),
            classify_intent: Mutex::new(past),
            analyze_reaction: Mutex::new(past),
            analyze_profile_update: Mutex::new(past),
            analyze_group_profile_update: Mutex::new(past),
            count_tokens: Mutex::new(past),
        }
    }
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
#[allow(dead_code)]
struct GenerateContentResponse {
    candidates: Option<Vec<Candidate>>,
    #[serde(rename = "promptFeedback")]
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
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
#[allow(dead_code)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
    #[serde(rename = "safetyRatings")]
    safety_ratings: Option<Vec<SafetyRating>>,
}

#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
struct SafetyRating {
    category: String,
    probability: String,
    blocked: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ReactionAnalysis {
    pub sentiment_score: f32, // -1.0 (Negative) to 1.0 (Positive)
    pub reasoning: String,
    pub tags: Vec<String>, // e.g. "sarcastic", "supportive", "confused"
}


impl VertexClient {
    pub fn new(config: Arc<crate::config::AppConfig>) -> Self {
        Self {
            config,
            http_client: Client::new(),
            rate_limiters: Arc::new(EndpointRateLimiters::new()),
            token_provider: Arc::new(OnceCell::new()),
        }
    }

    async fn get_token(&self) -> Result<String> {
        let provider = self.token_provider.get_or_try_init(|| async {
            AuthBuilder::default()
                .with_quota_project_id(&self.config.ai.gcp_project_id)
                .build_access_token_credentials()
        }).await.map_err(|e| anyhow::anyhow!("Failed to build credentials provider: {}", e))?;

        let access_token = provider.access_token().await.map_err(|e| anyhow::anyhow!("Failed to get access token: {}", e))?;
        Ok(access_token.token)
    }

    async fn wait_for_rate_limit(&self, endpoint: EndpointType) {
        let mutex = match endpoint {
            EndpointType::GenerateContent => &self.rate_limiters.generate_content,
            EndpointType::GenerateImage => &self.rate_limiters.generate_image,
            EndpointType::ClassifyIntent => &self.rate_limiters.classify_intent,
            EndpointType::AnalyzeReaction => &self.rate_limiters.analyze_reaction,
            EndpointType::AnalyzeProfileUpdate => &self.rate_limiters.analyze_profile_update,
            EndpointType::AnalyzeGroupProfileUpdate => &self.rate_limiters.analyze_group_profile_update,
            EndpointType::CountTokens => &self.rate_limiters.count_tokens,
        };
        let mut last = mutex.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(*last);
        let cooldown = Duration::from_millis(self.config.performance.api_cooldown_ms);
        if elapsed < cooldown {
            let wait = cooldown - elapsed;
            tokio::time::sleep(wait).await;
        }
        *last = Instant::now();
    }

    pub async fn generate_content(&self, contents: Vec<Content>, model: &crate::config::ModelSettings, use_search: bool) -> Result<String> {
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.config.ai.gcp_project_id, self.config.ai.gcp_location, model.name
        );

        let mut body_json = json!({
            "systemInstruction": {
                "parts": [{ "text": &self.config.bot.system_prompt }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": model.temperature.unwrap_or(crate::config::DEFAULT_CHAT_TEMPERATURE),
                "maxOutputTokens": model.max_output_tokens.unwrap_or(crate::config::DEFAULT_CHAT_MAX_OUTPUT_TOKENS)
            }
        });

        if use_search {
             if let Some(obj) = body_json.as_object_mut() {
                 obj.insert("tools".to_string(), json!([{ "googleSearch": {} }]));
             }
        }

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit(EndpointType::GenerateContent).await;
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
                         tracing::error!("Failed to parse Vertex AI response: {}. Raw text length: {}", e, resp_text.len());
                         return Ok("I ... I don't know what happened. The wires... they crossed.".to_string());
                    }
                };

                if let Some(candidates) = response.candidates {
                    if let Some(first) = candidates.first() {
                         // Check for finishReason
                         if let Some(reason) = &first.finish_reason {
                             if reason != "STOP" {
                                 tracing::warn!("Vertex AI finishReason: {}. Safety ratings: {:?}", reason, first.safety_ratings);
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
                tracing::warn!("Vertex AI returned success but no content found.");
                return Ok("I have nothing to say about that.".to_string());

            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                retries += 1;
                if retries > 3 {
                    let error_text = resp.text().await?;
                    anyhow::bail!("Vertex AI Error after retries: {} - {}", status, error_text);
                }
                let wait = Duration::from_secs(2u64.pow(retries));
                tracing::warn!("Vertex AI request failed ({}), retrying in {:?}...", status, wait);
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

    pub async fn generate_image(&self, prompt: &str, model: &crate::config::ModelSettings) -> Result<Vec<u8>> {
        let location = &self.config.ai.gcp_location;
        let url = format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:predict",
            location, self.config.ai.gcp_project_id, location, model.name
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
            self.wait_for_rate_limit(EndpointType::GenerateImage).await;
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
                 tracing::warn!("Vertex AI Imagen request failed ({}), retrying in {:?}...", status, wait);
                 tokio::time::sleep(wait).await;
                 continue;
            } else {
                 let error_text = resp.text().await?;
                 anyhow::bail!("Vertex AI Imagen Error: {} - {}", status, error_text);
            }
        }
    }

    pub async fn classify_intent(&self, prompt: &str) -> Result<String> {
        tracing::info!("Classifying intent for prompt");
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(prompt.to_string()) }],
        }];

        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.config.ai.gcp_project_id, self.config.ai.gcp_location, self.config.ai.models.classification.name
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": CLASSIFICATION_INSTRUCTION }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": self.config.ai.models.classification.temperature.unwrap_or(crate::config::DEFAULT_CLASSIFICATION_TEMPERATURE),
                "maxOutputTokens": self.config.ai.models.classification.max_output_tokens.unwrap_or(crate::config::DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS)
            }
        });

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit(EndpointType::ClassifyIntent).await;
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
                      tracing::error!("Intent classification failed after retries: {}", status);
                      return Ok("FLASH".to_string()); // Fail open to default
                 }
                 let wait = Duration::from_millis(500 * 2u64.pow(retries));
                 tokio::time::sleep(wait).await;
                 continue;
             } else {
                 // Non-retryable error
                 tracing::error!("Intent classification failed non-retryable: {}", status);
                 return Ok("FLASH".to_string());
             }
        }
    }

    pub async fn analyze_reaction(&self, user_prompt: &str, bot_response: &str, emoji: &str) -> Result<ReactionAnalysis> {
        let system_prompt = r#"You are an emotional intelligence analyst for a chat bot.
Your task is to analyze a user's emoji reaction to a bot's response in the context of their conversation.
Determine if the reaction is POSITIVE (reinforces behavior) or NEGATIVE (discourages behavior).
Account for sarcasm (e.g., crying emoji can be positive laughter, or negative sadness).
Output JSON ONLY with the following structure:
{
  "sentiment_score": float, // -1.0 to 1.0.
  "reasoning": string, // Explanation of your analysis.
  "tags": [string] // List of keywords describing the interaction.
}
Example:
Input: User="I broke prod", Bot="Good job", Emoji="😭"
Output: { "sentiment_score": -0.8, "reasoning": "User is distressed about breaking prod, bot was sarcastic but user is genuinely upset.", "tags": ["distress", "sarcasm_failure"] }
Example:
Input: User="Tell joke", Bot="Why did chicken cross road?", Emoji="😂"
Output: { "sentiment_score": 1.0, "reasoning": "User found the joke funny.", "tags": ["humor", "success"] }
"#;

        let user_msg = format!(
            "User: {}\nBot: {}\nUser Reacted With: {}",
            user_prompt, bot_response, emoji
        );

        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(user_msg) }],
        }];

        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.config.ai.gcp_project_id, self.config.ai.gcp_location, self.config.ai.models.classification.name
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": system_prompt }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": self.config.ai.models.classification.temperature.unwrap_or(crate::config::DEFAULT_REACTION_ANALYSIS_TEMPERATURE), // Low temp for analysis
                "maxOutputTokens": self.config.ai.models.classification.max_output_tokens.unwrap_or(crate::config::DEFAULT_REACTION_ANALYSIS_MAX_OUTPUT_TOKENS),
                "responseMimeType": "application/json"
            }
        });

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit(EndpointType::AnalyzeReaction).await;
            let token = self.get_token().await?;

            let resp = self.http_client.post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            if status.is_success() {
                 let json: serde_json::Value = resp.json().await?;
                 // Extract text
                 if let Some(candidates) = json.get("candidates").and_then(|c| c.as_array()) {
                    if let Some(first) = candidates.first() {
                         if let Some(parts) = first.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                            if let Some(text_part) = parts.first() {
                                if let Some(text) = text_part.get("text").and_then(|t| t.as_str()) {
                                    // Parse JSON from text
                                    match serde_json::from_str::<ReactionAnalysis>(text) {
                                        Ok(analysis) => return Ok(analysis),
                                        Err(e) => {
                                            tracing::error!("Failed to parse analysis JSON: {}. Text length: {}", e, text.len());
                                            // Fallback
                                            return Ok(ReactionAnalysis {
                                                sentiment_score: 0.0,
                                                reasoning: format!("Failed to parse: {}", text),
                                                tags: vec!["parse_error".to_string()]
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                anyhow::bail!("No content in analysis response");
            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                 retries += 1;
                 if retries > 3 {
                      anyhow::bail!("Analysis failed after retries: {}", status);
                 }
                 let wait = Duration::from_millis(500 * 2u64.pow(retries));
                 tokio::time::sleep(wait).await;
                 continue;
            } else {
                 let error_text = resp.text().await?;
                 anyhow::bail!("Analysis Error: {} - {}", status, error_text);
            }
        }
    }

    pub async fn analyze_profile_update(&self, current_profile: &crate::ai::memory::UserProfile, history: &str) -> Result<crate::ai::memory::UserProfile> {
        let system_prompt = r#"You are a user profile manager for a chatbot.
Your task is to analyze the recent conversation history and Update the user's profile.
- name: Extract the user's name if they mentioned it. Keep existing if known and not changed.
- nickname: Extract if the user explicitly asks to be called something (e.g. "call me Bob").
- personality_summary: concise summary of their personality traits observed so far.
- interaction_style: one or two words description (e.g. "casual", "sarcastic", "formal", "friendly").
- topics_of_interest: list of specific topics they have discussed or shown interest in.

Input will be the "Current Profile" and "Recent History".
Output the FULL updated profile as JSON.
Scale of personality analysis should be incremental - don't completely rewrite unless new info changes the perspective.

Structure:
{
  "id": "keep_original",
  "name": "string or null",
  "nickname": "string or null",
  "personality_summary": "string",
  "interaction_style": "string",
  "topics_of_interest": ["string"],
  "last_updated": 0
}
"#;

        let user_msg = format!(
            "Current Profile: {}\n\nRecent History:\n{}",
            serde_json::to_string_pretty(current_profile).unwrap_or_default(),
            history
        );

        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(user_msg) }],
        }];

        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.config.ai.gcp_project_id, self.config.ai.gcp_location, self.config.ai.models.classification.name
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": system_prompt }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": self.config.ai.models.classification.temperature.unwrap_or(crate::config::DEFAULT_PROFILE_UPDATE_TEMPERATURE),
                "maxOutputTokens": self.config.ai.models.classification.max_output_tokens.unwrap_or(crate::config::DEFAULT_PROFILE_UPDATE_MAX_OUTPUT_TOKENS),
                "responseMimeType": "application/json"
            }
        });

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit(EndpointType::AnalyzeProfileUpdate).await;
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
                                    match serde_json::from_str::<crate::ai::memory::UserProfile>(text) {
                                        Ok(mut profile) => {
                                            // Ensure ID and timestamp are handled correctly
                                            profile.id = current_profile.id.clone();
                                            profile.last_updated = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
                                            return Ok(profile);
                                        },
                                        Err(e) => {
                                            tracing::error!("Failed to parse profile update JSON: {}. Text length: {}", e, text.len());
                                            // Fail safe: return original
                                            return Ok(current_profile.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                anyhow::bail!("No content in profile analysis response");
            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                 retries += 1;
                 if retries > 3 {
                      anyhow::bail!("Profile analysis failed after retries: {}", status);
                 }
                 let wait = Duration::from_millis(500 * 2u64.pow(retries));
                 tokio::time::sleep(wait).await;
                 continue;
            } else {
                 let error_text = resp.text().await?;
                 anyhow::bail!("Profile Analysis Error: {} - {}", status, error_text);
            }
        }
    }

    pub async fn analyze_group_profile_update(&self, current_profile: &crate::ai::memory::GroupProfile, history: &str) -> Result<crate::ai::memory::GroupProfile> {
        let system_prompt = r#"You are a group profile manager for a chatbot in a group chat.
Your task is to analyze the recent conversation history and Update the group's profile.
- group_name: Extract the group's name if it was explicitly mentioned. Keep existing if known and not changed.
- group_vibe: one or two words description of the group's atmosphere (e.g. "chaotic", "serious", "meme-heavy", "supportive").
- inside_jokes: list of recurring jokes, memes, or specific funny references made by the group. Add new ones, but don't delete old ones unless they are definitely obsolete.
- common_topics: list of specific topics this group frequently discusses.
- important_memories: list of significant events or decisions that happened in this group chat.

Input will be the "Current Profile" and "Recent History".
Output the FULL updated profile as JSON.
Scale of analysis should be incremental - don't completely rewrite unless new info changes the perspective.

Structure:
{
  "id": "keep_original",
  "group_name": "string or null",
  "group_vibe": "string",
  "inside_jokes": ["string"],
  "common_topics": ["string"],
  "important_memories": ["string"],
  "last_updated": 0
}
"#;

        let user_msg = format!(
            "Current Profile: {}\n\nRecent History:\n{}",
            serde_json::to_string_pretty(current_profile).unwrap_or_default(),
            history
        );

        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some(user_msg) }],
        }];

        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            API_ENDPOINT, self.config.ai.gcp_project_id, self.config.ai.gcp_location, self.config.ai.models.classification.name
        );

        let body = json!({
            "systemInstruction": {
                "parts": [{ "text": system_prompt }]
            },
            "contents": contents,
            "generationConfig": {
                "temperature": self.config.ai.models.classification.temperature.unwrap_or(crate::config::DEFAULT_GROUP_PROFILE_UPDATE_TEMPERATURE), // Slightly higher than user profile for capturing "vibes"
                "maxOutputTokens": self.config.ai.models.classification.max_output_tokens.unwrap_or(crate::config::DEFAULT_GROUP_PROFILE_UPDATE_MAX_OUTPUT_TOKENS),
                "responseMimeType": "application/json"
            }
        });

        let mut retries = 0;
        loop {
            self.wait_for_rate_limit(EndpointType::AnalyzeGroupProfileUpdate).await;
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
                                    match serde_json::from_str::<crate::ai::memory::GroupProfile>(text) {
                                        Ok(mut profile) => {
                                            // Ensure ID and timestamp are handled correctly
                                            profile.id = current_profile.id.clone();
                                            profile.last_updated = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
                                            return Ok(profile);
                                        },
                                        Err(e) => {
                                            tracing::error!("Failed to parse group profile update JSON: {}. Text length: {}", e, text.len());
                                            // Fail safe: return original
                                            return Ok(current_profile.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                anyhow::bail!("No content in group profile analysis response");
            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                 retries += 1;
                 if retries > 3 {
                      anyhow::bail!("Group Profile analysis failed after retries: {}", status);
                 }
                 let wait = Duration::from_millis(500 * 2u64.pow(retries));
                 tokio::time::sleep(wait).await;
                 continue;
            } else {
                 let error_text = resp.text().await?;
                 anyhow::bail!("Group Profile Analysis Error: {} - {}", status, error_text);
            }
        }
    }

    pub async fn count_tokens(&self, contents: Vec<Content>, model: &crate::config::ModelSettings) -> Result<i32> {
        let url = format!(
            "{}/projects/{}/locations/{}/publishers/google/models/{}:countTokens",
            API_ENDPOINT, self.config.ai.gcp_project_id, self.config.ai.gcp_location, model.name
        );

        let body = json!({
            "contents": contents
        });

        // Simple retry for count_tokens as well, though less critical
        let mut retries = 0;
        loop {
            // Rate limit check (shared with generate)
            self.wait_for_rate_limit(EndpointType::CountTokens).await;
            let token = self.get_token().await?;

            let resp = self.http_client.post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            if status.is_success() {
                let json: serde_json::Value = resp.json().await?;
                if let Some(total_tokens) = json.get("totalTokens").and_then(|t| t.as_i64()) {
                    return Ok(total_tokens as i32);
                }
                anyhow::bail!("No totalTokens in response: {:?}", json);
            } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                 retries += 1;
                 if retries > 3 {
                      let error_text = resp.text().await?;
                      anyhow::bail!("CountTokens failed after retries: {} - {}", status, error_text);
                 }
                 let wait = Duration::from_millis(500 * 2u64.pow(retries));
                 tokio::time::sleep(wait).await;
                 continue;
            } else {
                 let error_text = resp.text().await?;
                 anyhow::bail!("CountTokens Error: {} - {}", status, error_text);
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_count_tokens_live() {
        // Only run if we can (this is an integration test)
        let config = match crate::config::AppConfig::load() {
            Ok(c) => Arc::new(c),
            Err(_) => return,
        };
        // Skip live tests if using placeholder config
        if config.ai.gcp_project_id.is_empty() || config.ai.gcp_project_id == "your-gcp-project-id" {
            println!("Skipping live test: real GCP Project ID not configured");
            return;
        }
        let client = VertexClient::new(config);

        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("Hello world".to_string()) }]
        }];

        match client.count_tokens(contents, &crate::config::ModelSettings { name: "gemini-3-flash-preview".to_string(), ..Default::default() }).await {
            Ok(count) => {
                println!("Token count: {}", count);
                assert!(count > 0);
            },
            Err(e) => {
                 panic!("Count tokens failed: {:?}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_classify_intent_mentions() {
        let config = match crate::config::AppConfig::load() {
            Ok(c) => Arc::new(c),
            Err(_) => return,
        };
        if config.ai.gcp_project_id.is_empty() || config.ai.gcp_project_id == "your-gcp-project-id" {
            println!("Skipping live test: real GCP Project ID not configured");
            return;
        }
        let client = VertexClient::new(config);

        // Test 1: Direct invocation
        let prompt_direct = "SYSTEM: Analyze if the user is talking *to* you or just talking *about* you. Reply IGNORE if they are mentioning you in passing to someone else without expecting a response. If they are addressing you directly (e.g. just '@Piotr' or asking a question), categorize the intent normally as FLASH, SEARCH, PRO, or IMAGE. User prompt: @Piotr";
        match client.classify_intent(prompt_direct).await {
            Ok(intent) => {
                println!("Direct invocation intent: {}", intent);
                assert_ne!(intent, "IGNORE");
            },
            Err(e) => panic!("Classification failed: {:?}", e),
        }

        // Test 2: Passing mention
        let prompt_passing = "SYSTEM: Analyze if the user is talking *to* you or just talking *about* you. Reply IGNORE if they are mentioning you in passing to someone else without expecting a response. If they are addressing you directly (e.g. just '@Piotr' or asking a question), categorize the intent normally as FLASH, SEARCH, PRO, or IMAGE. User prompt: I think @Piotr is broken";
        match client.classify_intent(prompt_passing).await {
            Ok(intent) => {
                println!("Passing mention intent: {}", intent);
                assert_eq!(intent, "IGNORE");
            },
            Err(e) => panic!("Classification failed: {:?}", e),
        }
    }

    #[test]
    fn test_reaction_analysis_parsing() {
        let raw_json = r#"{
            "sentiment_score": -0.8,
            "reasoning": "User is distressed.",
            "tags": ["distress", "sarcasm_failure"]
        }"#;

        let parsed: Result<ReactionAnalysis, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());
        let analysis = parsed.unwrap();
        assert_eq!(analysis.sentiment_score, -0.8);
        assert_eq!(analysis.tags.len(), 2);
    }

    #[test]
    fn test_generate_content_response_parsing() {
        let raw_json = r#"{
            "candidates": [
                {
                    "content": {
                        "role": "model",
                        "parts": [{"text": "Hello there!"}]
                    },
                    "finishReason": "STOP",
                    "safetyRatings": []
                }
            ],
            "promptFeedback": {
                "safetyRatings": []
            }
        }"#;

        let parsed: Result<GenerateContentResponse, _> = serde_json::from_str(raw_json);
        assert!(parsed.is_ok());

        let response = parsed.unwrap();
        assert!(response.candidates.is_some());

        let candidates = response.candidates.unwrap();
        assert_eq!(candidates.len(), 1);

        let first_candidate = &candidates[0];
        assert_eq!(first_candidate.finish_reason.as_deref(), Some("STOP"));

        let content = first_candidate.content.as_ref().unwrap();
        assert_eq!(content.role, "model");
        assert_eq!(content.parts[0].text.as_deref(), Some("Hello there!"));
    }

    #[test]
    fn test_generate_image_response_parsing() {
        // Test base64 image decoding logic using a dummy pixel
        // A valid 1x1 transparent PNG structure conceptually
        let b64_pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let raw_json = format!(r#"{{
            "predictions": [
                {{
                    "bytesBase64Encoded": "{}"
                }}
            ]
        }}"#, b64_pixel);

        let json: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
        let predictions = json.get("predictions").and_then(|p| p.as_array()).unwrap();
        let first = predictions.first().unwrap();
        let bytes_b64 = first.get("bytesBase64Encoded").and_then(|b| b.as_str()).unwrap();

        use base64::{Engine as _, engine::general_purpose};
        let bytes = general_purpose::STANDARD.decode(bytes_b64).unwrap();
        
        assert!(bytes.len() > 0);
        assert_eq!(&bytes[0..4], &[0x89, 0x50, 0x4e, 0x47]); // PNG header
    }

    #[tokio::test]
    async fn test_generate_image_live() {
        let config = match crate::config::AppConfig::load() {
            Ok(c) => Arc::new(c),
            Err(_) => return,
        };
        if config.ai.gcp_project_id.is_empty() || config.ai.gcp_project_id == "your-gcp-project-id" {
            println!("Skipping live test: real GCP Project ID not configured");
            return;
        }
        let client = VertexClient::new(config.clone());

        match client.generate_image("A tiny red dot", &config.ai.models.imagen).await {
            Ok(bytes) => {
                println!("Image generated successfully, size: {} bytes", bytes.len());
                assert!(bytes.len() > 0);
            },
            Err(e) => panic!("Image generation failed: {:?}", e),
        }
    }

    // --- SECURITY & STRICT TESTS ---

    #[test]
    fn test_malformed_json_deserialization() {
        let raw_json_missing_fields = r#"{
            "candidates": [
                {
                    "content": {
                        "parts": [{"text": "Missing role"}]
                    }
                }
            ]
        }"#;

        // Missing role should fail according to our strict struct definitions usually,
        // but let's see if serde handles it based on Option/Defaults.
        // Actually, role in Content is String, not Option<String>, so it MUST fail.
        let parsed: Result<GenerateContentResponse, _> = serde_json::from_str(raw_json_missing_fields);
        assert!(parsed.is_err(), "Deserialization should fail when required fields are missing");

        let raw_garbage = r#"This is not JSON at all"#;
        let parsed2: Result<GenerateContentResponse, _> = serde_json::from_str(raw_garbage);
        assert!(parsed2.is_err(), "Should fail on complete garbage");
    }

    #[test]
    fn test_content_and_part_serialization() {
        // Security: Ensure our JSON construction for Vertex exactly matches expected structure
        // preventing injection via malformed fields.
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part { text: Some("Normal text\nwith \"quotes\" and \\slashes".to_string()) }],
        };

        let serialized = serde_json::to_string(&content).unwrap();
        // It should properly escape the quotes and slashes, keeping JSON valid
        let deserialized: Content = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.role, "user");
        assert_eq!(deserialized.parts[0].text.as_deref(), Some("Normal text\nwith \"quotes\" and \\slashes"));
    }

    #[tokio::test]
    async fn test_generate_content_adversarial_input() {
        // We aren't testing live Vertex here, but we are testing that if
        // Vertex returns adversarial JSON strings inside the text part, we parse them safely
        // when expecting a Profile or similar.

        let adversarial_text = r#"{"id": "fake", "name": "Fake Name", "personality_summary": "} Malicious Payload {", "interaction_style": "evil", "topics_of_interest": [], "last_updated": 0}"#;

        // This is valid JSON, so it should parse securely into UserProfile.
        let parsed = serde_json::from_str::<crate::ai::memory::UserProfile>(adversarial_text);
        assert!(parsed.is_ok());
        let prof = parsed.unwrap();
        assert_eq!(prof.personality_summary, "} Malicious Payload {");
        // We proved that the JSON parser safely contains the structural characters inside the text field
    }

    #[test]
    fn test_classification_instruction_parsing_edge_cases() {
        // Intent classification expects a single word mostly. Let's ensure if it returns weirdness, we don't panic.
        // The parsing logic just grabs the text. We test the struct decoding logic.
        let edge_case_json = r#"{
            "candidates": [
                {
                    "content": {
                        "role": "model",
                        "parts": [{"text": "  FLASH \n\n"}]
                    }
                }
            ]
        }"#;
        let parsed: GenerateContentResponse = serde_json::from_str(edge_case_json).unwrap();
        let candidates = parsed.candidates.unwrap();
        let text = candidates[0].content.as_ref().unwrap().parts[0].text.as_deref().unwrap();

        // The consuming code does `.trim().to_uppercase()`.
        let final_intent = text.trim().to_uppercase();
        assert_eq!(final_intent, "FLASH");
    }

    #[tokio::test]
    async fn test_rate_limiters_independence() {
        let config = std::sync::Arc::new(crate::config::AppConfig::default());
        let client = VertexClient::new(config);

        // First call should be instant (since new() sets them to 2 secs in the past)
        let start = Instant::now();
        client.wait_for_rate_limit(EndpointType::GenerateContent).await;
        
        // Second call to same endpoint should take ~1.5 seconds
        client.wait_for_rate_limit(EndpointType::GenerateContent).await;
        let elapsed = start.elapsed();
        
        // Use a generous upper bound for CI environments
        assert!(elapsed >= Duration::from_millis(1400), "Should wait for rate limit");
        assert!(elapsed < Duration::from_millis(3000), "Should not wait excessively");

        // Call to a different endpoint should be instant because they are independent
        let start2 = Instant::now();
        client.wait_for_rate_limit(EndpointType::ClassifyIntent).await;
        let elapsed2 = start2.elapsed();
        assert!(elapsed2 < Duration::from_millis(100), "Independent endpoint should not be blocked");
    }

    #[tokio::test]
    async fn test_rate_limiter_cooldown_config() {
        let mut config_data = crate::config::AppConfig::default();
        config_data.performance.api_cooldown_ms = 300; // Custom cooldown
        let config = std::sync::Arc::new(config_data);
        let client = VertexClient::new(config);

        // First call should be instant
        let start = Instant::now();
        client.wait_for_rate_limit(EndpointType::GenerateContent).await;
        
        // Second call should wait ~300ms
        client.wait_for_rate_limit(EndpointType::GenerateContent).await;
        let elapsed = start.elapsed();
        
        assert!(elapsed >= Duration::from_millis(250), "Should wait according to config cooldown");
        assert!(elapsed < Duration::from_millis(1000), "Should not wait excessively");
    }
}
