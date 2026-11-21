//! Reject a model diff that touches the wrong files, deletes an assertion,
//! or lowers a fixed loop bound in the measured test.

use std::collections::HashMap;
use std::path::{Component, Path};

use syn::visit::Visit;
use syn::{Expr, ExprForLoop, ItemFn, Lit};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Add(String),
    Del(String),
}

pub fn check_diff(
    diff: &str,
    files: &HashMap<String, String>,
    test_file: Option<&str>,
    test_name: &str,
    protected: &[String],
) -> Result<(), String> {
    let parsed = parse_diff(diff)?;
    if parsed.is_empty() || !diff_changes_source(&parsed) {
        return Err("diff is empty".into());
    }
    for file in &parsed {
        check_path(&file.path, protected)?;
    }
    if let Some(test_file) = test_file {
        if let Some(file) = parsed.iter().find(|file| file.path == test_file) {
            let original = files
                .get(test_file)
                .ok_or_else(|| format!("test file {test_file} is not in the tree"))?;
            let patched = apply_hunks(original, &file.hunks)?;
            check_test_edits(original, &patched, test_name)?;
        }
    }
    Ok(())
}

fn diff_changes_source(files: &[FileDiff]) -> bool {
    files.iter().any(|file| {
        file.lines_changed()
            .any(|line| matches!(line, DiffLine::Add(_) | DiffLine::Del(_)))
    })
}

impl FileDiff {
    fn lines_changed(&self) -> impl Iterator<Item = &DiffLine> {
        self.hunks.iter().flat_map(|hunk| hunk.lines.iter())
    }
}

fn check_path(path: &str, protected: &[String]) -> Result<(), String> {
    if path.is_empty() || path == "/dev/null" {
        return Err("diff has no path".into());
    }
    if path.starts_with('/') {
        return Err(format!("{path} is absolute"));
    }
    let as_path = Path::new(path);
    if as_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("{path} escapes the worktree"));
    }
    if path.split('/').any(|part| part == ".git") {
        return Err(format!("{path} touches .git"));
    }
    let name = as_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "noctum.toml" | "model.lock.toml" | "qemu.lock.toml"
    ) {
        return Err(format!("{path} is protected"));
    }
    for pattern in protected {
        if glob_match::glob_match(pattern, path) {
            return Err(format!("{path} matches protected path {pattern}"));
        }
    }
    if path.starts_with("target/") || path.contains("/target/") {
        return Err(format!("{path} is under target/"));
    }
    Ok(())
}

fn check_test_edits(before: &str, after: &str, test_name: &str) -> Result<(), String> {
    let before_fn = find_fn(before, test_name)?;
    let after_fn = find_fn(after, test_name)?;
    let before_asserts = count_asserts(&before_fn);
    let after_asserts = count_asserts(&after_fn);
    if after_asserts < before_asserts {
        return Err("diff removes an assertion from the measured test".into());
    }
    let before_bounds = loop_bounds(&before_fn);
    let after_bounds = loop_bounds(&after_fn);
    if !bounds_hold(&before_bounds, &after_bounds) {
        return Err("diff lowers a fixed loop bound in the measured test".into());
    }
    Ok(())
}

fn bounds_hold(before: &[u128], after: &[u128]) -> bool {
    let mut used = vec![false; after.len()];
    for bound in before {
        let Some(index) = after
            .iter()
            .enumerate()
            .position(|(index, value)| !used[index] && *value >= *bound)
        else {
            return false;
        };
        used[index] = true;
    }
    true
}

