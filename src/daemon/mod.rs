use crate::analyzer::{AnalysisType, OllamaClient};
use crate::config::{Config, OllamaEndpoint, ScheduleConfig};
use crate::db::Database;
use crate::mutation::{
    analyze_and_generate_mutations, executor::execute_mutation_test, MutationConfig,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::{interval, Duration};

/// Compute a SHA256 hash of the content
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Daemon status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    /// Daemon is waiting for scheduled window
    Waiting,
    /// Daemon is actively processing
    Processing,
    /// Daemon is stopping
    Stopping,
}

/// A task to be processed by an endpoint worker
struct AnalysisTask {
    repository_id: i64,
    file_path: PathBuf,
    content: String,
    content_hash: String,
}

/// The background daemon that manages analysis tasks
pub struct Daemon {
    config: Config,
    status: DaemonStatus,
    schedule: Arc<TokioMutex<ScheduleConfig>>,
    should_stop: Arc<AtomicBool>,
    trigger_scan: Arc<AtomicBool>,
    db: Database,
}

impl Daemon {
    /// Create a new daemon instance
    pub fn new(config: Config, db: Database) -> Self {
        let schedule = config.schedule.clone();
        Self {
            schedule: Arc::new(TokioMutex::new(schedule)),
            config,
            status: DaemonStatus::Waiting,
            should_stop: Arc::new(AtomicBool::new(false)),
            trigger_scan: Arc::new(AtomicBool::new(false)),
            db,
        }
    }

    /// Trigger an immediate scan (works anytime, ignores schedule)
    pub fn trigger_scan(&self) {
        self.trigger_scan.store(true, Ordering::SeqCst);
        tracing::info!("Scan triggered manually");
    }

    /// Update the schedule (called when config is reloaded)
    pub async fn set_schedule(&self, schedule: ScheduleConfig) {
        *self.schedule.lock().await = schedule;
    }

    /// Check if we're in the scheduled window
    async fn is_in_schedule(&self) -> bool {
        self.schedule.lock().await.is_in_window()
    }

    /// Get current daemon status
    pub fn status(&self) -> DaemonStatus {
        self.status
    }

    /// Signal the daemon to stop
    pub fn stop(&self) {
        self.should_stop.store(true, Ordering::SeqCst);
    }

    /// Run the daemon loop
    pub async fn run(&mut self) -> anyhow::Result<()> {
        tracing::info!(
            "Daemon started (scheduled window: {:02}:00 - {:02}:00)",
            self.config.schedule.start_hour,
            self.config.schedule.end_hour
        );

        let check_interval = Duration::from_secs(self.config.schedule.check_interval_seconds);
        let mut ticker = interval(check_interval);

        while !self.should_stop.load(Ordering::SeqCst) {
            ticker.tick().await;

            // Check if a scan was triggered manually
            let scan_triggered = self.trigger_scan.swap(false, Ordering::SeqCst);
            if scan_triggered {
                tracing::info!("Running manually triggered scan");
                self.status = DaemonStatus::Processing;
                self.process_tasks().await?;
                self.status = DaemonStatus::Waiting;
                continue;
            }

            // Check if we're in the scheduled window
            let in_window = self.is_in_schedule().await;

            match (self.status, in_window) {
                (DaemonStatus::Waiting, true) => {
                    tracing::info!("Entering scheduled window, starting processing");
                    self.status = DaemonStatus::Processing;
                    self.process_tasks().await?;
                }
                (DaemonStatus::Processing, true) => {
                    // Continue processing
                    self.process_tasks().await?;
                }
                (DaemonStatus::Processing, false) => {
                    tracing::info!("Exiting scheduled window, pausing");
                    self.status = DaemonStatus::Waiting;
                }
                (DaemonStatus::Waiting, false) => {
                    // Normal state, waiting for window
                    tracing::debug!("Outside scheduled window, waiting");
                }
                (DaemonStatus::Stopping, _) => {
                    break;
                }
            }
        }

        self.status = DaemonStatus::Stopping;
        tracing::info!("Daemon stopped");
        Ok(())
    }

