//! CLI checks use temporary repositories and data directories.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use noctum::config::Config;
use noctum::db::Database;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    config_path: PathBuf,
    config: Config,
}

impl Fixture {
    fn new(manifest: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo [demo]'s files");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("Cargo.toml"), manifest).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
        let config = Config {
            data_dir: Some(directory.path().join("data")),
            ..Config::default()
        };
        let config_path = directory.path().join("config.toml");
        config.save(Some(&config_path)).unwrap();
        let fixture = Self {
            _directory: directory,
            root,
            config_path,
            config,
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Test"]);
        fixture.git(&["config", "user.email", "test@localhost"]);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "Initial source"]);
        fixture
    }

    fn single() -> Self {
        Self::new("[package]\nname = 'demo'\nversion = '0.1.0'\nedition = '2021'\n")
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_noctum"))
            .arg("--config")
            .arg(&self.config_path)
            .args(args)
            .current_dir(self.root.join("src"))
            .output()
            .unwrap()
    }

    async fn db(&self) -> Database {
        let db = Database::new(&self.config.database_path()).await.unwrap();
        db.run_migrations().await.unwrap();
        db
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[tokio::test]
async fn init_finds_the_root_and_can_run_twice() {
    let fixture = Fixture::single();
    for _ in 0..2 {
        let output = fixture.cli(&["init"]);
        assert!(output.status.success(), "{output:?}");
        assert!(stdout(&output).contains("Next: noctum --config"));
    }
    let config = noctum::repo_config::RepoConfig::load(&fixture.root).unwrap();
    assert_eq!(config.package.as_deref(), Some("demo"));
    let db = fixture.db().await;
    let repos = db.get_repositories().await.unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(
        PathBuf::from(&repos[0].path),
        fixture.root.canonicalize().unwrap()
    );
    assert!(repos[0].enabled);
    assert_eq!(fixture.git(&["rev-list", "--count", "HEAD"]), "1");
}

#[tokio::test]
async fn init_keeps_configuration_and_disabled_state() {
    let fixture = Fixture::single();
    let text = "# Keep these settings.\npackage = 'demo'\ntimeout_seconds = 123\n";
    fs::write(fixture.root.join("noctum.toml"), text).unwrap();
    assert!(fixture.cli(&["init"]).status.success());
    let db = fixture.db().await;
    let id = db.get_repositories().await.unwrap()[0].id;
    db.set_enabled(id, false).await.unwrap();
    let output = fixture.cli(&["init"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("disabled"));
    assert!(!db.get_repository(id).await.unwrap().unwrap().enabled);
    assert_eq!(
        fs::read_to_string(fixture.root.join("noctum.toml")).unwrap(),
        text
    );
    let output = fixture.cli(&["init", "--package", "other"]);
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(fixture.root.join("noctum.toml")).unwrap(),
        text
    );
}

#[test]
fn init_rejects_invalid_configuration_without_registration() {
    let fixture = Fixture::single();
    let text = "unknown_key = true\n";
    fs::write(fixture.root.join("noctum.toml"), text).unwrap();
    assert!(!fixture.cli(&["init"]).status.success());
    assert!(!fixture.config.database_path().exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("noctum.toml")).unwrap(),
        text
    );
}

#[test]
fn init_lists_workspace_packages_and_respects_exclusions() {
    let fixture = Fixture::new("[workspace]\nmembers = ['crates/*']\nexclude = ['crates/skip']\n");
    for name in ["first", "second", "skip"] {
        let dir = fixture.root.join("crates").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = '{name}'\nversion = '0.1.0'\n"),
        )
        .unwrap();
    }
    let output = fixture.cli(&["init"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("first, second"), "{error}");
    assert!(!fixture.root.join("noctum.toml").exists());
    assert!(!fixture.config.database_path().exists());
    assert!(!fixture.cli(&["init", "--package", "skip"]).status.success());
    assert!(fixture
        .cli(&["init", "--package", "second"])
        .status
        .success());
}