pub fn parse_diff(diff: &str) -> Result<Vec<FileDiff>, String> {
    let mut files = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    for line in diff.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            push_hunk(&mut current, &mut hunk);
            push_file(&mut files, &mut current);
            let path = git_path(rest)?;
            current = Some(FileDiff {
                path,
                hunks: Vec::new(),
            });
            continue;
        }
        if let Some(rest) = raw.strip_prefix("+++ ") {
            let path = strip_ab(rest);
            if path != "/dev/null" {
                if current.is_none() {
                    current = Some(FileDiff {
                        path: path.to_string(),
                        hunks: Vec::new(),
                    });
                } else if let Some(file) = current.as_mut() {
                    if file.path.is_empty() || file.path == "/dev/null" {
                        file.path = path.to_string();
                    }
                }
            }
            continue;
        }
        if raw.starts_with("--- ")
            || raw.starts_with("index ")
            || raw.starts_with("new file")
            || raw.starts_with("deleted file")
            || raw.starts_with("similarity ")
            || raw.starts_with("rename ")
            || raw.starts_with("old mode")
            || raw.starts_with("new mode")
        {
            continue;
        }
        if let Some(header) = raw.strip_prefix("@@ ") {
            push_hunk(&mut current, &mut hunk);
            let old_start = parse_hunk_start(header)?;
            hunk = Some(Hunk {
                old_start,
                lines: Vec::new(),
            });
            continue;
        }
        if raw.starts_with('\\') {
            continue;
        }
        let Some(hunk) = hunk.as_mut() else {
            if raw.is_empty() {
                continue;
            }
            return Err(format!("diff line outside a hunk: {raw}"));
        };
        if let Some(text) = raw.strip_prefix('+') {
            hunk.lines.push(DiffLine::Add(text.to_string()));
        } else if let Some(text) = raw.strip_prefix('-') {
            hunk.lines.push(DiffLine::Del(text.to_string()));
        } else if let Some(text) = raw.strip_prefix(' ') {
            hunk.lines.push(DiffLine::Context(text.to_string()));
        } else if raw.is_empty() {
            hunk.lines.push(DiffLine::Context(String::new()));
        } else {
            return Err(format!("cannot parse diff line: {raw}"));
        }
    }
    push_hunk(&mut current, &mut hunk);
    push_file(&mut files, &mut current);
    if files.is_empty() {
        return Err("diff is empty".into());
    }
    Ok(files)
}

fn push_hunk(current: &mut Option<FileDiff>, hunk: &mut Option<Hunk>) {
    if let (Some(file), Some(hunk)) = (current.as_mut(), hunk.take()) {
        file.hunks.push(hunk);
    }
}

fn push_file(files: &mut Vec<FileDiff>, current: &mut Option<FileDiff>) {
    if let Some(file) = current.take() {
        files.push(file);
    }
}

fn git_path(header: &str) -> Result<String, String> {
    let mut parts = header.split_whitespace();
    let _old = parts.next();
    let new = parts.next().ok_or("diff --git is missing a path")?;
    let path = strip_ab(new);
    if path == "/dev/null" {
        return Err("diff deletes a file without a destination".into());
    }
    Ok(path.to_string())
}

fn strip_ab(path: &str) -> &str {
    let path = path.trim_matches('"');
    path.strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .unwrap_or(path)
}

fn parse_hunk_start(header: &str) -> Result<usize, String> {
    let spec = header
        .split_whitespace()
        .next()
        .ok_or("hunk header is empty")?;
    let old = spec
        .trim_start_matches('-')
        .split(',')
        .next()
        .ok_or("hunk header has no old start")?;
    old.parse::<usize>()
        .map_err(|_| format!("bad hunk header: {header}"))
}

pub fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, String> {
    let keep_newline = original.ends_with('\n');
    let mut lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        let mut split: Vec<String> = original.split('\n').map(str::to_string).collect();
        if keep_newline {
            split.pop();
        }
        split
    };
    // Models mis-number hunks. The changed lines are the proposal, so a hunk
    // that does not sit on its header line is applied where its old lines match.
    let mut floor = 0usize;
    for hunk in hunks {
        let mut cursor = locate_hunk(&lines, floor, hunk)?;
        for line in &hunk.lines {
            match line {
                DiffLine::Context(text) | DiffLine::Del(text) => {
                    if lines.get(cursor).map(String::as_str) != Some(text.as_str()) {
                        return Err(format!(
                            "hunk does not match at line {}: expected {text:?}",
                            cursor + 1
                        ));
                    }
                    if matches!(line, DiffLine::Del(_)) {
                        lines.remove(cursor);
                    } else {
                        cursor += 1;
                    }
                }
                DiffLine::Add(text) => {
                    lines.insert(cursor, text.clone());
                    cursor += 1;
                }
            }
        }
        floor = cursor;
    }
    let mut out = lines.join("\n");
    if keep_newline || original.is_empty() && out.is_empty() {
        if keep_newline {
            out.push('\n');
        }
    } else if keep_newline {
        out.push('\n');
    }
    if keep_newline && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn old_lines(hunk: &Hunk) -> Vec<&str> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            DiffLine::Context(text) | DiffLine::Del(text) => Some(text.as_str()),
            DiffLine::Add(_) => None,
        })
        .collect()
}

