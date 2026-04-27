//! Repo-root `meta-invariants.harn` bootstrap policy.
//!
//! Solves the "predicate code that gates predicate code" problem flagged in
//! decision 2 of `docs/src/flow-predicates.md`. Per-directory `invariants.harn`
//! files declare slice gates; this module declares the small, hand-authored
//! policy that gates *those* gates' authorship.
//!
//! Two validation entrypoints front the policy:
//!
//! - [`validate_predicate_edit`] checks a proposed edit to an
//!   `invariants.harn` file. Archivist may propose; humans and other
//!   non-Archivist actors may also propose. Promotion still requires the
//!   normal slice approval chain — this function only enforces the bootstrap
//!   rules (parseability, kind annotation, archivist provenance, semantic
//!   fallback presence).
//! - [`validate_bootstrap_edit`] checks a proposed edit to
//!   `meta-invariants.harn` itself. Archivist authorship is rejected outright;
//!   any other author yields `RequireApproval` routing to a human maintainer
//!   listed in the previous policy. The previous policy hash is pinned in the
//!   result so the slice approval chain has an explicit audit reference.
//!
//! See parent epic #571, the predicate decision record (#584), and the
//! implementation ticket (#734).

use std::path::{Path, PathBuf};

use harn_lexer::Lexer;
use harn_parser::{peel_attributes, Attribute, AttributeArg, Node, Parser};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::discovery::{
    parse_invariants_source, ArchivistMetadata, DiagnosticSeverity, DiscoveryDiagnostic,
};
use super::result::{Approver, InvariantBlockError, Verdict};
use crate::flow::slice::PredicateHash;

/// Filename used for the repo-root bootstrap policy file.
pub const META_INVARIANTS_FILE: &str = "meta-invariants.harn";

/// Default maintainer routed when the discovered policy doesn't list any.
///
/// Mirrors the Ship Captain ceiling default approver so a freshly-seeded
/// repository still ends up at the same human review desk.
pub const DEFAULT_MAINTAINER_ROLE: &str = "flow-platform";

/// Stable error codes attached to [`BootstrapViolation`]s.
pub mod codes {
    pub const PARSE_ERROR: &str = "bootstrap_parse_error";
    pub const KIND_COLLISION: &str = "bootstrap_kind_collision";
    pub const MISSING_ARCHIVIST: &str = "bootstrap_missing_archivist";
    pub const ARCHIVIST_PROVENANCE_INCOMPLETE: &str = "bootstrap_archivist_provenance_incomplete";
    pub const MISSING_SEMANTIC_FALLBACK: &str = "bootstrap_missing_semantic_fallback";
    pub const UNRESOLVED_SEMANTIC_FALLBACK: &str = "bootstrap_unresolved_semantic_fallback";
    pub const ARCHIVIST_AUTHORED_BOOTSTRAP: &str = "bootstrap_archivist_cannot_author_bootstrap";
}

/// Identity of the actor proposing a predicate-authorship change.
///
/// Stored on the proposal envelope and consulted by both validators: the
/// bootstrap policy is the only place Archivist authorship is a hard error,
/// and only the meta-invariants validator checks the discriminant — normal
/// `invariants.harn` edits accept any author because the slice approval chain
/// is responsible for promotion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditAuthor {
    /// The Archivist persona — propose-only, never auto-promotes bootstrap.
    Archivist,
    /// A named human maintainer (e.g. `user:alice`).
    Human { id: String },
    /// Any other automated actor (Fixer, Ship Captain, replay tools).
    System { id: String },
}

impl EditAuthor {
    pub fn human(id: impl Into<String>) -> Self {
        Self::Human { id: id.into() }
    }

    pub fn system(id: impl Into<String>) -> Self {
        Self::System { id: id.into() }
    }

    fn label(&self) -> String {
        match self {
            EditAuthor::Archivist => "archivist".to_string(),
            EditAuthor::Human { id } => format!("human:{id}"),
            EditAuthor::System { id } => format!("system:{id}"),
        }
    }
}

