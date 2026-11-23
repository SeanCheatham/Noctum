use super::*;
use std::sync::atomic::AtomicUsize;

const ORIGINAL: &str =
    "pub fn hot() -> u32 { 1 }\n#[test]\nfn selected() { assert_eq!(hot(), 1); }\n";
const REPAIRED: &str =
    "pub fn hot() -> u32 { 1 + 0 }\n#[test]\nfn selected() { assert_eq!(hot(), 1); }\n";

struct NoMeasurement;
impl Measurer for NoMeasurement {
    async fn measure(&self, _: &MeasureRequest) -> Result<Measurement> {
        bail!("the repair check must not request a measurement")
    }
}

struct RepairModel;
impl Inferencer for RepairModel {
    async fn complete(
        &self,
        messages: &[crate::ports::ChatMessage],
        _: &AtomicBool,
    ) -> Result<Completion> {
        let message = &messages[1].content;
        let source = message.split("Test source:").next().unwrap();
        assert!(source.contains("pub fn hot() -> u32 { 1 }"), "{source}");
        assert!(!source.contains("pub fn hot() -> u32 { 2 }"), "{source}");
        assert!(message.contains("Use the original source above as the base"));
        assert!(message.contains("the test result is wrong"));
        Ok(Completion {
            summary: "Keep the test result.".into(),
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n-pub fn hot() -> u32 { 1 }\n+pub fn hot() -> u32 { 1 + 0 }\n #[test]\n fn selected() { assert_eq!(hot(), 1); }\n".into(),
        })
    }
}

struct PackageCheck(Arc<AtomicUsize>);
impl HostCheck for PackageCheck {
    async fn run(&self, root: &Path, _: &RepoConfig, _: &str) -> Result<CheckOutput> {
        let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
        let source = std::fs::read_to_string(root.join("src/lib.rs"))?;
        if first {
            assert!(source.contains("pub fn hot() -> u32 { 2 }"));
        } else {
            assert_eq!(source, REPAIRED);
        }
        Ok(CheckOutput {
            success: !first,
            timed_out: false,
            output: if first {
                "the test result is wrong"
            } else {
                "tests passed"
            }
            .into(),
        })
    }
}

#[tokio::test]
async fn repair_uses_the_original_source_as_its_base() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), ORIGINAL).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='repair-check'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "init",
        ],
    ] {
        let result = crate::process::run(root, "git", &args, &[], Duration::from_secs(10)).await;
        assert!(result.success, "{}", result.combined());
    }
    let base = worktree::head_sha(root).await.unwrap();
    std::fs::write(root.join("src/lib.rs"), ORIGINAL.replace("{ 1 }", "{ 2 }")).unwrap();
    let failed = crate::process::run(root, "git", &["diff"], &[], Duration::from_secs(10)).await;
    assert!(failed.success);
    let db = Database::new(&data.path().join("perf.db")).await.unwrap();
    db.run_migrations().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let config = Config {
        data_dir: Some(data.path().into()),
        ..Config::default()
    };
    let night = Night::new(
        db,
        config,
        NoMeasurement,
        RepairModel,
        PackageCheck(calls.clone()),
    )
    .with_execution(crate::runtime::Execution::Local);
    let original_hot = "pub fn hot() -> u32 { 1 }";
    let pick = Pick {
        hot: HotFunction {
            name: "hot".into(),
            file: "src/lib.rs".into(),
            line_start: 1,
            line_end: 1,
            exclusive: 20_000,
            share: 1.0,
            source: original_hot.into(),
            is_test: false,
        },
        test_name: "selected".into(),
        harness: "unittests src/lib.rs".into(),
        blob: String::new(),
        candidate_id: 0,
    };
    let test = ScoredTest {
        identity: LinkedTest {
            discovered: DiscoveredTest {
                name: "selected".into(),
                file: root.join("src/lib.rs"),
                fn_path: "selected".into(),
                file_fn_path: "selected".into(),
                harness: Some("unittests src/lib.rs".into()),
                body: "assert_eq!(hot(), 1);".into(),
                body_bytes: 21,
                source: "#[test]\nfn selected() { assert_eq!(hot(), 1); }".into(),
                skip: None,
            },
            logical: "unittests src/lib.rs".into(),
            executable: root.join("unused"),
            working_directory: root.into(),
            score_name: "selected".into(),
            guest_args: Vec::new(),
            bench: None,
        },
        baseline: Measurement {
            total: 20_000,
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
            wall_ms: 0,
            dump: profile::ProfileDump {
                total: 20_000,
                blocks: Vec::new(),
                error: None,
            },
            map_start: 0,
            map_end: u64::MAX,
            load_bias: 0,
            serial_log: String::new(),
        },
        eligible: true,
    };
    let opts = RunOptions::manual(Arc::new(AtomicBool::new(false)));
    let completion = Completion {
        summary: "The first change fails.".into(),
        diff: failed.stdout,
    };
    let result = night
        .host_with_repair(
            root,
            &RepoConfig::default(),
            &test,
            &pick,
            &completion,
            &base,
            &opts,
        )
        .await
        .unwrap();
    assert!(matches!(result, HostRound::Ready(_, _)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        REPAIRED
    );
    assert_eq!(worktree::head_sha(root).await.unwrap(), base);
}
