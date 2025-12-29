//! Language abstraction layer for multi-language support.
//!
//! This module defines traits and implementations for language-specific operations
//! like finding source files, running tests, and generating analysis prompts.

#![allow(dead_code)]

mod rust;
mod typescript;

use anyhow::Result;
use std::path::{Path, PathBuf};

pub use rust::RustLanguage;
pub use typescript::TypeScriptLanguage;

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
}

impl Language {
    /// Detect the primary language of a repository by examining its contents.
    pub fn detect(repo_path: &Path) -> Option<Self> {
        // Check for language-specific marker files
        if repo_path.join("Cargo.toml").exists() {
            return Some(Language::Rust);
        }
        if repo_path.join("package.json").exists() {
            return Some(Language::TypeScript);
        }

        None
    }

    /// Human-readable name for the language.
    pub fn name(&self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::TypeScript => "TypeScript",
        }
    }

    /// File extensions for this language.
    pub fn file_extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["rs"],
            Language::TypeScript => &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        }
    }

    /// Directories to skip when scanning for source files.
    pub fn skip_directories(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["target", "node_modules", ".git"],
            Language::TypeScript => &["node_modules", ".git", "dist", "build", ".next", "coverage"],
        }
    }

    /// Find all source files in a directory for this language.
    pub fn find_source_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        match self {
            Language::Rust => RustLanguage.find_source_files(dir),
            Language::TypeScript => TypeScriptLanguage.find_source_files(dir),
        }
    }

    /// Run a compile check to verify compilation without running tests.
    ///
    /// Returns `Ok(())` if compilation succeeds, `Err(error_output)` if it fails.
    pub async fn compile_check(
        &self,
        repo_path: &Path,
        timeout_seconds: u64,
    ) -> Result<(), String> {
        match self {
            Language::Rust => RustLanguage.compile_check(repo_path, timeout_seconds).await,
            Language::TypeScript => {
                TypeScriptLanguage
                    .compile_check(repo_path, timeout_seconds)
                    .await
            }
        }
    }

    /// Run the test suite for a project.
    pub async fn run_tests(&self, repo_path: &Path, timeout_seconds: u64) -> TestRunResult {
        match self {
            Language::Rust => RustLanguage.run_tests(repo_path, timeout_seconds).await,
            Language::TypeScript => {
                TypeScriptLanguage
                    .run_tests(repo_path, timeout_seconds)
                    .await
            }
        }
    }

    /// Generate a prompt for code analysis.
    pub fn analysis_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.analysis_prompt(file_path, content),
            Language::TypeScript => TypeScriptLanguage.analysis_prompt(file_path, content),
        }
    }

    /// Generate a prompt for mutation generation.
    pub fn mutation_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.mutation_prompt(file_path, content),
            Language::TypeScript => TypeScriptLanguage.mutation_prompt(file_path, content),
        }
    }

    /// Minimum file size (bytes) for analysis.
    pub fn min_file_size(&self) -> usize {
        match self {
            Language::Rust => 50,
            Language::TypeScript => 50,
        }
    }

    /// Maximum file size (bytes) for analysis.
    pub fn max_file_size(&self) -> usize {
        match self {
            Language::Rust => 100_000,
            Language::TypeScript => 100_000,
        }
    }

    /// Minimum file size (bytes) for mutation testing.
    pub fn min_mutation_file_size(&self) -> usize {
        match self {
            Language::Rust => 100,
            Language::TypeScript => 100,
        }
    }

    /// Maximum file size (bytes) for mutation testing.
    pub fn max_mutation_file_size(&self) -> usize {
        match self {
            Language::Rust => 50_000,
            Language::TypeScript => 50_000,
        }
    }

    /// Find context files (documentation, config) in a directory.
    pub fn find_context_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        match self {
            Language::Rust => RustLanguage.find_context_files(dir),
            Language::TypeScript => TypeScriptLanguage.find_context_files(dir),
        }
    }

    /// Generate a prompt for documentation/context file analysis.
    pub fn documentation_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.documentation_prompt(file_path, content),
            Language::TypeScript => TypeScriptLanguage.documentation_prompt(file_path, content),
        }
    }

    /// Generate a prompt for architecture-focused file analysis.
    pub fn architecture_file_analysis_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.architecture_file_analysis_prompt(file_path, content),
            Language::TypeScript => {
                TypeScriptLanguage.architecture_file_analysis_prompt(file_path, content)
            }
        }
    }

    /// Generate a prompt for diagram architecture extraction.
    pub fn diagram_architecture_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.diagram_architecture_prompt(file_path, content),
            Language::TypeScript => {
                TypeScriptLanguage.diagram_architecture_prompt(file_path, content)
            }
        }
    }

    /// Generate a prompt for diagram data flow extraction.
    pub fn diagram_data_flow_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.diagram_data_flow_prompt(file_path, content),
            Language::TypeScript => TypeScriptLanguage.diagram_data_flow_prompt(file_path, content),
        }
    }

    /// Generate a prompt for diagram database schema extraction.
    pub fn diagram_database_schema_prompt(&self, file_path: &str, content: &str) -> String {
        match self {
            Language::Rust => RustLanguage.diagram_database_schema_prompt(file_path, content),
            Language::TypeScript => {
                TypeScriptLanguage.diagram_database_schema_prompt(file_path, content)
            }
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
    fn test_language_detect_typescript() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("package.json"), "{}").unwrap();

        let lang = Language::detect(temp_dir.path());
        assert_eq!(lang, Some(Language::TypeScript));
    }

    #[test]
    fn test_language_detect_rust_takes_precedence() {
        // If both Cargo.toml and package.json exist, Rust takes precedence
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(temp_dir.path().join("package.json"), "{}").unwrap();

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
        assert_eq!(Language::TypeScript.name(), "TypeScript");
    }

    #[test]
    fn test_language_display() {
        assert_eq!(format!("{}", Language::Rust), "Rust");
        assert_eq!(format!("{}", Language::TypeScript), "TypeScript");
    }

    #[test]
    fn test_language_file_extensions() {
        assert_eq!(Language::Rust.file_extensions(), &["rs"]);
        assert!(Language::TypeScript.file_extensions().contains(&"ts"));
        assert!(Language::TypeScript.file_extensions().contains(&"tsx"));
        assert!(Language::TypeScript.file_extensions().contains(&"js"));
    }

    #[test]
    fn test_language_skip_directories() {
        let rust_skip = Language::Rust.skip_directories();
        assert!(rust_skip.contains(&"target"));
        assert!(rust_skip.contains(&".git"));

        let ts_skip = Language::TypeScript.skip_directories();
        assert!(ts_skip.contains(&"node_modules"));
        assert!(ts_skip.contains(&".git"));
        assert!(ts_skip.contains(&"dist"));
    }

    #[test]
    fn test_language_file_size_limits() {
        for lang in [Language::Rust, Language::TypeScript] {
            assert!(lang.min_file_size() < lang.max_file_size());
            assert!(lang.min_mutation_file_size() < lang.max_mutation_file_size());
        }
    }
}
