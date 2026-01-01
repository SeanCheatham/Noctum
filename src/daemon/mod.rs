use crate::analyzer::{AnalysisType, OllamaClient};
use crate::config::{Config, OllamaEndpoint};
use crate::db::Database;
use crate::diagram::{
    clean_dot_output, render_dot_to_svg, validate_dot_syntax, DiagramExtractor, DiagramGenerator,
    DiagramType,
};
use crate::language::Language;
use crate::mutation::{
    analyze_and_generate_mutations, executor::execute_mutation_test, MutationConfig,
};
use crate::project::discover_projects;
use crate::repo_config::RepoConfig;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};

/// Maximum number of retries for DOT diagram generation when syntax errors occur
const DOT_MAX_RETRIES: usize = 3;

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

/// Result of running a shell command.
#[derive(Debug)]
pub struct CommandResult {
    /// Whether the command succeeded (exit code 0).
    pub success: bool,
    /// Combined stdout and stderr output.
    #[allow(dead_code)]
    pub output: String,
    /// How long the command took to run in milliseconds.
    #[allow(dead_code)]
    pub duration_ms: u64,
}

/// Run a shell command with a timeout.
///
/// The command is executed via `sh -c` to support shell features like pipes.
/// Returns a `CommandResult` with success status, output, and duration.
async fn run_command_with_timeout(
    working_dir: &Path,
    command: &str,
    timeout_seconds: u64,
) -> CommandResult {
    use std::process::Stdio;
    use std::time::Instant;

    let start = Instant::now();

    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            return CommandResult {
                success: false,
                output: format!("Failed to spawn command: {}", e),
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    };

    let timeout = Duration::from_secs(timeout_seconds);
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}{}", stdout, stderr);

            CommandResult {
                success: output.status.success(),
                output: combined,
                duration_ms,
            }
        }
        Ok(Err(e)) => CommandResult {
            success: false,
            output: format!("Command execution error: {}", e),
            duration_ms,
        },
        Err(_) => CommandResult {
            success: false,
            output: format!("Command timed out after {} seconds", timeout_seconds),
            duration_ms,
        },
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

/// The type of analysis to perform for a task
#[derive(Debug, Clone, Copy)]
enum AnalysisTaskType {
    /// Granular code understanding (for File Analysis tab)
    CodeUnderstanding,
    /// Architecture-focused analysis (for Architecture summary aggregation)
    ArchitectureFileAnalysis,
    /// Diagram extraction for a specific diagram type
    DiagramExtraction(DiagramType),
    /// Documentation/context file analysis (READMEs, Cargo.toml, etc.)
    DocumentationAnalysis,
}

/// An analysis task to be processed by a worker
struct AnalysisTask {
    repository_id: i64,
    file_path: PathBuf,
    content: String,
    content_hash: String,
    task_type: AnalysisTaskType,
    /// The programming language of the file being analyzed.
    language: Language,
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

        // Load repository-level configuration
        let repo_config = RepoConfig::load(temp_repo_path).unwrap_or_default();

        // Log which features are enabled
        tracing::info!(
            "Repository {} config: code_analysis={}, architecture_analysis={}, diagram_creation={}, mutation_testing={}",
            repo.name,
            repo_config.enable_code_analysis,
            repo_config.enable_architecture_analysis,
            repo_config.enable_diagram_creation,
            repo_config.enable_mutation_testing
        );

        // Check if any analysis is enabled
        let any_analysis_enabled = repo_config.enable_code_analysis
            || repo_config.enable_architecture_analysis
            || repo_config.enable_diagram_creation
            || repo_config.enable_mutation_testing;

        if !any_analysis_enabled {
            tracing::info!(
                "No analysis features enabled for {}, skipping",
                repo.name
            );
            return Ok(false);
        }

        // Discover projects in the repository
        let projects = discover_projects(temp_repo_path)?;

        if projects.is_empty() {
            tracing::debug!("No projects found in repository: {}", repo.name);
            return Ok(false);
        }

        tracing::info!(
            "Discovered {} project(s) in {}: {:?}",
            projects.len(),
            repo.name,
            projects.iter().map(|p| &p.name).collect::<Vec<_>>()
        );

        // Collect source files from all projects with their language
        // file_data now includes: (original_path, content, hash, language)
        let mut file_data: Vec<(PathBuf, String, String, Language)> = Vec::new();
        let mut context_file_data: Vec<(PathBuf, String, String, Language)> = Vec::new();

        for project in &projects {
            // Find source files for this project
            let source_files = project.language.find_source_files(&project.root)?;

            for file_path in source_files {
                let content = match tokio::fs::read_to_string(&file_path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to read file {:?}: {}", file_path, e);
                        continue;
                    }
                };

                // Use language-specific size limits
                let min_size = project.language.min_file_size();
                let max_size = project.language.max_file_size();
                if content.len() > max_size || content.len() < min_size {
                    tracing::debug!("Skipping file due to size: {:?}", file_path);
                    continue;
                }

                let original_file_path =
                    translate_temp_to_original(temp_repo_path, original_repo_path, &file_path);
                let content_hash = compute_hash(&content);

                file_data.push((original_file_path, content, content_hash, project.language));
            }

