//! One night with a real llama.cpp completion and the pinned guest.
//!
//! `cargo test` skips the guest run. `qemu/real-pass.sh` runs it. The production lockfile
//! stays on Qwen2.5-Coder-7B-Instruct Q5_K_M. `NOCTUM_GGUF` is the file this
//! process actually loads. The fixture leaves `tests` empty. The night measures
//! the discovered test, reads the plugin's guest-PC ranking, and asks the model
//! about whichever in-repo function that ranking puts first. A profile that
//! does not map back to source fails the run. A measured rejection fails it too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::builder::CargoHost;
use crate::lockfile::{ModelLock, QemuLock};
use crate::model::{self, LlamaEndpoint};
use crate::orchestrator::{Night, RunOptions};
use crate::ports::HostCheck;
use crate::qemu::QemuRunner;

fn required(name: &str) -> PathBuf {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let path = PathBuf::from(&value);
    assert!(path.exists(), "{name} does not exist: {value}");
    path
}

#[tokio::test]
#[ignore = "runs llama.cpp and the pinned QEMU guest; see qemu/real-pass.sh"]
async fn real_model_proposes_a_diff_and_the_gate_decides() {
    let stage = required("NOCTUM_QEMU_STAGE");
    let server = required("NOCTUM_LLAMA_SERVER");
    let gguf = required("NOCTUM_GGUF");
    let lock = ModelLock::load().unwrap();
    let available = model::mem_available().unwrap();
    let production_need = lock.gguf_bytes.saturating_add(1024 * 1024 * 1024);
    println!(
        "REAL_PASS production_pin={} production_need_bytes={production_need} available_bytes={available} loaded={}",
        lock.gguf_filename,
        gguf.display()
    );
    assert_eq!(
        lock.gguf_filename, "qwen2.5-coder-7b-instruct-q5_k_m.gguf",
        "production pin moved"
    );
    if gguf.file_name().and_then(|name| name.to_str()) != Some(lock.gguf_filename.as_str()) {
        assert!(
            available < production_need,
            "a smaller model was loaded while the pinned 7B file would fit"
        );
    }

    // The ranking comes from the guest dump. Print the PCs and the mapped
    // functions. A completion that git cannot apply never reaches QEMU. The
    // model is sampled up to three times; one measured candidate is enough.
    // A rejected bound-lowering is not one of those misses: it ends the night.
    std::env::set_var("NOCTUM_PROFILE_TRACE", "1");
    let mut missed = String::new();
    for attempt in 1..=3 {
        if let Err(reason) = one_night(&stage, &server, &gguf, attempt).await {
            println!("REAL_PASS attempt {attempt} was not kept: {reason}");
            match another_sample(&reason) {
                Sample::Again => {
                    missed = reason;
                    continue;
                }
                Sample::Stop(message) => panic!("{message}\n{reason}"),
            }
        }
        return;
    }
    panic!("three completions and the guest never measured a candidate\n{missed}");
}

