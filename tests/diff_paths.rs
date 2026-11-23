use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn git_file_names_keep_patch_and_assertion_checks() {
    let original = "#[test]\nfn chosen() { assert_eq!(1, 1); }\n";
    for name in ["tests/my file.rs", "tests/my b/file.rs", "tests/é file.rs"] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let file = root.join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, original).unwrap();
        git(root, &["init", "-q"]);
        git(root, &["add", "."]);
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "init",
            ],
        );
        std::fs::write(&file, format!("{original}// Keep the result check.\n")).unwrap();
        let diff = git(root, &["diff", "--", name]);
        let files = HashMap::from([(name.to_string(), original.to_string())]);
        noctum::diffcheck::check_diff(&diff, &files, Some(name), "chosen", &[]).unwrap();
        assert!(noctum::diffcheck::check_diff(
            &diff,
            &files,
            Some(name),
            "chosen",
            &[name.to_string()]
        )
        .is_err());
        let without_headers: String = diff
            .split_inclusive('\n')
            .filter(|line| !line.starts_with("--- ") && !line.starts_with("+++ "))
            .collect();
        let repaired = noctum::diffcheck::add_missing_file_headers(&without_headers);
        noctum::diffcheck::check_diff(&repaired, &files, Some(name), "chosen", &[]).unwrap();
        std::fs::write(&file, original).unwrap();
        let mut apply = Command::new("git")
            .args(["apply", "--check", "-"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        apply
            .stdin
            .take()
            .unwrap()
            .write_all(repaired.as_bytes())
            .unwrap();
        let output = apply.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(&file, "#[test]\nfn chosen() {}\n").unwrap();
        let removed = git(root, &["diff", "--", name]);
        let error =
            noctum::diffcheck::check_diff(&removed, &files, Some(name), "chosen", &[]).unwrap_err();
        assert!(error.contains("assertion"), "{name}: {error}");
    }
}
