//! Choose which Rust tests are worth a guest run.
//!
//! The screen is lexical: plain `#[test]` only, no ignore or should_panic,
//! no custom harness attribute, and no test body that names the network, a
//! clock, a process, or a database client.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Expr, Item, ItemFn, Meta};

const DENY: &[&str] = &[
    "std::net",
    "TcpStream",
    "UdpSocket",
    "UnixStream",
    "std::process",
    "Command::new",
    "std::time",
    "Instant::now",
    "SystemTime::now",
    "thread::sleep",
    "tokio::net",
    "reqwest",
    "hyper::",
    "postgres",
    "mysql",
    "redis",
    "mongodb",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredTest {
    pub name: String,
    pub file: PathBuf,
    pub fn_path: String,
    pub body: String,
    pub body_bytes: usize,
    pub source: String,
    /// Set when the test must not be booted. The dashboard stores this text.
    pub skip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSpan {
    pub path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestId {
    pub harness: Option<String>,
    pub name: String,
}

pub fn parse_test_id(raw: &str) -> TestId {
    if let Some((left, right)) = raw.split_once("::") {
        if left.contains('/') || left.starts_with("unittests") {
            return TestId {
                harness: Some(left.to_string()),
                name: right.to_string(),
            };
        }
    }
    TestId {
        harness: None,
        name: raw.to_string(),
    }
}

pub fn logical_harness(package_root: &Path, src_path: &Path, kind: &str) -> String {
    let rel = src_path
        .strip_prefix(package_root)
        .unwrap_or(src_path)
        .to_string_lossy()
        .replace('\\', "/");
    if kind == "test" || rel.starts_with("tests/") {
        rel
    } else {
        format!("unittests {rel}")
    }
}

pub fn discover_harness(root: &Path) -> Vec<DiscoveredTest> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk_file(root, "", &mut out, &mut seen);
    out
}

pub fn classify_source(src: &str) -> Vec<DiscoveredTest> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_items(&file.items, "", Path::new("lib.rs"), src, &mut out);
    out
}

pub fn function_spans(src: &str) -> Vec<FnSpan> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    span_items(&file.items, "", src, &mut out);
    out
}

pub fn direct_callees(src: &str, fn_path: &str) -> Vec<String> {
    let Ok(file) = syn::parse_file(src) else {
        return Vec::new();
    };
    let mut fns = Vec::new();
    collect_fn_refs(&file.items, "", &mut fns);
    let Some(func) = fns.into_iter().find(|(path, _)| path == fn_path) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut visitor = CallVisitor { names: &mut names };
    visitor.visit_block(&func.1.block);
    names.sort();
    names.dedup();
    names
}

pub fn order_for_scan(mut tests: Vec<DiscoveredTest>, max_scan: usize) -> Vec<DiscoveredTest> {
    let (mut skipped, mut keep) = (Vec::new(), Vec::new());
    for test in tests.drain(..) {
        if test.skip.is_some() {
            skipped.push(test);
        } else {
            keep.push(test);
        }
    }
    keep.sort_by(|a, b| b.body_bytes.cmp(&a.body_bytes).then(a.name.cmp(&b.name)));
    keep.truncate(max_scan);
    keep.extend(skipped);
    keep
}

fn walk_file(
    path: &Path,
    prefix: &str,
    out: &mut Vec<DiscoveredTest>,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(canon) = path.canonicalize() else {
        return;
    };
    if !seen.insert(canon) {
        return;
    }
    let Ok(src) = fs::read_to_string(path) else {
        return;
    };
    let Ok(file) = syn::parse_file(&src) else {
        return;
    };
    walk_items_follow(&file.items, prefix, path, &src, out, seen);
}

fn walk_items_follow(
    items: &[Item],
    prefix: &str,
    file: &Path,
    src: &str,
    out: &mut Vec<DiscoveredTest>,
    seen: &mut HashSet<PathBuf>,
) {
    for item in items {
        match item {
            Item::Mod(module) => {
                let next = join(prefix, &module.ident.to_string());
                if let Some((_, nested)) = &module.content {
                    walk_items_follow(nested, &next, file, src, out, seen);
                } else if let Some(child) =
                    resolve_mod(file, &module.ident.to_string(), &module.attrs)
                {
                    walk_file(&child, &next, out, seen);
                }
            }
            Item::Fn(func) => {
                if let Some(test) = test_from_fn(func, prefix, file, src) {
                    out.push(test);
                }
            }
            _ => {}
        }
    }
}

