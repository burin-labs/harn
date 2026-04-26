//! Discovery and parsing of `invariants.harn` Flow predicate files.
//!
//! Mirrors `metadata_resolve` semantics: predicates declared in higher
//! directories apply to all descendants, with stricter (closer-to-leaf)
//! files overriding when names collide. This module owns the walk + parse;
//! evaluation lives in [`super::executor`].
//!
//! See parent epic #571 and ticket #579 for the design rationale.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harn_lexer::{Lexer, Span};
use harn_parser::{peel_attributes, Attribute, AttributeArg, Node, Parser};

use super::executor::PredicateKind;

/// Filename used for per-directory Flow invariant declarations.
pub const INVARIANTS_FILE: &str = "invariants.harn";

/// One `invariants.harn` file discovered on disk, with its predicates
/// already parsed into typed metadata.
#[derive(Clone, Debug)]
pub struct DiscoveredInvariantFile {
    /// Absolute path to the source file.
    pub path: PathBuf,
    /// Path relative to the discovery root, normalised with `/` separators.
    pub relative_dir: String,
    /// Raw source — kept around so callers can render diagnostics.
    pub source: String,
    /// Predicates declared at the top level, in source order.
    pub predicates: Vec<DiscoveredPredicate>,
    /// Parse / attribute errors encountered when reading this file.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

/// One Flow predicate declaration parsed out of an invariants file.
#[derive(Clone, Debug)]
pub struct DiscoveredPredicate {
    /// Function name. Composition uses `relative_dir + name` as the
    /// identity for stricter-child overrides.
    pub name: String,
    /// `Deterministic` (default) or `Semantic`.
    pub kind: PredicateKind,
    /// Optional Archivist provenance block.
    pub archivist: Option<ArchivistMetadata>,
    /// Advisory historical flag — predicates that legalise existing state
    /// rather than gate new atoms.
    pub retroactive: bool,
    /// Span of the function declaration in the source file (1-based).
    pub span: Span,
}

/// Provenance metadata pulled from `@archivist(...)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArchivistMetadata {
    pub evidence: Vec<String>,
    pub confidence: Option<f64>,
    pub source_date: Option<String>,
    pub coverage_examples: Vec<String>,
}

