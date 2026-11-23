//! Timed process execution. Long commands run in their own process group so a
//! timeout can signal the whole tree.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CmdOutput {
    pub success: bool,
    pub timed_out: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl CmdOutput {
    pub fn combined(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

pub async fn run(
    cwd: &Path,
    program: impl AsRef<OsStr>,
    args: &[impl AsRef<OsStr>],
    env: &[(&str, &str)],
    timeout: Duration,
) -> CmdOutput {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    spawn(command, timeout, None).await
}

pub async fn sh(cwd: &Path, script: &str, timeout: Duration) -> CmdOutput {
    run(cwd, "sh", &["-c", script], &[], timeout).await
}

pub async fn run_input(
    cwd: &Path,
    program: &str,
    args: &[String],
    timeout: Duration,
    input: Vec<u8>,
) -> CmdOutput {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    spawn(command, timeout, Some(input)).await
}

pub async fn run_logged(
    cwd: &Path,
    program: &str,
    args: &[String],
    timeout: Duration,
    log: &Path,
) -> anyhow::Result<CmdOutput> {
    let file = std::fs::File::create(log)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    Ok(spawn(command, timeout, None).await)
}

pub async fn run_logged_with_disk_limit(
    cwd: &Path,
    program: &str,
    args: &[String],
    timeout: Duration,
    log: &Path,
    disk: &Path,
) -> anyhow::Result<CmdOutput> {
    require_free_space(disk, 12 * 1024 * 1024 * 1024)?;
    watch_disk(
        run_logged(cwd, program, args, timeout, log),
        Duration::from_secs(2),
        || require_free_space(disk, 2 * 1024 * 1024 * 1024),
    )
    .await
}

pub(crate) fn require_free_space(path: &Path, reserve: u64) -> anyhow::Result<()> {
    let free = crate::model::free_bytes(path)?;
    if free < reserve {
        anyhow::bail!("operation needs {reserve} free bytes under {}, have {free}; free disk space before trying again", path.display());
    }
    Ok(())
}

pub(crate) async fn watch_disk<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>>,
    period: Duration,
    mut check: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<T> {
    check()?;
    tokio::pin!(future);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = interval.tick() => check()?,
        }
    }
}

struct SpawnedGroup(u32);
impl Drop for SpawnedGroup {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .args(["-s", "KILL", "--", &format!("-{}", self.0)])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

async fn spawn(mut command: Command, timeout: Duration, input: Option<Vec<u8>>) -> CmdOutput {
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return CmdOutput {
                success: false,
                timed_out: false,
                code: None,
                stdout: String::new(),
                stderr: format!("failed to spawn command: {err}"),
                duration_ms: elapsed(started),
            };
        }
    };
    if let (Some(mut pipe), Some(input)) = (child.stdin.take(), input) {
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = pipe.write_all(&input).await;
        });
    }
    let pid = child.id().unwrap_or(0);
    let group = SpawnedGroup(pid);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Reads run beside the wait. Dropping a Child while a read is still
    // joined to it can stall the runtime, so the child stays in this task.
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let (finished, timed_out) = tokio::select! {
        status = child.wait() => (status, false),
        _ = tokio::time::sleep(timeout) => {
            signal_group(pid, "-KILL");
            let status = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            (status, true)
        }
    };
    drop(group);
    let (out, err) = pipes(stdout_task, stderr_task).await;

    match finished {
        Ok(status) => CmdOutput {
            success: status.success() && !timed_out,
            timed_out,
            code: status.code(),
            stdout: lossy(&out),
            stderr: if timed_out {
                format!("timed out after {} seconds", timeout.as_secs())
            } else {
                lossy(&err)
            },
            duration_ms: elapsed(started),
        },
        Err(err) => CmdOutput {
            success: false,
            timed_out,
            code: None,
            stdout: lossy(&out),
            stderr: if timed_out {
                format!("timed out after {} seconds", timeout.as_secs())
            } else {
                err.to_string()
            },
            duration_ms: elapsed(started),
        },
    }
}

async fn read_pipe(mut pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(&mut buf).await;
    }
    buf
}

