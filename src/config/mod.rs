//! Application configuration at `~/.config/noctum/config.toml`.
//!
//! The model and the QEMU bundle are lockfiles, not fields in this file.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local, Timelike};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub general: GeneralConfig,
    pub web: WebConfig,
    pub schedule: ScheduleConfig,
    pub gate: GateConfig,
    pub guest: GuestConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GeneralConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_author_name")]
    pub author_name: String,
    #[serde(default = "default_author_email")]
    pub author_email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScheduleConfig {
    #[serde(default = "default_start_hour")]
    pub start_hour: u8,
    #[serde(default = "default_end_hour")]
    pub end_hour: u8,
    #[serde(default = "default_check_interval")]
    pub check_interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GateConfig {
    #[serde(default = "default_ratio")]
    pub min_improvement_ratio: f64,
    #[serde(default = "default_delta")]
    pub min_insn_delta: u64,
    #[serde(default = "default_candidates")]
    pub max_candidates: u32,
    #[serde(default = "default_tests")]
    pub max_tests: u32,
    #[serde(default = "default_scan")]
    pub max_scan: u32,
    #[serde(default = "default_repeats")]
    pub measure_repeats: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GuestConfig {
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
}

fn default_log_level() -> String {
    "info".to_string()
}
fn default_author_name() -> String {
    "Noctum".to_string()
}
fn default_author_email() -> String {
    "noctum@localhost".to_string()
}
fn default_port() -> u16 {
    8420
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_start_hour() -> u8 {
    22
}
fn default_end_hour() -> u8 {
    6
}
fn default_check_interval() -> u64 {
    60
}
fn default_ratio() -> f64 {
    0.01
}
fn default_delta() -> u64 {
    10_000
}
fn default_candidates() -> u32 {
    3
}
fn default_tests() -> u32 {
    3
}
fn default_scan() -> u32 {
    20
}
fn default_repeats() -> u32 {
    2
}
fn default_memory() -> u32 {
    1024
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            author_name: default_author_name(),
            author_email: default_author_email(),
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
impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_improvement_ratio: default_ratio(),
            min_insn_delta: default_delta(),
            max_candidates: default_candidates(),
            max_tests: default_tests(),
            max_scan: default_scan(),
            measure_repeats: default_repeats(),
        }
    }
}
impl Default for GuestConfig {
    fn default() -> Self {
        Self {
            memory_mb: default_memory(),
        }
    }
}
impl ScheduleConfig {
    pub fn is_in_window(&self) -> bool {
        let hour = Local::now().hour() as u8;
        self.is_hour_in_window(hour)
    }

    pub fn is_hour_in_window(&self, hour: u8) -> bool {
        if self.start_hour <= self.end_hour {
            hour >= self.start_hour && hour < self.end_hour
        } else {
            hour >= self.start_hour || hour < self.end_hour
        }
    }

    /// Identity of the current window. An overnight window that is still open
    /// after midnight belongs to the previous evening, so a restart does not
    /// treat it as a new night.
    pub fn window_key_at(&self, now: DateTime<Local>) -> String {
        let hour = now.hour();
        let date = if self.start_hour > self.end_hour && hour < u32::from(self.end_hour) {
            now.date_naive() - Duration::days(1)
        } else {
            now.date_naive()
        };
        date.format("%Y-%m-%d").to_string()
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = path.map(PathBuf::from).or_else(Self::default_config_path);
        let config = if let Some(ref path) = config_path {
            if path.exists() {
                let contents = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read config from {}", path.display()))?;
                toml::from_str(&contents)
                    .with_context(|| format!("Failed to parse config from {}", path.display()))?
            } else {
                Config::default()
            }
        } else {
            Config::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        self.validate()?;
        let config_path = path
            .map(PathBuf::from)
            .or_else(Self::default_config_path)
            .context("No config path available")?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(&config_path, contents)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.schedule.start_hour > 23 || self.schedule.end_hour > 23 {
            bail!("schedule hours must be 0-23");
        }
        if self.web.port == 0 {
            bail!("web port must be non-zero");
        }
        if !self.gate.min_improvement_ratio.is_finite() || self.gate.min_improvement_ratio < 0.0 {
            bail!("min_improvement_ratio must be a non-negative finite number");
        }
        if self.gate.min_insn_delta == 0 {
            bail!("min_insn_delta must be at least 1");
        }
        if self.gate.measure_repeats == 0 {
            bail!("measure_repeats must be at least 1");
        }
        if self.gate.max_candidates == 0 || self.gate.max_tests == 0 || self.gate.max_scan == 0 {
            bail!("candidate and test budgets must be at least 1");
        }
        if self.guest.memory_mb < 128 {
            bail!("guest memory must be at least 128 MB");
        }
        Ok(())
    }

    pub fn default_config_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "noctum", "noctum")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_else(|| {
            ProjectDirs::from("com", "noctum", "noctum")
                .map(|dirs| dirs.data_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".noctum"))
        })
    }

    pub fn database_path(&self) -> PathBuf {
        self.data_dir().join("perf.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn schedule(start: u8, end: u8) -> ScheduleConfig {
        ScheduleConfig {
            start_hour: start,
            end_hour: end,
            check_interval_seconds: 60,
        }
    }

    #[test]
    fn daytime_window_includes_the_start_and_excludes_the_end() {
        let config = schedule(9, 17);
        assert!(config.is_hour_in_window(9));
        assert!(config.is_hour_in_window(16));
        assert!(!config.is_hour_in_window(8));
        assert!(!config.is_hour_in_window(17));
    }

    #[test]
    fn overnight_window_wraps_past_midnight() {
        let config = schedule(22, 6);
        assert!(config.is_hour_in_window(22));
        assert!(config.is_hour_in_window(0));
        assert!(config.is_hour_in_window(5));
        assert!(!config.is_hour_in_window(6));
        assert!(!config.is_hour_in_window(12));
        assert!(!config.is_hour_in_window(21));
    }

    #[test]
    fn equal_hours_are_an_empty_window() {
        let config = schedule(12, 12);
        assert!(!config.is_hour_in_window(12));
        assert!(!config.is_hour_in_window(0));
    }

    #[test]
    fn defaults_match_the_plan() {
        let config = Config::default();
        assert_eq!(config.schedule.start_hour, 22);
        assert_eq!(config.schedule.end_hour, 6);
        assert_eq!(config.schedule.check_interval_seconds, 60);
        assert_eq!(config.web.port, 8420);
        assert_eq!(config.web.host, "127.0.0.1");
        assert_eq!(config.gate.min_improvement_ratio, 0.01);
        assert_eq!(config.gate.min_insn_delta, 10_000);
        assert_eq!(config.gate.max_candidates, 3);
        assert_eq!(config.gate.max_tests, 3);
        assert_eq!(config.gate.max_scan, 20);
        assert_eq!(config.gate.measure_repeats, 2);
        assert_eq!(config.guest.memory_mb, 1024);
        assert_eq!(config.general.author_name, "Noctum");
        assert_eq!(config.general.author_email, "noctum@localhost");
    }

    #[test]
    fn old_endpoint_list_is_a_parse_error() {
        let toml = r#"
[[endpoints]]
name = "Local"
url = "http://localhost:11434"
model = "qwen2.5-coder"
"#;
        let error = toml::from_str::<Config>(toml).unwrap_err().to_string();
        assert!(error.contains("endpoints"), "{error}");
    }

    #[test]
    fn database_file_is_perf_db() {
        let config = Config {
            data_dir: Some("/test/data".into()),
            ..Config::default()
        };
        assert_eq!(config.database_path(), PathBuf::from("/test/data/perf.db"));
    }

    #[test]
    fn overnight_window_key_belongs_to_the_evening_it_started() {
        let config = schedule(22, 6);
        let evening = Local.with_ymd_and_hms(2026, 11, 20, 23, 0, 0).unwrap();
        let morning = Local.with_ymd_and_hms(2026, 11, 21, 2, 0, 0).unwrap();
        assert_eq!(config.window_key_at(evening), "2026-11-20");
        assert_eq!(config.window_key_at(morning), "2026-11-20");
    }

    #[test]
    fn round_trip_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::default();
        config.save(Some(&path)).unwrap();
        let loaded = Config::load(Some(&path)).unwrap();
        assert_eq!(loaded.gate.max_candidates, 3);
        assert_eq!(loaded.web.host, "127.0.0.1");
    }
}
