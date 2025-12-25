//! Mutation test executor.
//!
//! Handles applying mutations, running tests, and reverting changes.

use crate::mutation::{GeneratedMutation, MutationConfig, MutationTestResult, TestOutcome};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;

/// Execute mutation testing for a single mutation.
///
/// This function:
/// 1. Applies the mutation to the source file
/// 2. Runs `cargo test`
/// 3. Reverts the file (always, even on error)
/// 4. Returns the test result
pub async fn execute_mutation_test(
    repo_path: &Path,
    mutation: GeneratedMutation,
    config: &MutationConfig,
) -> Result<MutationTestResult> {
    let file_path = Path::new(&mutation.file_path);
    let start_time = Instant::now();

    // Read original file content
    let original_content = tokio::fs::read_to_string(file_path)
        .await
        .context("Failed to read file for mutation")?;

    // Apply mutation
    let mutated_content = apply_mutation(&original_content, &mutation)?;

    // Write mutated file
    tokio::fs::write(file_path, &mutated_content)
        .await
        .context("Failed to write mutated file")?;

    // Run cargo test with timeout
    let test_result = run_cargo_test(repo_path, config).await;

    // ALWAYS revert the file, even if test failed
    if let Err(e) = tokio::fs::write(file_path, &original_content).await {
        // Critical error - file may be left in mutated state
        tracing::warn!(
            "CRITICAL: Failed to revert file {}: {}",
            file_path.display(),
            e
        );
        // Try once more
        let _ = tokio::fs::write(file_path, &original_content).await;
    }

    let execution_time_ms = start_time.elapsed().as_millis() as u64;

    let (outcome, killing_test, test_output) = match test_result {
        TestResult::Passed => (TestOutcome::Survived, None, None),
        TestResult::Failed { test_name, output } => (
            TestOutcome::Killed,
            Some(test_name),
            Some(truncate_output(&output, config.max_test_output_bytes)),
        ),
        TestResult::CompileError { output } => {
            // Log detailed info for compile errors to help debug
            tracing::warn!(
                "Compile error for mutation in {}:{}\n  Description: {}\n  Find: '{}'\n  Replace: '{}'",
                mutation.file_path, mutation.line_number, mutation.description,
                mutation.find, mutation.replace
            );
            // Log first few lines of the error
            let error_preview: String = output.lines().take(10).collect::<Vec<_>>().join("\n");
            tracing::warn!("  Error output:\n{}", error_preview);

            (
                TestOutcome::CompileError,
                None,
                Some(truncate_output(&output, config.max_test_output_bytes)),
            )
        }
        TestResult::Timeout => (TestOutcome::Timeout, None, None),
    };

    tracing::info!(
        "Mutation test complete: {}:{} ({}) = {:?} ({}ms)",
        mutation.file_path, mutation.line_number, mutation.description, outcome, execution_time_ms
    );

    Ok(MutationTestResult {
        mutation,
        outcome,
        killing_test,
        test_output,
        execution_time_ms,
    })
}

/// Line tolerance when applying the mutation (search nearby lines).
const LINE_TOLERANCE: usize = 3;

