//! Repository-level configuration.
//!
//! Handles loading and parsing `.noctum.toml` configuration files from repositories.
//! This configuration controls which analysis features are enabled and defines
//! build/test commands for mutation testing.

use serde::Deserialize;
use std::path::Path;

/// Repository-level configuration loaded from `.noctum.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoConfig {
    /// Enable code analysis (File Analysis tab). Default: false.
    #[serde(default)]
    pub enable_code_analysis: bool,

    /// Enable architecture analysis (Architecture summary). Default: false.
    #[serde(default)]
    pub enable_architecture_analysis: bool,

    /// Enable diagram creation (system architecture, data flow, etc.). Default: false.
    #[serde(default)]
    pub enable_diagram_creation: bool,

    /// Enable mutation testing. Default: false.
    /// Note: Also requires mutation rules to be configured.
    #[serde(default)]
    pub enable_mutation_testing: bool,

    /// Mutation testing configuration.
    #[serde(default)]
    pub mutation: MutationRepoConfig,
}

/// Mutation testing configuration section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MutationRepoConfig {
    /// Rules for matching files to build/test commands.
    /// Rules are evaluated in order; the first matching glob wins.
    #[serde(default)]
    pub rules: Vec<MutationRule>,
}

/// A single mutation testing rule that maps a glob pattern to commands.
#[derive(Debug, Clone, Deserialize)]
pub struct MutationRule {
    /// Glob pattern to match file paths (e.g., `"**/*.rs"`, `"src/**/*.ts"`).
    pub glob: String,
    /// Command to run for build/compile checking.
    pub build_command: String,
    /// Command to run tests.
    pub test_command: String,
    /// Timeout in seconds for test execution (defaults to 300).
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    300
}

impl RepoConfig {
    /// Check if `.noctum.toml` exists in the repository.
    pub fn exists(repo_path: &Path) -> bool {
        repo_path.join(".noctum.toml").exists()
    }

    /// Load configuration from `.noctum.toml`.
    ///
    /// Returns `Some(config)` if the file exists and is valid (or empty).
    /// Returns `None` if the file doesn't exist.
    /// Returns `Some(default)` if the file is empty or contains only whitespace.
    pub fn load(repo_path: &Path) -> Option<Self> {
        let config_path = repo_path.join(".noctum.toml");
        if !config_path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&config_path).ok()?;
        // Empty file or whitespace-only returns default config
        if content.trim().is_empty() {
            return Some(Self::default());
        }
        toml::from_str(&content).ok()
    }
}

impl MutationRepoConfig {
    /// Find the first rule matching the given file path.
    ///
    /// Returns `None` if no rule matches, indicating the file should be skipped.
    #[allow(dead_code)]
    pub fn find_rule(&self, file_path: &str) -> Option<&MutationRule> {
        self.rules
            .iter()
            .find(|rule| glob_match::glob_match(&rule.glob, file_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_exists_returns_false_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        assert!(!RepoConfig::exists(temp_dir.path()));
    }

    #[test]
    fn test_exists_returns_true_when_present() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".noctum.toml"), "").unwrap();
        assert!(RepoConfig::exists(temp_dir.path()));
    }

    #[test]
    fn test_load_returns_none_when_missing() {
        let temp_dir = TempDir::new().unwrap();
        assert!(RepoConfig::load(temp_dir.path()).is_none());
    }

    #[test]
    fn test_load_empty_file_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".noctum.toml"), "").unwrap();

        let config = RepoConfig::load(temp_dir.path()).unwrap();
        assert!(config.mutation.rules.is_empty());
        // All enable flags default to false
        assert!(!config.enable_code_analysis);
        assert!(!config.enable_architecture_analysis);
        assert!(!config.enable_diagram_creation);
        assert!(!config.enable_mutation_testing);
    }

    #[test]
    fn test_load_whitespace_only_returns_default() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".noctum.toml"), "   \n\n  ").unwrap();

        let config = RepoConfig::load(temp_dir.path()).unwrap();
        assert!(config.mutation.rules.is_empty());
    }

    #[test]
    fn test_load_valid_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_content = r#"
[[mutation.rules]]
glob = "**/*.rs"
build_command = "cargo check"
test_command = "cargo test"

