//! TypeScript/JavaScript language support.

use super::{TestOutcome, TestRunResult};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::process::Command;

/// TypeScript/JavaScript language handler.
///
/// This covers both TypeScript (.ts, .tsx) and JavaScript (.js, .jsx, .mjs, .cjs) files.
pub struct TypeScriptLanguage;

/// Context file types that provide project-level information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFileType {
    /// package.json - npm package manifest
    PackageJson,
    /// tsconfig.json or jsconfig.json - TypeScript/JavaScript configuration
    TsConfig,
    /// README or other markdown documentation
    Markdown,
}

impl TypeScriptLanguage {
    /// Find all source files in a directory.
    pub fn find_source_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        let root_dir = dir.to_path_buf();
        let skip_dirs: &[&str] = &[
            "node_modules",
            ".git",
            "dist",
            "build",
            ".next",
            "coverage",
            ".turbo",
            ".cache",
        ];
        let extensions: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

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

            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.contains(&ext) {
                        files.push(path.to_path_buf());
                    }
                }
            }
        }

        Ok(files)
    }

    /// Run a compile check to verify TypeScript compilation without running tests.
    ///
    /// For TypeScript projects, runs `tsc --noEmit` if tsconfig.json exists.
    /// Otherwise, skips the compile check (JavaScript/TypeScript is often interpreted).
    ///
    /// Returns `Ok(())` if compilation succeeds or is skipped, `Err(error_output)` if it fails.
    pub async fn compile_check(
        &self,
        repo_path: &Path,
        timeout_seconds: u64,
    ) -> Result<(), String> {
        // For TypeScript, try to run tsc --noEmit if tsconfig.json exists
        // Otherwise, skip compile check (JavaScript/TypeScript is often interpreted)
        let tsconfig_path = repo_path.join("tsconfig.json");
        if tsconfig_path.exists() {
            let timeout = std::time::Duration::from_secs(timeout_seconds);

            let check_future = async {
                Command::new("npx")
                    .arg("tsc")
                    .arg("--noEmit")
                    .current_dir(repo_path)
                    .output()
                    .await
            };

            match tokio::time::timeout(timeout, check_future).await {
                Ok(Ok(output)) => process_compile_output(output),
                Ok(Err(e)) => Err(format!("Failed to run tsc --noEmit: {}", e)),
                Err(_) => Err("TypeScript type check timed out".to_string()),
            }
        } else {
            // No tsconfig.json, skip compile check for JavaScript/TypeScript
            // Type errors will be caught during test execution
            Ok(())
        }
    }

    /// Run tests using the appropriate test runner.
    ///
    /// Detects the test runner from package.json and runs tests.
    /// Tries vitest, jest, mocha, or npm test in that order.
    pub async fn run_tests(&self, project_root: &Path, timeout_seconds: u64) -> TestRunResult {
        let start = Instant::now();

        // Try to detect test runner from package.json
        let test_command = self.detect_test_command(project_root);

        let (program, args) = match test_command.as_deref() {
            Some("vitest") => ("npx", vec!["vitest", "run"]),
            Some("jest") => ("npx", vec!["jest", "--passWithNoTests"]),
            Some("mocha") => ("npx", vec!["mocha"]),
            _ => ("npm", vec!["test", "--", "--passWithNoTests"]),
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds),
            Command::new(program)
                .args(&args)
                .current_dir(project_root)
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
                    // Test runner failed - need to distinguish between:
                    // 1. Actual test failures (should be marked as "killed")
                    // 2. Compilation/type errors (should be marked as "compile error")
                    // 3. Setup/configuration errors (should be marked as "compile error")

                    let stderr_lower = stderr.to_lowercase();
                    let combined_lower = combined.to_lowercase();

                    // Extract failing test name - if found, it's a real test failure
                    let failing_test = extract_failing_test(&combined);

                    // Check for compilation/type error patterns
                    let is_compile_error = stderr_lower.contains("cannot find module")
                        || stderr_lower.contains("syntaxerror")
                        || stderr_lower.contains("typeerror")
                        || stderr_lower.contains("referenceerror")
                        || stderr_lower.contains("module not found")
                        || stderr_lower.contains("cannot resolve")
                        || combined_lower.contains("error ts") // TypeScript error codes (TS2304, TS2322, etc.)
                        || combined_lower.contains("typescript error")
                        || combined_lower.contains("type error")
                        || combined_lower.contains("compilation error")
                        || combined_lower.contains("build error")
                        || combined_lower.contains("failed to compile")
                        || combined_lower.contains("failed to build")
                        || combined_lower.contains("parse error")
                        || combined_lower.contains("unexpected token")
                        // If there are errors but no specific failing test found, likely a compile/setup error
                        || (combined_lower.contains("error") && failing_test.is_none() && !combined_lower.contains("test"));

                    if failing_test.is_some() && !is_compile_error {
                        // Found a specific failing test and no compile errors - real test failure
                        TestRunResult {
                            outcome: TestOutcome::Failed,
                            failing_test,
                            output: Some(truncated),
                            duration_ms,
                        }
                    } else {
                        // No specific test failure found or compile errors detected
                        // Treat as compilation/setup error
                        TestRunResult {
                            outcome: TestOutcome::CompileError,
                            failing_test: None,
                            output: Some(truncated),
                            duration_ms,
                        }
                    }
                }
            }
            Ok(Err(e)) => TestRunResult {
                outcome: TestOutcome::CompileError,
                failing_test: None,
                output: Some(format!("Failed to execute tests: {}", e)),
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

    /// Detect the test command from package.json.
    fn detect_test_command(&self, project_root: &Path) -> Option<String> {
        let package_json = project_root.join("package.json");
        let content = std::fs::read_to_string(&package_json).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;

        // Check devDependencies for test frameworks
        if let Some(dev_deps) = json.get("devDependencies").and_then(|d| d.as_object()) {
            if dev_deps.contains_key("vitest") {
                return Some("vitest".to_string());
            }
            if dev_deps.contains_key("jest") {
                return Some("jest".to_string());
            }
            if dev_deps.contains_key("mocha") {
                return Some("mocha".to_string());
            }
        }

        // Check dependencies too
        if let Some(deps) = json.get("dependencies").and_then(|d| d.as_object()) {
            if deps.contains_key("vitest") {
                return Some("vitest".to_string());
            }
            if deps.contains_key("jest") {
                return Some("jest".to_string());
            }
        }

        // Fallback to npm test
        None
    }

    /// Generate a prompt for code analysis.
    pub fn analysis_prompt(&self, file_path: &str, content: &str) -> String {
        format!(
            "Analyze the following TypeScript/JavaScript code and provide a brief summary of what it does:\n\n\
             File: {}\n\n\
             ```typescript\n{}\n```\n\n\
             Provide a concise analysis including:\n\
             1. Purpose of the code\n\
             2. Key functions, classes, or React components\n\
             3. Any potential issues or improvements\n\
             4. Up to two specific code modification recommendations\n\n\
             IMPORTANT: Respond only in English (or code)",
            file_path, content
        )
    }

    /// Generate a prompt for mutation testing.
    pub fn mutation_prompt(&self, file_path: &str, content: &str) -> String {
        let numbered_code = add_line_numbers(content);
        format!(
            r#"You are a mutation testing expert. Analyze this TypeScript/JavaScript code and generate up to 3 small, targeted mutations.

VALID mutation types:
- Comparison operators: > to >=, < to <=, == to !=, === to !==, etc.
- Boolean literals: true to false, false to true
- Arithmetic operators: + to -, * to /, etc.
- Boundary values: n to n+1, n to n-1
- Return values: undefined to null, null to undefined
- Array methods: .find to .filter, .some to .every
- Numeric constants: 0 to 1, 1 to 0
- String literals (small changes only)

RULES:
- The "find" text must be copied EXACTLY from the code (same spacing, same characters)
- The "replace" text should differ by only ONE small change
- Skip comments, imports, type definitions, and test code
- Avoid React JSX mutations (too likely to cause syntax errors)

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

Example for line `   42 |     if (count > 0) {{`:
  line_number: 42
  find: "count > 0"
  replace: "count >= 0"
  description: "Changed > to >=""#
        )
    }

    /// Find context files (package.json, tsconfig.json, READMEs, markdown docs) in a directory.
    pub fn find_context_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        let root_dir = dir.to_path_buf();
        let skip_dirs: &[&str] = &["node_modules", ".git", "dist", "build", ".next", "coverage"];

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

            // Include: package.json, tsconfig.json, README files, and markdown files
            let is_context_file = file_name == "package.json"
                || file_name == "tsconfig.json"
                || file_name == "jsconfig.json"
                || file_name.to_lowercase().starts_with("readme")
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

        if file_name == "package.json" {
            Some(ContextFileType::PackageJson)
        } else if file_name == "tsconfig.json" || file_name == "jsconfig.json" {
            Some(ContextFileType::TsConfig)
        } else if file_name.to_lowercase().starts_with("readme")
            || file_path.extension().and_then(|e| e.to_str()) == Some("md")
        {
            Some(ContextFileType::Markdown)
        } else {
            None
        }
    }

    /// Generate a documentation analysis prompt based on context file type.
    pub fn documentation_prompt(&self, file_path: &str, content: &str) -> String {
        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == "package.json" {
            self.package_json_prompt(file_path, content)
        } else {
            self.markdown_doc_prompt(file_path, content)
        }
    }

    /// Generate a prompt for analyzing package.json.
    fn package_json_prompt(&self, file_path: &str, content: &str) -> String {
        format!(
            r#"Analyze this package.json file and extract project-level information:

File: {}

```json
{}
```

Provide a concise summary including:
1. **Package Name and Description**: What is this package?
2. **Purpose**: What does this project do based on its description, keywords, and dependencies?
3. **Main Scripts**: What build, test, and development commands are available?
4. **Key Dependencies**: What major libraries/frameworks does it use? (React, Express, etc.)
5. **Dev Stack**: What development tools are configured? (TypeScript, ESLint, Prettier, etc.)
6. **Project Type**: Is this a library, application, monorepo package, etc.?

IMPORTANT: Respond only in English"#,
            file_path, content
        )
    }

    /// Generate a prompt for analyzing markdown documentation.
    fn markdown_doc_prompt(&self, file_path: &str, content: &str) -> String {
        format!(
            r#"Analyze this documentation file and extract project-level information:

File: {}

```markdown
{}
```

Provide a concise summary including:
1. **Purpose**: What is this project/component about?
2. **Key Features**: What are the main capabilities described?
3. **Usage**: How is this meant to be used?
4. **Architecture Notes**: Any architectural patterns or design decisions mentioned?
5. **Dependencies/Requirements**: What does this project depend on?

IMPORTANT: Respond only in English"#,
            file_path, content
        )
    }

    /// Generate a prompt for architecture-focused file analysis.
    pub fn architecture_file_analysis_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this TypeScript/JavaScript file from an ARCHITECTURAL perspective.

