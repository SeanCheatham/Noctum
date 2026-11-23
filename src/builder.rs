//! Musl `cargo test` builds. Cargo is the only build. A dynamic harness is
//! refused; there is no gnu fallback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::ports::CheckOutput;
use crate::process::bench_env;
use crate::repo_config::RepoConfig;
use crate::runtime::Execution;

#[derive(Debug, Clone)]
pub struct CargoTarget {
    pub name: String,
    pub kind: String,
    pub src_path: PathBuf,
    pub harness: bool,
    pub test_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub manifest: PathBuf,
    pub manifest_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub targets: Vec<CargoTarget>,
    pub uses_criterion: bool,
}

#[derive(Debug, Clone)]
pub struct BuiltHarness {
    pub logical_name: String,
    pub executable: PathBuf,
    pub src_path: PathBuf,
    pub kind: String,
    pub target_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedTest {
    pub name: String,
    pub kind: String,
}

#[cfg(test)]
pub async fn resolve_package(worktree: &Path, requested: Option<&str>) -> Result<ResolvedPackage> {
    resolve_package_with(&Execution::Local, worktree, requested).await
}

pub async fn resolve_package_with(
    execution: &Execution,
    worktree: &Path,
    requested: Option<&str>,
) -> Result<ResolvedPackage> {
    let output = execution
        .run(
            worktree,
            "cargo",
            &[
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
            ],
            &[],
            Duration::from_secs(120),
        )
        .await;
    let output = if output.success {
        output
    } else {
        execution
            .run(
                worktree,
                "cargo",
                &["metadata", "--format-version", "1", "--no-deps"],
                &[],
                Duration::from_secs(180),
            )
            .await
    };
    if !output.success {
        bail!("cargo metadata failed: {}", output.combined());
    }
    let meta: Value = serde_json::from_str(&output.stdout).context("parse cargo metadata")?;
    let workspace_root = PathBuf::from(meta["workspace_root"].as_str().unwrap_or(""));
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .map(|items| items.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let packages = meta["packages"]
        .as_array()
        .context("metadata has no packages")?;
    let selected: Vec<&Value> = packages
        .iter()
        .filter(|pkg| pkg["id"].as_str().is_some_and(|id| members.contains(&id)))
        .collect();
    let package = if let Some(name) = requested {
        selected
            .iter()
            .copied()
            .find(|pkg| pkg["name"].as_str() == Some(name))
            .with_context(|| format!("package {name} is not in this workspace"))?
    } else if selected.len() == 1 {
        selected[0]
    } else {
        let default =
            meta["workspace_default_members"]
                .as_array()
                .and_then(|members| match members.as_slice() {
                    [member] => Some(member),
                    _ => None,
                });
        if let Some(default) = default {
            selected
                .iter()
                .copied()
                .find(|pkg| pkg["id"] == *default)
                .context("default package is not in the workspace")?
        } else {
            bail!(
                "workspace has {} packages; set package in noctum.toml",
                selected.len()
            );
        }
    };
    let manifest = PathBuf::from(package["manifest_path"].as_str().context("manifest path")?);
    let manifest_text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("read {}", manifest.display()))?;
    let bench_harness = bench_harness_flags(&manifest_text);
    let uses_criterion = package["dependencies"].as_array().is_some_and(|deps| {
        deps.iter()
            .any(|dep| dep["name"].as_str() == Some("criterion"))
    });
    let mut targets = Vec::new();
    for target in package["targets"].as_array().context("targets")? {
        let kinds = target["kind"].as_array().cloned().unwrap_or_default();
        let kind = kinds
            .iter()
            .filter_map(|kind| kind.as_str())
            .find(|kind| matches!(*kind, "lib" | "bin" | "test" | "bench"))
            .map(str::to_string);
        let Some(kind) = kind else { continue };
        let name = target["name"].as_str().unwrap_or("").to_string();
        // Current cargo metadata omits `harness`. The manifest is what says
        // `harness = false`, which is the only bench shape that is scored.
        let harness = target["harness"].as_bool().unwrap_or_else(|| {
            if kind == "bench" {
                bench_harness.get(&name).copied().unwrap_or(true)
            } else {
                test_harness_flag(&manifest_text, &kind, &name)
            }
        });
        targets.push(CargoTarget {
            name,
            kind,
            src_path: PathBuf::from(target["src_path"].as_str().unwrap_or("")),
            harness,
            test_enabled: target["test"].as_bool().unwrap_or(true),
        });
    }
    Ok(ResolvedPackage {
        name: package["name"].as_str().unwrap_or("").to_string(),
        manifest_dir: manifest.parent().unwrap_or(worktree).to_path_buf(),
        manifest,
        workspace_root,
        targets,
        uses_criterion,
    })
}

pub fn forces_abort(workspace: &Path, manifest: &Path) -> Result<bool> {
    let mut texts = Vec::new();
    for path in [
        manifest.to_path_buf(),
        workspace.join("Cargo.toml"),
        workspace.join(".cargo/config.toml"),
        workspace.join(".cargo/config"),
        manifest
            .parent()
            .unwrap_or(workspace)
            .join(".cargo/config.toml"),
    ] {
        if path.is_file() {
            texts.push(std::fs::read_to_string(&path).with_context(|| path.display().to_string())?);
        }
    }
    Ok(texts.iter().any(|text| text_forces_abort(text)))
}

fn text_forces_abort(text: &str) -> bool {
    text.lines().any(|line| {
        let code = line.split('#').next().unwrap_or("");
        code.contains("panic = \"abort\"")
            || code.contains("panic=\"abort\"")
            || code.contains("panic=abort")
    })
}

/// `harness` flags from `[[bench]]` tables. Cargo's default is `true`.
pub fn bench_harness_flags(manifest: &str) -> HashMap<String, bool> {
    let mut flags = HashMap::new();
    let Ok(value) = toml::from_str::<Value>(manifest) else {
        return flags;
    };
    let Some(tables) = value.get("bench").and_then(Value::as_array) else {
        return flags;
    };
    for table in tables {
        let Some(name) = table.get("name").and_then(Value::as_str) else {
            continue;
        };
        let harness = table
            .get("harness")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        flags.insert(name.to_string(), harness);
    }
    flags
}

pub fn feature_args(repo: &RepoConfig) -> Vec<String> {
    let mut args = Vec::new();
    if repo.no_default_features {
        args.push("--no-default-features".into());
    }
    if repo.all_features {
        args.push("--all-features".into());
    } else if !repo.features.is_empty() {
        args.extend(["--features".into(), repo.features.join(",")]);
    }
    args
}

fn test_harness_flag(manifest: &str, kind: &str, name: &str) -> bool {
    let Ok(value) = toml::from_str::<toml::Value>(manifest) else {
        return true;
    };
    let target = if kind == "lib" {
        value.get("lib")
    } else {
        value
            .get(kind)
            .and_then(|v| v.as_array())
            .and_then(|items| {
                items
                    .iter()
                    .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
            })
    };
    target
        .and_then(|t| t.get("harness"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

pub async fn build_test_targets_with(
    execution: &Execution,
    worktree: &Path,
    package: &str,
    repo: &RepoConfig,
    targets: &[CargoTarget],
) -> Result<Vec<BuiltHarness>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "test".into(),
        "--no-run".into(),
        "--release".into(),
        "--message-format=json".into(),
        "--target".into(),
        execution.target().triple().into(),
        "-p".into(),
        package.into(),
    ];
    for target in targets {
        match target.kind.as_str() {
            "lib" => args.push("--lib".into()),
            "bin" | "test" => args.extend([format!("--{}", target.kind), target.name.clone()]),
            other => bail!("{other} is not a test harness target"),
        }
    }
    args.extend(feature_args(repo));
    build_harnesses(execution, worktree, &args, repo.timeout_seconds, "test").await
}

pub fn clean_paths(worktree: &Path, paths: &[String]) -> Result<()> {
    use std::path::Component;

    let mut checked = Vec::new();
    for rel in paths {
        let parts: Vec<_> = Path::new(rel).components().collect();
        if parts.is_empty()
            || parts
                .iter()
                .any(|part| !matches!(part, Component::Normal(_)))
            || parts.first().is_some_and(|part| {
                part.as_os_str()
                    .as_encoded_bytes()
                    .eq_ignore_ascii_case(b".git")
            })
        {
            bail!("clean_paths must name a path inside the worktree: {rel}");
        }
        let mut parent = worktree.to_path_buf();
        for part in &parts[..parts.len() - 1] {
            parent.push(part.as_os_str());
            match std::fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.is_symlink() => {
                    bail!("clean_paths cannot pass through a symbolic link: {rel}");
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(error).with_context(|| parent.display().to_string()),
            }
        }
        checked.push(worktree.join(rel));
    }
    for path in checked {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.is_dir() && !metadata.is_symlink() {
                    std::fs::remove_dir_all(&path)
                        .with_context(|| format!("remove {}", path.display()))?;
                } else {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("remove {}", path.display()))?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| path.display().to_string()),
        }
    }
    Ok(())
}

