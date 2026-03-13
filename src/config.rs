use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub ai: AiConfig,
    pub signal: SignalConfig,
    pub performance: PerformanceConfig,
    pub bot: BotConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SecurityConfig {
    pub profile_encryption_key: String,
    pub anonymize_key: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AiConfig {
    pub gcp_project_id: String,
    pub gcp_location: String,
    pub models: AiModelsConfig,
    pub generation: AiGenerationConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AiModelsConfig {
    pub chat: String,
    pub classification: String,
    pub imagen: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AiGenerationConfig {
    pub temperature: f32,
    pub max_output_tokens: i32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SignalConfig {
    pub phone_number: Option<String>,
    pub data_path: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PerformanceConfig {
    pub max_concurrent_requests: usize,
    pub message_processing_timeout_secs: u64,
    pub api_cooldown_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BotConfig {
    pub name: String,
    pub location: String,
    pub persona: String,
    pub target_message_length_chars: usize,
}
