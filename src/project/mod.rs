//! Project discovery for multi-project/monorepo support.
//!
//! This module provides functionality to discover sub-projects within a repository,
//! supporting workspaces (Cargo workspaces, npm workspaces) and mixed-language repos.

use crate::language::Language;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// A discovered project within a repository.
#[derive(Debug, Clone)]
pub struct Project {
    /// Absolute path to the project root.
    pub root: PathBuf,
    /// Path relative to repository root (e.g., "packages/api").
    /// Empty string for root-level projects.
    pub relative_path: String,
    /// Detected language for this project.
    pub language: Language,
    /// Project name (from manifest or directory name).
    pub name: String,
    /// Type of project (standalone, workspace member, etc.).
    pub project_type: ProjectType,
}

/// The type of project structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    /// Standalone project (single Cargo.toml/package.json at root).
    Standalone,
    /// Member of a Cargo/npm workspace.
    WorkspaceMember,
    /// Root of a workspace (contains workspace members).
    WorkspaceRoot,
}

/// Marker file found during directory scan.
#[derive(Debug)]
struct MarkerFile {
    path: PathBuf,
    language: Language,
}

/// Discover all projects within a repository.
///
/// Walks the directory tree looking for language marker files (Cargo.toml, package.json),
/// parses workspace configurations, and returns a list of discovered projects.
///
/// If no projects are found, returns an empty Vec. The caller should decide how to
/// handle repos with no detected project structure.
pub fn discover_projects(repo_path: &Path) -> Result<Vec<Project>> {
    let mut projects = Vec::new();
    let repo_path = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());

    // Collect all marker files
    let markers = find_marker_files(&repo_path)?;

    if markers.is_empty() {
        tracing::debug!("No project markers found in {}", repo_path.display());
        return Ok(projects);
    }

    // Process Rust projects (Cargo.toml)
    let rust_markers: Vec<_> = markers
        .iter()
        .filter(|m| m.language == Language::Rust)
        .collect();

    // Collect all workspace member paths first to avoid mis-classification
    let mut workspace_member_paths: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();

    // First pass: identify workspace roots and their members
    for marker in &rust_markers {
        let cargo_toml_path = &marker.path;
        let project_root = cargo_toml_path.parent().unwrap_or(&repo_path);

        if let Some(members) = parse_cargo_workspace(cargo_toml_path) {
            // This is a workspace root
            let relative = relative_path(&repo_path, project_root);

            // Only add workspace root if it's also a package (has [package] section)
            if let Some(name) = parse_cargo_package_name(cargo_toml_path) {
                projects.push(Project {
                    root: project_root.to_path_buf(),
                    relative_path: relative,
                    language: Language::Rust,
                    name,
                    project_type: ProjectType::WorkspaceRoot,
                });
            }

            // Add workspace members and track their paths
            for member_glob in members {
                let member_paths = resolve_workspace_glob(project_root, &member_glob);
                for member_path in member_paths {
                    let member_cargo_toml = member_path.join("Cargo.toml");
                    if member_cargo_toml.exists() {
                        let member_name = parse_cargo_package_name(&member_cargo_toml)
                            .unwrap_or_else(|| directory_name(&member_path));
                        let member_relative = relative_path(&repo_path, &member_path);

                        // Track this path as a workspace member
                        workspace_member_paths.insert(member_path.clone());

                        projects.push(Project {
                            root: member_path,
                            relative_path: member_relative,
                            language: Language::Rust,
                            name: member_name,
                            project_type: ProjectType::WorkspaceMember,
                        });
                    }
                }
            }
        }
    }

    // Second pass: add standalone projects (ones not in any workspace)
    for marker in &rust_markers {
        let cargo_toml_path = &marker.path;
        let project_root = cargo_toml_path.parent().unwrap_or(&repo_path);

        // Skip if this is a workspace root (already processed) or a workspace member
        if parse_cargo_workspace(cargo_toml_path).is_some() {
            continue;
        }

        // Skip if this is a workspace member
        if workspace_member_paths.contains(&project_root.to_path_buf()) {
            continue;
        }

        // Skip if already added (shouldn't happen, but just in case)
        let relative = relative_path(&repo_path, project_root);
        if projects.iter().any(|p| p.relative_path == relative) {
            continue;
        }

        let name = parse_cargo_package_name(cargo_toml_path)
            .unwrap_or_else(|| directory_name(project_root));

        projects.push(Project {
            root: project_root.to_path_buf(),
            relative_path: relative,
            language: Language::Rust,
            name,
            project_type: ProjectType::Standalone,
        });
    }

    // Process TypeScript/JavaScript projects (package.json)
    let ts_markers: Vec<_> = markers
        .iter()
        .filter(|m| m.language == Language::TypeScript)
        .collect();

    // Collect all npm workspace member paths first
    let mut npm_workspace_member_paths: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();

    // First pass: identify npm workspace roots and their members
    for marker in &ts_markers {
        let package_json_path = &marker.path;
        let project_root = package_json_path.parent().unwrap_or(&repo_path);

        if let Some(members) = parse_npm_workspace(package_json_path) {
            // This is a workspace root
            let relative = relative_path(&repo_path, project_root);

            // Add workspace root
            let name = parse_npm_package_name(package_json_path)
                .unwrap_or_else(|| directory_name(project_root));

            projects.push(Project {
                root: project_root.to_path_buf(),
                relative_path: relative,
                language: Language::TypeScript,
                name,
                project_type: ProjectType::WorkspaceRoot,
            });

            // Add workspace members and track their paths
            for member_glob in members {
                let member_paths = resolve_workspace_glob(project_root, &member_glob);
                for member_path in member_paths {
                    let member_package_json = member_path.join("package.json");
                    if member_package_json.exists() {
                        let member_name = parse_npm_package_name(&member_package_json)
                            .unwrap_or_else(|| directory_name(&member_path));
                        let member_relative = relative_path(&repo_path, &member_path);

                        // Track this path as a workspace member
                        npm_workspace_member_paths.insert(member_path.clone());

                        projects.push(Project {
                            root: member_path,
                            relative_path: member_relative,
                            language: Language::TypeScript,
                            name: member_name,
                            project_type: ProjectType::WorkspaceMember,
                        });
                    }
                }
            }
        }
    }

    // Second pass: add standalone TypeScript projects
    for marker in &ts_markers {
        let package_json_path = &marker.path;
        let project_root = package_json_path.parent().unwrap_or(&repo_path);

        // Skip if this is a workspace root or member
        if parse_npm_workspace(package_json_path).is_some() {
            continue;
        }
        if npm_workspace_member_paths.contains(&project_root.to_path_buf()) {
            continue;
        }

        // Skip if already added (e.g., as Rust project at same path)
        let relative = relative_path(&repo_path, project_root);
        if projects.iter().any(|p| p.relative_path == relative) {
            continue;
        }

        let name = parse_npm_package_name(package_json_path)
            .unwrap_or_else(|| directory_name(project_root));

        projects.push(Project {
            root: project_root.to_path_buf(),
            relative_path: relative,
            language: Language::TypeScript,
            name,
            project_type: ProjectType::Standalone,
        });
    }

    // Deduplicate projects by relative_path
    projects.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    projects.dedup_by(|a, b| a.relative_path == b.relative_path);

    tracing::info!(
        "Discovered {} project(s) in {}",
        projects.len(),
        repo_path.display()
    );
    for project in &projects {
        tracing::debug!(
            "  - {} ({:?}) at {}",
            project.name,
            project.project_type,
            project.relative_path
        );
    }

    Ok(projects)
}

