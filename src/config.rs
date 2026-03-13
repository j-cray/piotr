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

use serde_json::Value;
use std::path::Path;
use regex::Regex;

impl AppConfig {
    pub fn load() -> Result<Self> {
        // Find configuration path
        let config_path = Path::new("config.json5");
        
        // 1 & 2. Read config file and resolve $include directives
        let mut raw_value = Self::load_and_resolve_includes(config_path, 0)?;

        // 3. Apply env entries into process.env before substitution
        Self::apply_env_injection(&mut raw_value)?;

        // 4. Substitute ${VAR} references in string values
        Self::substitute_env_vars(&mut raw_value)?;

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

    fn apply_env_injection(value: &mut Value) -> Result<()> {
        if let Some(env_val) = value.get_mut("env") {
            if let Some(env_obj) = env_val.as_object_mut() {
                // vars map
                if let Some(vars_val) = env_obj.remove("vars") {
                    if let Some(vars_obj) = vars_val.as_object() {
                        for (k, v) in vars_obj {
                            if let Some(s) = v.as_str() {
                                unsafe { std::env::set_var(k, s) };
                            }
                        }
                    }
                }

                // other string fields
                let mut to_remove = Vec::new();
                for (k, v) in env_obj.iter() {
                    if k != "shellEnv" {
                        if let Some(s) = v.as_str() {
                            unsafe { std::env::set_var(k, s) };
                            to_remove.push(k.clone());
                        }
                    }
                }

                for k in to_remove {
                    env_obj.remove(&k);
                }
            }
        }
        Ok(())
    }

    fn substitute_env_vars(value: &mut Value) -> Result<()> {
        // Matches $${VAR} as escape or ${VAR} as variable
        let re = Regex::new(r"(?P<escape>\$\$)\{(?P<evar>[A-Z_][A-Z0-9_]*)\}|\$(?P<unescaped>\$)?\{(?P<var>[A-Z_][A-Z0-9_]*)\}").unwrap();
        Self::substitute_recursive(value, &re)
    }

    fn substitute_recursive(value: &mut Value, re: &Regex) -> Result<()> {
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
                        match std::env::var(var_name) {
                            Ok(val) => new_string.push_str(&val),
                            Err(_) => anyhow::bail!("MissingEnvVarError: {}", var_name),
                        }
                    }
                    last_end = m.end();
                }
                new_string.push_str(&s[last_end..]);
                *s = new_string;
            }
            Value::Object(map) => {
                for (_, v) in map.iter_mut() {
                    Self::substitute_recursive(v, re)?;
                }
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    Self::substitute_recursive(item, re)?;
                }
            }
            _ => {}
        }
        Ok(())
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
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

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

    #[test]
    fn test_include_resolution_and_merge() {
        let dir = tempdir().unwrap();
        let base_path = dir.path().join("base.json5");
        let root_path = dir.path().join("root.json5");

        let mut base_file = File::create(&base_path).unwrap();
        write!(base_file, r#"{{ bot: {{ name: "BaseBot", location: "BaseLoc" }}, performance: {{ max_concurrent_requests: 1 }} }}"#).unwrap();

        let mut root_file = File::create(&root_path).unwrap();
        write!(root_file, r#"{{ $include: "./base.json5", bot: {{ location: "RootLoc" }} }}"#).unwrap();

        let val = AppConfig::load_and_resolve_includes(&root_path, 0).expect("Failed to load and resolve includes");
        
        // Root location should override Base location, but name and performance should be preserved
        let bot_obj = val.get("bot").unwrap().as_object().unwrap();
        assert_eq!(bot_obj.get("name").unwrap().as_str().unwrap(), "BaseBot");
        assert_eq!(bot_obj.get("location").unwrap().as_str().unwrap(), "RootLoc");

        let perf_obj = val.get("performance").unwrap().as_object().unwrap();
        assert_eq!(perf_obj.get("max_concurrent_requests").unwrap().as_u64().unwrap(), 1);
    }

    #[test]
    fn test_env_injection_and_substitution() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("env_config.json5");

        // Clear it first to prevent local environment contamination from making it pass by accident
        unsafe {
            std::env::remove_var("TEST_SUBST_VAR");
            std::env::remove_var("ROOT_ENV_VAR");
        }

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
                persona: "Look at $${{ESCAPED_VAR}}"
            }}
        }}"#).unwrap();

        let mut val = AppConfig::load_and_resolve_includes(&config_path, 0).unwrap();
        
        // Apply injection
        AppConfig::apply_env_injection(&mut val).unwrap();
        
        // Assert env var is set
        assert_eq!(std::env::var("TEST_SUBST_VAR").unwrap(), "Hello injected var");
        assert_eq!(std::env::var("ROOT_ENV_VAR").unwrap(), "Root level injection");

        // Assert they are removed from the JSON tree
        let env_obj = val.get("env").unwrap().as_object().unwrap();
        assert!(env_obj.get("vars").is_none());
        assert!(env_obj.get("ROOT_ENV_VAR").is_none());

        // Process substitutions
        AppConfig::substitute_env_vars(&mut val).unwrap();
        
        let bot_name = val.get("bot").unwrap().as_object().unwrap().get("name").unwrap().as_str().unwrap();
        let bot_persona = val.get("bot").unwrap().as_object().unwrap().get("persona").unwrap().as_str().unwrap();

        assert_eq!(bot_name, "Hello injected var");
        // Ensure escaping works
        assert_eq!(bot_persona, "Look at ${ESCAPED_VAR}");
    }
}
