//! LLM-based mutation analysis - discovers mutation points and generates mutations.

use crate::analyzer::OllamaClient;
use crate::mutation::{GeneratedMutation, Replacement};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;

/// A single replacement as returned by the LLM.
#[derive(Debug, Deserialize)]
struct RawReplacement {
    /// Line number (1-based) where this replacement is located.
    /// May be adjusted ±3 lines to find the actual match in the source.
    line_number: usize,
    /// The exact text to find (a small expression or fragment)
    find: String,
    /// The replacement text
    replace: String,
}

/// A mutation as returned by the LLM (may have multiple replacements).
#[derive(Debug, Deserialize)]
struct RawMutation {
    /// All replacements for this mutation (e.g., import + main change)
    replacements: Vec<RawReplacement>,
    /// Why this is a high-value mutation point
    reasoning: String,
    /// Brief description of the change (e.g., "Changed > to >=")
    description: String,
}

/// Response structure for mutation analysis
#[derive(Debug, Deserialize)]
struct AnalysisResponse {
    mutations: Vec<RawMutation>,
}

/// Response structure for fixing a single mutation
#[derive(Debug, Deserialize)]
struct FixMutationResponse {
    replacements: Vec<RawReplacement>,
    reasoning: String,
    description: String,
}

/// JSON schema for structured output (mutation analysis)
fn analysis_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "mutations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "replacements": {
                            "type": "array",
                            "description": "All replacements for this mutation. Use multiple when adding imports.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "line_number": {
                                        "type": "integer",
                                        "description": "Approximate line number (1-based) where this replacement is located"
                                    },
                                    "find": {
                                        "type": "string",
                                        "description": "The exact text to find and replace (a small expression like 'count > 0' or 'true')"
                                    },
                                    "replace": {
                                        "type": "string",
                                        "description": "The replacement text (e.g., 'count >= 0' or 'false')"
                                    }
                                },
                                "required": ["line_number", "find", "replace"]
                            }
                        },
                        "reasoning": {
                            "type": "string",
                            "description": "Why this is a high-value mutation point worth testing"
                        },
                        "description": {
                            "type": "string",
                            "description": "Brief description of what was changed (e.g., 'Changed > to >=')"
                        }
                    },
                    "required": ["replacements", "reasoning", "description"]
                }
            }
        },
        "required": ["mutations"]
    })
}

/// JSON schema for fixing a mutation that failed to compile
fn fix_mutation_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "replacements": {
                "type": "array",
                "description": "All replacements for the fixed mutation. Add import replacements if needed.",
                "items": {
                    "type": "object",
                    "properties": {
                        "line_number": {
                            "type": "integer",
                            "description": "Approximate line number (1-based) where this replacement is located"
                        },
                        "find": {
                            "type": "string",
                            "description": "The exact text to find and replace"
                        },
                        "replace": {
                            "type": "string",
                            "description": "The replacement text"
                        }
                    },
                    "required": ["line_number", "find", "replace"]
                }
            },
            "reasoning": {
                "type": "string",
                "description": "Why this mutation is valuable and how you fixed the compile error"
            },
            "description": {
                "type": "string",
                "description": "Brief description of the mutation (e.g., 'Changed > to >=')"
            }
        },
        "required": ["replacements", "reasoning", "description"]
    })
}

