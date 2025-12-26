//! Mutation test executor.
//!
//! Handles applying mutations, running tests, and reverting changes.
//! Includes retry logic for compile errors - re-prompts the LLM up to 3 times.

use crate::analyzer::OllamaClient;
use crate::mutation::analyzer::fix_mutation_with_error;
use crate::mutation::{
    GeneratedMutation, MutationConfig, MutationTestResult, Replacement, TestOutcome,
};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;

/// Maximum number of times to retry a mutation that fails to compile.
const MAX_COMPILE_RETRIES: u8 = 3;

/// Execute mutation testing for a single mutation.
///
/// This function:
/// 1. Applies the mutation to the source file
/// 2. Runs `cargo check` to verify compilation
/// 3. If compilation fails, re-prompts the LLM to fix the mutation (up to 3 times)
/// 4. Runs `cargo test` if compilation succeeds
/// 5. Reverts the file (always, even on error)
/// 6. Returns the test result
pub async fn execute_mutation_test(
    client: &OllamaClient,
    repo_path: &Path,
    mutation: GeneratedMutation,
    original_code: &str,
    config: &MutationConfig,
) -> Result<MutationTestResult> {
    let start_time = Instant::now();

    // Clone the file path before moving mutation
    let file_path_str = mutation.file_path.clone();
    let file_path = Path::new(&file_path_str);

    // Read original file content
    let original_content = tokio::fs::read_to_string(file_path)
        .await
        .context("Failed to read file for mutation")?;

    let mut current_mutation = mutation;
    let mut last_compile_error: Option<String> = None;

    // Retry loop for compile errors
    for attempt in 1..=MAX_COMPILE_RETRIES {
        // Apply mutation
        let mutated_content =
            match apply_replacements(&original_content, &current_mutation.replacements) {
                Ok(content) => content,
                Err(e) => {
                    // Failed to apply mutation - can't retry this
                    tracing::warn!("Failed to apply mutation: {}", e);
                    return Ok(MutationTestResult {
                        mutation: current_mutation,
                        outcome: TestOutcome::CompileError,
                        killing_test: None,
                        test_output: Some(format!("Failed to apply mutation: {}", e)),
                        execution_time_ms: start_time.elapsed().as_millis() as u64,
                    });
                }
            };

        // Write mutated file
        tokio::fs::write(file_path, &mutated_content)
            .await
            .context("Failed to write mutated file")?;

        // Fast compile check first
        match run_cargo_check(repo_path, config.test_timeout_seconds).await {
            Ok(()) => {
                // Compilation succeeded! Run the full test suite
                let test_result = run_cargo_test(repo_path, config).await;

                // Revert file before returning
                revert_file(file_path, &original_content).await;

                let execution_time_ms = start_time.elapsed().as_millis() as u64;

                let (outcome, killing_test, test_output) = match test_result {
                    TestResult::Passed => (TestOutcome::Survived, None, None),
                    TestResult::Failed { test_name, output } => (
                        TestOutcome::Killed,
                        Some(test_name),
                        Some(truncate_output(&output, config.max_test_output_bytes)),
                    ),
                    TestResult::CompileError { output } => {
                        // This shouldn't happen since cargo check passed, but handle it
                        (
                            TestOutcome::CompileError,
                            None,
                            Some(truncate_output(&output, config.max_test_output_bytes)),
                        )
                    }
                    TestResult::Timeout => (TestOutcome::Timeout, None, None),
                };

                tracing::info!(
                    "Mutation test complete: {} ({}) = {:?} ({}ms)",
                    current_mutation.file_path,
                    current_mutation.description,
                    outcome,
                    execution_time_ms
                );

                return Ok(MutationTestResult {
                    mutation: current_mutation,
                    outcome,
                    killing_test,
                    test_output,
                    execution_time_ms,
                });
            }
            Err(compile_error) => {
                // Revert file before retrying or returning
                revert_file(file_path, &original_content).await;

                last_compile_error = Some(compile_error.clone());

                if attempt < MAX_COMPILE_RETRIES {
                    tracing::info!(
                        "Mutation compile error (attempt {}/{}), re-prompting LLM: {}",
                        attempt,
                        MAX_COMPILE_RETRIES,
                        current_mutation.description
                    );

                    // Log first few lines of the error
                    let error_preview: String = compile_error
                        .lines()
                        .take(10)
                        .collect::<Vec<_>>()
                        .join("\n");
                    tracing::debug!("Compile error preview:\n{}", error_preview);

                    // Re-prompt LLM to fix the mutation
                    match fix_mutation_with_error(
                        client,
                        &current_mutation.file_path,
                        original_code,
                        &current_mutation,
                        &compile_error,
                        attempt,
                    )
                    .await
                    {
                        Ok(fixed_mutation) => {
                            tracing::debug!(
                                "LLM provided fixed mutation: {}",
                                fixed_mutation.description
                            );
                            current_mutation = fixed_mutation;
                            // Continue to next iteration of retry loop
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get fixed mutation from LLM: {}", e);
                            // Can't retry further, break out
                            break;
                        }
                    }
                } else {
                    tracing::warn!(
                        "Mutation failed to compile after {} attempts: {}",
                        MAX_COMPILE_RETRIES,
                        current_mutation.description
                    );
                }
            }
        }
    }

    // All retries exhausted or LLM fix failed
    let execution_time_ms = start_time.elapsed().as_millis() as u64;

    // Log detailed info for the final compile error
    tracing::warn!(
        "Compile error for mutation in {}\n  Description: {}",
        current_mutation.file_path,
        current_mutation.description
    );

    Ok(MutationTestResult {
        mutation: current_mutation,
        outcome: TestOutcome::CompileError,
        killing_test: None,
        test_output: last_compile_error.map(|e| truncate_output(&e, config.max_test_output_bytes)),
        execution_time_ms,
    })
}

