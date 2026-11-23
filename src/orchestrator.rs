//! One repository, one night. QEMU and the model are called from here only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::builder::{self, BuiltHarness, CargoTarget, ResolvedPackage};
use crate::config::Config;
use crate::db::{Database, Repository};
use crate::diffcheck::{self, parse_diff};
use crate::gate::{self, GateDecision, GateInput};
use crate::hashutil::{self, sha256_bytes, sha256_file};
use crate::model::{self, PromptInput};
use crate::ports::{
    comparable_stdout, CheckOutput, Completion, HostCheck, Inferencer, MeasureRequest, Measurement,
    Measurer,
};
use crate::profile::{self, HotFunction, LineHit};
use crate::repo_config::RepoConfig;
use crate::selector::{self, DiscoveredTest, FnSpan};
use crate::worktree::{self, PerfSync};
use crate::{fence, LOG_LIMIT};

pub struct RunOptions {
    pub ignore_window: bool,
    pub stop: Arc<AtomicBool>,
    pub asset_error: Option<String>,
    pub only_repo: Option<PathBuf>,
    pub test: Option<String>,
    pub max_attempts: Option<u32>,
    pub measurement_only: bool,
    pub retry: bool,
}

impl RunOptions {
    pub fn manual(stop: Arc<AtomicBool>) -> Self {
        Self {
            ignore_window: true,
            stop,
            asset_error: None,
            only_repo: None,
            test: None,
            max_attempts: None,
            measurement_only: false,
            retry: false,
        }
    }
}

pub struct Night<M, I, H> {
    pub db: Database,
    pub config: Config,
    pub measurer: M,
    pub inferencer: I,
    pub host: H,
    execution: crate::runtime::Execution,
    attempts: AtomicU32,
}

