use serde::Deserialize;
use anyhow::Result;

pub const DEFAULT_CHAT_TEMPERATURE: f32 = 0.5;
pub const DEFAULT_CHAT_MAX_OUTPUT_TOKENS: i32 = 8192;

pub const DEFAULT_CLASSIFICATION_TEMPERATURE: f32 = 0.0;
pub const DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS: i32 = 256;

pub const DEFAULT_REACTION_ANALYSIS_TEMPERATURE: f32 = 0.2;
pub const DEFAULT_REACTION_ANALYSIS_MAX_OUTPUT_TOKENS: i32 = 512;

pub const DEFAULT_PROFILE_UPDATE_TEMPERATURE: f32 = 0.1;
pub const DEFAULT_PROFILE_UPDATE_MAX_OUTPUT_TOKENS: i32 = 1024;

pub const DEFAULT_GROUP_PROFILE_UPDATE_TEMPERATURE: f32 = 0.2;
pub const DEFAULT_GROUP_PROFILE_UPDATE_MAX_OUTPUT_TOKENS: i32 = 2048;

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub ai: AiConfig,
    pub signal: SignalConfig,
    pub performance: PerformanceConfig,
    pub bot: BotConfig,
}

use serde_json::Value;
use std::path::{Path, PathBuf};
use regex::Regex;

impl AppConfig {
    pub fn load() -> Result<Self> {
        let local_config = Path::new("config.json5");
        
        use etcetera::base_strategy::{BaseStrategy, choose_base_strategy};

        let strategy = choose_base_strategy()
            .expect("Failed to determine system configuration directory strategy");
        let fallback_config = strategy.config_dir().join("piotr").join("config.json5");

        let config_path = if local_config.exists() {
            PathBuf::from(local_config)
        } else if fallback_config.exists() {
            fallback_config
        } else {
            // Neither exists. Scaffold the default configuration on first boot.
            if let Some(parent) = fallback_config.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("Failed to create config directory {:?}: {}", parent, e))?;
            }
            
            let default_config_content = include_str!("../config.example.json5");
            std::fs::write(&fallback_config, default_config_content)
                .map_err(|e| anyhow::anyhow!("Failed to write default config to {:?}: {}", fallback_config, e))?;
            
