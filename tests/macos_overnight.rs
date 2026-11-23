#![cfg(target_os = "macos")]
use noctum::config::{Config, ModelQuant};
use noctum::db::Database;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(path: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

#[test]
fn missing_host_helper_has_an_actionable_error() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    Config {
        data_dir: Some(temp.path().join("data")),
        ..Config::default()
    }
    .save(Some(&config))
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_noctum"))
        .args(["--config", config.to_str().unwrap(), "runtime", "prepare"])
        .env("NOCTUM_HOST_HELPER", temp.path().join("missing-helper"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Noctum host helper is missing"));
    assert!(!temp.path().join("data/runtime/kernel.tar.partial").exists());
}

#[tokio::test]
#[ignore = "requires the worker and cached Inkling weights; run with --release"]
async fn mac_cli_runs_a_full_overnight_pass() {
    let cache =
        PathBuf::from(std::env::var_os("NOCTUM_TEST_MODEL_DATA").expect("NOCTUM_TEST_MODEL_DATA"));
    let data = PathBuf::from(
        std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").expect("NOCTUM_TEST_RUNTIME_DATA"),
    );
    std::fs::create_dir_all(&data).unwrap();
    let data = data.canonicalize().unwrap();
    let temp = tempfile::Builder::new()
        .prefix("Noctum Mac test ")
        .tempdir()
        .unwrap();
    let repo = temp.path().join("repository");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"mac-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        include_str!("../qemu/fixtures/observed/src/lib.rs"),
    )
    .unwrap();
    std::fs::write(
        repo.join("noctum.toml"),
        "package = \"mac-fixture\"\ntests = [\"encode_batch\"]\n",
    )
    .unwrap();
    git(&repo, &["init"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "Add fixture"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let quant = ModelQuant::Q2;
    let weights = noctum::lockfile::gguf_path(&data, quant);
    std::fs::create_dir_all(weights.parent().unwrap()).unwrap();
    if !weights.exists() {
        std::fs::hard_link(noctum::lockfile::gguf_path(&cache, quant), &weights).unwrap();
    }
    let runtime = noctum::lockfile::model_dir(&data);
    std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    if !runtime.exists() {
        std::os::unix::fs::symlink(noctum::lockfile::model_dir(&cache), &runtime).unwrap();
    }
    let mut config = Config {
        data_dir: Some(data.clone()),
        ..Config::default()
    };
    config.gate.max_candidates = 1;
    config.gate.max_tests = 1;
    config.guest.memory_mb = 512;
    config.schedule.start_hour = 0;
    config.schedule.end_hour = 0;
    let config_path = data.join("config.toml");
    config.save(Some(&config_path)).unwrap();
    let db = Database::new(&config.database_path()).await.unwrap();
    db.run_migrations().await.unwrap();
    for old in db.get_repositories().await.unwrap() {
        db.set_enabled(old.id, false).await.unwrap();
    }
    let id = db
        .add_repository(repo.to_str().unwrap(), "Mac fixture")
        .await
        .unwrap();
    let result = tokio::process::Command::new(env!("CARGO_BIN_EXE_noctum"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "run",
            "--repo",
            repo.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    std::fs::write(data.join("cli.stdout"), &result.stdout).unwrap();
    std::fs::write(data.join("cli.stderr"), &result.stderr).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let run = db.latest_run(id).await.unwrap().unwrap();
    assert_eq!(run.target, noctum::target::GuestTarget::native().triple());
    assert_eq!(run.status, "ok", "{:?}", run.error);
    let trials = db.trials_for_run(run.id).await.unwrap();
    assert_eq!(trials.len(), 1, "{trials:?}");
    assert_eq!(trials[0].status, "accepted", "{trials:?}");
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert!(git(&repo, &["status", "--porcelain"]).trim().is_empty());
    assert!(!git(&repo, &["show", "noctum/perf:Cargo.toml"]).contains("noctum-fence"));
    assert!(git(&repo, &["log", "-1", "--format=%B", "noctum/perf"])
        .contains(&format!("Target: {}", run.target)));
    let helpers = Command::new("pgrep")
        .args(["-f", "/noctum-host$"])
        .output()
        .unwrap();
    assert!(helpers.stdout.is_empty(), "worker helper did not stop");
    std::fs::write(
        data.join("result.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "run": run, "trials": trials,
            "user_head_preserved": true, "user_checkout_clean": true, "worker_removed": true,
        }))
        .unwrap(),
    )
    .unwrap();
    println!(
        "MAC_OVERNIGHT {} -> {} instructions",
        trials[0].baseline_count.unwrap(),
        trials[0].insn_count.unwrap()
    );
}
