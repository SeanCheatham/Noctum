#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use noctum::config::Config;
use noctum::db::Database;
use noctum::optimization::{Goal, Selection};
use noctum::orchestrator::{Night, RunOptions};
use noctum::ports::{ChatMessage, Completion, Inferencer};
use noctum::runtime::{ContainerSession, Execution, RuntimeHost, RuntimeMeasurer};

const ORIGINAL: &str = "pub fn encode(input: &[u8]) -> Vec<u8> { let copy = std::hint::black_box(input.to_vec()); copy.clone() }";
const ORIGINAL_OTHER: &str = "pub fn encode_other(input: &[u8]) -> Vec<u8> { let copy = std::hint::black_box(input.to_vec()); std::hint::black_box(copy.clone()) }";
const REPLACEMENT: &str = "pub fn encode(input: &[u8]) -> Vec<u8> { input.to_vec() }";

struct FixedModel;
impl Inferencer for FixedModel {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _: &AtomicBool,
    ) -> anyhow::Result<Completion> {
        anyhow::ensure!(
            messages[1].content.contains("Optimization goal: Memory"),
            "memory goal is missing"
        );
        anyhow::ensure!(
            messages[1].content.contains("Syntax analysis only"),
            "source findings are missing"
        );
        let other = messages[1].content.contains("Hot function: encode_other\n");
        let file = if other {
            "src/second.rs"
        } else {
            "src/first.rs"
        };
        let original = if other {
            ORIGINAL_OTHER.to_string()
        } else {
            ORIGINAL.into()
        };
        let replacement = if other {
            REPLACEMENT.replace("fn encode(", "fn encode_other(")
        } else {
            REPLACEMENT.into()
        };
        Ok(Completion {
            summary: "Remove the intermediate copy.".into(),
            diff: format!("diff --git a/{file} b/{file}\n--- a/{file}\n+++ b/{file}\n@@ -1,2 +1,2 @@\n-{original}\n+{replacement}\n // End of encoder.\n"),
        })
    }
}

async fn git(root: &Path, args: &[&str]) -> String {
    let result = noctum::process::run(root, "git", args, &[], Duration::from_secs(10)).await;
    assert!(result.success, "{}", result.combined());
    result.stdout
}

#[tokio::test]
#[ignore = "requires a prepared container worker"]
async fn memory_sweep_accepts_a_measured_copy_reduction() {
    let data = std::path::PathBuf::from(std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").unwrap())
        .canonicalize()
        .unwrap();
    std::fs::create_dir_all(data.join("worktrees")).unwrap();
    let sandbox = tempfile::tempdir_in(data.join("worktrees")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let root = root.canonicalize().unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='memory-fixture'\nversion='0.1.0'\nedition='2021'\n[[bench]]\nname='workload'\nharness=false\n",
    )
    .unwrap();
    std::fs::write(root.join(".gitignore"), "/target/\n").unwrap();
    std::fs::write(
        root.join("noctum.toml"),
        "package='memory-fixture'\nbenches=['workload']\nskip_tests=['*']\nclean_paths=[]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/first.rs"),
        format!("{ORIGINAL}\n// End of encoder.\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("src/second.rs"),
        format!("{ORIGINAL_OTHER}\n// End of encoder.\n"),
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub mod first; pub mod second;\n#[cfg(test)] mod tests {\n #[test] fn empty() { assert!(super::first::encode(&[]).is_empty()); assert!(super::second::encode_other(&[]).is_empty()); }\n}\n").unwrap();
    std::fs::create_dir(root.join("benches")).unwrap();
    std::fs::write(root.join("benches/workload.rs"), "use memory_fixture::{first::encode, second::encode_other};\nfn main() {\n let input = std::hint::black_box([7u8; 256]);\n for _ in 0..64 {\n  let output = encode(std::hint::black_box(&input));\n  let other = encode_other(std::hint::black_box(&input));\n  assert_eq!(output.as_slice(), input.as_slice());\n  assert_eq!(other.as_slice(), input.as_slice());\n  std::hint::black_box((output, other));\n }\n}\n").unwrap();
    git(&root, &["init", "-q"]).await;
    git(&root, &["config", "user.name", "Test"]).await;
    git(&root, &["config", "user.email", "test@example.com"]).await;
    git(&root, &["add", "."]).await;
    git(&root, &["commit", "-qm", "Add fixture"]).await;
    let head = git(&root, &["rev-parse", "HEAD"]).await;
    let db = Database::new(&temp.path().join("results.db"))
        .await
        .unwrap();
    db.run_migrations().await.unwrap();
    let id = db
        .add_repository(root.to_str().unwrap(), "memory fixture")
        .await
        .unwrap();
    let repo = db.get_repository(id).await.unwrap().unwrap();
    let mut config = Config {
        data_dir: Some(sandbox.path().into()),
        ..Default::default()
    };
    config.optimization.goal = Goal::Memory;
    config.optimization.selection = Selection::Sweep;
    config.gate.max_candidates = 2;
    config.gate.min_insn_delta = 100_000_000;
    config.guest.memory_mb = 512;
    let session = ContainerSession::start(&data, std::slice::from_ref(&root))
        .await
        .unwrap();
    let execution = Execution::Container(session.clone());
    let night = Night::new(
        db.clone(),
        config,
        RuntimeMeasurer::Container(session.clone()),
        FixedModel,
        RuntimeHost(execution.clone()),
    )
    .with_execution(execution);
    let result = night
        .run_repository(&repo, &RunOptions::manual(Arc::new(AtomicBool::new(false))))
        .await;
    session.stop().await.unwrap();
    result.unwrap();
    let run = db.latest_run(id).await.unwrap().unwrap();
    assert_eq!(run.status, "ok", "{:?}", run.error);
    assert!(run.measurement_profile.contains("allocator=system-v1"));
    let trials = db.trials_for_run(run.id).await.unwrap();
    assert_eq!(trials.len(), 2, "{trials:?}");
    assert!(
        trials.iter().all(|trial| trial.status == "accepted"),
        "{trials:?}"
    );
    assert!(trials
        .iter()
        .all(|trial| trial.baseline_count.unwrap() < 100_000_000));
    let before: noctum::optimization::MemoryStats =
        serde_json::from_str(trials[0].memory_before_json.as_deref().unwrap()).unwrap();
    let after: noctum::optimization::MemoryStats =
        serde_json::from_str(trials[0].memory_after_json.as_deref().unwrap()).unwrap();
    assert!(after.allocations < before.allocations);
    assert!(after.allocated_bytes < before.allocated_bytes);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]).await, head);
    assert!(git(&root, &["status", "--porcelain"]).await.is_empty());
    assert!(!git(&root, &["show", "noctum/perf:Cargo.toml"])
        .await
        .contains("noctum-fence"));
    let second_before: noctum::optimization::MemoryStats =
        serde_json::from_str(trials[1].memory_before_json.as_deref().unwrap()).unwrap();
    let final_stats: noctum::optimization::MemoryStats =
        serde_json::from_str(trials[1].memory_after_json.as_deref().unwrap()).unwrap();
    assert_eq!(after, second_before);
    assert!(final_stats.allocations < second_before.allocations);
    println!("Memory: {before:?} -> {after:?} -> {final_stats:?}");
}
