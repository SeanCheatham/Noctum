//! GraphViz DOT diagram generation prompts.
//!
//! This module provides prompts for the second phase of diagram generation:
//! aggregating per-file extractions into final DOT diagrams.

use super::DiagramType;

/// Provides prompts for generating DOT diagrams from aggregated extractions
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

    /// Generate a system architecture DOT diagram
    pub fn architecture_diagram_prompt(repo_name: &str, extractions: &str) -> String {
        format!(
            r#"Generate a GraphViz DOT diagram showing the system architecture of '{}'.

Based on these file analyses:
{}

Create a DOT digraph that shows:
- Main modules/components as labeled nodes
- Dependencies between modules as directed edges with labels
- Group related modules into subgraph clusters
- External dependencies as separate nodes

DOT syntax reference:
- Graph declaration: `digraph G {{ ... }}`
- Nodes with labels: `node_name [label="Display Label"];`
- Edges with labels: `source -> target [label="relationship"];`
- Clusters: `subgraph cluster_name {{ label="Group"; node1; node2; }}`
- Layout direction: `rankdir=LR;` for left-to-right

Example structure:
```
digraph Architecture {{
    rankdir=TB;

    subgraph cluster_web {{
        label="Web Layer";
        handlers [label="HTTP Handlers"];
        templates [label="Templates"];
    }}

    subgraph cluster_db {{
        label="Database Layer";
        models [label="Models"];
        queries [label="Queries"];
    }}

    handlers -> queries [label="executes"];
    templates -> handlers [label="renders"];
}}
```

Rules:
1. Use snake_case for node names (no spaces, no special chars except underscore)
2. Use descriptive labels in quotes
3. Keep the diagram focused - show major components, not every file
4. Group by architectural layer using subgraph clusters
5. Prefix cluster names with "cluster_" for proper rendering

Output ONLY valid DOT code. No markdown code fences. No explanations."#,
            repo_name, extractions
        )
    }

    /// Generate a data flow DOT diagram
    pub fn data_flow_diagram_prompt(repo_name: &str, extractions: &str) -> String {
        format!(
            r#"Generate a GraphViz DOT diagram showing data flow in '{}'.

Based on these file analyses:
{}

Create a DOT digraph showing:
- Data sources on the left (users, external APIs, files, etc.)
- Processing stages in the middle
- Data sinks on the right (databases, responses, files, etc.)
- Directed edges showing data movement with labels describing the data

DOT syntax reference:
- Graph: `digraph DataFlow {{ rankdir=LR; ... }}`
- Nodes: `node_name [label="Label"];`
- Edges: `source -> target [label="data description"];`
- Clusters for grouping: `subgraph cluster_sources {{ label="Sources"; ... }}`

Example structure:
```
digraph DataFlow {{
    rankdir=LR;

    subgraph cluster_sources {{
        label="Data Sources";
        user [label="User Request"];
        config [label="Config Files"];
    }}

    subgraph cluster_processing {{
        label="Processing";
        validation [label="Validation"];
        transform [label="Transform"];
    }}

    subgraph cluster_sinks {{
        label="Data Sinks";
        database [label="Database"];
        response [label="HTTP Response"];
    }}

    user -> validation [label="JSON payload"];
    validation -> transform [label="validated data"];
    transform -> database [label="model objects"];
    transform -> response [label="JSON response"];
}}
```

Rules:
1. Use snake_case for node names
2. Show the main data paths, not every detail
3. Label edges with what data flows through them
4. Use rankdir=LR for left-to-right flow
5. Group related elements in clusters

Output ONLY valid DOT code. No markdown code fences. No explanations."#,
            repo_name, extractions
        )
    }

    /// Generate a database schema DOT diagram
    pub fn database_schema_diagram_prompt(repo_name: &str, extractions: &str) -> String {
        format!(
            r#"Generate a GraphViz DOT diagram showing the database schema for '{}'.

Based on these file analyses:
{}

Create a DOT digraph showing:
- Each database table as a record-shaped node
- Key columns listed inside each table
- Foreign key relationships as edges between tables

DOT syntax for tables using record shapes:
```
digraph Schema {{
    rankdir=LR;
    node [shape=record];

    users [label="{{users|id: INTEGER PK|name: TEXT|email: TEXT|created_at: TIMESTAMP}}"];
    posts [label="{{posts|id: INTEGER PK|user_id: INTEGER FK|title: TEXT|content: TEXT}}"];

    posts -> users [label="belongs_to"];
}}
```

Alternative HTML-like label syntax:
```
digraph Schema {{
    rankdir=LR;
    node [shape=plaintext];

    users [label=<
        <TABLE BORDER="1" CELLBORDER="0" CELLSPACING="0">
        <TR><TD BGCOLOR="lightblue"><B>users</B></TD></TR>
        <TR><TD>id: INTEGER PK</TD></TR>
        <TR><TD>name: TEXT</TD></TR>
        </TABLE>
    >];
}}
```

Rules:
1. Use the actual table names from the codebase
2. Mark primary keys (PK) and foreign keys (FK)
3. Show only the most important columns (5-7 max per table)
4. Draw edges for foreign key relationships
5. Use record or plaintext shapes for table rendering

If no database tables are found in the extractions, output:
```
digraph Schema {{
    no_schema [label="No database schema detected"];
}}
```

Output ONLY valid DOT code. No markdown code fences. No explanations."#,
            repo_name, extractions
        )
    }

    /// Prompt to fix invalid DOT syntax
    pub fn fix_dot_prompt(dot_code: &str, error_message: &str) -> String {
        format!(
            r#"The following GraphViz DOT diagram has a syntax error:

{}

Error: {}

Fix the DOT syntax error and output the corrected diagram.

Common fixes:
- Ensure all braces {{ }} are balanced
- Use snake_case for node names (no spaces or special characters)
- Put labels in quotes: `node [label="My Label"];`
- Edges use -> for digraph (not - or -->)
- Statements should end with semicolons
- Graph must start with `digraph Name {{ ... }}`
- Cluster subgraphs must start with "cluster_" prefix

Output ONLY the corrected DOT code. No markdown code fences. No explanations."#,
            dot_code, error_message
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
    fn test_architecture_diagram_prompt_contains_dot_syntax() {
        let prompt = DiagramGenerator::architecture_diagram_prompt("test", "extractions");
        assert!(prompt.contains("DOT"));
        assert!(prompt.contains("digraph"));
        assert!(prompt.contains("->"));
        assert!(prompt.contains("snake_case"));
    }

    #[test]
    fn test_data_flow_diagram_prompt_contains_rankdir() {
        let prompt = DiagramGenerator::data_flow_diagram_prompt("test", "extractions");
        assert!(prompt.contains("rankdir"));
        assert!(prompt.contains("LR"));
    }

    #[test]
    fn test_database_schema_diagram_prompt_contains_record() {
        let prompt = DiagramGenerator::database_schema_diagram_prompt("test", "extractions");
        assert!(prompt.contains("record"));
        assert!(prompt.contains("PK"));
        assert!(prompt.contains("FK"));
    }

    #[test]
    fn test_fix_dot_prompt_contains_error() {
        let prompt = DiagramGenerator::fix_dot_prompt("digraph { broken", "Unbalanced braces");
        assert!(prompt.contains("digraph { broken"));
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
