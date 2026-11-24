use std::process::Command;

#[test]
fn analyze_reads_working_source_without_runtime_or_registration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn work(xs: &[String]) { for x in xs { consume(x.clone()); } }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!("data_dir = {:?}\n", root.join("data")),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_noctum"))
        .args([
            "--config",
            root.join("config.toml").to_str().unwrap(),
            "analyze",
            "--repo",
            root.to_str().unwrap(),
            "--json",
        ])
        .env("NOCTUM_HOST_HELPER", root.join("missing-helper"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let functions: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(functions[0]["function"], "work");
    assert_eq!(functions[0]["analysis"]["findings"][0]["kind"], "copy");
    assert_eq!(functions[0]["analysis"]["findings"][0]["in_loop"], true);
    assert!(!root.join("data").exists());
    assert!(!root.join("noctum.toml").exists());
    assert!(
        Command::new("git")
            .args(["rev-parse", "--verify", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap()
            .status
            .code()
            != Some(0)
    );
}
