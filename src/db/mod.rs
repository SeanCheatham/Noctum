//! SQLite database layer for persistent storage.
//!
//! Manages repositories, analysis results, mutation testing results, and daemon state.
//! Handles migrations and provides async CRUD operations via sqlx.

mod models;

pub use models::*;

use anyhow::{Context, Result};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;

/// Database wrapper for SQLite operations
#[derive(Clone)]
pub struct Database {
    pool: Pool<Sqlite>,
}

impl Database {
    /// Create a new database connection
    pub async fn new(path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create database directory: {:?}", parent))?;
        }

        let database_url = format!("sqlite:{}?mode=rwc", path.display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .with_context(|| format!("Failed to connect to database: {}", database_url))?;

        Ok(Self { pool })
    }

    /// Run database migrations
    pub async fn run_migrations(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS repositories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create repositories table")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS analysis_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repository_id INTEGER NOT NULL,
                file_path TEXT NOT NULL,
                analysis_type TEXT NOT NULL,
                result TEXT NOT NULL,
                severity TEXT,
                content_hash TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (repository_id) REFERENCES repositories(id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create analysis_results table")?;

        // Add content_hash column if it doesn't exist (migration for existing databases)
        let _ = sqlx::query("ALTER TABLE analysis_results ADD COLUMN content_hash TEXT")
            .execute(&self.pool)
            .await;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS daemon_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                status TEXT NOT NULL DEFAULT 'idle',
                current_task TEXT,
                last_active TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create daemon_state table")?;

        // Initialize daemon state if not exists
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO daemon_state (id, status) VALUES (1, 'idle')
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to initialize daemon state")?;

        // Create mutation_results table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS mutation_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repository_id INTEGER NOT NULL,
                file_path TEXT NOT NULL,
                description TEXT NOT NULL,
                reasoning TEXT NOT NULL,
                replacements_json TEXT NOT NULL,
                test_outcome TEXT NOT NULL,
                killing_test TEXT,
                test_output TEXT,
                execution_time_ms INTEGER,
                content_hash TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (repository_id) REFERENCES repositories(id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create mutation_results table")?;

        // Create indexes for mutation_results
        let _ = sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_mutation_results_repo_file \
             ON mutation_results(repository_id, file_path)",
        )
        .execute(&self.pool)
        .await;

        let _ = sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_mutation_results_outcome \
             ON mutation_results(test_outcome)",
        )
        .execute(&self.pool)
        .await;

        // Create diagrams table for DOT diagram storage
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS diagrams (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repository_id INTEGER NOT NULL,
                diagram_type TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT NOT NULL,
                dot_content TEXT NOT NULL,
                svg_content TEXT NOT NULL,
                content_hash TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (repository_id) REFERENCES repositories(id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create diagrams table")?;

        // Create indexes for diagrams
        let _ = sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_diagrams_repo_type \
             ON diagrams(repository_id, diagram_type)",
        )
        .execute(&self.pool)
        .await;

        Ok(())
    }

    /// Get all repositories
    pub async fn get_repositories(&self) -> Result<Vec<Repository>> {
        let repos = sqlx::query_as::<_, Repository>("SELECT * FROM repositories ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .context("Failed to fetch repositories")?;

        Ok(repos)
    }

    /// Get a repository by ID
    pub async fn get_repository(&self, id: i64) -> Result<Option<Repository>> {
        let repo = sqlx::query_as::<_, Repository>("SELECT * FROM repositories WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to fetch repository")?;

        Ok(repo)
    }

    /// Add a new repository
    pub async fn add_repository(&self, path: &str, name: &str) -> Result<i64> {
        let result =
            sqlx::query("INSERT INTO repositories (path, name) VALUES (?, ?) RETURNING id")
                .bind(path)
                .bind(name)
                .fetch_one(&self.pool)
                .await
                .context("Failed to add repository")?;

        Ok(sqlx::Row::get(&result, "id"))
    }

    /// Delete a repository and all its associated data
    pub async fn delete_repository(&self, id: i64) -> Result<bool> {
        // Delete associated diagrams first
        sqlx::query("DELETE FROM diagrams WHERE repository_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to delete diagrams")?;

        // Delete associated mutation results
        sqlx::query("DELETE FROM mutation_results WHERE repository_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to delete mutation results")?;

        // Delete associated analysis results
        sqlx::query("DELETE FROM analysis_results WHERE repository_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to delete analysis results")?;

        // Delete the repository itself
        let result = sqlx::query("DELETE FROM repositories WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("Failed to delete repository")?;

        Ok(result.rows_affected() > 0)
    }

    /// Get recent analysis results (latest per file)
    pub async fn get_recent_results(&self, limit: i32) -> Result<Vec<AnalysisResult>> {
        // Get the latest result for each file/analysis_type combination
        let results = sqlx::query_as::<_, AnalysisResult>(
            r#"
            SELECT ar.* FROM analysis_results ar
            INNER JOIN (
                SELECT file_path, analysis_type, MAX(created_at) as max_created
                FROM analysis_results
                GROUP BY file_path, analysis_type
            ) latest ON ar.file_path = latest.file_path
                AND ar.analysis_type = latest.analysis_type
                AND ar.created_at = latest.max_created
            ORDER BY ar.created_at DESC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch analysis results")?;

        Ok(results)
    }

    /// Get daemon status
    pub async fn get_daemon_status(&self) -> Result<DaemonState> {
        let state = sqlx::query_as::<_, DaemonState>("SELECT * FROM daemon_state WHERE id = 1")
            .fetch_one(&self.pool)
            .await
            .context("Failed to fetch daemon state")?;

        Ok(state)
    }

    /// Update daemon status
    pub async fn update_daemon_status(
        &self,
        status: &str,
        current_task: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE daemon_state SET status = ?, current_task = ?, last_active = CURRENT_TIMESTAMP WHERE id = 1",
        )
        .bind(status)
        .bind(current_task)
        .execute(&self.pool)
        .await
        .context("Failed to update daemon state")?;

        Ok(())
    }

    /// Save an analysis result
    pub async fn save_analysis_result(
        &self,
        repository_id: i64,
        file_path: &str,
        analysis_type: &str,
        result: &str,
        severity: Option<&str>,
        content_hash: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO analysis_results (repository_id, file_path, analysis_type, result, severity, content_hash) \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(repository_id)
        .bind(file_path)
        .bind(analysis_type)
        .bind(result)
        .bind(severity)
        .bind(content_hash)
        .fetch_one(&self.pool)
        .await
        .context("Failed to save analysis result")?;

        Ok(sqlx::Row::get(&row, "id"))
    }

    /// Get the latest content hash for a file
    pub async fn get_latest_file_hash(
        &self,
        repository_id: i64,
        file_path: &str,
        analysis_type: &str,
    ) -> Result<Option<String>> {
        let result = sqlx::query_scalar::<_, Option<String>>(
            "SELECT content_hash FROM analysis_results \
             WHERE repository_id = ? AND file_path = ? AND analysis_type = ? \
             ORDER BY id DESC LIMIT 1",
        )
        .bind(repository_id)
        .bind(file_path)
        .bind(analysis_type)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to fetch file hash")?;

        Ok(result.flatten())
    }

    /// Get analysis results for a specific repository and analysis type (latest per file)
    pub async fn get_repository_results(
        &self,
        repository_id: i64,
        analysis_type: &str,
    ) -> Result<Vec<AnalysisResult>> {
        // Get only the latest result for each file
        let results = sqlx::query_as::<_, AnalysisResult>(
            r#"
            SELECT ar.* FROM analysis_results ar
            INNER JOIN (
                SELECT file_path, MAX(created_at) as max_created
                FROM analysis_results
                WHERE repository_id = ? AND analysis_type = ?
                GROUP BY file_path
            ) latest ON ar.file_path = latest.file_path
                AND ar.created_at = latest.max_created
            WHERE ar.repository_id = ? AND ar.analysis_type = ?
            ORDER BY ar.file_path
            "#,
        )
        .bind(repository_id)
        .bind(analysis_type)
        .bind(repository_id)
        .bind(analysis_type)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch repository results")?;

        Ok(results)
    }

    /// Get all analysis results for a repository (latest per file/type)
    pub async fn get_all_repository_results(
        &self,
        repository_id: i64,
    ) -> Result<Vec<AnalysisResult>> {
        let results = sqlx::query_as::<_, AnalysisResult>(
            r#"
            SELECT ar.* FROM analysis_results ar
            INNER JOIN (
                SELECT file_path, analysis_type, MAX(created_at) as max_created
                FROM analysis_results
                WHERE repository_id = ?
                GROUP BY file_path, analysis_type
            ) latest ON ar.file_path = latest.file_path
                AND ar.analysis_type = latest.analysis_type
                AND ar.created_at = latest.max_created
            WHERE ar.repository_id = ?
            ORDER BY ar.analysis_type DESC, ar.file_path
            "#,
        )
        .bind(repository_id)
        .bind(repository_id)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch all repository results")?;

        Ok(results)
    }

    /// Save a mutation test result
    #[allow(clippy::too_many_arguments)]
    pub async fn save_mutation_result(
        &self,
        repository_id: i64,
        file_path: &str,
        description: &str,
        reasoning: &str,
        replacements_json: &str,
        test_outcome: &str,
        killing_test: Option<&str>,
        test_output: Option<&str>,
        execution_time_ms: Option<i32>,
        content_hash: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            r#"
            INSERT INTO mutation_results (
                repository_id, file_path, description, reasoning, replacements_json,
                test_outcome, killing_test, test_output, execution_time_ms, content_hash
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING id
            "#,
        )
        .bind(repository_id)
        .bind(file_path)
        .bind(description)
        .bind(reasoning)
        .bind(replacements_json)
        .bind(test_outcome)
        .bind(killing_test)
        .bind(test_output)
        .bind(execution_time_ms)
        .bind(content_hash)
        .fetch_one(&self.pool)
        .await
        .context("Failed to save mutation result")?;

        Ok(sqlx::Row::get(&row, "id"))
    }

    /// Get mutation results for a repository
    pub async fn get_mutation_results(&self, repository_id: i64) -> Result<Vec<MutationResult>> {
        let results = sqlx::query_as::<_, MutationResult>(
            r#"
            SELECT * FROM mutation_results
            WHERE repository_id = ?
            ORDER BY created_at DESC, file_path
            "#,
        )
        .bind(repository_id)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch mutation results")?;

        Ok(results)
    }

    /// Get mutation summary statistics for a repository
    pub async fn get_mutation_summary(&self, repository_id: i64) -> Result<MutationSummary> {
        let results = self.get_mutation_results(repository_id).await?;

        let mut summary = MutationSummary::default();
        for result in results {
            summary.total += 1;
            match result.test_outcome.as_str() {
                "killed" => summary.killed += 1,
                "survived" => summary.survived += 1,
                "timeout" => summary.timeout += 1,
                "compile_error" => summary.compile_error += 1,
                _ => {}
            }
        }

        Ok(summary)
    }

    /// Check if a file has mutation results for a given content hash
    pub async fn has_mutation_results_for_hash(
        &self,
        repository_id: i64,
        file_path: &str,
        content_hash: &str,
    ) -> Result<bool> {
        let count = sqlx::query_scalar::<_, i32>(
            r#"
            SELECT COUNT(*) FROM mutation_results
            WHERE repository_id = ? AND file_path = ? AND content_hash = ?
            "#,
        )
        .bind(repository_id)
        .bind(file_path)
        .bind(content_hash)
        .fetch_one(&self.pool)
        .await
        .context("Failed to check mutation results for hash")?;

        Ok(count > 0)
    }

    /// Save a new diagram (inserts new row, keeping history)
    #[allow(clippy::too_many_arguments)]
    pub async fn save_diagram(
        &self,
        repository_id: i64,
        diagram_type: &str,
        title: &str,
        description: &str,
        dot_content: &str,
        svg_content: &str,
        content_hash: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            r#"
            INSERT INTO diagrams (repository_id, diagram_type, title, description, dot_content, svg_content, content_hash)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            RETURNING id
            "#,
        )
        .bind(repository_id)
        .bind(diagram_type)
        .bind(title)
        .bind(description)
        .bind(dot_content)
        .bind(svg_content)
        .bind(content_hash)
        .fetch_one(&self.pool)
        .await
        .context("Failed to save diagram")?;

        Ok(sqlx::Row::get(&row, "id"))
    }

    /// Get the latest diagram of each type for a repository
    pub async fn get_latest_diagrams(&self, repository_id: i64) -> Result<Vec<Diagram>> {
        let diagrams = sqlx::query_as::<_, Diagram>(
            r#"
            SELECT d.* FROM diagrams d
            INNER JOIN (
                SELECT diagram_type, MAX(id) as max_id
                FROM diagrams
                WHERE repository_id = ?
                GROUP BY diagram_type
            ) latest ON d.diagram_type = latest.diagram_type
                AND d.id = latest.max_id
            WHERE d.repository_id = ?
            ORDER BY d.diagram_type
            "#,
        )
        .bind(repository_id)
        .bind(repository_id)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch latest diagrams")?;

        Ok(diagrams)
    }

    /// Get the latest content hash for diagrams of a repository
    /// Used to determine if diagrams need regeneration
    pub async fn get_latest_diagram_hash(
        &self,
        repository_id: i64,
        diagram_type: &str,
    ) -> Result<Option<String>> {
        let result = sqlx::query_scalar::<_, Option<String>>(
            r#"
            SELECT content_hash FROM diagrams
            WHERE repository_id = ? AND diagram_type = ?
            ORDER BY id DESC
            LIMIT 1
            "#,
        )
        .bind(repository_id)
        .bind(diagram_type)
        .fetch_optional(&self.pool)
        .await
        .context("Failed to fetch diagram hash")?;

        Ok(result.flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{NamedTempFile, TempDir};

    async fn create_test_db() -> (Database, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let db = Database::new(&db_path).await.unwrap();
        db.run_migrations().await.unwrap();
        (db, temp_dir)
    }

    #[tokio::test]
    async fn test_database_creation() {
        let temp_file = NamedTempFile::new().unwrap();
        let db = Database::new(temp_file.path()).await;
        assert!(db.is_ok(), "Database creation should succeed");
    }

    #[tokio::test]
    async fn test_run_migrations() {
        let (db, _temp_dir) = create_test_db().await;
        assert!(db.run_migrations().await.is_ok());
    }

    #[tokio::test]
    async fn test_add_repository() {
        let (db, _temp_dir) = create_test_db().await;

        let id = db.add_repository("/test/path", "Test Repo").await.unwrap();
        assert!(id > 0, "Repository ID should be positive");

        let repo = db.get_repository(id).await.unwrap().unwrap();
        assert_eq!(repo.path, "/test/path");
        assert_eq!(repo.name, "Test Repo");
        assert!(repo.enabled);
    }

    #[tokio::test]
    async fn test_get_repositories() {
        let (db, _temp_dir) = create_test_db().await;

        db.add_repository("/path1", "Repo 1").await.unwrap();
        db.add_repository("/path2", "Repo 2").await.unwrap();

        let repos = db.get_repositories().await.unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "Repo 1");
        assert_eq!(repos[1].name, "Repo 2");
    }

    #[tokio::test]
    async fn test_get_repository_not_found() {
        let (db, _temp_dir) = create_test_db().await;

        let repo = db.get_repository(999).await.unwrap();
        assert!(repo.is_none(), "Non-existent repository should return None");
    }

    #[tokio::test]
    async fn test_save_analysis_result() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let id = db
            .save_analysis_result(
                repo_id,
                "src/main.rs",
                "code_understanding",
                "Test analysis result",
                Some("info"),
                Some("hash123"),
            )
            .await
            .unwrap();

        assert!(id > 0);

        let results = db
            .get_repository_results(repo_id, "code_understanding")
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "src/main.rs");
        assert_eq!(results[0].analysis_type, "code_understanding");
        assert_eq!(results[0].result, "Test analysis result");
        assert_eq!(results[0].severity, Some("info".to_string()));
        assert_eq!(results[0].content_hash, Some("hash123".to_string()));
    }

    #[tokio::test]
    async fn test_get_recent_results() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        db.save_analysis_result(repo_id, "file1.rs", "type1", "result1", None, None)
            .await
            .unwrap();
        db.save_analysis_result(repo_id, "file2.rs", "type2", "result2", None, None)
            .await
            .unwrap();

        let results = db.get_recent_results(10).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_get_repository_results() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        db.save_analysis_result(repo_id, "file1.rs", "type1", "result1", None, None)
            .await
            .unwrap();
        db.save_analysis_result(repo_id, "file2.rs", "type1", "result2", None, None)
            .await
            .unwrap();
        db.save_analysis_result(repo_id, "file1.rs", "type2", "result3", None, None)
            .await
            .unwrap();

        let results = db.get_repository_results(repo_id, "type1").await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.analysis_type == "type1"));
    }

    #[tokio::test]
    async fn test_get_all_repository_results() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        db.save_analysis_result(repo_id, "file1.rs", "type1", "result1", None, None)
            .await
            .unwrap();
        db.save_analysis_result(repo_id, "file2.rs", "type2", "result2", None, None)
            .await
            .unwrap();

        let results = db.get_all_repository_results(repo_id).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_get_latest_file_hash() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        db.save_analysis_result(repo_id, "test.rs", "type1", "result", None, Some("hash1"))
            .await
            .unwrap();
        db.save_analysis_result(repo_id, "test.rs", "type1", "result2", None, Some("hash2"))
            .await
            .unwrap();

        let hash = db
            .get_latest_file_hash(repo_id, "test.rs", "type1")
            .await
            .unwrap();
        assert_eq!(hash, Some("hash2".to_string()));
    }

    #[tokio::test]
    async fn test_get_latest_file_hash_no_results() {
        let (db, _temp_dir) = create_test_db().await;
        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let hash = db
            .get_latest_file_hash(repo_id, "nonexistent.rs", "type1")
            .await
            .unwrap();
        assert!(hash.is_none());
    }

    #[tokio::test]
    async fn test_daemon_state() {
        let (db, _temp_dir) = create_test_db().await;

        db.update_daemon_status("processing", Some("analyzing files"))
            .await
            .unwrap();

        let state = db.get_daemon_status().await.unwrap();
        assert_eq!(state.status, "processing");
        assert_eq!(state.current_task, Some("analyzing files".to_string()));

        db.update_daemon_status("idle", None).await.unwrap();
        let state = db.get_daemon_status().await.unwrap();
        assert_eq!(state.status, "idle");
        assert!(state.current_task.is_none());
    }

    #[tokio::test]
    async fn test_save_mutation_result() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let replacements_json = serde_json::json!({
            "line_number": 10,
            "find": "x > 0",
            "replace": "x >= 0"
        })
        .to_string();

        let id = db
            .save_mutation_result(
                repo_id,
                "src/main.rs",
                "Changed > to >=",
                "Test reasoning",
                &replacements_json,
                "killed",
                Some("test_foo"),
                Some("Test output"),
                Some(100),
                Some("hash123"),
            )
            .await
            .unwrap();

        assert!(id > 0);

        let results = db.get_mutation_results(repo_id).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "src/main.rs");
        assert_eq!(results[0].description, "Changed > to >=");
        assert_eq!(results[0].test_outcome, "killed");
        assert_eq!(results[0].killing_test, Some("test_foo".to_string()));
        assert_eq!(results[0].execution_time_ms, Some(100));
    }

    #[tokio::test]
    async fn test_get_mutation_summary() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let replacements_json = "{}".to_string();

        db.save_mutation_result(
            repo_id,
            "f1.rs",
            "desc1",
            "reason",
            &replacements_json,
            "killed",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        db.save_mutation_result(
            repo_id,
            "f2.rs",
            "desc2",
            "reason",
            &replacements_json,
            "killed",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        db.save_mutation_result(
            repo_id,
            "f3.rs",
            "desc3",
            "reason",
            &replacements_json,
            "survived",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        db.save_mutation_result(
            repo_id,
            "f4.rs",
            "desc4",
            "reason",
            &replacements_json,
            "timeout",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        db.save_mutation_result(
            repo_id,
            "f5.rs",
            "desc5",
            "reason",
            &replacements_json,
            "compile_error",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let summary = db.get_mutation_summary(repo_id).await.unwrap();
        assert_eq!(summary.total, 5);
        assert_eq!(summary.killed, 2);
        assert_eq!(summary.survived, 1);
        assert_eq!(summary.timeout, 1);
        assert_eq!(summary.compile_error, 1);
    }

    #[tokio::test]
    async fn test_has_mutation_results_for_hash() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let replacements_json = "{}".to_string();
        db.save_mutation_result(
            repo_id,
            "test.rs",
            "desc",
            "reason",
            &replacements_json,
            "killed",
            None,
            None,
            None,
            Some("hash123"),
        )
        .await
        .unwrap();

        assert!(db
            .has_mutation_results_for_hash(repo_id, "test.rs", "hash123")
            .await
            .unwrap());
        assert!(!db
            .has_mutation_results_for_hash(repo_id, "test.rs", "different")
            .await
            .unwrap());
        assert!(!db
            .has_mutation_results_for_hash(repo_id, "other.rs", "hash123")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_duplicate_repository_path() {
        let (db, _temp_dir) = create_test_db().await;

        db.add_repository("/path", "Repo1").await.unwrap();

        let result = db.add_repository("/path", "Repo2").await;
        assert!(result.is_err(), "Duplicate path should fail");
    }

    #[tokio::test]
    async fn test_delete_repository() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test/path", "Test Repo").await.unwrap();

        // Add some analysis results
        db.save_analysis_result(repo_id, "file.rs", "type1", "result", None, None)
            .await
            .unwrap();

        // Add some mutation results
        db.save_mutation_result(
            repo_id, "file.rs", "desc", "reason", "{}", "killed", None, None, None, None,
        )
        .await
        .unwrap();

        // Add some diagrams
        db.save_diagram(
            repo_id,
            "system_architecture",
            "Title",
            "Desc",
            "digraph { a -> b }",
            "<svg></svg>",
            None,
        )
        .await
        .unwrap();

        // Delete the repository
        let deleted = db.delete_repository(repo_id).await.unwrap();
        assert!(deleted, "Repository should be deleted");

        // Verify repository is gone
        let repo = db.get_repository(repo_id).await.unwrap();
        assert!(repo.is_none(), "Repository should not exist after deletion");

        // Verify analysis results are gone
        let results = db.get_repository_results(repo_id, "type1").await.unwrap();
        assert!(results.is_empty(), "Analysis results should be deleted");

        // Verify mutation results are gone
        let mutations = db.get_mutation_results(repo_id).await.unwrap();
        assert!(mutations.is_empty(), "Mutation results should be deleted");

        // Verify diagrams are gone
        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert!(diagrams.is_empty(), "Diagrams should be deleted");
    }

    #[tokio::test]
    async fn test_delete_nonexistent_repository() {
        let (db, _temp_dir) = create_test_db().await;

        let deleted = db.delete_repository(999).await.unwrap();
        assert!(
            !deleted,
            "Deleting non-existent repository should return false"
        );
    }

    // =========================================================================
    // Diagram tests
    // =========================================================================

    #[tokio::test]
    async fn test_save_diagram() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let id = db
            .save_diagram(
                repo_id,
                "system_architecture",
                "System Architecture",
                "High-level view of components",
                "digraph { web -> db }",
                "<svg>web-db</svg>",
                Some("hash123"),
            )
            .await
            .unwrap();

        assert!(id > 0);

        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert_eq!(diagrams.len(), 1);
        assert_eq!(diagrams[0].diagram_type, "system_architecture");
        assert_eq!(diagrams[0].title, "System Architecture");
        assert_eq!(diagrams[0].dot_content, "digraph { web -> db }");
        assert_eq!(diagrams[0].svg_content, "<svg>web-db</svg>");
        assert_eq!(diagrams[0].content_hash, Some("hash123".to_string()));
    }

    #[tokio::test]
    async fn test_get_latest_diagrams_multiple_types() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        // Save diagrams of different types
        db.save_diagram(
            repo_id,
            "system_architecture",
            "Architecture",
            "Desc",
            "digraph { a -> b }",
            "<svg>a-b</svg>",
            None,
        )
        .await
        .unwrap();

        db.save_diagram(
            repo_id,
            "data_flow",
            "Data Flow",
            "Desc",
            "digraph { x -> y }",
            "<svg>x-y</svg>",
            None,
        )
        .await
        .unwrap();

        db.save_diagram(
            repo_id,
            "database_schema",
            "DB Schema",
            "Desc",
            "digraph { users -> posts }",
            "<svg>users-posts</svg>",
            None,
        )
        .await
        .unwrap();

        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert_eq!(diagrams.len(), 3);

        // Verify all types are present
        let types: Vec<_> = diagrams.iter().map(|d| d.diagram_type.as_str()).collect();
        assert!(types.contains(&"system_architecture"));
        assert!(types.contains(&"data_flow"));
        assert!(types.contains(&"database_schema"));
    }

    #[tokio::test]
    async fn test_get_latest_diagrams_returns_newest() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        // Save two versions of the same diagram type
        // The one with the higher ID should be returned (as a proxy for "newest")
        let old_id = db
            .save_diagram(
                repo_id,
                "system_architecture",
                "Old Version",
                "Desc",
                "digraph { old -> content }",
                "<svg>old</svg>",
                Some("hash1"),
            )
            .await
            .unwrap();

        let new_id = db
            .save_diagram(
                repo_id,
                "system_architecture",
                "New Version",
                "Desc",
                "digraph { new -> content }",
                "<svg>new</svg>",
                Some("hash2"),
            )
            .await
            .unwrap();

        // Verify new_id > old_id (insertion order)
        assert!(new_id > old_id);

        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert_eq!(diagrams.len(), 1);
        // The newest (by created_at or id) should be returned
        // Since they may have same timestamp, verify by checking the id
        assert_eq!(diagrams[0].id, new_id);
        assert_eq!(diagrams[0].title, "New Version");
        assert_eq!(diagrams[0].dot_content, "digraph { new -> content }");
        assert_eq!(diagrams[0].content_hash, Some("hash2".to_string()));
    }

    #[tokio::test]
    async fn test_get_latest_diagram_hash() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        // No diagrams yet
        let hash = db
            .get_latest_diagram_hash(repo_id, "system_architecture")
            .await
            .unwrap();
        assert!(hash.is_none());

        // Save a diagram
        db.save_diagram(
            repo_id,
            "system_architecture",
            "Title",
            "Desc",
            "digraph { a -> b }",
            "<svg></svg>",
            Some("hash123"),
        )
        .await
        .unwrap();

        let hash = db
            .get_latest_diagram_hash(repo_id, "system_architecture")
            .await
            .unwrap();
        assert_eq!(hash, Some("hash123".to_string()));

        // Different type should return None
        let hash = db
            .get_latest_diagram_hash(repo_id, "data_flow")
            .await
            .unwrap();
        assert!(hash.is_none());
    }

    #[tokio::test]
    async fn test_delete_repository_deletes_diagrams() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        // Add a diagram
        db.save_diagram(
            repo_id,
            "system_architecture",
            "Title",
            "Desc",
            "digraph { a -> b }",
            "<svg></svg>",
            None,
        )
        .await
        .unwrap();

        // Verify diagram exists
        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert_eq!(diagrams.len(), 1);

        // Delete repository
        db.delete_repository(repo_id).await.unwrap();

        // Verify diagrams are gone
        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert!(diagrams.is_empty());
    }

    #[tokio::test]
    async fn test_get_latest_diagrams_empty() {
        let (db, _temp_dir) = create_test_db().await;

        let repo_id = db.add_repository("/test", "Test").await.unwrap();

        let diagrams = db.get_latest_diagrams(repo_id).await.unwrap();
        assert!(diagrams.is_empty());
    }
}