/// Find all marker files in a directory tree.
fn find_marker_files(repo_path: &Path) -> Result<Vec<MarkerFile>> {
    let mut markers = Vec::new();

    let root_dir = repo_path.to_path_buf();
    let skip_dirs = ["target", "node_modules", ".git", "dist", "build"];

    for entry in walkdir::WalkDir::new(repo_path)
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

        // Check for Rust marker
        if file_name == "Cargo.toml" {
            markers.push(MarkerFile {
                path: path.to_path_buf(),
                language: Language::Rust,
            });
        }

        // Check for TypeScript/JavaScript marker
        if file_name == "package.json" {
            markers.push(MarkerFile {
                path: path.to_path_buf(),
                language: Language::TypeScript,
            });
        }
    }

    Ok(markers)
}

/// Parse Cargo.toml for workspace members.
/// Returns None if not a workspace, Some(members) if it is.
fn parse_cargo_workspace(cargo_toml_path: &Path) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(cargo_toml_path).ok()?;
    let doc: toml::Value = content.parse().ok()?;

    let workspace = doc.get("workspace")?;
    let members = workspace.get("members")?;
    let members_array = members.as_array()?;

    Some(
        members_array
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}

/// Parse Cargo.toml for package name.
fn parse_cargo_package_name(cargo_toml_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(cargo_toml_path).ok()?;
    let doc: toml::Value = content.parse().ok()?;

    let package = doc.get("package")?;
    let name = package.get("name")?;
    name.as_str().map(String::from)
}

/// Parse package.json for npm/yarn/pnpm workspace members.
/// Returns None if not a workspace, Some(members) if it is.
fn parse_npm_workspace(package_json_path: &Path) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(package_json_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // npm/yarn workspaces are in the "workspaces" field
    let workspaces = json.get("workspaces")?;

    // Can be array of strings or object with "packages" key
    if let Some(arr) = workspaces.as_array() {
        Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        )
    } else if let Some(obj) = workspaces.as_object() {
        // Yarn workspaces can have { packages: [...] }
        let packages = obj.get("packages")?.as_array()?;
        Some(
            packages
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        )
    } else {
        None
    }
}