pub async fn build_tests_with(
    execution: &Execution,
    worktree: &Path,
    package: &str,
    repo: &RepoConfig,
) -> Result<Vec<BuiltHarness>> {
    let mut args = vec![
        "test".to_string(),
        "--no-run".into(),
        "--release".into(),
        "--message-format=json".into(),
        "--target".into(),
        execution.target().triple().into(),
        "-p".into(),
        package.into(),
    ];
    args.extend(feature_args(repo));
    build_harnesses(execution, worktree, &args, repo.timeout_seconds, "test").await
}

pub async fn build_bench_with(
    execution: &Execution,
    worktree: &Path,
    package: &str,
    bench: &str,
    repo: &RepoConfig,
) -> Result<Vec<BuiltHarness>> {
    let mut args = vec![
        "bench".to_string(),
        "--no-run".into(),
        "--message-format=json".into(),
        "--target".into(),
        execution.target().triple().into(),
        "-p".into(),
        package.into(),
        "--bench".into(),
        bench.into(),
    ];
    args.extend(feature_args(repo));
    build_harnesses(execution, worktree, &args, repo.timeout_seconds, "bench").await
}

pub fn pick_bench(harnesses: &[BuiltHarness], name: &str) -> Result<PathBuf> {
    harnesses
        .iter()
        .find(|harness| harness.target_name == name)
        .or_else(|| harnesses.iter().find(|harness| harness.kind == "bench"))
        .map(|harness| harness.executable.clone())
        .context("cargo bench produced no binary")
}

