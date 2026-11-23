//! SQLite history. The file is `perf.db`. `noctum.db` is never opened.

mod models;

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::str::FromStr;

pub use models::{Candidate, DaemonState, PendingCommit, Repository, Run, TestBaseline, Trial};

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn open_read_only(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new().filename(path).read_only(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    pub async fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        Ok(Self { pool })
    }

    pub async fn run_migrations(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS repositories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS daemon_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                status TEXT NOT NULL DEFAULT 'idle',
                phase TEXT NOT NULL DEFAULT 'idle',
                detail TEXT,
                last_active TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO daemon_state (id, status, phase) VALUES (1, 'idle', 'idle')
             ON CONFLICT(id) DO NOTHING",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                repository_id INTEGER NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
                git_sha TEXT NOT NULL,
                package TEXT,
                started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                finished_at TEXT,
                status TEXT NOT NULL,
                error TEXT,
                window_key TEXT,
                perf_sha TEXT
            )",
        )
        .execute(&self.pool)
        .await?;
        let target_column: Option<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('runs') WHERE name = 'target'")
                .fetch_optional(&self.pool)
                .await?;
        if target_column.is_none() {
            sqlx::query("ALTER TABLE runs ADD COLUMN target TEXT NOT NULL DEFAULT 'x86_64-unknown-linux-musl'")
                .execute(&self.pool).await?;
        }
        let profile_column: Option<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('runs') WHERE name = 'measurement_profile'",
        )
        .fetch_optional(&self.pool)
        .await?;
        if profile_column.is_none() {
            sqlx::query("ALTER TABLE runs ADD COLUMN measurement_profile TEXT NOT NULL DEFAULT 'legacy-tb-v1'")
                .execute(&self.pool).await?;
        }
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS test_baselines (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                harness TEXT NOT NULL,
                test_name TEXT NOT NULL,
                status TEXT NOT NULL,
                insn_count INTEGER,
                stdout_sha256 TEXT,
                binary_sha256 TEXT,
                wall_ms INTEGER,
                reason TEXT
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS candidates (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                test_name TEXT NOT NULL,
                harness TEXT NOT NULL,
                rank INTEGER NOT NULL,
                symbol TEXT NOT NULL,
                file TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                exclusive_count INTEGER NOT NULL,
                share REAL NOT NULL,
                file_blob TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS trials (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                candidate_id INTEGER NOT NULL REFERENCES candidates(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                reason TEXT,
                model_summary TEXT,
                unified_diff TEXT,
                baseline_count INTEGER,
                insn_count INTEGER,
                improvement_ratio REAL,
                host_check_passed INTEGER NOT NULL DEFAULT 0,
                stdout_matched INTEGER NOT NULL DEFAULT 0,
                tool_output TEXT
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS accepted_commits (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                trial_id INTEGER NOT NULL UNIQUE REFERENCES trials(id) ON DELETE CASCADE,
                git_sha TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn add_repository(&self, path: &str, name: &str) -> Result<i64> {
        let row = sqlx::query("INSERT INTO repositories (path, name) VALUES (?, ?) RETURNING id")
            .bind(path)
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn get_repositories(&self) -> Result<Vec<Repository>> {
        Ok(
            sqlx::query_as("SELECT * FROM repositories ORDER BY name COLLATE NOCASE, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn enabled_repositories(&self) -> Result<Vec<Repository>> {
        Ok(sqlx::query_as(
            "SELECT * FROM repositories WHERE enabled = 1 ORDER BY name COLLATE NOCASE, id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_repository(&self, id: i64) -> Result<Option<Repository>> {
        Ok(sqlx::query_as("SELECT * FROM repositories WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn get_repository_by_path(&self, path: &str) -> Result<Option<Repository>> {
        Ok(sqlx::query_as("SELECT * FROM repositories WHERE path = ?")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn delete_repository(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM repositories WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        sqlx::query(
            "UPDATE repositories SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(enabled)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_daemon(&self, status: &str, phase: &str, detail: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE daemon_state SET status = ?, phase = ?, detail = ?, last_active = CURRENT_TIMESTAMP WHERE id = 1",
        )
        .bind(status)
        .bind(phase)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_daemon(&self) -> Result<DaemonState> {
        Ok(sqlx::query_as(
            "SELECT status, phase, detail, last_active FROM daemon_state WHERE id = 1",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn insert_run(
        &self,
        repository_id: i64,
        git_sha: &str,
        package: Option<&str>,
        status: &str,
        window_key: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO runs (repository_id, git_sha, package, status, window_key) VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(repository_id)
        .bind(git_sha)
        .bind(package)
        .bind(status)
        .bind(window_key)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn set_run_target(&self, id: i64, target: &str) -> Result<()> {
        sqlx::query("UPDATE runs SET target = ? WHERE id = ?")
            .bind(target)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_run_profile(&self, id: i64, profile: &str) -> Result<()> {
        sqlx::query("UPDATE runs SET measurement_profile = ? WHERE id = ?")
            .bind(profile)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn finish_run(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
        perf_sha: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE runs SET status = ?, error = ?, perf_sha = ?, finished_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(perf_sha)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn repo_finished_window(
        &self,
        repository_id: i64,
        window_key: &str,
        target: &str,
        profile: &str,
    ) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT id FROM runs WHERE repository_id = ? AND window_key = ? AND target = ? AND measurement_profile = ? AND status = 'ok' LIMIT 1",
        )
        .bind(repository_id)
        .bind(window_key)
        .bind(target)
        .bind(profile)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    pub async fn latest_run(&self, repository_id: i64) -> Result<Option<Run>> {
        Ok(
            sqlx::query_as("SELECT * FROM runs WHERE repository_id = ? ORDER BY id DESC LIMIT 1")
                .bind(repository_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn insert_baseline(
        &self,
        run_id: i64,
        harness: &str,
        test_name: &str,
        status: &str,
        insn_count: Option<i64>,
        stdout_sha256: Option<&str>,
        binary_sha256: Option<&str>,
        wall_ms: Option<i64>,
        reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO test_baselines
             (run_id, harness, test_name, status, insn_count, stdout_sha256, binary_sha256, wall_ms, reason)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(harness)
        .bind(test_name)
        .bind(status)
        .bind(insn_count)
        .bind(stdout_sha256)
        .bind(binary_sha256)
        .bind(wall_ms)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn baselines_for_run(&self, run_id: i64) -> Result<Vec<TestBaseline>> {
        Ok(
            sqlx::query_as("SELECT * FROM test_baselines WHERE run_id = ? ORDER BY id")
                .bind(run_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn insert_candidate(
        &self,
        run_id: i64,
        test_name: &str,
        harness: &str,
        rank: i64,
        symbol: &str,
        file: &str,
        line_start: i64,
        line_end: i64,
        exclusive_count: i64,
        share: f64,
        file_blob: &str,
    ) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO candidates
             (run_id, test_name, harness, rank, symbol, file, line_start, line_end, exclusive_count, share, file_blob)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(run_id)
        .bind(test_name)
        .bind(harness)
        .bind(rank)
        .bind(symbol)
        .bind(file)
        .bind(line_start)
        .bind(line_end)
        .bind(exclusive_count)
        .bind(share)
        .bind(file_blob)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn set_candidate_blob(&self, id: i64, file_blob: &str) -> Result<()> {
        sqlx::query("UPDATE candidates SET file_blob = ? WHERE id = ?")
            .bind(file_blob)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_trial(
        &self,
        candidate_id: i64,
        status: &str,
        reason: Option<&str>,
        summary: Option<&str>,
        diff: Option<&str>,
        baseline_count: Option<i64>,
        insn_count: Option<i64>,
        improvement_ratio: Option<f64>,
        host_check_passed: bool,
        stdout_matched: bool,
        tool_output: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO trials
             (candidate_id, status, reason, model_summary, unified_diff, baseline_count, insn_count,
              improvement_ratio, host_check_passed, stdout_matched, tool_output)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(candidate_id)
        .bind(status)
        .bind(reason)
        .bind(summary)
        .bind(diff)
        .bind(baseline_count)
        .bind(insn_count)
        .bind(improvement_ratio)
        .bind(host_check_passed)
        .bind(stdout_matched)
        .bind(tool_output.map(|text| crate::hashutil::tail_str(text, crate::LOG_LIMIT)))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    pub async fn insert_commit(&self, trial_id: i64, git_sha: &str) -> Result<()> {
        sqlx::query("INSERT INTO accepted_commits (trial_id, git_sha) VALUES (?, ?)")
            .bind(trial_id)
            .bind(git_sha)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn commit_for_trial(&self, trial_id: i64) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT git_sha FROM accepted_commits WHERE trial_id = ?")
                .bind(trial_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn trial_exists(
        &self,
        repository_id: i64,
        test_name: &str,
        symbol: &str,
        file_blob: &str,
        target: &str,
        profile: &str,
    ) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT t.id FROM trials t
             JOIN candidates c ON c.id = t.candidate_id
             JOIN runs r ON r.id = c.run_id
             WHERE r.repository_id = ? AND c.test_name = ? AND c.symbol = ? AND c.file_blob = ? AND r.target = ? AND r.measurement_profile = ? AND t.status != 'error'
             LIMIT 1",
        )
        .bind(repository_id)
        .bind(test_name)
        .bind(symbol)
        .bind(file_blob)
        .bind(target)
        .bind(profile)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    pub async fn pending_commits(&self, repository_id: i64) -> Result<Vec<PendingCommit>> {
        Ok(sqlx::query_as(
            "SELECT t.id AS trial_id, t.unified_diff AS unified_diff, t.model_summary AS model_summary,
                    c.test_name AS test_name, r.target AS target, t.baseline_count AS baseline_count,
                    t.insn_count AS insn_count, t.improvement_ratio AS improvement_ratio
             FROM trials t
             JOIN candidates c ON c.id = t.candidate_id
             JOIN runs r ON r.id = c.run_id
             LEFT JOIN accepted_commits a ON a.trial_id = t.id
             WHERE r.repository_id = ? AND t.status = 'accepted' AND a.id IS NULL
               AND t.unified_diff IS NOT NULL",
        )
        .bind(repository_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn candidates_for_run(&self, run_id: i64) -> Result<Vec<Candidate>> {
        Ok(sqlx::query_as(
            "SELECT id, run_id, test_name, harness, rank, symbol, file, line_start, line_end,
                    exclusive_count, share, file_blob
             FROM candidates
             WHERE run_id = ?
             ORDER BY rank, id",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn trials_for_run(&self, run_id: i64) -> Result<Vec<Trial>> {
        Ok(sqlx::query_as(
            "SELECT r.target, t.id, t.candidate_id, t.status, t.reason, t.model_summary, t.unified_diff,
                    t.baseline_count, t.insn_count, t.improvement_ratio, t.host_check_passed,
                    t.stdout_matched, t.tool_output, c.test_name, c.harness, c.symbol, c.file
             FROM trials t
             JOIN candidates c ON c.id = t.candidate_id
             JOIN runs r ON r.id = c.run_id
             WHERE c.run_id = ?
             ORDER BY t.id",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn trial(&self, id: i64) -> Result<Option<Trial>> {
        Ok(sqlx::query_as(
            "SELECT r.target, t.id, t.candidate_id, t.status, t.reason, t.model_summary, t.unified_diff,
                    t.baseline_count, t.insn_count, t.improvement_ratio, t.host_check_passed,
                    t.stdout_matched, t.tool_output, c.test_name, c.harness, c.symbol, c.file
             FROM trials t
             JOIN candidates c ON c.id = t.candidate_id
             JOIN runs r ON r.id = c.run_id
             WHERE t.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn accepted_delta(&self, run_id: i64) -> Result<i64> {
        let row: (Option<i64>,) = sqlx::query_as(
            "SELECT SUM(t.baseline_count - t.insn_count) FROM trials t
             JOIN candidates c ON c.id = t.candidate_id
             JOIN accepted_commits a ON a.trial_id = t.id
             WHERE c.run_id = ? AND t.status = 'accepted'",
        )
        .bind(run_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::Database;
    use crate::target::GuestTarget;

    #[tokio::test]
    async fn target_keeps_run_history_separate() {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::new(&temp.path().join("perf.db")).await.unwrap();
        db.run_migrations().await.unwrap();
        let repo = db.add_repository("/fixture", "fixture").await.unwrap();
        let run = db
            .insert_run(repo, "head", None, "ok", Some("night"))
            .await
            .unwrap();
        // Old rows retain the original x86 target.
        sqlx::query("ALTER TABLE runs DROP COLUMN target")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("ALTER TABLE runs DROP COLUMN measurement_profile")
            .execute(&db.pool)
            .await
            .unwrap();
        db.run_migrations().await.unwrap();
        let x86 = "x86_64-unknown-linux-musl";
        let arm = GuestTarget::Aarch64.triple();
        assert_eq!(db.latest_run(repo).await.unwrap().unwrap().target, x86);
        assert_eq!(
            db.latest_run(repo)
                .await
                .unwrap()
                .unwrap()
                .measurement_profile,
            "legacy-tb-v1"
        );
        assert!(db
            .repo_finished_window(repo, "night", x86, "legacy-tb-v1")
            .await
            .unwrap());
        assert!(!db
            .repo_finished_window(repo, "night", arm, "legacy-tb-v1")
            .await
            .unwrap());
        let candidate = db
            .insert_candidate(
                run,
                "test",
                "lib",
                1,
                "hot",
                "src/lib.rs",
                1,
                2,
                200_000,
                1.0,
                "blob",
            )
            .await
            .unwrap();
        let trial = db
            .insert_trial(
                candidate,
                "accepted",
                None,
                Some("Reduce work"),
                Some("diff"),
                Some(200_000),
                Some(100_000),
                Some(0.5),
                true,
                true,
                None,
            )
            .await
            .unwrap();
        assert!(db
            .trial_exists(repo, "test", "hot", "blob", x86, "legacy-tb-v1")
            .await
            .unwrap());
        assert!(!db
            .trial_exists(repo, "test", "hot", "blob", arm, "legacy-tb-v1")
            .await
            .unwrap());
        assert_eq!(db.trial(trial).await.unwrap().unwrap().target, x86);
        assert_eq!(db.pending_commits(repo).await.unwrap()[0].target, x86);
        sqlx::query("UPDATE trials SET status = 'error' WHERE id = ?")
            .bind(trial)
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(!db
            .trial_exists(repo, "test", "hot", "blob", x86, "legacy-tb-v1")
            .await
            .unwrap());
        sqlx::query("UPDATE trials SET status = 'accepted' WHERE id = ?")
            .bind(trial)
            .execute(&db.pool)
            .await
            .unwrap();
        db.set_run_profile(run, "ram-insn-v2").await.unwrap();
        assert!(!db
            .repo_finished_window(repo, "night", x86, "legacy-tb-v1")
            .await
            .unwrap());
        assert!(db
            .repo_finished_window(repo, "night", x86, "ram-insn-v2")
            .await
            .unwrap());
        assert!(!db
            .trial_exists(repo, "test", "hot", "blob", x86, "legacy-tb-v1")
            .await
            .unwrap());
        assert!(db
            .trial_exists(repo, "test", "hot", "blob", x86, "ram-insn-v2")
            .await
            .unwrap());
        db.set_run_target(run, arm).await.unwrap();
        assert_eq!(db.trials_for_run(run).await.unwrap()[0].target, arm);
    }
}