/// Revert a file to its original content, with retry on failure.
async fn revert_file(file_path: &Path, original_content: &str) {
    if let Err(e) = tokio::fs::write(file_path, original_content).await {
        tracing::warn!(
            "CRITICAL: Failed to revert file {}: {}",
            file_path.display(),
            e
        );
        // Try once more
        let _ = tokio::fs::write(file_path, original_content).await;
    }
}

/// Line tolerance when applying replacements (search nearby lines).
const LINE_TOLERANCE: usize = 3;

/// Apply multiple replacements to file content.
///
/// Replacements are applied in descending line order to prevent line number
/// shifts from affecting subsequent replacements (important when a replacement
/// adds or removes lines, like adding an import).
fn apply_replacements(content: &str, replacements: &[Replacement]) -> Result<String> {
    if replacements.is_empty() {
        anyhow::bail!("No replacements to apply");
    }

    // Sort replacements by line number descending
    let mut sorted_replacements: Vec<&Replacement> = replacements.iter().collect();
    sorted_replacements.sort_by(|a, b| b.line_number.cmp(&a.line_number));

    let mut current_content = content.to_string();

    for replacement in sorted_replacements {
        current_content = apply_single_replacement(&current_content, replacement)?;
    }

    Ok(current_content)
}

/// Apply a single replacement to file content.
///
/// Searches for `replacement.find` within a window around `replacement.line_number`
/// and replaces the first occurrence with `replacement.replace`.
fn apply_single_replacement(content: &str, replacement: &Replacement) -> Result<String> {
    let lines: Vec<&str> = content.lines().collect();
    let line_count = lines.len();

    if replacement.line_number == 0 || replacement.line_number > line_count + LINE_TOLERANCE {
        anyhow::bail!(
            "Line number {} out of range (file has {} lines)",
            replacement.line_number,
            line_count
        );
    }

    // Search for the "find" text within the line window
    let start_line = replacement
        .line_number
        .saturating_sub(LINE_TOLERANCE)
        .max(1);
    let end_line = (replacement.line_number + LINE_TOLERANCE).min(line_count);

    let mut target_line = None;
    for line_num in start_line..=end_line {
        if lines[line_num - 1].contains(&replacement.find) {
            target_line = Some(line_num);
            break;
        }
    }

    // Fallback: search entire file
    let target_line = match target_line {
        Some(ln) => ln,
        None => lines
            .iter()
            .position(|l| l.contains(&replacement.find))
            .map(|idx| idx + 1)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find '{}' in file (searched around line {})",
                    replacement.find,
                    replacement.line_number
                )
            })?,
    };

    // Build the new content with the replacement on the target line
    let mut new_lines: Vec<String> = Vec::with_capacity(lines.len());

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        if line_num == target_line {
            // Replace only the first occurrence on this line
            let new_line = line.replacen(&replacement.find, &replacement.replace, 1);
            new_lines.push(new_line);
        } else {
            new_lines.push(line.to_string());
        }
    }

    // Preserve original line ending style
    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    Ok(new_lines.join(line_ending))
}