            println!("No configuration found. A default configuration template has been generated at {:?}", fallback_config);
            fallback_config
        };
        
        Self::load_from(&config_path)
    }

    pub fn load_from(config_path: &Path) -> Result<Self> {
        // 1 & 2. Read config file and resolve $include directives
        let mut raw_value = Self::load_and_resolve_includes(config_path, 0)?;

        // 3. Collect env entries local to the config before substitution
        let injected_env = Self::apply_env_injection(&mut raw_value)?;

        // 4. Substitute ${VAR} references in string values
        Self::substitute_env_vars(&mut raw_value, &injected_env)?;

        // 5 & 6 & 7. Apply defaults, validate schema, normalize (done by deserialize)
        let config = config::Config::builder()
            .add_source(config::File::from_str(
                &serde_json::to_string(&raw_value)?,
                config::FileFormat::Json
            ))
            // 8. Apply runtime overrides
            .add_source(config::Environment::with_prefix("PIOTR").separator("__"))
            .build()?;

        let app_config: AppConfig = config.try_deserialize()?;
        Ok(app_config)
    }

    fn load_and_resolve_includes(path: &Path, depth: usize) -> Result<Value> {
        if depth > 10 {
            anyhow::bail!("Circular Include Error or Max Depth (10) Exceeded at path {:?}", path);
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file {:?}: {}", path, e))?;
        
        let mut value: Value = json5::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file {:?}: {}", path, e))?;

        Self::resolve_includes_recursive(&mut value, path.parent().unwrap_or(Path::new("")), depth)?;
        Ok(value)
    }

    fn resolve_includes_recursive(value: &mut Value, base_dir: &Path, depth: usize) -> Result<()> {
        match value {
            Value::Object(map) => {
                // Check for $include key
                if let Some(include_val) = map.remove("$include") {
                    let mut includes = Vec::new();
                    match include_val {
                        Value::String(s) => includes.push(s),
                        Value::Array(arr) => {
                            for item in arr {
                                if let Value::String(s) = item {
                                    includes.push(s);
                                }
                            }
                        }
                        _ => {}
                    }

                    // Load all includes
                    let mut merged_include = Value::Object(serde_json::Map::new());
                    for inc_path_str in includes {
                        let inc_path = base_dir.join(inc_path_str);
                        let inc_val = Self::load_and_resolve_includes(&inc_path, depth + 1)?;
                        json_value_merge::Merge::merge(&mut merged_include, &inc_val);
                    }

                    // If it only contained $include, replace entirely
                    if map.is_empty() {
                        *value = merged_include;
                    } else {
                        // Otherwise merge included content WITH siblings (siblings override include)
                        // It must be an object
                        let mut final_merged = merged_include;
                        
                        // Recursive resolve siblings first
                        for (_, v) in map.iter_mut() {
                            Self::resolve_includes_recursive(v, base_dir, depth)?;
                        }

                        // Siblings override includes
                        json_value_merge::Merge::merge(&mut final_merged, &Value::Object(map.clone()));
                        *value = final_merged;
                    }
                } else {
                    // No include, just recurse
                    for (_, v) in map.iter_mut() {
                        Self::resolve_includes_recursive(v, base_dir, depth)?;
                    }
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    Self::resolve_includes_recursive(item, base_dir, depth)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_env_injection(value: &mut Value) -> Result<std::collections::HashMap<String, String>> {
        let mut injected_env = std::collections::HashMap::new();
        if let Some(env_val) = value.get_mut("env") {
            if let Some(env_obj) = env_val.as_object_mut() {
                // vars map
                if let Some(vars_val) = env_obj.remove("vars") {
                    if let Some(vars_obj) = vars_val.as_object() {
                        for (k, v) in vars_obj {
                            if let Some(s) = v.as_str() {
                                injected_env.insert(k.clone(), s.to_string());
                            }
                        }
                    }
                }

                // other string fields
                let mut to_remove = Vec::new();
                for (k, v) in env_obj.iter() {
                    if k != "shellEnv" {
                        if let Some(s) = v.as_str() {
                            injected_env.insert(k.clone(), s.to_string());
                            to_remove.push(k.clone());
                        }
                    }
                }

                for k in to_remove {
                    env_obj.remove(&k);
                }
            }
        }
        Ok(injected_env)
    }

    fn substitute_env_vars(value: &mut Value, injected_env: &std::collections::HashMap<String, String>) -> Result<()> {
        // Matches $${VAR} as escape or ${VAR} as variable
        let re = Regex::new(r"(?P<escape>\$\$)\{(?P<evar>[A-Z_][A-Z0-9_]*)\}|\$(?P<unescaped>\$)?\{(?P<var>[A-Z_][A-Z0-9_]*)\}").unwrap();
        Self::substitute_recursive(value, &re, injected_env)
    }

    fn substitute_recursive(value: &mut Value, re: &Regex, injected_env: &std::collections::HashMap<String, String>) -> Result<()> {
        match value {
            Value::String(s) => {
                let mut new_string = String::new();
                let mut last_end = 0;
                
                for cap in re.captures_iter(s.as_str()) {
                    let m = cap.get(0).unwrap();
                    new_string.push_str(&s[last_end..m.start()]);
                    
                    if cap.name("escape").is_some() || cap.name("unescaped").is_some() {
                        // $${VAR} -> ${VAR}
                        // m.start() points to the first $. We skip it by starting at m.start() + 1
                        new_string.push_str(&s[m.start() + 1..m.end()]);
                    } else if let Some(var_match) = cap.name("var") {
                        let var_name = var_match.as_str();
                        if let Some(val) = injected_env.get(var_name) {
                            new_string.push_str(val);
                        } else {
                            match std::env::var(var_name) {
                                Ok(val) => new_string.push_str(&val),
                                Err(_) => anyhow::bail!("MissingEnvVarError: {}", var_name),
                            }
                        }
                    }
                    last_end = m.end();
                }
                new_string.push_str(&s[last_end..]);
                *s = new_string;
            }
            Value::Object(map) => {
                for (_, v) in map.iter_mut() {
                    Self::substitute_recursive(v, re, injected_env)?;
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    Self::substitute_recursive(item, re, injected_env)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct DatabaseConfig {
    pub url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://data/piotr.db".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SecurityConfig {
    pub profile_encryption_key: String,
    pub anonymize_key: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct AiConfig {
    pub gcp_project_id: String,
    pub gcp_location: String,
    pub models: AiModelsConfig,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            gcp_project_id: String::new(),
            gcp_location: "us-central1".to_string(),
            models: AiModelsConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct AiModelsConfig {
    #[serde(default = "default_chat_model")]
    pub chat: ModelSettings,
    #[serde(default = "default_classification_model")]
    pub classification: ModelSettings,
    #[serde(default = "default_imagen_model")]
    pub imagen: ModelSettings,
}

impl Default for AiModelsConfig {
    fn default() -> Self {
        Self {
            chat: default_chat_model(),
            classification: default_classification_model(),
            imagen: default_imagen_model(),
        }
    }
}

fn default_chat_model() -> ModelSettings {
    ModelSettings {
        name: "gemini-3-pro-preview".to_string(),
        temperature: Some(DEFAULT_CHAT_TEMPERATURE),
        max_output_tokens: Some(DEFAULT_CHAT_MAX_OUTPUT_TOKENS),
        max_input_tokens: Some(1000000),
    }
}

fn default_classification_model() -> ModelSettings {
    ModelSettings {
        name: "gemini-3-flash-preview".to_string(),
        temperature: Some(DEFAULT_CLASSIFICATION_TEMPERATURE),
        max_output_tokens: Some(DEFAULT_CLASSIFICATION_MAX_OUTPUT_TOKENS),
        max_input_tokens: Some(1000000),
    }
}

fn default_imagen_model() -> ModelSettings {
    ModelSettings {
        name: "imagen-3.0-generate-001".to_string(),
        temperature: None,
        max_output_tokens: None,
        max_input_tokens: None,
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelSettings {
    pub name: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<i32>,
    pub max_input_tokens: Option<i32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct SignalConfig {
    pub phone_number: Option<String>,
    pub data_path: String,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            phone_number: None,
            data_path: "data/signal-cli".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct PerformanceConfig {
    pub max_concurrent_requests: usize,
    pub message_processing_timeout_secs: u64,
    pub api_cooldown_ms: u64,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 10,
            message_processing_timeout_secs: 30,
            api_cooldown_ms: 1500,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct BotConfig {
    pub name: String,
    pub location: String,
    pub system_prompt: String,
    pub target_message_length_chars: usize,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            name: "Piotr".to_string(),
            location: "Unknown".to_string(),
            system_prompt: "Helpful and witty AI assistant".to_string(),
            target_message_length_chars: 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_app_config_deserialize_json5() {
        let json5_str = r#"
        {
            database: { url: "sqlite://test.db" },
            security: { profileEncryptionKey: "abc", anonymizeKey: "def" },
            ai: {
                gcpProjectId: "test-proj",
                gcpLocation: "us-west1",
                models: { 
                    chat: { name: "m1", temperature: 0.7, maxOutputTokens: 100, maxInputTokens: 1000 }, 
                    classification: { name: "m2" }, 
                    imagen: { name: "m3" } 
                }
            },
            signal: { dataPath: "/tmp/signal", phoneNumber: "+1234567890" },
            performance: { maxConcurrentRequests: 5, messageProcessingTimeoutSecs: 10, apiCooldownMs: 500 },
            bot: { name: "TestBot", location: "Earth", systemPrompt: "Tester", targetMessageLengthChars: 500 }
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
        assert_eq!(app_config.ai.models.chat.name, "m1");
        assert_eq!(app_config.ai.models.chat.temperature.unwrap(), 0.7);
        assert_eq!(app_config.ai.models.chat.max_output_tokens.unwrap(), 100);
        assert_eq!(app_config.ai.models.chat.max_input_tokens.unwrap(), 1000);
        assert_eq!(app_config.signal.data_path, "/tmp/signal");
        assert_eq!(app_config.signal.phone_number.as_deref(), Some("+1234567890"));
        assert_eq!(app_config.performance.api_cooldown_ms, 500);
        assert_eq!(app_config.bot.name, "TestBot");
    }

    #[test]
    fn test_include_resolution_and_merge() {
        let dir = tempdir().unwrap();
        let base_path = dir.path().join("base.json5");
        let root_path = dir.path().join("root.json5");

        let mut base_file = File::create(&base_path).unwrap();
        write!(base_file, r#"{{ bot: {{ name: "BaseBot", location: "BaseLoc" }}, performance: {{ maxConcurrentRequests: 1 }} }}"#).unwrap();

        let mut root_file = File::create(&root_path).unwrap();
        write!(root_file, r#"{{ $include: "./base.json5", bot: {{ location: "RootLoc" }} }}"#).unwrap();

        let val = AppConfig::load_and_resolve_includes(&root_path, 0).expect("Failed to load and resolve includes");
        
        // Root location should override Base location, but name and performance should be preserved
        let bot_obj = val.get("bot").unwrap().as_object().unwrap();
        assert_eq!(bot_obj.get("name").unwrap().as_str().unwrap(), "BaseBot");
        assert_eq!(bot_obj.get("location").unwrap().as_str().unwrap(), "RootLoc");

        let perf_obj = val.get("performance").unwrap().as_object().unwrap();
        assert_eq!(perf_obj.get("maxConcurrentRequests").unwrap().as_u64().unwrap(), 1);
    }

    #[test]
    fn test_env_injection_and_substitution() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("env_config.json5");

        let mut file = File::create(&config_path).unwrap();
        write!(file, r#"{{
            env: {{
                vars: {{
                    TEST_SUBST_VAR: "Hello injected var"
                }},
                ROOT_ENV_VAR: "Root level injection"
            }},
            bot: {{
                name: "${{TEST_SUBST_VAR}}",
                systemPrompt: "Look at $${{ESCAPED_VAR}}"
            }}
        }}"#).unwrap();

        let mut val = AppConfig::load_and_resolve_includes(&config_path, 0).unwrap();
        
        // Apply injection
        let injected_env = AppConfig::apply_env_injection(&mut val).unwrap();
        
        // Assert env vars are collected
        assert_eq!(injected_env.get("TEST_SUBST_VAR").unwrap(), "Hello injected var");
        assert_eq!(injected_env.get("ROOT_ENV_VAR").unwrap(), "Root level injection");

        // Assert they are removed from the JSON tree
        let env_obj = val.get("env").unwrap().as_object().unwrap();
        assert!(env_obj.get("vars").is_none());
        assert!(env_obj.get("ROOT_ENV_VAR").is_none());

        // Process substitutions
        AppConfig::substitute_env_vars(&mut val, &injected_env).unwrap();
        
        let bot_name = val.get("bot").unwrap().as_object().unwrap().get("name").unwrap().as_str().unwrap();
        let bot_persona = val.get("bot").unwrap().as_object().unwrap().get("systemPrompt").unwrap().as_str().unwrap();

        assert_eq!(bot_name, "Hello injected var");
        // Ensure escaping works
        assert_eq!(bot_persona, "Look at ${ESCAPED_VAR}");
    }

    #[test]
    fn test_include_array_and_max_depth() {
        let dir = tempdir().unwrap();
        let common1 = dir.path().join("common1.json5");
        let common2 = dir.path().join("common2.json5");
        let root = dir.path().join("root.json5");

        let mut f1 = File::create(&common1).unwrap();
        write!(f1, r#"{{ ai: {{ gcpLocation: "europe-west4" }} }}"#).unwrap();

        let mut f2 = File::create(&common2).unwrap();
        write!(f2, r#"{{ bot: {{ location: "ArrayBot" }} }}"#).unwrap();

        let mut f_root = File::create(&root).unwrap();
        write!(f_root, r#"{{ $include: ["./common1.json5", "./common2.json5"] }}"#).unwrap();

        let val = AppConfig::load_and_resolve_includes(&root, 0).unwrap();
        
        // Assert array includes merged correctly
        assert_eq!(val.get("ai").unwrap().as_object().unwrap().get("gcpLocation").unwrap().as_str().unwrap(), "europe-west4");
        assert_eq!(val.get("bot").unwrap().as_object().unwrap().get("location").unwrap().as_str().unwrap(), "ArrayBot");

        // Test max depth
        let mut loop_file = File::create(dir.path().join("loop.json5")).unwrap();
        write!(loop_file, r#"{{ $include: "./loop.json5" }}"#).unwrap();

        let res = AppConfig::load_and_resolve_includes(&dir.path().join("loop.json5"), 0);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("Max Depth"));
    }

    #[test]
    fn test_missing_env_var_error() {
        let json_str = r#"{ "database": { "url": "${MISSING_DB_URL_12345}" } }"#;
        let mut val: Value = json5::from_str(json_str).unwrap();
        let injected_env = std::collections::HashMap::new();

        let res = AppConfig::substitute_env_vars(&mut val, &injected_env);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("MissingEnvVarError: MISSING_DB_URL_12345"));
    }
}