/// Parse package.json for package name.
fn parse_npm_package_name(package_json_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(package_json_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    json.get("name")?.as_str().map(String::from)
}

/// Resolve a workspace member glob pattern to actual paths.
fn resolve_workspace_glob(workspace_root: &Path, pattern: &str) -> Vec<PathBuf> {
    let full_pattern = workspace_root.join(pattern);
    let pattern_str = full_pattern.to_string_lossy();

    match glob::glob(&pattern_str) {
        Ok(paths) => paths.filter_map(Result::ok).collect(),
        Err(_) => {
            // Fall back to treating it as a literal path
            let literal_path = workspace_root.join(pattern);
            if literal_path.exists() {
                vec![literal_path]
            } else {
                vec![]
            }
        }
    }
}

/// Get the relative path from repo root to a directory.
fn relative_path(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Get the directory name as a string.
fn directory_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_cargo_toml(dir: &Path, name: &str, is_workspace: bool, members: &[&str]) {
        let mut content = String::new();

        if !name.is_empty() {
            content.push_str(&format!(
                r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"
"#,
                name
            ));
        }

        if is_workspace {
            content.push_str("\n[workspace]\nmembers = [\n");
            for member in members {
                content.push_str(&format!("    \"{}\",\n", member));
            }
            content.push_str("]\n");
        }

        std::fs::write(dir.join("Cargo.toml"), content).unwrap();
    }

    #[test]
    fn test_discover_standalone_rust_project() {
        let temp = TempDir::new().unwrap();
        create_cargo_toml(temp.path(), "myproject", false, &[]);
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();

        let projects = discover_projects(temp.path()).unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "myproject");
        assert_eq!(projects[0].project_type, ProjectType::Standalone);
        assert_eq!(projects[0].language, Language::Rust);
        assert!(projects[0].relative_path.is_empty());
    }

    #[test]
    fn test_discover_cargo_workspace() {
        let temp = TempDir::new().unwrap();

        // Create workspace root
        create_cargo_toml(temp.path(), "", true, &["crates/*"]);

        // Create workspace members
        let member1 = temp.path().join("crates/member1");
        let member2 = temp.path().join("crates/member2");
        std::fs::create_dir_all(&member1).unwrap();
        std::fs::create_dir_all(&member2).unwrap();
        create_cargo_toml(&member1, "member1", false, &[]);
        create_cargo_toml(&member2, "member2", false, &[]);

        let projects = discover_projects(temp.path()).unwrap();

        assert_eq!(projects.len(), 2);
        assert!(projects.iter().any(|p| p.name == "member1"));
        assert!(projects.iter().any(|p| p.name == "member2"));
        assert!(projects
            .iter()
            .all(|p| p.project_type == ProjectType::WorkspaceMember));
    }

    #[test]
    fn test_discover_empty_repo() {
        let temp = TempDir::new().unwrap();

        let projects = discover_projects(temp.path()).unwrap();

        assert!(projects.is_empty());
    }

    #[test]
    fn test_discover_nested_project() {
        let temp = TempDir::new().unwrap();

        // Create a project in a subdirectory
        let subdir = temp.path().join("backend");
        std::fs::create_dir_all(&subdir).unwrap();
        create_cargo_toml(&subdir, "backend", false, &[]);

        let projects = discover_projects(temp.path()).unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "backend");
        assert_eq!(projects[0].relative_path, "backend");
    }

    #[test]
    fn test_parse_cargo_workspace_members() {
        let temp = TempDir::new().unwrap();
        let cargo_toml = temp.path().join("Cargo.toml");
        std::fs::write(
            &cargo_toml,
            r#"
[workspace]
members = ["crates/*", "lib"]
"#,
        )
        .unwrap();

        let members = parse_cargo_workspace(&cargo_toml).unwrap();

        assert_eq!(members.len(), 2);
        assert!(members.contains(&"crates/*".to_string()));
        assert!(members.contains(&"lib".to_string()));
    }

    #[test]
    fn test_parse_cargo_package_name() {
        let temp = TempDir::new().unwrap();
        let cargo_toml = temp.path().join("Cargo.toml");
        std::fs::write(
            &cargo_toml,
            r#"
[package]
name = "my-awesome-crate"
version = "1.0.0"
"#,
        )
        .unwrap();

        let name = parse_cargo_package_name(&cargo_toml).unwrap();

        assert_eq!(name, "my-awesome-crate");
    }
}