async fn pipes(
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
) -> (Vec<u8>, Vec<u8>) {
    let joined = tokio::time::timeout(Duration::from_secs(2), async {
        let out = stdout_task.await.unwrap_or_default();
        let err = stderr_task.await.unwrap_or_default();
        (out, err)
    })
    .await;
    joined.unwrap_or_default()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

pub fn signal_group(pid: u32, signal: &str) {
    if pid == 0 {
        return;
    }
    // A child in our own process group must be signaled by pid. Signaling
    // that group would kill the daemon.
    let own = process_group_id(std::process::id());
    let child_group = process_group_id(pid);
    let target = if child_group == Some(pid) && own != Some(pid) {
        format!("-{pid}")
    } else {
        pid.to_string()
    };
    let name = signal.trim_start_matches('-');
    let _ = std::process::Command::new("kill")
        .args(["-s", name, "--", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "macos")]
fn process_group_id(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(not(target_os = "macos"))]
fn process_group_id(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    let mut fields = after.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    fields.next()?.parse().ok()
}

/// Line tables, one codegen unit, and a frozen timestamp.
///
/// `cargo test --release` selects the release profile. `cargo bench` selects
/// the bench profile. Both are set so the harness stays symbolizable either way.
pub fn bench_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("CARGO_PROFILE_RELEASE_DEBUG", "line-tables-only"),
        ("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1"),
        ("CARGO_PROFILE_RELEASE_STRIP", "none"),
        ("CARGO_PROFILE_BENCH_DEBUG", "line-tables-only"),
        ("CARGO_PROFILE_BENCH_CODEGEN_UNITS", "1"),
        ("CARGO_PROFILE_BENCH_STRIP", "none"),
        ("SOURCE_DATE_EPOCH", "0"),
        ("CARGO_TERM_COLOR", "never"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let output = run(dir.path(), "echo", &["hello"], &[], Duration::from_secs(5)).await;
        assert!(output.success);
        assert_eq!(output.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn passes_request_bytes_and_closes_stdin() {
        let directory = tempfile::tempdir().unwrap();
        let input = b"{\"request\":42}\n".to_vec();
        let output = run_input(
            directory.path(),
            "cat",
            &[],
            Duration::from_secs(5),
            input.clone(),
        )
        .await;
        assert!(output.success, "{}", output.combined());
        assert_eq!(output.stdout.as_bytes(), input);
    }

    #[tokio::test]
    async fn cancellation_kills_child_processes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let task = tokio::spawn(async move {
            run(
                &path,
                "sh",
                &["-c", "sleep 30 & echo $! > child.pid; wait"],
                &[],
                Duration::from_secs(60),
            )
            .await
        });
        for _ in 0..100 {
            if directory.path().join("child.pid").is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = std::fs::read_to_string(directory.path().join("child.pid")).unwrap();
        task.abort();
        let _ = task.await;
        for _ in 0..100 {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("child process {pid} is still running");
    }

    #[tokio::test]
    async fn disk_limit_prevents_a_command_from_starting() {
        let directory = tempfile::tempdir().unwrap();
        let result = watch_disk(
            run_logged(
                directory.path(),
                "touch",
                &["started".into()],
                Duration::from_secs(10),
                &directory.path().join("log"),
            ),
            Duration::from_secs(2),
            || anyhow::bail!("disk reserve reached"),
        )
        .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("disk reserve reached"));
        assert!(!directory.path().join("started").exists());
        assert!(!directory.path().join("log").exists());
    }

    #[tokio::test]
    async fn disk_limit_stops_the_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let args = vec!["-c".into(), "sleep 30 & echo $! > child.pid; wait".into()];
        let result = watch_disk(
            run_logged(
                directory.path(),
                "sh",
                &args,
                Duration::from_secs(60),
                &directory.path().join("log"),
            ),
            Duration::from_millis(10),
            || {
                if directory.path().join("child.pid").is_file() {
                    anyhow::bail!("disk reserve reached");
                }
                Ok(())
            },
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("disk reserve"));
        let pid = std::fs::read_to_string(directory.path().join("child.pid")).unwrap();
        for _ in 0..100 {
            let output = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("child process {pid} is still running");
    }

    #[tokio::test]
    async fn timeout_kills_a_sleep() {
        let dir = tempfile::tempdir().unwrap();
        let output = run(dir.path(), "sleep", &["30"], &[], Duration::from_secs(1)).await;
        assert!(output.timed_out);
        assert!(!output.success);
    }
}
