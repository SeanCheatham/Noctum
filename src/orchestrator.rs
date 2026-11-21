//! One repository, one night. QEMU and the model are called from here only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Local;

use crate::builder::{self, BuiltHarness, CargoHost, ResolvedPackage};
use crate::config::Config;
use crate::db::{Database, Repository};
use crate::diffcheck::{self, parse_diff};
use crate::gate::{self, GateDecision, GateInput};
use crate::hashutil::{self, sha256_bytes, sha256_file};
use crate::model::{self, PromptInput};
use crate::ports::{
    comparable_stdout, Completion, HostCheck, Inferencer, MeasureRequest, Measurement, Measurer,
};
use crate::profile::{self, HotFunction, LineHit};
use crate::repo_config::RepoConfig;
use crate::selector::{self, DiscoveredTest, FnSpan};
use crate::worktree::{self, PerfSync};
use crate::{fence, lockfile, qemu, LOG_LIMIT};

pub struct RunOptions {
    pub ignore_window: bool,
    pub stop: Arc<AtomicBool>,
    pub asset_error: Option<String>,
    pub only_repo: Option<PathBuf>,
}

impl RunOptions {
    pub fn manual(stop: Arc<AtomicBool>) -> Self {
        Self {
            ignore_window: true,
            stop,
            asset_error: None,
            only_repo: None,
        }
    }
}

pub struct Night<M, I, H> {
    pub db: Database,
    pub config: Config,
    pub measurer: M,
    pub inferencer: I,
    pub host: H,
}