    /// Process background analysis tasks
    async fn process_tasks(&mut self) -> anyhow::Result<()> {
        tracing::debug!("Processing tasks");

        // Update daemon state in database
        self.db
            .update_daemon_status("processing", Some("scanning repositories"))
            .await?;

        // Get enabled endpoints from config
        let endpoints: Vec<_> = self
            .config
            .endpoints
            .iter()
            .filter(|e| e.enabled)
            .cloned()
            .collect();

        if endpoints.is_empty() {
            tracing::debug!("No Ollama endpoints configured, waiting...");
            self.db
                .update_daemon_status("idle", Some("no endpoints configured"))
                .await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            return Ok(());
        }

        // Get all enabled repositories
        let repositories = match self.db.get_repositories().await {
            Ok(repos) => repos,
            Err(e) => {
                tracing::error!("Failed to fetch repositories: {}", e);
                return Ok(());
            }
        };

        let enabled_repos: Vec<_> = repositories.into_iter().filter(|r| r.enabled).collect();

        if enabled_repos.is_empty() {
            tracing::debug!("No enabled repositories to analyze");
            self.db.update_daemon_status("idle", None).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            return Ok(());
        }

        // Process each repository with parallel workers
        let mut any_files_analyzed = false;
        for repo in enabled_repos {
            // Check if we should stop before processing each repo
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            tracing::info!("Analyzing repository: {} ({})", repo.name, repo.path);
            self.db
                .update_daemon_status("processing", Some(&format!("analyzing {}", repo.name)))
                .await?;

            match self.analyze_repository_parallel(&repo, &endpoints).await {
                Ok(files_analyzed) => {
                    if files_analyzed {
                        any_files_analyzed = true;
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to analyze repository {}: {}", repo.name, e);
                }
            }
        }

        self.db.update_daemon_status("idle", None).await?;

        // If we generated architecture summaries, wait longer before next cycle
        let delay = if any_files_analyzed {
            Duration::from_secs(20 * 60) // 20 minutes
        } else {
            Duration::from_secs(5)
        };

        tracing::debug!("Sleeping for {:?} before next processing cycle", delay);
        tokio::time::sleep(delay).await;

        Ok(())
    }

    /// Analyze a repository using parallel workers (one per endpoint)
    /// Returns true if any files were analyzed (i.e., had changes)
    async fn analyze_repository_parallel(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<bool> {
        let repo_path = std::path::Path::new(&repo.path);

        if !repo_path.exists() {
            tracing::warn!("Repository path does not exist: {}", repo.path);
            return Ok(false);
        }

        // Find all Rust files in the repository
        let rust_files = self.find_rust_files(repo_path)?;

        if rust_files.is_empty() {
            tracing::debug!("No Rust files found in repository: {}", repo.name);
            return Ok(false);
        }

        tracing::info!(
            "Found {} Rust files in {}, distributing across {} endpoint(s)",
            rust_files.len(),
            repo.name,
            endpoints.len()
        );

        // Create work queue channel
        let (tx, rx) = mpsc::channel::<AnalysisTask>(100);
        let rx = Arc::new(TokioMutex::new(rx));

        // Spawn worker tasks for each endpoint
        let mut worker_handles = Vec::new();
        for endpoint in endpoints {
            let worker_rx = Arc::clone(&rx);
            let db = self.db.clone();
            let should_stop = Arc::clone(&self.should_stop);
            let endpoint = endpoint.clone();

            let handle =
                tokio::spawn(
                    async move { endpoint_worker(endpoint, worker_rx, db, should_stop).await },
                );
            worker_handles.push(handle);
        }

        // Send tasks to the work queue
        let repository_id = repo.id;
        let mut tasks_sent = 0;
        for file_path in rust_files {
            // Check if we should stop
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            // Read file content
            let content = match tokio::fs::read_to_string(&file_path).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read file {:?}: {}", file_path, e);
                    continue;
                }
            };

            // Skip very large files (> 100KB)
            if content.len() > 100_000 {
                tracing::debug!("Skipping large file: {:?}", file_path);
                continue;
            }

            // Skip very small files (< 50 bytes, likely just module declarations)
            if content.len() < 50 {
                tracing::debug!("Skipping small file: {:?}", file_path);
                continue;
            }

            // Compute content hash
            let content_hash = compute_hash(&content);
            let file_path_str = file_path.to_string_lossy().to_string();

            // Check if file has changed since last analysis
            let existing_hash = self
                .db
                .get_latest_file_hash(
                    repository_id,
                    &file_path_str,
                    &AnalysisType::CodeUnderstanding.to_string(),
                )
                .await
                .unwrap_or(None);

            if existing_hash.as_ref() == Some(&content_hash) {
                tracing::debug!("Skipping unchanged file: {:?}", file_path);
                continue;
            }

            let task = AnalysisTask {
                repository_id,
                file_path,
                content,
                content_hash,
            };

            if tx.send(task).await.is_err() {
                // All receivers dropped, workers are done
                break;
            }
            tasks_sent += 1;
        }

        // Drop sender to signal workers that no more tasks are coming
        drop(tx);

        // Wait for all workers to complete
        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::warn!("Worker task failed: {}", e);
            }
        }

        let files_analyzed = tasks_sent > 0;

        // Check if we should continue with architecture summary
        if self.should_stop.load(Ordering::SeqCst) {
            return Ok(files_analyzed);
        }

        // Only generate architecture summary if at least one file was analyzed
        if files_analyzed {
            if let Err(e) = self.generate_architecture_summary(repo, endpoints).await {
                tracing::warn!(
                    "Failed to generate architecture summary for {}: {}",
                    repo.name,
                    e
                );
            }
        } else {
            tracing::debug!(
                "Skipping architecture summary for {} - no files changed",
                repo.name
            );
        }

        // Run mutation testing after architecture summary
        if !self.should_stop.load(Ordering::SeqCst) {
            if let Err(e) = self.run_mutation_testing(repo, endpoints).await {
                tracing::warn!("Failed to run mutation testing for {}: {}", repo.name, e);
            }
        }

        Ok(files_analyzed)
    }

    /// Generate an architectural summary by aggregating all file analysis results
    async fn generate_architecture_summary(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<()> {
        tracing::info!("Generating architecture summary for {}", repo.name);

        self.db
            .update_daemon_status("processing", Some(&format!("summarizing {}", repo.name)))
            .await?;

        // Get all code understanding results for this repository
        let results = self
            .db
            .get_repository_results(repo.id, &AnalysisType::CodeUnderstanding.to_string())
            .await?;

        if results.is_empty() {
            tracing::debug!("No file analyses to summarize for {}", repo.name);
            return Ok(());
        }

        // Build a summary of all file analyses, filtering out deleted files
        let mut file_summaries = String::new();
        let mut included_count = 0;
        for result in &results {
            // Skip results for files that no longer exist
            let file_path = std::path::Path::new(&result.file_path);
            if !file_path.exists() {
                tracing::debug!("Skipping deleted file in summary: {}", result.file_path);
                continue;
            }
            file_summaries.push_str(&format!("\n## {}\n{}\n", result.file_path, result.result));
            included_count += 1;
        }

        if included_count == 0 {
            tracing::debug!("No existing files to summarize for {}", repo.name);
            return Ok(());
        }

        // Truncate if too long (keep under ~50k chars to avoid token limits)
        let truncated = if file_summaries.len() > 50000 {
            format!(
                "{}...\n\n(truncated, {} files total)",
                &file_summaries[..50000],
                included_count
            )
        } else {
            file_summaries
        };

        let prompt = format!(
            "You are analyzing a Rust codebase called '{}'. \
             Below are summaries of individual files in the project.\n\n\
             Based on these file summaries, provide a high-level architectural overview including:\n\
             1. **Purpose**: What is this project/application about?\n\
             2. **Architecture**: What architectural patterns are used (e.g., layered, microservices, MVC)?\n\
             3. **Key Components**: What are the main modules/components and their responsibilities?\n\
             4. **Data Flow**: How does data flow through the system?\n\
             5. **Dependencies**: What external dependencies or integrations exist?\n\
             6. **Suggestions**: Any architectural improvements or concerns?\n\n\
             IMPORTANT: Respond only in English (or code)\n\n
             File Summaries:\n{}\n",
            repo.name, truncated
        );

        // Use the first available endpoint for the summary
        let endpoint = match endpoints.first() {
            Some(ep) => ep,
            None => {
                tracing::warn!("No endpoints available for architecture summary");
                return Ok(());
            }
        };

        let client = OllamaClient::new(&endpoint.url, &endpoint.model);

        if !client.is_available().await {
            tracing::warn!(
                "Endpoint {} not available for architecture summary",
                endpoint.name
            );
            return Ok(());
        }

        match client.generate(&prompt).await {
            Ok(summary) => {
                tracing::info!("Generated architecture summary for {}", repo.name);

                // Save the summary - use repo path as the "file_path" to identify it
                self.db
                    .save_analysis_result(
                        repo.id,
                        &format!("[{}] Architecture Summary", repo.name),
                        &AnalysisType::ArchitectureSummary.to_string(),
                        &summary,
                        Some("info"),
                        None, // No content hash for architecture summaries
                    )
                    .await?;
            }
            Err(e) => {
                tracing::warn!("Failed to generate architecture summary: {}", e);
            }
        }

        Ok(())
    }

    /// Run LLM-driven mutation testing on a repository
    async fn run_mutation_testing(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<()> {
        tracing::info!("Starting mutation testing for {}", repo.name);

        self.db
            .update_daemon_status(
                "processing",
                Some(&format!("mutation testing {}", repo.name)),
            )
            .await?;

        let repo_path = std::path::Path::new(&repo.path);
        let config = MutationConfig::default();

        // Get first available endpoint
        let endpoint = match endpoints.first() {
            Some(ep) => ep,
            None => {
                tracing::warn!("No endpoints available for mutation testing");
                return Ok(());
            }
        };

        let client = OllamaClient::new(&endpoint.url, &endpoint.model);

        if !client.is_available().await {
            tracing::warn!(
                "Endpoint {} not available for mutation testing",
                endpoint.name
            );
            return Ok(());
        }

        // Find Rust files
        let rust_files = self.find_rust_files(repo_path)?;
        let mut total_mutations = 0;

        for file_path in rust_files {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            // Read file
            let content = match tokio::fs::read_to_string(&file_path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Skip small/large files
            if content.len() < 100 || content.len() > 50_000 {
                continue;
            }

            let content_hash = compute_hash(&content);
            let file_path_str = file_path.to_string_lossy().to_string();

            // Check if already tested with this hash
            if self
                .db
                .has_mutation_results_for_hash(repo.id, &file_path_str, &content_hash)
                .await
                .unwrap_or(false)
            {
                tracing::debug!(
                    "Skipping mutation testing for unchanged file: {}",
                    file_path_str
                );
                continue;
            }

            // Analyze and generate mutations in a single LLM call
            tracing::debug!("Analyzing mutations for {}", file_path_str);
            let mutations = match analyze_and_generate_mutations(
                &client,
                &file_path_str,
                &content,
                config.max_mutations_per_file,
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("Failed to analyze mutations in {}: {}", file_path_str, e);
                    continue;
                }
            };

            if mutations.is_empty() {
                tracing::debug!("No mutations generated for {}", file_path_str);
                continue;
            }

            tracing::info!(
                "Generated {} mutations for {}",
                mutations.len(),
                file_path_str
            );

            // Pre-compute original lines for building replacement details
            let original_lines: Vec<&str> = content.lines().collect();

            for mutation in mutations {
                if self.should_stop.load(Ordering::SeqCst) {
                    break;
                }

                // Execute the mutation test
                let result = match execute_mutation_test(repo_path, mutation, &config).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to execute mutation test: {}", e);
                        continue;
                    }
                };

                // Build replacements JSON with find/replace info
                // Format: { "line_number": N, "find": "...", "replace": "...", "original_line": "..." }
                let original_line = original_lines
                    .get(result.mutation.line_number.saturating_sub(1))
                    .unwrap_or(&"")
                    .to_string();

                let replacements_json = serde_json::json!({
                    "line_number": result.mutation.line_number,
                    "find": result.mutation.find,
                    "replace": result.mutation.replace,
                    "original_line": original_line
                })
                .to_string();

                if let Err(e) = self
                    .db
                    .save_mutation_result(
                        repo.id,
                        &file_path_str,
                        &result.mutation.description,
                        &result.mutation.reasoning,
                        &replacements_json,
                        &result.outcome.to_string(),
                        result.killing_test.as_deref(),
                        result.test_output.as_deref(),
                        Some(result.execution_time_ms as i32),
                        Some(&content_hash),
                    )
                    .await
                {
                    tracing::warn!("Failed to save mutation result: {}", e);
                }

                total_mutations += 1;
            }
        }

        tracing::info!(
            "Completed mutation testing for {} ({} mutations)",
            repo.name,
            total_mutations
        );
        Ok(())
    }

    /// Find all Rust files in a directory recursively
    fn find_rust_files(&self, dir: &std::path::Path) -> anyhow::Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Skip hidden directories and common non-source directories
                let name = e.file_name().to_string_lossy();
                !name.starts_with('.') && name != "target" && name != "node_modules"
            })
        {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path.to_path_buf());
            }
        }

        Ok(files)
    }
}

