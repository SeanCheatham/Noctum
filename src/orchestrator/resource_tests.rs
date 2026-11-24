use super::*;
use crate::optimization::{Goal, MemoryStats, Selection};

struct NoModel;
impl Inferencer for NoModel {
    async fn complete(
        &self,
        _: &[crate::ports::ChatMessage],
        _: &AtomicBool,
    ) -> Result<Completion> {
        bail!("no model request is expected")
    }
}
struct NoMeasure;
impl Measurer for NoMeasure {
    async fn measure(&self, _: &MeasureRequest) -> Result<Measurement> {
        bail!("no measurement request is expected")
    }
}
struct NoHost;
impl HostCheck for NoHost {
    async fn run(&self, _: &Path, _: &RepoConfig, _: &str) -> Result<CheckOutput> {
        bail!("no package check is expected")
    }
}

#[tokio::test]
async fn memory_acceptance_saves_evidence_and_rejection_restores_source() {
    const ORIGINAL: &str = "pub fn hot(xs: &[u8]) -> Vec<u8> { let temporary = xs.to_vec(); temporary.clone() }\n#[test]\nfn selected() { assert_eq!(hot(&[1, 2]), vec![1, 2]); }\n";
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), ORIGINAL).unwrap();
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
            "Add test",
        ],
    ] {
        assert!(
            crate::process::run(root.path(), "git", &args, &[], Duration::from_secs(10))
                .await
                .success
        );
    }
    let base = worktree::head_sha(root.path()).await.unwrap();
    let db = Database::new(&data.path().join("perf.db")).await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = db
        .add_repository(root.path().to_str().unwrap(), "fixture")
        .await
        .unwrap();
    let run = db.insert_run(repo, &base, None, "ok", None).await.unwrap();
    let mut config = Config {
        data_dir: Some(data.path().into()),
        ..Default::default()
    };
    config.optimization.goal = Goal::Memory;
    config.optimization.selection = Selection::Sweep;
    let night = Night::new(db.clone(), config, NoMeasure, NoModel, NoHost)
        .with_execution(crate::runtime::Execution::Local);
    let hot = selector::function_spans(ORIGINAL).remove(0);
    let mut pick = Pick {
        hot: HotFunction {
            name: hot.path,
            file: "src/lib.rs".into(),
            line_start: 1,
            line_end: 1,
            exclusive: 1000,
            share: 0.1,
            source: hot.source,
            is_test: false,
        },
        test_name: "selected".into(),
        harness: "unittests src/lib.rs".into(),
        blob: "blob".into(),
        search_key: "key".into(),
        analysis: analysis::FunctionAnalysis::default(),
        candidate_id: 0,
    };
    pick.analysis = analysis::analyze(&pick.hot.source, 1);
    night
        .store_candidates(run, std::slice::from_mut(&mut pick))
        .await
        .unwrap();
    let measurement = Measurement {
        memory: Some(MemoryStats {
            allocations: 10,
            allocated_bytes: 1000,
        }),
        total: 20_000,
        stdout: b"ok".to_vec(),
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
    };
    let test = ScoredTest {
        baseline: measurement.clone(),
        eligible: true,
        identity: LinkedTest {
            discovered: selector::discover_harness(&root.path().join("src/lib.rs")).remove(0),
            logical: pick.harness.clone(),
            executable: root.path().join("unused"),
            working_directory: root.path().into(),
            score_name: "selected".into(),
            guest_args: Vec::new(),
            bench: None,
        },
    };
    let key = night
        .search_key(root.path(), &RepoConfig::default(), &pick.hot, &test)
        .unwrap();
    let mut changed_config = RepoConfig::default();
    changed_config.features.push("other".into());
    assert_ne!(
        key,
        night
            .search_key(root.path(), &changed_config, &pick.hot, &test)
            .unwrap()
    );
    let replacement = ORIGINAL.replace(
        "let temporary = xs.to_vec(); temporary.clone()",
        "xs.to_vec()",
    );
    std::fs::write(root.path().join("src/lib.rs"), &replacement).unwrap();
    let diff = crate::process::run(root.path(), "git", &["diff"], &[], Duration::from_secs(10))
        .await
        .stdout;
    let mut candidate = measurement;
    candidate.memory = Some(MemoryStats {
        allocations: 5,
        allocated_bytes: 500,
    });
    candidate.total = 10_000;
    assert!(matches!(
        night
            .finish_measurement(
                root.path(),
                &RepoConfig::default(),
                &test,
                &pick,
                &base,
                "Remove the copy.",
                &diff,
                Pair::Match(candidate)
            )
            .await
            .unwrap(),
        TrialFlow::Accepted
    ));
    let accepted = db.trials_for_run(run).await.unwrap();
    assert_eq!(accepted[0].status, "accepted");
    assert!(accepted[0]
        .memory_after_json
        .as_ref()
        .unwrap()
        .contains("500"));
    assert!(accepted[0].analysis_json.as_ref().unwrap().contains("copy"));
    assert!(db.commit_for_trial(accepted[0].id).await.unwrap().is_some());
    assert_ne!(worktree::head_sha(root.path()).await.unwrap(), base);
    let current = worktree::head_sha(root.path()).await.unwrap();
    std::fs::write(root.path().join("src/lib.rs"), ORIGINAL).unwrap();
    assert!(matches!(
        night
            .finish_measurement(
                root.path(),
                &RepoConfig::default(),
                &test,
                &pick,
                &current,
                "Reject this change.",
                &diff,
                Pair::Mismatch
            )
            .await
            .unwrap(),
        TrialFlow::Next
    ));
    assert_eq!(
        std::fs::read_to_string(root.path().join("src/lib.rs")).unwrap(),
        replacement
    );
    assert_eq!(db.trials_for_run(run).await.unwrap()[1].status, "rejected");
}
