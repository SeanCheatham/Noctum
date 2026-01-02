//! Mutation test executor.
//!
//! Handles applying mutations, running tests, and reverting changes.
//! Includes retry logic for compile errors - re-prompts the LLM up to 3 times.

use crate::analyzer::OllamaClient;
use crate::mutation::analyzer::{analyze_test_output, fix_mutation_with_error};
use crate::mutation::{
    GeneratedMutation, MutationConfig, MutationTestResult, Replacement, TestOutcome,
};
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
use tokio::time::Duration;

/// Maximum number of times to retry a mutation that fails to compile.
const MAX_COMPILE_RETRIES: u8 = 3;

/// Execute mutation testing for a single mutation.
///
/// This function:
/// 1. Applies the mutation to the source file
/// 2. Runs the configured build command to check compilation
/// 3. If compilation fails, re-prompts the LLM to fix the mutation (up to 3 times)
/// 4. Runs the configured test command if compilation succeeds
/// 5. Reverts the file (always, even on error)
/// 6. Returns the test result
#[allow(clippy::too_many_arguments)]
pub async fn execute_mutation_test(
    client: &OllamaClient,
    repo_path: &Path,
    mutation: GeneratedMutation,
    original_code: &str,
    config: &MutationConfig,
    build_command: &str,
    test_command: &str,
    timeout_seconds: u64,
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

        // Fast compile check first using configured build command
        match run_build_command(repo_path, build_command, timeout_seconds).await {
            Ok(()) => {
                // Compilation succeeded! Run the test suite using configured test command
                let test_result = run_tests_with_command(
                    client,
                    repo_path,
                    test_command,
                    timeout_seconds,
                    config,
                )
                .await;

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
                        // This shouldn't happen since compile check passed, but handle it
                        // (can occur if test execution triggers additional compilation)
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
                    tracing::debug!(
                        "Mutation compile error (attempt {}/{}), re-prompting LLM: {}",
                        attempt,
                        MAX_COMPILE_RETRIES,
                        current_mutation.description
                    );

                    // Log last portion of the error (most relevant info is usually at the end)
                    let error_preview = truncate_output_tail(&compile_error, 500);
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

/// Run the build command and check if compilation succeeds.
///
/// Returns `Ok(())` if the command succeeds (exit code 0),
/// or `Err(output)` with the command output if it fails.
async fn run_build_command(
    repo_path: &Path,
    build_command: &str,
    timeout_seconds: u64,
) -> std::result::Result<(), String> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(build_command)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            return Err(format!("Failed to spawn build command: {}", e));
        }
    };

    let timeout = Duration::from_secs(timeout_seconds);
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

    match result {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(())
            } else {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("{}{}", stdout, stderr))
            }
        }
        Ok(Err(e)) => Err(format!("Build command execution error: {}", e)),
        Err(_) => Err(format!(
            "Build command timed out after {} seconds",
            timeout_seconds
        )),
    }
}

/// Run test command and analyze output with LLM.
async fn run_tests_with_command(
    client: &OllamaClient,
    repo_path: &Path,
    test_command: &str,
    timeout_seconds: u64,
    config: &MutationConfig,
) -> TestResult {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(test_command)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            return TestResult::CompileError {
                output: format!("Failed to spawn test command: {}", e),
            };
        }
    };

    let timeout = Duration::from_secs(timeout_seconds);
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

    let (exit_code, output) = match result {
        Ok(Ok(cmd_output)) => {
            let stdout = String::from_utf8_lossy(&cmd_output.stdout);
            let stderr = String::from_utf8_lossy(&cmd_output.stderr);
            let combined = format!("{}{}", stdout, stderr);
            let code = cmd_output.status.code();
            (code, combined)
        }
        Ok(Err(e)) => {
            return TestResult::CompileError {
                output: format!("Test command execution error: {}", e),
            };
        }
        Err(_) => {
            return TestResult::Timeout;
        }
    };

    // Optimization: exit code 0 means tests passed - skip LLM analysis
    // This saves an inference call for every surviving mutation
    if exit_code == Some(0) {
        return TestResult::Passed;
    }

    // For non-zero exit codes, use LLM to analyze test output
    // This helps extract the specific failing test name and distinguish
    // between test failures vs. compile errors
    let truncated_output = truncate_output(&output, config.max_test_output_bytes);

    match analyze_test_output(client, &truncated_output, exit_code).await {
        Ok(analysis) => match analysis.outcome.as_str() {
            "passed" => TestResult::Passed,
            "failed" => TestResult::Failed {
                test_name: analysis
                    .failing_test
                    .unwrap_or_else(|| "unknown".to_string()),
                output: truncated_output,
            },
            "compile_error" => TestResult::CompileError {
                output: truncated_output,
            },
            "timeout" => TestResult::Timeout,
            _ => {
                // Fallback based on exit code
                tracing::warn!(
                    "Unexpected LLM outcome: {}, falling back to exit code",
                    analysis.outcome
                );
                match exit_code {
                    Some(0) => TestResult::Passed,
                    Some(_) => TestResult::Failed {
                        test_name: "unknown".to_string(),
                        output: truncated_output,
                    },
                    None => TestResult::Timeout,
                }
            }
        },
        Err(e) => {
            // If LLM analysis fails, fall back to exit code
            tracing::warn!(
                "Failed to analyze test output with LLM: {}, falling back to exit code",
                e
            );
            match exit_code {
                Some(0) => TestResult::Passed,
                Some(_) => TestResult::Failed {
                    test_name: "unknown".to_string(),
                    output: truncated_output,
                },
                None => TestResult::Timeout,
            }
        }
    }
}

fn truncate_output(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        output.to_string()
    } else {
        format!("{}...(truncated)", &output[..max_bytes])
    }
}

/// Truncate output from the beginning, keeping the tail (last N bytes).
pub fn truncate_output_tail(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        output.to_string()
    } else {
        let start = output.len() - max_bytes;
        // Find a safe UTF-8 boundary
        let start = output
            .char_indices()
            .map(|(i, _)| i)
            .find(|&i| i >= start)
            .unwrap_or(start);
        format!("(truncated)...{}", &output[start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    // truncate_output_tail tests
    // =========================================================================

    #[test]
    fn test_truncate_output_tail_short() {
        let short = "hello";
        assert_eq!(truncate_output_tail(short, 100), "hello");
    }

    #[test]
    fn test_truncate_output_tail_long() {
        let long = "abcdefghij".repeat(10); // 100 chars
        let truncated = truncate_output_tail(&long, 50);
        assert!(truncated.starts_with("(truncated)..."));
        assert!(truncated.ends_with("abcdefghij"));
        assert!(truncated.len() < 100);
    }

    #[test]
    fn test_truncate_output_tail_exact_boundary() {
        let exact = "a".repeat(50);
        assert_eq!(truncate_output_tail(&exact, 50), exact);
    }

    #[test]
    fn test_truncate_output_tail_empty() {
        assert_eq!(truncate_output_tail("", 100), "");
    }

    #[test]
    fn test_truncate_output_tail_keeps_end() {
        let content = "start_middle_end";
        let truncated = truncate_output_tail(content, 5);
        // Last 5 chars of "start_middle_end" are "e_end"
        assert!(truncated.ends_with("e_end"));
        assert!(truncated.starts_with("(truncated)..."));
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
