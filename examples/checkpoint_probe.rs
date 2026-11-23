//! Check guest counts from a saved state or a fresh boot.

use anyhow::{bail, Context, Result};
use noctum::ports::{MeasureRequest, Measurer};
use noctum::runtime::{ContainerSession, Execution, RuntimeMeasurer};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let data = PathBuf::from(std::env::args_os().nth(1).context("set a data directory")?);
    std::fs::create_dir_all(&data)?;
    let data = data.canonicalize()?;
    let repository = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .map(|p| p.canonicalize())
        .transpose()?;
    let test_name = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "memchr::tests::forward1_oneshot".into());
    let precise = std::env::var_os("NOCTUM_CHECKPOINT_PRECISE").is_some();
    noctum::runtime::prepare_image(&data).await?;
    let source = data.join("checkpoint-probe");
    let fence = noctum::fence::install_fence_crate(&data)?;
    if let Some(repository) = &repository {
        noctum::worktree::snapshot(repository, &source).await?;
    } else {
        std::fs::create_dir_all(source.join("src"))?;
        std::fs::write(source.join("Cargo.toml"), format!("[package]\nname = \"checkpoint-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nnoctum-fence = {{ path = {} }}\n", serde_json::to_string(&fence)?))?;
        std::fs::write(
            source.join("src/main.rs"),
            r#"
fn main() {
    let bound: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    let _guard = noctum_fence::guard();
    let mut total = 0u64;
    for index in 0..bound {
        let values = vec![std::hint::black_box(index as u64); index % 73 + 1];
        total = std::hint::black_box(total ^ values.iter().sum::<u64>());
    }
    println!("{total}");
}
"#,
        )?;
    }
    let script = source.join("checkpoint.py");
    std::fs::write(
        &script,
        include_str!("../experiments/compatibility/checkpoint.py"),
    )?;
    let session =
        ContainerSession::start(&data, &repository.into_iter().collect::<Vec<_>>()).await?;
    let result = async {
        let execution = Execution::Container(session.clone());
        let target = execution.target().triple();
        if precise {
            let header = std::fs::read(
                std::env::var_os("NOCTUM_CHECKPOINT_HEADER")
                    .context("set NOCTUM_CHECKPOINT_HEADER to the QEMU 9.2 header")?,
            )?;
            if noctum::hashutil::sha256_bytes(&header)
                != "e4c5bf7e92075d7c1e6b1dcb6164bca6c549b2912a62b56ce1a777be4c7b8788"
            {
                bail!("QEMU plugin header hash differs");
            }
            std::fs::write(source.join("qemu-plugin.h"), header)?;
            std::fs::write(
                source.join("profile-insn.c"),
                include_str!("../qemu/plugin/noctum_profile.c")
                    .replace("if (!has_fence) {", "if (0 && !has_fence) {"),
            )?;
            let flags = execution
                .run(
                    &source,
                    "pkg-config",
                    &["--cflags", "--libs", "glib-2.0"],
                    &[],
                    Duration::from_secs(10),
                )
                .await;
            if !flags.success {
                bail!("{}", flags.combined());
            }
            let mut args = vec![
                "-shared",
                "-fPIC",
                "-fvisibility=hidden",
                "-O2",
                "-Wall",
                "-Wextra",
                "-I.",
                "profile-insn.c",
                "-o",
                "profile-insn.so",
            ];
            args.extend(flags.stdout.split_whitespace());
            let compiled = execution
                .run(&source, "gcc", &args, &[], Duration::from_secs(30))
                .await;
            if !compiled.success {
                bail!("{}", compiled.combined());
            }
        }
        eprintln!("Build test workload");
        let (harness, guest_args) = if source.join("noctum.toml").is_file() {
            let config = noctum::repo_config::RepoConfig::load(&source)?;
            let package = noctum::builder::resolve_package_with(
                &execution,
                &source,
                config.package.as_deref(),
            )
            .await?;
            let target = package
                .targets
                .iter()
                .find(|t| t.kind == "lib")
                .context("library target is missing")?;
            let test = noctum::selector::discover_harness(&target.src_path)
                .into_iter()
                .find(|t| t.fn_path == test_name)
                .context("test source is missing")?;
            let original = std::fs::read_to_string(&test.file)?;
            std::fs::write(
                &test.file,
                noctum::fence::inject_guards(&original, &[test.file_fn_path])?,
            )?;
            let manifest = std::fs::read_to_string(&package.manifest)?;
            std::fs::write(
                &package.manifest,
                noctum::fence::add_fence_dependency(&manifest, &fence)?,
            )?;
            let built = noctum::builder::build_test_targets_with(
                &execution,
                &source,
                &package.name,
                &config,
                std::slice::from_ref(target),
            )
            .await?;
            (
                built[0].executable.clone(),
                vec![test_name, "--exact".into(), "--test-threads=1".into()],
            )
        } else {
            let built = execution
                .run(
                    &source,
                    "cargo",
                    &["build", "--release", "--target", target],
                    &noctum::process::bench_env(),
                    Duration::from_secs(180),
                )
                .await;
            if !built.success {
                bail!("{}", built.combined());
            }
            (
                source.join(format!("target/{target}/release/checkpoint-probe")),
                vec!["10000".into()],
            )
        };
        let guest_exe = format!("/work/{}", harness.strip_prefix(&source)?.display());
        std::fs::write(
            source.join("checkpoint-args.json"),
            serde_json::to_vec(&guest_args)?,
        )?;
        let measurer = RuntimeMeasurer::Container(session.clone());
        let mut initial = Vec::new();
        for index in 0..2 {
            eprintln!("Run initial measurement {}", index + 1);
            let measured = measurer
                .measure(&MeasureRequest {
                    worktree: source.clone(),
                    working_directory: None,
                    harness: harness.clone(),
                    guest_exe: guest_exe.clone(),
                    args: guest_args.clone(),
                    timeout_secs: 120,
                    scratch: source.join(format!("initial-{index}")),
                    memory_mb: 512,
                })
                .await?;
            let application = measured
                .dump
                .blocks
                .iter()
                .filter(|(pc, _)| *pc >= measured.map_start && *pc < measured.map_end)
                .map(|(_, count)| *count)
                .sum::<u64>();
            initial.push((measured.total, application));
        }
        std::fs::write(
            source.join("initial.json"),
            serde_json::to_vec_pretty(&initial)?,
        )?;
        eprintln!("Initial counts: {initial:?}. Check the guest checkpoint.");
        let qemu = std::env::var("NOCTUM_CHECKPOINT_QEMU")
            .unwrap_or_else(|_| "/opt/noctum/guest/qemu-system-aarch64".into());
        let checked = execution
            .run(
                &source,
                "python3",
                &[
                    script.to_str().unwrap(),
                    source.to_str().unwrap(),
                    harness.to_str().unwrap(),
                ],
                &[
                    ("NOCTUM_CHECKPOINT_QEMU", qemu.as_str()),
                    ("NOCTUM_CHECKPOINT_PRECISE", if precise { "1" } else { "0" }),
                    (
                        "NOCTUM_CHECKPOINT_COLD",
                        if std::env::var_os("NOCTUM_CHECKPOINT_COLD").is_some() {
                            "1"
                        } else {
                            "0"
                        },
                    ),
                    (
                        "NOCTUM_CHECKPOINT_SLEEP_OFF",
                        if std::env::var_os("NOCTUM_CHECKPOINT_SLEEP_OFF").is_some() {
                            "1"
                        } else {
                            "0"
                        },
                    ),
                    (
                        "NOCTUM_CHECKPOINT_RAM",
                        if std::env::var_os("NOCTUM_CHECKPOINT_RAM").is_some() {
                            "1"
                        } else {
                            "0"
                        },
                    ),
                ],
                Duration::from_secs(300),
            )
            .await;
        print!("{}", checked.stdout);
        if !checked.success {
            bail!("{}", checked.stderr);
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let stopped = session.stop().await;
    result.and(stopped)
}