/// One diagnostic surfaced by discovery — covers both parse errors and
/// the structural attribute checks that go beyond the typechecker
/// (`@invariant` requires `@archivist`, etc.).
#[derive(Clone, Debug)]
pub struct DiscoveryDiagnostic {
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

/// Walk from `root` down through every component of `target_dir`,
/// collecting `invariants.harn` at each level.
///
/// Returns the files in root-to-leaf order so callers can apply
/// stricter-child overrides simply by iterating in order.
///
/// `target_dir` is interpreted relative to `root`. Absolute paths or
/// paths that escape `root` are silently clamped — discovery never reads
/// files outside `root`.
pub fn discover_invariants(root: &Path, target_dir: &Path) -> Vec<DiscoveredInvariantFile> {
    let mut files = Vec::new();
    let candidates = candidate_directories(root, target_dir);

    for dir in candidates {
        let path = dir.join(INVARIANTS_FILE);
        if !path.is_file() {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let relative_dir = relative_dir_label(root, &dir);
        let parsed = parse_invariants_source(&source);
        files.push(DiscoveredInvariantFile {
            path,
            relative_dir,
            source,
            predicates: parsed.predicates,
            diagnostics: parsed.diagnostics,
        });
    }

    files
}

/// Parse a single `invariants.harn` source string. Exposed publicly for
/// tests, the LSP, and tooling that has the file contents in hand.
pub fn parse_invariants_source(source: &str) -> ParsedInvariantFile {
    let mut diagnostics = Vec::new();
    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(error) => {
            diagnostics.push(DiscoveryDiagnostic {
                severity: DiagnosticSeverity::Error,
                message: format!("lex error: {error:?}"),
                span: None,
            });
            return ParsedInvariantFile {
                predicates: Vec::new(),
                diagnostics,
            };
        }
    };
    let program = match Parser::new(tokens).parse() {
        Ok(p) => p,
        Err(error) => {
            diagnostics.push(DiscoveryDiagnostic {
                severity: DiagnosticSeverity::Error,
                message: format!("parse error: {error:?}"),
                span: None,
            });
            return ParsedInvariantFile {
                predicates: Vec::new(),
                diagnostics,
            };
        }
    };

    let mut predicates = Vec::new();
    for node in &program {
        let (attrs, inner) = peel_attributes(node);
        let Node::FnDecl { name, .. } = &inner.node else {
            continue;
        };
        let Some(predicate) = predicate_from_attributes(name, attrs, inner.span, &mut diagnostics)
        else {
            continue;
        };
        predicates.push(predicate);
    }

    ParsedInvariantFile {
        predicates,
        diagnostics,
    }
}

/// Parsed-but-not-yet-located output of [`parse_invariants_source`].
#[derive(Clone, Debug, Default)]
pub struct ParsedInvariantFile {
    pub predicates: Vec<DiscoveredPredicate>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

fn predicate_from_attributes(
    name: &str,
    attrs: &[Attribute],
    span: Span,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
) -> Option<DiscoveredPredicate> {
    // The Flow predicate marker is a *bare* `@invariant`. Anything with
    // arguments is the handler-IR form and is not part of Flow discovery.
    let invariant = attrs.iter().find(|a| a.name == "invariant")?;
    if !invariant.args.is_empty() {
        return None;
    }

    let deterministic = attrs.iter().any(|a| a.name == "deterministic");
    let semantic = attrs.iter().any(|a| a.name == "semantic");
    let kind = match (deterministic, semantic) {
        (true, true) => {
            diagnostics.push(DiscoveryDiagnostic {
                severity: DiagnosticSeverity::Error,
                message: format!(
                    "predicate `{name}` declares both `@deterministic` and \
                     `@semantic`; pick exactly one"
                ),
                span: Some(span),
            });
            PredicateKind::Deterministic
        }
        (false, false) => {
            // Default per design: predicates without an explicit mode are
            // deterministic.
            PredicateKind::Deterministic
        }
        (true, false) => PredicateKind::Deterministic,
        (false, true) => PredicateKind::Semantic,
    };

    let archivist = attrs
        .iter()
        .find(|a| a.name == "archivist")
        .map(parse_archivist_attribute);
    if archivist.is_none() {
        diagnostics.push(DiscoveryDiagnostic {
            severity: DiagnosticSeverity::Warning,
            message: format!(
                "predicate `{name}` is missing `@archivist(...)` provenance \
                 (evidence, confidence, source_date, coverage_examples)"
            ),
            span: Some(span),
        });
    }

    let retroactive = attrs.iter().any(|a| a.name == "retroactive");

    Some(DiscoveredPredicate {
        name: name.to_string(),
        kind,
        archivist,
        retroactive,
        span,
    })
}

fn parse_archivist_attribute(attr: &Attribute) -> ArchivistMetadata {
    let mut metadata = ArchivistMetadata::default();
    for arg in &attr.args {
        let Some(name) = arg.name.as_deref() else {
            continue;
        };
        match name {
            "evidence" => metadata.evidence = string_list_arg(arg),
            "confidence" => metadata.confidence = number_arg(arg),
            "source_date" => metadata.source_date = string_arg(arg),
            "coverage_examples" => metadata.coverage_examples = string_list_arg(arg),
            _ => {}
        }
    }
    metadata
}

fn string_arg(arg: &AttributeArg) -> Option<String> {
    match &arg.value.node {
        Node::StringLiteral(s) | Node::RawStringLiteral(s) => Some(s.clone()),
        _ => None,
    }
}

fn number_arg(arg: &AttributeArg) -> Option<f64> {
    match &arg.value.node {
        Node::FloatLiteral(f) => Some(*f),
        Node::IntLiteral(i) => Some(*i as f64),
        _ => None,
    }
}

fn string_list_arg(arg: &AttributeArg) -> Vec<String> {
    match &arg.value.node {
        Node::ListLiteral(items) => items
            .iter()
            .filter_map(|item| match &item.node {
                Node::StringLiteral(s) | Node::RawStringLiteral(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        Node::StringLiteral(s) | Node::RawStringLiteral(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Build the root → target chain of directories to inspect, in order.
///
/// Mirrors `MetadataState::resolve`: starts at `root`, then descends one
/// component at a time. Empty / `.` / `..` components are stripped so a
/// caller can't escape the root.
fn candidate_directories(root: &Path, target_dir: &Path) -> Vec<PathBuf> {
    let mut chain = vec![root.to_path_buf()];

    // Make `target_dir` relative to `root` if it is absolute, otherwise
    // treat it as already-relative.
    let relative = target_dir.strip_prefix(root).unwrap_or_else(|_| {
        if target_dir.is_absolute() {
            Path::new("")
        } else {
            target_dir
        }
    });

    let mut current = root.to_path_buf();
    for component in relative.components() {
        use std::path::Component;
        match component {
            Component::Normal(name) => {
                current.push(name);
                chain.push(current.clone());
            }
            Component::CurDir => {}
            // Refuse to escape `root`.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                continue;
            }
        }
    }

    chain
}

fn relative_dir_label(root: &Path, dir: &Path) -> String {
    let rel = dir.strip_prefix(root).unwrap_or(dir);
    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        if let std::path::Component::Normal(name) = component {
            parts.push(name.to_string_lossy().into_owned());
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Resolve a directory's effective predicate set.
///
/// Walks the discovered files in root-to-leaf order, layering them so a
/// stricter (closer-to-leaf) declaration with the same name overrides the
/// ancestor. Returns predicates in deterministic source order, oldest
/// ancestor first then newest within each level.
pub fn resolve_predicates(files: &[DiscoveredInvariantFile]) -> Vec<(String, DiscoveredPredicate)> {
    // Use a BTreeMap for stable iteration so callers get deterministic
    // output regardless of disk ordering.
    let mut effective: BTreeMap<String, (String, DiscoveredPredicate)> = BTreeMap::new();
    for file in files {
        for predicate in &file.predicates {
            let key = predicate.name.clone();
            let qualified = if file.relative_dir == "." {
                predicate.name.clone()
            } else {
                format!("{}::{}", file.relative_dir, predicate.name)
            };
            effective.insert(key, (qualified, predicate.clone()));
        }
    }
    effective.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), contents).unwrap();
    }

    fn sample_predicate(name: &str) -> String {
        format!(
            r#"
@invariant
@deterministic
@archivist(evidence: ["https://example.com/spec"], confidence: 0.95, source_date: "2026-04-01")
fn {name}(slice) -> bool {{
    return true
}}
"#
        )
    }

    #[test]
    fn discover_walks_from_root_to_leaf() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(root, INVARIANTS_FILE, &sample_predicate("root_check"));
        let nested = root.join("crates").join("foo");
        write(&nested, INVARIANTS_FILE, &sample_predicate("inner_check"));

        let files = discover_invariants(root, &nested);
        let labels: Vec<_> = files.iter().map(|f| f.relative_dir.clone()).collect();
        assert_eq!(labels, vec![".".to_string(), "crates/foo".to_string()]);
        assert_eq!(files[0].predicates[0].name, "root_check");
        assert_eq!(files[0].predicates[0].kind, PredicateKind::Deterministic);
        assert_eq!(files[1].predicates[0].name, "inner_check");
    }

    #[test]
    fn discover_clamps_parent_dir_traversal() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        write(&root, INVARIANTS_FILE, &sample_predicate("root_check"));

        let files = discover_invariants(&root, Path::new("../../escape"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_dir, ".");
    }

    #[test]
    fn parse_picks_up_archivist_metadata() {
        let source = sample_predicate("foo");
        let parsed = parse_invariants_source(&source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let pred = &parsed.predicates[0];
        let arch = pred.archivist.as_ref().expect("archivist present");
        assert_eq!(arch.evidence, vec!["https://example.com/spec".to_string()]);
        assert_eq!(arch.confidence, Some(0.95));
        assert_eq!(arch.source_date.as_deref(), Some("2026-04-01"));
    }

    #[test]
    fn parse_warns_when_archivist_missing() {
        let source = r#"
@invariant
@deterministic
fn missing_arch(slice) -> bool { return true }
"#;
        let parsed = parse_invariants_source(source);
        assert_eq!(parsed.predicates.len(), 1);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("missing `@archivist(...)`")));
    }

    #[test]
    fn parse_errors_when_kinds_collide() {
        let source = r#"
@invariant
@deterministic
@semantic
@archivist(evidence: ["x"])
fn both_modes(slice) -> bool { return true }
"#;
        let parsed = parse_invariants_source(source);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error
                && d.message.contains("pick exactly one")));
    }

    #[test]
    fn parse_recognises_semantic_mode_and_retroactive() {
        let source = r#"
@invariant
@semantic
@retroactive
@archivist(evidence: ["https://x"], confidence: 0.5)
fn check(slice) -> bool { return true }
"#;
        let parsed = parse_invariants_source(source);
        assert_eq!(parsed.predicates.len(), 1);
        let pred = &parsed.predicates[0];
        assert_eq!(pred.kind, PredicateKind::Semantic);
        assert!(pred.retroactive);
    }

    #[test]
    fn parse_skips_handler_ir_invariants() {
        // `@invariant("name", "glob")` is the harn-ir handler form; it
        // should never be treated as a Flow predicate.
        let source = r#"
@invariant("fs.writes", "src/**")
fn handler_check(slice) -> bool { return true }
"#;
        let parsed = parse_invariants_source(source);
        assert!(parsed.predicates.is_empty(), "{:?}", parsed.predicates);
    }

    #[test]
    fn resolve_predicates_overrides_ancestors() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(root, INVARIANTS_FILE, &sample_predicate("shared"));
        let nested = root.join("crates");
        // Override `shared` and add `extra`.
        write(
            &nested,
            INVARIANTS_FILE,
            &format!(
                "{}{}",
                sample_predicate("shared"),
                sample_predicate("extra")
            ),
        );

        let files = discover_invariants(root, &nested);
        let resolved = resolve_predicates(&files);
        let qualified: Vec<_> = resolved.iter().map(|(q, _)| q.clone()).collect();
        // `shared` is overridden by the deeper file → qualified path.
        assert!(qualified.contains(&"crates::shared".to_string()));
        // `extra` only exists in the deeper file.
        assert!(qualified.contains(&"crates::extra".to_string()));
        // The root version should not appear in the resolved set.
        assert!(!qualified.contains(&"shared".to_string()));
    }
}
