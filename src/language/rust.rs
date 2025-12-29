//! Rust language support.

use super::{TestOutcome, TestRunResult};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;

/// Rust language handler.
pub struct RustLanguage;

/// Context file types that provide project-level information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFileType {
    /// Cargo.toml - Rust package manifest
    CargoToml,
    /// README or other markdown documentation
    Markdown,
}

impl RustLanguage {
    pub fn find_source_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        let root_dir = dir.to_path_buf();
        let skip_dirs: &[&str] = &["target", "node_modules", ".git"];

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Don't filter the root directory itself (may be a temp dir starting with .)
                if e.path() == root_dir {
                    return true;
                }
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

    /// Run `cargo check` to quickly verify compilation without running tests.
    ///
    /// Returns `Ok(())` if compilation succeeds, `Err(error_output)` if it fails.
    pub async fn compile_check(
        &self,
        repo_path: &Path,
        timeout_seconds: u64,
    ) -> Result<(), String> {
        let timeout = std::time::Duration::from_secs(timeout_seconds);

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

    /// Find context files (Cargo.toml, READMEs, markdown docs) in a directory.
    pub fn find_context_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        let root_dir = dir.to_path_buf();
        let skip_dirs: &[&str] = &["target", "node_modules", ".git"];

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Don't filter the root directory itself (may be a temp dir starting with .)
                if e.path() == root_dir {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.') && !skip_dirs.contains(&name.as_ref())
            })
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

            // Include: README files, Cargo.toml, and markdown files
            let is_context_file = file_name.to_lowercase().starts_with("readme")
                || file_name == "Cargo.toml"
                || extension == "md";

            if is_context_file {
                files.push(path.to_path_buf());
            }
        }

        Ok(files)
    }

    /// Determine the type of a context file.
    pub fn context_file_type(&self, file_path: &Path) -> Option<ContextFileType> {
        let file_name = file_path.file_name().and_then(|n| n.to_str())?;

        if file_name == "Cargo.toml" {
            Some(ContextFileType::CargoToml)
        } else {
            let extension = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let is_readme = file_name.to_lowercase().starts_with("readme");
            if is_readme || extension == "md" {
                Some(ContextFileType::Markdown)
            } else {
                None
            }
        }
    }

    /// Generate a prompt for documentation/context file analysis.
    pub fn documentation_prompt(&self, file_path: &str, content: &str) -> String {
        let path = Path::new(file_path);
        match self.context_file_type(path) {
            Some(ContextFileType::CargoToml) => self.cargo_toml_prompt(file_path, content),
            Some(ContextFileType::Markdown) => self.markdown_doc_prompt(file_path, content),
            None => self.markdown_doc_prompt(file_path, content), // fallback
        }
    }

    /// Prompt for analyzing Cargo.toml files.
    fn cargo_toml_prompt(&self, file_path: &str, content: &str) -> String {
        format!(
            r#"Analyze this Cargo.toml file for PROJECT STRUCTURE information.

File: {}

```toml
{}
```

Extract the following architectural context:

1. **Project Identity**: Package name, version, and description (if present)

2. **Crate Type**: Is this a binary, library, or workspace? What does it produce?

3. **Key Dependencies**: List the most important external dependencies and their purpose:
   - Web framework (axum, actix, etc.)
   - Database (sqlx, diesel, etc.)
   - Serialization (serde, etc.)
   - Async runtime (tokio, async-std, etc.)
   - Other significant crates

4. **Feature Flags**: Any notable feature configurations?

5. **Workspace Structure**: If this is a workspace, what are the member crates?

Keep the analysis concise and focused on what these dependencies tell us about the project's architecture.

IMPORTANT: Respond only in English."#,
            file_path, content
        )
    }

    /// Prompt for analyzing markdown documentation files.
    fn markdown_doc_prompt(&self, file_path: &str, content: &str) -> String {
        format!(
            r#"Analyze this documentation file for PROJECT CONTEXT.

File: {}

```markdown
{}
```

Extract the following architectural context:

1. **Project Purpose**: What is this project/module for? (1-2 sentences)

2. **Architecture Overview**: Any documented architecture, structure, or design decisions?

3. **Module/Component Structure**: Does it describe how the code is organized?

4. **External Integrations**: Any mentioned external services, APIs, or systems?

5. **Key Concepts**: Important domain concepts or terminology defined?

Focus on information that helps understand the system architecture.
Skip installation instructions, contribution guidelines, or license information.
If the document has no architectural relevance, say "No architectural context".

IMPORTANT: Respond only in English."#,
            file_path, content
        )
    }

    /// Prompt for architecture-focused file analysis.
    pub fn architecture_file_analysis_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this Rust file from an ARCHITECTURAL perspective.