File: {}

```typescript
{}
```

Extract ONLY the following (skip if not present):
1. **Layer**: Where does this fit? (presentation/UI, business logic, data access, infrastructure)
2. **Key Abstractions**: Main classes, interfaces, types, or React components defined
3. **Dependencies**: What does this module depend on? (imports from other project files)
4. **Exports**: What does this module provide to others?
5. **Patterns**: Design patterns used (Factory, Strategy, Observer, Hooks, HOCs, etc.)
6. **Component Type**: For React: Container vs Presentational, Custom Hook, Context Provider, etc.

Be concise - this will be aggregated with other files for an overall architecture summary.

IMPORTANT: Respond only in English (or code)"#,
            file_path, code
        )
    }

    /// Generate a prompt for architecture diagram extraction.
    pub fn diagram_architecture_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this TypeScript/JavaScript file for ARCHITECTURAL diagram information.

File: {}

```typescript
{}
```

Extract ONLY the following for diagram generation:
1. **Module Name**: The logical name of this module
2. **Module Type**: api_route, service, utility, react_component, hook, context, middleware, etc.
3. **Dependencies**: List of modules this file imports (from project files, not npm packages)
4. **Exports**: What this module provides
5. **Data Flow**: What data comes in (props, params) and what goes out (return, response)