/// One bootstrap-policy violation produced by the validators.
///
/// The `code` field is owned `String` so the type round-trips through serde.
/// The static codes in [`codes`] are the canonical authoring source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapViolation {
    pub code: String,
    pub message: String,
    /// Predicate name when the violation is tied to a specific declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

impl BootstrapViolation {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            predicate: None,
        }
    }

    fn with_predicate(mut self, predicate: impl Into<String>) -> Self {
        self.predicate = Some(predicate.into());
        self
    }
}

/// Parsed `meta-invariants.harn` contents.
///
/// The file is ordinary Harn syntax so the existing lexer and parser handle
/// it. The policy carries the file's content hash (pinned for replay audit)
/// and the maintainer routing list extracted from a top-level
/// `@bootstrap_maintainers(...)` attribute, if present. Parser diagnostics
/// are returned alongside via [`BootstrapPolicy::parse_with_diagnostics`] so
/// the policy struct itself is serde-clean for audit payloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPolicy {
    /// Stable content hash of the entire file source.
    pub hash: PredicateHash,
    /// Maintainer approvers in source order. At least one entry is always
    /// present — empty input lists fall back to [`DEFAULT_MAINTAINER_ROLE`].
    pub maintainers: Vec<Approver>,
}

impl BootstrapPolicy {
    /// Parse a `meta-invariants.harn` source string. Discards any parser
    /// diagnostics; use [`BootstrapPolicy::parse_with_diagnostics`] when you
    /// need to surface them.
    ///
    /// The hash is computed across the full source bytes so any change — even
    /// reordering whitespace — produces a new hash. This matches the existing
    /// `predicate_source_hash` shape (`sha256:<hex>`).
    pub fn parse(source: &str) -> Self {
        Self::parse_with_diagnostics(source).0
    }

    /// Parse a `meta-invariants.harn` source string and return the structural
    /// diagnostics raised by the lexer/parser. Missing optional configuration
    /// (e.g. no `@bootstrap_maintainers` attribute) is a silent fall back to
    /// defaults — only real parse errors appear here.
    pub fn parse_with_diagnostics(source: &str) -> (Self, Vec<DiscoveryDiagnostic>) {
        let hash = bootstrap_hash(source);
        let parsed = parse_invariants_source(source);
        let mut diagnostics = parsed.diagnostics;

        let mut maintainers = Vec::new();
        match collect_top_level_attributes(source) {
            Ok(attrs) => {
                for attr in attrs {
                    if attr.name == "bootstrap_maintainers" {
                        maintainers.extend(parse_maintainers(&attr.args));
                    }
                }
            }
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
        if maintainers.is_empty() {
            maintainers.push(Approver::role(DEFAULT_MAINTAINER_ROLE));
        }

        (Self { hash, maintainers }, diagnostics)
    }
}

/// Bootstrap policy as discovered on disk.
#[derive(Clone, Debug)]
pub struct DiscoveredBootstrapPolicy {
    /// Absolute path to `meta-invariants.harn`.
    pub path: PathBuf,
    /// Raw source — kept around so callers can render diagnostics or echo it
    /// back in audit payloads.
    pub source: String,
    /// Parsed policy contents.
    pub policy: BootstrapPolicy,
    /// Diagnostics raised while parsing.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

/// Look for `<root>/meta-invariants.harn` and parse it if present.
///
/// Returns `None` when the file is missing or unreadable. A parse failure is
/// surfaced as `Some(...)` with the structural diagnostics returned on
/// [`DiscoveredBootstrapPolicy::diagnostics`] — callers that need to
/// fail-fast should inspect that field.
pub fn discover_bootstrap_policy(root: &Path) -> Option<DiscoveredBootstrapPolicy> {
    let path = root.join(META_INVARIANTS_FILE);
    if !path.is_file() {
        return None;
    }
    let source = std::fs::read_to_string(&path).ok()?;
    let (policy, diagnostics) = BootstrapPolicy::parse_with_diagnostics(&source);
    Some(DiscoveredBootstrapPolicy {
        path,
        source,
        policy,
        diagnostics,
    })
}

/// Result of running a bootstrap validator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapValidation {
    /// Effective verdict for the proposed edit. `Block` when the proposal
    /// violates a structural rule or — for bootstrap edits — the author lacks
    /// authorship rights. `RequireApproval` when a human cosigner must
    /// promote the edit. `Allow` only when the edit is structurally clean and
    /// no approval routing is required.
    pub verdict: Verdict,
    /// Hash of the previous committed bootstrap policy. `None` for an initial
    /// seed where there is no prior policy to compare against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_policy_hash: Option<PredicateHash>,
    /// Hash of the proposed `meta-invariants.harn` source. Set only by
    /// [`validate_bootstrap_edit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_policy_hash: Option<PredicateHash>,
    /// Author label echoed back for audit-log convenience.
    pub author: String,
    /// Structured violations. Empty on the happy path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<BootstrapViolation>,
}

impl BootstrapValidation {
    /// True when the validation produced a structural `Block`.
    pub fn is_blocked(&self) -> bool {
        matches!(self.verdict, Verdict::Block { .. })
    }

