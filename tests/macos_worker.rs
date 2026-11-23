#![cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn workers(_data: &Path) -> String {
    let output = Command::new("pgrep")
        .args(["-f", "/MacOS/com.apple.Virtualization.VirtualMachine$"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
#[ignore = "requires a prepared container worker"]
async fn guest_fences_repeat_and_count_more_work() {
    use noctum::ports::{MeasureRequest, Measurer};
    use noctum::runtime::{ContainerSession, Execution, RuntimeMeasurer};
    let data = PathBuf::from(std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").unwrap())
        .canonicalize()
        .unwrap();
    std::fs::create_dir_all(data.join("worktrees")).unwrap();
    let temp = tempfile::tempdir_in(data.join("worktrees")).unwrap();
    let root = temp.path();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    std::fs::write(root.join(".gitignore"), "/target/\n/result-*/\n").unwrap();
    std::fs::create_dir_all(root.join("member/src")).unwrap();
    std::fs::create_dir_all(root.join("member/fixtures")).unwrap();
    std::fs::write(
        root.join("member/fixtures/input.bin"),
        [0, 255, 13, 10, 128],
    )
    .unwrap();
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
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=[\"member\",\"fence\"]\nresolver=\"2\"\n",
    )
    .unwrap();
    std::fs::write(root.join("member/Cargo.toml"), "[package]\nname=\"fence-test\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nnoctum-fence={path=\"../fence\"}\n").unwrap();
    std::fs::write(root.join("member/src/main.rs"), r#"use std::io::Write;
fn main() {
    let bound: u64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let input = std::fs::read("fixtures/input.bin").unwrap();
    assert_eq!(input, std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/input.bin")).unwrap());
    std::io::stdout().write_all(&input).unwrap();
    std::io::stderr().write_all(&input).unwrap();
    let _guard = noctum_fence::guard();
    let mut n = 0u64;
    for i in 0..bound { n = std::hint::black_box(n.wrapping_add(i)); }
    std::hint::black_box(n);
}"#).unwrap();
    let session = ContainerSession::start(&data, &[]).await.unwrap();
    let execution = Execution::Container(session.clone());
    let target = execution.target().triple();
    std::fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel=\"1.99.0\"\nprofile=\"minimal\"\n",
    )
    .unwrap();
    execution.ensure_target(root).await.unwrap();
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
    let file = root.join("member/src/main.rs");
    let source = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        format!("// {}\n{source}", "input growth ".repeat(200)),
    )
    .unwrap();
    let rebuilt = execution
        .run(
            root,
            "cargo",
            &["build", "--release", "--target", target],
            &[],
            Duration::from_secs(120),
        )
        .await;
    assert!(rebuilt.success, "{}", rebuilt.combined());
    let measurer = RuntimeMeasurer::Container(session.clone());
    let mut counts = Vec::new();
    for (index, bound) in [1000, 1000, 2000, 2000].into_iter().enumerate() {
        if index == 1 || index == 3 {
            session.suspend().await.unwrap();
            session.suspend().await.unwrap();
            let start = Instant::now();
            while !workers(&data).is_empty() {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "worker did not stop"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let measured = measurer
            .measure(&MeasureRequest {
                worktree: root.into(),
                working_directory: Some(root.join("member")),
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
        assert_eq!(measured.stdout, [0, 255, 13, 10, 128]);
        assert_eq!(measured.stderr, [0, 255, 13, 10, 128]);
        counts.push(measured.total);
    }
    session.suspend().await.unwrap();
    session.stop().await.unwrap();
    let stopped = execution
        .run(root, "true", &[] as &[&str], &[], Duration::from_secs(10))
        .await;
    assert!(!stopped.success);
    assert!(stopped.stderr.contains("worker is stopped"));
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
    let session = noctum::runtime::ContainerSession::start(&data, &[])
        .await
        .unwrap();
    std::fs::write(marker, "ready").unwrap();
    std::future::pending::<()>().await;
    session.stop().await.unwrap();
}

#[test]
#[ignore = "requires a prepared container worker"]
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
    assert!(!workers(&data).trim().is_empty());
    child.kill().unwrap();
    child.wait().unwrap();
    let start = Instant::now();
    while !workers(&data).trim().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(45),
            "worker survived controller exit"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[tokio::test]
#[ignore = "requires a prepared container worker"]
async fn stop_cancels_an_active_guest_command() {
    use noctum::runtime::{ContainerSession, Execution};
    let data = PathBuf::from(std::env::var_os("NOCTUM_TEST_RUNTIME_DATA").unwrap())
        .canonicalize()
        .unwrap();
    std::fs::create_dir_all(data.join("worktrees")).unwrap();
    let directory = tempfile::tempdir_in(data.join("worktrees")).unwrap();
    let marker = directory.path().join("started");
    let session = ContainerSession::start(&data, &[]).await.unwrap();
    let duplicate = ContainerSession::start(&data, &[]).await;
    assert!(format!("{:?}", duplicate.unwrap_err()).contains("worker is in use"));
    let execution = Execution::Container(session.clone());
    let cwd = directory.path().to_path_buf();
    let task = tokio::spawn(async move {
        execution
            .run(
                &cwd,
                "sh",
                &["-c", "touch started; sleep 60"],
                &[],
                Duration::from_secs(120),
            )
            .await
    });
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            !task.is_finished(),
            "guest command stopped before its marker"
        );
        assert!(
            start.elapsed() < Duration::from_secs(45),
            "guest command did not start"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let start = Instant::now();
    session.stop().await.unwrap();
    session.stop().await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(!output.success);
    assert!(start.elapsed() < Duration::from_secs(10));
}
