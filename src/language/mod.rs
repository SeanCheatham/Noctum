//! Language abstraction layer for multi-language support.
//!
//! This module defines traits and implementations for language-specific operations
//! like finding source files, running tests, and generating analysis prompts.

mod rust;

use anyhow::Result;
use std::path::{Path, PathBuf};

pub use rust::RustLanguage;

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    // Future: Python, TypeScript, Go, etc.
}

impl Language {
    /// Detect the primary language of a repository by examining its contents.
    pub fn detect(repo_path: &Path) -> Option<Self> {
        // Check for language-specific marker files
        if repo_path.join("Cargo.toml").exists() {
            return Some(Language::Rust);
        }
        // Future: check for pyproject.toml, package.json, go.mod, etc.

        None
    }

    /// Human-readable name for the language.
    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "Rust",
        }
    }

    /// File extensions for this language.
    pub fn file_extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
        }
    }

    /// Directories to skip when scanning for source files.
    pub fn skip_directories(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["target", "node_modules", ".git"],
        }
    }

    /// Find all source files in a directory for this language.
    pub fn find_source_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        match self {
            Language::Rust => RustLanguage.find_source_files(dir),
        }
    }

    /// Run the test suite for a project.
    pub async fn run_tests(&self, repo_path: &Path, timeout_seconds: u64) -> TestRunResult {
        match self {
            Language::Rust => RustLanguage.run_tests(repo_path, timeout_seconds).await,
        }
    }

    /// Generate a prompt for code analysis.
    pub fn analysis_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.analysis_prompt(file_path, content),
        }
    }

    /// Generate a prompt for mutation generation.
    pub fn mutation_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.mutation_prompt(file_path, content),
        }
    }

    /// Minimum file size (bytes) for analysis.
    pub fn min_file_size(&self) -> usize {
        match self {
            Language::Rust => 50,
        }
    }

    /// Maximum file size (bytes) for analysis.
    pub fn max_file_size(&self) -> usize {
        match self {
            Language::Rust => 100_000,
        }
    }

    /// Minimum file size (bytes) for mutation testing.
    pub fn min_mutation_file_size(&self) -> usize {
        match self {
            Language::Rust => 100,
        }
    }

    /// Maximum file size (bytes) for mutation testing.
    pub fn max_mutation_file_size(&self) -> usize {
        match self {
            Language::Rust => 50_000,
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Result of running a test suite.
#[derive(Debug, Clone)]
pub struct TestRunResult {
    pub outcome: TestOutcome,
    /// The test that killed the mutation (if any).
    pub failing_test: Option<String>,
    /// Captured test output (may be truncated).
    pub output: Option<String>,
    /// How long the tests took to run.
    pub duration_ms: u64,
}

/// Outcome of running tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    /// All tests passed.
    Passed,
    /// At least one test failed.
    Failed,
    /// Tests timed out.
    Timeout,
    /// Code failed to compile.
    CompileError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_language_detect_rust() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("Cargo.toml"), "[package]").unwrap();

        let lang = Language::detect(temp_dir.path());
        assert_eq!(lang, Some(Language::Rust));
    }

    #[test]
    fn test_language_detect_unknown() {
        let temp_dir = TempDir::new().unwrap();
        let lang = Language::detect(temp_dir.path());
        assert_eq!(lang, None);
    }

    #[test]
    fn test_language_name() {
        assert_eq!(Language::Rust.name(), "Rust");
    }

    #[test]
    fn test_language_display() {
        assert_eq!(format!("{}", Language::Rust), "Rust");
    }

    #[test]
    fn test_language_file_extensions() {
        assert_eq!(Language::Rust.file_extensions(), &["rs"]);
    }

    #[test]
    fn test_language_skip_directories() {
        let skip = Language::Rust.skip_directories();
        assert!(skip.contains(&"target"));
        assert!(skip.contains(&".git"));
    }

    #[test]
    fn test_language_file_size_limits() {
        let lang = Language::Rust;
        assert!(lang.min_file_size() < lang.max_file_size());
        assert!(lang.min_mutation_file_size() < lang.max_mutation_file_size());
    }
}
