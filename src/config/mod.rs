use anyhow::{Context, Result};
use chrono::Timelike;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Application configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// General settings
    #[serde(default)]
    pub general: GeneralConfig,

    /// Web server settings
    #[serde(default)]
    pub web: WebConfig,

    /// Ollama endpoints
    #[serde(default)]
    pub endpoints: Vec<OllamaEndpoint>,

    /// Schedule settings for when to run analysis
    #[serde(default)]
    pub schedule: ScheduleConfig,

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

/// An Ollama endpoint configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaEndpoint {
    /// Display name for this endpoint
    pub name: String,

    /// Ollama API URL
    pub url: String,

    /// Model to use for analysis
    pub model: String,

    /// Whether this endpoint is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Schedule configuration for when analysis runs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    /// Start hour (0-23) of the allowed window
    #[serde(default = "default_start_hour")]
    pub start_hour: u8,

    /// End hour (0-23) of the allowed window
    #[serde(default = "default_end_hour")]
    pub end_hour: u8,

    /// How often to check schedule (in seconds)
    #[serde(default = "default_check_interval")]
    pub check_interval_seconds: u64,
}

impl ScheduleConfig {
    /// Check if the current time is within the scheduled window
    pub fn is_in_window(&self) -> bool {
        let now = chrono::Local::now();
        let current_hour = now.hour() as u8;
        self.is_hour_in_window(current_hour)
    }

    /// Check if a specific hour is within the scheduled window (for testing)
    pub fn is_hour_in_window(&self, hour: u8) -> bool {
        if self.start_hour <= self.end_hour {
            // Normal range: e.g., 9-17 (9am to 5pm)
            hour >= self.start_hour && hour < self.end_hour
        } else {
            // Overnight range: e.g., 22-6 (10pm to 6am)
            hour >= self.start_hour || hour < self.end_hour
        }
    }
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

fn default_start_hour() -> u8 {
    22 // 10pm
}

fn default_end_hour() -> u8 {
    6 // 6am
}

fn default_check_interval() -> u64 {
    60 // Check every minute
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

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            start_hour: default_start_hour(),
            end_hour: default_end_hour(),
            check_interval_seconds: default_check_interval(),
        }
    }
}

impl Config {
    /// Load configuration from file, or create default if not found
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = path.map(PathBuf::from).or_else(Self::default_config_path);

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

    /// Save configuration to file
    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        let config_path = path
            .map(PathBuf::from)
            .or_else(Self::default_config_path)
            .context("No config path available")?;

        // Ensure parent directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        let contents = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(&config_path, contents)
            .with_context(|| format!("Failed to write config to {:?}", config_path))?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // ScheduleConfig tests
    // =========================================================================

    #[test]
    fn test_normal_range_inside() {
        // 9am to 5pm window
        let config = ScheduleConfig {
            start_hour: 9,
            end_hour: 17,
            check_interval_seconds: 60,
        };

        assert!(config.is_hour_in_window(9)); // Start hour is included
        assert!(config.is_hour_in_window(12)); // Middle of window
        assert!(config.is_hour_in_window(16)); // Near end
    }

    #[test]
    fn test_normal_range_outside() {
        // 9am to 5pm window
        let config = ScheduleConfig {
            start_hour: 9,
            end_hour: 17,
            check_interval_seconds: 60,
        };

        assert!(!config.is_hour_in_window(8)); // Before start
        assert!(!config.is_hour_in_window(17)); // End hour is excluded
        assert!(!config.is_hour_in_window(20)); // After end
        assert!(!config.is_hour_in_window(0)); // Midnight
    }

    #[test]
    fn test_overnight_range_inside() {
        // 10pm to 6am window (default)
        let config = ScheduleConfig {
            start_hour: 22,
            end_hour: 6,
            check_interval_seconds: 60,
        };

        assert!(config.is_hour_in_window(22)); // Start hour
        assert!(config.is_hour_in_window(23)); // Late night
        assert!(config.is_hour_in_window(0)); // Midnight
        assert!(config.is_hour_in_window(3)); // Early morning
        assert!(config.is_hour_in_window(5)); // Near end
    }

    #[test]
    fn test_overnight_range_outside() {
        // 10pm to 6am window
        let config = ScheduleConfig {
            start_hour: 22,
            end_hour: 6,
            check_interval_seconds: 60,
        };

        assert!(!config.is_hour_in_window(6)); // End hour is excluded
        assert!(!config.is_hour_in_window(12)); // Midday
        assert!(!config.is_hour_in_window(21)); // Just before start
        assert!(!config.is_hour_in_window(7)); // Just after end
    }

    #[test]
    fn test_same_hour_range() {
        // When start == end, window is empty (or could be interpreted as 24h)
        let config = ScheduleConfig {
            start_hour: 12,
            end_hour: 12,
            check_interval_seconds: 60,
        };

        // With current implementation, this means empty window
        assert!(!config.is_hour_in_window(12));
        assert!(!config.is_hour_in_window(0));
    }

    #[test]
    fn test_boundary_hours() {
        let config = ScheduleConfig {
            start_hour: 0,
            end_hour: 23,
            check_interval_seconds: 60,
        };

        assert!(config.is_hour_in_window(0)); // Start at midnight
        assert!(config.is_hour_in_window(12)); // Midday
        assert!(config.is_hour_in_window(22)); // Near end
        assert!(!config.is_hour_in_window(23)); // End hour excluded
    }

