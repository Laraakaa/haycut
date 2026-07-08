use std::{fs, io};

use camino::Utf8Path;
use serde::{Deserialize, Serialize};

const CONFIG_FILE_NAME: &str = "haycut.toml";

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub token: TokenConfig,
    pub trace: TraceConfig,
    #[serde(default)]
    pub model: Option<ModelConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelConfig {
    /// Base URL of an OpenAI-compatible completions API, without trailing slash.
    /// Example: `https://api.openai.com/v1`
    pub base_url: String,
    /// Model identifier to pass in the request body.
    pub model: String,
    /// Name of the environment variable that holds the API key.
    pub api_key_env_var: String,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key_env_var: "OPENAI_API_KEY".to_string(),
            timeout_secs: 60,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TokenConfig {
    pub soft_budget: u32,
    pub hard_budget: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TraceConfig {
    pub max_output_bytes: u64,
    pub store_full_output: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            token: TokenConfig {
                soft_budget: 40_000,
                hard_budget: 80_000,
            },
            trace: TraceConfig {
                max_output_bytes: 1_000_000,
                store_full_output: true,
            },
            model: None,
        }
    }
}

impl Config {
    pub fn load_from_current_dir() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Utf8Path::new(CONFIG_FILE_NAME);

        Self::load_from_path(path)
    }

    fn load_from_path(path: &Utf8Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path)?;
        let config = toml::from_str(&contents)?;

        Ok(config)
    }

    pub fn default_toml() -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(&Self::default())
    }
}

pub fn create_default_config(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = Utf8Path::new(CONFIG_FILE_NAME);

    create_default_config_at(path, force)
}

fn create_default_config_at(
    path: &Utf8Path,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() && !force {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "haycut.toml already exists. Use --force to overwrite.",
        )));
    }

    let default_config = Config::default_toml()?;
    fs::write(path, default_config)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use camino::{Utf8Path, Utf8PathBuf};

    use super::*;

    fn test_config_path(name: &str) -> Utf8PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after Unix epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "haycut-{name}-{}-{timestamp}.toml",
            std::process::id()
        ));

        Utf8PathBuf::from_path_buf(path).expect("test path should be valid UTF-8")
    }

    fn remove_if_exists(path: &Utf8Path) {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to remove test file {path}: {error}"),
        }
    }

    #[test]
    fn default_config_has_sensible_values() {
        let config = Config::default();

        assert_eq!(config.token.soft_budget, 40_000);
        assert_eq!(config.token.hard_budget, 80_000);
        assert_eq!(config.trace.max_output_bytes, 1_000_000);
        assert!(config.trace.store_full_output);
    }

    #[test]
    fn default_toml_can_be_loaded_back() {
        let contents = Config::default_toml().expect("default config should serialize");
        let config: Config = toml::from_str(&contents).expect("default TOML should deserialize");

        assert_eq!(config.token.soft_budget, 40_000);
        assert_eq!(config.token.hard_budget, 80_000);
        assert_eq!(config.trace.max_output_bytes, 1_000_000);
        assert!(config.trace.store_full_output);
    }

    #[test]
    fn load_from_path_uses_defaults_when_file_is_missing() {
        let path = test_config_path("missing");
        remove_if_exists(&path);

        let config = Config::load_from_path(&path).expect("missing config should load defaults");

        assert_eq!(config.token.soft_budget, 40_000);
        assert_eq!(config.token.hard_budget, 80_000);
    }

    #[test]
    fn load_from_path_reads_existing_config() {
        let path = test_config_path("existing");
        remove_if_exists(&path);
        fs::write(
            &path,
            r#"
[token]
soft_budget = 10
hard_budget = 20

[trace]
max_output_bytes = 30
store_full_output = false
"#,
        )
        .expect("test config should be written");

        let config = Config::load_from_path(&path).expect("existing config should load");

        assert_eq!(config.token.soft_budget, 10);
        assert_eq!(config.token.hard_budget, 20);
        assert_eq!(config.trace.max_output_bytes, 30);
        assert!(!config.trace.store_full_output);

        remove_if_exists(&path);
    }

    #[test]
    fn create_default_config_refuses_to_overwrite_without_force() {
        let path = test_config_path("no-overwrite");
        remove_if_exists(&path);
        fs::write(&path, "existing config").expect("test config should be written");

        let error = create_default_config_at(&path, false).expect_err("existing file should fail");
        let contents = fs::read_to_string(&path).expect("existing config should remain readable");

        assert!(error.to_string().contains("already exists"));
        assert_eq!(contents, "existing config");

        remove_if_exists(&path);
    }

    #[test]
    fn create_default_config_overwrites_when_forced() {
        let path = test_config_path("force-overwrite");
        remove_if_exists(&path);
        fs::write(&path, "existing config").expect("test config should be written");

        create_default_config_at(&path, true).expect("force should overwrite existing config");
        let config = Config::load_from_path(&path).expect("forced config should be loadable");

        assert_eq!(config.token.soft_budget, 40_000);
        assert_eq!(config.token.hard_budget, 80_000);

        remove_if_exists(&path);
    }
}
