//! The three edges of a night: measure, infer, and the host check.
//!
//! Production uses QEMU, llama-server, and cargo. Tests substitute the first
//! two. The instruction count still comes from the measurement, never from a
//! constant inside the orchestrator.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::profile::ProfileDump;
use crate::repo_config::RepoConfig;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MeasureRequest {
    pub worktree: std::path::PathBuf,
    pub harness: std::path::PathBuf,
    pub guest_exe: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    pub scratch: std::path::PathBuf,
    pub memory_mb: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Measurement {
    pub total: u64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub wall_ms: u64,
    pub dump: ProfileDump,
    pub map_start: u64,
    pub map_end: u64,
    pub load_bias: u64,
    pub serial_log: String,
}

impl Measurement {
    pub fn matches(&self, other: &Measurement) -> bool {
        self.total == other.total
            && comparable_stdout(&self.stdout) == comparable_stdout(&other.stdout)
            && self.exit_code == other.exit_code
    }
}

/// Libtest prints `finished in 0.01s` on the summary line. Under icount that
/// duration follows the instruction count, so a cheaper body changes the
/// string. The gate compares the result text and leaves the timer out.
pub fn comparable_stdout(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if let Some(index) = line.find("finished in ") {
            if line[..index].contains("test result:") {
                out.push_str(&line[..index]);
                out.push_str("finished in TIME");
                if line.ends_with('\n') {
                    out.push('\n');
                }
                continue;
            }
        }
        out.push_str(line);
    }
    out.into_bytes()
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub summary: String,
    pub diff: String,
}

#[derive(Debug, Clone)]
pub struct CheckOutput {
    pub success: bool,
    pub timed_out: bool,
    pub output: String,
}

pub trait Measurer: Send + Sync {
    fn measure(
        &self,
        req: &MeasureRequest,
    ) -> impl std::future::Future<Output = anyhow::Result<Measurement>> + Send;
}

pub trait Inferencer: Send + Sync {
    fn complete(
        &self,
        messages: &[ChatMessage],
        cancel: &AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<Completion>> + Send;
}

pub trait HostCheck: Send + Sync {
    fn run(
        &self,
        worktree: &Path,
        repo: &RepoConfig,
        package: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<CheckOutput>> + Send;
}

#[cfg(test)]
mod tests {
    use super::comparable_stdout;

    #[test]
    fn libtest_duration_is_not_part_of_the_comparison() {
        let slow = b"test tests::encode_batch ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.42s\n";
        let fast = b"test tests::encode_batch ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        assert_eq!(comparable_stdout(slow), comparable_stdout(fast));
        let failed = b"test tests::encode_batch ... ok\ntest result: ok. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        assert_ne!(comparable_stdout(slow), comparable_stdout(failed));
    }
}