impl<M, I, H> Night<M, I, H>
where
    M: Measurer,
    I: Inferencer,
    H: HostCheck,
{
    pub fn new(db: Database, config: Config, measurer: M, inferencer: I, host: H) -> Self {
        Self {
            db,
            config,
            measurer,
            inferencer,
            host,
            execution: crate::runtime::Execution::default(),
            attempts: AtomicU32::new(0),
        }
    }

    pub fn with_execution(mut self, execution: crate::runtime::Execution) -> Self {
        self.execution = execution;
        self
    }

    pub async fn run_pass(&self, opts: &RunOptions) -> Result<()> {
        if let Some(err) = &opts.asset_error {
            self.db.set_daemon("error", "idle", Some(err)).await?;
            return Ok(());
        }
        let repos = self.db.enabled_repositories().await?;
        for repo in repos {
            if !self.proceed(opts) {
                break;
            }
            if let Some(only) = &opts.only_repo {
                if !worktree::same_path(Path::new(&repo.path), only) {
                    continue;
                }
            }
            if self.already_ran(&repo, opts).await? {
                continue;
            }
            self.run_repository(&repo, opts).await?;
        }
        self.db.set_daemon("idle", "idle", None).await?;
        Ok(())
    }

    pub async fn run_repository(&self, repo: &Repository, opts: &RunOptions) -> Result<()> {
        let result = self.try_repository(repo, opts).await;
        if result.is_err() {
            let _ = self.db.set_daemon("idle", "idle", None).await;
        }
        result
    }

    async fn try_repository(&self, repo: &Repository, opts: &RunOptions) -> Result<()> {
        if let Some(err) = &opts.asset_error {
            return self.finish_early(repo, "", "error", err, None).await;
        }
        let user_repo = PathBuf::from(&repo.path);
        if !user_repo.is_dir() {
            return self
                .finish_early(repo, "", "skipped", "repository path is missing", None)
                .await;
        }
        let user_repo = user_repo.canonicalize()?;
        let head = match worktree::head_sha(&user_repo).await {
            Ok(sha) => sha,
            Err(err) => {
                return self
                    .finish_early(repo, "", "skipped", &err.to_string(), None)
                    .await
            }
        };
        let mut repo_config = match RepoConfig::load(&user_repo) {
            Ok(config) => config,
            Err(err) => {
                return self
                    .finish_early(repo, &head, "skipped", &err.to_string(), None)
                    .await
            }
        };
        if let Some(test) = &opts.test {
            repo_config.tests = vec![test.clone()];
            repo_config.benches.clear();
        }
        self.execution.ensure_target(&user_repo).await?;
        self.phase("building", &repo.name).await;
        let dest = if opts.measurement_only {
            self.config.data_dir().join("measurements/source")
        } else {
            self.config
                .data_dir()
                .join("worktrees")
                .join(repo.id.to_string())
        };
        let sync = if opts.measurement_only {
            worktree::snapshot(&user_repo, &dest).await?;
            PerfSync::Fresh
        } else {
            worktree::sync_perf(&user_repo, &head, &dest).await?
        };
        if let PerfSync::Conflict(detail) = sync {
            return self
                .finish_early(repo, &head, "skipped", &detail, None)
                .await;
        }
        let source_sha = if opts.measurement_only {
            worktree::head_sha(&dest).await?
        } else {
            head
        };
        let run_id = self
            .db
            .insert_run(
                repo.id,
                &source_sha,
                repo_config.package.as_deref(),
                "ok",
                self.window_key(opts).as_deref(),
            )
            .await?;
        self.db
            .set_run_target(run_id, self.execution.target().triple())
            .await?;
        self.db
            .set_run_profile(run_id, &self.measurement_profile().await?)
            .await?;
        let result = self
            .after_sync(repo, opts, run_id, &user_repo, &dest, &repo_config)
            .await;
        let cleanup = worktree::restore_tracked(&dest, &repo_config.clean_paths).await;
        let result = result.and(cleanup);
        if let Err(err) = &result {
            let _ = self
                .db
                .finish_run(run_id, "error", Some(&err.to_string()), None)
                .await;
        }
        if opts.measurement_only {
            let removal = worktree::remove(&user_repo, &dest).await;
            return result.and(removal);
        }
        result
    }

    async fn after_sync(
        &self,
        repo: &Repository,
        opts: &RunOptions,
        run_id: i64,
        user_repo: &Path,
        dest: &Path,
        repo_config: &RepoConfig,
    ) -> Result<()> {
        if !opts.measurement_only {
            self.retry_pending(repo.id, dest, repo_config).await?;
        }
        if !self.proceed(opts) {
            return self.close(run_id, dest, "ok", None).await;
        }
        let built = match self.select_and_build(dest, repo_config).await {
            Ok(built) => built,
            Err(err) => {
                return self
                    .close(run_id, dest, "skipped", Some(&err.to_string()))
                    .await
            }
        };
        if !self.proceed(opts) {
            return self.close(run_id, dest, "ok", None).await;
        }
        self.phase("measuring", &repo.name).await;
        let mut measured = self
            .baseline(run_id, dest, repo_config, &built, opts)
            .await?;
        if opts.measurement_only {
            return self.close(run_id, dest, "ok", None).await;
        }
        let eligible: Vec<&ScoredTest> = measured.iter().filter(|test| test.eligible).collect();
        if eligible.is_empty() {
            return self.close(run_id, dest, "ok", None).await;
        }
        if !self.proceed(opts) {
            return self.close(run_id, dest, "ok", None).await;
        }
        let (mut picks, mapped) = self
            .rank(repo.id, dest, repo_config, &measured, opts.retry)
            .await?;
        if !mapped {
            return self
                .close(
                    run_id,
                    dest,
                    "error",
                    Some("profile did not map guest PCs back to repository source"),
                )
                .await;
        }
        let limit = self.config.gate.max_candidates as usize;
        picks.truncate(limit);
        self.store_candidates(run_id, &mut picks).await?;
        self.trials(repo, opts, run_id, dest, repo_config, &mut measured, &picks)
            .await?;
        self.close(run_id, dest, "ok", None).await?;
        let _ = user_repo;
        Ok(())
    }

    async fn select_and_build(&self, dest: &Path, repo_config: &RepoConfig) -> Result<BuildSet> {
        let package =
            builder::resolve_package_with(&self.execution, dest, repo_config.package.as_deref())
                .await?;
        if builder::forces_abort(&package.workspace_root, &package.manifest)? {
            bail!("panic = \"abort\" is set; the fence would not close");
        }
        let found = gather_tests(&package, repo_config);
        let benches = split_benches(repo_config, &package)?;
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        let fence_dir = fence::install_fence_crate(&self.config.data_dir())?;
        let mut selected = Vec::new();
        let build_tests =
            !found.is_empty() || !repo_config.tests.is_empty() || benches.score.is_empty();
        let mut linked = Vec::new();
        if build_tests {
            let targets = requested_test_targets(&package, &found, repo_config);
            let mut harnesses = if let Some(targets) = targets {
                builder::build_test_targets_with(
                    &self.execution,
                    dest,
                    &package.name,
                    repo_config,
                    &targets,
                )
                .await?
            } else {
                builder::build_tests_with(&self.execution, dest, &package.name, repo_config).await?
            };
            harnesses.retain(|h| {
                package
                    .targets
                    .iter()
                    .any(|t| t.harness && t.test_enabled && same_file(&t.src_path, &h.src_path))
            });
            let active = active_tests(
                &self.execution,
                &found,
                &harnesses,
                repo_config.timeout_seconds,
            )
            .await?;
            selected = select_tests(&active, repo_config, self.config.gate.max_scan as usize)?;
            if selected.iter().any(|test| test.skip.is_none()) {
                inject_selected(dest, &package.manifest, &fence_dir, &selected)?;
                let targets = selected_test_targets(&package, &selected);
                let rebuilt = builder::build_test_targets_with(
                    &self.execution,
                    dest,
                    &package.name,
                    repo_config,
                    &targets,
                )
                .await?;
                harnesses.retain(|h| !rebuilt.iter().any(|r| r.logical_name == h.logical_name));
                harnesses.extend(rebuilt);
            }
            linked = link_harnesses(
                &self.execution,
                dest,
                &package,
                &selected,
                &harnesses,
                repo_config.timeout_seconds,
            )
            .await?;
        }
        if !benches.score.is_empty() {
            ensure_fence_manifest(&package.manifest, &fence_dir)?;
            for target in &benches.score {
                // Capture `main` before the fence is inserted. The fence is
                // stripped again before the model sees the tree.
                let mut unit = linked_bench(&package, target, PathBuf::new());
                inject_main(&target.src_path)?;
                let built = builder::build_bench_with(
                    &self.execution,
                    dest,
                    &package.name,
                    &target.name,
                    repo_config,
                )
                .await?;
                unit.executable = builder::pick_bench(&built, &target.name)?;
                linked.push(unit);
            }
        }
        for harness in &linked {
            if harness.discovered.skip.is_none() {
                check_elf(dest, &harness.executable)?;
            }
        }
        let mut sources = source_files(&selected);
        sources.extend(benches.score.iter().map(|target| target.src_path.clone()));
        restore_fences(dest, &package.manifest, &sources).await?;
        Ok(BuildSet {
            tests: linked,
            bench_skips: benches.skips,
        })
    }

    async fn baseline(
        &self,
        run_id: i64,
        dest: &Path,
        repo_config: &RepoConfig,
        built: &BuildSet,
        opts: &RunOptions,
    ) -> Result<Vec<ScoredTest>> {
        let mut scored = Vec::new();
        let mut eligible = 0u32;
        for (name, reason) in &built.bench_skips {
            let reason = crate::skip::explain("skipped_env", Some(reason));
            self.db
                .insert_baseline(
                    run_id,
                    name,
                    name,
                    "skipped_env",
                    None,
                    None,
                    None,
                    None,
                    reason.as_deref(),
                )
                .await?;
        }
        for test in &built.tests {
            if let Some(reason) = &test.discovered.skip {
                self.store_baseline(run_id, test, "skipped_env", None, None, Some(reason), None)
                    .await?;
                continue;
            }
            // A listed bench is requested by name. The scan cap applies to
            // discovered tests, not to that list.
            let scan_capped = test.bench.is_none()
                && (eligible >= self.config.gate.max_tests || !self.proceed(opts));
            if scan_capped || (test.bench.is_some() && !self.proceed(opts)) {
                continue;
            }
            self.phase(
                "measuring",
                &format!("{} :: {}", test.logical, test.score_name),
            )
            .await;
            let pair = self.measure_pair(dest, repo_config, test).await?;
            match pair {
                Pair::Fail(reason) => {
                    self.store_baseline(
                        run_id,
                        test,
                        "skipped_fail",
                        None,
                        None,
                        Some(&reason),
                        None,
                    )
                    .await?;
                }
                Pair::Mismatch => {
                    self.store_baseline(
                        run_id,
                        test,
                        "skipped_nondet",
                        None,
                        None,
                        Some("two baseline runs disagreed"),
                        None,
                    )
                    .await?;
                }
                Pair::Match(measurement) => {
                    let status = if measurement.dump.error.is_some() || measurement.total == 0 {
                        "skipped_fail"
                    } else if measurement.total < self.config.gate.min_insn_delta {
                        "skipped_small"
                    } else {
                        "measured"
                    };
                    let reason = measurement.dump.error.clone().or_else(|| {
                        (status == "skipped_fail").then(|| "fence did not fire".to_string())
                    });
                    let wall_ms = measurement.wall_ms as i64;
                    self.store_baseline(
                        run_id,
                        test,
                        status,
                        Some(measurement.total as i64),
                        Some(&sha256_bytes(&measurement.stdout)),
                        reason.as_deref(),
                        Some(wall_ms),
                    )
                    .await?;
                    if status == "measured" {
                        eligible += 1;
                        scored.push(ScoredTest {
                            identity: test.clone(),
                            baseline: measurement,
                            eligible: true,
                        });
                    }
                }
            }
        }
        Ok(scored)
    }

    async fn rank(
        &self,
        repository_id: i64,
        dest: &Path,
        repo_config: &RepoConfig,
        measured: &[ScoredTest],
        retry: bool,
    ) -> Result<(Vec<Pick>, bool)> {
        let spans = collect_spans(dest).await?;
        let mut best: HashMap<(String, String), Pick> = HashMap::new();
        let mut mapped_function = false;
        let trace = std::env::var_os("NOCTUM_PROFILE_TRACE").is_some();
        for test in measured {
            if !test.eligible {
                continue;
            }
            let elf = std::fs::read(&test.identity.executable)?;
            let located = locate_dump(&elf, &test.baseline)?;
            if trace {
                let mut blocks = test.baseline.dump.blocks.clone();
                blocks.sort_by_key(|block| std::cmp::Reverse(block.1));
                eprintln!(
                    "PROFILE_MAP test={} bias={:#x} start={:#x} end={:#x} blocks={}",
                    test.identity.discovered.fn_path,
                    test.baseline.load_bias,
                    test.baseline.map_start,
                    test.baseline.map_end,
                    blocks.len()
                );
                for (pc, n) in blocks.into_iter().take(8) {
                    let addr = pc.wrapping_sub(test.baseline.load_bias);
                    eprintln!(
                        "PROFILE_HIT pc={pc:#x} addr={addr:#x} n={n} hit={:?}",
                        located.get(&addr)
                    );
                }
            }
            let ranked = profile::rank(
                &test.baseline.dump,
                test.baseline.map_start,
                test.baseline.map_end,
                test.baseline.load_bias,
                |addr| located.get(&addr).cloned(),
                &spans,
                dest,
                &repo_config.protected_paths,
                &test.identity.discovered.fn_path,
            );
            // The only candidates are functions this dump mapped. A miss does
            // not fall back to the test name or any other symbol.
            mapped_function |= ranked.mapped_function;
            for hot in ranked.functions {
                let blob = worktree::file_blob(dest, &hot.file).await?;
                if !retry
                    && self
                        .db
                        .trial_exists(
                            repository_id,
                            &test.identity.discovered.fn_path,
                            &hot.name,
                            &blob,
                            self.execution.target().triple(),
                            &self.measurement_profile().await?,
                        )
                        .await?
                {
                    continue;
                }
                let key = (hot.file.clone(), hot.name.clone());
                let replace = best
                    .get(&key)
                    .map(|existing| hot.exclusive > existing.hot.exclusive)
                    .unwrap_or(true);
                if replace {
                    best.insert(
                        key,
                        Pick {
                            hot,
                            test_name: test.identity.score_name.clone(),
                            harness: test.identity.logical.clone(),
                            blob,
                            candidate_id: 0,
                        },
                    );
                }
            }
        }
        let mut picks: Vec<Pick> = best.into_values().collect();
        picks.sort_by(|a, b| {
            b.hot
                .exclusive
                .cmp(&a.hot.exclusive)
                .then(a.hot.name.cmp(&b.hot.name))
        });
        if trace {
            if picks.is_empty() {
                eprintln!("PROFILE_RANK none mapped_function={mapped_function}");
            }
            for (index, pick) in picks.iter().enumerate() {
                eprintln!(
                    "PROFILE_RANK {index} symbol={} file={}:{}-{} exclusive={} share={:.4} test={}",
                    pick.hot.name,
                    pick.hot.file,
                    pick.hot.line_start,
                    pick.hot.line_end,
                    pick.hot.exclusive,
                    pick.hot.share,
                    pick.test_name
                );
            }
        }
        Ok((picks, mapped_function))
    }

    async fn store_candidates(&self, run_id: i64, picks: &mut [Pick]) -> Result<()> {
        for (index, pick) in picks.iter_mut().enumerate() {
            pick.candidate_id = self
                .db
                .insert_candidate(
                    run_id,
                    &pick.test_name,
                    &pick.harness,
                    index as i64,
                    &pick.hot.name,
                    &pick.hot.file,
                    pick.hot.line_start as i64,
                    pick.hot.line_end as i64,
                    pick.hot.exclusive as i64,
                    pick.hot.share,
                    &pick.blob,
                )
                .await?;
        }
        Ok(())
    }

    async fn trials(
        &self,
        repo: &Repository,
        opts: &RunOptions,
        run_id: i64,
        dest: &Path,
        repo_config: &RepoConfig,
        measured: &mut [ScoredTest],
        picks: &[Pick],
    ) -> Result<()> {
        for (index, pick) in picks.iter().enumerate() {
            if !self.proceed(opts)
                || opts
                    .max_attempts
                    .is_some_and(|max| self.attempts.load(Ordering::SeqCst) >= max)
            {
                break;
            }
            let still_eligible = measured
                .iter()
                .any(|test| test.eligible && test.identity.score_name == pick.test_name);
            if !still_eligible {
                continue;
            }
            // An accept changes the file. The next function was ranked on the
            // previous blob; it is still tried, and the trial is recorded
            // against the blob the model actually sees.
            let blob_now = worktree::file_blob(dest, &pick.hot.file).await?;
            if blob_now != pick.blob {
                if !function_remains(dest, &pick.hot.file, &pick.hot.name) {
                    continue;
                }
                if !opts.retry
                    && self
                        .db
                        .trial_exists(
                            repo.id,
                            &pick.test_name,
                            &pick.hot.name,
                            &blob_now,
                            self.execution.target().triple(),
                            &self.measurement_profile().await?,
                        )
                        .await?
                {
                    continue;
                }
                self.db
                    .set_candidate_blob(pick.candidate_id, &blob_now)
                    .await?;
            }
            let flow = self
                .one_trial(repo, opts, run_id, dest, repo_config, measured, pick)
                .await?;
            match flow {
                TrialFlow::Stop => break,
                TrialFlow::Accepted if index + 1 < picks.len() && self.proceed(opts) => {
                    if !self.rebaseline(dest, repo_config, measured).await? {
                        break;
                    }
                }
                TrialFlow::Accepted | TrialFlow::Next => {}
            }
        }
        Ok(())
    }

    async fn one_trial(
        &self,
        _repo: &Repository,
        opts: &RunOptions,
        _run_id: i64,
        dest: &Path,
        repo_config: &RepoConfig,
        measured: &[ScoredTest],
        pick: &Pick,
    ) -> Result<TrialFlow> {
        let base = worktree::head_sha(dest).await?;
        let test = measured
            .iter()
            .find(|test| test.identity.score_name == pick.test_name)
            .context("candidate test is missing")?;
        self.phase("inferring", &pick.hot.name).await;
        let completion = match self.propose(dest, test, pick, None, None, opts).await? {
            Proposal::Ready(completion) => completion,
            Proposal::Stop => return Ok(TrialFlow::Stop),
            Proposal::Failed => return Ok(TrialFlow::Next),
        };
        let validated = self
            .check_proposal(dest, repo_config, test, &completion)
            .await;
        let completion = match validated {
            Ok(()) => completion,
            Err(reason) => {
                self.reject(pick, &reason, Some(&completion), false, false, None, test)
                    .await?;
                return Ok(TrialFlow::Next);
            }
        };
        if !self.proceed(opts) {
            return Ok(TrialFlow::Stop);
        }
        worktree::apply(dest, &completion.diff, false).await?;
        let host = self
            .host_with_repair(dest, repo_config, test, pick, &completion, &base, opts)
            .await?;
        let HostRound::Ready(summary, diff) = host else {
            return Ok(if matches!(host, HostRound::Stop) {
                TrialFlow::Stop
            } else {
                TrialFlow::Next
            });
        };
        if !self.proceed(opts) {
            self.reset(dest, &base, repo_config).await?;
            return Ok(TrialFlow::Stop);
        }
        self.phase("measuring", &pick.test_name).await;
        let rebuilt = self.rebuild_one(dest, repo_config, test).await;
        let rebuilt = match rebuilt {
            Ok(rebuilt) => rebuilt,
            Err(err) => {
                self.reset(dest, &base, repo_config).await?;
                self.reject(
                    pick,
                    &err.to_string(),
                    Some(&Completion { summary, diff }),
                    true,
                    false,
                    None,
                    test,
                )
                .await?;
                return Ok(TrialFlow::Next);
            }
        };
        let pair = self.measure_pair(dest, repo_config, &rebuilt).await?;
        self.finish_measurement(dest, repo_config, test, pick, &base, &summary, &diff, pair)
            .await
    }

    async fn finish_measurement(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &ScoredTest,
        pick: &Pick,
        base: &str,
        summary: &str,
        diff: &str,
        pair: Pair,
    ) -> Result<TrialFlow> {
        let completion = Completion {
            summary: summary.to_string(),
            diff: diff.to_string(),
        };
        let measurement = match pair {
            Pair::Match(measurement) => measurement,
            Pair::Mismatch => {
                self.reset(dest, base, repo_config).await?;
                self.reject(
                    pick,
                    "nondeterministic",
                    Some(&completion),
                    true,
                    false,
                    None,
                    test,
                )
                .await?;
                return Ok(TrialFlow::Next);
            }
            Pair::Fail(reason) => {
                self.reset(dest, base, repo_config).await?;
                self.reject(pick, &reason, Some(&completion), true, false, None, test)
                    .await?;
                return Ok(TrialFlow::Next);
            }
        };
        let decision = gate::decide(&GateInput {
            baseline: test.baseline.total,
            candidate: measurement.total,
            repeats_match: true,
            guest_ok: measurement.exit_code == 0 && measurement.dump.error.is_none(),
            host_ok: true,
            stdout_matches: comparable_stdout(&measurement.stdout)
                == comparable_stdout(&test.baseline.stdout),
            stderr_matches: measurement.stderr == test.baseline.stderr,
            diff_ok: true,
            min_improvement_ratio: self.config.gate.min_improvement_ratio,
            min_insn_delta: self.config.gate.min_insn_delta,
        });
        match decision {
            GateDecision::Accept {
                improvement,
                delta: _,
            } => {
                self.accept(
                    dest,
                    repo_config,
                    pick,
                    test,
                    &measurement,
                    summary,
                    diff,
                    improvement,
                )
                .await?;
                return Ok(TrialFlow::Accepted);
            }
            GateDecision::Reject { reason } => {
                self.reset(dest, base, repo_config).await?;
                self.reject(
                    pick,
                    reason,
                    Some(&completion),
                    true,
                    comparable_stdout(&measurement.stdout)
                        == comparable_stdout(&test.baseline.stdout),
                    Some(&measurement),
                    test,
                )
                .await?;
            }
        }
        Ok(TrialFlow::Next)
    }

    async fn propose(
        &self,
        dest: &Path,
        test: &ScoredTest,
        pick: &Pick,
        repair_diff: Option<&str>,
        repair_output: Option<&str>,
        opts: &RunOptions,
    ) -> Result<Proposal> {
        if !self.proceed(opts) {
            return Ok(Proposal::Stop);
        }
        if self
            .attempts
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                if opts.max_attempts.is_some_and(|max| used >= max) {
                    None
                } else {
                    used.checked_add(1)
                }
            })
            .is_err()
        {
            tracing::info!("model request limit reached");
            return Ok(Proposal::Stop);
        }
        tracing::info!(
            attempt = self.attempts.load(Ordering::SeqCst),
            repair = repair_diff.is_some(),
            "request model change"
        );
        let hot = refreshed_hot(dest, &pick.hot);
        let test_source = function_source(dest, &test.identity.discovered);
        let callees = callee_sources(dest, &hot);
        let messages = model::build_messages_for_target(
            &PromptInput {
                test_name: &test.identity.score_name,
                test_source: &test_source,
                hot: &hot,
                callee_sources: &callees,
                repair_diff,
                repair_output,
            },
            self.execution.target().triple(),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        self.execution.suspend_for_inference().await?;
        let watcher = spawn_cancel(
            cancel.clone(),
            opts.stop.clone(),
            opts.ignore_window,
            self.config.schedule.clone(),
        );
        let result = self.inferencer.complete(&messages, &cancel).await;
        watcher.abort();
        match result {
            Ok(mut completion) => {
                completion.diff = diffcheck::add_missing_file_headers(&completion.diff);
                Ok(Proposal::Ready(completion))
            }
            Err(err) if cancel.load(Ordering::SeqCst) => {
                tracing::info!("inference cancelled: {err}");
                Ok(Proposal::Stop)
            }
            Err(err) => {
                self.reject_error(pick, &err.to_string()).await?;
                Ok(Proposal::Failed)
            }
        }
    }

    async fn check_proposal(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &ScoredTest,
        completion: &Completion,
    ) -> std::result::Result<(), String> {
        let test_file = rel_path(dest, &test.identity.discovered.file);
        let mut files = HashMap::new();
        if let Some(path) = &test_file {
            let text = std::fs::read_to_string(dest.join(path)).unwrap_or_default();
            files.insert(path.clone(), text);
        }
        if let Ok(parsed) = parse_diff(&completion.diff) {
            for file in parsed {
                let text = std::fs::read_to_string(dest.join(&file.path)).unwrap_or_default();
                files.insert(file.path, text);
            }
        }
        diffcheck::check_diff(
            &completion.diff,
            &files,
            test_file.as_deref(),
            &test.identity.discovered.file_fn_path,
            &repo_config.protected_paths,
        )?;
        worktree::apply_check(dest, &completion.diff)
            .await
            .map_err(|err| err.to_string())
    }

    async fn host_with_repair(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &ScoredTest,
        pick: &Pick,
        completion: &Completion,
        base: &str,
        opts: &RunOptions,
    ) -> Result<HostRound> {
        let first = self.confirm_host(dest, repo_config, test).await?;
        if first.success {
            return Ok(HostRound::Ready(
                completion.summary.clone(),
                completion.diff.clone(),
            ));
        }
        if !self.proceed(opts) {
            self.reset(dest, base, repo_config).await?;
            return Ok(HostRound::Stop);
        }
        let tail = hashutil::last_lines(&first.output, 80);
        tracing::warn!(output = %tail, "test suite check failed");
        self.reset(dest, base, repo_config).await?;
        self.phase("inferring", "repair").await;
        let repair = match self
            .propose(dest, test, pick, Some(&completion.diff), Some(&tail), opts)
            .await?
        {
            Proposal::Ready(repair) => repair,
            Proposal::Stop => {
                self.reset(dest, base, repo_config).await?;
                if opts
                    .max_attempts
                    .is_some_and(|max| self.attempts.load(Ordering::SeqCst) >= max)
                    && self.proceed(opts)
                {
                    self.reject(
                        pick,
                        "host check failed; model request limit reached",
                        Some(completion),
                        false,
                        false,
                        None,
                        test,
                    )
                    .await?;
                }
                return Ok(HostRound::Stop);
            }
            Proposal::Failed => {
                self.reset(dest, base, repo_config).await?;
                return Ok(HostRound::Done);
            }
        };
        self.reset(dest, base, repo_config).await?;
        if let Err(reason) = self.check_proposal(dest, repo_config, test, &repair).await {
            // A repair that lowers a fixed bound, drops an assertion, or
            // otherwise fails validation ends the candidate. The trial stores
            // that reason so a later sample is not drawn for it.
            self.reject(pick, &reason, Some(&repair), false, false, None, test)
                .await?;
            return Ok(HostRound::Done);
        }
        worktree::apply(dest, &repair.diff, false).await?;
        let second = self.confirm_host(dest, repo_config, test).await?;
        if second.success {
            return Ok(HostRound::Ready(repair.summary, repair.diff));
        }
        self.reset(dest, base, repo_config).await?;
        let output = hashutil::tail_str(&second.output, LOG_LIMIT);
        self.reject(
            pick,
            "host check failed",
            Some(&repair),
            false,
            false,
            None,
            test,
        )
        .await?;
        let _ = output;
        Ok(HostRound::Done)
    }

    /// Host `cargo test`, and for a bench also `cargo bench --no-run`.
    ///
    /// `cargo test` does not compile a harness=false bench. A bench that fails
    /// to build is the same repair as a host-suite failure, still only once.
    async fn confirm_host(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &ScoredTest,
    ) -> Result<CheckOutput> {
        self.phase("checking", &test.identity.score_name).await;
        let package = pick_package(&self.execution, dest, repo_config).await?;
        let check = self.host.run(dest, repo_config, &package).await?;
        if !check.success {
            return Ok(check);
        }
        let Some(name) = &test.identity.bench else {
            return Ok(check);
        };
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        match builder::build_bench_with(&self.execution, dest, &package, name, repo_config).await {
            Ok(_) => Ok(check),
            Err(err) => Ok(CheckOutput {
                success: false,
                timed_out: false,
                output: err.to_string(),
            }),
        }
    }

    async fn rebuild_one(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &ScoredTest,
    ) -> Result<LinkedTest> {
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        let package =
            builder::resolve_package_with(&self.execution, dest, repo_config.package.as_deref())
                .await?;
        let fence_dir = fence::install_fence_crate(&self.config.data_dir())?;
        if let Some(name) = &test.identity.bench {
            ensure_fence_manifest(&package.manifest, &fence_dir)?;
            inject_main(&test.identity.discovered.file)?;
            let built =
                builder::build_bench_with(&self.execution, dest, &package.name, name, repo_config)
                    .await?;
            let executable = builder::pick_bench(&built, name)?;
            check_elf(dest, &executable)?;
            restore_fences(
                dest,
                &package.manifest,
                std::slice::from_ref(&test.identity.discovered.file),
            )
            .await?;
            let mut linked = test.identity.clone();
            linked.executable = executable;
            return Ok(linked);
        }
        let selected = vec![test.identity.discovered.clone()];
        inject_selected(dest, &package.manifest, &fence_dir, &selected)?;
        let harnesses = builder::build_test_targets_with(
            &self.execution,
            dest,
            &package.name,
            repo_config,
            &selected_test_targets(&package, &selected),
        )
        .await?;
        let mut linked = link_harnesses(
            &self.execution,
            dest,
            &package,
            &selected,
            &harnesses,
            repo_config.timeout_seconds,
        )
        .await?;
        restore_fences(dest, &package.manifest, &source_files(&selected)).await?;
        linked.pop().context("rebuild produced no harness")
    }

    /// `Ok(false)` means a measured test has no new count that repeated.
    /// Later candidates must not be scored against the pre-accept baseline.
    async fn rebaseline(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        measured: &mut [ScoredTest],
    ) -> Result<bool> {
        if measured.is_empty() {
            return Ok(true);
        }
        self.phase("measuring", "rebaseline").await;
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        let package =
            builder::resolve_package_with(&self.execution, dest, repo_config.package.as_deref())
                .await?;
        let fence_dir = fence::install_fence_crate(&self.config.data_dir())?;
        let tests: Vec<DiscoveredTest> = measured
            .iter()
            .filter(|test| test.identity.bench.is_none())
            .map(|test| test.identity.discovered.clone())
            .collect();
        let mut sources = source_files(&tests);
        let linked = if tests.is_empty() {
            Vec::new()
        } else {
            inject_selected(dest, &package.manifest, &fence_dir, &tests)?;
            let harnesses = builder::build_test_targets_with(
                &self.execution,
                dest,
                &package.name,
                repo_config,
                &selected_test_targets(&package, &tests),
            )
            .await?;
            link_harnesses(
                &self.execution,
                dest,
                &package,
                &tests,
                &harnesses,
                repo_config.timeout_seconds,
            )
            .await?
        };
        let mut bench_bins: HashMap<String, PathBuf> = HashMap::new();
        if measured.iter().any(|test| test.identity.bench.is_some()) {
            ensure_fence_manifest(&package.manifest, &fence_dir)?;
        }
        for test in measured.iter() {
            let Some(name) = &test.identity.bench else {
                continue;
            };
            if bench_bins.contains_key(name) {
                continue;
            }
            inject_main(&test.identity.discovered.file)?;
            sources.push(test.identity.discovered.file.clone());
            let built =
                builder::build_bench_with(&self.execution, dest, &package.name, name, repo_config)
                    .await?;
            bench_bins.insert(name.clone(), builder::pick_bench(&built, name)?);
        }
        restore_fences(dest, &package.manifest, &sources).await?;
        for test in measured.iter_mut() {
            if let Some(name) = &test.identity.bench {
                let Some(executable) = bench_bins.get(name) else {
                    tracing::warn!("rebaseline lost bench {name}; stopping further candidates");
                    return Ok(false);
                };
                test.identity.executable = executable.clone();
            } else {
                let Some(linked_test) = linked
                    .iter()
                    .find(|item| item.score_name == test.identity.score_name)
                else {
                    tracing::warn!(
                        "rebaseline lost {}; stopping further candidates",
                        test.identity.score_name
                    );
                    return Ok(false);
                };
                test.identity.executable = linked_test.executable.clone();
            }
            let label = test.identity.score_name.clone();
            match self.measure_pair(dest, repo_config, &test.identity).await? {
                Pair::Match(measurement) => {
                    if measurement.dump.error.is_some() || measurement.total == 0 {
                        tracing::warn!("rebaseline of {label} had no fenced count");
                        return Ok(false);
                    }
                    test.eligible = measurement.total >= self.config.gate.min_insn_delta;
                    test.baseline = measurement;
                }
                Pair::Mismatch => {
                    tracing::warn!("rebaseline of {label} disagreed; stopping further candidates");
                    return Ok(false);
                }
                Pair::Fail(reason) => {
                    tracing::warn!(
                        "rebaseline of {label} failed ({reason}); stopping further candidates"
                    );
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    async fn accept(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        pick: &Pick,
        test: &ScoredTest,
        measurement: &Measurement,
        summary: &str,
        diff: &str,
        improvement: f64,
    ) -> Result<()> {
        let message = format!(
            "{summary}\n\nTest: {}\nTarget: {}\nBaseline: {}\nCandidate: {}\nRatio: {improvement:.6}\n",
            pick.test_name, self.execution.target().triple(), test.baseline.total, measurement.total
        );
        let paths = worktree::diff_paths(diff);
        let sha = worktree::commit(
            dest,
            &paths,
            &message,
            &self.config.general.author_name,
            &self.config.general.author_email,
        )
        .await;
        let sha = match sha {
            Ok(sha) => sha,
            Err(err) => {
                self.reset(
                    dest,
                    &worktree::head_sha(dest).await.unwrap_or_default(),
                    repo_config,
                )
                .await?;
                let trial = self
                    .db
                    .insert_trial(
                        pick.candidate_id,
                        "accepted",
                        None,
                        Some(summary),
                        Some(diff),
                        Some(test.baseline.total as i64),
                        Some(measurement.total as i64),
                        Some(improvement),
                        true,
                        true,
                        Some(&err.to_string()),
                    )
                    .await?;
                let _ = trial;
                return Ok(());
            }
        };
        let trial = self
            .db
            .insert_trial(
                pick.candidate_id,
                "accepted",
                None,
                Some(summary),
                Some(diff),
                Some(test.baseline.total as i64),
                Some(measurement.total as i64),
                Some(improvement),
                true,
                true,
                None,
            )
            .await?;
        self.db.insert_commit(trial, &sha).await?;
        Ok(())
    }

    async fn reject(
        &self,
        pick: &Pick,
        reason: &str,
        completion: Option<&Completion>,
        host_ok: bool,
        stdout_ok: bool,
        measurement: Option<&Measurement>,
        test: &ScoredTest,
    ) -> Result<()> {
        self.db
            .insert_trial(
                pick.candidate_id,
                "rejected",
                Some(reason),
                completion.map(|item| item.summary.as_str()),
                completion.map(|item| item.diff.as_str()),
                Some(test.baseline.total as i64),
                measurement.map(|item| item.total as i64),
                measurement.map(|item| ratio(test.baseline.total, item.total)),
                host_ok,
                stdout_ok,
                None,
            )
            .await?;
        Ok(())
    }

    async fn reject_error(&self, pick: &Pick, reason: &str) -> Result<()> {
        self.db
            .insert_trial(
                pick.candidate_id,
                "error",
                Some(reason),
                None,
                None,
                None,
                None,
                None,
                false,
                false,
                None,
            )
            .await?;
        Ok(())
    }

    async fn measure_pair(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &LinkedTest,
    ) -> Result<Pair> {
        let first = self.measure_once(dest, repo_config, test).await?;
        if !scored_ok(&first) {
            return Ok(Pair::Fail(fail_reason(&first)));
        }
        let second = self.measure_once(dest, repo_config, test).await?;
        if !scored_ok(&second) {
            return Ok(Pair::Fail(fail_reason(&second)));
        }
        if first.matches(&second) {
            Ok(Pair::Match(first))
        } else {
            let application_count = |m: &Measurement| {
                m.dump
                    .blocks
                    .iter()
                    .filter(|(pc, _)| *pc >= m.map_start && *pc < m.map_end)
                    .map(|(_, count)| count)
                    .sum::<u64>()
            };
            tracing::warn!(test = %test.score_name, first_count = first.total, second_count = second.total, first_application_count = application_count(&first), second_application_count = application_count(&second), output_matched = comparable_stdout(&first.stdout) == comparable_stdout(&second.stdout), "measurement runs disagreed");
            Ok(Pair::Mismatch)
        }
    }

    async fn measure_once(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &LinkedTest,
    ) -> Result<Measurement> {
        let scratch = self
            .config
            .data_dir()
            .join("scratch")
            .join(unique_scratch());
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)?;
        let measurement = self
            .measurer
            .measure(&MeasureRequest {
                worktree: dest.to_path_buf(),
                working_directory: Some(test.working_directory.clone()),
                harness: test.executable.clone(),
                guest_exe: guest_exe(dest, &test.executable),
                args: test.guest_args.clone(),
                timeout_secs: repo_config.timeout_seconds,
                scratch: scratch.clone(),
                memory_mb: self.config.guest.memory_mb,
            })
            .await;
        let _ = std::fs::remove_dir_all(&scratch);
        let measurement = measurement?;
        if !scored_ok(&measurement) {
            let output = format!(
                "{}\n{}\n{}",
                String::from_utf8_lossy(&measurement.stdout),
                String::from_utf8_lossy(&measurement.stderr),
                measurement.serial_log
            );
            tracing::warn!(test = %test.score_name, output = %hashutil::tail_str(&output, LOG_LIMIT), "guest measurement failed");
        }
        Ok(measurement)
    }

    async fn retry_pending(
        &self,
        repository_id: i64,
        dest: &Path,
        repo_config: &RepoConfig,
    ) -> Result<()> {
        let pending = self.db.pending_commits(repository_id).await?;
        for item in pending {
            if item.target != self.execution.target().triple() {
                continue;
            }
            let base = worktree::head_sha(dest).await?;
            if worktree::apply(dest, &item.unified_diff, false)
                .await
                .is_err()
            {
                continue;
            }
            let message = format!(
                "{}\n\nTest: {}\nTarget: {}\nBaseline: {}\nCandidate: {}\nRatio: {:.6}\n",
                item.model_summary.unwrap_or_default(),
                item.test_name,
                item.target,
                item.baseline_count.unwrap_or(0),
                item.insn_count.unwrap_or(0),
                item.improvement_ratio.unwrap_or(0.0)
            );
            let paths = worktree::diff_paths(&item.unified_diff);
            match worktree::commit(
                dest,
                &paths,
                &message,
                &self.config.general.author_name,
                &self.config.general.author_email,
            )
            .await
            {
                Ok(sha) => self.db.insert_commit(item.trial_id, &sha).await?,
                Err(_) => self.reset(dest, &base, repo_config).await?,
            }
        }
        Ok(())
    }

    async fn store_baseline(
        &self,
        run_id: i64,
        test: &LinkedTest,
        status: &str,
        insn_count: Option<i64>,
        stdout_sha256: Option<&str>,
        reason: Option<&str>,
        wall_ms: Option<i64>,
    ) -> Result<()> {
        let binary = sha256_file(&test.executable).ok();
        let reason = crate::skip::explain(status, reason);
        self.db
            .insert_baseline(
                run_id,
                &test.logical,
                &test.score_name,
                status,
                insn_count,
                stdout_sha256,
                binary.as_deref(),
                wall_ms,
                reason.as_deref(),
            )
            .await
    }

    async fn reset(&self, dest: &Path, sha: &str, repo_config: &RepoConfig) -> Result<()> {
        if sha.is_empty() {
            return Ok(());
        }
        worktree::reset_hard(dest, sha).await?;
        worktree::clean(dest, &repo_config.clean_paths).await?;
        Ok(())
    }

    async fn close(
        &self,
        run_id: i64,
        dest: &Path,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let perf = worktree::head_sha(dest).await.ok();
        self.db
            .finish_run(run_id, status, error, perf.as_deref())
            .await?;
        self.db.set_daemon("idle", "idle", None).await?;
        Ok(())
    }

    async fn finish_early(
        &self,
        repo: &Repository,
        sha: &str,
        status: &str,
        error: &str,
        perf: Option<&str>,
    ) -> Result<()> {
        let id = self.db.insert_run(repo.id, sha, None, status, None).await?;
        self.db
            .set_run_target(id, self.execution.target().triple())
            .await?;
        self.db
            .set_run_profile(id, &self.measurement_profile().await?)
            .await?;
        self.db.finish_run(id, status, Some(error), perf).await?;
        Ok(())
    }

    async fn already_ran(&self, repo: &Repository, opts: &RunOptions) -> Result<bool> {
        if opts.ignore_window {
            return Ok(false);
        }
        let Some(key) = self.window_key(opts) else {
            return Ok(false);
        };
        self.db
            .repo_finished_window(
                repo.id,
                &key,
                self.execution.target().triple(),
                &self.measurement_profile().await?,
            )
            .await
    }

    async fn measurement_profile(&self) -> Result<String> {
        self.execution
            .measurement_profile(&self.config.data_dir(), self.config.guest.memory_mb)
            .await
    }

    fn proceed(&self, opts: &RunOptions) -> bool {
        !opts.stop.load(Ordering::SeqCst)
            && (opts.ignore_window || self.config.schedule.is_in_window())
    }

    fn window_key(&self, opts: &RunOptions) -> Option<String> {
        if self.config.schedule.is_in_window() {
            Some(self.config.schedule.window_key_at(Local::now()))
        } else if opts.ignore_window {
            None
        } else {
            Some(self.config.schedule.window_key_at(Local::now()))
        }
    }

    async fn phase(&self, phase: &str, detail: &str) {
        let _ = self.db.set_daemon("running", phase, Some(detail)).await;
    }
}

enum TrialFlow {
    Next,
    Stop,
    Accepted,
}

enum Proposal {
    Ready(Completion),
    Stop,
    Failed,
}

enum Pair {
    Match(Measurement),
    Mismatch,
    Fail(String),
}

enum HostRound {
    Ready(String, String),
    Done,
    Stop,
}

struct BuildSet {
    tests: Vec<LinkedTest>,
    bench_skips: Vec<(String, String)>,
}

struct BenchSplit {
    skips: Vec<(String, String)>,
    score: Vec<CargoTarget>,
}

fn split_benches(repo_config: &RepoConfig, package: &ResolvedPackage) -> Result<BenchSplit> {
    let mut skips = Vec::new();
    let mut score = Vec::new();
    for name in &repo_config.benches {
        let target = package
            .targets
            .iter()
            .find(|target| target.kind == "bench" && target.name == *name);
        let Some(target) = target else {
            bail!("bench {name} is not a cargo target");
        };
        let src = std::fs::read_to_string(&target.src_path).unwrap_or_default();
        if let Some(reason) = bench_reason(target.harness, &src, package.uses_criterion) {
            skips.push((name.clone(), reason.to_string()));
        } else {
            score.push(target.clone());
        }
    }
    Ok(BenchSplit { skips, score })
}

#[derive(Clone)]
struct LinkedTest {
    discovered: DiscoveredTest,
    logical: String,
    executable: PathBuf,
    working_directory: PathBuf,
    /// Libtest path, or the cargo target name of a harness=false bench.
    score_name: String,
    guest_args: Vec<String>,
    /// Set for a bench. The guest runs that binary with no libtest filter.
    bench: Option<String>,
}

struct ScoredTest {
    identity: LinkedTest,
    baseline: Measurement,
    eligible: bool,
}

struct Pick {
    hot: HotFunction,
    test_name: String,
    harness: String,
    blob: String,
    candidate_id: i64,
}

#[derive(Clone)]
struct Found {
    discovered: DiscoveredTest,
    logical: String,
}

pub fn bench_reason(harness: bool, src: &str, uses_criterion: bool) -> Option<&'static str> {
    if uses_criterion || src.contains("criterion::") {
        return Some("adaptive bench");
    }
    if harness || src.contains("#[bench]") {
        return Some("adaptive bench");
    }
    if !src.contains("fn main") {
        return Some("bench has no fn main");
    }
    None
}

fn gather_tests(package: &ResolvedPackage, repo_config: &RepoConfig) -> Vec<Found> {
    let mut found = Vec::new();
    for target in &package.targets {
        if !matches!(target.kind.as_str(), "lib" | "bin" | "test")
            || !target.harness
            || !target.test_enabled
        {
            continue;
        }
        let logical =
            selector::logical_harness(&package.workspace_root, &target.src_path, &target.kind);
        for mut test in selector::discover_harness(&target.src_path) {
            test.harness = Some(logical.clone());
            if selector::glob_skipped(&test.fn_path, &repo_config.skip_tests)
                || selector::glob_skipped(&short_name(&test.fn_path), &repo_config.skip_tests)
            {
                test.skip = Some("skip_tests".into());
            }
            found.push(Found {
                logical: logical.clone(),
                discovered: test,
            });
        }
    }
    found
}

fn selected_test_targets(
    package: &ResolvedPackage,
    selected: &[DiscoveredTest],
) -> Vec<CargoTarget> {
    package
        .targets
        .iter()
        .filter(|target| {
            let logical =
                selector::logical_harness(&package.workspace_root, &target.src_path, &target.kind);
            target.harness
                && target.test_enabled
                && selected
                    .iter()
                    .any(|test| test.skip.is_none() && test.harness.as_deref() == Some(&logical))
        })
        .cloned()
        .collect()
}

fn requested_test_targets(
    package: &ResolvedPackage,
    found: &[Found],
    config: &RepoConfig,
) -> Option<Vec<CargoTarget>> {
    if config.tests.is_empty() {
        return None;
    }
    let mut logicals = Vec::new();
    for raw in &config.tests {
        let id = selector::parse_test_id(raw);
        if let Some(harness) = id.harness {
            logicals.push(harness);
        } else {
            let matched: Vec<_> = found
                .iter()
                .filter(|f| name_matches(&f.discovered.fn_path, &id.name))
                .collect();
            if matched.is_empty() {
                return None;
            }
            logicals.extend(matched.iter().map(|f| f.logical.clone()));
        }
    }
    Some(
        package
            .targets
            .iter()
            .filter(|target| {
                target.harness
                    && target.test_enabled
                    && logicals.contains(&selector::logical_harness(
                        &package.workspace_root,
                        &target.src_path,
                        &target.kind,
                    ))
            })
            .cloned()
            .collect(),
    )
}

async fn active_tests(
    execution: &crate::runtime::Execution,
    found: &[Found],
    harnesses: &[BuiltHarness],
    timeout: u64,
) -> Result<Vec<Found>> {
    let mut active = Vec::new();
    for harness in harnesses {
        for listed in builder::list_tests_with(execution, &harness.executable, timeout).await? {
            if listed.kind != "test" {
                continue;
            }
            let matches: Vec<_> = found
                .iter()
                .filter(|f| {
                    f.logical == harness.logical_name && f.discovered.fn_path == listed.name
                })
                .collect();
            if let [item] = matches.as_slice() {
                active.push((*item).clone());
                continue;
            }
            let reason = if matches.is_empty() {
                "generated test has no directly editable source function; add a plain #[test] wrapper for the workload"
            } else {
                "active test has multiple conditional source definitions; add a plain #[test] wrapper with a unique name"
            };
            active.push(Found {
                logical: harness.logical_name.clone(),
                discovered: DiscoveredTest {
                    name: listed.name.clone(),
                    fn_path: listed.name.clone(),
                    file_fn_path: listed.name,
                    file: harness.src_path.clone(),
                    harness: Some(harness.logical_name.clone()),
                    body: String::new(),
                    body_bytes: 0,
                    source: String::new(),
                    skip: Some(reason.into()),
                },
            });
        }
    }
    tracing::info!(tests = active.len(), "read active test inventory");
    Ok(active)
}

fn select_tests(
    found: &[Found],
    repo_config: &RepoConfig,
    max_scan: usize,
) -> Result<Vec<DiscoveredTest>> {
    if repo_config.tests.is_empty() {
        let tests: Vec<DiscoveredTest> = found.iter().map(|item| item.discovered.clone()).collect();
        return Ok(selector::order_for_scan(tests, max_scan));
    }
    let mut selected = Vec::new();
    for raw in &repo_config.tests {
        let id = selector::parse_test_id(raw);
        let matches: Vec<&Found> = found
            .iter()
            .filter(|item| name_matches(&item.discovered.fn_path, &id.name))
            .filter(|item| {
                id.harness
                    .as_ref()
                    .is_none_or(|harness| harness == &item.logical)
            })
            .collect();
        if matches.is_empty() {
            let names = found
                .iter()
                .take(10)
                .map(|f| format!("{}::{}", f.logical, f.discovered.fn_path))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("test {raw} is not active for the selected target and features; available tests include: {names}");
        }
        if matches.len() > 1 {
            let mut test = matches[0].discovered.clone();
            let choices = matches
                .iter()
                .map(|item| format!("{}::{}", item.logical, item.discovered.fn_path))
                .collect::<Vec<_>>()
                .join(", ");
            test.skip = Some(format!(
                "test {raw} is ambiguous; select one full test name: {choices}"
            ));
            selected.push(test);
            continue;
        }
        selected.push(matches[0].discovered.clone());
    }
    Ok(selected)
}

fn name_matches(fn_path: &str, wanted: &str) -> bool {
    fn_path == wanted || fn_path.ends_with(&format!("::{wanted}"))
}

fn short_name(fn_path: &str) -> String {
    fn_path.rsplit("::").next().unwrap_or(fn_path).to_string()
}

fn inject_selected(
    dest: &Path,
    manifest: &Path,
    fence_dir: &Path,
    tests: &[DiscoveredTest],
) -> Result<()> {
    let text = std::fs::read_to_string(manifest)?;
    std::fs::write(manifest, fence::add_fence_dependency(&text, fence_dir)?)?;
    let mut by_file: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for test in tests {
        if test.skip.is_some() {
            continue;
        }
        by_file
            .entry(test.file.clone())
            .or_default()
            .push(test.file_fn_path.clone());
    }
    for (file, mut names) in by_file {
        names.sort();
        names.dedup();
        let src = std::fs::read_to_string(&file)?;
        let next = fence::inject_guards(&src, &names)
            .with_context(|| format!("insert fences in {}", file.display()))?;
        std::fs::write(file, next)?;
    }
    let _ = dest;
    Ok(())
}

async fn link_harnesses(
    execution: &crate::runtime::Execution,
    dest: &Path,
    package: &ResolvedPackage,
    selected: &[DiscoveredTest],
    harnesses: &[BuiltHarness],
    timeout: u64,
) -> Result<Vec<LinkedTest>> {
    let mut listed = Vec::new();
    let mut source_paths = HashMap::new();
    for harness in harnesses {
        for test in selector::discover_harness(&harness.src_path) {
            source_paths.insert((harness.logical_name.clone(), test.fn_path), test.file);
        }
        let tests = builder::list_tests_with(execution, &harness.executable, timeout).await?;
        for test in tests {
            listed.push((harness.clone(), test));
        }
    }
    let mut linked = Vec::new();
    for test in selected {
        if test.skip.is_some() {
            let executable = harnesses
                .iter()
                .find(|harness| test.harness.as_deref() == Some(&harness.logical_name))
                .map(|harness| harness.executable.clone())
                .unwrap_or_else(|| dest.join("target").join("missing"));
            linked.push(linked_test(
                test.clone(),
                test.harness.clone().unwrap_or_else(|| "unknown".into()),
                executable,
                package.manifest_dir.clone(),
            ));
            continue;
        }
        let Some((harness, _)) = listed.iter().find(|(harness, item)| {
            item.name == test.fn_path
                && test
                    .harness
                    .as_ref()
                    .is_none_or(|name| name == &harness.logical_name)
                && source_paths
                    .get(&(harness.logical_name.clone(), test.fn_path.clone()))
                    .is_some_and(|file| same_file(file, &test.file))
        }) else {
            bail!("built harness did not list {}", test.fn_path);
        };
        linked.push(linked_test(
            test.clone(),
            harness.logical_name.clone(),
            harness.executable.clone(),
            package.manifest_dir.clone(),
        ));
    }
    Ok(linked)
}

fn linked_test(
    discovered: DiscoveredTest,
    logical: String,
    executable: PathBuf,
    working_directory: PathBuf,
) -> LinkedTest {
    let guest_args = vec![
        discovered.fn_path.clone(),
        "--exact".into(),
        "--test-threads=1".into(),
    ];
    LinkedTest {
        score_name: discovered.fn_path.clone(),
        guest_args,
        bench: None,
        discovered,
        logical,
        executable,
        working_directory,
    }
}

fn linked_bench(
    package: &ResolvedPackage,
    target: &CargoTarget,
    executable: PathBuf,
) -> LinkedTest {
    let src = std::fs::read_to_string(&target.src_path).unwrap_or_default();
    let source = selector::function_spans(&src)
        .into_iter()
        .find(|span| span.path == "main")
        .map(|span| span.source)
        .unwrap_or_else(|| src.clone());
    let logical = target
        .src_path
        .strip_prefix(&package.workspace_root)
        .unwrap_or(&target.src_path)
        .to_string_lossy()
        .replace('\\', "/");
    LinkedTest {
        discovered: DiscoveredTest {
            name: target.name.clone(),
            file: target.src_path.clone(),
            fn_path: "main".into(),
            file_fn_path: "main".into(),
            harness: Some(logical.clone()),
            body: source.clone(),
            body_bytes: source.len(),
            source,
            skip: None,
        },
        logical,
        executable,
        working_directory: package.manifest_dir.clone(),
        score_name: target.name.clone(),
        guest_args: Vec::new(),
        bench: Some(target.name.clone()),
    }
}

fn ensure_fence_manifest(manifest: &Path, fence_dir: &Path) -> Result<()> {
    let text = std::fs::read_to_string(manifest)?;
    std::fs::write(manifest, fence::add_fence_dependency(&text, fence_dir)?)?;
    Ok(())
}

fn inject_main(src_path: &Path) -> Result<()> {
    let src = std::fs::read_to_string(src_path)?;
    std::fs::write(src_path, fence::inject_guards(&src, &["main".into()])?)?;
    Ok(())
}

fn same_file(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
}

fn check_elf(dest: &Path, executable: &Path) -> Result<()> {
    let bytes = std::fs::read(executable).with_context(|| executable.display().to_string())?;
    if !profile::elf_is_static(&bytes)? {
        bail!(
            "{} is dynamically linked; a PT_INTERP harness is skipped",
            executable.display()
        );
    }
    if !profile::elf_has_repo_line(&bytes, dest)? {
        bail!("dwarf resolved no line in the worktree");
    }
    Ok(())
}

async fn restore_fences(dest: &Path, manifest: &Path, sources: &[PathBuf]) -> Result<()> {
    for source in sources {
        let text = std::fs::read_to_string(source).unwrap_or_default();
        std::fs::write(source, fence::strip_guard(&text))?;
    }
    let mut paths = Vec::new();
    if let Ok(rel) = manifest.strip_prefix(dest) {
        paths.push(rel.to_string_lossy().replace('\\', "/"));
    }
    paths.push("Cargo.lock".into());
    if let Some(dir) = manifest.parent() {
        if let Ok(rel) = dir.join("Cargo.lock").strip_prefix(dest) {
            paths.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    worktree::checkout_files(dest, &paths).await?;
    Ok(())
}

fn source_files(tests: &[DiscoveredTest]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = tests.iter().map(|test| test.file.clone()).collect();
    files.sort();
    files.dedup();
    files
}

fn locate_dump(elf: &[u8], measurement: &Measurement) -> Result<HashMap<u64, LineHit>> {
    let addrs: Vec<u64> = measurement
        .dump
        .blocks
        .iter()
        .map(|(pc, _)| pc.wrapping_sub(measurement.load_bias))
        .collect();
    let hits = profile::symbolize(elf, &addrs)?;
    let mut map = HashMap::new();
    for (addr, hit) in addrs.into_iter().zip(hits) {
        if let Some(hit) = hit {
            map.entry(addr).or_insert(hit);
        }
    }
    Ok(map)
}

async fn collect_spans(worktree: &Path) -> Result<HashMap<String, Vec<FnSpan>>> {
    let mut map = HashMap::new();
    let files = crate::process::run(
        worktree,
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--exclude=/target/",
            "--",
            "*.rs",
        ],
        &[],
        std::time::Duration::from_secs(30),
    )
    .await;
    if !files.success {
        bail!("cannot list Rust source files: {}", files.combined());
    }
    for name in files.stdout.split('\0').filter(|name| !name.is_empty()) {
        if name
            .split('/')
            .any(|part| part == "target" || part == ".git" || part == "..")
        {
            continue;
        }
        let text = std::fs::read_to_string(worktree.join(name))
            .with_context(|| format!("read Rust source {name}"))?;
        map.insert(name.replace('\\', "/"), selector::function_spans(&text));
    }
    Ok(map)
}

fn refreshed_hot(dest: &Path, hot: &HotFunction) -> HotFunction {
    let mut hot = hot.clone();
    let text = std::fs::read_to_string(dest.join(&hot.file)).unwrap_or_default();
    if let Some(span) = selector::function_spans(&text)
        .into_iter()
        .find(|span| span.path == hot.name)
    {
        hot.source = span.source;
        hot.line_start = span.line_start;
        hot.line_end = span.line_end;
    }
    hot
}

fn function_source(dest: &Path, test: &DiscoveredTest) -> String {
    let text = std::fs::read_to_string(&test.file)
        .unwrap_or_else(|_| std::fs::read_to_string(dest.join(&test.file)).unwrap_or_default());
    selector::function_spans(&text)
        .into_iter()
        .find(|span| span.path == test.file_fn_path)
        .map(|span| span.source)
        .unwrap_or_else(|| test.source.clone())
}

fn callee_sources(dest: &Path, hot: &HotFunction) -> Vec<(String, String)> {
    if !hot.is_test {
        return Vec::new();
    }
    let text = std::fs::read_to_string(dest.join(&hot.file)).unwrap_or_default();
    let names = selector::direct_callees(&text, &hot.name);
    let spans = selector::function_spans(&text);
    names
        .into_iter()
        .filter_map(|name| {
            spans
                .iter()
                .find(|span| span.path == name || span.path.ends_with(&format!("::{name}")))
                .map(|span| (name, span.source.clone()))
        })
        .collect()
}

fn rel_path(dest: &Path, file: &Path) -> Option<String> {
    file.strip_prefix(dest)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .or_else(|| {
            file.canonicalize().ok().and_then(|abs| {
                dest.canonicalize().ok().and_then(|root| {
                    abs.strip_prefix(root)
                        .ok()
                        .map(|path| path.to_string_lossy().replace('\\', "/"))
                })
            })
        })
}

fn guest_exe(dest: &Path, executable: &Path) -> String {
    let rel = executable.strip_prefix(dest).unwrap_or(executable);
    format!("/work/{}", rel.to_string_lossy().replace('\\', "/"))
}

fn function_remains(dest: &Path, file: &str, name: &str) -> bool {
    let text = std::fs::read_to_string(dest.join(file)).unwrap_or_default();
    selector::function_spans(&text)
        .iter()
        .any(|span| span.path == name)
}

fn scored_ok(measurement: &Measurement) -> bool {
    measurement.exit_code == 0 && measurement.dump.error.is_none() && measurement.total > 0
}

fn fail_reason(measurement: &Measurement) -> String {
    if let Some(error) = &measurement.dump.error {
        return error.clone();
    }
    if measurement.exit_code != 0 {
        return format!("guest exited {}", measurement.exit_code);
    }
    "fence did not fire".into()
}

fn ratio(baseline: u64, candidate: u64) -> f64 {
    if baseline == 0 {
        0.0
    } else {
        (baseline as f64 - candidate as f64) / baseline as f64
    }
}

fn spawn_cancel(
    cancel: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    ignore_window: bool,
    schedule: crate::config::ScheduleConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if stop.load(Ordering::SeqCst) || (!ignore_window && !schedule.is_in_window()) {
                cancel.store(true, Ordering::SeqCst);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
}

async fn pick_package(
    execution: &crate::runtime::Execution,
    dest: &Path,
    repo_config: &RepoConfig,
) -> Result<String> {
    let package =
        builder::resolve_package_with(execution, dest, repo_config.package.as_deref()).await?;
    Ok(package.name)
}

fn unique_scratch() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

#[cfg(test)]
mod repair_tests;

#[cfg(test)]
mod source_path_tests {
    use super::*;

    #[tokio::test]
    async fn source_spans_include_workspace_and_custom_target_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("members/parser/code")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(
            dir.path().join("members/parser/code/entry.rs"),
            "fn parse() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("target/generated.rs"),
            "fn generated() {}\n",
        )
        .unwrap();
        assert!(
            crate::process::run(
                dir.path(),
                "git",
                &["init", "-q"],
                &[],
                Duration::from_secs(10)
            )
            .await
            .success
        );
        let spans = collect_spans(dir.path()).await.unwrap();
        assert_eq!(spans["members/parser/code/entry.rs"][0].path, "parse");
        assert!(!spans.contains_key("target/generated.rs"));
    }
}
