//! D2 diagram generation prompts.
//!
//! This module provides prompts for the second phase of diagram generation:
//! aggregating per-file extractions into final D2 diagrams.

use super::DiagramType;

/// Provides prompts for generating D2 diagrams from aggregated extractions
pub struct DiagramGenerator;

impl DiagramGenerator {
    /// Get the generation prompt for a specific diagram type
    pub fn prompt_for_type(
        diagram_type: DiagramType,
        repo_name: &str,
        extractions: &str,
    ) -> String {
        match diagram_type {
            DiagramType::SystemArchitecture => {
                Self::architecture_diagram_prompt(repo_name, extractions)
            }
            DiagramType::DataFlow => Self::data_flow_diagram_prompt(repo_name, extractions),
            DiagramType::DatabaseSchema => {
                Self::database_schema_diagram_prompt(repo_name, extractions)
            }
        }
    }

    /// Generate a system architecture D2 diagram
    pub fn architecture_diagram_prompt(repo_name: &str, extractions: &str) -> String {
        format!(
            r#"Generate a D2 diagram showing the system architecture of '{}'.

Based on these file analyses:
{}

Create a D2 diagram that shows:
- Main modules/components as labeled shapes
- Dependencies between modules as arrows with descriptive labels
- Group related modules into containers when logical
- External dependencies as separate shapes

D2 syntax reference:
- Shapes: `component_name: "Display Label"`
- Arrows: `source -> target: "relationship"`
- Containers: `layer: Layer Name {{ child1; child2 }}`
- Bidirectional: `a <-> b`

Example structure:
```
web: Web Layer {{
    handlers: HTTP Handlers
    templates: Templates
}}

db: Database Layer {{
    models: Models
    queries: Queries
}}

web.handlers -> db.queries: executes
```

Rules:
1. Use snake_case for identifiers (no spaces, no special chars except underscore)
2. Use descriptive labels in quotes
3. Keep the diagram focused - show major components, not every file
4. Group by architectural layer when possible

Output ONLY valid D2 code. No markdown code fences. No explanations."#,
            repo_name, extractions
        )
    }

    /// Generate a data flow D2 diagram
    pub fn data_flow_diagram_prompt(repo_name: &str, extractions: &str) -> String {
        format!(
            r#"Generate a D2 diagram showing data flow in '{}'.

Based on these file analyses:
{}

Create a D2 diagram showing:
- Data sources on the left (users, external APIs, files, etc.)
- Processing stages in the middle
- Data sinks on the right (databases, responses, files, etc.)
- Arrows showing data movement with labels describing the data

D2 syntax reference:
- Shapes: `name: "Label"`
- Arrows: `source -> target: "data description"`
- Direction hint: Add `.direction: right` to encourage left-to-right flow

Example structure:
```
direction: right

sources: Data Sources {{
    user: User Request
    config: Config Files
}}

processing: Processing {{
    validation: Validation
    transform: Transform
}}

sinks: Data Sinks {{
    database: Database
    response: HTTP Response
}}

sources.user -> processing.validation: JSON payload
processing.validation -> processing.transform: validated data
processing.transform -> sinks.database: model objects
processing.transform -> sinks.response: JSON response
```

Rules:
1. Use snake_case for identifiers
2. Show the main data paths, not every detail
3. Label arrows with what data flows through them
4. Group related elements

Output ONLY valid D2 code. No markdown code fences. No explanations."#,
            repo_name, extractions
        )
    }

    /// Generate a database schema D2 diagram
    pub fn database_schema_diagram_prompt(repo_name: &str, extractions: &str) -> String {
        format!(
            r#"Generate a D2 diagram showing the database schema for '{}'.

Based on these file analyses:
{}

Create a D2 diagram showing:
- Each database table as a shape
- Key columns listed inside each table
- Foreign key relationships as arrows between tables

D2 syntax for tables:
```
users: users {{
    shape: sql_table
    id: INTEGER PK
    name: TEXT
    email: TEXT
    created_at: TIMESTAMP
}}

posts: posts {{
    shape: sql_table
    id: INTEGER PK
    user_id: INTEGER FK
    title: TEXT
    content: TEXT
}}

posts.user_id -> users.id: belongs_to
```

Rules:
1. Use the actual table names from the codebase
2. Include primary keys (PK) and foreign keys (FK)
3. Show only the most important columns (5-7 max per table)
4. Draw arrows for foreign key relationships
5. Use `shape: sql_table` for proper table rendering

If no database tables are found in the extractions, create a simple diagram showing "No database schema detected".

Output ONLY valid D2 code. No markdown code fences. No explanations."#,
            repo_name, extractions
        )
    }

    /// Prompt to fix invalid D2 syntax
    pub fn fix_d2_prompt(d2_code: &str, error_message: &str) -> String {
        format!(
            r#"The following D2 diagram has a syntax error:

{}

Error: {}

Fix the D2 syntax error and output the corrected diagram.

Common fixes:
- Ensure all braces {{ }} are balanced
- Use snake_case for identifiers (no spaces or special characters)
- Put labels with spaces in quotes: `name: "My Label"`
- Arrows use -> not - or -->
- Check for typos in shape names

Output ONLY the corrected D2 code. No markdown code fences. No explanations."#,
            d2_code, error_message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_architecture_diagram_prompt_contains_repo_name() {
        let prompt =
            DiagramGenerator::architecture_diagram_prompt("my-project", "file extractions here");
        assert!(prompt.contains("my-project"));
        assert!(prompt.contains("file extractions here"));
    }

    #[test]
    fn test_architecture_diagram_prompt_contains_d2_syntax() {
        let prompt = DiagramGenerator::architecture_diagram_prompt("test", "extractions");
        assert!(prompt.contains("D2 syntax"));
        assert!(prompt.contains("->"));
        assert!(prompt.contains("snake_case"));
    }

    #[test]
    fn test_data_flow_diagram_prompt_contains_direction() {
        let prompt = DiagramGenerator::data_flow_diagram_prompt("test", "extractions");
        assert!(prompt.contains("direction"));
        assert!(prompt.contains("left"));
        assert!(prompt.contains("right"));
    }

    #[test]
    fn test_database_schema_diagram_prompt_contains_sql_table() {
        let prompt = DiagramGenerator::database_schema_diagram_prompt("test", "extractions");
        assert!(prompt.contains("sql_table"));
        assert!(prompt.contains("PK"));
        assert!(prompt.contains("FK"));
    }

    #[test]
    fn test_fix_d2_prompt_contains_error() {
        let prompt = DiagramGenerator::fix_d2_prompt("broken { code", "Unbalanced braces");
        assert!(prompt.contains("broken { code"));
        assert!(prompt.contains("Unbalanced braces"));
        assert!(prompt.contains("Fix"));
    }

    #[test]
    fn test_prompt_for_type_dispatches_correctly() {
        let arch_prompt = DiagramGenerator::prompt_for_type(
            DiagramType::SystemArchitecture,
            "repo",
            "extractions",
        );
        assert!(arch_prompt.contains("system architecture"));

        let flow_prompt =
            DiagramGenerator::prompt_for_type(DiagramType::DataFlow, "repo", "extractions");
        assert!(flow_prompt.contains("data flow"));

        let db_prompt =
            DiagramGenerator::prompt_for_type(DiagramType::DatabaseSchema, "repo", "extractions");
        assert!(db_prompt.contains("database schema"));
    }
}
