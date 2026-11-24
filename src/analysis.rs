//! Syntax-based findings for Rust functions. Findings need measurement.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

use crate::selector::{self, FnSpan};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: String,
    pub line: usize,
    pub in_loop: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FunctionAnalysis {
    pub findings: Vec<Finding>,
    pub calls: Vec<String>,
    pub max_loop_depth: usize,
    pub is_test: bool,
}

impl FunctionAnalysis {
    pub fn memory_sites(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| matches!(f.kind.as_str(), "allocation" | "copy" | "growth"))
            .count()
    }

    pub fn prompt(&self) -> String {
        let mut out = format!("Syntax analysis only. Types, allocation behavior, and call targets are not resolved.\nMaximum loop depth: {}\nNamed calls: {}\n", self.max_loop_depth, self.calls.join(", "));
        for finding in self.findings.iter().take(24) {
            out.push_str(&format!(
                "- {} at line {}{}: {}\n",
                finding.kind,
                finding.line,
                if finding.in_loop {
                    " inside a loop"
                } else {
                    ""
                },
                finding.detail
            ));
        }
        out
    }
}

pub fn analyze(source: &str, first_line: usize) -> FunctionAnalysis {
    let Ok(function) = syn::parse_str::<syn::ItemFn>(source) else {
        return FunctionAnalysis::default();
    };
    let is_test = function
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("test"));
    let mut scan = Scan {
        result: FunctionAnalysis {
            is_test,
            ..Default::default()
        },
        calls: BTreeSet::new(),
        depth: 0,
        first_line,
    };
    if function.sig.unsafety.is_some() {
        scan.add("effect", function.sig.span(), "unsafe function");
    }
    if function.sig.asyncness.is_some() {
        scan.add("effect", function.sig.span(), "async function");
    }
    scan.visit_block(&function.block);
    scan.result.calls = scan.calls.into_iter().collect();
    scan.result
}

struct Scan {
    result: FunctionAnalysis,
    calls: BTreeSet<String>,
    depth: usize,
    first_line: usize,
}

impl Scan {
    fn add(&mut self, kind: &str, span: proc_macro2::Span, detail: &str) {
        self.result.findings.push(Finding {
            kind: kind.into(),
            line: self.first_line + span.start().line.saturating_sub(1),
            in_loop: self.depth > 0,
            detail: detail.into(),
        });
    }
    fn enter_loop(&mut self) {
        self.depth += 1;
        self.result.max_loop_depth = self.result.max_loop_depth.max(self.depth);
    }
    fn call(&mut self, name: String, span: proc_macro2::Span) {
        let leaf = name.rsplit("::").next().unwrap_or(&name);
        let kind = match leaf {
            "clone" | "to_owned" | "to_vec" | "to_string" => Some("copy"),
            "collect" | "with_capacity" | "into_boxed_slice" => Some("allocation"),
            "push" | "push_str" | "extend" | "reserve" | "reserve_exact" => Some("growth"),
            "contains" | "find" | "position" | "sort" | "sort_by" | "sort_unstable"
                if self.depth > 0 =>
            {
                Some("repeated_search")
            }
            "lock" | "read" | "write" | "spawn" | "now" | "random" | "thread_rng" => Some("effect"),
            "new"
                if name.starts_with("Vec::")
                    || name.starts_with("String::")
                    || name.starts_with("Box::")
                    || name.starts_with("HashMap::") =>
            {
                Some("allocation")
            }
            _ => None,
        };
        if let Some(kind) = kind {
            self.add(
                kind,
                span,
                &format!("call to {name}; verify its type and cost"),
            );
        }
        self.calls.insert(name);
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_fn(&mut self, _: &'ast syn::ItemFn) {}
    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.enter_loop();
        visit::visit_expr_for_loop(self, node);
        self.depth -= 1;
    }
    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.enter_loop();
        visit::visit_expr_while(self, node);
        self.depth -= 1;
    }
    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.enter_loop();
        visit::visit_expr_loop(self, node);
        self.depth -= 1;
    }
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.call(node.method.to_string(), node.method.span());
        visit::visit_expr_method_call(self, node);
    }
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let name = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            self.call(name, node.span());
        }
        visit::visit_expr_call(self, node);
    }
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if let Some(segment) = node.path.segments.last() {
            if matches!(segment.ident.to_string().as_str(), "vec" | "format") {
                self.add(
                    "allocation",
                    node.span(),
                    "allocation macro; expansion is not analyzed",
                );
            }
        }
    }
    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.add("effect", node.span(), "unsafe block");
        visit::visit_expr_unsafe(self, node);
    }
}

