#![cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[tokio::test]
#[ignore = "requires Docker and a prepared worker image"]
async fn guest_fences_repeat_and_count_more_work() {
    use noctum::ports::{MeasureRequest, Measurer};
    use noctum::runtime::{DockerSession, Execution, RuntimeMeasurer};
    let data = PathBuf::from(std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").unwrap())
        .canonicalize()
        .unwrap();
    let temp = tempfile::tempdir_in(&data).unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("fence/src")).unwrap();
    std::fs::write(
        root.join("fence/Cargo.toml"),
        include_str!("../crates/noctum-fence/Cargo.toml"),
    )
    .unwrap();
    std::fs::write(
        root.join("fence/src/lib.rs"),
        include_str!("../crates/noctum-fence/src/lib.rs"),
    )
    .unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"fence-test\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nnoctum-fence={path=\"fence\"}\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { let bound: u64 = std::env::args().nth(1).unwrap().parse().unwrap(); let _guard = noctum_fence::guard(); let mut n = 0u64; for i in 0..bound { n = std::hint::black_box(n.wrapping_add(i)); } std::hint::black_box(n); }").unwrap();
    let session = DockerSession::start(&data, &[]).await.unwrap();
    let execution = Execution::Docker(session.clone());
    let target = execution.target().triple();
    let built = execution
        .run(
            root,
            "cargo",
            &["build", "--release", "--target", target],
            &[],
            Duration::from_secs(120),
        )
        .await;
    assert!(built.success, "{}", built.combined());
    let measurer = RuntimeMeasurer::Docker(session.clone());
    let mut counts = Vec::new();
    for (index, bound) in [1000, 1000, 2000, 2000].into_iter().enumerate() {
        let measured = measurer
            .measure(&MeasureRequest {
                worktree: root.into(),
                harness: root.join(format!("target/{target}/release/fence-test")),
                guest_exe: format!("/work/target/{target}/release/fence-test"),
                args: vec![bound.to_string()],
                timeout_secs: 60,
                scratch: root.join(format!("result-{index}")),
                memory_mb: 512,
            })
            .await;
        if let Err(error) = measured {
            session.stop().await.unwrap();
            panic!("{error:#}");
        }
        let measured = measured.unwrap();
        assert_eq!(measured.exit_code, 0);
        counts.push(measured.total);
    }
    session.stop().await.unwrap();
    assert_eq!(counts[0], counts[1]);
    assert_eq!(counts[2], counts[3]);
    assert!(counts[2] > counts[0] && counts[0] > 0);
    println!("GUEST_FENCES {target}: {counts:?}");
}

#[tokio::test]
#[ignore = "worker process for the controller exit test"]
async fn controller_fixture() {
    let Some(marker) = std::env::var_os("NOCTUM_TEST_WORKER_MARKER") else {
        return;
    };
    let data = PathBuf::from(std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").unwrap());
    let session = noctum::runtime::DockerSession::start(&data, &[])
        .await
        .unwrap();
    std::fs::write(marker, "ready").unwrap();
    std::future::pending::<()>().await;
    session.stop().await.unwrap();
}

#[test]
#[ignore = "requires Docker and a prepared worker image"]
fn worker_stops_after_controller_exit() {
    let data = PathBuf::from(std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").unwrap())
        .canonicalize()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("ready");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "controller_fixture", "--ignored", "--nocapture"])
        .env("NOCTUM_TEST_WORKER_MARKER", &marker)
        .env("NOCTUM_TEST_RUNTIME_DATA", &data)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "controller exited before startup"
        );
        if start.elapsed() > Duration::from_secs(600) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("worker startup timed out");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let key = noctum::hashutil::sha256_bytes(data.as_os_str().as_encoded_bytes());
    let workers = || {
        let output = Command::new("docker")
            .args([
                "ps",
                "--all",
                "--quiet",
                "--filter",
                &format!("label=org.noctum.data={}", &key[..12]),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(!workers().trim().is_empty());
    child.kill().unwrap();
    child.wait().unwrap();
    let start = Instant::now();
    while !workers().trim().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(45),
            "worker survived controller exit"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}
