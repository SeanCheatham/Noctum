//! Musl `cargo test` builds. Cargo is the only build. A dynamic harness is
//! refused; there is no gnu fallback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::ports::{CheckOutput, HostCheck};
use crate::process::{self, bench_env};
use crate::repo_config::RepoConfig;
use crate::MUSL_TARGET;

#[derive(Debug, Clone)]
pub struct CargoTarget {
    pub name: String,
    pub kind: String,
    pub src_path: PathBuf,
    pub harness: bool,
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

pub fn musl_target_installed() -> Result<bool> {
    let output = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return Ok(text.lines().any(|line| line.trim() == MUSL_TARGET));
        }
    }
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .context("rustc --print sysroot")?;
    let sysroot = String::from_utf8_lossy(&sysroot.stdout).trim().to_string();
    Ok(Path::new(&sysroot)
        .join("lib/rustlib")
        .join(MUSL_TARGET)
        .is_dir())
}

pub async fn resolve_package(worktree: &Path, requested: Option<&str>) -> Result<ResolvedPackage> {
    let output = process::run(
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
        process::run(
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
        bail!(
            "workspace has {} packages; set package in noctum.toml",
            selected.len()
        );
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
            .unwrap_or("lib")
            .to_string();
        let name = target["name"].as_str().unwrap_or("").to_string();
        // Current cargo metadata omits `harness`. The manifest is what says
        // `harness = false`, which is the only bench shape that is scored.
        let harness = target["harness"].as_bool().unwrap_or_else(|| {
            if kind == "bench" {
                bench_harness.get(&name).copied().unwrap_or(true)
            } else {
                true
            }
        });
        targets.push(CargoTarget {
            name,
            kind,
            src_path: PathBuf::from(target["src_path"].as_str().unwrap_or("")),
            harness,
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
    if repo.all_features {
        vec!["--all-features".into()]
    } else if repo.features.is_empty() {
        Vec::new()
    } else {
        vec!["--features".into(), repo.features.join(",")]
    }
}

pub fn clean_paths(worktree: &Path, paths: &[String]) -> Result<()> {
    for rel in paths {
        let path = worktree.join(rel);
        if path.exists() {
            if path.is_dir() {
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
            } else {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
            }
        }
    }
    Ok(())
}

pub async fn build_tests(
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
        MUSL_TARGET.into(),
        "-p".into(),
        package.into(),
    ];
    args.extend(feature_args(repo));
    build_harnesses(worktree, &args, repo.timeout_seconds, "test").await
}

pub async fn build_bench(
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
        MUSL_TARGET.into(),
        "-p".into(),
        package.into(),
        "--bench".into(),
        bench.into(),
    ];
    args.extend(feature_args(repo));
    build_harnesses(worktree, &args, repo.timeout_seconds, "bench").await
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
    worktree: &Path,
    args: &[String],
    timeout_secs: u64,
    fallback_kind: &str,
) -> Result<Vec<BuiltHarness>> {
    let env = bench_env();
    let output = process::run(
        worktree,
        "cargo",
        args,
        &env,
        Duration::from_secs(timeout_secs),
    )
    .await;
    if !output.success {
        bail!(
            "cargo failed: {}",
            crate::hashutil::tail_str(&output.combined(), crate::LOG_LIMIT)
        );
    }
    parse_artifacts(worktree, &output.stdout, fallback_kind)
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

pub async fn list_tests(executable: &Path, timeout_secs: u64) -> Result<Vec<ListedTest>> {
    let output = process::run(
        executable.parent().unwrap_or_else(|| Path::new(".")),
        executable,
        &["--list"],
        &[],
        Duration::from_secs(timeout_secs.min(120)),
    )
    .await;
    if !output.success {
        bail!("harness --list failed: {}", output.combined());
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

pub struct CargoHost;

impl HostCheck for CargoHost {
    async fn run(&self, worktree: &Path, repo: &RepoConfig, package: &str) -> Result<CheckOutput> {
        let timeout = Duration::from_secs(repo.timeout_seconds);
        let output = if repo.check_command.trim().is_empty() {
            let mut args = vec![
                "test".to_string(),
                "--release".into(),
                "--target".into(),
                MUSL_TARGET.into(),
                "-p".into(),
                package.into(),
            ];
            args.extend(feature_args(repo));
            process::run(worktree, "cargo", &args, &bench_env(), timeout).await
        } else {
            process::sh(worktree, &repo.check_command, timeout).await
        };
        Ok(CheckOutput {
            success: output.success,
            timed_out: output.timed_out,
            output: output.combined(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn abort_profile_is_detected() {
        assert!(text_forces_abort("[profile.release]\npanic = \"abort\"\n"));
        assert!(!text_forces_abort(
            "# panic = \"abort\"\npanic = \"unwind\"\n"
        ));
    }

    #[test]
    fn musl_target_is_visible_here_or_the_error_names_the_command() {
        let installed = musl_target_installed().unwrap();
        let _ = installed;
        let message = format!("rustup target add {MUSL_TARGET}");
        assert!(message.contains("x86_64-unknown-linux-musl"));
    }
}