File: {}

```rust
{}
```

Provide a brief architectural analysis including:

1. **Purpose**: What is the primary responsibility of this module? (1 sentence)

2. **Layer**: Which architectural layer does this belong to?
   - Presentation (web handlers, CLI, templates)
   - Application (business logic, services)
   - Infrastructure (database, external APIs, file I/O)
   - Cross-cutting (configuration, logging, utilities)

3. **Key Abstractions**: What are the main types/traits defined here and what do they represent?

4. **Integration Points**: How does this module integrate with other parts of the system?

5. **Design Patterns**: Any notable patterns used? (e.g., Repository, Factory, Builder)

Keep the analysis concise and focused on architectural significance.
Do not describe implementation details or suggest improvements.

IMPORTANT: Respond only in English."#,
            file_path, code
        )
    }

    /// Prompt for extracting architecture-relevant information from a file (for diagrams).
    pub fn diagram_architecture_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this Rust file for ARCHITECTURAL information only.

File: {}

```rust
{}
```

Extract ONLY the following (be very concise, use bullet points):

1. **Module Role**: What role does this module play in the system? (e.g., "HTTP handler", "database layer", "business logic", "configuration")

2. **Public Interface**: List the main public structs, traits, and functions exposed by this module (just names, no details)

3. **Internal Dependencies**: Which other internal modules does this depend on? (based on `use crate::` or `use super::`)

4. **External Dependencies**: Which external crates are used? (just crate names)

5. **Component Type**: Classify as one of: web/api, database, business_logic, utility, configuration, other

Keep responses brief and factual. Focus on structure, not implementation details.
If this file has no significant architectural role (e.g., just re-exports), say "Minimal architectural significance".

IMPORTANT: Respond only in English."#,
            file_path, code
        )
    }

    /// Prompt for extracting data flow information from a file (for diagrams).
    pub fn diagram_data_flow_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this Rust file for DATA FLOW patterns.

File: {}

```rust
{}
```

Extract ONLY the following (be very concise):

1. **Data Sources**: Where does data come from? Examples:
   - HTTP requests (axum handlers, request bodies)
   - File reads (std::fs, tokio::fs)
   - Database queries (sqlx, diesel)
   - Environment variables, configuration files
   - Message queues, channels

2. **Data Transformations**: What transformations occur?
   - Parsing (JSON, TOML, etc.)
   - Validation
   - Mapping between types
   - Aggregation, filtering

3. **Data Sinks**: Where does data go?
   - HTTP responses
   - File writes
   - Database writes
   - External API calls
   - Logging

4. **Async Boundaries**: Any async/await patterns, channels (mpsc, oneshot), or spawned tasks?

If this file has no significant data flow (e.g., type definitions only, utilities), say "No significant data flow".

IMPORTANT: Respond only in English."#,
            file_path, code
        )
    }

    /// Prompt for extracting database schema information from a file (for diagrams).
    pub fn diagram_database_schema_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this Rust file for DATABASE-RELATED structures.

File: {}

```rust
{}
```

Extract ONLY the following (be very concise):

1. **Database Models**: Structs that represent database tables
   - Look for #[derive(FromRow)], sqlx attributes, or structs matching table patterns
   - List struct names and their key fields

2. **Table Relationships**: Any foreign key references or relationships
   - Look for fields like `repository_id`, `user_id`, etc.
   - Note which tables reference which

3. **SQL Operations**: Types of queries in this file
   - CREATE TABLE statements (from migrations)
   - SELECT, INSERT, UPDATE, DELETE patterns
   - Which tables are operated on

4. **Schema Migrations**: Any table creation or alteration
   - Column definitions
   - Indexes
   - Constraints

If this file has no database relevance, say "No database content".

IMPORTANT: Respond only in English."#,
            file_path, code
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