Format as structured text that can be aggregated later.

IMPORTANT: Respond only in English (or code)"#,
            file_path, code
        )
    }

    /// Generate a prompt for data flow diagram extraction.
    pub fn diagram_data_flow_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this TypeScript/JavaScript file for DATA FLOW diagram information.

File: {}

```typescript
{}
```

Extract ONLY the following for data flow diagram generation:
1. **Data Sources**: External APIs, databases, localStorage, props, context
2. **Data Transformations**: How is data processed or transformed?
3. **Data Sinks**: Where does data go? (state updates, API calls, renders)
4. **State Management**: What state is managed here? (useState, Redux, context)
5. **Side Effects**: API calls, subscriptions, event handlers

Skip if this file has no significant data flow.

IMPORTANT: Respond only in English (or code)"#,
            file_path, code
        )
    }

    /// Generate a prompt for database schema diagram extraction.
    pub fn diagram_database_schema_prompt(&self, file_path: &str, code: &str) -> String {
        format!(
            r#"Analyze this TypeScript/JavaScript file for DATABASE/SCHEMA diagram information.

File: {}

```typescript
{}
```

Extract ONLY the following:
1. **Models/Types**: TypeScript interfaces or types that represent data structures
2. **Database Operations**: Prisma, TypeORM, Mongoose, or raw SQL operations
3. **Relationships**: Foreign keys, associations, or references between types
4. **Validation Rules**: Zod schemas, class-validator decorators, etc.

Skip if this file has no database-related content.

IMPORTANT: Respond only in English (or code)"#,
            file_path, code
        )
    }
}