            // Find context files for this project
            let ctx_files = project.language.find_context_files(&project.root)?;

            for file_path in ctx_files {
                let content = match tokio::fs::read_to_string(&file_path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to read context file {:?}: {}", file_path, e);
                        continue;
                    }
                };

                // Context files have different size limits
                if content.len() > project.language.max_file_size() {
                    tracing::debug!("Skipping context file due to size: {:?}", file_path);
                    continue;
                }

                let original_file_path =
                    translate_temp_to_original(temp_repo_path, original_repo_path, &file_path);
                let content_hash = compute_hash(&content);

                context_file_data.push((
                    original_file_path,
                    content,
                    content_hash,
                    project.language,
                ));
            }
        }

        if file_data.is_empty() {
            tracing::debug!(
                "No suitable source files found in repository: {}",
                repo.name
            );
            return Ok(false);
        }

        tracing::info!(
            "Found {} source files and {} context files in {}, distributing across {} endpoint(s)",
            file_data.len(),
            context_file_data.len(),
            repo.name,
            endpoints.len()
        );

        // Compute combined hash for diagram change detection
        let combined_hash = {
            let mut hasher = Sha256::new();
            for (_, _, hash, _) in &file_data {
                hasher.update(hash.as_bytes());
            }
            format!("{:x}", hasher.finalize())
        };

        // =========================================================================
        // PHASE 1: PARALLEL ANALYSIS
        // Run enabled analysis types concurrently based on repo config.
        // =========================================================================

        let mut code_changed = false;
        let mut arch_changed = false;
        let mut diagrams_changed = false;
        let mut docs_changed = false;

        // Only run analyses that are enabled
        let run_code = repo_config.enable_code_analysis;
        let run_arch = repo_config.enable_architecture_analysis;
        let run_diagrams = repo_config.enable_diagram_creation;

        if run_code || run_arch || run_diagrams {
            tracing::info!("Starting parallel analysis phase for {}", repo.name);

            // Run enabled analysis types in parallel
            // We use Option futures to conditionally include each analysis
            let code_future = async {
                if run_code {
                    self.run_code_understanding_analysis(repo, &file_data, endpoints)
                        .await
                } else {
                    Ok(false)
                }
            };

            let arch_future = async {
                if run_arch {
                    self.run_architecture_file_analysis(repo, &file_data, endpoints)
                        .await
                } else {
                    Ok(false)
                }
            };

            let diagram_future = async {
                if run_diagrams {
                    self.run_diagram_extractions(repo, &file_data, endpoints)
                        .await
                } else {
                    Ok(false)
                }
            };

            // Documentation analysis is needed for architecture summary
            let doc_future = async {
                if run_arch {
                    self.run_documentation_analysis(repo, &context_file_data, endpoints)
                        .await
                } else {
                    Ok(false)
                }
            };

            let (code_result, arch_result, diagram_result, doc_result) =
                tokio::join!(code_future, arch_future, diagram_future, doc_future);

            code_changed = code_result.unwrap_or_else(|e| {
                tracing::warn!("Code understanding analysis failed: {}", e);
                false
            });

            arch_changed = arch_result.unwrap_or_else(|e| {
                tracing::warn!("Architecture file analysis failed: {}", e);
                false
            });

            diagrams_changed = diagram_result.unwrap_or_else(|e| {
                tracing::warn!("Diagram extraction failed: {}", e);
                false
            });

            docs_changed = doc_result.unwrap_or_else(|e| {
                tracing::warn!("Documentation analysis failed: {}", e);
                false
            });
        }

        let any_changed = code_changed || arch_changed || diagrams_changed || docs_changed;

        // Check if we should continue
        if self.should_stop.load(Ordering::SeqCst) {
            return Ok(any_changed);
        }

        // =========================================================================
        // PHASE 2: AGGREGATION
        // Generate architecture summary and D2 diagrams from the extracted data.
        // Only run if the corresponding features are enabled.
        // =========================================================================

        let should_aggregate = any_changed && (run_arch || run_diagrams);
        if should_aggregate {
            tracing::info!("Starting aggregation phase for {}", repo.name);

            let arch_summary_future = async {
                if run_arch {
                    self.generate_architecture_summary(repo, endpoints).await
                } else {
                    Ok(())
                }
            };

            let diagrams_future = async {
                if run_diagrams {
                    self.generate_diagrams(repo, endpoints, &combined_hash)
                        .await
                } else {
                    Ok(())
                }
            };

            let (arch_summary_result, diagrams_result) =
                tokio::join!(arch_summary_future, diagrams_future);

            if let Err(e) = arch_summary_result {
                tracing::warn!(
                    "Failed to generate architecture summary for {}: {}",
                    repo.name,
                    e
                );
            }

            if let Err(e) = diagrams_result {
                tracing::warn!("Failed to generate diagrams for {}: {}", repo.name, e);
            }
        } else if !any_changed && (run_arch || run_diagrams) {
            tracing::debug!(
                "Skipping aggregation phase for {} - no files changed",
                repo.name
            );
        }

        // Check if we should continue
        if self.should_stop.load(Ordering::SeqCst) {
            return Ok(any_changed);
        }

        // =========================================================================
        // PHASE 3: MUTATION TESTING
        // This must be sequential as it modifies files in the temp directory.
        // Only run if mutation testing is enabled in the repo config.
        // =========================================================================

        if repo_config.enable_mutation_testing {
            if let Err(e) = self
                .run_mutation_testing(
                    repo,
                    endpoints,
                    temp_repo_path,
                    original_repo_path,
                    &repo_config,
                )
                .await
            {
                tracing::warn!("Failed to run mutation testing for {}: {}", repo.name, e);
            }
        }

        // temp_dir is dropped here, cleaning up the temp copy
        tracing::debug!("Cleaning up temp directory for {}", repo.name);
        drop(temp_dir);

        Ok(any_changed)
    }

    /// Run code understanding analysis on files (for File Analysis tab)
    async fn run_code_understanding_analysis(
        &self,
        repo: &crate::db::Repository,
        file_data: &[(PathBuf, String, String, Language)],
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<bool> {
        let (tx, rx) = mpsc::channel::<AnalysisTask>(100);
        let rx = Arc::new(TokioMutex::new(rx));

        let mut worker_handles = Vec::new();
        for endpoint in endpoints {
            let worker_rx = Arc::clone(&rx);
            let db = self.db.clone();
            let should_stop = Arc::clone(&self.should_stop);
            let endpoint = endpoint.clone();

            let handle =
                tokio::spawn(
                    async move { analysis_worker(endpoint, worker_rx, db, should_stop).await },
                );
            worker_handles.push(handle);
        }

        let repository_id = repo.id;
        let mut tasks_sent = 0;

        for (file_path, content, content_hash, language) in file_data {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            let file_path_str = file_path.to_string_lossy().to_string();

            // Check if file has changed since last code understanding analysis
            let existing_hash = self
                .db
                .get_latest_file_hash(
                    repository_id,
                    &file_path_str,
                    &AnalysisType::CodeUnderstanding.to_string(),
                )
                .await
                .unwrap_or(None);

            if existing_hash.as_ref() == Some(content_hash) {
                continue; // Skip unchanged file
            }

            let task = AnalysisTask {
                repository_id,
                file_path: file_path.clone(),
                content: content.clone(),
                content_hash: content_hash.clone(),
                task_type: AnalysisTaskType::CodeUnderstanding,
                language: *language,
            };

            if tx.send(task).await.is_err() {
                break;
            }
            tasks_sent += 1;
        }

        drop(tx);

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::warn!("Code understanding worker failed: {}", e);
            }
        }

        Ok(tasks_sent > 0)
    }

    /// Run architecture-focused file analysis (for Architecture summary aggregation)
    async fn run_architecture_file_analysis(
        &self,
        repo: &crate::db::Repository,
        file_data: &[(PathBuf, String, String, Language)],
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<bool> {
        let (tx, rx) = mpsc::channel::<AnalysisTask>(100);
        let rx = Arc::new(TokioMutex::new(rx));

        let mut worker_handles = Vec::new();
        for endpoint in endpoints {
            let worker_rx = Arc::clone(&rx);
            let db = self.db.clone();
            let should_stop = Arc::clone(&self.should_stop);
            let endpoint = endpoint.clone();

            let handle =
                tokio::spawn(
                    async move { analysis_worker(endpoint, worker_rx, db, should_stop).await },
                );
            worker_handles.push(handle);
        }

        let repository_id = repo.id;
        let mut tasks_sent = 0;

        for (file_path, content, content_hash, language) in file_data {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            let file_path_str = file_path.to_string_lossy().to_string();

            // Check if file has changed since last architecture file analysis
            let existing_hash = self
                .db
                .get_latest_file_hash(
                    repository_id,
                    &file_path_str,
                    &AnalysisType::ArchitectureFileAnalysis.to_string(),
                )
                .await
                .unwrap_or(None);

            if existing_hash.as_ref() == Some(content_hash) {
                continue;
            }

            let task = AnalysisTask {
                repository_id,
                file_path: file_path.clone(),
                content: content.clone(),
                content_hash: content_hash.clone(),
                task_type: AnalysisTaskType::ArchitectureFileAnalysis,
                language: *language,
            };

            if tx.send(task).await.is_err() {
                break;
            }
            tasks_sent += 1;
        }

        drop(tx);

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::warn!("Architecture file analysis worker failed: {}", e);
            }
        }

        Ok(tasks_sent > 0)
    }

    /// Run diagram extraction for all diagram types on all files
    async fn run_diagram_extractions(
        &self,
        repo: &crate::db::Repository,
        file_data: &[(PathBuf, String, String, Language)],
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<bool> {
        let (tx, rx) = mpsc::channel::<AnalysisTask>(100);
        let rx = Arc::new(TokioMutex::new(rx));

        let mut worker_handles = Vec::new();
        for endpoint in endpoints {
            let worker_rx = Arc::clone(&rx);
            let db = self.db.clone();
            let should_stop = Arc::clone(&self.should_stop);
            let endpoint = endpoint.clone();

            let handle =
                tokio::spawn(
                    async move { analysis_worker(endpoint, worker_rx, db, should_stop).await },
                );
            worker_handles.push(handle);
        }

        let repository_id = repo.id;
        let mut tasks_sent = 0;

        // For each diagram type, check if we need to extract for each file
        for diagram_type in DiagramType::all() {
            let analysis_type_str = format!("diagram_extraction_{}", diagram_type.as_str());

            for (file_path, content, content_hash, language) in file_data {
                if self.should_stop.load(Ordering::SeqCst) {
                    break;
                }

                let file_path_str = file_path.to_string_lossy().to_string();

                // Check if file has changed since last extraction for this diagram type
                let existing_hash = self
                    .db
                    .get_latest_file_hash(repository_id, &file_path_str, &analysis_type_str)
                    .await
                    .unwrap_or(None);

                if existing_hash.as_ref() == Some(content_hash) {
                    continue;
                }

                let task = AnalysisTask {
                    repository_id,
                    file_path: file_path.clone(),
                    content: content.clone(),
                    content_hash: content_hash.clone(),
                    task_type: AnalysisTaskType::DiagramExtraction(*diagram_type),
                    language: *language,
                };

                if tx.send(task).await.is_err() {
                    break;
                }
                tasks_sent += 1;
            }
        }

        drop(tx);

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::warn!("Diagram extraction worker failed: {}", e);
            }
        }

        Ok(tasks_sent > 0)
    }

    /// Run documentation analysis on context files (READMEs, Cargo.toml, .md files)
    async fn run_documentation_analysis(
        &self,
        repo: &crate::db::Repository,
        context_file_data: &[(PathBuf, String, String, Language)],
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<bool> {
        if context_file_data.is_empty() {
            return Ok(false);
        }

        let (tx, rx) = mpsc::channel::<AnalysisTask>(100);
        let rx = Arc::new(TokioMutex::new(rx));

        let mut worker_handles = Vec::new();
        for endpoint in endpoints {
            let worker_rx = Arc::clone(&rx);
            let db = self.db.clone();
            let should_stop = Arc::clone(&self.should_stop);
            let endpoint = endpoint.clone();

            let handle =
                tokio::spawn(
                    async move { analysis_worker(endpoint, worker_rx, db, should_stop).await },
                );
            worker_handles.push(handle);
        }

        let repository_id = repo.id;
        let mut tasks_sent = 0;

        for (file_path, content, content_hash, language) in context_file_data {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            let file_path_str = file_path.to_string_lossy().to_string();

            // Check if file has changed since last documentation analysis
            let existing_hash = self
                .db
                .get_latest_file_hash(
                    repository_id,
                    &file_path_str,
                    &AnalysisType::Documentation.to_string(),
                )
                .await
                .unwrap_or(None);

            if existing_hash.as_ref() == Some(content_hash) {
                continue;
            }

            let task = AnalysisTask {
                repository_id,
                file_path: file_path.clone(),
                content: content.clone(),
                content_hash: content_hash.clone(),
                task_type: AnalysisTaskType::DocumentationAnalysis,
                language: *language,
            };

            if tx.send(task).await.is_err() {
                break;
            }
            tasks_sent += 1;
        }

        drop(tx);

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::warn!("Documentation analysis worker failed: {}", e);
            }
        }

        Ok(tasks_sent > 0)
    }

    /// Generate D2 diagrams from extracted data
    async fn generate_diagrams(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
        combined_hash: &str,
    ) -> anyhow::Result<()> {
        tracing::info!("Generating D2 diagrams for {}", repo.name);

        self.db
            .update_daemon_status(
                "processing",
                Some(&format!("generating diagrams for {}", repo.name)),
            )
            .await?;

        for diagram_type in DiagramType::all() {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            // Check if diagrams need regeneration based on combined hash
            let existing_hash = self
                .db
                .get_latest_diagram_hash(repo.id, diagram_type.as_str())
                .await
                .unwrap_or(None);

            if existing_hash.as_ref() == Some(&combined_hash.to_string()) {
                tracing::debug!(
                    "Skipping {} diagram for {} - no changes",
                    diagram_type.title(),
                    repo.name
                );
                continue;
            }

            if let Err(e) = self
                .generate_single_diagram(repo, endpoints, *diagram_type, combined_hash)
                .await
            {
                tracing::warn!(
                    "Failed to generate {} diagram for {}: {}",
                    diagram_type.title(),
                    repo.name,
                    e
                );
            }
        }

        Ok(())
    }

    /// Generate a single D2 diagram with retry logic for syntax errors
    async fn generate_single_diagram(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
        diagram_type: DiagramType,
        combined_hash: &str,
    ) -> anyhow::Result<()> {
        let analysis_type_str = format!("diagram_extraction_{}", diagram_type.as_str());

        // Fetch all extractions for this diagram type
        let results = self
            .db
            .get_repository_results(repo.id, &analysis_type_str)
            .await?;

        if results.is_empty() {
            tracing::debug!(
                "No {} extractions found for {}",
                diagram_type.title(),
                repo.name
            );
            return Ok(());
        }

        // Build aggregated extractions, filtering out deleted files and empty results
        let mut extractions = String::new();
        let mut included_count = 0;
        for result in &results {
            let file_path = std::path::Path::new(&result.file_path);
            if !file_path.exists() {
                continue;
            }
            // Skip "no content" type responses
            let result_lower = result.result.to_lowercase();
            if result_lower.contains("no significant")
                || result_lower.contains("no database content")
                || result_lower.contains("minimal architectural")
            {
                continue;
            }
            extractions.push_str(&format!("\n## {}\n{}\n", result.file_path, result.result));
            included_count += 1;
        }

        if included_count == 0 {
            tracing::debug!(
                "No relevant {} extractions for {}",
                diagram_type.title(),
                repo.name
            );
            return Ok(());
        }

        // Truncate if too long
        let truncated = if extractions.len() > 50000 {
            format!(
                "{}...\n\n(truncated, {} files total)",
                &extractions[..50000],
                included_count
            )
        } else {
            extractions
        };

        // Generate the diagram with retry logic
        let prompt = DiagramGenerator::prompt_for_type(diagram_type, &repo.name, &truncated);

        let mut dot_code: Option<String> = None;
        let mut last_error: Option<String> = None;

        for attempt in 0..=DOT_MAX_RETRIES {
            let current_prompt = if attempt == 0 {
                prompt.clone()
            } else {
                // Use fix prompt for retries
                DiagramGenerator::fix_dot_prompt(
                    dot_code.as_deref().unwrap_or(""),
                    last_error.as_deref().unwrap_or("Unknown error"),
                )
            };

            // Try each endpoint
            for endpoint in endpoints {
                let client = OllamaClient::new(&endpoint.url, &endpoint.model);

                if !client.is_available().await {
                    continue;
                }

                match client.generate(&current_prompt).await {
                    Ok(raw_output) => {
                        let cleaned = clean_dot_output(&raw_output);

                        match validate_dot_syntax(&cleaned) {
                            Ok(()) => {
                                dot_code = Some(cleaned);
                                break;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    "DOT validation failed for {} (attempt {}): {}",
                                    diagram_type.title(),
                                    attempt + 1,
                                    e
                                );
                                dot_code = Some(cleaned);
                                last_error = Some(e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Endpoint {} failed for {} diagram: {}",
                            endpoint.name,
                            diagram_type.title(),
                            e
                        );
                    }
                }
            }

            // If we got valid DOT, break out of retry loop
            if dot_code.is_some() && last_error.is_none() {
                break;
            }

            // Clear error for next attempt if we're retrying
            if attempt < DOT_MAX_RETRIES && dot_code.is_some() {
                tracing::debug!(
                    "Retrying {} diagram generation (attempt {}/{})",
                    diagram_type.title(),
                    attempt + 2,
                    DOT_MAX_RETRIES + 1
                );
            }
        }

        // Render and save diagram if we got valid DOT
        match (dot_code, last_error) {
            (Some(code), None) => {
                // Render DOT to SVG
                let svg_content = match render_dot_to_svg(&code) {
                    Ok(svg) => svg,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to render {} diagram to SVG for {}: {}",
                            diagram_type.title(),
                            repo.name,
                            e
                        );
                        return Ok(());
                    }
                };

                tracing::info!(
                    "Generated {} diagram for {}",
                    diagram_type.title(),
                    repo.name
                );

                self.db
                    .save_diagram(
                        repo.id,
                        diagram_type.as_str(),
                        diagram_type.title(),
                        diagram_type.description(),
                        &code,
                        &svg_content,
                        Some(combined_hash),
                    )
                    .await?;
            }
            (Some(_), Some(e)) => {
                tracing::warn!(
                    "Failed to generate valid {} diagram for {} after {} retries: {}",
                    diagram_type.title(),
                    repo.name,
                    DOT_MAX_RETRIES,
                    e
                );
            }
            (None, _) => {
                tracing::warn!(
                    "No endpoints available for {} diagram generation",
                    diagram_type.title()
                );
            }
        }

        Ok(())
    }

    /// Generate an architectural summary by aggregating architecture file analysis results
    async fn generate_architecture_summary(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
    ) -> anyhow::Result<()> {
        tracing::info!("Generating architecture summary for {}", repo.name);

        self.db
            .update_daemon_status("processing", Some(&format!("summarizing {}", repo.name)))
            .await?;

        // Get documentation analysis results first (READMEs, Cargo.toml, etc.)
        // These provide high-level project context
        let doc_results = self
            .db
            .get_repository_results(repo.id, &AnalysisType::Documentation.to_string())
            .await?;

        // Get architecture file analysis results (not code understanding)
        // This provides architecture-focused analysis rather than granular code details
        let results = self
            .db
            .get_repository_results(repo.id, &AnalysisType::ArchitectureFileAnalysis.to_string())
            .await?;

        if results.is_empty() && doc_results.is_empty() {
            tracing::debug!(
                "No architecture file analyses to summarize for {}",
                repo.name
            );
            return Ok(());
        }

        // Build documentation context section (appears first in prompt)
        let mut doc_context = String::new();
        for result in &doc_results {
            let file_path = std::path::Path::new(&result.file_path);
            if !file_path.exists() {
                continue;
            }
            doc_context.push_str(&format!("\n## {}\n{}\n", result.file_path, result.result));
        }

        // Build a summary of all code file analyses, filtering out deleted files
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

        if included_count == 0 && doc_context.is_empty() {
            tracing::debug!("No existing files to summarize for {}", repo.name);
            return Ok(());
        }

        // Truncate code summaries if too long (keep under ~45k chars to leave room for docs)
        let truncated_code = if file_summaries.len() > 45000 {
            format!(
                "{}...\n\n(truncated, {} files total)",
                &file_summaries[..45000],
                included_count
            )
        } else {
            file_summaries
        };

        // Truncate doc context if needed (keep under ~5k chars)
        let truncated_docs = if doc_context.len() > 5000 {
            format!("{}...\n\n(documentation truncated)", &doc_context[..5000])
        } else {
            doc_context
        };

        // Build the prompt with documentation context first
        let doc_section = if !truncated_docs.is_empty() {
            format!(
                "# Project Documentation Context\n\
                 The following is extracted from project documentation (README, Cargo.toml, etc.):\n{}\n\n",
                truncated_docs
            )
        } else {
            String::new()
        };

        let prompt = format!(
            "You are analyzing a Rust codebase called '{}'.\n\n\
             {}\
             # Code Architecture Analyses\n\
             Below are architecture-focused analyses of individual source files:\n{}\n\n\
             Based on ALL the information above (documentation AND code analyses), \
             provide a high-level architectural overview including:\n\
             1. **Purpose**: What is this project/application about?\n\
             2. **Architecture**: What architectural patterns are used (e.g., layered, microservices, MVC)?\n\
             3. **Key Components**: What are the main modules/components and their responsibilities?\n\
             4. **Data Flow**: How does data flow through the system?\n\
             5. **Dependencies**: What external dependencies or integrations exist?\n\
             6. **Suggestions**: Any architectural improvements or concerns?\n\n\
             IMPORTANT: Respond only in English (or code)",
            repo.name, doc_section, truncated_code
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

                    // Save the summary
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
    ///
    /// Requires a `.noctum.toml` configuration file in the repository with mutation rules.
    /// Files without a matching rule are skipped. Baseline tests must pass before mutations.
    async fn run_mutation_testing(
        &self,
        repo: &crate::db::Repository,
        endpoints: &[OllamaEndpoint],
        temp_repo_path: &Path,
        original_repo_path: &Path,
        repo_config: &RepoConfig,
    ) -> anyhow::Result<()> {
        tracing::info!("Starting mutation testing for {}", repo.name);

        self.db
            .update_daemon_status(
                "processing",
                Some(&format!("mutation testing {}", repo.name)),
            )
            .await?;

        if repo_config.mutation.rules.is_empty() {
            tracing::info!(
                "No mutation rules configured for {}, skipping mutation testing",
                repo.name
            );
            return Ok(());
        }

        // Run baseline verification for each rule (both build and test commands)
        // Rules that fail baseline are excluded from mutation testing
        let mut valid_rules: Vec<&crate::repo_config::MutationRule> = Vec::new();

        tracing::info!(
            "Running baseline verification for {} mutation rule(s) in {}",
            repo_config.mutation.rules.len(),
            repo.name
        );

        for rule in &repo_config.mutation.rules {
            tracing::debug!(
                "Verifying baseline for rule '{}': build='{}', test='{}'",
                rule.glob,
                rule.build_command,
                rule.test_command
            );

            // Run build command first
            let build_result =
                run_command_with_timeout(temp_repo_path, &rule.build_command, rule.timeout_seconds)
                    .await;
            if !build_result.success {
                tracing::warn!(
                    "Excluding rule '{}' from mutation testing: baseline build '{}' failed",
                    rule.glob,
                    rule.build_command
                );
                tracing::debug!("Build output:\n{}", build_result.output);
                continue;
            }

            // Run test command
            let test_result =
                run_command_with_timeout(temp_repo_path, &rule.test_command, rule.timeout_seconds)
                    .await;
            if !test_result.success {
                tracing::warn!(
                    "Excluding rule '{}' from mutation testing: baseline test '{}' failed",
                    rule.glob,
                    rule.test_command
                );
                tracing::debug!("Test output:\n{}", test_result.output);
                continue;
            }

            tracing::debug!("Baseline passed for rule '{}'", rule.glob);
            valid_rules.push(rule);
        }

        if valid_rules.is_empty() {
            tracing::warn!(
                "No mutation rules passed baseline verification for {}, skipping mutation testing",
                repo.name
            );
            return Ok(());
        }

        tracing::info!(
            "{}/{} mutation rules passed baseline verification for {}",
            valid_rules.len(),
            repo_config.mutation.rules.len(),
            repo.name
        );

        let config = MutationConfig::default();

        // Find first available endpoint
        let (client, endpoint_name) = match find_available_endpoint(endpoints).await {
            Some((c, name)) => (c, name),
            None => {
                tracing::warn!("No endpoints available for mutation testing");
                return Ok(());
            }
        };

        // Discover projects to run mutation testing per-project
        let projects = discover_projects(temp_repo_path)?;

        let mut total_mutations = 0;
        let mut current_client = client;
        let mut current_endpoint_idx = endpoints
            .iter()
            .position(|e| e.name == endpoint_name)
            .unwrap_or(0);

        for project in projects {
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }

            // Find source files for this project
            let source_files = project.language.find_source_files(&project.root)?;

            for file_path in source_files {
                if self.should_stop.load(Ordering::SeqCst) {
                    break;
                }

                // Get relative path for glob matching
                let relative_path = file_path
                    .strip_prefix(temp_repo_path)
                    .unwrap_or(&file_path)
                    .to_string_lossy();

                // Find matching rule from validated rules only - skip file if no rule matches
                let rule = match valid_rules
                    .iter()
                    .find(|r| glob_match::glob_match(&r.glob, &relative_path))
                {
                    Some(r) => *r,
                    None => {
                        tracing::debug!("Skipping {}: no matching mutation rule", relative_path);
                        continue;
                    }
                };

                // Read file from temp copy
                let content = match tokio::fs::read_to_string(&file_path).await {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Use language-specific size limits for mutations
                let min_size = project.language.min_mutation_file_size();
                let max_size = project.language.max_mutation_file_size();
                if content.len() < min_size || content.len() > max_size {
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
                        if let Some((new_client, new_name)) =
                            find_available_endpoint(remaining).await
                        {
                            tracing::info!(
                                "Switching to endpoint {} for mutation analysis",
                                new_name
                            );
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

                    // Execute the mutation test using configured commands
                    let result = match execute_mutation_test(
                        &current_client,
                        &project.root,
                        mutation,
                        &content,
                        &config,
                        &rule.build_command,
                        &rule.test_command,
                        rule.timeout_seconds,
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!("Failed to execute mutation test: {}", e);
                            continue;
                        }
                    };

                    // Skip compile errors - they're not useful to the user
                    // Just log them for debugging purposes
                    if result.outcome == crate::mutation::TestOutcome::CompileError {
                        tracing::debug!(
                            "Mutation compile error (not saving): {} - {}",
                            original_file_path_str,
                            result.mutation.description
                        );
                        continue;
                    }

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
        }

        tracing::info!(
            "Completed mutation testing for {} ({} mutations)",
            repo.name,
            total_mutations
        );
        Ok(())
    }
}

/// Worker function for analysis tasks
async fn analysis_worker(
    endpoint: OllamaEndpoint,
    receiver: Arc<TokioMutex<mpsc::Receiver<AnalysisTask>>>,
    db: Database,
    should_stop: Arc<AtomicBool>,
) {
    let client = OllamaClient::new(&endpoint.url, &endpoint.model);

    if !client.is_available().await {
        tracing::warn!(
            "Ollama endpoint '{}' at {} is not available for generic analysis, skipping",
            endpoint.name,
            endpoint.url
        );
        return;
    }

    tracing::info!(
        "Analysis worker started for endpoint '{}' ({})",
        endpoint.name,
        endpoint.url
    );

    loop {
        if should_stop.load(Ordering::SeqCst) {
            tracing::debug!(
                "Generic worker for '{}' stopping due to shutdown signal",
                endpoint.name
            );
            break;
        }

        let task = {
            let mut rx = receiver.lock().await;
            tokio::select! {
                task = rx.recv() => task,
                _ = wait_for_stop_signal(&should_stop) => {
                    tracing::debug!(
                        "Generic worker for '{}' stopping due to shutdown signal",
                        endpoint.name
                    );
                    break;
                }
            }
        };

        let task = match task {
            Some(t) => t,
            None => {
                tracing::debug!(
                    "Generic worker for '{}' finished - no more tasks",
                    endpoint.name
                );
                break;
            }
        };

        let file_path_str = task.file_path.to_string_lossy().to_string();

        // Build the appropriate prompt based on task type and language
        let (prompt, analysis_type_str) = match task.task_type {
            AnalysisTaskType::ArchitectureFileAnalysis => {
                let prompt = DiagramExtractor::architecture_file_analysis_prompt(
                    &file_path_str,
                    &task.content,
                    task.language,
                );
                (prompt, AnalysisType::ArchitectureFileAnalysis.to_string())
            }
            AnalysisTaskType::DiagramExtraction(diagram_type) => {
                let prompt = DiagramExtractor::prompt_for_type(
                    diagram_type,
                    &file_path_str,
                    &task.content,
                    task.language,
                );
                let analysis_type = format!("diagram_extraction_{}", diagram_type.as_str());
                (prompt, analysis_type)
            }
            AnalysisTaskType::CodeUnderstanding => {
                // Use language-specific analysis prompt
                let prompt = task.language.analysis_prompt(&file_path_str, &task.content);
                (prompt, AnalysisType::CodeUnderstanding.to_string())
            }
            AnalysisTaskType::DocumentationAnalysis => {
                let prompt = DiagramExtractor::documentation_analysis_prompt(
                    &file_path_str,
                    &task.content,
                    task.language,
                );
                (prompt, AnalysisType::Documentation.to_string())
            }
        };

        tracing::info!(
            "Processing {} for: {} (endpoint: {})",
            analysis_type_str,
            file_path_str,
            endpoint.name
        );

        match client.generate(&prompt).await {
            Ok(result) => {
                tracing::info!("Completed {} for: {}", analysis_type_str, file_path_str);

                let severity = determine_severity(&result);

                if let Err(e) = db
                    .save_analysis_result(
                        task.repository_id,
                        &file_path_str,
                        &analysis_type_str,
                        &result,
                        severity.as_deref(),
                        Some(&task.content_hash),
                    )
                    .await
                {
                    tracing::warn!("Failed to save {} result: {}", analysis_type_str, e);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Generic worker '{}' failed {} for {}: {}",
                    endpoint.name,
                    analysis_type_str,
                    file_path_str,
                    e
                );
            }
        }
    }

    tracing::debug!(
        "Generic analysis worker for endpoint '{}' stopped",
        endpoint.name
    );
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
    // Language::Rust.find_source_files tests
    // =========================================================================

    #[test]
    fn test_find_source_files_empty_dir() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let files = Language::Rust.find_source_files(temp_dir.path()).unwrap();
        assert_eq!(files.len(), 0);
    }

    #[test]
    fn test_find_source_files_with_files() {
        let temp_dir = tempfile::TempDir::with_prefix("test_rust_files").unwrap();
        std::fs::write(temp_dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp_dir.path().join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(temp_dir.path().join("test.txt"), "not rust").unwrap();

        let files = Language::Rust.find_source_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.ends_with("main.rs")));
        assert!(files.iter().any(|f| f.ends_with("lib.rs")));
        assert!(!files.iter().any(|f| f.ends_with("test.txt")));
    }

    #[test]
    fn test_find_source_files_recursive() {
        let temp_dir = tempfile::TempDir::with_prefix("test_rust_files").unwrap();
        let subdir = temp_dir.path().join("src");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("main.rs"), "fn main() {}").unwrap();

        let files = Language::Rust.find_source_files(temp_dir.path()).unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("src/main.rs"));
    }

    #[test]
    fn test_find_source_files_excludes_target() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let target_dir = temp_dir.path().join("target");
        std::fs::create_dir_all(target_dir.join("debug")).unwrap();
        std::fs::write(target_dir.join("debug").join("lib.rs"), "fn test() {}").unwrap();

        let files = Language::Rust.find_source_files(temp_dir.path()).unwrap();

        assert!(!files.iter().any(|f| f.to_string_lossy().contains("target")));
    }

    #[test]
    fn test_find_source_files_excludes_hidden() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let hidden_dir = temp_dir.path().join(".hidden");
        std::fs::create_dir_all(&hidden_dir).unwrap();
        std::fs::write(hidden_dir.join("lib.rs"), "fn test() {}").unwrap();

        let files = Language::Rust.find_source_files(temp_dir.path()).unwrap();

        assert!(!files
            .iter()
            .any(|f| f.to_string_lossy().contains(".hidden")));
    }
}