async fn build_harnesses(
    execution: &Execution,
    worktree: &Path,
    args: &[String],
    timeout_secs: u64,
    fallback_kind: &str,
) -> Result<Vec<BuiltHarness>> {
    let env = bench_env();
    let output = execution
        .run(
            worktree,
            "cargo",
            args,
            &env,
            Duration::from_secs(timeout_secs),
        )
        .await;
    if !output.success {
        bail!(
            "cargo {} failed:\n{}",
            args.join(" "),
            cargo_diagnostics(&output)
        );
    }
    parse_artifacts(worktree, &output.stdout, fallback_kind)
}

fn cargo_diagnostics(output: &crate::process::CmdOutput) -> String {
    let mut other = Vec::new();
    let mut errors = Vec::new();
    for line in output.stdout.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(message) if message["reason"] == "compiler-message" => {
                let diagnostic = &message["message"];
                let text = diagnostic["rendered"]
                    .as_str()
                    .or_else(|| diagnostic["message"].as_str())
                    .unwrap_or("");
                if diagnostic["level"] == "error" || diagnostic["level"] == "fatal" {
                    errors.push(text.to_string());
                } else {
                    other.push(text.to_string());
                }
            }
            Err(_) => other.push(line.to_string()),
            _ => {}
        }
    }
    let text = [output.stderr.clone(), other.join("\n"), errors.join("\n")].join("\n");
    crate::hashutil::tail_str(&text, crate::LOG_LIMIT).to_string()
}