enum TestResult {
    Passed,
    Failed { test_name: String, output: String },
    CompileError { output: String },
    Timeout,
}

/// Run `cargo check` to quickly verify compilation without running tests.
///
/// Returns `Ok(())` if compilation succeeds, `Err(error_output)` if it fails.
async fn run_cargo_check(repo_path: &Path, timeout_secs: u64) -> Result<(), String> {
    let timeout = std::time::Duration::from_secs(timeout_secs);

    let check_future = async {
        Command::new("cargo")
            .arg("check")
            .current_dir(repo_path)
            .output()
            .await
    };

    match tokio::time::timeout(timeout, check_future).await {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(stderr.to_string())
            }
        }
        Ok(Err(e)) => Err(format!("Failed to run cargo check: {}", e)),
        Err(_) => Err("Cargo check timed out".to_string()),
    }
}

async fn run_cargo_test(repo_path: &Path, config: &MutationConfig) -> TestResult {
    let timeout = std::time::Duration::from_secs(config.test_timeout_seconds);

    let test_future = async {
        Command::new("cargo")
            .arg("test")
            .arg("--")
            .arg("--test-threads=1") // Deterministic ordering
            .current_dir(repo_path)
            .output()
            .await
    };

    let result = tokio::time::timeout(timeout, test_future).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}\n{}", stdout, stderr);

            if output.status.success() {
                TestResult::Passed
            } else {
                // Check if it's a compile error
                if stderr.contains("error[E") || stderr.contains("could not compile") {
                    TestResult::CompileError { output: combined }
                } else {
                    // Extract failing test name
                    let test_name =
                        extract_failing_test(&combined).unwrap_or_else(|| "unknown".to_string());
                    TestResult::Failed {
                        test_name,
                        output: combined,
                    }
                }
            }
        }
        Ok(Err(e)) => TestResult::CompileError {
            output: format!("Failed to run cargo test: {}", e),
        },
        Err(_) => {
            tracing::debug!("Test timed out after {:?}", timeout);
            TestResult::Timeout
        }
    }
}

/// Extract the name of the first failing test from cargo test output.
fn extract_failing_test(output: &str) -> Option<String> {
    // Look for patterns like "test module::test_name ... FAILED"
    for line in output.lines() {
        if line.contains("FAILED") && line.trim().starts_with("test ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return Some(parts[1].to_string());
            }
        }
    }
    None
}