fn walk_items(items: &[Item], prefix: &str, file: &Path, src: &str, out: &mut Vec<DiscoveredTest>) {
    for item in items {
        match item {
            Item::Mod(module) => {
                let next = join(prefix, &module.ident.to_string());
                if let Some((_, nested)) = &module.content {
                    walk_items(nested, &next, file, src, out);
                }
            }
            Item::Fn(func) => {
                if let Some(test) = test_from_fn(func, prefix, file, src) {
                    out.push(test);
                }
            }
            _ => {}
        }
    }
}

fn test_from_fn(func: &ItemFn, prefix: &str, file: &Path, src: &str) -> Option<DiscoveredTest> {
    let attrs: Vec<String> = func.attrs.iter().map(attr_path).collect();
    let plain = attrs.iter().any(|attr| attr == "test");
    let custom = attrs
        .iter()
        .any(|attr| attr != "test" && attr.ends_with("test"));
    if !plain && !custom {
        return None;
    }
    let fn_path = join(prefix, &func.sig.ident.to_string());
    let body = slice_span(src, func.block.span().byte_range());
    let source = slice_span(src, func.span().byte_range());
    let skip = skip_reason(&attrs, plain, &body);
    Some(DiscoveredTest {
        name: fn_path.clone(),
        file: file.to_path_buf(),
        fn_path,
        body_bytes: body.len(),
        body,
        source,
        skip,
    })
}

fn skip_reason(attrs: &[String], plain: bool, body: &str) -> Option<String> {
    if !plain {
        return Some("custom test attribute".into());
    }
    if attrs.iter().any(|attr| attr == "ignore") {
        return Some("ignore".into());
    }
    if attrs.iter().any(|attr| attr == "should_panic") {
        return Some("should_panic".into());
    }
    for token in DENY {
        if body.contains(token) {
            return Some(format!("denylist: {token}"));
        }
    }
    None
}