/// Apply a mutation to file content using search/replace.
/// Searches for `mutation.find` within a window around `mutation.line_number`
/// and replaces the first occurrence with `mutation.replace`.
fn apply_mutation(content: &str, mutation: &GeneratedMutation) -> Result<String> {
    let lines: Vec<&str> = content.lines().collect();
    let line_count = lines.len();

    if mutation.line_number == 0 || mutation.line_number > line_count + LINE_TOLERANCE {
        anyhow::bail!(
            "Line number {} out of range (file has {} lines)",
            mutation.line_number,
            line_count
        );
    }

    // Search for the "find" text within the line window
    let start_line = mutation.line_number.saturating_sub(LINE_TOLERANCE).max(1);
    let end_line = (mutation.line_number + LINE_TOLERANCE).min(line_count);

    let mut target_line = None;
    for line_num in start_line..=end_line {
        if lines[line_num - 1].contains(&mutation.find) {
            target_line = Some(line_num);
            break;
        }
    }

    // Fallback: search entire file
    let target_line = match target_line {
        Some(ln) => ln,
        None => lines
            .iter()
            .position(|l| l.contains(&mutation.find))
            .map(|idx| idx + 1)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find '{}' in file (searched around line {})",
                    mutation.find,
                    mutation.line_number
                )
            })?,
    };

    // Build the new content with the replacement on the target line
    let mut new_lines: Vec<String> = Vec::with_capacity(lines.len());

    for (idx, line) in lines.iter().enumerate() {
        let line_num = idx + 1;
        if line_num == target_line {
            // Replace only the first occurrence on this line
            let new_line = line.replacen(&mutation.find, &mutation.replace, 1);
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
    // apply_mutation tests
    // =========================================================================

    fn make_test_mutation(
        line_number: usize,
        find: &str,
        replace: &str,
        description: &str,
    ) -> GeneratedMutation {
        GeneratedMutation {
            file_path: "test.rs".to_string(),
            line_number,
            find: find.to_string(),
            replace: replace.to_string(),
            reasoning: "test reasoning".to_string(),
            description: description.to_string(),
        }
    }

    #[test]
    fn test_apply_mutation_simple_replace() {
        let content = "fn foo() {\n    if x > 0 {\n    }\n}";
        let mutation = make_test_mutation(2, "x > 0", "x >= 0", "Changed > to >=");

        let result = apply_mutation(content, &mutation).unwrap();
        assert!(result.contains("if x >= 0 {"));
        assert!(!result.contains("if x > 0 {"));
    }

    #[test]
    fn test_apply_mutation_with_tolerance() {
        // Line number is off by 1, but should still find the text
        let content = "line 1\nline 2\nif count > 0 {\nline 4";
        let mutation = make_test_mutation(2, "count > 0", "count >= 0", "Changed > to >=");

        let result = apply_mutation(content, &mutation).unwrap();
        assert!(result.contains("count >= 0"));
        assert!(!result.contains("count > 0"));
    }

    #[test]
    fn test_apply_mutation_not_found() {
        let content = "line 1\nline 2";
        let mutation = make_test_mutation(1, "nonexistent", "replacement", "Invalid");

        let result = apply_mutation(content, &mutation);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Could not find"));
    }

    #[test]
    fn test_apply_mutation_line_out_of_bounds() {
        let content = "line 1\nline 2";
        let mutation = make_test_mutation(100, "line 1", "changed", "Out of bounds");

        let result = apply_mutation(content, &mutation);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of range"));
    }

    #[test]
    fn test_apply_mutation_line_zero() {
        let content = "line 1\nline 2";
        let mutation = make_test_mutation(0, "line 1", "changed", "Line zero");

        let result = apply_mutation(content, &mutation);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("out of range"));
    }

    #[test]
    fn test_apply_mutation_preserves_other_lines() {
        let content = "line 1\nline 2 has target\nline 3\nline 4";
        let mutation = make_test_mutation(2, "target", "REPLACED", "Replace target");

        let result = apply_mutation(content, &mutation).unwrap();
        let lines: Vec<&str> = result.lines().collect();

        assert_eq!(lines[0], "line 1");
        assert_eq!(lines[1], "line 2 has REPLACED");
        assert_eq!(lines[2], "line 3");
        assert_eq!(lines[3], "line 4");
    }

    #[test]
    fn test_apply_mutation_first_line() {
        let content = "first > line\nsecond line";
        let mutation = make_test_mutation(1, ">", ">=", "Changed operator");

        let result = apply_mutation(content, &mutation).unwrap();
        assert!(result.starts_with("first >= line"));
    }

    #[test]
    fn test_apply_mutation_boolean() {
        let content = "fn test() {\n    return true;\n}";
        let mutation = make_test_mutation(2, "true", "false", "Changed true to false");

        let result = apply_mutation(content, &mutation).unwrap();
        assert!(result.contains("return false;"));
        assert!(!result.contains("return true;"));
    }

    #[test]
    fn test_apply_mutation_only_first_occurrence_on_line() {
        // If "true" appears twice on the same line, only replace the first
        let content = "let x = true && true;";
        let mutation = make_test_mutation(1, "true", "false", "Changed true to false");

        let result = apply_mutation(content, &mutation).unwrap();
        assert_eq!(result.trim(), "let x = false && true;");
    }
}