pub fn has_global_allocator(source: &str) -> bool {
    struct Allocator(bool);
    impl<'ast> Visit<'ast> for Allocator {
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            self.0 |= attr.path().is_ident("global_allocator")
                || (attr.path().is_ident("cfg_attr")
                    && attr
                        .meta
                        .require_list()
                        .is_ok_and(|list| list.tokens.to_string().contains("global_allocator")));
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return false;
    };
    let mut visitor = Allocator(false);
    visitor.visit_file(&file);
    visitor.0
}

pub async fn collect_spans(worktree: &Path) -> Result<HashMap<String, Vec<FnSpan>>> {
    let mut map = HashMap::new();
    let files = crate::process::run(
        worktree,
        "git",
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--exclude=/target/",
            "--",
            "*.rs",
        ],
        &[],
        Duration::from_secs(30),
    )
    .await;
    if !files.success {
        bail!("cannot list Rust source files: {}", files.combined());
    }
    let root = worktree.canonicalize()?;
    for name in files.stdout.split('\0').filter(|name| !name.is_empty()) {
        if name
            .split('/')
            .any(|part| matches!(part, "target" | ".git" | ".."))
        {
            continue;
        }
        let path = worktree.join(name);
        // Source analysis must not follow a link outside the repository.
        let resolved = path
            .canonicalize()
            .with_context(|| format!("read Rust source {name}"))?;
        anyhow::ensure!(
            resolved.starts_with(&root),
            "Rust source {name} is outside the repository"
        );
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read Rust source {name}"))?;
        map.insert(name.replace('\\', "/"), selector::function_spans(&text));
    }
    Ok(map)
}

#[derive(Serialize)]
pub struct FunctionReport {
    pub file: String,
    pub function: String,
    pub line_start: usize,
    pub line_end: usize,
    pub protected: bool,
    pub analysis: FunctionAnalysis,
}

pub async fn inventory(root: &Path, protected: &[String]) -> Result<Vec<FunctionReport>> {
    let mut out = Vec::new();
    for (file, spans) in collect_spans(root).await? {
        for span in spans {
            out.push(FunctionReport {
                protected: protected
                    .iter()
                    .any(|pattern| glob_match::glob_match(pattern, &file)),
                analysis: analyze(&span.source, span.line_start),
                file: file.clone(),
                function: span.path,
                line_start: span.line_start,
                line_end: span.line_end,
            });
        }
    }
    out.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_start.cmp(&b.line_start)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_loop_allocations_and_ignores_comments_and_strings() {
        let result = analyze("fn work(xs: &[String]) {\n // Vec::new()\n let _ = \"x.clone()\";\n for x in xs { let v = x.clone(); for _ in 0..2 { consume(v.to_owned()); } }\n}", 10);
        assert_eq!(result.max_loop_depth, 2);
        assert_eq!(result.memory_sites(), 2);
        assert!(result.findings.iter().all(|f| f.in_loop && f.line == 13));
        assert!(result.calls.contains(&"consume".into()));
    }

    #[test]
    fn allocator_check_reads_attributes() {
        assert!(!has_global_allocator("// global_allocator\nfn work() {}"));
        assert!(has_global_allocator(
            "#[global_allocator] static A: Allocator = Allocator;"
        ));
        assert!(has_global_allocator(
            "#[cfg_attr(feature = \"custom\", global_allocator)] static A: Allocator = Allocator;"
        ));
    }

    #[test]
    fn supports_methods_and_marks_tests() {
        assert_eq!(
            analyze("fn copy(&self) { self.buffer.clone(); }", 1).memory_sites(),
            1
        );
        let result = analyze(
            "#[test] fn check() { let _ = vec![1]; unsafe { work(); } }",
            1,
        );
        assert!(result.is_test);
        assert!(result.findings.iter().any(|f| f.kind == "effect"));
    }
}
