//! `noctum.toml` for one repository.
//!
//! Unknown keys are an error, so a leftover mutation config fails loudly.
//! The file can name a shell command, so it must be owned by the current user
//! and must not be world-writable.

use serde::Deserialize;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSecurityCheck {
    Safe,
    WrongOwner {
        expected_uid: u32,
        actual_uid: u32,
    },
    WorldWritable,
    MetadataError(String),
    #[allow(dead_code)]
    NotSupported,
}

impl ConfigSecurityCheck {
    pub fn is_safe(&self) -> bool {
        matches!(
            self,
            ConfigSecurityCheck::Safe | ConfigSecurityCheck::NotSupported
        )
    }
}

impl fmt::Display for ConfigSecurityCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigSecurityCheck::Safe => write!(f, "ok"),
            ConfigSecurityCheck::WrongOwner {
                expected_uid,
                actual_uid,
            } => write!(
                f,
                "noctum.toml is owned by uid {actual_uid}, current user is uid {expected_uid}"
            ),
            ConfigSecurityCheck::WorldWritable => {
                write!(f, "noctum.toml is world-writable")
            }
            ConfigSecurityCheck::MetadataError(err) => {
                write!(f, "could not read noctum.toml metadata: {err}")
            }
            ConfigSecurityCheck::NotSupported => write!(f, "ownership checks are not available"),
        }
    }
}

#[cfg(unix)]
pub fn check_config_security(config_path: &Path) -> ConfigSecurityCheck {
    use std::os::unix::fs::MetadataExt;

    let metadata = match std::fs::metadata(config_path) {
        Ok(metadata) => metadata,
        Err(err) => return ConfigSecurityCheck::MetadataError(err.to_string()),
    };
    let Some(current_uid) = current_uid() else {
        return ConfigSecurityCheck::MetadataError("could not determine current user id".into());
    };
    let file_uid = metadata.uid();
    if file_uid != current_uid {
        return ConfigSecurityCheck::WrongOwner {
            expected_uid: current_uid,
            actual_uid: file_uid,
        };
    }
    if metadata.mode() & 0o002 != 0 {
        return ConfigSecurityCheck::WorldWritable;
    }
    ConfigSecurityCheck::Safe
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::NamedTempFile::new().ok()?;
    Some(temp.path().metadata().ok()?.uid())
}

#[cfg(not(unix))]
pub fn check_config_security(_config_path: &Path) -> ConfigSecurityCheck {
    ConfigSecurityCheck::NotSupported
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RepoConfig {
    pub package: Option<String>,
    pub tests: Vec<String>,
    pub skip_tests: Vec<String>,
    pub benches: Vec<String>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub protected_paths: Vec<String>,
    pub check_command: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_clean_paths")]
    pub clean_paths: Vec<String>,
}

fn default_timeout() -> u64 {
    600
}

fn default_clean_paths() -> Vec<String> {
    vec!["target".to_string()]
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            package: None,
            tests: Vec::new(),
            skip_tests: Vec::new(),
            benches: Vec::new(),
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            protected_paths: Vec::new(),
            check_command: String::new(),
            timeout_seconds: 600,
            clean_paths: vec!["target".to_string()],
        }
    }
}

#[derive(Debug)]
pub enum RepoError {
    Missing,
    Insecure(String),
    Parse(String),
    Io(String),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoError::Missing => write!(f, "repository has no noctum.toml"),
            RepoError::Insecure(err) => write!(f, "{err}"),
            RepoError::Parse(err) => write!(f, "failed to parse noctum.toml: {err}"),
            RepoError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RepoError {}

impl RepoConfig {
    pub fn exists(repo_path: &Path) -> bool {
        repo_path.join("noctum.toml").exists()
    }

    pub fn load(repo_path: &Path) -> Result<Self, RepoError> {
        Self::load_at(&repo_path.join("noctum.toml"), true)
    }

    #[cfg(test)]
    pub fn load_unchecked(repo_path: &Path) -> Result<Self, RepoError> {
        Self::load_at(&repo_path.join("noctum.toml"), false)
    }

    fn load_at(config_path: &Path, check_security: bool) -> Result<Self, RepoError> {
        if !config_path.exists() {
            return Err(RepoError::Missing);
        }
        if check_security {
            let check = check_config_security(config_path);
            if !check.is_safe() {
                return Err(RepoError::Insecure(check.to_string()));
            }
        }
        let content = std::fs::read_to_string(config_path)
            .map_err(|err| RepoError::Io(format!("read {}: {err}", config_path.display())))?;
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        toml::from_str(&content).map_err(|err| RepoError::Parse(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn empty_file_uses_plan_defaults() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("noctum.toml"), "   \n").unwrap();
        let config = RepoConfig::load_unchecked(dir.path()).unwrap();
        assert!(config.tests.is_empty());
        assert!(config.benches.is_empty());
        assert_eq!(config.timeout_seconds, 600);
        assert_eq!(config.clean_paths, vec!["target".to_string()]);
        assert!(!config.all_features);
    }

    #[test]
    fn parses_the_documented_shape() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("noctum.toml"),
            r#"
package = "demo"
tests = ["encode_batch"]
skip_tests = ["*_io_*"]
protected_paths = ["fixtures/**"]
timeout_seconds = 120
"#,
        )
        .unwrap();
        let config = RepoConfig::load_unchecked(dir.path()).unwrap();
        assert_eq!(config.package.as_deref(), Some("demo"));
        assert_eq!(config.tests, vec!["encode_batch".to_string()]);
        assert_eq!(config.skip_tests, vec!["*_io_*".to_string()]);
        assert_eq!(config.timeout_seconds, 120);
        assert_eq!(config.clean_paths, vec!["target".to_string()]);
    }

    #[test]
    fn mutation_keys_are_named_in_the_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("noctum.toml"),
            "enable_mutation_testing = true\n",
        )
        .unwrap();
        let error = RepoConfig::load_unchecked(dir.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("enable_mutation_testing"), "{error}");
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            RepoConfig::load(dir.path()),
            Err(RepoError::Missing)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_file_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("noctum.toml");
        std::fs::write(&path, "package = \"demo\"\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o666);
        std::fs::set_permissions(&path, perms).unwrap();
        let error = RepoConfig::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains("world-writable"), "{error}");
    }
}