enum Sample {
    Again,
    Stop(&'static str),
}

/// Whether the real-pass harness may draw another completion for this candidate.
///
/// A diff that will not apply can be sampled again. A host-check failure already
/// spent the one repair inside the night. Another night is still allowed when
/// that failure is a compile error or a rewrite that still walks the input.
/// A lowered fixed `for` bound and a dropped or rewritten assertion are rejected
/// before that repair. A diff that deletes the nested scan the assertion
/// observes fails the host check. This harness does not take another sample
/// after any of those.
fn another_sample(reason: &str) -> Sample {
    if reason.starts_with("PROFILE_UNMAPPED") {
        return Sample::Stop("profile could not be mapped back to source");
    }
    if reason.starts_with("GATE ") {
        return Sample::Stop("measured trial was not kept");
    }
    if reason.contains("lowers a fixed loop bound") {
        return Sample::Stop("a lowered loop bound ended the night");
    }
    if reason.contains("removes an assertion") || reason.contains("changes an assertion") {
        return Sample::Stop("an assertion edit ended the night");
    }
    if deletes_asserted_work(reason) {
        return Sample::Stop("a diff that deletes asserted work ended the night");
    }
    Sample::Again
}

/// A host-check failure whose diff removes the nested pair scan and does not
/// replace it with another walk of the input. The count the test asserts is
/// gone, so another completion would only be a second try at the same cheat.
fn deletes_asserted_work(reason: &str) -> bool {
    if !reason.contains("host check failed") {
        return false;
    }
    let Some(diff) = reason.split_once("diff:\n").map(|(_, diff)| diff) else {
        return false;
    };
    let removed_scan = diff.lines().any(|line| {
        let Some(body) = line.strip_prefix('-') else {
            return false;
        };
        if body.starts_with('-') {
            return false;
        }
        body.contains("while i < data.len()")
            || body.contains("while j < data.len()")
            || (body.contains("data[i]") && body.contains("data[j]"))
    });
    if !removed_scan {
        return false;
    }
    let replacement_walks_input = diff.lines().any(|line| {
        let Some(body) = line.strip_prefix('+') else {
            return false;
        };
        if body.starts_with('+') {
            return false;
        }
        let reads = body.contains("data")
            || body.contains("byte")
            || body.contains("freq")
            || body.contains("input");
        let walks = body.contains("for ")
            || body.contains("while ")
            || body.contains(".iter(")
            || body.contains("fold");
        reads && walks
    });
    !replacement_walks_input
}

#[test]
fn a_lowered_for_bound_ends_sampling_and_a_host_failure_does_not() {
    let host_fail = "attempt 1 status=rejected reason=Some(\"host check failed\") summary=Some(\"The diff was updated to remove the unnecessary black_box call\")";
    assert!(
        matches!(another_sample(host_fail), Sample::Again),
        "a host-check failure still has its one repair"
    );
    let lowered = "attempt 2 status=rejected reason=Some(\"diff lowers a fixed loop bound in the measured test\") summary=Some(\"Reduced the number of guest instructions\")\ndiff:\n-        for _ in 0..12 {\n+        for _ in 0..1 {";
    assert!(
        matches!(another_sample(lowered), Sample::Stop(_)),
        "a lowered for-bound must not be sampled again"
    );
    let unapplied = "attempt 2 status=rejected reason=Some(\"hunk does not match at line 25\")";
    assert!(matches!(another_sample(unapplied), Sample::Again));
    let dropped = "attempt 1 status=rejected reason=Some(\"diff removes an assertion from the measured test\")\ndiff:\n-    assert_eq!(total, 6400);";
    assert!(
        matches!(another_sample(dropped), Sample::Stop(_)),
        "dropping an assertion must not be sampled again"
    );
    let rewritten =
        "attempt 1 status=rejected reason=Some(\"diff changes an assertion in the measured test\")";
    assert!(matches!(another_sample(rewritten), Sample::Stop(_)));
    let deleted = "attempt 1 status=rejected reason=Some(\"host check failed\") summary=Some(\"Return zero\")\ndiff:\n-        while i < data.len() {\n-            if data[i] == data[j] {\n+pub fn equal_pairs(data: &[u8]) -> u64 {\n+    let _ = data;\n+    0\n+}";
    assert!(
        matches!(another_sample(deleted), Sample::Stop(_)),
        "deleting the asserted scan must not be sampled again"
    );
    let linear_bug = "attempt 1 status=rejected reason=Some(\"host check failed\") summary=Some(\"Count with a frequency table\")\ndiff:\n-        while j < data.len() {\n+    for &byte in data {\n+        freq[byte as usize] = freq[byte as usize].wrapping_add(1);\n+    }";
    assert!(
        matches!(another_sample(linear_bug), Sample::Again),
        "a rewrite that still walks the input may be sampled again"
    );
}

async fn one_night(stage: &Path, server: &Path, gguf: &Path, attempt: u32) -> Result<(), String> {
    let repo = init_fixture();
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let data = tempfile::tempdir().unwrap();
    install_stage(
        stage,
        &data.path().join("qemu").join(QemuLock::load().unwrap().id),
    );
    let config = crate::config::Config {
        data_dir: Some(data.path().to_path_buf()),
        gate: crate::config::GateConfig {
            max_candidates: 1,
            max_tests: 1,
            ..crate::config::GateConfig::default()
        },
        guest: crate::config::GuestConfig { memory_mb: 512 },
        ..crate::config::Config::default()
    };
    let db = crate::db::Database::new(&data.path().join("perf.db"))
        .await
        .unwrap();
    db.run_migrations().await.unwrap();
    let id = db
        .add_repository(&repo.path().display().to_string(), "fixture")
        .await
        .unwrap();
    let night = Night::new(
        db.clone(),
        config,
        QemuRunner::new(data.path(), 512),
        LlamaEndpoint {
            server: server.to_path_buf(),
            gguf: gguf.to_path_buf(),
        },
        CargoHost,
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
    let baselines = db.baselines_for_run(run.id).await.unwrap();
    let candidates = db.candidates_for_run(run.id).await.unwrap();
    let trials = db.trials_for_run(run.id).await.unwrap();
    if run.status != "ok" {
        let err = run.error.clone().unwrap_or_default();
        if err.contains("did not map guest PCs") {
            return Err(format!("PROFILE_UNMAPPED {err} baselines={baselines:?}"));
        }
        panic!("run {}: {err} baselines={baselines:?}", run.status);
    }
    if candidates.is_empty() {
        return Err(format!(
            "PROFILE_UNMAPPED profile ranked no in-repo function baselines={baselines:?}"
        ));
    }
    let top = &candidates[0];
    println!(
        "PROFILE_RANK symbol={} file={}:{}-{} exclusive={} share={:.4} test={}",
        top.symbol,
        top.file,
        top.line_start,
        top.line_end,
        top.exclusive_count,
        top.share,
        top.test_name
    );
    assert_eq!(top.rank, 0, "the stored candidate is not the first rank");
    assert!(
        top.exclusive_count > 0,
        "ranked function has no instructions"
    );
    assert!(
        top.file.starts_with("src/"),
        "ranked file is outside the repo: {}",
        top.file
    );
    let toml = std::fs::read_to_string(repo.path().join("noctum.toml")).unwrap();
    assert!(
        !toml.contains("tests ="),
        "the fixture named a test: {toml}"
    );
    let measured = baselines
        .iter()
        .find(|row| row.status == "measured")
        .unwrap_or_else(|| panic!("no measured test: {baselines:?}"));
    assert!(
        measured.insn_count.unwrap_or(0) >= 10_000,
        "baseline {}",
        measured.insn_count.unwrap_or(0)
    );
    assert_eq!(trials.len(), 1, "model was not asked: {trials:?}");
    let trial = &trials[0];
    assert_eq!(
        trial.symbol, top.symbol,
        "the model was not asked about the function the profile ranked first"
    );
    assert_eq!(trial.file, top.file);
    let diff = trial.unified_diff.clone().unwrap_or_default();
    assert!(
        diff.contains("diff --git") || diff.contains("+++"),
        "model did not return a diff: {diff}"
    );
    let before = trial
        .baseline_count
        .ok_or_else(|| "trial has no baseline count".to_string())?;
    let Some(after) = trial.insn_count else {
        return Err(format!(
            "attempt {attempt} status={} reason={:?} summary={:?}\ndiff:\n{diff}",
            trial.status, trial.reason, trial.model_summary
        ));
    };
    println!(
        "REAL_PASS attempt={attempt} before={before} after={after} status={} reason={:?} summary={:?} sha256={}",
        trial.status,
        trial.reason,
        trial.model_summary,
        crate::hashutil::sha256_file(gguf).unwrap_or_default()
    );
    println!("REAL_PASS diff:\n{diff}");
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
    assert!(
        git(repo.path(), &["status", "--porcelain"])
            .trim()
            .is_empty(),
        "the original checkout was modified"
    );
    match trial.status.as_str() {
        "accepted" => {
            let delta = before.saturating_sub(after);
            assert!(delta >= 10_000, "accepted a {delta} instruction drop");
            assert!(
                (delta as f64) / (before as f64) >= 0.01,
                "accepted below the ratio"
            );
            let committed = git(repo.path(), &["show", "noctum/perf:src/lib.rs"]);
            assert!(!committed.contains("_noctum_guard"), "{committed}");
            let manifest = git(repo.path(), &["show", "noctum/perf:Cargo.toml"]);
            assert!(!manifest.contains("noctum-fence"), "{manifest}");
            let show = git(repo.path(), &["show", "--format=full", "noctum/perf"]);
            println!("REAL_PASS kept commit:\n{show}");
        }
        "rejected" => {
            let perf = Command::new("git")
                .args(["rev-parse", "--verify", "--quiet", "refs/heads/noctum/perf"])
                .current_dir(repo.path())
                .status()
                .unwrap();
            assert!(!perf.success(), "a rejected diff was committed");
            return Err(format!(
                "GATE before={before} after={after} reason={:?} summary={:?}",
                trial.reason, trial.model_summary
            ));
        }
        other => panic!("unexpected trial status {other}: {trial:?}"),
    }
    Ok(())
}

const OBSERVED: &str = include_str!("../qemu/fixtures/observed/src/lib.rs");

fn write_sources(path: &Path) {
    // `equal_pairs` returns the count `encode_batch` asserts. The `for` bound
    // in that test is a literal the diff checker refuses to lower. This file
    // does not tell the night which function to edit.
    std::fs::write(path.join("src/lib.rs"), OBSERVED).unwrap();
}

fn replace_equal_pairs(src: &str, replacement: &str) -> String {
    let start = src
        .find("#[inline(never)]\npub fn equal_pairs")
        .expect("equal_pairs");
    let end = src[start..]
        .find("\n#[cfg(test)]\nfn pattern")
        .map(|offset| start + offset)
        .expect("pattern");
    format!(
        "{}{}\n{}",
        &src[..start],
        replacement.trim_end(),
        &src[end..]
    )
}

const DELETED_SCAN: &str = r#"#[inline(never)]
pub fn equal_pairs(data: &[u8]) -> u64 {
    let _ = data;
    0
}"#;

const LINEAR_SCAN: &str = r#"#[inline(never)]
pub fn equal_pairs(data: &[u8]) -> u64 {
    let mut freq = [0u64; 256];
    for &byte in data {
        freq[byte as usize] = freq[byte as usize].wrapping_add(1);
    }
    let mut count = 0u64;
    for times in freq {
        count = count.wrapping_add(times.wrapping_mul(times));
    }
    count
}"#;

#[test]
fn the_fixture_rejects_a_bound_shrink_a_dropped_assert_and_a_rewritten_total() {
    assert!(
        !OBSERVED.contains("freq"),
        "the measured source must not already be the linear count"
    );
    let ordered = crate::selector::order_for_scan(crate::selector::classify_source(OBSERVED), 20);
    let kept: Vec<_> = ordered
        .iter()
        .filter(|test| test.skip.is_none())
        .map(|test| test.name.as_str())
        .collect();
    assert_eq!(kept, vec!["encode_batch", "tiny_assert"], "{kept:?}");

    let repo = init_fixture();
    let files = files_of(repo.path());
    let lowered = OBSERVED.replace("0..32u8", "0..1u8");
    let bound = diff_to(repo.path(), &lowered);
    let err = crate::diffcheck::check_diff(&bound, &files, Some("src/lib.rs"), "encode_batch", &[])
        .unwrap_err();
    assert!(err.contains("loop bound"), "{err}");

    let dropped = OBSERVED.replace("assert_eq!(total, 6400);", "let _ = total;");
    let err = crate::diffcheck::check_diff(
        &diff_to(repo.path(), &dropped),
        &files,
        Some("src/lib.rs"),
        "encode_batch",
        &[],
    )
    .unwrap_err();
    assert!(err.contains("removes an assertion"), "{err}");

    let rewritten = OBSERVED.replace("assert_eq!(total, 6400);", "assert_eq!(total, 1);");
    let err = crate::diffcheck::check_diff(
        &diff_to(repo.path(), &rewritten),
        &files,
        Some("src/lib.rs"),
        "encode_batch",
        &[],
    )
    .unwrap_err();
    assert!(err.contains("changes an assertion"), "{err}");

    let deleted = replace_equal_pairs(OBSERVED, DELETED_SCAN);
    crate::diffcheck::check_diff(
        &diff_to(repo.path(), &deleted),
        &files,
        Some("src/lib.rs"),
        "encode_batch",
        &[],
    )
    .expect("deleting the scan is not an assert or bound edit");
    let linear = replace_equal_pairs(OBSERVED, LINEAR_SCAN);
    crate::diffcheck::check_diff(
        &diff_to(repo.path(), &linear),
        &files,
        Some("src/lib.rs"),
        "encode_batch",
        &[],
    )
    .expect("a linear count keeps the asserted total");
}

#[tokio::test]
async fn deleting_the_scan_fails_the_host_check_and_the_linear_count_passes() {
    let repo = init_fixture();
    let host = CargoHost;
    let config = crate::repo_config::RepoConfig {
        package: Some("fixture".into()),
        timeout_seconds: 180,
        ..crate::repo_config::RepoConfig::default()
    };
    let ok = host.run(repo.path(), &config, "fixture").await.unwrap();
    assert!(ok.success, "baseline host check failed:\n{}", ok.output);

    let lib = repo.path().join("src/lib.rs");
    std::fs::write(&lib, replace_equal_pairs(OBSERVED, DELETED_SCAN)).unwrap();
    let deleted = host.run(repo.path(), &config, "fixture").await.unwrap();
    assert!(
        !deleted.success,
        "deleting the scan still passed the host check:\n{}",
        deleted.output
    );
    assert!(
        deleted.output.contains("FAILED") || deleted.output.contains("assertion"),
        "deleted scan did not fail an assertion:\n{}",
        deleted.output
    );

    std::fs::write(&lib, replace_equal_pairs(OBSERVED, LINEAR_SCAN)).unwrap();
    let linear = host.run(repo.path(), &config, "fixture").await.unwrap();
    assert!(
        linear.success,
        "linear count failed the host check:\n{}",
        linear.output
    );
}

fn files_of(path: &Path) -> std::collections::HashMap<String, String> {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "src/lib.rs".to_string(),
        std::fs::read_to_string(path.join("src/lib.rs")).unwrap(),
    );
    files
}

fn diff_to(dir: &Path, next: &str) -> String {
    std::fs::write(dir.join("src/lib.rs"), next).unwrap();
    let diff = git(dir, &["diff", "--", "src/lib.rs"]);
    git(dir, &["checkout", "--", "src/lib.rs"]);
    diff
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
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
        "package = \"fixture\"\ntimeout_seconds = 300\n",
    )
    .unwrap();
    write_sources(path);
    git(path, &["init"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    let lock = Command::new("cargo")
        .args(["generate-lockfile"])
        .current_dir(path)
        .status()
        .unwrap();
    assert!(lock.success(), "cargo generate-lockfile");
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "init"]);
    dir
}

fn install_stage(stage: &Path, bundle: &Path) {
    std::fs::create_dir_all(bundle).unwrap();
    for name in [
        "qemu-system-x86_64",
        "qemu-img",
        "noctum_profile.so",
        "vmlinuz",
        "initramfs.cpio.gz",
    ] {
        let dest = bundle.join(name);
        std::fs::copy(stage.join(name), &dest).unwrap_or_else(|err| {
            panic!("copy {} from {}: {err}", name, stage.display());
        });
        if name.ends_with(".so") || !name.contains('.') {
            let mut perms = std::fs::metadata(&dest).unwrap().permissions();
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(perms.mode() | 0o755);
            std::fs::set_permissions(&dest, perms).unwrap();
        }
    }
    copy_tree(&stage.join("pc-bios"), &bundle.join("pc-bios"));
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