fn locate_hunk(lines: &[String], floor: usize, hunk: &Hunk) -> Result<usize, String> {
    let old = old_lines(hunk);
    let hint = if hunk.old_start == 0 {
        0
    } else {
        hunk.old_start.saturating_sub(1)
    };
    if old.is_empty() {
        let end = lines.len();
        return Ok(hint.clamp(floor.min(end), end));
    }
    let fits = |start: usize| {
        start >= floor
            && start + old.len() <= lines.len()
            && old
                .iter()
                .enumerate()
                .all(|(index, text)| lines[start + index] == *text)
    };
    if fits(hint) {
        return Ok(hint);
    }
    (floor..=lines.len().saturating_sub(old.len()))
        .find(|start| fits(*start))
        .ok_or_else(|| {
            format!(
                "hunk does not match at line {}: expected {:?}",
                hint + 1,
                old.first().copied().unwrap_or("")
            )
        })
}

fn find_fn(src: &str, test_name: &str) -> Result<ItemFn, String> {
    let file = syn::parse_file(src).map_err(|err| format!("parse test file: {err}"))?;
    let mut found = Vec::new();
    collect_fns(&file.items, "", &mut found);
    let leaf = test_name.rsplit("::").next().unwrap_or(test_name);
    if let Some((_, func)) = found.iter().find(|(path, _)| path == test_name) {
        return Ok((*func).clone());
    }
    let matches: Vec<_> = found
        .iter()
        .filter(|(path, _)| path == leaf || path.ends_with(&format!("::{leaf}")))
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0].1.clone());
    }
    if matches.is_empty() {
        Err(format!(
            "measured test {test_name} was not found after the diff"
        ))
    } else {
        Err(format!(
            "measured test {test_name} is ambiguous after the diff"
        ))
    }
}