fn truncate_output(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        output.to_string()
    } else {
        format!("{}...(truncated)", &output[..max_bytes])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // extract_failing_test tests
    // =========================================================================

    #[test]
    fn test_extract_failing_test() {
        let output = r#"
running 3 tests
test foo::bar::test_one ... ok
test foo::bar::test_two ... FAILED
test foo::bar::test_three ... ok
"#;
        assert_eq!(
            extract_failing_test(output),
            Some("foo::bar::test_two".to_string())
        );
    }

    #[test]
    fn test_extract_failing_test_none() {
        let output = "running 1 test\ntest foo ... ok\n";
        assert_eq!(extract_failing_test(output), None);
    }

    #[test]
    fn test_extract_failing_test_multiple_failures() {
        let output = r#"
running 3 tests
test first_test ... FAILED
test second_test ... FAILED
test third_test ... ok
"#;
        // Should return the first failure
        assert_eq!(extract_failing_test(output), Some("first_test".to_string()));
    }

    #[test]
    fn test_extract_failing_test_with_module_path() {
        let output = "test crate::module::submodule::test_name ... FAILED";
        assert_eq!(
            extract_failing_test(output),
            Some("crate::module::submodule::test_name".to_string())
        );
    }

    #[test]
    fn test_extract_failing_test_empty() {
        assert_eq!(extract_failing_test(""), None);
    }

    // =========================================================================
    // truncate_output tests
    // =========================================================================

    #[test]
    fn test_truncate_output() {
        let short = "hello";
        assert_eq!(truncate_output(short, 100), "hello");

        let long = "a".repeat(100);
        let truncated = truncate_output(&long, 50);
        assert!(truncated.len() < 100);
        assert!(truncated.ends_with("...(truncated)"));
    }

    #[test]
    fn test_truncate_output_exact_boundary() {
        let exact = "a".repeat(50);
        assert_eq!(truncate_output(&exact, 50), exact);
    }

    #[test]
    fn test_truncate_output_empty() {
        assert_eq!(truncate_output("", 100), "");
    }

    #[test]
    fn test_truncate_output_one_over() {
        let content = "a".repeat(51);
        let truncated = truncate_output(&content, 50);
        assert!(truncated.starts_with(&"a".repeat(50)));
        assert!(truncated.ends_with("...(truncated)"));
    }

    // =========================================================================
    // Helper for creating test replacements
    // =========================================================================

    fn make_replacement(line_number: usize, find: &str, replace: &str) -> Replacement {
        Replacement {
            line_number,
            find: find.to_string(),
            replace: replace.to_string(),
        }
    }

    // =========================================================================
    // apply_single_replacement tests
    // =========================================================================

    #[test]
    fn test_apply_single_replacement_simple() {
        let content = "fn foo() {\n    if x > 0 {\n    }\n}";
        let replacement = make_replacement(2, "x > 0", "x >= 0");

        let result = apply_single_replacement(content, &replacement).unwrap();
        assert!(result.contains("if x >= 0 {"));
        assert!(!result.contains("if x > 0 {"));
    }

    #[test]
    fn test_apply_single_replacement_with_tolerance() {
        // Line number is off by 1, but should still find the text
        let content = "line 1\nline 2\nif count > 0 {\nline 4";
        let replacement = make_replacement(2, "count > 0", "count >= 0");

        let result = apply_single_replacement(content, &replacement).unwrap();
        assert!(result.contains("count >= 0"));
        assert!(!result.contains("count > 0"));
    }

    #[test]
    fn test_apply_single_replacement_not_found() {
        let content = "line 1\nline 2";
        let replacement = make_replacement(1, "nonexistent", "replacement");

        let result = apply_single_replacement(content, &replacement);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Could not find"));
    }

    #[test]
    fn test_apply_single_replacement_line_out_of_bounds() {
        let content = "line 1\nline 2";
        let replacement = make_replacement(100, "line 1", "changed");

        let result = apply_single_replacement(content, &replacement);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of range"));
    }

    #[test]
    fn test_apply_single_replacement_line_zero() {
        let content = "line 1\nline 2";
        let replacement = make_replacement(0, "line 1", "changed");

        let result = apply_single_replacement(content, &replacement);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of range"));
    }

    #[test]
    fn test_apply_single_replacement_preserves_other_lines() {
        let content = "line 1\nline 2 has target\nline 3\nline 4";
        let replacement = make_replacement(2, "target", "REPLACED");

        let result = apply_single_replacement(content, &replacement).unwrap();
        let lines: Vec<&str> = result.lines().collect();

        assert_eq!(lines[0], "line 1");
        assert_eq!(lines[1], "line 2 has REPLACED");
        assert_eq!(lines[2], "line 3");
        assert_eq!(lines[3], "line 4");
    }

    #[test]
    fn test_apply_single_replacement_first_line() {
        let content = "first > line\nsecond line";
        let replacement = make_replacement(1, ">", ">=");

        let result = apply_single_replacement(content, &replacement).unwrap();
        assert!(result.starts_with("first >= line"));
    }

    #[test]
    fn test_apply_single_replacement_boolean() {
        let content = "fn test() {\n    return true;\n}";
        let replacement = make_replacement(2, "true", "false");

        let result = apply_single_replacement(content, &replacement).unwrap();
        assert!(result.contains("return false;"));
        assert!(!result.contains("return true;"));
    }

    #[test]
    fn test_apply_single_replacement_only_first_occurrence_on_line() {
        // If "true" appears twice on the same line, only replace the first
        let content = "let x = true && true;";
        let replacement = make_replacement(1, "true", "false");

        let result = apply_single_replacement(content, &replacement).unwrap();
        assert_eq!(result.trim(), "let x = false && true;");
    }

    // =========================================================================
    // apply_replacements tests (multiple replacements)
    // =========================================================================

    #[test]
    fn test_apply_replacements_single() {
        let content = "fn foo() {\n    if x > 0 {\n    }\n}";
        let replacements = vec![make_replacement(2, "x > 0", "x >= 0")];

        let result = apply_replacements(content, &replacements).unwrap();
        assert!(result.contains("if x >= 0 {"));
    }

    #[test]
    fn test_apply_replacements_multiple_different_lines() {
        let content = "use std::io;\n\nfn main() {\n    let x = true;\n}";
        let replacements = vec![
            make_replacement(1, "use std::io;", "use std::io;\nuse std::fs;"),
            make_replacement(4, "true", "false"),
        ];

        let result = apply_replacements(content, &replacements).unwrap();
        assert!(result.contains("use std::fs;"));
        assert!(result.contains("let x = false;"));
    }

    #[test]
    fn test_apply_replacements_descending_order() {
        // Verify that replacements are applied in descending line order
        // This is important when a replacement adds lines (like an import)
        let content = "line 1\nline 2\nline 3";
        let replacements = vec![
            make_replacement(1, "line 1", "modified 1"),
            make_replacement(3, "line 3", "modified 3"),
        ];

        let result = apply_replacements(content, &replacements).unwrap();
        let lines: Vec<&str> = result.lines().collect();

        assert_eq!(lines[0], "modified 1");
        assert_eq!(lines[1], "line 2");
        assert_eq!(lines[2], "modified 3");
    }

    #[test]
    fn test_apply_replacements_with_newlines_in_replace() {
        // Test adding an import (which adds a new line)
        let content = "use std::io;\n\nfn main() {}";
        let replacements = vec![make_replacement(
            1,
            "use std::io;",
            "use std::io;\nuse std::fs;",
        )];

        let result = apply_replacements(content, &replacements).unwrap();
        assert!(result.contains("use std::io;\nuse std::fs;"));
    }

    #[test]
    fn test_apply_replacements_empty() {
        let content = "fn foo() {}";
        let replacements: Vec<Replacement> = vec![];

        let result = apply_replacements(content, &replacements);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No replacements"));
    }

    #[test]
    fn test_apply_replacements_one_fails() {
        // If any replacement fails, the whole operation should fail
        let content = "line 1\nline 2";
        let replacements = vec![
            make_replacement(1, "line 1", "modified"),
            make_replacement(1, "nonexistent", "replacement"),
        ];

        let result = apply_replacements(content, &replacements);
        assert!(result.is_err());
    }
}
