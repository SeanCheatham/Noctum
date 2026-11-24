#![cfg(target_arch = "aarch64")]

use std::process::Command;

#[test]
fn allocator_counts_requests_and_preserves_output() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let fence = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/noctum-fence");
    std::fs::write(temp.path().join("Cargo.toml"), format!(
        "[package]\nname='memory-check'\nversion='0.1.0'\nedition='2021'\n[workspace]\n[dependencies]\nnoctum-fence={{path={:?}, features=['memory']}}\n", fence
    )).unwrap();
    std::fs::write(
        temp.path().join("src/main.rs"),
        r#"
fn main() {
    let fail = std::env::args().any(|arg| arg == "--fail");
    {
        let _guard = noctum_fence::guard();
        let mut buffer = std::hint::black_box(Vec::<u8>::with_capacity(16));
        buffer.resize(16, 7);
        buffer.reserve_exact(16);
        std::hint::black_box(&buffer);
        drop(buffer);
        let zeroes = std::hint::black_box(vec![0u8; 16]);
        std::hint::black_box(&zeroes);
        drop(zeroes);
        if fail { panic!("expected failure"); }
    }
    println!("complete");
}
"#,
    )
    .unwrap();
    let build = Command::new("cargo")
        .args(["build", "--release", "--offline"])
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    for _ in 0..2 {
        let result = Command::new(temp.path().join("target/release/memory-check"))
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"complete\n");
        let (stderr, stats) = noctum::optimization::extract_memory(&result.stderr).unwrap();
        assert!(stderr.is_empty(), "{stderr:?}");
        assert_eq!(
            stats,
            Some(noctum::optimization::MemoryStats {
                allocations: 3,
                allocated_bytes: 64
            })
        );
    }
    let failed = Command::new(temp.path().join("target/release/memory-check"))
        .arg("--fail")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let (stderr, stats) = noctum::optimization::extract_memory(&failed.stderr).unwrap();
    assert!(stats.is_some());
    assert!(String::from_utf8_lossy(&stderr).contains("expected failure"));
}