fn collect_fns<'a>(items: &'a [syn::Item], prefix: &str, out: &mut Vec<(String, &'a ItemFn)>) {
    for item in items {
        match item {
            syn::Item::Fn(func) => {
                let path = join(prefix, &func.sig.ident.to_string());
                out.push((path, func));
            }
            syn::Item::Mod(module) => {
                let next = join(prefix, &module.ident.to_string());
                if let Some((_, items)) = &module.content {
                    collect_fns(items, &next, out);
                }
            }
            syn::Item::Impl(item) => {
                for impl_item in &item.items {
                    if let syn::ImplItem::Fn(func) = impl_item {
                        let path = join(prefix, &func.sig.ident.to_string());
                        // Impl methods are not test functions. Skip them here.
                        let _ = path;
                    }
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

fn count_asserts(func: &ItemFn) -> usize {
    let mut visitor = AssertVisitor { count: 0 };
    visitor.visit_block(&func.block);
    visitor.count
}

fn loop_bounds(func: &ItemFn) -> Vec<u128> {
    let mut visitor = BoundVisitor { bounds: Vec::new() };
    visitor.visit_block(&func.block);
    visitor.bounds
}

struct AssertVisitor {
    count: usize,
}

impl<'ast> Visit<'ast> for AssertVisitor {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if is_assert(&mac.path) {
            self.count += 1;
        }
        syn::visit::visit_macro(self, mac);
    }
}

struct BoundVisitor {
    bounds: Vec<u128>,
}

impl<'ast> Visit<'ast> for BoundVisitor {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Expr::ForLoop(ExprForLoop { expr: iterator, .. }) = expr {
            if let Some(bound) = range_end(iterator) {
                self.bounds.push(bound);
            }
        }
        syn::visit::visit_expr(self, expr);
    }
}

fn is_assert(path: &syn::Path) -> bool {
    let Some(name) = path.segments.last() else {
        return false;
    };
    matches!(
        name.ident.to_string().as_str(),
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
    )
}

fn range_end(expr: &Expr) -> Option<u128> {
    let Expr::Range(range) = expr else {
        return None;
    };
    int_lit(range.end.as_deref()?)
}

fn int_lit(expr: &Expr) -> Option<u128> {
    let Expr::Lit(lit) = expr else {
        return None;
    };
    let Lit::Int(value) = &lit.lit else {
        return None;
    };
    value.base10_parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"
fn encode(input: &[u8]) -> usize { input.len() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_batch() {
        let mut n = 0u64;
        for _ in 0..10_000 {
            n = n.wrapping_add(encode(b"abcdefghijklmnopqrstuvwxyz").len() as u64);
        }
        assert_eq!(n, 26 * 10_000);
    }
}
"#;

    fn files() -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert("src/lib.rs".to_string(), SOURCE.to_string());
        map
    }

    #[test]
    fn allows_a_comment_in_production_code() {
        let diff = r#"diff --git a/src/encode.rs b/src/encode.rs
--- a/src/encode.rs
+++ b/src/encode.rs
@@ -1,2 +1,3 @@
 fn encode(input: &[u8]) -> usize { input.len() }
+// NOCTUM_OPT
 "#;
        let mut map = HashMap::new();
        map.insert(
            "src/encode.rs".to_string(),
            "fn encode(input: &[u8]) -> usize { input.len() }\n".into(),
        );
        check_diff(diff, &map, Some("src/lib.rs"), "tests::encode_batch", &[]).unwrap();
    }

    #[test]
    fn rejects_manifests_protected_paths_and_escapes() {
        let bad = [
            "diff --git a/Cargo.toml b/Cargo.toml\n--- a/Cargo.toml\n+++ b/Cargo.toml\n@@ -1 +1,2 @@\n [package]\n+name = \"x\"\n",
            "diff --git a/noctum.toml b/noctum.toml\n--- a/noctum.toml\n+++ b/noctum.toml\n@@ -1 +1,2 @@\n package = \"demo\"\n+x = 1\n",
            "diff --git a/../etc/passwd b/../etc/passwd\n--- a/../etc/passwd\n+++ b/../etc/passwd\n@@ -1 +1,2 @@\n root\n+x\n",
            "diff --git a/fixtures/sample.bin b/fixtures/sample.bin\n--- a/fixtures/sample.bin\n+++ b/fixtures/sample.bin\n@@ -1 +1,2 @@\n a\n+b\n",
        ];
        for diff in bad {
            assert!(check_diff(
                diff,
                &files(),
                None,
                "encode_batch",
                &["fixtures/**".to_string()]
            )
            .is_err());
        }
    }

    #[test]
    fn rejects_a_deleted_assertion_and_a_lowered_bound() {
        let original = SOURCE;
        let dropped = original.replace("assert_eq!(n, 26 * 10_000);", "let _ = n;");
        let lowered = original.replace("0..10_000", "0..10");
        assert!(check_test_edits(original, &dropped, "tests::encode_batch").is_err());
        assert!(check_test_edits(original, &lowered, "tests::encode_batch").is_err());
        assert!(check_test_edits(original, original, "tests::encode_batch").is_ok());
    }

    #[test]
    fn empty_diff_is_rejected() {
        let error = check_diff("", &files(), None, "encode_batch", &[]).unwrap_err();
        assert!(error.contains("empty"), "{error}");
    }

    #[test]
    fn a_misnumbered_hunk_is_applied_where_the_old_lines_match() {
        let original = "alpha\nbeta\ngamma\n";
        let hunks = vec![Hunk {
            old_start: 40,
            lines: vec![
                DiffLine::Context("alpha".into()),
                DiffLine::Del("beta".into()),
                DiffLine::Add("BETA".into()),
                DiffLine::Context("gamma".into()),
            ],
        }];
        let patched = apply_hunks(original, &hunks).unwrap();
        assert_eq!(patched, "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn an_off_by_one_second_hunk_still_applies() {
        let original = "alpha\nbeta\n\nfn test() {\n    let n = 1;\n}\n";
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
 alpha
-beta
+BETA

@@ -90,3 +90,3 @@
 fn test() {
-    let n = 1;
+    let n = 2;
 }
"#;
        let parsed = parse_diff(diff).unwrap();
        let patched = apply_hunks(original, &parsed[0].hunks).unwrap();
        assert_eq!(patched, "alpha\nBETA\n\nfn test() {\n    let n = 2;\n}\n");
    }

    #[test]
    fn a_misnumbered_hunk_still_rejects_a_lowered_bound() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -80,6 +80,6 @@
     fn encode_batch() {
         let mut n = 0u64;
-        for _ in 0..10_000 {
+        for _ in 0..10 {
             n = n.wrapping_add(encode(b"abcdefghijklmnopqrstuvwxyz").len() as u64);
         }
         assert_eq!(n, 26 * 10_000);
     }
"#;
        let err = check_diff(
            diff,
            &files(),
            Some("src/lib.rs"),
            "tests::encode_batch",
            &[],
        )
        .unwrap_err();
        assert!(err.contains("loop bound"), "{err}");
    }
}
