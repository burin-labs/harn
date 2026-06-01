//! The declarative rule data model.
//!
//! A rule is the atomic unit the engine consumes: an identity (`id`,
//! `language`, `severity`, `message`), a `rule` block describing *what to
//! match* (the atomic tier: `pattern` snippet, `kind`, or `regex`), and an
//! optional `fix` describing *how to rewrite* it. Relational/composite
//! matching (#2833) and `where`/`transform` (#2834) extend this model;
//! this module is the atomic-tier surface they build on.

use serde::Deserialize;

/// Diagnostic severity. Mirrors the `harn-lint` vocabulary so findings can
/// flow into the same reporting surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational; no action required.
    Info,
    /// Default — something worth a human's attention.
    #[default]
    Warning,
    /// A problem that should block.
    Error,
}

/// What flavor of work a rule performs, derived from its shape rather than
/// declared: a rule with a `fix` is a codemod; one with a `message` but no
/// `fix` is a lint; a bare matcher is a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// Find-only: report matches, no diagnostic text, no rewrite.
    Search,
    /// Report a diagnostic (`message` + `severity`), no rewrite.
    Lint,
    /// Rewrite matches via `fix`.
    Codemod,
}

/// The atomic-tier matcher. Exactly one of `pattern` / `kind` / `regex`
/// must be set; [`Matcher::resolve`] enforces that and yields the typed
/// [`AtomicMatcher`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    /// A code snippet in the target grammar with `$VAR` (single-node) and
    /// `$$$VAR` (variadic) metavariable holes.
    pub pattern: Option<String>,
    /// A bare tree-sitter node kind to match (e.g. `"call_expression"`).
    pub kind: Option<String>,
    /// A regular expression matched against node text.
    pub regex: Option<String>,
}

/// The resolved, exactly-one atomic matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomicMatcher {
    /// A snippet pattern with metavariable holes.
    Pattern(String),
    /// A tree-sitter node kind.
    Kind(String),
    /// A regex over node text.
    Regex(String),
}

impl Matcher {
    /// Collapse the optional fields into the single atomic form, rejecting
    /// the zero-or-many cases. Returns `Err` with a human-readable reason.
    pub fn resolve(&self) -> Result<AtomicMatcher, String> {
        let set: Vec<&str> = [
            self.pattern.as_ref().map(|_| "pattern"),
            self.kind.as_ref().map(|_| "kind"),
            self.regex.as_ref().map(|_| "regex"),
        ]
        .into_iter()
        .flatten()
        .collect();
        match set.as_slice() {
            [] => Err("rule block sets none of `pattern` / `kind` / `regex`".into()),
            [one] => Ok(match *one {
                "pattern" => AtomicMatcher::Pattern(self.pattern.clone().unwrap()),
                "kind" => AtomicMatcher::Kind(self.kind.clone().unwrap()),
                _ => AtomicMatcher::Regex(self.regex.clone().unwrap()),
            }),
            many => Err(format!(
                "rule block sets multiple matchers ({}); set exactly one",
                many.join(", ")
            )),
        }
    }
}

/// A single declarative rule.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Stable identifier (also the diagnostic code).
    pub id: String,
    /// Target language name (resolved via `harn_hostlib::ast::Language`).
    pub language: String,
    /// Diagnostic severity. Defaults to `warning`.
    #[serde(default)]
    pub severity: Severity,
    /// Human-readable diagnostic message. Empty for search-only rules.
    #[serde(default)]
    pub message: String,
    /// The atomic-tier matcher block.
    pub rule: Matcher,
    /// Replacement template. Its presence makes the rule a codemod.
    #[serde(default)]
    pub fix: Option<String>,
}

impl Rule {
    /// Derive the rule's kind from its shape (see [`RuleKind`]).
    pub fn kind(&self) -> RuleKind {
        if self.fix.is_some() {
            RuleKind::Codemod
        } else if self.message.is_empty() {
            RuleKind::Search
        } else {
            RuleKind::Lint
        }
    }

    /// Parse a single rule from a TOML document.
    pub fn from_toml_str(text: &str) -> Result<Self, Box<toml::de::Error>> {
        toml::from_str(text).map_err(Box::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_codemod_rule() {
        let rule = Rule::from_toml_str(
            r#"
            id = "destructure-default"
            language = "typescript"
            severity = "warning"
            message = "Collapse optional-chain default into a destructuring bind"
            fix = "{ $KEY: $SRC }"

            [rule]
            pattern = "$SRC?.$KEY ?? $DEFAULT"
            "#,
        )
        .expect("rule parses");
        assert_eq!(rule.id, "destructure-default");
        assert_eq!(rule.language, "typescript");
        assert_eq!(rule.severity, Severity::Warning);
        assert_eq!(rule.kind(), RuleKind::Codemod);
        assert_eq!(
            rule.rule.resolve().unwrap(),
            AtomicMatcher::Pattern("$SRC?.$KEY ?? $DEFAULT".into())
        );
    }

    #[test]
    fn severity_defaults_to_warning() {
        let rule = Rule::from_toml_str(
            r#"
            id = "x"
            language = "rust"
            [rule]
            kind = "macro_invocation"
            "#,
        )
        .unwrap();
        assert_eq!(rule.severity, Severity::Warning);
        // No message, no fix -> a search rule.
        assert_eq!(rule.kind(), RuleKind::Search);
    }

    #[test]
    fn lint_rule_has_message_no_fix() {
        let rule = Rule::from_toml_str(
            r#"
            id = "todo"
            language = "rust"
            message = "Found a TODO"
            [rule]
            regex = "TODO"
            "#,
        )
        .unwrap();
        assert_eq!(rule.kind(), RuleKind::Lint);
        assert_eq!(
            rule.rule.resolve().unwrap(),
            AtomicMatcher::Regex("TODO".into())
        );
    }

    #[test]
    fn rejects_multiple_matchers() {
        let rule = Rule::from_toml_str(
            r#"
            id = "x"
            language = "rust"
            [rule]
            kind = "foo"
            regex = "bar"
            "#,
        )
        .unwrap();
        assert!(rule.rule.resolve().is_err());
    }

    #[test]
    fn rejects_empty_matcher() {
        let rule = Rule::from_toml_str(
            r#"
            id = "x"
            language = "rust"
            [rule]
            "#,
        )
        .unwrap();
        assert!(rule.rule.resolve().is_err());
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let err = Rule::from_toml_str(
            r#"
            id = "x"
            language = "rust"
            bogus = true
            [rule]
            kind = "foo"
            "#,
        );
        assert!(err.is_err());
    }
}