#[test]
fn init_counts_the_root_workspace_package_once() {
    let fixture =
        Fixture::new("[package]\nname = 'demo'\nversion = '0.1.0'\n[workspace]\nmembers = ['.']\n");
    let output = fixture.cli(&["init"]);
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn init_finds_implicit_and_inherited_workspace_members() {
    let fixture = Fixture::new("[workspace]\nmembers=['app']\nexclude=['excluded']\n[workspace.dependencies]\ncommon={path='common'}\n");
    for name in ["app", "common", "platform", "tools", "excluded"] {
        let directory = fixture.root.join(name);
        fs::create_dir_all(directory.join("src")).unwrap();
        fs::write(directory.join("src/lib.rs"), "pub fn value() {}\n").unwrap();
        let dependencies = match name {
            "app" => "[dependencies]\ncommon.workspace=true\nexcluded={path='../excluded'}\n[target.'cfg(unix)'.dev-dependencies]\nplatform={path='../platform'}\n",
            "common" => "[build-dependencies]\ntools={path='../tools'}\n",
            _ => "",
        };
        fs::write(
            directory.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='0.1.0'\n{dependencies}"),
        )
        .unwrap();
    }
    let output = fixture.cli(&["init"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("app, common, platform, tools"), "{error}");
    assert!(!fixture.root.join("Cargo.lock").exists());
    assert!(!fixture.config.database_path().exists());
    let output = fixture.cli(&["init", "--package", "tools"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!fixture.root.join("Cargo.lock").exists());
}

#[test]
fn init_uses_the_root_or_the_single_default_member() {
    for explicit_default in [false, true] {
        let workspace = if explicit_default {
            "[workspace]\ndefault-members=['helper']\n"
        } else {
            "[workspace]\n"
        };
        let fixture = Fixture::new(&format!("[package]\nname='demo'\nversion='0.1.0'\n[dependencies]\nhelper={{path='helper'}}\n{workspace}"));
        let helper = fixture.root.join("helper");
        fs::create_dir_all(helper.join("src")).unwrap();
        fs::write(helper.join("src/lib.rs"), "pub fn value() {}\n").unwrap();
        fs::write(
            helper.join("Cargo.toml"),
            "[package]\nname='helper'\nversion='0.1.0'\n",
        )
        .unwrap();
        let output = fixture.cli(&["init"]);
        assert!(output.status.success(), "{output:?}");
        let expected = if explicit_default { "helper" } else { "demo" };
        assert!(fs::read_to_string(fixture.root.join("noctum.toml"))
            .unwrap()
            .contains(expected));
        assert!(!fixture.root.join("Cargo.lock").exists());
    }
}

#[test]
fn review_with_no_history_does_not_create_a_database() {
    let fixture = Fixture::single();
    let output = fixture.cli(&["review"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("No run history"));
    assert!(!fixture.config.database_path().exists());
}

#[test]
fn doctor_reports_multiple_failures_without_setup_writes() {
    let fixture = Fixture::single();
    let output = fixture.cli(&["doctor"]);
    assert!(!output.status.success());
    let text = stdout(&output);
    assert!(text.contains("FAIL Repository configuration"), "{text}");
    assert!(text.contains("FAIL Repository registration"), "{text}");
    assert!(text.contains("FAIL Model and server"), "{text}");
    assert!(text.contains("Run noctum --config"), "{text}");
    assert!(text.contains(" model pull"), "{text}");
    assert!(text.contains("Target:"), "{text}");
    assert!(!fixture.config.data_dir().exists());
    assert!(!fixture.root.join("noctum.toml").exists());
}

#[test]
fn run_rejects_a_zero_request_limit_before_setup() {
    let fixture = Fixture::single();
    let output = fixture.cli(&["run", "--max-attempts", "0"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--max-attempts"));
    assert!(!fixture.config.data_dir().exists());
    let output = fixture.cli(&["run", "--all", "--test", "batch"]);
    assert!(!output.status.success());
    assert!(!fixture.config.data_dir().exists());
}

#[test]
fn run_uses_the_current_repository_and_the_shared_run_lock() {
    let fixture = Fixture::single();
    assert!(fixture.cli(&["init"]).status.success());
    let runtime = fixture.config.data_dir().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let lock = fs::File::create(runtime.join("pass.lock")).unwrap();
    lock.try_lock().unwrap();
    let output = fixture.cli(&["run", "--test", "batch", "--max-attempts", "1", "--retry"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("already running"), "{error}");
    assert!(error.contains("Log:"));
}

#[test]
fn measure_keeps_its_history_separate_and_uses_the_shared_run_lock() {
    let fixture = Fixture::single();
    fs::write(fixture.root.join("noctum.toml"), "package = 'demo'\n").unwrap();
    let runtime = fixture.config.data_dir().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let lock = fs::File::create(runtime.join("pass.lock")).unwrap();
    lock.try_lock().unwrap();
    let output = fixture.cli(&["measure", "--test", "batch"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("already running"), "{error}");
    assert!(!fixture.config.database_path().exists());
    let entry = fs::read_dir(fixture.config.data_dir().join("measurements"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(entry.join("results.db").exists());
    assert!(entry.join("report.txt").exists());
    let log = fs::read_dir(fixture.config.data_dir().join("logs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fs::read_to_string(log).unwrap().contains("Command failed"));
}

#[tokio::test]
async fn review_shows_committed_pending_and_missing_changes() {
    let fixture = Fixture::single();
    assert!(fixture.cli(&["init"]).status.success());
    let db = fixture.db().await;
    let repo = db.get_repositories().await.unwrap()[0].id;
    let base = fixture.git(&["rev-parse", "HEAD"]);
    let run = db
        .insert_run(repo, &base, Some("demo"), "ok", None)
        .await
        .unwrap();
    db.set_run_target(run, "aarch64-unknown-linux-musl")
        .await
        .unwrap();
    let candidate = db
        .insert_candidate(
            run,
            "batch",
            "unittests src/lib.rs",
            1,
            "value",
            "src/lib.rs",
            1,
            1,
            100_000,
            1.0,
            "blob",
        )
        .await
        .unwrap();
    let rejected = db
        .insert_trial(
            candidate,
            "rejected",
            Some("no improvement"),
            None,
            None,
            Some(100_000),
            Some(100_000),
            Some(0.0),
            true,
            true,
            None,
        )
        .await
        .unwrap();
    let accepted = db
        .insert_trial(
            candidate,
            "accepted",
            None,
            Some("Reduce work"),
            Some("saved diff"),
            Some(100_000),
            Some(50_000),
            Some(0.5),
            true,
            true,
            None,
        )
        .await
        .unwrap();
    fs::write(
        fixture.root.join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }\n",
    )
    .unwrap();
    fixture.git(&["commit", "-qam", "Reduce work"]);
    let sha = fixture.git(&["rev-parse", "HEAD"]);
    db.insert_commit(accepted, &sha).await.unwrap();
    let pending = db
        .insert_trial(
            candidate,
            "accepted",
            None,
            None,
            Some("pending diff"),
            None,
            None,
            None,
            true,
            true,
            None,
        )
        .await
        .unwrap();
    let missing = db
        .insert_trial(
            candidate,
            "accepted",
            None,
            None,
            Some("missing diff"),
            None,
            None,
            None,
            true,
            true,
            None,
        )
        .await
        .unwrap();
    let missing_sha = "a".repeat(40);
    db.insert_commit(missing, &missing_sha).await.unwrap();
    db.insert_baseline(
        run,
        "unittests src/lib.rs",
        "batch",
        "eligible",
        Some(100_000),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    db.insert_baseline(
        run,
        "unittests src/lib.rs",
        "clock",
        "skipped",
        None,
        None,
        None,
        None,
        Some("reads the clock"),
    )
    .await
    .unwrap();
    db.finish_run(run, "ok", None, Some(&sha)).await.unwrap();
    let before = fixture.git(&["status", "--porcelain"]);
    let output = fixture.cli(&["review"]);
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    for expected in [
        format!("Base commit: {base}"),
        "Target: aarch64-unknown-linux-musl".into(),
        "Reduction: 50.00%".into(),
        "Test suite check: passed".into(),
        "saved diff".into(),
        format!("cherry-pick {sha}"),
        "Commit is pending".into(),
        "Commit is not available".into(),
        "reads the clock".into(),
    ] {
        assert!(text.contains(&expected), "{expected}: {text}");
    }
    assert!(
        text.find(&format!("Trial {accepted}:")).unwrap()
            < text.find(&format!("Trial {rejected}:")).unwrap()
    );
    assert!(text.contains(&format!("Trial {pending}:")));
    assert!(!text.contains(&format!("cherry-pick {missing_sha}")));
    let command = text
        .lines()
        .find_map(|l| l.strip_prefix("To apply: "))
        .unwrap();
    let command = command.replace("cherry-pick", "show --format= --stat");
    let output = Command::new("sh").args(["-c", &command]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.git(&["status", "--porcelain"]), before);
    assert_eq!(fixture.git(&["rev-parse", "HEAD"]), sha);
}
