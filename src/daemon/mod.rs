use crate::analyzer::{AnalysisType, OllamaClient};
use crate::config::{Config, OllamaEndpoint};
use crate::db::Database;
use crate::mutation::{
    analyze_and_generate_mutations, executor::execute_mutation_test, MutationConfig,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

/// Minimum file size (bytes) for code analysis. Smaller files are typically
/// just module declarations (`mod foo;`) with no meaningful content.
const ANALYSIS_MIN_FILE_SIZE: usize = 50;

/// Maximum file size (bytes) for code analysis. Larger files are likely
/// generated code or vendored dependencies.
const ANALYSIS_MAX_FILE_SIZE: usize = 100_000;

/// Minimum file size (bytes) for mutation testing. We need enough code
/// to have meaningful mutation targets.
const MUTATION_MIN_FILE_SIZE: usize = 100;

/// Maximum file size (bytes) for mutation testing. More conservative than
/// analysis since we need to compile and run tests.
const MUTATION_MAX_FILE_SIZE: usize = 50_000;

/// Compute a SHA256 hash of the content
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Copy a repository to a temporary directory for isolated mutation testing.
///
/// Returns the TempDir handle (which auto-cleans on drop) and the path to the
/// copied repository within it.
async fn copy_repo_to_temp(repo_path: &Path) -> anyhow::Result<tempfile::TempDir> {
    let repo_path = repo_path.to_path_buf();

    // Use spawn_blocking since fs_extra::dir::copy is synchronous
    let temp_dir = tokio::task::spawn_blocking(move || -> anyhow::Result<tempfile::TempDir> {
        let temp_dir = tempfile::TempDir::with_prefix("noctum-")?;

        let options = fs_extra::dir::CopyOptions {
            overwrite: false,
            skip_exist: false,
            buffer_size: 64 * 1024, // 64KB buffer
            copy_inside: true,      // Copy contents into dest, not as subdirectory
            content_only: true,     // Copy only contents, not the directory itself
            depth: 0,               // Unlimited depth
        };

        fs_extra::dir::copy(&repo_path, temp_dir.path(), &options)
            .map_err(|e| anyhow::anyhow!("Failed to copy repository: {}", e))?;

        Ok(temp_dir)
    })
    .await??;

    Ok(temp_dir)
}

/// Translate a path from the temp copy back to the original repository path.
///
/// Given a file path in the temp directory, returns the corresponding path
/// in the original repository for storage/display purposes.
fn translate_temp_to_original(
    temp_repo_path: &Path,
    original_repo_path: &Path,
    file_path: &Path,
) -> PathBuf {
    // Strip the temp prefix and join with original
    if let Ok(relative) = file_path.strip_prefix(temp_repo_path) {
        original_repo_path.join(relative)
    } else {
        // Fallback: return as-is if path doesn't have expected prefix
        file_path.to_path_buf()
    }
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

impl DaemonStatus {
    fn as_u8(self) -> u8 {
        match self {
            DaemonStatus::Waiting => 0,
            DaemonStatus::Processing => 1,
            DaemonStatus::Stopping => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => DaemonStatus::Waiting,
            1 => DaemonStatus::Processing,
            _ => DaemonStatus::Stopping,
        }
    }
}

/// A task to be processed by an endpoint worker
struct AnalysisTask {
    repository_id: i64,
    /// Original file path (for DB storage and display)
    file_path: PathBuf,
    content: String,
    content_hash: String,
}

/// Handle for controlling the daemon from outside (e.g., web handlers).
/// This is cheap to clone and doesn't require any locks.
#[derive(Clone)]
pub struct DaemonHandle {
    should_stop: Arc<AtomicBool>,
    trigger_scan: Arc<AtomicBool>,
    status: Arc<AtomicU8>,
}

impl DaemonHandle {
    /// Trigger an immediate scan (works anytime, ignores schedule)
    pub fn trigger_scan(&self) {
        self.trigger_scan.store(true, Ordering::SeqCst);
        tracing::info!("Scan triggered manually");
    }

    /// Signal the daemon to stop gracefully
    pub fn stop(&self) {
        tracing::info!("Shutdown requested, stopping daemon...");
        self.should_stop.store(true, Ordering::SeqCst);
    }

    /// Get current daemon status
    pub fn status(&self) -> DaemonStatus {
        DaemonStatus::from_u8(self.status.load(Ordering::SeqCst))
    }
}

/// The background daemon that manages analysis tasks
pub struct Daemon {
    config: Arc<RwLock<Config>>,
    status: Arc<AtomicU8>,
    should_stop: Arc<AtomicBool>,
    trigger_scan: Arc<AtomicBool>,
    db: Database,
}

impl Daemon {
    /// Create a new daemon instance with shared config
    pub fn new(config: Arc<RwLock<Config>>, db: Database) -> Self {
        Self {
            config,
            status: Arc::new(AtomicU8::new(DaemonStatus::Waiting.as_u8())),
            should_stop: Arc::new(AtomicBool::new(false)),
            trigger_scan: Arc::new(AtomicBool::new(false)),
            db,
        }
    }

    /// Get a handle for controlling the daemon from outside.
    /// The handle is cheap to clone and doesn't require locks.
    pub fn handle(&self) -> DaemonHandle {
        DaemonHandle {
            should_stop: Arc::clone(&self.should_stop),
            trigger_scan: Arc::clone(&self.trigger_scan),
            status: Arc::clone(&self.status),
        }
    }

    /// Check if we're in the scheduled window
    async fn is_in_schedule(&self) -> bool {
        self.config.read().await.schedule.is_in_window()
    }

    /// Get current daemon status
    pub fn status(&self) -> DaemonStatus {
        DaemonStatus::from_u8(self.status.load(Ordering::SeqCst))
    }

    /// Set daemon status
    fn set_status(&self, status: DaemonStatus) {
        self.status.store(status.as_u8(), Ordering::SeqCst);
    }

    /// Run the daemon loop
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let config = self.config.read().await;
        tracing::info!(
            "Daemon started (scheduled window: {:02}:00 - {:02}:00)",
            config.schedule.start_hour,
            config.schedule.end_hour
        );
        let check_interval = Duration::from_secs(config.schedule.check_interval_seconds);
        drop(config);

        let mut ticker = interval(check_interval);

        loop {
            // Wait for either the next tick or a stop signal
            tokio::select! {
                _ = ticker.tick() => {}
                _ = self.wait_for_stop() => {
                    break;
                }
            }

            // Check stop flag after waking up
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            // Check if a scan was triggered manually
            let scan_triggered = self.trigger_scan.swap(false, Ordering::SeqCst);
            if scan_triggered {
                tracing::info!("Running manually triggered scan");
                self.set_status(DaemonStatus::Processing);
                self.process_tasks().await?;
                self.set_status(DaemonStatus::Waiting);
                continue;
            }

            // Check if we're in the scheduled window
            let in_window = self.is_in_schedule().await;

            match (self.status(), in_window) {
                (DaemonStatus::Waiting, true) => {
                    tracing::info!("Entering scheduled window, starting processing");
                    self.set_status(DaemonStatus::Processing);
                    self.process_tasks().await?;
                }
                (DaemonStatus::Processing, true) => {
                    // Continue processing
                    self.process_tasks().await?;
                }
                (DaemonStatus::Processing, false) => {
                    tracing::info!("Exiting scheduled window, pausing");
                    self.set_status(DaemonStatus::Waiting);
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

        self.set_status(DaemonStatus::Stopping);
        tracing::info!("Daemon stopped");
        Ok(())
    }

    /// Wait until the stop flag is set (used for select!)
    async fn wait_for_stop(&self) {
        // Poll the stop flag periodically
        while !self.should_stop.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Sleep for a duration, but wake up early if shutdown is requested.
    /// Checks the stop flag every second.
    async fn interruptible_sleep(&self, seconds: u64) {
        for _ in 0..seconds {
            if self.should_stop.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Process background analysis tasks
    async fn process_tasks(&mut self) -> anyhow::Result<()> {
        tracing::debug!("Processing tasks");

        // Update daemon state in database
        self.db
            .update_daemon_status("processing", Some("scanning repositories"))
            .await?;

        // Get enabled endpoints from config (read fresh each cycle)
        let endpoints: Vec<_> = self
            .config
            .read()
            .await
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
        for repo in enabled_repos {
            // Check if we should stop before processing each repo
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            tracing::info!("Analyzing repository: {} ({})", repo.name, repo.path);
            self.db
                .update_daemon_status("processing", Some(&format!("analyzing {}", repo.name)))
                .await?;

            if let Err(e) = self.analyze_repository_parallel(&repo, &endpoints).await {
                tracing::warn!("Failed to analyze repository {}: {}", repo.name, e);
            }
        }

        self.db.update_daemon_status("idle", None).await?;

        // Wait before next cycle to avoid excessive resource usage
        // (especially since we copy the entire repo to temp each cycle)
        let delay_secs = 60 * 60; // 60 minutes

        tracing::debug!(
            "Sleeping for {} seconds before next processing cycle",
            delay_secs
        );
        self.interruptible_sleep(delay_secs).await;

        Ok(())
    }

    /// Analyze a repository using parallel workers (one per endpoint)
    /// Returns true if any files were analyzed (i.e., had changes)
    async fn analyze_repository_parallel(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<bool> {
        let original_repo_path = std::path::Path::new(&repo.path);

        if !original_repo_path.exists() {
            tracing::warn!("Repository path does not exist: {}", repo.path);
            return Ok(false);
        }

        // Copy repository to temp directory for isolated analysis
        // This ensures the original repo is never modified during mutation testing
        tracing::info!(
            "Copying repository {} to temp directory for analysis",
            repo.name
        );
        let temp_dir = match copy_repo_to_temp(original_repo_path).await {
            Ok(dir) => dir,
            Err(e) => {
                tracing::error!("Failed to copy repository to temp: {}", e);
                return Err(e);
            }
        };
        let temp_repo_path = temp_dir.path();
        tracing::info!(
            "Repository copied to temp directory: {}",
            temp_repo_path.display()
        );

        // Find all Rust files in the temp copy
        let rust_files = self.find_rust_files(temp_repo_path)?;

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

            if content.len() > ANALYSIS_MAX_FILE_SIZE {
                tracing::debug!("Skipping large file: {:?}", file_path);
                continue;
            }

            if content.len() < ANALYSIS_MIN_FILE_SIZE {
                tracing::debug!("Skipping small file: {:?}", file_path);
                continue;
            }

            // Compute content hash
            let content_hash = compute_hash(&content);

            // Translate temp path to original for DB storage
            let original_file_path =
                translate_temp_to_original(temp_repo_path, original_repo_path, &file_path);
            let original_file_path_str = original_file_path.to_string_lossy().to_string();

            // Check if file has changed since last analysis (using original path)
            let existing_hash = self
                .db
                .get_latest_file_hash(
                    repository_id,
                    &original_file_path_str,
                    &AnalysisType::CodeUnderstanding.to_string(),
                )
                .await
                .unwrap_or(None);

            if existing_hash.as_ref() == Some(&content_hash) {
                tracing::debug!("Skipping unchanged file: {:?}", original_file_path);
                continue;
            }

            let task = AnalysisTask {
                repository_id,
                file_path: original_file_path, // Use original path for storage
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

        // Run mutation testing after architecture summary (using temp copy)
        if !self.should_stop.load(Ordering::SeqCst) {
            if let Err(e) = self
                .run_mutation_testing(repo, endpoints, temp_repo_path, original_repo_path)
                .await
            {
                tracing::warn!("Failed to run mutation testing for {}: {}", repo.name, e);
            }
        }

        // temp_dir is dropped here, cleaning up the temp copy
        tracing::debug!("Cleaning up temp directory for {}", repo.name);
        drop(temp_dir);

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

        // Try each endpoint until one succeeds
        for endpoint in endpoints {
            let client = OllamaClient::new(&endpoint.url, &endpoint.model);

            if !client.is_available().await {
                tracing::debug!(
                    "Endpoint {} not available for architecture summary, trying next",
                    endpoint.name
                );
                continue;
            }

            match client.generate(&prompt).await {
                Ok(summary) => {
                    tracing::info!(
                        "Generated architecture summary for {} using endpoint {}",
                        repo.name,
                        endpoint.name
                    );

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

                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        "Endpoint {} failed for architecture summary: {}, trying next",
                        endpoint.name,
                        e
                    );
                }
            }
        }

        tracing::warn!(
            "All endpoints failed for architecture summary of {}",
            repo.name
        );
        Ok(())
    }

    /// Run LLM-driven mutation testing on a repository using a temp copy.
    ///
    /// The temp copy is created by `analyze_repository_parallel()` before any analysis,
    /// ensuring the original repository is never modified.
    async fn run_mutation_testing(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
        temp_repo_path: &Path,
        original_repo_path: &Path,
    ) -> anyhow::Result<()> {
        tracing::info!("Starting mutation testing for {}", repo.name);

        self.db
            .update_daemon_status(
                "processing",
                Some(&format!("mutation testing {}", repo.name)),
            )
            .await?;

        let config = MutationConfig::default();

        // Find first available endpoint
        let (client, endpoint_name) = match find_available_endpoint(endpoints).await {
            Some((c, name)) => (c, name),
            None => {
                tracing::warn!("No endpoints available for mutation testing");
                return Ok(());
            }
        };

        // Find Rust files in the temp copy
        let rust_files = self.find_rust_files(temp_repo_path)?;
        let mut total_mutations = 0;
        let mut current_client = client;
        let mut current_endpoint_idx = endpoints
            .iter()
            .position(|e| e.name == endpoint_name)
            .unwrap_or(0);

        for file_path in rust_files {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            // Read file from temp copy
            let content = match tokio::fs::read_to_string(&file_path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            if content.len() < MUTATION_MIN_FILE_SIZE || content.len() > MUTATION_MAX_FILE_SIZE {
                continue;
            }

            let content_hash = compute_hash(&content);

            // Keep temp path for file operations (analyzer and executor)
            let temp_file_path_str = file_path.to_string_lossy().to_string();

            // Translate temp path back to original for DB lookups and storage
            let original_file_path =
                translate_temp_to_original(temp_repo_path, original_repo_path, &file_path);
            let original_file_path_str = original_file_path.to_string_lossy().to_string();

            // Check if already tested with this hash (using original path for DB lookup)
            if self
                .db
                .has_mutation_results_for_hash(repo.id, &original_file_path_str, &content_hash)
                .await
                .unwrap_or(false)
            {
                tracing::debug!(
                    "Skipping mutation testing for unchanged file: {}",
                    original_file_path_str
                );
                continue;
            }

            // Analyze and generate mutations, with endpoint fallback
            // Pass temp path so mutations store temp paths for executor to use
            tracing::debug!("Analyzing mutations for {}", original_file_path_str);
            let mutations = match analyze_and_generate_mutations(
                &current_client,
                &temp_file_path_str,
                &content,
                config.max_mutations_per_file,
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        "Failed to analyze mutations in {} with current endpoint: {}",
                        original_file_path_str,
                        e
                    );

                    // Try to find another endpoint
                    let remaining = &endpoints[current_endpoint_idx + 1..];
                    if let Some((new_client, new_name)) = find_available_endpoint(remaining).await {
                        tracing::info!("Switching to endpoint {} for mutation analysis", new_name);
                        current_client = new_client;
                        current_endpoint_idx = endpoints
                            .iter()
                            .position(|ep| ep.name == new_name)
                            .unwrap_or(current_endpoint_idx);

                        // Retry with new endpoint
                        match analyze_and_generate_mutations(
                            &current_client,
                            &temp_file_path_str,
                            &content,
                            config.max_mutations_per_file,
                        )
                        .await
                        {
                            Ok(m) => m,
                            Err(e2) => {
                                tracing::warn!(
                                    "Retry also failed for {}: {}",
                                    original_file_path_str,
                                    e2
                                );
                                continue;
                            }
                        }
                    } else {
                        continue;
                    }
                }
            };

            if mutations.is_empty() {
                tracing::debug!("No mutations generated for {}", original_file_path_str);
                continue;
            }

            tracing::info!(
                "Generated {} mutations for {}",
                mutations.len(),
                original_file_path_str
            );

            // Pre-compute original lines for building replacement details
            let original_lines: Vec<&str> = content.lines().collect();

            for mutation in mutations {
                if self.should_stop.load(Ordering::SeqCst) {
                    break;
                }

                // Execute the mutation test on the temp copy
                let result = match execute_mutation_test(
                    &current_client,
                    temp_repo_path,
                    mutation,
                    &content,
                    &config,
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Failed to execute mutation test: {}", e);
                        continue;
                    }
                };

                // Build replacements JSON with all replacement info
                // Each replacement has: line_number, find, replace
                // We also include the original lines for context
                let replacements_with_context: Vec<serde_json::Value> = result
                    .mutation
                    .replacements
                    .iter()
                    .map(|r| {
                        let original_line = original_lines
                            .get(r.line_number.saturating_sub(1))
                            .unwrap_or(&"")
                            .to_string();
                        serde_json::json!({
                            "line_number": r.line_number,
                            "find": r.find,
                            "replace": r.replace,
                            "original_line": original_line
                        })
                    })
                    .collect();

                let replacements_json = serde_json::to_string(&replacements_with_context)
                    .unwrap_or_else(|_| "[]".to_string());

                // Save result with original path (not temp path) for UI display
                if let Err(e) = self
                    .db
                    .save_mutation_result(
                        repo.id,
                        &original_file_path_str,
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

    /// Find all Rust files in a directory recursively.
    ///
    /// Skips hidden directories (`.git`, etc.), `target/`, and `node_modules/`.
    fn find_rust_files(&self, dir: &std::path::Path) -> anyhow::Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !dir.is_dir() {
            return Ok(files);
        }

        let root_dir = dir.to_path_buf();

        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Don't filter the root directory itself (may be a temp dir starting with .)
                if e.path() == root_dir {
                    return true;
                }
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

        // Try to get a task from the queue, with shutdown check
        let task = {
            let mut rx = receiver.lock().await;
            tokio::select! {
                task = rx.recv() => task,
                _ = wait_for_stop_signal(&should_stop) => {
                    tracing::debug!(
                        "Worker for '{}' stopping due to shutdown signal",
                        endpoint.name
                    );
                    break;
                }
            }
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

/// Find the first available endpoint from a list.
/// Returns the client and endpoint name if found.
async fn find_available_endpoint(endpoints: &[OllamaEndpoint]) -> Option<(OllamaClient, String)> {
    for endpoint in endpoints {
        let client = OllamaClient::new(&endpoint.url, &endpoint.model);
        if client.is_available().await {
            return Some((client, endpoint.name.clone()));
        }
        tracing::debug!("Endpoint {} not available, trying next", endpoint.name);
    }
    None
}

/// Map keywords in analysis results to severity levels.
///
/// - "critical", "vulnerability", "unsafe" → "warning"
/// - "error", "bug" → "error"
/// - Everything else → "info"
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

/// Helper function to wait for shutdown signal (for use in tokio::select!)
async fn wait_for_stop_signal(should_stop: &AtomicBool) {
    while !should_stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(100)).await;
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

    fn create_test_daemon() -> (Daemon, tempfile::TempDir) {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let db_path = temp_dir.path().join("test.db");
            let db = Database::new(&db_path).await.unwrap();
            db.run_migrations().await.unwrap();
            db
        });
        let config = Arc::new(RwLock::new(Config::default()));
        (Daemon::new(config, db), temp_dir)
    }

    #[test]
    fn test_daemon_new() {
        let (daemon, _temp_dir) = create_test_daemon();
        assert_eq!(daemon.status(), DaemonStatus::Waiting);
    }

    #[test]
    fn test_daemon_trigger_scan() {
        let (daemon, _temp_dir) = create_test_daemon();
        let handle = daemon.handle();

        assert!(!daemon
            .trigger_scan
            .load(std::sync::atomic::Ordering::SeqCst));

        handle.trigger_scan();
        assert!(daemon
            .trigger_scan
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_daemon_stop() {
        let (daemon, _temp_dir) = create_test_daemon();
        let handle = daemon.handle();

        assert!(!daemon.should_stop.load(std::sync::atomic::Ordering::SeqCst));

        handle.stop();
        assert!(daemon.should_stop.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_daemon_config_sync() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let db = Database::new(temp_file.path()).await.unwrap();
        db.run_migrations().await.unwrap();

        let config = Arc::new(RwLock::new(Config::default()));
        let daemon = Daemon::new(config.clone(), db);

        // Update config externally
        {
            let mut cfg = config.write().await;
            cfg.schedule.start_hour = 10;
            cfg.schedule.end_hour = 20;
        }

        // Daemon should see the updated config
        let cfg = daemon.config.read().await;
        assert_eq!(cfg.schedule.start_hour, 10);
        assert_eq!(cfg.schedule.end_hour, 20);
    }

    // =========================================================================
    // find_rust_files tests
    // =========================================================================

    #[test]
    fn test_find_rust_files_empty_dir() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let (daemon, _db_dir) = create_test_daemon();
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();
        assert_eq!(files.len(), 0);
    }

    #[test]
    fn test_find_rust_files_with_files() {
        let temp_dir = tempfile::TempDir::with_prefix("test_rust_files").unwrap();
        std::fs::write(temp_dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp_dir.path().join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(temp_dir.path().join("test.txt"), "not rust").unwrap();

        let (daemon, _db_dir) = create_test_daemon();
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.ends_with("main.rs")));
        assert!(files.iter().any(|f| f.ends_with("lib.rs")));
        assert!(!files.iter().any(|f| f.ends_with("test.txt")));
    }

    #[test]
    fn test_find_rust_files_recursive() {
        let temp_dir = tempfile::TempDir::with_prefix("test_rust_files").unwrap();
        let subdir = temp_dir.path().join("src");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("main.rs"), "fn main() {}").unwrap();

        let (daemon, _db_dir) = create_test_daemon();
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

        let (daemon, _db_dir) = create_test_daemon();
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert!(!files.iter().any(|f| f.to_string_lossy().contains("target")));
    }

    #[test]
    fn test_find_rust_files_excludes_hidden() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let hidden_dir = temp_dir.path().join(".hidden");
        std::fs::create_dir_all(&hidden_dir).unwrap();
        std::fs::write(hidden_dir.join("lib.rs"), "fn test() {}").unwrap();

        let (daemon, _db_dir) = create_test_daemon();
        let files = daemon.find_rust_files(temp_dir.path()).unwrap();

        assert!(!files
            .iter()
            .any(|f| f.to_string_lossy().contains(".hidden")));
    }
}