    /// True when the proposal needs an approver cosignature before promotion.
    pub fn requires_approval(&self) -> bool {
        matches!(self.verdict, Verdict::RequireApproval { .. })
    }
}

/// Validate a proposed edit to a per-directory `invariants.harn` file.
///
/// Bootstrap policy promotes the soft warnings on `parse_invariants_source`
/// into hard errors:
///
/// - Source must lex/parse cleanly.
/// - Every `@invariant` predicate must declare exactly one of
///   `@deterministic` or `@semantic`.
/// - Every predicate must carry a complete `@archivist(evidence,
///   confidence, source_date, coverage_examples)` provenance block.
/// - `@semantic` predicates must declare a fallback whose target is a
///   deterministic predicate visible in the proposed source.
///
/// `previous_policy` lets the caller pin the previously committed bootstrap
/// hash into the validation result for audit. It does not change the rules
/// applied — the bootstrap rules themselves live in this function so a
/// repository can roll its policy hash forward without rewriting Rust.
pub fn validate_predicate_edit(
    proposed_source: &str,
    author: &EditAuthor,
    previous_policy: Option<&BootstrapPolicy>,
) -> BootstrapValidation {
    let violations = collect_predicate_edit_violations(proposed_source);
    let verdict = if violations.is_empty() {
        Verdict::Allow
    } else {
        Verdict::Block {
            error: build_block_error(codes::PARSE_ERROR, &violations),
        }
    };
    BootstrapValidation {
        verdict,
        previous_policy_hash: previous_policy.map(|policy| policy.hash.clone()),
        proposed_policy_hash: None,
        author: author.label(),
        violations,
    }
}

/// Validate a proposed edit to `meta-invariants.harn`.
///
/// - Archivist authorship is a hard `Block` with the stable code
///   `bootstrap_archivist_cannot_author_bootstrap`.
/// - Otherwise, the proposed source is parsed for structural problems. If
///   parsing fails, the result is `Block`.
/// - Clean proposals from a non-Archivist author yield `RequireApproval`,
///   routed to one of the maintainers listed in the previous policy (or the
///   default maintainer role on initial seed). The previous policy's hash and
///   the proposed policy's hash are both pinned in the result so the slice
///   approval chain has explicit audit pointers.
pub fn validate_bootstrap_edit(
    proposed_source: &str,
    author: &EditAuthor,
    previous_policy: Option<&BootstrapPolicy>,
) -> BootstrapValidation {
    let proposed_hash = bootstrap_hash(proposed_source);
    let previous_hash = previous_policy.map(|policy| policy.hash.clone());
    let mut violations = Vec::new();

    if matches!(author, EditAuthor::Archivist) {
        violations.push(BootstrapViolation::new(
            codes::ARCHIVIST_AUTHORED_BOOTSTRAP,
            "Archivist persona is propose-only and may not author or promote \
             meta-invariants.harn edits — escalate to a human maintainer",
        ));
        let error = build_block_error(codes::ARCHIVIST_AUTHORED_BOOTSTRAP, &violations);
        return BootstrapValidation {
            verdict: Verdict::Block { error },
            previous_policy_hash: previous_hash,
            proposed_policy_hash: Some(proposed_hash),
            author: author.label(),
            violations,
        };
    }

    let (parsed_policy, parse_diagnostics) =
        BootstrapPolicy::parse_with_diagnostics(proposed_source);
    for diagnostic in parse_diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
    {
        violations.push(BootstrapViolation::new(
            codes::PARSE_ERROR,
            diagnostic.message.clone(),
        ));
    }

    if !violations.is_empty() {
        let error = build_block_error(codes::PARSE_ERROR, &violations);
        return BootstrapValidation {
            verdict: Verdict::Block { error },
            previous_policy_hash: previous_hash,
            proposed_policy_hash: Some(proposed_hash),
            author: author.label(),
            violations,
        };
    }

    let approver = previous_policy
        .and_then(|policy| policy.maintainers.first().cloned())
        .or_else(|| parsed_policy.maintainers.first().cloned())
        .unwrap_or_else(|| Approver::role(DEFAULT_MAINTAINER_ROLE));

    BootstrapValidation {
        verdict: Verdict::RequireApproval { approver },
        previous_policy_hash: previous_hash,
        proposed_policy_hash: Some(proposed_hash),
        author: author.label(),
        violations,
    }
}

fn collect_predicate_edit_violations(proposed_source: &str) -> Vec<BootstrapViolation> {
    let parsed = parse_invariants_source(proposed_source);
    let mut violations = Vec::new();

    for diagnostic in &parsed.diagnostics {
        if diagnostic.severity != DiagnosticSeverity::Error {
            continue;
        }
        let code = if diagnostic.message.contains("pick exactly one") {
            codes::KIND_COLLISION
        } else if diagnostic
            .message
            .contains("must declare a deterministic fallback")
        {
            codes::MISSING_SEMANTIC_FALLBACK
        } else if diagnostic
            .message
            .contains("same invariants.harn file or an ancestor file")
        {
            codes::UNRESOLVED_SEMANTIC_FALLBACK
        } else {
            codes::PARSE_ERROR
        };
        violations.push(BootstrapViolation::new(code, diagnostic.message.clone()));
    }

    for predicate in &parsed.predicates {
        // The parser already raises kind-collision and missing-semantic-fallback
        // as error diagnostics, picked up in the loop above. Bootstrap adds the
        // missing-/incomplete-archivist checks because the parser only warns.
        match predicate.archivist.as_ref() {
            None => {
                violations.push(
                    BootstrapViolation::new(
                        codes::MISSING_ARCHIVIST,
                        format!(
                            "predicate `{}` must declare `@archivist(...)` provenance \
                             before promotion",
                            predicate.name
                        ),
                    )
                    .with_predicate(&predicate.name),
                );
            }
            Some(archivist) => {
                for missing in archivist_missing_fields(archivist) {
                    violations.push(
                        BootstrapViolation::new(
                            codes::ARCHIVIST_PROVENANCE_INCOMPLETE,
                            format!(
                                "predicate `{}` is missing required @archivist field `{missing}`",
                                predicate.name
                            ),
                        )
                        .with_predicate(&predicate.name),
                    );
                }
            }
        }
    }

    deduplicate_violations(violations)
}

fn deduplicate_violations(violations: Vec<BootstrapViolation>) -> Vec<BootstrapViolation> {
    let mut seen = std::collections::BTreeSet::<(String, String, Option<String>)>::new();
    let mut out = Vec::with_capacity(violations.len());
    for violation in violations {
        let key = (
            violation.code.to_string(),
            violation.message.clone(),
            violation.predicate.clone(),
        );
        if seen.insert(key) {
            out.push(violation);
        }
    }
    out
}

fn archivist_missing_fields(archivist: &ArchivistMetadata) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if archivist.evidence.is_empty() {
        missing.push("evidence");
    }
    if archivist.confidence.is_none() {
        missing.push("confidence");
    }
    if archivist.source_date.is_none() {
        missing.push("source_date");
    }
    if archivist.coverage_examples.is_empty() {
        missing.push("coverage_examples");
    }
    missing
}

