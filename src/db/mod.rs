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
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (repository_id) REFERENCES repositories(id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create analysis_results table")?;

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

    /// Add a new repository
    pub async fn add_repository(&self, path: &str, name: &str) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO repositories (path, name) VALUES (?, ?) RETURNING id",
        )
        .bind(path)
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .context("Failed to add repository")?;

        Ok(sqlx::Row::get(&result, "id"))
    }

    /// Get recent analysis results
    pub async fn get_recent_results(&self, limit: i32) -> Result<Vec<AnalysisResult>> {
        let results = sqlx::query_as::<_, AnalysisResult>(
            "SELECT * FROM analysis_results ORDER BY created_at DESC LIMIT ?",
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
    pub async fn update_daemon_status(&self, status: &str, current_task: Option<&str>) -> Result<()> {
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
}
