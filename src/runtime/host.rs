use super::protocol::Reply;
use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct Host(Arc<Connection>);

#[derive(Debug)]
struct Connection {
    input: Mutex<Option<ChildStdin>>,
    output: Mutex<BufReader<ChildStdout>>,
    child: Mutex<Child>,
    gate: Mutex<()>,
    closed: AtomicBool,
}

pub fn executable() -> Result<PathBuf> {
    let path = if let Some(path) = std::env::var_os("NOCTUM_HOST_HELPER") {
        PathBuf::from(path)
    } else {
        std::env::current_exe()?
            .parent()
            .context("Noctum has no installation directory")?
            .join("noctum-host")
    };
    if !path.is_file() {
        bail!("Noctum host helper is missing at {}. Run sh scripts/build-host.sh release, or install Noctum again.", path.display());
    }
    Ok(path)
}

impl Host {
    pub async fn start(log: Option<&Path>) -> Result<Self> {
        let mut command = Command::new(executable()?);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        if let Some(log) = log {
            command.stderr(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)?,
            );
        } else {
            command.stderr(Stdio::inherit());
        }
        let mut child = command.spawn().context("start the Noctum host helper")?;
        let host = Self(Arc::new(Connection {
            input: Mutex::new(child.stdin.take()),
            output: Mutex::new(BufReader::new(
                child
                    .stdout
                    .take()
                    .context("helper response pipe is missing")?,
            )),
            child: Mutex::new(child),
            gate: Mutex::new(()),
            closed: AtomicBool::new(false),
        }));
        let hello: Value = host
            .call(
                serde_json::json!({"operation": "hello"}),
                Duration::from_secs(15),
            )
            .await?;
        if hello["protocolVersion"] != 1 || hello["sdkVersion"] != "0.57.0" {
            host.close().await?;
            bail!("Noctum host helper has a different protocol or SDK version. Build or install both Noctum binaries together.");
        }
        Ok(host)
    }

    pub async fn call<R: DeserializeOwned>(&self, request: Value, timeout: Duration) -> Result<R> {
        let _gate = self.0.gate.lock().await;
        if self.0.closed.load(Ordering::SeqCst) {
            bail!("Noctum host helper is closed");
        }
        let operation = async {
            let mut bytes = serde_json::to_vec(&request)?;
            bytes.push(b'\n');
            {
                let mut input = self.0.input.lock().await;
                let input = input.as_mut().context("helper request pipe is closed")?;
                input.write_all(&bytes).await?;
                input.flush().await?;
            }
            let mut line = String::new();
            if self.0.output.lock().await.read_line(&mut line).await? == 0 {
                bail!("Noctum host helper closed its response pipe");
            }
            let reply: Reply<R> =
                serde_json::from_str(&line).context("read Noctum host helper response")?;
            if let Some(error) = reply.error {
                bail!("{error}");
            }
            reply.value.context("Noctum host helper returned no value")
        };
        match tokio::time::timeout(timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                self.close().await?;
                bail!(
                    "Noctum host operation timed out after {} seconds",
                    timeout.as_secs()
                );
            }
        }
    }

    pub async fn close(&self) -> Result<()> {
        if self.0.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.0.input.lock().await.take();
        let mut child = self.0.child.lock().await;
        match tokio::time::timeout(Duration::from_secs(25), child.wait()).await {
            Ok(result) => {
                let status = result.context("wait for the Noctum host helper")?;
                if !status.success() {
                    bail!("Noctum host helper failed: {status}");
                }
            }
            Err(_) => {
                child.kill().await.context("stop the Noctum host helper")?;
                child.wait().await?;
                bail!("Noctum host helper did not stop after its request pipe closed");
            }
        }
        Ok(())
    }
}