fn build_block_error(code: &'static str, violations: &[BootstrapViolation]) -> InvariantBlockError {
    let summary = violations
        .iter()
        .map(|v| {
            if let Some(predicate) = &v.predicate {
                format!("{} (in `{predicate}`): {}", v.code, v.message)
            } else {
                format!("{}: {}", v.code, v.message)
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let message = if summary.is_empty() {
        "bootstrap policy rejected the proposed edit".to_string()
    } else {
        format!("bootstrap policy rejected the proposed edit: {summary}")
    };
    InvariantBlockError::new(code, message)
}

fn bootstrap_hash(source: &str) -> PredicateHash {
    PredicateHash::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(source.as_bytes()))
    ))
}

fn parse_maintainers(args: &[AttributeArg]) -> Vec<Approver> {
    args.iter()
        .flat_map(|arg| match &arg.value.node {
            Node::ListLiteral(items) => items
                .iter()
                .filter_map(|item| match &item.node {
                    Node::StringLiteral(s) | Node::RawStringLiteral(s) => {
                        Some(parse_maintainer_str(s))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Node::StringLiteral(s) | Node::RawStringLiteral(s) => {
                vec![parse_maintainer_str(s)]
            }
            _ => Vec::new(),
        })
        .collect()
}

fn parse_maintainer_str(value: &str) -> Approver {
    if let Some(role) = value.strip_prefix("role:") {
        Approver::role(role.trim())
    } else if let Some(principal) = value.strip_prefix("user:") {
        Approver::principal(format!("user:{}", principal.trim()))
    } else {
        Approver::principal(value.trim())
    }
}

/// Walk the top-level attribute lists in a Harn source string. Used by the
/// bootstrap parser to find a `@bootstrap_maintainers(...)` configuration
/// without needing it to live on a real predicate function.
fn collect_top_level_attributes(source: &str) -> Result<Vec<Attribute>, DiscoveryDiagnostic> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|error| DiscoveryDiagnostic {
            severity: DiagnosticSeverity::Error,
            message: format!("lex error: {error:?}"),
            span: None,
        })?;
    let program = Parser::new(tokens)
        .parse()
        .map_err(|error| DiscoveryDiagnostic {
            severity: DiagnosticSeverity::Error,
            message: format!("parse error: {error:?}"),
            span: None,
        })?;
    let mut out = Vec::new();
    for node in &program {
        let (attrs, _inner) = peel_attributes(node);
        out.extend(attrs.iter().cloned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const VALID_PREDICATE: &str = r#"
@invariant
@deterministic
@archivist(
  evidence: ["https://example.com/spec"],
  confidence: 0.95,
  source_date: "2026-04-26",
  coverage_examples: ["crates/api/src/auth.rs"]
)
fn no_raw_tokens(slice) {
  return flow_invariant_allow()
}
"#;

    const VALID_BOOTSTRAP: &str = r#"
@bootstrap_maintainers(approvers: ["role:flow-platform", "user:alice"])
fn _meta_invariants_marker() {
  return nil
}
"#;

    #[test]
    fn parses_bootstrap_policy_hash_and_maintainers() {
        let policy = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        assert!(policy.hash.as_str().starts_with("sha256:"));
        assert_eq!(policy.maintainers.len(), 2);
        assert!(matches!(
            policy.maintainers[0],
            Approver::Role { ref name } if name == "flow-platform"
        ));
        assert!(matches!(
            policy.maintainers[1],
            Approver::Principal { ref id } if id == "user:alice"
        ));
    }

    #[test]
    fn bootstrap_hash_is_stable_across_identical_sources() {
        let a = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        let b = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn bootstrap_hash_changes_when_source_changes() {
        let a = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        let b = BootstrapPolicy::parse(&VALID_BOOTSTRAP.replace("alice", "bob"));
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn bootstrap_default_maintainer_when_attribute_missing() {
        let policy = BootstrapPolicy::parse("// no maintainer attribute here\n");
        assert_eq!(policy.maintainers.len(), 1);
        assert!(matches!(
            policy.maintainers[0],
            Approver::Role { ref name } if name == DEFAULT_MAINTAINER_ROLE
        ));
    }

    #[test]
    fn validate_initial_seed_predicate_edit_passes_without_prior_policy() {
        let result = validate_predicate_edit(VALID_PREDICATE, &EditAuthor::Archivist, None);
        assert!(matches!(result.verdict, Verdict::Allow), "{result:?}");
        assert_eq!(result.previous_policy_hash, None);
        assert!(result.violations.is_empty());
        assert_eq!(result.author, "archivist");
    }

    #[test]
    fn validate_normal_predicate_edit_pins_previous_policy_hash() {
        let policy = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        let result = validate_predicate_edit(
            VALID_PREDICATE,
            &EditAuthor::human("user:alice"),
            Some(&policy),
        );
        assert!(matches!(result.verdict, Verdict::Allow));
        assert_eq!(result.previous_policy_hash, Some(policy.hash.clone()));
    }

    #[test]
    fn validate_predicate_edit_blocks_when_archivist_provenance_missing() {
        let source = r#"
@invariant
@deterministic
fn no_provenance(slice) { return true }
"#;
        let result = validate_predicate_edit(source, &EditAuthor::Archivist, None);
        assert!(result.is_blocked());
        let codes: Vec<&str> = result.violations.iter().map(|v| v.code.as_str()).collect();
        assert!(codes.contains(&codes::MISSING_ARCHIVIST), "{codes:?}");
    }

    #[test]
    fn validate_predicate_edit_blocks_when_archivist_provenance_partial() {
        let source = r#"
@invariant
@deterministic
@archivist(evidence: ["https://x"])
fn partial_provenance(slice) { return true }
"#;
        let result = validate_predicate_edit(source, &EditAuthor::Archivist, None);
        assert!(result.is_blocked());
        let missing_fields: Vec<String> = result
            .violations
            .iter()
            .filter(|v| v.code == codes::ARCHIVIST_PROVENANCE_INCOMPLETE)
            .map(|v| v.message.clone())
            .collect();
        assert!(
            missing_fields.iter().any(|m| m.contains("confidence")),
            "{missing_fields:?}"
        );
        assert!(
            missing_fields.iter().any(|m| m.contains("source_date")),
            "{missing_fields:?}"
        );
        assert!(
            missing_fields
                .iter()
                .any(|m| m.contains("coverage_examples")),
            "{missing_fields:?}"
        );
    }

    #[test]
    fn validate_predicate_edit_blocks_when_kinds_collide() {
        let source = r#"
@invariant
@deterministic
@semantic
@archivist(evidence: ["x"], confidence: 0.5, source_date: "2026-04-26", coverage_examples: ["a"])
fn dual_mode(slice) { return true }
"#;
        let result = validate_predicate_edit(source, &EditAuthor::Archivist, None);
        assert!(result.is_blocked());
        let codes: Vec<&str> = result.violations.iter().map(|v| v.code.as_str()).collect();
        assert!(codes.contains(&codes::KIND_COLLISION), "{codes:?}");
    }

    #[test]
    fn validate_predicate_edit_blocks_when_semantic_fallback_missing() {
        let source = r#"
@invariant
@semantic
@archivist(evidence: ["x"], confidence: 0.5, source_date: "2026-04-26", coverage_examples: ["a"])
fn semantic_no_fallback(slice) { return true }
"#;
        let result = validate_predicate_edit(source, &EditAuthor::Archivist, None);
        assert!(result.is_blocked());
        let codes: Vec<&str> = result.violations.iter().map(|v| v.code.as_str()).collect();
        assert!(
            codes.contains(&codes::MISSING_SEMANTIC_FALLBACK),
            "{codes:?}"
        );
    }

    #[test]
    fn validate_bootstrap_edit_rejects_archivist_authorship() {
        let previous = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        let proposed = VALID_BOOTSTRAP.replace("alice", "mallory");
        let result = validate_bootstrap_edit(&proposed, &EditAuthor::Archivist, Some(&previous));
        assert!(result.is_blocked());
        let codes: Vec<&str> = result.violations.iter().map(|v| v.code.as_str()).collect();
        assert_eq!(codes, vec![codes::ARCHIVIST_AUTHORED_BOOTSTRAP]);
        assert_eq!(result.previous_policy_hash, Some(previous.hash));
        assert!(result.proposed_policy_hash.is_some());
    }

    #[test]
    fn validate_bootstrap_edit_routes_human_to_require_approval() {
        let previous = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        let proposed = VALID_BOOTSTRAP.replace("alice", "carol");
        let result =
            validate_bootstrap_edit(&proposed, &EditAuthor::human("user:carol"), Some(&previous));
        assert!(result.requires_approval(), "{result:?}");
        let approver = match &result.verdict {
            Verdict::RequireApproval { approver } => approver.clone(),
            other => panic!("expected RequireApproval, got {other:?}"),
        };
        assert!(matches!(approver, Approver::Role { ref name } if name == "flow-platform"));
        assert_eq!(result.previous_policy_hash, Some(previous.hash));
    }

    #[test]
    fn validate_bootstrap_edit_initial_seed_uses_default_role() {
        let result =
            validate_bootstrap_edit("// initial seed\n", &EditAuthor::human("user:alice"), None);
        assert!(result.requires_approval());
        let approver = match &result.verdict {
            Verdict::RequireApproval { approver } => approver.clone(),
            other => panic!("expected RequireApproval, got {other:?}"),
        };
        assert!(matches!(
            approver,
            Approver::Role { ref name } if name == DEFAULT_MAINTAINER_ROLE
        ));
        assert_eq!(result.previous_policy_hash, None);
    }

    #[test]
    fn validate_bootstrap_edit_blocks_unparseable_source() {
        let proposed = r#"
@invariant
@deterministic
@semantic
@archivist(evidence: ["x"])
fn bad(slice) { return true }
"#;
        let previous = BootstrapPolicy::parse(VALID_BOOTSTRAP);
        let result =
            validate_bootstrap_edit(proposed, &EditAuthor::human("user:alice"), Some(&previous));
        assert!(result.is_blocked(), "{result:?}");
    }

    #[test]
    fn discover_returns_none_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(discover_bootstrap_policy(tmp.path()).is_none());
    }

    #[test]
    fn discover_loads_meta_invariants_from_root() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(META_INVARIANTS_FILE), VALID_BOOTSTRAP).unwrap();
        let discovered = discover_bootstrap_policy(tmp.path()).expect("policy present");
        assert!(discovered.path.ends_with(META_INVARIANTS_FILE));
        assert_eq!(discovered.policy.maintainers.len(), 2);
        assert_eq!(discovered.source, VALID_BOOTSTRAP);
    }
}