[[mutation.rules]]
glob = "**/*.ts"
build_command = "npm run build"
test_command = "npm test"
timeout_seconds = 600
"#;
        std::fs::write(temp_dir.path().join(".noctum.toml"), config_content).unwrap();

        let config = RepoConfig::load(temp_dir.path()).unwrap();
        assert_eq!(config.mutation.rules.len(), 2);

        let rust_rule = &config.mutation.rules[0];
        assert_eq!(rust_rule.glob, "**/*.rs");
        assert_eq!(rust_rule.build_command, "cargo check");
        assert_eq!(rust_rule.test_command, "cargo test");
        assert_eq!(rust_rule.timeout_seconds, 300); // default

        let ts_rule = &config.mutation.rules[1];
        assert_eq!(ts_rule.glob, "**/*.ts");
        assert_eq!(ts_rule.build_command, "npm run build");
        assert_eq!(ts_rule.test_command, "npm test");
        assert_eq!(ts_rule.timeout_seconds, 600); // custom
    }

    #[test]
    fn test_load_invalid_toml_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".noctum.toml"), "invalid {{{{ toml").unwrap();

        assert!(RepoConfig::load(temp_dir.path()).is_none());
    }

    #[test]
    fn test_find_rule_matches_simple_extension() {
        let config = MutationRepoConfig {
            rules: vec![MutationRule {
                glob: "**/*.rs".to_string(),
                build_command: "cargo check".to_string(),
                test_command: "cargo test".to_string(),
                timeout_seconds: 300,
            }],
        };

        assert!(config.find_rule("src/main.rs").is_some());
        assert!(config.find_rule("src/lib/utils.rs").is_some());
        assert!(config.find_rule("main.rs").is_some());
        assert!(config.find_rule("src/main.ts").is_none());
    }

    #[test]
    fn test_find_rule_matches_directory_prefix() {
        let config = MutationRepoConfig {
            rules: vec![MutationRule {
                glob: "packages/frontend/**/*.tsx".to_string(),
                build_command: "npm run build".to_string(),
                test_command: "npm test".to_string(),
                timeout_seconds: 300,
            }],
        };

        assert!(config.find_rule("packages/frontend/src/App.tsx").is_some());
        assert!(config.find_rule("packages/frontend/components/Button.tsx").is_some());
        assert!(config.find_rule("packages/backend/src/index.ts").is_none());
        assert!(config.find_rule("src/App.tsx").is_none());
    }

    #[test]
    fn test_find_rule_returns_first_match() {
        let config = MutationRepoConfig {
            rules: vec![
                MutationRule {
                    glob: "src/special/**/*.rs".to_string(),
                    build_command: "special check".to_string(),
                    test_command: "special test".to_string(),
                    timeout_seconds: 100,
                },
                MutationRule {
                    glob: "**/*.rs".to_string(),
                    build_command: "cargo check".to_string(),
                    test_command: "cargo test".to_string(),
                    timeout_seconds: 300,
                },
            ],
        };

        // Should match first rule
        let rule = config.find_rule("src/special/foo.rs").unwrap();
        assert_eq!(rule.test_command, "special test");

        // Should match second rule
        let rule = config.find_rule("src/other/bar.rs").unwrap();
        assert_eq!(rule.test_command, "cargo test");
    }

    #[test]
    fn test_find_rule_returns_none_when_no_match() {
        let config = MutationRepoConfig {
            rules: vec![MutationRule {
                glob: "**/*.rs".to_string(),
                build_command: "cargo check".to_string(),
                test_command: "cargo test".to_string(),
                timeout_seconds: 300,
            }],
        };

        assert!(config.find_rule("src/main.py").is_none());
        assert!(config.find_rule("package.json").is_none());
    }

    #[test]
    fn test_find_rule_empty_rules() {
        let config = MutationRepoConfig { rules: vec![] };
        assert!(config.find_rule("any/file.rs").is_none());
    }

    #[test]
    fn test_default_timeout() {
        assert_eq!(default_timeout(), 300);
    }

    #[test]
    fn test_load_with_enable_flags() {
        let temp_dir = TempDir::new().unwrap();
        let config_content = r#"
enable_code_analysis = true
enable_architecture_analysis = true
enable_diagram_creation = false
enable_mutation_testing = true

[[mutation.rules]]
glob = "**/*.rs"
build_command = "cargo check"
test_command = "cargo test"
"#;
        std::fs::write(temp_dir.path().join(".noctum.toml"), config_content).unwrap();

        let config = RepoConfig::load(temp_dir.path()).unwrap();
        assert!(config.enable_code_analysis);
        assert!(config.enable_architecture_analysis);
        assert!(!config.enable_diagram_creation);
        assert!(config.enable_mutation_testing);
        assert_eq!(config.mutation.rules.len(), 1);
    }

    #[test]
    fn test_load_partial_enable_flags() {
        let temp_dir = TempDir::new().unwrap();
        // Only set some flags, others should default to false
        let config_content = r#"
enable_code_analysis = true
"#;
        std::fs::write(temp_dir.path().join(".noctum.toml"), config_content).unwrap();

        let config = RepoConfig::load(temp_dir.path()).unwrap();
        assert!(config.enable_code_analysis);
        assert!(!config.enable_architecture_analysis);
        assert!(!config.enable_diagram_creation);
        assert!(!config.enable_mutation_testing);
    }
}