fn parse_artifacts(
    worktree: &Path,
    stdout: &str,
    fallback_kind: &str,
) -> Result<Vec<BuiltHarness>> {
    let mut harnesses = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(executable) = value["executable"].as_str() else {
            continue;
        };
        let target = &value["target"];
        let kinds: Vec<&str> = target["kind"]
            .as_array()
            .map(|items| items.iter().filter_map(|k| k.as_str()).collect())
            .unwrap_or_default();
        if kinds.contains(&"custom-build") {
            continue;
        }
        if fallback_kind == "test"
            && (value["profile"]["test"].as_bool() != Some(true)
                || !kinds.iter().any(|k| matches!(*k, "lib" | "bin" | "test")))
        {
            continue;
        }
        let src = PathBuf::from(target["src_path"].as_str().unwrap_or(""));
        let kind = kinds
            .iter()
            .copied()
            .find(|kind| matches!(*kind, "test" | "bench" | "lib" | "bin"))
            .unwrap_or(fallback_kind);
        let logical = crate::selector::logical_harness(worktree, &src, kind);
        harnesses.push(BuiltHarness {
            logical_name: logical,
            executable: PathBuf::from(executable),
            src_path: src,
            kind: kind.to_string(),
            target_name: target["name"].as_str().unwrap_or("").to_string(),
        });
    }
    if harnesses.is_empty() {
        bail!("cargo produced no test harness");
    }
    Ok(harnesses)
}

pub async fn list_tests_with(
    execution: &Execution,
    executable: &Path,
    timeout_secs: u64,
) -> Result<Vec<ListedTest>> {
    let output = execution
        .run(
            executable.parent().unwrap_or_else(|| Path::new(".")),
            executable,
            &["--list"],
            &[],
            Duration::from_secs(timeout_secs.min(120)),
        )
        .await;
    if !output.success {
        bail!(
            "harness --list failed (exit {:?}, timeout {}): {}",
            output.code,
            output.timed_out,
            output.combined()
        );
    }
    Ok(parse_test_list(&output.stdout))
}

pub fn parse_test_list(stdout: &str) -> Vec<ListedTest> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, kind) = line.rsplit_once(": ")?;
            let name = name.trim();
            let kind = kind.trim();
            if name.is_empty() || kind.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            Some(ListedTest {
                name: name.to_string(),
                kind: kind.to_string(),
            })
        })
        .collect()
}