    // =========================================================================
    // Default value tests
    // =========================================================================

    #[test]
    fn test_default_schedule_config() {
        let config = ScheduleConfig::default();
        assert_eq!(config.start_hour, 22);
        assert_eq!(config.end_hour, 6);
        assert_eq!(config.check_interval_seconds, 60);
    }

    #[test]
    fn test_default_web_config() {
        let config = WebConfig::default();
        assert_eq!(config.port, 8420);
        assert_eq!(config.host, "127.0.0.1");
    }

    #[test]
    fn test_default_general_config() {
        let config = GeneralConfig::default();
        assert_eq!(config.log_level, "info");
    }

    // =========================================================================
    // Config parsing tests
    // =========================================================================

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[general]
log_level = "debug"

[web]
port = 9000
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.general.log_level, "debug");
        assert_eq!(config.web.port, 9000);
        // Defaults should still apply
        assert_eq!(config.web.host, "127.0.0.1");
        assert_eq!(config.schedule.start_hour, 22);
    }

    #[test]
    fn test_parse_endpoints() {
        let toml = r#"
[[endpoints]]
name = "Local"
url = "http://localhost:11434"
model = "llama2"
enabled = true

[[endpoints]]
name = "Remote"
url = "http://remote:11434"
model = "codellama"
enabled = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.endpoints.len(), 2);
        assert_eq!(config.endpoints[0].name, "Local");
        assert!(config.endpoints[0].enabled);
        assert_eq!(config.endpoints[1].name, "Remote");
        assert!(!config.endpoints[1].enabled);
    }

    #[test]
    fn test_parse_schedule() {
        let toml = r#"
[schedule]
start_hour = 1
end_hour = 5
check_interval_seconds = 120
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.schedule.start_hour, 1);
        assert_eq!(config.schedule.end_hour, 5);
        assert_eq!(config.schedule.check_interval_seconds, 120);
    }

    #[test]
    fn test_empty_config() {
        let toml = "";
        let config: Config = toml::from_str(toml).unwrap();
        // All defaults should apply
        assert_eq!(config.schedule.start_hour, 22);
        assert_eq!(config.schedule.end_hour, 6);
        assert!(config.endpoints.is_empty());
    }

    // =========================================================================
    // File I/O tests
    // =========================================================================

    #[test]
    fn test_config_load_nonexistent() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(temp_file.path()).unwrap();

        let config = Config::load(Some(temp_file.path())).unwrap();
        assert_eq!(config.schedule.start_hour, 22);
        assert_eq!(config.endpoints.len(), 0);
    }

    #[test]
    fn test_config_load_valid_file() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();

        let toml_content = r#"
[general]
log_level = "debug"

[web]
port = 9000

[schedule]
start_hour = 8
end_hour = 18
"#;

        std::fs::write(temp_file.path(), toml_content).unwrap();

        let config = Config::load(Some(temp_file.path())).unwrap();
        assert_eq!(config.general.log_level, "debug");
        assert_eq!(config.web.port, 9000);
        assert_eq!(config.schedule.start_hour, 8);
        assert_eq!(config.schedule.end_hour, 18);
    }

    #[test]
    fn test_config_load_invalid_toml() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();

        std::fs::write(temp_file.path(), "invalid {{{{ toml").unwrap();

        let result = Config::load(Some(temp_file.path()));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_save() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();

        let config = Config {
            general: GeneralConfig {
                log_level: "debug".to_string(),
            },
            web: WebConfig {
                port: 9000,
                host: "0.0.0.0".to_string(),
            },
            endpoints: vec![],
            schedule: ScheduleConfig {
                start_hour: 8,
                end_hour: 18,
                check_interval_seconds: 120,
            },
            data_dir: None,
        };

        config.save(Some(temp_file.path())).unwrap();

        let content = std::fs::read_to_string(temp_file.path()).unwrap();
        assert!(content.contains("log_level"));
        assert!(content.contains("port"));
        assert!(content.contains("start_hour"));
    }

    #[test]
    fn test_config_save_creates_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("subdir").join("config.toml");

        let config = Config::default();
        config.save(Some(&config_path)).unwrap();

        assert!(config_path.exists());
    }

    #[test]
    fn test_default_config_path() {
        let path = Config::default_config_path();
        assert!(path.is_some());
        assert!(path.unwrap().ends_with("config.toml"));
    }

    #[test]
    fn test_data_dir_with_custom() {
        let config = Config {
            data_dir: Some("/custom/path".into()),
            ..Default::default()
        };

        assert_eq!(config.data_dir(), PathBuf::from("/custom/path"));
    }

    #[test]
    fn test_data_dir_default() {
        let config = Config {
            data_dir: None,
            ..Default::default()
        };

        let path = config.data_dir();
        assert!(!path.as_os_str().is_empty());
    }

    #[test]
    fn test_database_path() {
        let config = Config {
            data_dir: Some("/test/data".into()),
            ..Default::default()
        };

        let db_path = config.database_path();
        assert_eq!(db_path, PathBuf::from("/test/data/noctum.db"));
    }
}