pub async fn prepare_assets(data_dir: &Path) -> Result<()> {
    if lockfile::verify_model(data_dir).is_err() {
        model::pull(data_dir).await?;
        lockfile::verify_model(data_dir)?;
    }
    if lockfile::verify_qemu(data_dir).is_err() {
        qemu::fetch(data_dir).await?;
    }
    Ok(())
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
        }
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
                if Path::new(&repo.path) != only {
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
        let head = match worktree::head_sha(&user_repo).await {
            Ok(sha) => sha,
            Err(err) => {
                return self
                    .finish_early(repo, "", "skipped", &err.to_string(), None)
                    .await
            }
        };
        let repo_config = match RepoConfig::load(&user_repo) {
            Ok(config) => config,
            Err(err) => {
                return self
                    .finish_early(repo, &head, "skipped", &err.to_string(), None)
                    .await
            }
        };
        if !builder::musl_target_installed()? {
            return self
                .finish_early(
                    repo,
                    &head,
                    "skipped",
                    "musl target is not installed; run rustup target add x86_64-unknown-linux-musl",
                    None,
                )
                .await;
        }
        self.phase("building", &repo.name).await;
        let dest = self
            .config
            .data_dir()
            .join("worktrees")
            .join(repo.id.to_string());
        let sync = worktree::sync_perf(&user_repo, &head, &dest).await?;
        if let PerfSync::Conflict(detail) = sync {
            return self
                .finish_early(repo, &head, "skipped", &detail, None)
                .await;
        }
        let run_id = self
            .db
            .insert_run(
                repo.id,
                &head,
                repo_config.package.as_deref(),
                "ok",
                self.window_key(opts).as_deref(),
            )
            .await?;
        let result = self
            .after_sync(repo, opts, run_id, &user_repo, &dest, &repo_config)
            .await;
        if let Err(err) = &result {
            let _ = self
                .db
                .finish_run(run_id, "error", Some(&err.to_string()), None)
                .await;
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
        self.retry_pending(repo.id, dest, repo_config).await?;
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
        let eligible: Vec<&ScoredTest> = measured.iter().filter(|test| test.eligible).collect();
        if eligible.is_empty() {
            return self.close(run_id, dest, "ok", None).await;
        }
        if !self.proceed(opts) {
            return self.close(run_id, dest, "ok", None).await;
        }
        let (mut picks, mapped) = self.rank(repo.id, dest, repo_config, &measured).await?;
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
        let package = builder::resolve_package(dest, repo_config.package.as_deref()).await?;
        if builder::forces_abort(&package.workspace_root, &package.manifest)? {
            bail!("panic = \"abort\" is set; the fence would not close");
        }
        let found = gather_tests(&package, repo_config);
        let selected = select_tests(&found, repo_config, self.config.gate.max_scan as usize)?;
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        let fence_dir = fence::install_fence_crate(&self.config.data_dir())?;
        inject_selected(dest, &package.manifest, &fence_dir, &selected)?;
        let harnesses = builder::build_tests(dest, &package.name, repo_config).await?;
        let linked =
            link_harnesses(dest, &selected, &harnesses, repo_config.timeout_seconds).await?;
        for harness in &linked {
            check_elf(dest, &harness.executable)?;
        }
        restore_fences(dest, &package.manifest, &source_files(&selected)).await?;
        let bench_skips = bench_skips(repo_config, &package)?;
        Ok(BuildSet {
            tests: linked,
            bench_skips,
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
                    Some(reason),
                )
                .await?;
        }
        for test in &built.tests {
            if let Some(reason) = &test.discovered.skip {
                self.store_baseline(run_id, test, "skipped_env", None, None, Some(reason), None)
                    .await?;
                continue;
            }
            if eligible >= self.config.gate.max_tests || !self.proceed(opts) {
                continue;
            }
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
    ) -> Result<(Vec<Pick>, bool)> {
        let spans = collect_spans(dest);
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
                if self
                    .db
                    .trial_exists(
                        repository_id,
                        &test.identity.discovered.fn_path,
                        &hot.name,
                        &blob,
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
                            test_name: test.identity.discovered.fn_path.clone(),
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
            if !self.proceed(opts) {
                break;
            }
            let blob_now = worktree::file_blob(dest, &pick.hot.file).await?;
            if blob_now != pick.blob {
                continue;
            }
            let flow = self
                .one_trial(repo, opts, run_id, dest, repo_config, measured, pick)
                .await?;
            match flow {
                TrialFlow::Stop => break,
                TrialFlow::Accepted if index + 1 < picks.len() && self.proceed(opts) => {
                    self.rebaseline(dest, repo_config, measured).await?;
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
            .find(|test| test.identity.discovered.fn_path == pick.test_name)
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
        let hot = refreshed_hot(dest, &pick.hot);
        let test_source = function_source(dest, &test.identity.discovered);
        let callees = callee_sources(dest, &hot);
        let messages = model::build_messages(&PromptInput {
            test_name: &test.identity.discovered.fn_path,
            test_source: &test_source,
            hot: &hot,
            callee_sources: &callees,
            repair_diff,
            repair_output,
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let watcher = spawn_cancel(
            cancel.clone(),
            opts.stop.clone(),
            opts.ignore_window,
            self.config.schedule.clone(),
        );
        let result = self.inferencer.complete(&messages, &cancel).await;
        watcher.abort();
        match result {
            Ok(completion) => Ok(Proposal::Ready(completion)),
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
            &test.identity.discovered.fn_path,
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
        let first = self
            .host
            .run(dest, repo_config, &pick_package(dest, repo_config).await?)
            .await?;
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
        self.phase("inferring", "repair").await;
        let repair = match self
            .propose(dest, test, pick, Some(&completion.diff), Some(&tail), opts)
            .await?
        {
            Proposal::Ready(repair) => repair,
            Proposal::Stop => {
                self.reset(dest, base, repo_config).await?;
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
        let second = self
            .host
            .run(dest, repo_config, &pick_package(dest, repo_config).await?)
            .await?;
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

    async fn rebuild_one(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        test: &ScoredTest,
    ) -> Result<LinkedTest> {
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        let package = builder::resolve_package(dest, repo_config.package.as_deref()).await?;
        let fence_dir = fence::install_fence_crate(&self.config.data_dir())?;
        let selected = vec![test.identity.discovered.clone()];
        inject_selected(dest, &package.manifest, &fence_dir, &selected)?;
        let harnesses = builder::build_tests(dest, &package.name, repo_config).await?;
        let mut linked =
            link_harnesses(dest, &selected, &harnesses, repo_config.timeout_seconds).await?;
        restore_fences(dest, &package.manifest, &source_files(&selected)).await?;
        linked.pop().context("rebuild produced no harness")
    }

    async fn rebaseline(
        &self,
        dest: &Path,
        repo_config: &RepoConfig,
        measured: &mut [ScoredTest],
    ) -> Result<()> {
        if measured.is_empty() {
            return Ok(());
        }
        self.phase("measuring", "rebaseline").await;
        builder::clean_paths(dest, &repo_config.clean_paths)?;
        let package = builder::resolve_package(dest, repo_config.package.as_deref()).await?;
        let fence_dir = fence::install_fence_crate(&self.config.data_dir())?;
        let selected: Vec<DiscoveredTest> = measured
            .iter()
            .map(|test| test.identity.discovered.clone())
            .collect();
        inject_selected(dest, &package.manifest, &fence_dir, &selected)?;
        let harnesses = builder::build_tests(dest, &package.name, repo_config).await?;
        let linked =
            link_harnesses(dest, &selected, &harnesses, repo_config.timeout_seconds).await?;
        restore_fences(dest, &package.manifest, &source_files(&selected)).await?;
        for test in measured.iter_mut() {
            let Some(linked_test) = linked
                .iter()
                .find(|item| item.discovered.fn_path == test.identity.discovered.fn_path)
            else {
                continue;
            };
            test.identity.executable = linked_test.executable.clone();
            if let Pair::Match(measurement) =
                self.measure_pair(dest, repo_config, linked_test).await?
            {
                test.baseline = measurement;
            }
        }
        Ok(())
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
            "{summary}\n\nTest: {}\nBaseline: {}\nCandidate: {}\nRatio: {improvement:.6}\n",
            pick.test_name, test.baseline.total, measurement.total
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
                harness: test.executable.clone(),
                guest_exe: guest_exe(dest, &test.executable),
                args: vec![
                    test.discovered.fn_path.clone(),
                    "--exact".into(),
                    "--test-threads=1".into(),
                ],
                timeout_secs: repo_config.timeout_seconds,
                scratch: scratch.clone(),
                memory_mb: self.config.guest.memory_mb,
            })
            .await?;
        let _ = std::fs::remove_dir_all(&scratch);
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
            let base = worktree::head_sha(dest).await?;
            if worktree::apply(dest, &item.unified_diff, false)
                .await
                .is_err()
            {
                continue;
            }
            let message = format!(
                "{}\n\nTest: {}\nBaseline: {}\nCandidate: {}\nRatio: {:.6}\n",
                item.model_summary.unwrap_or_default(),
                item.test_name,
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
        self.db
            .insert_baseline(
                run_id,
                &test.logical,
                &test.discovered.fn_path,
                status,
                insn_count,
                stdout_sha256,
                binary.as_deref(),
                wall_ms,
                reason,
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
        self.db.repo_finished_window(repo.id, &key).await
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

fn bench_skips(
    repo_config: &RepoConfig,
    package: &ResolvedPackage,
) -> Result<Vec<(String, String)>> {
    let mut skips = Vec::new();
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
        }
    }
    Ok(skips)
}

#[derive(Clone)]
struct LinkedTest {
    discovered: DiscoveredTest,
    logical: String,
    executable: PathBuf,
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
        if !matches!(target.kind.as_str(), "lib" | "bin" | "test") {
            continue;
        }
        let logical =
            selector::logical_harness(&package.workspace_root, &target.src_path, &target.kind);
        for mut test in selector::discover_harness(&target.src_path) {
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
            bail!("test {raw} was not found");
        }
        if matches.len() > 1 && id.harness.is_none() {
            let mut test = matches[0].discovered.clone();
            test.skip = Some(format!("test {raw} is ambiguous; use tests/file.rs::{raw}"));
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
            .push(test.fn_path.clone());
    }
    for (file, names) in by_file {
        let src = std::fs::read_to_string(&file)?;
        let next = fence::inject_guards(&src, &names)?;
        std::fs::write(file, next)?;
    }
    let _ = dest;
    Ok(())
}

async fn link_harnesses(
    dest: &Path,
    selected: &[DiscoveredTest],
    harnesses: &[BuiltHarness],
    timeout: u64,
) -> Result<Vec<LinkedTest>> {
    let mut listed = Vec::new();
    for harness in harnesses {
        let tests = builder::list_tests(&harness.executable, timeout).await?;
        for test in tests {
            listed.push((harness.clone(), test));
        }
    }
    let mut linked = Vec::new();
    for test in selected {
        if test.skip.is_some() {
            let executable = harnesses
                .iter()
                .find(|harness| same_file(&harness.src_path, &test.file))
                .map(|harness| harness.executable.clone())
                .unwrap_or_else(|| dest.join("target").join("missing"));
            linked.push(LinkedTest {
                logical: listed
                    .iter()
                    .find(|(harness, _)| same_file(&harness.src_path, &test.file))
                    .map(|(harness, _)| harness.logical_name.clone())
                    .unwrap_or_else(|| "unknown".into()),
                executable,
                discovered: test.clone(),
            });
            continue;
        }
        let Some((harness, _)) = listed.iter().find(|(_, item)| item.name == test.fn_path) else {
            bail!("built harness did not list {}", test.fn_path);
        };
        linked.push(LinkedTest {
            logical: harness.logical_name.clone(),
            executable: harness.executable.clone(),
            discovered: test.clone(),
        });
    }
    Ok(linked)
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

fn collect_spans(worktree: &Path) -> HashMap<String, Vec<FnSpan>> {
    let mut map = HashMap::new();
    for root in ["src", "tests", "benches"] {
        walk_rs(&worktree.join(root), worktree, &mut map);
    }
    map
}

fn walk_rs(dir: &Path, worktree: &Path, map: &mut HashMap<String, Vec<FnSpan>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, worktree, map);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if let Ok(rel) = path.strip_prefix(worktree) {
                let key = rel.to_string_lossy().replace('\\', "/");
                map.insert(key, selector::function_spans(&text));
            }
        }
    }
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
        .find(|span| span.path == test.fn_path)
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

async fn pick_package(dest: &Path, repo_config: &RepoConfig) -> Result<String> {
    let package = builder::resolve_package(dest, repo_config.package.as_deref()).await?;
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

impl Default for CargoHost {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use object::{Object, ObjectSymbol};

    #[test]
    fn adaptive_benches_are_named() {
        assert_eq!(
            bench_reason(true, "fn bench() {}", false),
            Some("adaptive bench")
        );
        assert_eq!(
            bench_reason(false, "use criterion::Criterion;\nfn main() {}", false),
            Some("adaptive bench")
        );
        assert_eq!(bench_reason(false, "fn main() { loop {} }", false), None);
    }

    #[test]
    fn short_libtest_names_match_the_module_path() {
        assert!(name_matches("tests::encode_batch", "encode_batch"));
        assert!(name_matches("tests::encode_batch", "tests::encode_batch"));
        assert!(!name_matches("tests::encode_batch", "encode"));
    }

    fn source(body: &str) -> String {
        format!(
            r#"
#[inline(never)]
pub fn encode(input: &[u8]) -> Vec<u8> {{
    {body}
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn tiny_assert() {{
        assert_eq!(encode(b"a").len(), 1);
    }}

    #[test]
    fn encode_batch() {{
        let mut n = 0usize;
        for _ in 0..10_000 {{
            n += encode(b"abcd").len();
        }}
        assert_eq!(n, 40_000);
    }}

    #[test]
    fn opens_socket() {{
        let _ = std::net::TcpStream::connect("127.0.0.1:9");
    }}
}}
"#
        )
    }

    fn slow_source() -> String {
        source(
            "let mut out = Vec::with_capacity(input.len());\n    \
             for &byte in input { out.push(byte.wrapping_add(1)); }\n    \
             out",
        )
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn init_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        std::fs::create_dir(path.join("src")).unwrap();
        std::fs::write(path.join(".gitignore"), "/target\n").unwrap();
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            path.join("noctum.toml"),
            "package = \"fixture\"\ntimeout_seconds = 180\n",
        )
        .unwrap();
        std::fs::write(path.join("src/lib.rs"), slow_source()).unwrap();
        git(path, &["init"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Test"]);
        let lock = std::process::Command::new("cargo")
            .args(["generate-lockfile"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(lock.success(), "cargo generate-lockfile");
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "init"]);
        dir
    }

    fn diff_to(dir: &Path, next: &str) -> String {
        std::fs::write(dir.join("src/lib.rs"), next).unwrap();
        let diff = git(dir, &["diff", "--", "src/lib.rs"]);
        git(dir, &["checkout", "--", "src/lib.rs"]);
        diff
    }

    struct Scripted;

    impl Measurer for Scripted {
        async fn measure(
            &self,
            req: &crate::ports::MeasureRequest,
        ) -> Result<crate::ports::Measurement> {
            let source =
                std::fs::read_to_string(req.worktree.join("src/lib.rs")).unwrap_or_default();
            let total = if source.contains("NOCTUM_SLOW") {
                400_000
            } else if source.contains("to_vec") {
                80_000
            } else {
                200_000
            };
            let bytes = std::fs::read(&req.harness)?;
            let addr = encode_pc(&bytes);
            Ok(crate::ports::Measurement {
                total,
                stdout: b"test tests::encode_batch ... ok\n".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
                wall_ms: 4,
                dump: crate::profile::ProfileDump {
                    total,
                    blocks: vec![(addr, total * 9 / 10)],
                    error: None,
                },
                map_start: 0,
                map_end: u64::MAX,
                load_bias: 0,
                serial_log: String::new(),
            })
        }
    }

    fn encode_pc(bytes: &[u8]) -> u64 {
        let file = object::File::parse(bytes).unwrap();
        let mut fallback = 0u64;
        for symbol in file.symbols() {
            let Ok(name) = symbol.name() else { continue };
            if symbol.address() == 0 {
                continue;
            }
            let demangled = rustc_demangle::demangle(name).to_string();
            if demangled.ends_with("::encode") {
                return symbol.address();
            }
            if demangled.contains("encode") {
                fallback = symbol.address();
            }
        }
        fallback
    }

    struct Fixed {
        diff: String,
    }

    impl Inferencer for Fixed {
        async fn complete(
            &self,
            _messages: &[crate::ports::ChatMessage],
            _cancel: &AtomicBool,
        ) -> Result<crate::ports::Completion> {
            Ok(crate::ports::Completion {
                summary: "shorter encode".into(),
                diff: self.diff.clone(),
            })
        }
    }

    async fn run_fixture(
        diff: String,
    ) -> (tempfile::TempDir, tempfile::TempDir, Database, i64, String) {
        let repo = init_fixture();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let data = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            data_dir: Some(data.path().to_path_buf()),
            gate: crate::config::GateConfig {
                max_candidates: 1,
                max_tests: 1,
                ..crate::config::GateConfig::default()
            },
            ..crate::config::Config::default()
        };
        let db = Database::new(&data.path().join("perf.db")).await.unwrap();
        db.run_migrations().await.unwrap();
        let id = db
            .add_repository(&repo.path().display().to_string(), "fixture")
            .await
            .unwrap();
        let night = Night::new(
            db.clone(),
            config,
            Scripted,
            Fixed { diff },
            crate::builder::CargoHost,
        );
        let row = db.get_repository(id).await.unwrap().unwrap();
        night
            .run_repository(
                &row,
                &RunOptions {
                    ignore_window: true,
                    stop: Arc::new(AtomicBool::new(false)),
                    asset_error: None,
                    only_repo: None,
                },
            )
            .await
            .unwrap();
        (repo, data, db, id, head)
    }

    #[tokio::test]
    async fn canned_diff_commits_on_perf_and_leaves_the_clone() {
        let fast = source("input.to_vec()");
        let repo_for_diff = init_fixture();
        let diff = diff_to(repo_for_diff.path(), &fast);
        assert!(diff.contains("to_vec"), "{diff}");
        let (repo, _data, db, id, head) = run_fixture(diff).await;
        let run = db.latest_run(id).await.unwrap().unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.error);
        let baselines = db.baselines_for_run(run.id).await.unwrap();
        assert!(
            baselines
                .iter()
                .any(|row| row.test_name.contains("opens_socket") && row.status == "skipped_env"),
            "{baselines:?}"
        );
        let trials = db.trials_for_run(run.id).await.unwrap();
        assert_eq!(trials.len(), 1, "{trials:?}");
        assert_eq!(trials[0].status, "accepted", "{:?}", trials[0].reason);
        assert!(
            trials[0].symbol.contains("encode"),
            "{:?}",
            trials[0].symbol
        );
        let porcelain = git(repo.path(), &["status", "--porcelain"]);
        assert!(porcelain.trim().is_empty(), "{porcelain}");
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
        let committed = git(repo.path(), &["show", "noctum/perf:src/lib.rs"]);
        assert!(committed.contains("to_vec"), "{committed}");
        assert!(!committed.contains("_noctum_guard"), "{committed}");
        let manifest = git(repo.path(), &["show", "noctum/perf:Cargo.toml"]);
        assert!(!manifest.contains("noctum-fence"), "{manifest}");
    }

    #[tokio::test]
    async fn a_diff_that_drops_an_assertion_is_rejected_before_another_measurement() {
        let repo_for_diff = init_fixture();
        let broken = slow_source().replace("assert_eq!(n, 40_000);", "let _ = n;");
        let diff = diff_to(repo_for_diff.path(), &broken);
        let (repo, _data, db, id, head) = run_fixture(diff).await;
        let run = db.latest_run(id).await.unwrap().unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.error);
        let trials = db.trials_for_run(run.id).await.unwrap();
        assert_eq!(trials.len(), 1, "{trials:?}");
        assert_eq!(trials[0].status, "rejected");
        let reason = trials[0].reason.clone().unwrap_or_default();
        assert!(
            reason.contains("assert") || reason == "diff rejected",
            "{reason}"
        );
        let perf = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/noctum/perf"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(!perf.success(), "rejected diff was committed");
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
        assert!(git(repo.path(), &["status", "--porcelain"])
            .trim()
            .is_empty());
    }

    fn no_perf(repo: &Path) {
        let perf = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/noctum/perf"])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(!perf.success(), "a rejected diff was committed");
    }

    #[tokio::test]
    async fn a_diff_that_breaks_another_assertion_is_rejected_by_the_host_suite() {
        let repo_for_diff = init_fixture();
        let broken = source("let _ = input;\n    Vec::new()");
        let diff = diff_to(repo_for_diff.path(), &broken);
        let (repo, _data, db, id, head) = run_fixture(diff).await;
        let run = db.latest_run(id).await.unwrap().unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.error);
        let trials = db.trials_for_run(run.id).await.unwrap();
        assert_eq!(trials.len(), 1, "{trials:?}");
        assert_eq!(trials[0].status, "rejected");
        assert_eq!(trials[0].reason.as_deref(), Some("host check failed"));
        no_perf(repo.path());
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
    }

    struct Sequenced {
        diffs: Vec<String>,
        calls: Arc<AtomicUsize>,
    }

    impl Inferencer for Sequenced {
        async fn complete(
            &self,
            _messages: &[crate::ports::ChatMessage],
            _cancel: &AtomicBool,
        ) -> Result<crate::ports::Completion> {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let diff = self
                .diffs
                .get(index)
                .cloned()
                .unwrap_or_else(|| self.diffs.last().cloned().unwrap_or_default());
            Ok(crate::ports::Completion {
                summary: format!("sample {}", index + 1),
                diff,
            })
        }
    }

    /// The first sample fails the host suite, so the one allowed repair is drawn.
    /// That repair lowers a fixed `for` bound. The rejection ends the candidate:
    /// the third sample, which would clear the existing gate, is never requested.
    #[tokio::test]
    async fn a_lowered_for_bound_is_rejected_without_another_sample() {
        let host_repo = init_fixture();
        let host_fail = diff_to(host_repo.path(), &source("let _ = input;\n    Vec::new()"));
        let bound_repo = init_fixture();
        let lowered = slow_source().replace("0..10_000", "0..10");
        let bound = diff_to(bound_repo.path(), &lowered);
        assert!(
            bound
                .lines()
                .any(|line| line.starts_with('+') && line.contains("0..10 {")),
            "{bound}"
        );
        let keep_repo = init_fixture();
        let keep = diff_to(keep_repo.path(), &source("input.to_vec()"));
        assert!(keep.contains("to_vec"), "{keep}");

        let calls = Arc::new(AtomicUsize::new(0));
        let repo = init_fixture();
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        let data = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            data_dir: Some(data.path().to_path_buf()),
            gate: crate::config::GateConfig {
                max_candidates: 1,
                max_tests: 1,
                min_improvement_ratio: 0.01,
                min_insn_delta: 10_000,
                ..crate::config::GateConfig::default()
            },
            ..crate::config::Config::default()
        };
        assert_eq!(config.gate.min_improvement_ratio, 0.01);
        assert_eq!(config.gate.min_insn_delta, 10_000);
        let db = Database::new(&data.path().join("perf.db")).await.unwrap();
        db.run_migrations().await.unwrap();
        let id = db
            .add_repository(&repo.path().display().to_string(), "fixture")
            .await
            .unwrap();
        let night = Night::new(
            db.clone(),
            config,
            Scripted,
            Sequenced {
                diffs: vec![host_fail, bound, keep],
                calls: calls.clone(),
            },
            crate::builder::CargoHost,
        );
        let row = db.get_repository(id).await.unwrap().unwrap();
        night
            .run_repository(
                &row,
                &RunOptions {
                    ignore_window: true,
                    stop: Arc::new(AtomicBool::new(false)),
                    asset_error: None,
                    only_repo: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "host failure draws one repair; a lowered for-bound draws no further sample"
        );
        let run = db.latest_run(id).await.unwrap().unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.error);
        let trials = db.trials_for_run(run.id).await.unwrap();
        assert_eq!(trials.len(), 1, "{trials:?}");
        assert_eq!(trials[0].status, "rejected");
        let reason = trials[0].reason.clone().unwrap_or_default();
        assert!(reason.contains("lowers a fixed loop bound"), "{reason}");
        let diff = trials[0].unified_diff.clone().unwrap_or_default();
        assert!(
            diff.lines()
                .any(|line| line.starts_with('+') && line.contains("0..10 {")),
            "{diff}"
        );
        assert!(!diff.contains("to_vec"), "{diff}");
        assert!(trials[0].insn_count.is_none(), "{trials:?}");
        no_perf(repo.path());
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
        assert!(git(repo.path(), &["status", "--porcelain"])
            .trim()
            .is_empty());
    }

    #[tokio::test]
    async fn a_slower_diff_is_reset() {
        let repo_for_diff = init_fixture();
        let slower = source(
            "let _marker = \"NOCTUM_SLOW\";\n    \
             let mut out = Vec::with_capacity(input.len());\n    \
             for &byte in input { out.push(byte.wrapping_add(1)); }\n    \
             let _ = _marker;\n    \
             out",
        );
        let diff = diff_to(repo_for_diff.path(), &slower);
        let (repo, _data, db, id, head) = run_fixture(diff).await;
        let run = db.latest_run(id).await.unwrap().unwrap();
        assert_eq!(run.status, "ok", "{:?}", run.error);
        let trials = db.trials_for_run(run.id).await.unwrap();
        assert_eq!(trials.len(), 1, "{trials:?}");
        assert_eq!(trials[0].status, "rejected");
        assert_eq!(trials[0].reason.as_deref(), Some("slower"));
        no_perf(repo.path());
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
        assert!(git(repo.path(), &["status", "--porcelain"])
            .trim()
            .is_empty());
    }

    struct Unmapped;

    impl Measurer for Unmapped {
        async fn measure(
            &self,
            _req: &crate::ports::MeasureRequest,
        ) -> Result<crate::ports::Measurement> {
            Ok(crate::ports::Measurement {
                total: 50_000,
                stdout: b"test tests::encode_batch ... ok\n".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
                wall_ms: 4,
                dump: crate::profile::ProfileDump {
                    total: 50_000,
                    blocks: vec![(0x7fff_ffff_0000, 50_000)],
                    error: None,
                },
                map_start: 0,
                map_end: u64::MAX,
                load_bias: 0,
                serial_log: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn an_unmapped_profile_does_not_invent_a_function() {
        let repo = init_fixture();
        let data = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            data_dir: Some(data.path().to_path_buf()),
            gate: crate::config::GateConfig {
                max_candidates: 1,
                max_tests: 1,
                ..crate::config::GateConfig::default()
            },
            ..crate::config::Config::default()
        };
        let db = Database::new(&data.path().join("perf.db")).await.unwrap();
        db.run_migrations().await.unwrap();
        let id = db
            .add_repository(&repo.path().display().to_string(), "fixture")
            .await
            .unwrap();
        let night = Night::new(
            db.clone(),
            config,
            Unmapped,
            Fixed {
                diff: String::new(),
            },
            crate::builder::CargoHost,
        );
        let row = db.get_repository(id).await.unwrap().unwrap();
        night
            .run_repository(
                &row,
                &RunOptions {
                    ignore_window: true,
                    stop: Arc::new(AtomicBool::new(false)),
                    asset_error: None,
                    only_repo: None,
                },
            )
            .await
            .unwrap();
        let run = db.latest_run(id).await.unwrap().unwrap();
        assert_eq!(run.status, "error", "{:?}", run.error);
        assert_eq!(
            run.error.as_deref(),
            Some("profile did not map guest PCs back to repository source")
        );
        assert!(db.trials_for_run(run.id).await.unwrap().is_empty());
        assert!(db.candidates_for_run(run.id).await.unwrap().is_empty());
        no_perf(repo.path());
    }
}