/// Worker function that pulls tasks from the queue and processes them
async fn endpoint_worker(
    endpoint: OllamaEndpoint,
    receiver: Arc<TokioMutex<mpsc::Receiver<AnalysisTask>>>,
    db: Database,
    should_stop: Arc<AtomicBool>,
) {
    let client = OllamaClient::new(&endpoint.url, &endpoint.model);

    // Check if endpoint is available
    if !client.is_available().await {
        tracing::warn!(
            "Ollama endpoint '{}' at {} is not available, skipping",
            endpoint.name,
            endpoint.url
        );
        return;
    }

    tracing::info!(
        "Worker started for endpoint '{}' ({})",
        endpoint.name,
        endpoint.url
    );

    loop {
        // Check if we should stop
        if should_stop.load(Ordering::SeqCst) {
            tracing::debug!(
                "Worker for '{}' stopping due to shutdown signal",
                endpoint.name
            );
            break;
        }

        // Try to get a task from the queue
        let task = {
            let mut rx = receiver.lock().await;
            rx.recv().await
        };

        let task = match task {
            Some(t) => t,
            None => {
                // Channel closed, no more tasks
                tracing::debug!("Worker for '{}' finished - no more tasks", endpoint.name);
                break;
            }
        };

        // Process the task
        let file_path_str = task.file_path.to_string_lossy().to_string();
        tracing::debug!("Worker '{}' analyzing: {}", endpoint.name, file_path_str);

        // Build the analysis prompt
        let prompt = format!(
            "Analyze the following Rust code and provide a brief summary of what it does:\n\n\
             File: {}\n\n\
             ```rust\n{}\n```\n\n\
             Provide a concise analysis including:\n\
             1. Purpose of the code\n\
             2. Key functions/structs\n\
             3. Any potential issues or improvements\n
             4. Up to two specific code modification recommendations\n\n
             IMPORTANT: Respond only in English (or code)",
            file_path_str, task.content
        );

        // Run code understanding analysis
        match client.generate(&prompt).await {
            Ok(result) => {
                tracing::info!(
                    "Worker '{}' completed analysis of {}",
                    endpoint.name,
                    file_path_str
                );

                // Determine severity from result (simple heuristic)
                let severity = determine_severity(&result);

                // Save result to database
                if let Err(e) = db
                    .save_analysis_result(
                        task.repository_id,
                        &file_path_str,
                        &AnalysisType::CodeUnderstanding.to_string(),
                        &result,
                        severity.as_deref(),
                        Some(&task.content_hash),
                    )
                    .await
                {
                    tracing::warn!("Failed to save analysis result: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Worker '{}' failed to analyze {}: {}",
                    endpoint.name,
                    file_path_str,
                    e
                );
            }
        }
    }

    tracing::info!("Worker for endpoint '{}' stopped", endpoint.name);
}

/// Determine severity based on analysis result content
fn determine_severity(result: &str) -> Option<String> {
    let lower = result.to_lowercase();

    if lower.contains("critical") || lower.contains("vulnerability") || lower.contains("unsafe") {
        Some("warning".to_string())
    } else if lower.contains("error") || lower.contains("bug") {
        Some("error".to_string())
    } else {
        // Default to info for improvements, suggestions, or any other content
        Some("info".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash_deterministic() {
        let content = "hello world";
        let hash1 = compute_hash(content);
        let hash2 = compute_hash(content);
        assert_eq!(hash1, hash2, "Same content should produce same hash");
    }

    #[test]
    fn test_compute_hash_different_content() {
        let hash1 = compute_hash("hello");
        let hash2 = compute_hash("world");
        assert_ne!(
            hash1, hash2,
            "Different content should produce different hash"
        );
    }

    #[test]
    fn test_compute_hash_empty_string() {
        let hash = compute_hash("");
        // SHA256 of empty string is a known value
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_compute_hash_known_value() {
        // SHA256 of "hello" is a known value
        let hash = compute_hash("hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_compute_hash_unicode() {
        let hash = compute_hash("こんにちは");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 produces 64 hex chars
    }

    #[test]
    fn test_determine_severity_critical() {
        assert_eq!(
            determine_severity("This has a critical issue"),
            Some("warning".to_string())
        );
    }

    #[test]
    fn test_determine_severity_vulnerability() {
        assert_eq!(
            determine_severity("Found a security vulnerability"),
            Some("warning".to_string())
        );
    }

    #[test]
    fn test_determine_severity_unsafe() {
        assert_eq!(
            determine_severity("Uses unsafe code block"),
            Some("warning".to_string())
        );
    }

    #[test]
    fn test_determine_severity_error() {
        assert_eq!(
            determine_severity("There is an error in the logic"),
            Some("error".to_string())
        );
    }

    #[test]
    fn test_determine_severity_bug() {
        assert_eq!(
            determine_severity("This code has a bug"),
            Some("error".to_string())
        );
    }

    #[test]
    fn test_determine_severity_improvement() {
        assert_eq!(
            determine_severity("Consider an improvement here"),
            Some("info".to_string())
        );
    }

    #[test]
    fn test_determine_severity_default() {
        assert_eq!(
            determine_severity("This code looks fine"),
            Some("info".to_string())
        );
    }

    #[test]
    fn test_determine_severity_case_insensitive() {
        assert_eq!(
            determine_severity("CRITICAL issue found"),
            Some("warning".to_string())
        );
        assert_eq!(
            determine_severity("BUG detected"),
            Some("error".to_string())
        );
    }

    // =========================================================================
    // Daemon lifecycle tests
    // =========================================================================

    #[test]
    fn test_daemon_new() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        assert_eq!(daemon.status(), DaemonStatus::Waiting);
    }

    #[test]
    fn test_daemon_trigger_scan() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        assert!(!daemon
            .trigger_scan
            .load(std::sync::atomic::Ordering::SeqCst));

        daemon.trigger_scan();
        assert!(daemon
            .trigger_scan
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_daemon_stop() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        assert!(!daemon.should_stop.load(std::sync::atomic::Ordering::SeqCst));

        daemon.stop();
        assert!(daemon.should_stop.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_daemon_set_schedule() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let db = Database::new(temp_file.path()).await.unwrap();
        db.run_migrations().await.unwrap();

        let daemon = Daemon::new(Config::default(), db);

        let new_schedule = ScheduleConfig {
            start_hour: 10,
            end_hour: 20,
            check_interval_seconds: 30,
        };

        daemon.set_schedule(new_schedule.clone()).await;

        let schedule = daemon.schedule.lock().await;
        assert_eq!(schedule.start_hour, 10);
        assert_eq!(schedule.end_hour, 20);
        assert_eq!(schedule.check_interval_seconds, 30);
    }

    // =========================================================================
    // find_rust_files tests
    // =========================================================================

    #[test]
    fn test_find_rust_files_empty_dir() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let temp_dir2 = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir2.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 0);
    }

    #[test]
    fn test_find_rust_files_with_files() {
        // Use with_prefix to avoid directory names starting with '.' which would be filtered
        let temp_dir = tempfile::TempDir::with_prefix("test_rust_files").unwrap();
        let db_temp_dir = tempfile::TempDir::new().unwrap();

        // Create some Rust files
        std::fs::write(temp_dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp_dir.path().join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(temp_dir.path().join("test.txt"), "not rust").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = db_temp_dir.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.ends_with("main.rs")));
        assert!(files.iter().any(|f| f.ends_with("lib.rs")));
        assert!(!files.iter().any(|f| f.ends_with("test.txt")));
    }

    #[test]
    fn test_find_rust_files_recursive() {
        // Use with_prefix to avoid directory names starting with '.' which would be filtered
        let temp_dir = tempfile::TempDir::with_prefix("test_rust_files").unwrap();
        let db_temp_dir = tempfile::TempDir::new().unwrap();
        let subdir = temp_dir.path().join("src");

        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("main.rs"), "fn main() {}").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = db_temp_dir.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("src/main.rs"));
    }

    #[test]
    fn test_find_rust_files_excludes_target() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let target_dir = temp_dir.path().join("target");

        std::fs::create_dir_all(target_dir.join("debug")).unwrap();
        std::fs::write(target_dir.join("debug").join("lib.rs"), "fn test() {}").unwrap();

        let temp_dir2 = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir2.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert!(!files.iter().any(|f| f.to_string_lossy().contains("target")));
    }

    #[test]
    fn test_find_rust_files_excludes_hidden() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let hidden_dir = temp_dir.path().join(".hidden");

        std::fs::create_dir_all(&hidden_dir).unwrap();
        std::fs::write(hidden_dir.join("lib.rs"), "fn test() {}").unwrap();

        let temp_dir2 = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir2.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });

        let daemon = Daemon::new(Config::default(), db);
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert!(!files
            .iter()
            .any(|f| f.to_string_lossy().contains(".hidden")));
    }
}
