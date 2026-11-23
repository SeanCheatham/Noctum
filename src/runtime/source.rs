use anyhow::Result;
use rust_embed::Embed;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Embed)]
#[folder = "src/"]
#[include = "**/*.rs"]
struct Sources;
#[derive(Embed)]
#[folder = "templates/"]
#[include = "**/*.html"]
struct Templates;
#[derive(Embed)]
#[folder = "static/"]
#[include = "**/*.css"]
#[include = "**/*.js"]
#[include = "**/*.png"]
#[include = "**/*.ico"]
#[include = "**/*.webmanifest"]
#[include = "**/*.svg"]
struct Static;

pub fn files() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for name in Sources::iter() {
        files.insert(
            format!("src/{name}"),
            Sources::get(&name).unwrap().data.to_vec(),
        );
    }
    for name in Templates::iter() {
        files.insert(
            format!("templates/{name}"),
            Templates::get(&name).unwrap().data.to_vec(),
        );
    }
    for name in Static::iter() {
        files.insert(
            format!("static/{name}"),
            Static::get(&name).unwrap().data.to_vec(),
        );
    }
    macro_rules! add { ($($name:literal),* $(,)?) => { $(files.insert($name.into(), include_bytes!(concat!("../../", $name)).to_vec());)* }; }
    add!(
        "Cargo.toml",
        "Cargo.lock",
        "model.lock.toml",
        "runtime/build-worker.sh",
        "runtime/assets.lock.toml",
        "grammar/patch_schema.json",
        "crates/noctum-fence/Cargo.toml",
        "crates/noctum-fence/src/lib.rs",
        "qemu/build-bundle.sh",
        "qemu/kernel-arm64.fragment",
        "qemu/busybox.fragment",
        "qemu/plugin/noctum_profile.c",
        "qemu/runner/runner.c",
        "qemu/init/init",
        "qemu/fixtures/observed/src/lib.rs",
        "qemu/fixtures/harness/src/lib.rs"
    );
    files
}

pub fn identity() -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for (name, bytes) in files() {
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}

pub fn write(directory: &Path) -> Result<()> {
    for (name, bytes) in files() {
        let path = directory.join(&name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, bytes)?;
        #[cfg(unix)]
        if matches!(name.as_str(), "qemu/build-bundle.sh" | "qemu/init/init") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                directory.join(&name),
                std::fs::Permissions::from_mode(0o755),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_context_contains_worker_inputs() {
        let files = super::files();
        for name in [
            "Cargo.toml",
            "src/bin/noctum-worker.rs",
            "grammar/patch_schema.json",
            "static/favicon.ico",
            "qemu/build-bundle.sh",
        ] {
            assert!(
                files.get(name).is_some_and(|bytes| !bytes.is_empty()),
                "missing {name}"
            );
        }
        assert!(files
            .keys()
            .all(|name| !name.starts_with("target/") && !name.ends_with(".gguf")));
    }
}
