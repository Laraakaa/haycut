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
    /// Optional explicit cheap model for deterministic weak-model steps.
    /// Falls back to `model`.
    #[serde(default)]
    pub weak_model: Option<ModelConfig>,
    /// Optional explicit capable model for planning and patch generation.
    /// Falls back to `model`.
    #[serde(default)]
    pub strong_model: Option<ModelConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelConfig {
    /// Base URL of an OpenAI-compatible completions API, without trailing slash.
    /// Example: `https://api.openai.com/v1`
    pub base_url: String,
    /// Model identifier to pass in the request body.
    pub model: String,
    /// Name of the environment variable that holds the API key.
    /// Ignored if `api_key` is set.
    #[serde(default)]
    pub api_key_env_var: Option<String>,
    /// API key value directly in the config file. Takes precedence over
    /// `api_key_env_var` if both are set. Less secure than an environment
    /// variable, but convenient for keys that only reach a local proxy.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            api_key_env_var: Some("OPENAI_API_KEY".to_string()),
            api_key: None,
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
            weak_model: None,
            strong_model: None,
        }
    }
}

/// Thin user-level config containing only the keys a user would set once
/// for their whole machine (primarily model configuration). Stored at the
/// platform config dir, e.g. `~/.config/haycut/config.toml` on Linux/macOS.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct UserConfig {
    #[serde(default)]
    pub model: Option<ModelConfig>,
    #[serde(default)]
    pub weak_model: Option<ModelConfig>,
    #[serde(default)]
    pub strong_model: Option<ModelConfig>,
}

impl UserConfig {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        match user_config_path() {
            Some(path) => Self::load_from_path(&path),
            None => Ok(Self::default()),
        }
    }

    fn load_from_path(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }

    pub fn default_toml() -> String {
        "# HayCut user config — applies to every project on this machine.\n\
         # https://github.com/Laraakaa/haycut\n\
         #\n\
         # Configure the model used by `haycut agent`.\n\
         # Remove the leading `#` characters and fill in your values.\n\
         #\n\
         # Provide the API key either via an environment variable\n\
         # (api_key_env_var) or directly in this file (api_key). If both are\n\
         # set, api_key takes precedence. A direct value is less secure but\n\
         # convenient for local-only keys (e.g. a local LLM proxy).\n\
         #\n\
         # [model]\n\
         # base_url = \"https://api.openai.com/v1\"\n\
         # model = \"gpt-4o-mini\"\n\
         # api_key_env_var = \"OPENAI_API_KEY\"\n\
         # # api_key = \"sk-...\"\n\
         # timeout_secs = 60\n\
         #\n\
         # Optional explicit cheap model for weak-model steps.\n\
         # Falls back to [model].\n\
         #\n\
         # [weak_model]\n\
         # base_url = \"https://api.openai.com/v1\"\n\
         # model = \"gpt-4o-mini\"\n\
         # api_key_env_var = \"OPENAI_API_KEY\"\n\
         # # api_key = \"sk-...\"\n\
         # timeout_secs = 60\n\
         #\n\
         # weak_model can also point at a local, OpenAI-compatible endpoint\n\
         # such as Ollama. No API key is required for local endpoints.\n\
         #\n\
         # [weak_model]\n\
         # base_url = \"http://localhost:11434/v1\"\n\
         # model = \"qwen2.5:7b-instruct\"\n\
         # timeout_secs = 60\n\
         #\n\
         # Optional explicit capable model for planning/patch generation.\n\
         # Falls back to [model].\n\
         #\n\
         # [strong_model]\n\
         # base_url = \"https://api.openai.com/v1\"\n\
         # model = \"gpt-4o\"\n\
         # api_key_env_var = \"OPENAI_API_KEY\"\n\
         # # api_key = \"sk-...\"\n\
         # timeout_secs = 120\n"
            .to_string()
    }

    /// Create the user config file if it does not already exist.
    /// The parent directory is created automatically.
    /// Returns the path that was created, or `None` if the path could not be
    /// determined or the file already existed.
    pub fn create_if_missing() -> Result<Option<std::path::PathBuf>, Box<dyn std::error::Error>> {
        let Some(path) = user_config_path() else {
            return Ok(None);
        };
        if path.exists() {
            return Ok(None);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, Self::default_toml())?;
        Ok(Some(path))
    }

    /// Resolved path for the user config file, if the platform dirs can be
    /// determined.
    pub fn path() -> Option<std::path::PathBuf> {
        user_config_path()
    }
}

fn user_config_path() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            std::path::PathBuf::from(home)
                .join(".config")
                .join("haycut")
                .join("config.toml"),
        )
    }
    #[cfg(not(unix))]
    {
        directories::ProjectDirs::from("", "", "haycut")
            .map(|dirs| dirs.config_dir().join("config.toml"))
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

    /// Load model configuration: project config `[model]` wins if present,
    /// otherwise falls back to the user-level config.
    /// Returns `None` when neither source provides `[model]`.
    pub fn load_model() -> Result<Option<ModelConfig>, Box<dyn std::error::Error>> {
        let project_model = Self::load_from_current_dir()?.model;
        if project_model.is_some() {
            return Ok(project_model);
        }
        Ok(UserConfig::load()?.model)
    }

    /// Load the weak model: `[weak_model]` from project then user config,
    /// falling back to the main model.
    pub fn load_weak_model() -> Result<Option<ModelConfig>, Box<dyn std::error::Error>> {
        let project = Self::load_from_current_dir()?.weak_model;
        if project.is_some() {
            return Ok(project);
        }
        if let Some(user_weak) = UserConfig::load()?.weak_model {
            return Ok(Some(user_weak));
        }
        Self::load_model()
    }

    /// Load the strong model: `[strong_model]` from project then user config,
    /// falling back to the main model.
    pub fn load_strong_model() -> Result<Option<ModelConfig>, Box<dyn std::error::Error>> {
        let project = Self::load_from_current_dir()?.strong_model;
        if project.is_some() {
            return Ok(project);
        }
        if let Some(user_strong) = UserConfig::load()?.strong_model {
            return Ok(Some(user_strong));
        }
        Self::load_model()
    }

    pub fn default_toml() -> Result<String, toml::ser::Error> {
        // Prepend a short comment header to the serialised TOML.
        let body = toml::to_string_pretty(&Self::default())?;
        Ok(format!(
            "# HayCut project config\n\
             # https://github.com/Laraakaa/haycut\n\
             #\n\
             # Model configuration lives in the user config file, not here.\n\
             # Run `haycut init` to see where that file is located.\n\n{body}"
        ))
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
