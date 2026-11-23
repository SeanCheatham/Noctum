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
    "thread_rng",
    "rand::rng(",
    "rand::random(",
    "OsRng",
    "from_entropy",
    "from_os_rng",
    "getrandom(",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredTest {
    pub name: String,
    pub file: PathBuf,
    pub fn_path: String,
    pub file_fn_path: String,
    pub harness: Option<String>,
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
    walk_file(root, "", true, &mut out, &mut seen);
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
    skipped.truncate(max_scan);
    keep.extend(skipped);
    keep
}

fn walk_file(
    path: &Path,
    prefix: &str,
    root: bool,
    out: &mut Vec<DiscoveredTest>,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(canon) = path.canonicalize() else {
        return;
    };
    if !seen.insert(canon.clone()) {
        return;
    }
    let Ok(src) = fs::read_to_string(path) else {
        return;
    };
    let Ok(file) = syn::parse_file(&src) else {
        return;
    };
    let context = SourceFile {
        path,
        directory: module_dir(path, root),
        source: &src,
    };
    walk_items_follow(&file.items, prefix, "", &context, out, seen);
    seen.remove(&canon);
}

struct SourceFile<'a> {
    path: &'a Path,
    directory: PathBuf,
    source: &'a str,
}

fn walk_items_follow(
    items: &[Item],
    prefix: &str,
    file_prefix: &str,
    file: &SourceFile<'_>,
    out: &mut Vec<DiscoveredTest>,
    seen: &mut HashSet<PathBuf>,
) {
    for item in items {
        match item {
            Item::Mod(module) => {
                let next = join(prefix, &module.ident.to_string());
                let path_base = if file_prefix.is_empty() {
                    file.path.parent().unwrap_or_else(|| Path::new("."))
                } else {
                    &file.directory
                };
                if let Some((_, nested)) = &module.content {
                    let directory = path_attribute(&module.attrs)
                        .map(|p| path_base.join(p))
                        .unwrap_or_else(|| file.directory.join(module.ident.to_string()));
                    let context = SourceFile {
                        path: file.path,
                        directory,
                        source: file.source,
                    };
                    walk_items_follow(
                        nested,
                        &next,
                        &join(file_prefix, &module.ident.to_string()),
                        &context,
                        out,
                        seen,
                    );
                } else {
                    let child = path_attribute(&module.attrs)
                        .map(|p| path_base.join(p))
                        .or_else(|| resolve_mod(&file.directory, &module.ident.to_string()));
                    if let Some(child) = child {
                        walk_file(&child, &next, false, out, seen);
                    }
                }
            }
            Item::Fn(func) => {
                if let Some(mut test) = test_from_fn(func, prefix, file.path, file.source) {
                    test.file_fn_path = join(file_prefix, &func.sig.ident.to_string());
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
        file_fn_path: fn_path.clone(),
        harness: None,
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
                let owner = slice_span(src, item.self_ty.span().byte_range());
                let owner = if let Some((_, name, _)) = &item.trait_ {
                    format!("<{owner} as {}>", slice_span(src, name.span().byte_range()))
                } else {
                    owner
                };
                let owner = join(prefix, &owner);
                for impl_item in &item.items {
                    if let syn::ImplItem::Fn(func) = impl_item {
                        let path = join(&owner, &func.sig.ident.to_string());
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

fn module_dir(file: &Path, root: bool) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    if root {
        return parent.to_path_buf();
    }
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("mod") => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
        None => parent.to_path_buf(),
    }
}

fn path_attribute(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
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
    })
}

fn resolve_mod(directory: &Path, name: &str) -> Option<PathBuf> {
    let candidates = [
        directory.join(format!("{name}.rs")),
        directory.join(name).join("mod.rs"),
    ];
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
    fn external_modules_keep_a_file_relative_test_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("suite.rs");
        fs::write(&root, "mod outer { mod nested; }").unwrap();
        fs::create_dir_all(dir.path().join("outer/nested")).unwrap();
        fs::write(dir.path().join("outer/nested.rs"), "mod deeper;").unwrap();
        fs::write(
            dir.path().join("outer/nested/deeper.rs"),
            "mod tests { #[test] fn works() { assert_eq!(2, 2); } }",
        )
        .unwrap();
        let tests = discover_harness(&root);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].fn_path, "outer::nested::deeper::tests::works");
        assert_eq!(tests[0].file_fn_path, "tests::works");
        let src = fs::read_to_string(&tests[0].file).unwrap();
        let fenced = crate::fence::inject_guards(&src, &[tests[0].file_fn_path.clone()]).unwrap();
        assert!(fenced.contains(crate::fence::GUARD));
    }

    #[test]
    fn entropy_sources_are_skipped_but_fixed_seeds_are_kept() {
        let tests = classify_source("#[test] fn random() { let _rng = rand::thread_rng(); } #[test] fn fixed() { let _rng = rand::rngs::StdRng::seed_from_u64(7); }");
        assert!(tests[0].skip.as_deref().unwrap().contains("thread_rng"));
        assert!(tests[1].skip.is_none());
    }

    #[test]
    fn module_path_attributes_use_the_source_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib.rs");
        fs::write(
            &root,
            "mod plain; #[path = \"custom\"] mod inline { #[path = \"unit.rs\"] mod sub; }",
        )
        .unwrap();
        fs::write(
            dir.path().join("plain.rs"),
            "#[path = \"special.rs\"] mod renamed;",
        )
        .unwrap();
        fs::write(dir.path().join("special.rs"), "#[test] fn first() {}").unwrap();
        fs::create_dir(dir.path().join("custom")).unwrap();
        fs::write(dir.path().join("custom/unit.rs"), "#[test] fn second() {}").unwrap();
        let tests = discover_harness(&root);
        assert_eq!(
            tests.iter().map(|t| t.fn_path.as_str()).collect::<Vec<_>>(),
            ["plain::renamed::first", "inline::sub::second"]
        );
        assert_eq!(tests[0].file_fn_path, "first");
        assert_eq!(tests[1].file_fn_path, "second");
    }

    #[test]
    fn nonroot_lib_and_main_modules_use_their_own_directory() {
        for name in ["lib", "main"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("suite.rs");
            fs::write(&root, format!("mod {name};")).unwrap();
            fs::write(dir.path().join(format!("{name}.rs")), "mod nested;").unwrap();
            fs::create_dir(dir.path().join(name)).unwrap();
            fs::write(
                dir.path().join(name).join("nested.rs"),
                "#[test] fn works() {}",
            )
            .unwrap();
            let tests = discover_harness(&root);
            assert_eq!(tests.len(), 1);
            assert_eq!(tests[0].fn_path, format!("{name}::nested::works"));
        }
    }

    #[test]
    fn method_spans_keep_type_and_trait_names() {
        let spans = function_spans("impl First { fn new() {} } impl Second { fn new() {} } impl Trait for First { fn new() {} }");
        assert_eq!(
            spans.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            ["First::new", "Second::new", "<First as Trait>::new"]
        );
    }

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
