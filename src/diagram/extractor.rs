//! Diagram extraction prompts for per-file analysis.
//!
//! This module provides prompts for the first phase of diagram generation:
//! extracting diagram-relevant information from individual source files.

use super::DiagramType;

/// Provides prompts for extracting diagram-relevant information from source files
pub struct DiagramExtractor;

impl DiagramExtractor {
    /// Get the extraction prompt for a specific diagram type
    pub fn prompt_for_type(diagram_type: DiagramType, file_path: &str, code: &str) -> String {
        match diagram_type {
            DiagramType::SystemArchitecture => Self::architecture_prompt(file_path, code),
            DiagramType::DataFlow => Self::data_flow_prompt(file_path, code),
            DiagramType::DatabaseSchema => Self::database_schema_prompt(file_path, code),
        }
    }

    /// Prompt for extracting architecture-relevant information from a file
    pub fn architecture_prompt(file_path: &str, code: &str) -> String {
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

    /// Prompt for extracting data flow information from a file
    pub fn data_flow_prompt(file_path: &str, code: &str) -> String {
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

    /// Prompt for extracting database schema information from a file
    pub fn database_schema_prompt(file_path: &str, code: &str) -> String {
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

    /// Prompt for architecture-focused file analysis (used for Architecture tab)
    /// This is different from the granular code_understanding used for File Analysis tab
    pub fn architecture_file_analysis_prompt(file_path: &str, code: &str) -> String {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_architecture_prompt_contains_file_path() {
        let prompt = DiagramExtractor::architecture_prompt("src/main.rs", "fn main() {}");
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("fn main()"));
    }

    #[test]
    fn test_data_flow_prompt_contains_expected_sections() {
        let prompt = DiagramExtractor::data_flow_prompt("src/handler.rs", "async fn handle() {}");
        assert!(prompt.contains("Data Sources"));
        assert!(prompt.contains("Data Transformations"));
        assert!(prompt.contains("Data Sinks"));
        assert!(prompt.contains("Async Boundaries"));
    }

    #[test]
    fn test_database_schema_prompt_contains_expected_sections() {
        let prompt = DiagramExtractor::database_schema_prompt("src/db.rs", "struct User {}");
        assert!(prompt.contains("Database Models"));
        assert!(prompt.contains("Table Relationships"));
        assert!(prompt.contains("SQL Operations"));
        assert!(prompt.contains("Schema Migrations"));
    }

    #[test]
    fn test_prompt_for_type_dispatches_correctly() {
        let arch_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::SystemArchitecture, "test.rs", "code");
        assert!(arch_prompt.contains("ARCHITECTURAL"));

        let flow_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::DataFlow, "test.rs", "code");
        assert!(flow_prompt.contains("DATA FLOW"));

        let db_prompt =
            DiagramExtractor::prompt_for_type(DiagramType::DatabaseSchema, "test.rs", "code");
        assert!(db_prompt.contains("DATABASE"));
    }

    #[test]
    fn test_architecture_file_analysis_prompt() {
        let prompt = DiagramExtractor::architecture_file_analysis_prompt(
            "src/web/mod.rs",
            "pub mod handlers;",
        );
        assert!(prompt.contains("ARCHITECTURAL"));
        assert!(prompt.contains("Layer"));
        assert!(prompt.contains("Key Abstractions"));
    }
}
