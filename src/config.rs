use serde::Deserialize;
use anyhow::Result;

#[derive(Debug, Deserialize, Clone, Default)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub ai: AiConfig,
    pub signal: SignalConfig,
    pub performance: PerformanceConfig,
    pub bot: BotConfig,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let config = config::Config::builder()
            .add_source(config::File::with_name("config.json5").required(true))
            // Optionally allow environment variables to override settings
            .add_source(config::Environment::with_prefix("PIOTR").separator("__"))
            .build()?;

        let app_config: AppConfig = config.try_deserialize()?;
        Ok(app_config)
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SecurityConfig {
    pub profile_encryption_key: String,
    pub anonymize_key: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct AiConfig {
    pub gcp_project_id: String,
    pub gcp_location: String,
    pub models: AiModelsConfig,
    pub generation: AiGenerationConfig,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct AiModelsConfig {
    pub chat: String,
    pub classification: String,
    pub imagen: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct AiGenerationConfig {
    pub temperature: f32,
    pub max_output_tokens: i32,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SignalConfig {
    pub phone_number: Option<String>,
    pub data_path: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct PerformanceConfig {
    pub max_concurrent_requests: usize,
    pub message_processing_timeout_secs: u64,
    pub api_cooldown_ms: u64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct BotConfig {
    pub name: String,
    pub location: String,
    pub persona: String,
    pub target_message_length_chars: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_config_deserialize_json5() {
        let json5_str = r#"
        {
            database: { url: "sqlite://test.db" },
            security: { profile_encryption_key: "abc", anonymize_key: "def" },
            ai: {
                gcp_project_id: "test-proj",
                gcp_location: "us-west1",
                models: { chat: "m1", classification: "m2", imagen: "m3" },
                generation: { temperature: 0.7, max_output_tokens: 100 }
            },
            signal: { data_path: "/tmp/signal", phone_number: "+1234567890" },
            performance: { max_concurrent_requests: 5, message_processing_timeout_secs: 10, api_cooldown_ms: 500 },
            bot: { name: "TestBot", location: "Earth", persona: "Tester", target_message_length_chars: 500 }
        }
        "#;

        let config = config::Config::builder()
            .add_source(config::File::from_str(json5_str, config::FileFormat::Json5))
            .build()
            .expect("Failed to build config from string");

        let app_config: AppConfig = config.try_deserialize().expect("Failed to deserialize AppConfig");
        
        assert_eq!(app_config.database.url, "sqlite://test.db");
        assert_eq!(app_config.security.anonymize_key, "def");
        assert_eq!(app_config.ai.gcp_project_id, "test-proj");
        assert_eq!(app_config.ai.models.chat, "m1");
        assert_eq!(app_config.ai.generation.temperature, 0.7);
        assert_eq!(app_config.signal.data_path, "/tmp/signal");
        assert_eq!(app_config.signal.phone_number.as_deref(), Some("+1234567890"));
        assert_eq!(app_config.performance.api_cooldown_ms, 500);
        assert_eq!(app_config.bot.name, "TestBot");
    }

    #[test]
    fn test_load_default_template_config() {
        // Ensure the config.json5 template file at the project root is valid and parses into AppConfig
        let config_res = AppConfig::load();
        
        assert!(config_res.is_ok(), "Failed to load root config.json5: {:?}", config_res.err());
        let config = config_res.unwrap();
        
        // Assert some known defaults from the template
        assert_eq!(config.database.url, "sqlite://data/piotr.db");
        assert_eq!(config.ai.gcp_location, "us-central1");
        assert_eq!(config.bot.name, "Piotr");
        assert_eq!(config.performance.max_concurrent_requests, 10);
    }
}