/// Add line numbers to code for mutation prompts.
fn add_line_numbers(code: &str) -> String {
    code.lines()
        .enumerate()
        .map(|(i, line)| format!("{:5} | {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Truncate output to a maximum length.
fn truncate_output(output: &str, max_len: usize) -> String {
    if output.len() <= max_len {
        output.to_string()
    } else {
        format!("{}...\n(output truncated)", &output[..max_len])
    }
}

/// Extract a failing test name from test output.
fn extract_failing_test(output: &str) -> Option<String> {
    // First pass: look for specific test name markers (● or ❌)
    // These are more specific than file-level failures
    for line in output.lines() {
        // Jest format: "● test name"
        if line.starts_with("●") {
            return Some(line.trim_start_matches('●').trim().to_string());
        }
        // Vitest format: "❌ test name"
        if line.contains("❌") {
            let parts: Vec<&str> = line.split("❌").collect();
            if parts.len() > 1 && !parts[1].trim().is_empty() {
                return Some(parts[1].trim().to_string());
            }
        }
    }

    // Second pass: fall back to file-level failure if no specific test found
    for line in output.lines() {
        if line.starts_with("FAIL ") {
            return Some(line.trim_start_matches("FAIL ").trim().to_string());
        }
    }

    None
}

/// Process the output of a compile command and return Ok or Err based on success.
fn process_compile_output(output: std::process::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!("{}\n{}", stdout, stderr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_find_source_files_empty_dir() {
        let temp = TempDir::new().unwrap();
        let lang = TypeScriptLanguage;
        let files = lang.find_source_files(temp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_find_source_files_with_ts_files() {
        let temp = TempDir::with_prefix("ts_test").unwrap();
        std::fs::write(temp.path().join("index.ts"), "export const x = 1;").unwrap();
        std::fs::write(
            temp.path().join("app.tsx"),
            "export const App = () => <div/>;",
        )
        .unwrap();
        std::fs::write(temp.path().join("main.js"), "console.log('hi');").unwrap();
        std::fs::write(temp.path().join("other.txt"), "not typescript").unwrap();

        let lang = TypeScriptLanguage;
        let files = lang.find_source_files(temp.path()).unwrap();

        assert_eq!(files.len(), 3);
        assert!(files.iter().any(|f| f.ends_with("index.ts")));
        assert!(files.iter().any(|f| f.ends_with("app.tsx")));
        assert!(files.iter().any(|f| f.ends_with("main.js")));
        assert!(!files.iter().any(|f| f.ends_with("other.txt")));
    }

    #[test]
    fn test_find_source_files_excludes_node_modules() {
        let temp = TempDir::new().unwrap();
        let node_modules = temp.path().join("node_modules/some-package");
        std::fs::create_dir_all(&node_modules).unwrap();
        std::fs::write(node_modules.join("index.js"), "module.exports = {};").unwrap();

        let lang = TypeScriptLanguage;
        let files = lang.find_source_files(temp.path()).unwrap();

        assert!(!files
            .iter()
            .any(|f| f.to_string_lossy().contains("node_modules")));
    }

    #[test]
    fn test_find_context_files() {
        let temp = TempDir::with_prefix("ts_context").unwrap();
        std::fs::write(temp.path().join("package.json"), "{}").unwrap();
        std::fs::write(temp.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(temp.path().join("README.md"), "# Hello").unwrap();
        std::fs::write(temp.path().join("index.ts"), "export {}").unwrap();

        let lang = TypeScriptLanguage;
        let files = lang.find_context_files(temp.path()).unwrap();

        assert_eq!(files.len(), 3);
        assert!(files.iter().any(|f| f.ends_with("package.json")));
        assert!(files.iter().any(|f| f.ends_with("tsconfig.json")));
        assert!(files.iter().any(|f| f.ends_with("README.md")));
        assert!(!files.iter().any(|f| f.ends_with("index.ts")));
    }

    #[test]
    fn test_context_file_type() {
        let lang = TypeScriptLanguage;

        assert_eq!(
            lang.context_file_type(Path::new("package.json")),
            Some(ContextFileType::PackageJson)
        );
        assert_eq!(
            lang.context_file_type(Path::new("tsconfig.json")),
            Some(ContextFileType::TsConfig)
        );
        assert_eq!(
            lang.context_file_type(Path::new("README.md")),
            Some(ContextFileType::Markdown)
        );
        assert_eq!(lang.context_file_type(Path::new("index.ts")), None);
    }

    #[test]
    fn test_analysis_prompt_contains_file_path() {
        let lang = TypeScriptLanguage;
        let prompt = lang.analysis_prompt("src/index.ts", "const x = 1;");

        assert!(prompt.contains("src/index.ts"));
        assert!(prompt.contains("const x = 1;"));
        assert!(prompt.contains("TypeScript/JavaScript"));
    }

    #[test]
    fn test_add_line_numbers() {
        let code = "line1\nline2\nline3";
        let numbered = add_line_numbers(code);

        assert!(numbered.contains("    1 | line1"));
        assert!(numbered.contains("    2 | line2"));
        assert!(numbered.contains("    3 | line3"));
    }

    #[test]
    fn test_extract_failing_test_jest() {
        let output = "FAIL src/app.test.ts\n● should work correctly";
        let failing = extract_failing_test(output);

        assert_eq!(failing, Some("should work correctly".to_string()));
    }

    #[test]
    fn test_process_compile_output_success() {
        // Run a command that succeeds to get a valid ExitStatus
        let output = std::process::Command::new("true").output().unwrap();
        assert!(output.status.success());

        let result = process_compile_output(output);
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_compile_output_failure() {
        // Run a command that fails to get a failure ExitStatus
        let output = std::process::Command::new("false").output().unwrap();
        assert!(!output.status.success());

        let result = process_compile_output(output);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_compile_output_failure_contains_output() {
        // Run a command that fails and produces stderr
        let output = std::process::Command::new("sh")
            .args(["-c", "echo 'error message' >&2; exit 1"])
            .output()
            .unwrap();

        let result = process_compile_output(output);
        assert!(result.is_err());
        let err_msg = result.unwrap_err();
        assert!(err_msg.contains("error message"));
    }
}
