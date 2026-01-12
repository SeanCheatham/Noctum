//! Language-agnostic analysis helpers.
//!
//! This module provides generic prompts and helpers for code analysis,
//! mutation testing, and diagram generation. The LLM infers the programming
//! language from the code content itself.

use std::path::{Path, PathBuf};

/// Generate a generic code analysis prompt.
///
/// The LLM will infer the programming language from the code content.
pub fn analysis_prompt(file_path: &str, content: &str) -> String {
    format!(
        "Analyze the following code and provide a brief summary of what it does:\n\n\
         File: {}\n\n\
         ```\n{}\n```\n\n\
         Provide a concise analysis including:\n\
         1. Purpose of the code\n\
         2. Key functions/types\n\
         3. Any potential issues or improvements\n\
         4. Up to two specific code modification recommendations\n\n\
         IMPORTANT: Respond only in English (or code)",
        file_path, content
    )
}

/// Generate a prompt for documentation/context file analysis.
pub fn documentation_prompt(file_path: &str, content: &str) -> String {
    let path = Path::new(file_path);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Detect file type and use appropriate prompt
    if file_name == "Cargo.toml" {
        cargo_toml_prompt(file_path, content)
    } else if file_name == "package.json" {
        package_json_prompt(file_path, content)
    } else if extension == "md" || file_name.to_lowercase().starts_with("readme") {
        markdown_doc_prompt(file_path, content)
    } else {
        // Generic context file prompt
        markdown_doc_prompt(file_path, content)
    }
}