pub async fn host_check_with(
    execution: &Execution,
    worktree: &Path,
    repo: &RepoConfig,
    package: &str,
) -> Result<CheckOutput> {
    let timeout = Duration::from_secs(repo.timeout_seconds);
    let output = if repo.check_command.trim().is_empty() {
        let mut args = vec![
            "test".to_string(),
            "--release".into(),
            "--target".into(),
            execution.target().triple().into(),
            "-p".into(),
            package.into(),
        ];
        args.extend(feature_args(repo));
        execution
            .run(worktree, "cargo", &args, &bench_env(), timeout)
            .await
    } else {
        execution
            .run(worktree, "sh", &["-c", &repo.check_command], &[], timeout)
            .await
    };
    Ok(CheckOutput {
        success: output.success,
        timed_out: output.timed_out,
        output: output.combined(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_paths_checks_all_paths_before_removal() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        for invalid in [
            "",
            ".",
            "..",
            "../outside",
            "/tmp/outside",
            ".git",
            ".git/index",
            ".GIT",
        ] {
            assert!(clean_paths(root.path(), &["target".into(), invalid.into()]).is_err());
            assert!(root.path().join("target").is_dir());
        }
        clean_paths(root.path(), &["target".into(), "missing/cache".into()]).unwrap();
        assert!(!root.path().join("target").exists());
    }

    #[cfg(unix)]
    #[test]
    fn clean_paths_keeps_symbolic_link_targets() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("keep");
        std::fs::write(&file, "keep").unwrap();
        symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(clean_paths(root.path(), &["link/keep".into()]).is_err());
        clean_paths(root.path(), &["link".into()]).unwrap();
        assert!(file.is_file());
        assert!(!root.path().join("link").exists());
        symlink(outside.path().join("missing"), root.path().join("broken")).unwrap();
        clean_paths(root.path(), &["broken".into()]).unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("broken")).is_err());
    }

    #[tokio::test]
    async fn package_selection_uses_cargo_workspace_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for path in [root.join("src"), root.join("helper/src")] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(root.join("src/lib.rs"), "pub fn root() {}\n").unwrap();
        std::fs::write(root.join("helper/src/lib.rs"), "pub fn helper() {}\n").unwrap();
        std::fs::write(
            root.join("helper/Cargo.toml"),
            "[package]\nname='helper'\nversion='0.1.0'\n",
        )
        .unwrap();
        for (default, expected) in [("", "root"), ("default-members=['helper']\n", "helper")] {
            std::fs::write(root.join("Cargo.toml"), format!("[package]\nname='root'\nversion='0.1.0'\n[dependencies]\nhelper={{path='helper'}}\n[workspace]\n{default}")).unwrap();
            assert_eq!(resolve_package(root, None).await.unwrap().name, expected);
            assert!(!root.join("Cargo.lock").exists());
        }
    }

    #[test]
    fn parses_libtest_list() {
        let listed =
            parse_test_list("tests::encode_batch: test\nparked: ignore\n0 tests, 0 benchmarks\n");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].name, "tests::encode_batch");
        assert_eq!(listed[1].kind, "ignore");
    }

    #[test]
    fn bench_harness_false_is_read_from_the_manifest() {
        let manifest = "\
[package]
name = \"fixture\"
version = \"0.1.0\"

[[bench]]
name = \"fold\"
harness = false

[[bench]]
name = \"legacy\"
";
        let flags = bench_harness_flags(manifest);
        assert_eq!(flags.get("fold"), Some(&false));
        assert_eq!(flags.get("legacy"), Some(&true));
    }

    #[test]
    fn build_errors_show_compiler_text_instead_of_artifact_records() {
        let output = crate::process::CmdOutput { success: false, timed_out: false, code: Some(101),
            stdout: [serde_json::json!({"reason":"compiler-artifact", "target":{"name":"dependency"}}).to_string(), serde_json::json!({"reason":"compiler-message", "message":{"level":"error", "rendered":"error: cannot find macro `vec` in this scope\n --> src/lib.rs:4:1\n"}}).to_string()].join("\n"),
            stderr: "Compiling dependency".into(), duration_ms: 1 };
        let text = cargo_diagnostics(&output);
        assert!(text.contains("cannot find macro `vec`"));
        assert!(text.contains("src/lib.rs:4:1"));
        assert!(!text.contains("compiler-artifact"));
    }

    #[test]
    fn executable_artifacts_must_be_test_harnesses() {
        let line = |kind: &str, test: bool, name: &str| {
            serde_json::json!({
                "reason": "compiler-artifact", "executable": format!("/repo/target/{name}"),
                "profile": { "test": test },
                "target": { "kind": [kind], "name": name, "src_path": format!("/repo/{name}.rs") }
            })
            .to_string()
        };
        let output = [
            line("bin", false, "app"),
            line("example", false, "example"),
            line("lib", true, "library"),
            line("test", true, "integration"),
        ]
        .join("\n");
        let harnesses = parse_artifacts(Path::new("/repo"), &output, "test").unwrap();
        assert_eq!(
            harnesses
                .iter()
                .map(|h| h.target_name.as_str())
                .collect::<Vec<_>>(),
            ["library", "integration"]
        );
    }

    #[test]
    fn abort_profile_is_detected() {
        assert!(text_forces_abort("[profile.release]\npanic = \"abort\"\n"));
        assert!(!text_forces_abort(
            "# panic = \"abort\"\npanic = \"unwind\"\n"
        ));
    }
}
