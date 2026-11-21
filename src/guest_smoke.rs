//! Boots the pinned guest. `cargo test` skips it. `qemu/smoke.sh` runs it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::builder::{self, BuiltHarness};
use crate::ports::{MeasureRequest, Measurement, Measurer};
use crate::qemu::QemuRunner;
use crate::repo_config::RepoConfig;

fn stage_dir() -> PathBuf {
    if let Ok(path) = std::env::var("NOCTUM_QEMU_STAGE") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qemu/out/stage")
}

#[tokio::test]
#[ignore = "boots the pinned QEMU guest; run qemu/smoke.sh"]
async fn fenced_musl_harness_counts_guest_instructions() {
    let stage = stage_dir();
    assert!(
        stage.join("qemu-system-x86_64").is_file(),
        "QEMU bundle stage is missing at {}. Run qemu/build-bundle.sh",
        stage.display()
    );
    let data = tempfile::tempdir().unwrap();
    let lock = crate::lockfile::QemuLock::load().unwrap();
    install_stage(&stage, &data.path().join("qemu").join(&lock.id));

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("qemu/fixtures/harness");
    // Unit-test harnesses are reported as kind "lib" with an executable.
    let harness = build_harness(&fixture).await;
    let guest_bin = fixture.join("guest-harness");
    std::fs::copy(&harness.executable, &guest_bin).unwrap();
    mark_executable(&guest_bin);
    assert_static_elf(&guest_bin);
    assert_fence_bytes(&guest_bin);

    let runner = QemuRunner::new(data.path(), 512);
    let small = measure_twice(&runner, data.path(), &fixture, "tests::small_loop").await;
    let large = measure_twice(&runner, data.path(), &fixture, "tests::encode_batch").await;
    println!(
        "SMOKE small_loop total={} encode_batch total={}",
        small.0.total, large.0.total
    );
    assert_eq!(
        small.0.total,
        small.1.total,
        "{}",
        counts("small_loop", &small.0, &small.1)
    );
    assert_eq!(
        large.0.total,
        large.1.total,
        "{}",
        counts("encode_batch", &large.0, &large.1)
    );
    assert_eq!(
        stable_stdout(&small.0.stdout),
        stable_stdout(&small.1.stdout)
    );
    assert_eq!(
        stable_stdout(&large.0.stdout),
        stable_stdout(&large.1.stdout)
    );
    assert!(
        large.0.total >= 10_000,
        "encode_batch counted {}",
        large.0.total
    );
    assert!(
        large.0.total > small.0.total.saturating_mul(3),
        "small {} did not stay below encode_batch {}",
        small.0.total,
        large.0.total
    );
}

async fn build_harness(fixture: &Path) -> BuiltHarness {
    let built = builder::build_tests(fixture, "noctum-smoke-harness", &RepoConfig::default())
        .await
        .expect("musl test harness");
    built
        .into_iter()
        .find(|item| item.kind == "test" || item.kind == "lib")
        .expect("cargo produced a test harness")
}

async fn measure_twice(
    runner: &QemuRunner,
    data: &Path,
    fixture: &Path,
    test: &str,
) -> (Measurement, Measurement) {
    let first = run_once(runner, data, fixture, test, 1).await;
    let second = run_once(runner, data, fixture, test, 2).await;
    (first, second)
}

async fn run_once(
    runner: &QemuRunner,
    data: &Path,
    fixture: &Path,
    test: &str,
    run: u32,
) -> Measurement {
    let scratch = data.join("runs").join(format!("{test}-{run}"));
    std::fs::create_dir_all(&scratch).unwrap();
    let measurement = runner
        .measure(&MeasureRequest {
            worktree: fixture.to_path_buf(),
            harness: fixture.join("guest-harness"),
            guest_exe: "/work/guest-harness".into(),
            args: vec![
                test.to_string(),
                "--exact".into(),
                "--test-threads=1".into(),
            ],
            timeout_secs: 300,
            scratch,
            memory_mb: 512,
        })
        .await
        .unwrap_or_else(|err| panic!("{test} run {run}: {err}"));
    assert_eq!(
        measurement.exit_code,
        0,
        "{test} run {run} exited {}\nstdout {}\nstderr {}\nserial {}\ndump {:?}",
        measurement.exit_code,
        String::from_utf8_lossy(&measurement.stdout),
        String::from_utf8_lossy(&measurement.stderr),
        measurement.serial_log,
        measurement.dump
    );
    assert!(
        measurement.dump.error.is_none(),
        "{test} run {run} dump error {:?}\nstdout {}\nstderr {}\nserial {}",
        measurement.dump.error,
        String::from_utf8_lossy(&measurement.stdout),
        String::from_utf8_lossy(&measurement.stderr),
        measurement.serial_log
    );
    assert!(measurement.total > 0, "{test} run {run} counted nothing");
    measurement
}

fn stable_stdout(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.contains("finished in"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn counts(name: &str, first: &Measurement, second: &Measurement) -> String {
    format!(
        "{name} counts {} and {}\nstdout {}\n{}",
        first.total,
        second.total,
        String::from_utf8_lossy(&first.stdout),
        first.serial_log
    )
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
        std::fs::copy(stage.join(name), &dest).unwrap();
        if name.ends_with(".so") || !name.contains('.') {
            mark_executable(&dest);
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

fn mark_executable(path: &Path) {
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn assert_static_elf(path: &Path) {
    let output = Command::new("ldd").arg(path).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("not a dynamic executable") || text.contains("statically linked"),
        "harness is not a static ELF: {text}"
    );
}

fn assert_fence_bytes(path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    assert!(
        bytes.windows(2).any(|pair| pair == [0xe6, 0xe9]),
        "harness has no out 0xe9 fence"
    );
}