fn cargo_toml_prompt(file_path: &str, content: &str) -> String {
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

fn package_json_prompt(file_path: &str, content: &str) -> String {
    format!(
        r#"Analyze this package.json file for PROJECT STRUCTURE information.

File: {}

```json
{}
```

Extract the following architectural context:

1. **Project Identity**: Package name, version, and description

2. **Project Type**: Is this a library, application, or monorepo? What does it produce?

3. **Key Dependencies**: List the most important dependencies and their purpose:
   - Framework (React, Vue, Next.js, Express, etc.)
   - Database/ORM (Prisma, TypeORM, etc.)
   - Build tools (TypeScript, Webpack, Vite, etc.)
   - Testing (Jest, Vitest, etc.)
   - Other significant packages

4. **Scripts**: What are the main npm scripts for building, testing, and running?

5. **Workspace Structure**: If this is a monorepo, what are the workspace packages?

Keep the analysis concise and focused on what these dependencies tell us about the project's architecture.

IMPORTANT: Respond only in English."#,
        file_path, content
    )
}

fn markdown_doc_prompt(file_path: &str, content: &str) -> String {
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

/// Generate a prompt for architecture-focused file analysis.
pub fn architecture_file_analysis_prompt(file_path: &str, code: &str) -> String {
    format!(
        r#"Analyze this source file from an ARCHITECTURAL perspective.

File: {}

```
{}
```

Provide a brief architectural analysis including:

1. **Purpose**: What is the primary responsibility of this module? (1 sentence)

2. **Layer**: What architectural layer does this belong to?
   - Domain/Business Logic
   - Application/Use Cases
   - Infrastructure/Adapters
   - Presentation/UI
   - Cross-cutting concerns

3. **Key Abstractions**: What are the main types, traits/interfaces, or functions?

4. **Dependencies**: What external modules or crates does this import? What does this suggest about its role?

5. **Design Patterns**: Any notable patterns (Repository, Factory, Strategy, etc.)?

6. **Integration Points**: How does this module interact with other parts of the system?

Keep the analysis concise and focused on architectural significance.
If this appears to be a generated file, test file, or utility with no architectural significance, say "No architectural significance".

IMPORTANT: Respond only in English."#,
        file_path, code
    )
}

/// Generate a prompt for extracting system architecture diagram information.
pub fn diagram_architecture_prompt(file_path: &str, code: &str) -> String {
    format!(
        r#"Analyze this source file for ARCHITECTURAL DIAGRAM information.

File: {}

```
{}
```

Extract information for creating a system architecture diagram (Graphviz DOT format).

Look for:
1. **Modules/Components**: What logical units exist in this file?
2. **External Dependencies**: What external services, databases, or systems are referenced?
3. **Internal Dependencies**: What other modules in this codebase does this depend on?
4. **Interfaces**: What APIs or interfaces does this expose?

Return a structured list of:
- Nodes (components, services, modules)
- Edges (dependencies, data flow, API calls)
- Node attributes (type: service|database|module|external)

If this file has no architectural diagram relevance, respond with "NO_DIAGRAM_CONTENT".

IMPORTANT: Respond only in English."#,
        file_path, code
    )
}

/// Generate a prompt for extracting data flow diagram information.
pub fn diagram_data_flow_prompt(file_path: &str, code: &str) -> String {
    format!(
        r#"Analyze this source file for DATA FLOW DIAGRAM information.

File: {}

```
{}
```

Extract information for creating a data flow diagram (Graphviz DOT format).

Look for:
1. **Data Sources**: Where does data originate? (user input, databases, external APIs)
2. **Data Transformations**: How is data processed or transformed?
3. **Data Destinations**: Where does data end up? (storage, responses, exports)
4. **Data Types**: What are the key data structures being passed around?

Return a structured list of:
- Nodes (data stores, processes, external entities)
- Edges with data type labels (what data flows between nodes)

If this file has no data flow relevance, respond with "NO_DIAGRAM_CONTENT".

IMPORTANT: Respond only in English."#,
        file_path, code
    )
}

/// Generate a prompt for extracting database schema diagram information.
pub fn diagram_database_schema_prompt(file_path: &str, code: &str) -> String {
    format!(
        r#"Analyze this source file for DATABASE SCHEMA information.

File: {}

```
{}
```

Extract information for creating an entity-relationship diagram (Graphviz DOT format).

Look for:
1. **Entities/Tables**: Database tables, models, or schema definitions
2. **Fields/Columns**: Key fields with their types
3. **Relationships**: Foreign keys, joins, or associations between entities
4. **Constraints**: Primary keys, unique constraints, indexes

Return a structured list of:
- Entities (tables/models with their fields)
- Relationships (one-to-one, one-to-many, many-to-many)

If this file has no database schema relevance, respond with "NO_DIAGRAM_CONTENT".

IMPORTANT: Respond only in English."#,
        file_path, code
    )
}

/// Find context files (manifests, READMEs, config files) in a directory.
///
/// This is a generic version that looks for common context files across languages.
pub fn find_context_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if !dir.is_dir() {
        return Ok(files);
    }

    let root_dir = dir.to_path_buf();
    let skip_dirs: &[&str] = &["target", "node_modules", ".git", "dist", "build"];

    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .max_depth(3) // Don't go too deep for context files
        .into_iter()
        .filter_entry(|e| {
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

        // Include common context files across languages
        let is_context_file = file_name.to_lowercase().starts_with("readme")
            || file_name == "Cargo.toml"
            || file_name == "package.json"
            || file_name == "tsconfig.json"
            || file_name == "jsconfig.json"
            || extension == "md";

        if is_context_file {
            files.push(path.to_path_buf());
        }
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analysis_prompt_contains_file_path() {
        let prompt = analysis_prompt("src/lib.rs", "fn foo() {}");
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("fn foo() {}"));
    }

    #[test]
    fn test_documentation_prompt_detects_cargo_toml() {
        let prompt = documentation_prompt("Cargo.toml", "[package]");
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("Crate Type"));
    }

    #[test]
    fn test_documentation_prompt_detects_package_json() {
        let prompt = documentation_prompt("package.json", "{}");
        assert!(prompt.contains("package.json"));
        assert!(prompt.contains("npm scripts"));
    }

    #[test]
    fn test_documentation_prompt_detects_readme() {
        let prompt = documentation_prompt("README.md", "# Project");
        assert!(prompt.contains("README.md"));
        assert!(prompt.contains("Architecture Overview"));
    }

    #[test]
    fn test_architecture_prompt_is_generic() {
        let prompt = architecture_file_analysis_prompt("src/main.rs", "fn main() {}");
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("ARCHITECTURAL"));
        assert!(prompt.contains("Layer"));
    }

    #[test]
    fn test_diagram_prompts_are_generic() {
        let arch = diagram_architecture_prompt("test.rs", "code");
        assert!(arch.contains("ARCHITECTURAL"));

        let flow = diagram_data_flow_prompt("test.rs", "code");
        assert!(flow.contains("DATA FLOW"));

        let db = diagram_database_schema_prompt("test.rs", "code");
        assert!(db.contains("DATABASE"));
    }
}