/// Add line numbers to code for better LLM alignment.
fn add_line_numbers(code: &str) -> String {
    code.lines()
        .enumerate()
        .map(|(i, line)| format!("{:4} | {}", i + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Generate the analysis prompt
fn analysis_prompt(file_path: &str, code: &str) -> String {
    let numbered_code = add_line_numbers(code);
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
- Skip comments, type definitions, and test code
- Each mutation has a "replacements" array - use multiple replacements if you need to add imports

File: {file_path}

```
{numbered_code}
```

For each mutation provide:
- replacements: Array of {{line_number, find, replace}} objects
- reasoning: Why this tests important logic
- description: What changed (e.g., "Changed > to >=")

Example for line `   42 |     if count > 0 {{`:
  replacements: [{{"line_number": 42, "find": "count > 0", "replace": "count >= 0"}}]
  description: "Changed > to >="

Example mutation requiring an import:
  replacements: [
    {{"line_number": 3, "find": "use std::io;", "replace": "use std::io;\nuse std::fs;"}},
    {{"line_number": 42, "find": "io::stdin()", "replace": "fs::File::open(\"x\")"}}
  ]
  description: "Changed stdin to file read""#
    )
}

/// Generate the prompt for fixing a failed mutation
fn fix_mutation_prompt(
    file_path: &str,
    code: &str,
    failed_mutation: &GeneratedMutation,
    compile_error: &str,
    attempt: u8,
) -> String {
    let numbered_code = add_line_numbers(code);
    let replacements_json = serde_json::to_string_pretty(&failed_mutation.replacements)
        .unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"You previously generated a mutation that caused a compile error. Fix it.

File: {file_path}

```
{numbered_code}
```

Failed mutation:
- Description: {description}
- Replacements: {replacements_json}

Compile error:
```
{compile_error}
```

Fix the mutation so it compiles. Common fixes:
- Add missing imports as a separate replacement
- Fix type mismatches in the replacement text
- Ensure the "find" text matches EXACTLY what's in the code
- If the error mentions an unknown type/function, add the appropriate use statement

This is attempt {attempt}/3. Return a corrected mutation with:
- replacements: Array of {{line_number, find, replace}} - include import additions if needed
- reasoning: Why this mutation is valuable and how you fixed it
- description: Brief description of the mutation"#,
        description = failed_mutation.description,
        compile_error = truncate_error(compile_error, 2000),
    )
}

/// Truncate error output to avoid huge prompts
fn truncate_error(error: &str, max_len: usize) -> &str {
    if error.len() <= max_len {
        error
    } else {
        &error[..max_len]
    }
}

/// Line tolerance when searching for the "find" text.
/// We'll search ±TOLERANCE lines from the given line number.
const LINE_TOLERANCE: usize = 3;

/// Analyze a file and generate mutations in a single LLM call.
///
/// Returns a list of ready-to-test mutations with their replacements.
pub async fn analyze_and_generate_mutations(
    client: &OllamaClient,
    file_path: &str,
    code: &str,
    max_mutations: usize,
) -> Result<Vec<GeneratedMutation>> {
    let prompt = analysis_prompt(file_path, code);
    let schema = analysis_schema();

    let parsed: AnalysisResponse = client
        .generate_structured(&prompt, schema)
        .await
        .context("Failed to get structured response for mutation analysis")?;

    let lines: Vec<&str> = code.lines().collect();
    let line_count = lines.len();

    let mutations: Vec<GeneratedMutation> = parsed
        .mutations
        .into_iter()
        .take(max_mutations * 2) // Take extra since some may be filtered
        .filter_map(|raw| {
            // Validate we have at least one replacement
            if raw.replacements.is_empty() {
                tracing::warn!("Mutation has no replacements: {}", raw.description);
                return None;
            }

            // Validate and adjust each replacement
            let mut validated_replacements = Vec::with_capacity(raw.replacements.len());

            for raw_repl in raw.replacements {
                // Validate line number is roughly in range
                if raw_repl.line_number == 0 || raw_repl.line_number > line_count + LINE_TOLERANCE {
                    tracing::warn!(
                        "Line number {} out of range (file has {} lines)",
                        raw_repl.line_number,
                        line_count
                    );
                    return None;
                }

                // Validate find/replace are not empty and different
                if raw_repl.find.is_empty() {
                    tracing::warn!("Empty 'find' text for mutation: {}", raw.description);
                    return None;
                }
                if raw_repl.find == raw_repl.replace {
                    tracing::warn!("'find' and 'replace' are identical: {}", raw_repl.find);
                    return None;
                }

                // Search for the "find" text within the line window
                let start_line = raw_repl.line_number.saturating_sub(LINE_TOLERANCE).max(1);
                let end_line = (raw_repl.line_number + LINE_TOLERANCE).min(line_count);

                let mut found_line = None;
                for line_num in start_line..=end_line {
                    if lines[line_num - 1].contains(&raw_repl.find) {
                        found_line = Some(line_num);
                        break;
                    }
                }

                let actual_line = match found_line {
                    Some(ln) => ln,
                    None => {
                        // Try searching the whole file as fallback
                        let global_match = lines.iter().position(|l| l.contains(&raw_repl.find));
                        match global_match {
                            Some(idx) => {
                                tracing::debug!(
                                    "Found '{}' at line {} (LLM said line {})",
                                    raw_repl.find,
                                    idx + 1,
                                    raw_repl.line_number
                                );
                                idx + 1
                            }
                            None => {
                                tracing::warn!(
                                    "Could not find '{}' in file {} (LLM suggested line {})",
                                    raw_repl.find,
                                    file_path,
                                    raw_repl.line_number
                                );
                                return None;
                            }
                        }
                    }
                };

                validated_replacements.push(Replacement {
                    line_number: actual_line,
                    find: raw_repl.find,
                    replace: raw_repl.replace,
                });
            }

            Some(GeneratedMutation {
                file_path: file_path.to_string(),
                replacements: validated_replacements,
                reasoning: raw.reasoning,
                description: raw.description,
            })
        })
        .take(max_mutations)
        .collect();

    tracing::debug!("Generated {} mutations for {}", mutations.len(), file_path);

    Ok(mutations)
}

/// Attempt to fix a mutation that caused a compile error.
///
/// Re-prompts the LLM with the original code, the failed mutation,
/// and the compile error, asking it to produce a corrected mutation.
pub async fn fix_mutation_with_error(
    client: &OllamaClient,
    file_path: &str,
    code: &str,
    failed_mutation: &GeneratedMutation,
    compile_error: &str,
    attempt: u8,
) -> Result<GeneratedMutation> {
    let prompt = fix_mutation_prompt(file_path, code, failed_mutation, compile_error, attempt);
    let schema = fix_mutation_schema();

    let parsed: FixMutationResponse = client
        .generate_structured(&prompt, schema)
        .await
        .context("Failed to get structured response for mutation fix")?;

    // Validate replacements (similar to analyze_and_generate_mutations)
    let lines: Vec<&str> = code.lines().collect();
    let line_count = lines.len();

    if parsed.replacements.is_empty() {
        anyhow::bail!("Fixed mutation has no replacements");
    }

    let mut validated_replacements = Vec::with_capacity(parsed.replacements.len());

    for raw_repl in parsed.replacements {
        // Validate line number
        if raw_repl.line_number == 0 || raw_repl.line_number > line_count + LINE_TOLERANCE {
            anyhow::bail!(
                "Line number {} out of range (file has {} lines)",
                raw_repl.line_number,
                line_count
            );
        }

        // Validate find/replace
        if raw_repl.find.is_empty() {
            anyhow::bail!("Empty 'find' text in fixed mutation");
        }
        if raw_repl.find == raw_repl.replace {
            anyhow::bail!("'find' and 'replace' are identical: {}", raw_repl.find);
        }

        // Search for the "find" text
        let start_line = raw_repl.line_number.saturating_sub(LINE_TOLERANCE).max(1);
        let end_line = (raw_repl.line_number + LINE_TOLERANCE).min(line_count);

        let mut found_line = None;
        for line_num in start_line..=end_line {
            if lines[line_num - 1].contains(&raw_repl.find) {
                found_line = Some(line_num);
                break;
            }
        }

        let actual_line = match found_line {
            Some(ln) => ln,
            None => {
                // Try searching the whole file as fallback
                lines
                    .iter()
                    .position(|l| l.contains(&raw_repl.find))
                    .map(|idx| idx + 1)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Could not find '{}' in file (LLM suggested line {})",
                            raw_repl.find,
                            raw_repl.line_number
                        )
                    })?
            }
        };

        validated_replacements.push(Replacement {
            line_number: actual_line,
            find: raw_repl.find,
            replace: raw_repl.replace,
        });
    }

    Ok(GeneratedMutation {
        file_path: file_path.to_string(),
        replacements: validated_replacements,
        reasoning: parsed.reasoning,
        description: parsed.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_line_numbers() {
        let code = "fn foo() {\n    bar()\n}";
        let numbered = add_line_numbers(code);

        assert!(numbered.contains("   1 | fn foo() {"));
        assert!(numbered.contains("   2 |     bar()"));
        assert!(numbered.contains("   3 | }"));
    }

    #[test]
    fn test_add_line_numbers_empty() {
        let numbered = add_line_numbers("");
        assert_eq!(numbered, "");
    }

    #[test]
    fn test_analysis_prompt_contains_file_path() {
        let prompt = analysis_prompt("src/lib.rs", "fn foo() {}");
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("   1 | fn foo() {}"));
    }
}
