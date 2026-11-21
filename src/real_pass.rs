//! One night with a real llama.cpp completion and the pinned guest.
//!
//! `cargo test` skips it. `qemu/real-pass.sh` runs it. The production lockfile
//! stays on Qwen2.5-Coder-7B-Instruct Q5_K_M. `NOCTUM_GGUF` is the file this
//! process actually loads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::builder::CargoHost;
use crate::lockfile::{ModelLock, QemuLock};
use crate::model::{self, LlamaEndpoint};
use crate::orchestrator::{Night, RunOptions};
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

    // A completion that git cannot apply never reaches QEMU. The model is
    // sampled up to three times; one measured candidate is enough.
    let mut missed = String::new();
    for attempt in 1..=3 {
        if let Err(reason) = one_night(&stage, &server, &gguf, attempt).await {
            println!("REAL_PASS attempt {attempt} was not measured again: {reason}");
            missed = reason;
            continue;
        }
        return;
    }
    panic!("three completions and the guest never measured a candidate\n{missed}");
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
    let trials = db.trials_for_run(run.id).await.unwrap();
    assert_eq!(run.status, "ok", "{:?} baselines={baselines:?}", run.error);
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
        }
        "rejected" => {
            let perf = Command::new("git")
                .args(["rev-parse", "--verify", "--quiet", "refs/heads/noctum/perf"])
                .current_dir(repo.path())
                .status()
                .unwrap();
            assert!(!perf.success(), "a rejected diff was committed");
        }
        other => panic!("unexpected trial status {other}: {trial:?}"),
    }
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]).trim(), head.trim());
    assert!(
        git(repo.path(), &["status", "--porcelain"])
            .trim()
            .is_empty(),
        "the original checkout was modified"
    );
    Ok(())
}

fn write_sources(path: &Path) {
    // Eight `black_box` calls do not change the sum. They live in `encode`,
    // which is what the profile ranks, and the batch loop bound is a literal
    // the diff checker refuses to lower. A raw string keeps the indentation.
    std::fs::write(
        path.join("src/lib.rs"),
        r#"#[inline(never)]
pub fn encode(input: &[u8]) -> u64 {
    let mut saved = 0u64;
    for &byte in input {
        saved = saved.wrapping_add(byte as u64);
    }
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    let _ = std::hint::black_box(saved);
    saved
}

#[test]
fn encode_batch() {
    let mut total = 0u64;
    for _ in 0..2_000 {
        total = total.wrapping_add(encode(b"abcd"));
    }
    assert_eq!(total, 788_000);
}

#[test]
fn tiny_assert() {
    assert_eq!(encode(b"a"), 97);
}
"#,
    )
    .unwrap();
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
        "package = \"fixture\"\ntests = [\"encode_batch\"]\ntimeout_seconds = 300\n",
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