fn attr_path(attr: &Attribute) -> String {
    let path = match &attr.meta {
        Meta::Path(path) | Meta::List(syn::MetaList { path, .. }) => path,
        Meta::NameValue(name) => &name.path,
    };
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn span_items(items: &[Item], prefix: &str, src: &str, out: &mut Vec<FnSpan>) {
    for item in items {
        match item {
            Item::Fn(func) => {
                let path = join(prefix, &func.sig.ident.to_string());
                let start = func.span().start().line;
                let end = func.span().end().line;
                out.push(FnSpan {
                    path,
                    line_start: start,
                    line_end: end,
                    source: slice_span(src, func.span().byte_range()),
                });
            }
            Item::Mod(module) => {
                let next = join(prefix, &module.ident.to_string());
                if let Some((_, nested)) = &module.content {
                    span_items(nested, &next, src, out);
                }
            }
            Item::Impl(item) => {
                for impl_item in &item.items {
                    if let syn::ImplItem::Fn(func) = impl_item {
                        let path = join(prefix, &func.sig.ident.to_string());
                        out.push(FnSpan {
                            path,
                            line_start: func.span().start().line,
                            line_end: func.span().end().line,
                            source: slice_span(src, func.span().byte_range()),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_fn_refs<'a>(items: &'a [Item], prefix: &str, out: &mut Vec<(String, &'a ItemFn)>) {
    for item in items {
        match item {
            Item::Fn(func) => out.push((join(prefix, &func.sig.ident.to_string()), func)),
            Item::Mod(module) => {
                if let Some((_, nested)) = &module.content {
                    collect_fn_refs(nested, &join(prefix, &module.ident.to_string()), out);
                }
            }
            _ => {}
        }
    }
}

struct CallVisitor<'a> {
    names: &'a mut Vec<String>,
}

impl<'ast> Visit<'ast> for CallVisitor<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Expr::Call(call) = expr {
            if let Expr::Path(path) = call.func.as_ref() {
                if let Some(segment) = path.path.segments.last() {
                    let name = segment.ident.to_string();
                    if !is_assert_name(&name) {
                        self.names.push(name);
                    }
                }
            }
        }
        syn::visit::visit_expr(self, expr);
    }
}

fn is_assert_name(name: &str) -> bool {
    matches!(
        name,
        "assert"
            | "assert_eq"
            | "assert_ne"
            | "debug_assert"
            | "debug_assert_eq"
            | "debug_assert_ne"
    )
}

fn resolve_mod(file: &Path, name: &str, attrs: &[Attribute]) -> Option<PathBuf> {
    let parent = file.parent()?;
    if let Some(path_attr) = attrs.iter().find_map(|attr| {
        let Meta::NameValue(name) = &attr.meta else {
            return None;
        };
        if !name.path.is_ident("path") {
            return None;
        }
        if let syn::Expr::Lit(lit) = &name.value {
            if let syn::Lit::Str(value) = &lit.lit {
                return Some(value.value());
            }
        }
        None
    }) {
        return Some(parent.join(path_attr));
    }
    let stem = file.file_stem()?.to_str()?;
    let candidates = if stem == "lib" || stem == "main" || stem == "mod" {
        vec![
            parent.join(format!("{name}.rs")),
            parent.join(name).join("mod.rs"),
        ]
    } else {
        vec![
            parent.join(stem).join(format!("{name}.rs")),
            parent.join(stem).join(name).join("mod.rs"),
        ]
    };
    candidates.into_iter().find(|path| path.is_file())
}

fn slice_span(src: &str, range: std::ops::Range<usize>) -> String {
    if range.start > range.end || range.end > src.len() {
        return String::new();
    }
    src[range].to_string()
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

pub fn glob_skipped(name: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNIPPET: &str = r#"
#[test]
fn encode_batch() {
    let mut n = 0u64;
    for _ in 0..10_000 {
        n = n.wrapping_add(encode(b"abc").len() as u64);
    }
    assert_eq!(n, 3);
}

#[test]
fn tiny_assert() {
    assert_eq!(encode(b"a").len(), 1);
}

#[test]
#[ignore]
fn parked() {}

#[test]
#[should_panic]
fn blows_up() { panic!("no"); }

#[tokio::test]
async fn spawned() {}

#[test]
fn opens_socket() {
    let _ = std::net::TcpStream::connect("127.0.0.1:9");
}
"#;

    #[test]
    fn classifies_plain_custom_ignore_and_denylist() {
        let found = classify_source(SNIPPET);
        let by_name = |name: &str| found.iter().find(|test| test.name == name).unwrap().clone();
        assert!(by_name("encode_batch").skip.is_none());
        assert!(by_name("tiny_assert").skip.is_none());
        assert_eq!(by_name("parked").skip.as_deref(), Some("ignore"));
        assert_eq!(by_name("blows_up").skip.as_deref(), Some("should_panic"));
        assert_eq!(
            by_name("spawned").skip.as_deref(),
            Some("custom test attribute")
        );
        assert!(by_name("opens_socket")
            .skip
            .unwrap()
            .starts_with("denylist:"));
        assert!(by_name("encode_batch").body_bytes > by_name("tiny_assert").body_bytes);
    }

    #[test]
    fn scan_order_is_largest_body_first_and_capped() {
        let ordered = order_for_scan(classify_source(SNIPPET), 1);
        let kept: Vec<_> = ordered
            .iter()
            .filter(|test| test.skip.is_none())
            .map(|test| test.name.as_str())
            .collect();
        assert_eq!(kept, vec!["encode_batch"]);
    }

    #[test]
    fn skip_glob_matches_the_test_name() {
        assert!(glob_skipped("read_io_file", &["*_io_*".into()]));
        assert!(!glob_skipped("encode_batch", &["*_io_*".into()]));
    }

    #[test]
    fn harness_prefix_parses_separately_from_a_module_path() {
        let id = parse_test_id("tests/parse.rs::parse_large");
        assert_eq!(id.harness.as_deref(), Some("tests/parse.rs"));
        assert_eq!(id.name, "parse_large");
        let id = parse_test_id("tests::encode_batch");
        assert!(id.harness.is_none());
        assert_eq!(id.name, "tests::encode_batch");
    }

    #[test]
    fn direct_callees_include_encode_once() {
        let src = format!("fn encode(input: &[u8]) -> usize {{ input.len() }}\n{SNIPPET}");
        let names = direct_callees(&src, "encode_batch");
        assert!(names.contains(&"encode".to_string()), "{names:?}");
        assert!(!names.iter().any(|name| name.starts_with("assert")));
    }
}
