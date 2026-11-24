//! Inject and remove the measurement guard.
//!
//! The guard is inserted on the same line as the opening brace so later line
//! numbers still match the restored source. The path dependency is added only
//! for the measurement build. `git checkout` puts both back.

use anyhow::{bail, Context, Result};
use std::path::Path;
use syn::spanned::Spanned;
use toml::Value;

const FENCE_CARGO: &str = include_str!("../crates/noctum-fence/Cargo.toml");
const FENCE_LIB: &str = include_str!("../crates/noctum-fence/src/lib.rs");
pub const GUARD: &str = " let _noctum_guard = ::noctum_fence::guard();";

pub fn strip_guard(source: &str) -> String {
    source.replace(GUARD, "")
}

pub fn install_fence_crate(data_dir: &Path) -> Result<std::path::PathBuf> {
    let dest = data_dir.join("fence").join("noctum-fence");
    std::fs::create_dir_all(dest.join("src"))?;
    std::fs::write(dest.join("Cargo.toml"), FENCE_CARGO)?;
    std::fs::write(dest.join("src/lib.rs"), FENCE_LIB)?;
    Ok(dest)
}

pub fn inject_guards(source: &str, fn_paths: &[String]) -> Result<String> {
    let file = syn::parse_file(source).context("parse rust source")?;
    let mut inserts = Vec::new();
    collect_inserts(&file.items, "", fn_paths, &mut inserts);
    if inserts.len() != fn_paths.len() {
        let found = inserts.len();
        bail!("found {found} fence sites, wanted {}", fn_paths.len());
    }
    inserts.sort_by_key(|pair| std::cmp::Reverse(pair.0));
    let mut out = source.to_string();
    for (offset, _) in inserts {
        let at = offset + 1;
        if !out.is_char_boundary(at) {
            bail!("fence insert is not on a char boundary");
        }
        out.insert_str(at, GUARD);
    }
    Ok(out)
}

fn collect_inserts(
    items: &[syn::Item],
    prefix: &str,
    wanted: &[String],
    out: &mut Vec<(usize, String)>,
) {
    for item in items {
        match item {
            syn::Item::Fn(func) => {
                let path = join(prefix, &func.sig.ident.to_string());
                if wanted.iter().any(|name| name == &path) {
                    let start = func.block.span().byte_range().start;
                    out.push((start, path));
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    collect_inserts(
                        nested,
                        &join(prefix, &module.ident.to_string()),
                        wanted,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

pub fn add_fence_dependency(manifest: &str, fence_dir: &Path) -> Result<String> {
    add_fence_dependency_with_memory(manifest, fence_dir, false)
}

pub fn add_fence_dependency_with_memory(
    manifest: &str,
    fence_dir: &Path,
    memory: bool,
) -> Result<String> {
    let mut value: Value = toml::from_str(manifest).context("parse Cargo.toml")?;
    let table = value.as_table_mut().context("Cargo.toml is not a table")?;
    let dev = table
        .entry("dev-dependencies")
        .or_insert_with(|| Value::Table(toml::map::Map::new()));
    let dev_table = dev
        .as_table_mut()
        .context("dev-dependencies is not a table")?;
    let mut dep = toml::map::Map::new();
    dep.insert(
        "path".into(),
        Value::String(fence_dir.to_string_lossy().to_string()),
    );
    if memory {
        dep.insert(
            "features".into(),
            Value::Array(vec![Value::String("memory".into())]),
        );
    }
    dev_table.insert("noctum-fence".into(), Value::Table(dep));
    toml::to_string(&value).context("write Cargo.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_is_the_first_statement_and_does_not_add_a_line() {
        let src = "fn encode() {}\n#[test]\nfn encode_batch() {\n    let x = 1;\n}\n";
        let out = inject_guards(src, &["encode_batch".into()]).unwrap();
        assert!(out.contains("_noctum_guard"));
        assert!(out.find("_noctum_guard").unwrap() < out.find("let x = 1").unwrap());
        assert_eq!(src.lines().count(), out.lines().count());
        assert!(!out.contains("fn encode() { let _noctum_guard"));
    }

    #[test]
    fn missing_function_is_an_error() {
        let src = "fn other() {}\n";
        assert!(inject_guards(src, &["encode_batch".into()]).is_err());
    }

    #[test]
    fn dependency_is_a_path_dev_dependency() {
        let manifest = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";
        let edited = add_fence_dependency(manifest, Path::new("/data/fence/noctum-fence")).unwrap();
        assert!(edited.contains("noctum-fence"));
        assert!(edited.contains("/data/fence/noctum-fence"));
        assert!(edited.contains("dev-dependencies"));
    }

    #[test]
    fn installed_crate_contains_the_arm64_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = install_fence_crate(dir.path()).unwrap();
        let lib = std::fs::read_to_string(path.join("src/lib.rs")).unwrap();
        assert!(lib.contains(".inst 0xd289c63f"));
    }
}
