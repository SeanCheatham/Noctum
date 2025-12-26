//! LLM-based mutation analysis - discovers mutation points and generates mutations.

use crate::analyzer::OllamaClient;
use crate::mutation::GeneratedMutation;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;

/// A mutation as returned by the LLM (line number + search/replace)
#[derive(Debug, Deserialize)]
struct RawMutation {
    /// Line number (1-based) where the mutation is located.
    /// May be adjusted ±3 lines to find the actual match in the source.
    line_number: usize,
    /// The exact text to find (a small expression or fragment)
    find: String,
    /// The replacement text
    replace: String,
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

/// JSON schema for structured output
fn analysis_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "mutations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "line_number": {
                            "type": "integer",
                            "description": "Approximate line number (1-based) where the mutation is located"
                        },
                        "find": {
                            "type": "string",
                            "description": "The exact text to find and replace (a small expression like 'count > 0' or 'true')"
                        },
                        "replace": {
                            "type": "string",
                            "description": "The replacement text (e.g., 'count >= 0' or 'false')"
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
                    "required": ["line_number", "find", "replace", "reasoning", "description"]
                }
            }
        },
        "required": ["mutations"]
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
            // Validate line number is roughly in range
            if raw.line_number == 0 || raw.line_number > line_count + LINE_TOLERANCE {
                tracing::warn!(
                    "Line number {} out of range (file has {} lines)",
                    raw.line_number, line_count
                );
                return None;
            }

            // Validate find/replace are not empty and different
            if raw.find.is_empty() {
                tracing::warn!("Empty 'find' text for mutation: {}", raw.description);
                return None;
            }
            if raw.find == raw.replace {
                tracing::warn!("'find' and 'replace' are identical: {}", raw.find);
                return None;
            }

            // Search for the "find" text within the line window
            let start_line = raw.line_number.saturating_sub(LINE_TOLERANCE).max(1);
            let end_line = (raw.line_number + LINE_TOLERANCE).min(line_count);

            let mut found_line = None;
            for line_num in start_line..=end_line {
                if lines[line_num - 1].contains(&raw.find) {
                    found_line = Some(line_num);
                    break;
                }
            }

            let actual_line = match found_line {
                Some(ln) => ln,
                None => {
                    // Try searching the whole file as fallback
                    let global_match = lines.iter().position(|l| l.contains(&raw.find));
                    match global_match {
                        Some(idx) => {
                            tracing::debug!(
                                "Found '{}' at line {} (LLM said line {})",
                                raw.find,
                                idx + 1,
                                raw.line_number
                            );
                            idx + 1
                        }
                        None => {
                            tracing::warn!(
                                "Could not find '{}' in file {} (LLM suggested line {})",
                                raw.find, file_path, raw.line_number
                            );
                            return None;
                        }
                    }
                }
            };

            Some(GeneratedMutation {
                file_path: file_path.to_string(),
                line_number: actual_line,
                find: raw.find,
                replace: raw.replace,
                reasoning: raw.reasoning,
                description: raw.description,
            })
        })
        .take(max_mutations)
        .collect();

    tracing::debug!("Generated {} mutations for {}", mutations.len(), file_path);

    Ok(mutations)
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
