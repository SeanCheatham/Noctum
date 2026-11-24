//! Repository setup, checks, and run reports.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use noctum::config::Config;
use noctum::db::Database;
use noctum::repo_config::RepoConfig;
use noctum::{builder, lockfile, process, runtime, worktree};

async fn git(repo: &Path, args: &[&str]) -> process::CmdOutput {
    process::run(repo, "git", args, &[], Duration::from_secs(15)).await
}

pub(super) async fn repository_root(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize().context("cannot find repository path")?;
    let output = git(&path, &["rev-parse", "--show-toplevel"]).await;
    if !output.success {
        bail!("cannot find Git repository: {}", output.combined());
    }
    Ok(PathBuf::from(output.stdout.trim()).canonicalize()?)
}

fn manifest(path: &Path) -> Result<toml::Value> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("read {}", path.display()))
}

fn packages(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let root_manifest = root.join("Cargo.toml");
    let value = manifest(&root_manifest)?;
    let exclusions: Vec<&str> = value
        .get("workspace")
        .and_then(|v| v.get("exclude"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    let mut paths = vec![root_manifest];
    if let Some(members) = value
        .get("workspace")
        .and_then(|v| v.get("members"))
        .and_then(|v| v.as_array())
    {
        for member in members {
            let member = member.as_str().context("workspace member must be a path")?;
            let pattern = format!(
                "{}/{member}/Cargo.toml",
                glob::Pattern::escape(&root.to_string_lossy())
            );
            for path in glob::glob(&pattern)? {
                let path = path?;
                let dir = path.parent().context("package directory is missing")?;
                let relative = dir.strip_prefix(root).unwrap_or(dir).to_string_lossy();
                if !exclusions
                    .iter()
                    .any(|p| glob_match::glob_match(p, &relative))
                {
                    paths.push(path);
                }
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    while let Some(path) = paths.pop() {
        let path = path.canonicalize()?;
        if !path.starts_with(root) {
            bail!(
                "workspace member {} is outside the Git repository",
                path.display()
            );
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        let directory = path.parent().context("package directory is missing")?;
        if directory != root
            && exclusions.iter().any(|p| {
                glob_match::glob_match(
                    p,
                    &directory
                        .strip_prefix(root)
                        .unwrap_or(directory)
                        .to_string_lossy(),
                )
            })
        {
            continue;
        }
        let package = manifest(&path)?;
        if value.get("workspace").is_some() {
            let mut sections = vec![&package];
            sections.extend(
                package
                    .get("target")
                    .and_then(|v| v.as_table())
                    .into_iter()
                    .flat_map(|v| v.values()),
            );
            for section in sections {
                for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    for (name, dependency) in section
                        .get(kind)
                        .and_then(|v| v.as_table())
                        .into_iter()
                        .flat_map(|v| v.iter())
                    {
                        let inherited =
                            dependency.get("workspace").and_then(|v| v.as_bool()) == Some(true);
                        let definition = if inherited {
                            value
                                .get("workspace")
                                .and_then(|v| v.get("dependencies"))
                                .and_then(|v| v.get(name))
                        } else {
                            Some(dependency)
                        };
                        if let Some(local) = definition
                            .and_then(|v| v.get("path"))
                            .and_then(|v| v.as_str())
                        {
                            let base = if inherited { root } else { directory };
                            let dependency = base.join(local).join("Cargo.toml").canonicalize()?;
                            if dependency.starts_with(root) {
                                paths.push(dependency);
                            }
                        }
                    }
                }
            }
        }
        if let Some(name) = package
            .get("package")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
        {
            result.push((name.to_string(), path));
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

fn select_package(root: &Path, requested: Option<&str>) -> Result<(String, PathBuf)> {
    let packages = packages(root)?;
    if let Some(name) = requested {
        return packages
            .into_iter()
            .find(|(n, _)| n == name)
            .with_context(|| format!("package {name} is not in the workspace members"));
    }
    let value = manifest(&root.join("Cargo.toml"))?;
    let defaults = value
        .get("workspace")
        .and_then(|v| v.get("default-members"))
        .and_then(|v| v.as_array());
    if let Some(defaults) = defaults {
        let selected: Vec<_> = packages
            .iter()
            .filter(|(_, path)| {
                let Ok(relative) = path.parent().unwrap().strip_prefix(root) else {
                    return false;
                };
                let relative = relative.to_string_lossy();
                let relative = if relative.is_empty() { "." } else { &relative };
                defaults
                    .iter()
                    .filter_map(|v| v.as_str())
                    .any(|pattern| glob_match::glob_match(pattern, relative))
            })
            .collect();
        if let [package] = selected.as_slice() {
            return Ok((*package).clone());
        }
    } else if let Some(name) = value
        .get("package")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
    {
        if let Some(package) = packages.iter().find(|(n, _)| n == name) {
            return Ok(package.clone());
        }
    }
    if packages.len() == 1 {
        return Ok(packages.into_iter().next().unwrap());
    }
    let names = packages
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("set package in noctum.toml; workspace packages: {names}")
}

pub async fn init(
    config: &Config,
    path: &Path,
    package: Option<&str>,
    config_path: Option<&Path>,
) -> Result<()> {
    let command = command_prefix(config_path);
    let root = repository_root(path).await?;
    worktree::head_sha(&root)
        .await
        .context("repository needs a Git commit")?;
    let config_path = root.join("noctum.toml");
    let existing = match fs::symlink_metadata(&config_path) {
        Ok(_) => Some(RepoConfig::load(&root)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    if let (Some(existing), Some(requested)) = (&existing, package) {
        if existing.package.as_deref() != Some(requested) {
            bail!("noctum.toml already exists; set package in that file");
        }
    }
    let requested = existing
        .as_ref()
        .and_then(|c| c.package.as_deref())
        .or(package);
    let (name, _) = select_package(&root, requested).with_context(|| {
        format!(
            "cannot select a package; use {command} init --package NAME, or correct noctum.toml",
        )
    })?;
    let db = super::open_db(config).await?;
    if existing.is_none() {
        let text = format!("package = {}\n", toml::Value::String(name.clone()));
        println!("Create {}:\n{text}", config_path.display());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&config_path)?;
        file.write_all(text.as_bytes())?;
    } else {
        println!("Keep {}", config_path.display());
    }
    let root_text = root.to_str().context("repository path must be UTF-8")?;
    if let Some(repo) = db.get_repository_by_path(root_text).await? {
        println!("Repository is registered: {}", repo.path);
        if !repo.enabled {
            println!("Repository is disabled. Enable it in the dashboard.");
        }
    } else {
        db.add_repository(root_text, &name).await?;
        println!("Register {}", root.display());
    }
    println!("Package: {name}");
    println!("Next: {command} doctor --repo {}", shell_quote(root_text));
    Ok(())
}

struct Checks<'a> {
    failures: usize,
    command: &'a str,
}

impl Checks<'_> {
    fn record(&mut self, label: &str, result: Result<()>, fix: &str) {
        match result {
            Ok(()) => println!("PASS {label}"),
            Err(err) => {
                self.failures += 1;
                let fix = fix.replace("noctum ", &format!("{} ", self.command));
                println!("FAIL {label}: {err:#}\n  {fix}");
            }
        }
    }
}

pub async fn doctor(config: &Config, path: &Path, config_path: Option<&Path>) -> Result<()> {
    let command = command_prefix(config_path);
    let mut checks = Checks {
        failures: 0,
        command: &command,
    };
    let root = repository_root(path).await;
    let root = match root {
        Ok(root) => {
            checks.record("Git repository", Ok(()), "");
            Some(root)
        }
        Err(err) => {
            checks.record(
                "Git repository",
                Err(err),
                "Use --repo with a Git repository path. Check that git is installed.",
            );
            None
        }
    };
    if let Some(root) = &root {
        checks.record(
            "Git commit",
            worktree::head_sha(root).await.map(|_| ()),
            "Create the first Git commit.",
        );
        match worktree::status_porcelain(root).await {
            Ok(status) if !status.is_empty() => println!(
                "NOTE Working files have changes. Noctum uses the committed source at HEAD."
            ),
            Ok(_) => println!("PASS Working files have no changes"),
            Err(err) => checks.record(
                "Git status",
                Err(err),
                "Run git status and correct the reported error.",
            ),
        }
        let conflicts = git(root, &["diff", "--name-only", "--diff-filter=U"]).await;
        let result = if !conflicts.success || !conflicts.stdout.trim().is_empty() {
            Err(anyhow::anyhow!(
                "Git conflict check failed: {}",
                conflicts.combined()
            ))
        } else {
            Ok(())
        };
        checks.record(
            "Git conflicts",
            result,
            "Complete or abort the current Git operation.",
        );
        match RepoConfig::load(root) {
            Ok(repo) => {
                checks.record("Repository configuration", Ok(()), "");
                let package = select_package(root, repo.package.as_deref());
                if let Ok((name, manifest)) = &package {
                    println!("Package: {name}");
                    let unwind = builder::forces_abort(root, manifest).and_then(|abort| {
                        if abort {
                            bail!("panic = abort prevents the measurement fence from closing")
                        }
                        Ok(())
                    });
                    checks.record(
                        "Panic configuration",
                        unwind,
                        "Set panic = \"unwind\" in the Cargo configuration.",
                    );
                }
                checks.record(
                    "Cargo workspace",
                    package.map(|_| ()),
                    "Set package in noctum.toml to a workspace package name.",
                );
            }
            Err(err) => checks.record(
                "Repository configuration",
                Err(err.into()),
                "Run noctum init, or correct noctum.toml and its file permissions.",
            ),
        }
        let registration = async {
            let db = Database::open_read_only(&config.database_path()).await?;
            let repo = db
                .get_repository_by_path(root.to_str().context("repository path must be UTF-8")?)
                .await?
                .context("repository is not registered")?;
            if !repo.enabled {
                bail!("repository is disabled")
            }
            Ok(())
        }
        .await;
        checks.record(
            "Repository registration",
            registration,
            "Run noctum init. If the repository is disabled, enable it in the dashboard.",
        );
    }
    println!("Check model file hashes. This can take some time.");
    let data = config.data_dir();
    let quant = config.model.quant;
    let model = tokio::task::spawn_blocking(move || lockfile::verify_model(&data, quant)).await?;
    checks.record("Model and server", model, "Run noctum model pull.");
    checks.record(
        "Apple Containerization helper",
        runtime::engine_ready().await,
        "Build or install noctum-host with Noctum.",
    );
    let image = if runtime::image_ready(&config.data_dir()).await {
        Ok(())
    } else {
        Err(anyhow::anyhow!("worker assets are missing"))
    };
    checks.record("Measurement worker", image, "Run noctum runtime prepare.");
    println!(
        "Target: {} (worker)",
        noctum::target::GuestTarget::Aarch64.triple()
    );
    if checks.failures != 0 {
        bail!("{} setup checks failed", checks.failures)
    }
    println!("Setup checks passed. Build and measurement checks run during noctum run.");
    Ok(())
}

pub async fn review(config: &Config, path: &Path, config_path: Option<&Path>) -> Result<()> {
    let command = command_prefix(config_path);
    let root = repository_root(path).await?;
    if !config.database_path().is_file() {
        println!("No run history. Run {command} init to register this repository.");
        return Ok(());
    }
    let db = Database::open_read_only(&config.database_path()).await?;
    println!("{}", review_report(&db, &root, &command).await?);
    Ok(())
}

async fn review_report(db: &Database, root: &Path, command: &str) -> Result<String> {
    let root_text = root.to_str().context("repository path must be UTF-8")?;
    let repo = db
        .get_repository_by_path(root_text)
        .await?
        .with_context(|| format!("repository is not registered; run {command} init"))?;
    let Some(run) = db.latest_run(repo.id).await? else {
        return Ok("No runs for this repository.".into());
    };
    let mut text = format!(
        "Repository: {}\nRun: {} ({})\nStarted: {}\nFinished: {}\nBase commit: {}\nTarget: {}\n",
        repo.path,
        run.id,
        run.status,
        run.started_at,
        run.finished_at.as_deref().unwrap_or("run is active"),
        run.git_sha,
        run.target
    );
    if let Some(err) = &run.error {
        writeln!(text, "Run error: {err}")?;
    }
    writeln!(
        text,
        "Counter profile: {}",
        if run.measurement_profile.is_empty() {
            "legacy-tb-v1"
        } else {
            &run.measurement_profile
        }
    )?;
    let trials = db.trials_for_run(run.id).await?;
    let accepted: Vec<_> = trials.iter().filter(|t| t.status == "accepted").collect();
    writeln!(text, "\nAccepted changes: {}", accepted.len())?;
    if !accepted.is_empty() {
        writeln!(
            text,
            "Instruction counts do not establish an elapsed-time improvement."
        )?;
        writeln!(
            text,
            "Changes can depend on earlier commits on noctum/perf. Apply changes in commit order."
        )?;
    }
    for trial in accepted {
        writeln!(
            text,
            "\nTrial {}: {}\nTest: {} :: {}\nFile: {}",
            trial.id, trial.symbol, trial.harness, trial.test_name, trial.file
        )?;
        writeln!(
            text,
            "Instructions: {} -> {}",
            count(trial.baseline_count),
            count(trial.insn_count)
        )?;
        if trial.memory_before_json.is_some() || trial.memory_after_json.is_some() {
            writeln!(
                text,
                "Memory: {} -> {}",
                noctum::optimization::memory_text(trial.memory_before_json.as_deref()),
                noctum::optimization::memory_text(trial.memory_after_json.as_deref())
            )?;
        }
        if let Some(analysis) = &trial.analysis_json {
            writeln!(text, "Source analysis: {analysis}")?;
        }
        if let Some(ratio) = trial.improvement_ratio {
            writeln!(text, "Reduction: {:.2}%", ratio * 100.0)?;
        }
        writeln!(
            text,
            "Test suite check: {}\nOutput check: {}",
            check_word(trial.host_check_passed),
            check_word(trial.stdout_matched)
        )?;
        if let Some(summary) = &trial.model_summary {
            writeln!(text, "Model summary: {summary}")?;
        }
        if let Some(sha) = db.commit_for_trial(trial.id).await? {
            writeln!(text, "Commit: {sha}")?;
            if valid_sha(&sha)
                && git(root, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
                    .await
                    .success
            {
                let parent = git(root, &["rev-parse", &format!("{sha}^")]).await;
                if parent.success {
                    writeln!(text, "Parent commit: {}", parent.stdout.trim())?;
                }
                writeln!(
                    text,
                    "To apply: git -C {} cherry-pick {sha}",
                    shell_quote(root_text)
                )?;
            } else {
                writeln!(
                    text,
                    "Commit is not available in this repository. Review the saved diff below."
                )?;
            }
        } else {
            writeln!(text, "Commit is pending. No Git command is available.")?;
        }
        writeln!(
            text,
            "Diff:\n{}",
            trial.unified_diff.as_deref().unwrap_or("No saved diff.")
        )?;
    }
    let others: Vec<_> = trials.iter().filter(|t| t.status != "accepted").collect();
    if !others.is_empty() {
        writeln!(text, "\nOther trials:")?;
    }
    for trial in others {
        writeln!(
            text,
            "Trial {}: {}: {} ({})",
            trial.id,
            trial.symbol,
            trial.status,
            trial.reason.as_deref().unwrap_or("no recorded reason")
        )?;
    }
    writeln!(text, "\nBaseline tests:")?;
    let baselines = db.baselines_for_run(run.id).await?;
    if baselines.is_empty() {
        writeln!(text, "No recorded baseline tests.")?;
    }
    for baseline in baselines {
        writeln!(
            text,
            "{} :: {}: {}, {} instructions{}",
            baseline.harness,
            baseline.test_name,
            baseline.status,
            count(baseline.insn_count),
            baseline
                .reason
                .as_ref()
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        )?;
    }
    Ok(text)
}

fn count(value: Option<i64>) -> String {
    value
        .map(|n| n.to_string())
        .unwrap_or_else(|| "not recorded".into())
}

fn check_word(passed: bool) -> &'static str {
    if passed {
        "passed"
    } else {
        "not passed"
    }
}

fn valid_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_prefix(config_path: Option<&Path>) -> String {
    match config_path {
        Some(path) => format!("noctum --config {}", shell_quote(&path.to_string_lossy())),
        None => "noctum".into(),
    }
}
