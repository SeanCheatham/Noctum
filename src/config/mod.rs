use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// General settings
    #[serde(default)]
    pub general: GeneralConfig,

    /// Web server settings
    #[serde(default)]
    pub web: WebConfig,

    /// Ollama settings
    #[serde(default)]
    pub ollama: OllamaConfig,

    /// Idle detection settings
    #[serde(default)]
    pub idle: IdleConfig,

    /// Data directory (where database and logs are stored)
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// Port for the web server
    #[serde(default = "default_port")]
    pub port: u16,

    /// Host to bind to
    #[serde(default = "default_host")]
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// Ollama API URL
    #[serde(default = "default_ollama_url")]
    pub url: String,

    /// Model to use for analysis
    #[serde(default = "default_model")]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleConfig {
    /// Minimum idle time (in seconds) before starting background work
    #[serde(default = "default_idle_threshold")]
    pub threshold_seconds: u64,

    /// How often to check for idle status (in seconds)
    #[serde(default = "default_check_interval")]
    pub check_interval_seconds: u64,
}

// Default value functions
fn default_log_level() -> String {
    "info".to_string()
}

fn default_port() -> u16 {
    8420
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_model() -> String {
    "codellama".to_string()
}

fn default_idle_threshold() -> u64 {
    300 // 5 minutes
}

fn default_check_interval() -> u64 {
    30
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
        }
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
        }
    }
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            url: default_ollama_url(),
            model: default_model(),
        }
    }
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            threshold_seconds: default_idle_threshold(),
            check_interval_seconds: default_check_interval(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            web: WebConfig::default(),
            ollama: OllamaConfig::default(),
            idle: IdleConfig::default(),
            data_dir: None,
        }
    }
}

impl Config {
    /// Load configuration from file, or create default if not found
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = path
            .map(PathBuf::from)
            .or_else(Self::default_config_path);

        let config = if let Some(ref path) = config_path {
            if path.exists() {
                let contents = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read config from {:?}", path))?;
                toml::from_str(&contents)
                    .with_context(|| format!("Failed to parse config from {:?}", path))?
            } else {
                Config::default()
            }
        } else {
            Config::default()
        };

        Ok(config)
    }

    /// Get the default configuration file path
    pub fn default_config_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "noctum", "noctum")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Get the data directory path
    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_else(|| {
            ProjectDirs::from("com", "noctum", "noctum")
                .map(|dirs| dirs.data_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".noctum"))
        })
    }

    /// Get the database file path
    pub fn database_path(&self) -> PathBuf {
        self.data_dir().join("noctum.db")
    }
}
