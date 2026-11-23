use super::{host::Host, source};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    image: String,
    init: String,
    kernel_url: String,
    kernel_archive_sha256: String,
    kernel_member: String,
    kernel_sha256: String,
}

fn lock() -> Result<Lock> {
    Ok(toml::from_str(include_str!(
        "../../runtime/assets.lock.toml"
    ))?)
}

pub fn lease(data: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(data.join("runtime"))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("runtime/worker.lock"))?;
    file.try_lock()
        .context("the worker is in use by another Noctum process")?;
    Ok(file)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Assets {
    pub build: String,
    pub kernel: PathBuf,
    pub kernel_sha256: String,
    pub image: String,
    pub store: PathBuf,
    pub rootfs: PathBuf,
    pub init: String,
}

impl Assets {
    pub fn load(data: &Path) -> Result<Self> {
        let path = data.join("runtime/worker.json");
        let assets: Self = serde_json::from_slice(
            &std::fs::read(&path)
                .with_context(|| format!("worker assets are missing: {}", path.display()))?,
        )?;
        let lock = lock()?;
        if assets.build != source::identity()
            || assets.image != lock.image
            || assets.init != lock.init
            || assets.kernel_sha256 != lock.kernel_sha256
        {
            bail!("worker assets have a different version. Run noctum runtime prepare.");
        }
        for path in [&assets.kernel, &assets.rootfs] {
            if !path.is_absolute() || !path.is_file() {
                bail!("worker asset is missing: {}", path.display());
            }
        }
        if crate::hashutil::sha256_file(&assets.kernel)? != assets.kernel_sha256 {
            bail!("worker kernel hash does not match the asset record");
        }
        Ok(assets)
    }
}

pub async fn prepare(data: &Path) -> Result<()> {
    std::fs::create_dir_all(data)?;
    let data = data.canonicalize()?;
    let _lease = lease(&data)?;
    if Assets::load(&data).is_ok() {
        return Ok(());
    }
    crate::process::require_free_space(&data, 12 * 1024 * 1024 * 1024)?;
    let directory = data.join("runtime");
    let lock = lock()?;
    let kernel = directory.join("kernel.bin");
    if crate::hashutil::sha256_file(&kernel).ok().as_deref() != Some(&lock.kernel_sha256) {
        tracing::info!("Download the worker kernel");
        let archive = directory.join("kernel.tar.zst");
        crate::model::download_verified(&lock.kernel_url, &archive, &lock.kernel_archive_sha256)
            .await?;
        let output = tokio::process::Command::new("/usr/bin/tar")
            .args(["-xOf"])
            .arg(&archive)
            .arg(&lock.kernel_member)
            .stdout(std::fs::File::create(&kernel)?)
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?
            .wait_with_output()
            .await?;
        if !output.status.success() {
            bail!(
                "cannot extract the worker kernel: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if crate::hashutil::sha256_file(&kernel)? != lock.kernel_sha256 {
            bail!("worker kernel hash does not match the lockfile");
        }
        std::fs::remove_file(archive)?;
    }
    let build = source::identity();
    let base = crate::hashutil::sha256_bytes(lock.image.as_bytes());
    let rootfs = directory.join(format!("worker-{base}.ext4"));
    let source = tempfile::tempdir_in(&directory)?;
    source::write(source.path())?;
    let assets = Assets {
        build,
        kernel,
        kernel_sha256: lock.kernel_sha256,
        image: lock.image,
        init: lock.init,
        store: directory.join("images"),
        rootfs,
    };
    let host = Host::start(Some(&directory.join("helper.log"))).await?;
    let result = crate::process::watch_disk(async {
        tracing::info!("Download the worker base image");
        if !assets.rootfs.is_file() {
        host.call::<Value>(json!({"operation":"unpack","store":assets.store,"image":assets.image,
            "rootfs":assets.rootfs,"rootfsSizeMb":16384}),Duration::from_secs(1800)).await?;
        }
        host.call::<Value>(json!({"operation":"start","store":assets.store,"image":assets.image,
            "kernel":assets.kernel,"initReference":assets.init,"rootfs":assets.rootfs,
            "network":true,"cpus":2,"memoryMb":3072,"arguments":["/bin/sleep","infinity"],
            "mounts":[{"source":source.path(),"destination":"/host-source","readOnly":true}]}),Duration::from_secs(300)).await?;
        tracing::info!("Build the worker. This can take several minutes.");
        #[derive(Deserialize)]
        struct Output { code:i32, stdout:Vec<u8>, stderr:Vec<u8> }
        let output:Output = host.call(json!({"operation":"exec","arguments":["/bin/sh","/host-source/runtime/build-worker.sh"],
            "diagnostics":true,"timeoutSeconds":7200}),Duration::from_secs(7220)).await?;
        let mut log=output.stdout;
        log.extend_from_slice(&output.stderr);
        std::fs::write(directory.join("build.log"),&log)?;
        if output.code != 0 { bail!("worker build failed. Read {}",directory.join("build.log").display()); }
        host.call::<Value>(json!({"operation":"stop"}),Duration::from_secs(30)).await?;
        Ok(())
    },Duration::from_secs(2),||crate::process::require_free_space(&data,2*1024*1024*1024)).await;
    let closed = host.close().await;
    if result.is_err() || closed.is_err() {
        result?;
        closed?;
    }
    let previous = std::fs::read(directory.join("worker.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Assets>(&bytes).ok());
    let record = directory.join("worker.json.partial");
    std::fs::write(&record, serde_json::to_vec_pretty(&assets)?)?;
    std::fs::rename(record, directory.join("worker.json"))?;
    if let Some(previous) = previous {
        if previous.rootfs != assets.rootfs && previous.rootfs.parent() == Some(directory.as_path())
        {
            let _ = std::fs::remove_file(previous.rootfs);
        }
    }
    Ok(())
}
