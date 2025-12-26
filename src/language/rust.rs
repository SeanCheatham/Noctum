//! Rust language support.

use super::{TestOutcome, TestRunResult};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;

/// Rust language handler.
pub struct RustLanguage;

impl RustLanguage {
    pub fn find_source_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        let skip_dirs: &[&str] = &["target", "node_modules", ".git"];

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.') && !skip_dirs.contains(&name.as_ref())
            })
        {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path.to_path_buf());
            }
        }

        Ok(files)
    }

    pub async fn run_tests(&self, repo_path: &Path, timeout_seconds: u64) -> TestRunResult {
        let start = Instant::now();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds),
            Command::new("cargo")
                .arg("test")
                .arg("--")
                .arg("--test-threads=1")
                .current_dir(repo_path)
                .output(),
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = format!("{}\n{}", stdout, stderr);
                let truncated = truncate_output(&combined, 10_000);

                if output.status.success() {
                    TestRunResult {
                        outcome: TestOutcome::Passed,
                        failing_test: None,
                        output: Some(truncated),
                        duration_ms,
                    }
                } else {
                    // Check if it's a compile error
                    if stderr.contains("error[E") || stderr.contains("could not compile") {
                        TestRunResult {
                            outcome: TestOutcome::CompileError,
                            failing_test: None,
                            output: Some(truncated),
                            duration_ms,
                        }
                    } else {
                        TestRunResult {
                            outcome: TestOutcome::Failed,
                            failing_test: extract_failing_test(&stdout),
                            output: Some(truncated),
                            duration_ms,
                        }
                    }
                }
            }
            Ok(Err(e)) => TestRunResult {
                outcome: TestOutcome::CompileError,
                failing_test: None,
                output: Some(format!("Failed to execute cargo test: {}", e)),
                duration_ms,
            },
            Err(_) => TestRunResult {
                outcome: TestOutcome::Timeout,
                failing_test: None,
                output: Some(format!("Test timed out after {} seconds", timeout_seconds)),
                duration_ms,
            },
        }
    }

    pub fn analysis_prompt(&self, file_path: &str, content: &str) -> String {
        format!(
            "Analyze the following Rust code and provide a brief summary of what it does:\n\n\
             File: {}\n\n\
             ```rust\n{}\n```\n\n\
             Provide a concise analysis including:\n\
             1. Purpose of the code\n\
             2. Key functions/structs\n\
             3. Any potential issues or improvements\n\
             4. Up to two specific code modification recommendations\n\n\
             IMPORTANT: Respond only in English (or code)",
            file_path, content
        )
    }

    pub fn mutation_prompt(&self, file_path: &str, content: &str) -> String {
        let numbered_code = add_line_numbers(content);
        format!(
            r#"You are a mutation testing expert. Analyze this Rust code and generate up to 3 small, targeted mutations.

VALID mutation types:
- Comparison operators: > to >=, < to <=, == to !=, etc.
- Boolean literals: true to false, false to true
- Arithmetic operators: + to -, * to /, etc.
- Boundary values: n to n+1, n to n-1
- Return values: Ok(x) to Err(...), Some(x) to None
- Numeric constants: 0 to 1, 1 to 0

RULES:
- The "find" text must be copied EXACTLY from the code (same spacing, same characters)
- The "replace" text should differ by only ONE small change
- Skip comments, imports, type definitions, and test code

File: {file_path}

```
{numbered_code}
```

For each mutation provide:
- line_number: The line where this expression appears
- find: The EXACT text to find (copy it precisely from the code above)
- replace: The modified text
- reasoning: Why this tests important logic
- description: What changed (e.g., "Changed > to >=")

Example for line `   42 |     if count > 0 {{`:
  line_number: 42
  find: "count > 0"
  replace: "count >= 0"
  description: "Changed > to >=""#
        )
    }
}

/// Add line numbers to code for better LLM alignment.
fn add_line_numbers(code: &str) -> String {
    code.lines()
        .enumerate()
        .map(|(i, line)| format!("{:4} | {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Truncate output to a maximum length.
fn truncate_output(output: &str, max_len: usize) -> String {
    if output.len() <= max_len {
        output.to_string()
    } else {
        format!("{}...(truncated)", &output[..max_len])
    }
}

/// Extract the name of the first failing test from cargo test output.
fn extract_failing_test(output: &str) -> Option<String> {
    for line in output.lines() {
        if line.starts_with("---- ") && line.ends_with(" stdout ----") {
            let test_name = line
                .trim_start_matches("---- ")
                .trim_end_matches(" stdout ----");
            return Some(test_name.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_source_files_empty() {
        let temp_dir = TempDir::new().unwrap();
        let handler = RustLanguage;
        let files = handler.find_source_files(temp_dir.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_find_source_files_with_rust_files() {
        let temp_dir = TempDir::with_prefix("test_rust").unwrap();
        std::fs::write(temp_dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp_dir.path().join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(temp_dir.path().join("readme.md"), "# Readme").unwrap();

        let handler = RustLanguage;
        let files = handler.find_source_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extension().unwrap() == "rs"));
    }

    #[test]
    fn test_find_source_files_skips_target() {
        let temp_dir = TempDir::new().unwrap();
        let target_dir = temp_dir.path().join("target");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("generated.rs"), "// generated").unwrap();

        let handler = RustLanguage;
        let files = handler.find_source_files(temp_dir.path()).unwrap();

        assert!(files.is_empty());
    }

    #[test]
    fn test_add_line_numbers() {
        let code = "fn foo() {\n    bar()\n}";
        let numbered = add_line_numbers(code);
        assert!(numbered.contains("   1 | fn foo() {"));
        assert!(numbered.contains("   2 |     bar()"));
        assert!(numbered.contains("   3 | }"));
    }

    #[test]
    fn test_truncate_output_short() {
        let output = "short";
        assert_eq!(truncate_output(output, 100), "short");
    }

    #[test]
    fn test_truncate_output_long() {
        let output = "a".repeat(100);
        let truncated = truncate_output(&output, 50);
        assert!(truncated.len() < 100);
        assert!(truncated.ends_with("...(truncated)"));
    }

    #[test]
    fn test_extract_failing_test() {
        let output = r#"
running 5 tests
test some_passing_test ... ok
test another_test ... ok
---- my_failing_test stdout ----
thread 'my_failing_test' panicked
"#;
        assert_eq!(
            extract_failing_test(output),
            Some("my_failing_test".to_string())
        );
    }

    #[test]
    fn test_extract_failing_test_none() {
        let output = "running 5 tests\ntest foo ... ok\n";
        assert_eq!(extract_failing_test(output), None);
    }

    #[test]
    fn test_analysis_prompt_contains_file() {
        let handler = RustLanguage;
        let prompt = handler.analysis_prompt("src/main.rs", "fn main() {}");
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("fn main()"));
    }

    #[test]
    fn test_mutation_prompt_contains_line_numbers() {
        let handler = RustLanguage;
        let prompt = handler.mutation_prompt("src/lib.rs", "fn foo() {\n    1 + 1\n}");
        assert!(prompt.contains("   1 | fn foo()"));
        assert!(prompt.contains("   2 |     1 + 1"));
    }
}
