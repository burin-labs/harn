//! The declarative rule data model.
//!
//! A rule is the atomic unit the engine consumes: an identity (`id`,
//! `language`, `severity`, `message`), a `rule` block describing *what to
//! match* (the atomic tier: `pattern` snippet, `kind`, or `regex`), and an
//! optional `fix` describing *how to rewrite* it. Relational/composite
//! matching (#2833) and `where`/`transform` (#2834) extend this model;
//! this module is the atomic-tier surface they build on.

use std::collections::BTreeMap;

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

impl Severity {
    /// The stable lowercase name (the inverse of the `Deserialize` rename),
    /// used for diagnostics and JSON surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// How risky a rule's `fix` is, mapped onto Burin's edit-safety taxonomy.
/// Ordered least → most dangerous; the codemod runner auto-applies only the
/// two safest tiers (see [`Safety::applicability`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Safety {
    /// Whitespace / formatting only.
    FormatOnly,
    /// Semantics-preserving rewrite.
    BehaviorPreserving,
    /// Changes behavior, but only within the matched scope. **Default** —
    /// conservative, so an undeclared codemod does not silently auto-apply.
    #[default]
    ScopeLocal,
    /// Changes an externally-visible surface (a signature, an export).
    SurfaceChanging,
    /// Changes capabilities / effects (I/O, permissions).
    CapabilityChanging,
    /// Always requires a human in the loop.
    NeedsHuman,
}

/// Whether a fix may be auto-applied (clippy/ESLint `machine-applicable`)
/// or is opt-in only (`suggestion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// Safe to auto-apply (`format-only` / `behavior-preserving`).
    MachineApplicable,
    /// Preview / opt-in only.
    Suggestion,
}

impl Applicability {
    /// The stable name used for diagnostics and JSON surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            Applicability::MachineApplicable => "machine-applicable",
            Applicability::Suggestion => "suggestion",
        }
    }
}

impl Safety {
    /// The stable kebab-case name (the inverse of the `Deserialize` rename),
    /// used for diagnostics and JSON surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            Safety::FormatOnly => "format-only",
            Safety::BehaviorPreserving => "behavior-preserving",
            Safety::ScopeLocal => "scope-local",
            Safety::SurfaceChanging => "surface-changing",
            Safety::CapabilityChanging => "capability-changing",
            Safety::NeedsHuman => "needs-human",
        }
    }

    /// The applicability tier this safety level maps to. `format-only` and
    /// `behavior-preserving` are machine-applicable; everything riskier is a
    /// suggestion.
    pub fn applicability(self) -> Applicability {
        if self <= Safety::BehaviorPreserving {
            Applicability::MachineApplicable
        } else {
            Applicability::Suggestion
        }
    }

    /// True when the runner may auto-apply this rule's fix without an
    /// explicit opt-in.
    pub fn is_auto_applicable(self) -> bool {
        self.applicability() == Applicability::MachineApplicable
    }
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
/// must be set on a node that carries one; [`RuleNode::atomic`] resolves it.
///
/// A `RuleNode` is the recursive matching algebra: an optional **atomic**
/// leaf (`pattern` / `kind` / `regex`), **relational** constraints
/// (`inside` / `has` / `follows` / `precedes`, each a sub-node tuned by
/// `stop_by` / `field`), and **composite** combinators (`all` / `any` /
/// `not` / `matches`). Every key set on a node is ANDed: the node matches a
/// tree-sitter node iff its atomic part matches *and* every relational and
/// composite part holds.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleNode {
    /// A code snippet in the target grammar with `$VAR` metavariable holes.
    pub pattern: Option<String>,
    /// A bare tree-sitter node kind (e.g. `"call_expression"`).
    pub kind: Option<String>,
    /// A regular expression matched against node text.
    pub regex: Option<String>,

    /// The node must be **inside** a node matching this sub-rule (ancestor).
    pub inside: Option<Box<RuleNode>>,
    /// The node must **have** a descendant matching this sub-rule.
    pub has: Option<Box<RuleNode>>,
    /// The node must **follow** a node matching this sub-rule (earlier).
    pub follows: Option<Box<RuleNode>>,
    /// The node must **precede** a node matching this sub-rule (later).
    pub precedes: Option<Box<RuleNode>>,

    /// Relational reach (used when this node is an `inside`/`has`/… target):
    /// `neighbor` (direct only, default), `end` (transitive), or a rule that
    /// halts the walk. (TOML `stopBy` or `stop_by`.)
    #[serde(default, alias = "stopBy")]
    pub stop_by: Option<StopBy>,
    /// Restrict an `inside`/`has` relation to a specific tree-sitter field.
    pub field: Option<String>,

    /// Every sub-rule must match the node.
    pub all: Option<Vec<RuleNode>>,
    /// At least one sub-rule must match the node.
    pub any: Option<Vec<RuleNode>>,
    /// The sub-rule must NOT match the node.
    pub not: Option<Box<RuleNode>>,
    /// Reference a utility rule by id (resolved from `[utils]`).
    pub matches: Option<String>,
}

/// How far a relational op (`inside` / `has` / `follows` / `precedes`)
/// walks the tree looking for a match.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StopBy {
    /// `"neighbor"` (direct parent/child/sibling only) or `"end"`
    /// (transitive — walk to the tree boundary).
    Keyword(StopKeyword),
    /// Walk until a node matching this rule is reached, then stop.
    Rule(Box<RuleNode>),
}

/// The keyword forms of [`StopBy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopKeyword {
    /// Only the immediate neighbor (default).
    Neighbor,
    /// Transitive — walk all the way to the tree boundary.
    End,
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

impl RuleNode {
    /// Resolve this node's atomic leaf. `Ok(None)` when the node is purely
    /// relational/composite; `Err` when more than one atomic key is set.
    pub fn atomic(&self) -> Result<Option<AtomicMatcher>, String> {
        let set: Vec<&str> = [
            self.pattern.as_ref().map(|_| "pattern"),
            self.kind.as_ref().map(|_| "kind"),
            self.regex.as_ref().map(|_| "regex"),
        ]
        .into_iter()
        .flatten()
        .collect();
        match set.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(match *one {
                "pattern" => AtomicMatcher::Pattern(self.pattern.clone().unwrap()),
                "kind" => AtomicMatcher::Kind(self.kind.clone().unwrap()),
                _ => AtomicMatcher::Regex(self.regex.clone().unwrap()),
            })),
            many => Err(format!(
                "rule node sets multiple atomic matchers ({}); set at most one",
                many.join(", ")
            )),
        }
    }

    /// True when `regex` is the only key set — a top-level grep-style rule
    /// that scans source text rather than the tree.
    pub fn is_pure_regex(&self) -> bool {
        self.regex.is_some()
            && self.pattern.is_none()
            && self.kind.is_none()
            && self.inside.is_none()
            && self.has.is_none()
            && self.follows.is_none()
            && self.precedes.is_none()
            && self.all.is_none()
            && self.any.is_none()
            && self.not.is_none()
            && self.matches.is_none()
    }

    /// True when the node sets no matching keys at all (an empty node, which
    /// is a rule authoring error).
    pub fn is_empty(&self) -> bool {
        self.pattern.is_none()
            && self.kind.is_none()
            && self.regex.is_none()
            && self.inside.is_none()
            && self.has.is_none()
            && self.follows.is_none()
            && self.precedes.is_none()
            && self.all.is_none()
            && self.any.is_none()
            && self.not.is_none()
            && self.matches.is_none()
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
    /// How risky the `fix` is. Gates auto-apply. Defaults to `scope-local`.
    #[serde(default)]
    pub safety: Safety,
    /// The matcher block (atomic / relational / composite algebra).
    pub rule: RuleNode,
    /// Local utility rules referenced by `matches`, keyed by id.
    /// (TOML `[utils.NAME]`.)
    #[serde(default)]
    pub utils: BTreeMap<String, RuleNode>,
    /// Predicates on captured metavars; a match survives only when every
    /// constraint holds. (TOML `[[where]]`.)
    #[serde(default, rename = "where")]
    pub where_constraints: Vec<Constraint>,
    /// Derived metavars synthesized from captured ones before `fix`
    /// interpolation, keyed by the new metavar name. (TOML `[transform.X]`.)
    #[serde(default)]
    pub transform: BTreeMap<String, Transform>,
    /// Replacement template. Its presence makes the rule a codemod.
    #[serde(default)]
    pub fix: Option<String>,
}

/// A `where` predicate on a captured metavar. Exactly one of `regex` /
/// `comparison` / `pattern` / `resolves_to` / `type` is set.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    /// The metavar this constraint applies to (without the leading `$`).
    pub metavar: String,
    /// The metavar's text must match this regex.
    #[serde(default)]
    pub regex: Option<String>,
    /// The metavar's text, parsed as a number, must satisfy this
    /// comparison (Semgrep `metavariable-comparison`).
    #[serde(default)]
    pub comparison: Option<Comparison>,
    /// A sub-pattern (Semgrep `metavariable-pattern`) run against the
    /// metavar's captured text; the constraint holds when it matches.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Harn-only semantic filter: the captured node must resolve to a
    /// declaration/binding matching this identity.
    #[serde(default, alias = "resolvesTo")]
    pub resolves_to: Option<ResolvedBindingConstraint>,
    /// Harn-only semantic filter: the capture's attributed type must equal
    /// this label.
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    /// Optional language override for `pattern` — lets a captured string
    /// literal be matched in a different grammar than the host file.
    #[serde(default)]
    pub language: Option<String>,
}

/// A Harn resolved-binding predicate for [`Constraint::resolves_to`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedBindingConstraint {
    /// Exact resolved id (`<kind>:<name>@<line>:<column>`), if supplied.
    #[serde(default)]
    pub id: Option<String>,
    /// Binding/declaration name.
    #[serde(default)]
    pub name: Option<String>,
    /// Binding kind (`fn`, `param`, `let`, ...).
    #[serde(default)]
    pub kind: Option<String>,
    /// 1-based declaration/binding line.
    #[serde(default)]
    pub line: Option<usize>,
    /// 1-based declaration/binding column.
    #[serde(default)]
    pub column: Option<usize>,
}

/// A numeric/string comparison for a [`Constraint`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    /// One of `<` `<=` `>` `>=` `==` `!=`.
    pub op: String,
    /// The right-hand side. Numbers compare numerically; strings/bools
    /// compare with `==` / `!=` only.
    pub value: toml::Value,
}

/// A metavar transform: read `source`, apply exactly one operation, bind
/// the result under a new metavar name (the map key).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    /// The source metavar name (without `$`) whose text is transformed.
    pub source: String,
    /// Regex find/replace.
    #[serde(default)]
    pub replace: Option<ReplaceOp>,
    /// A character-index slice.
    #[serde(default)]
    pub substring: Option<SubstringOp>,
    /// A case conversion.
    #[serde(default)]
    pub convert: Option<ConvertOp>,
}

/// Regex find/replace transform op.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceOp {
    /// The regex to find.
    pub regex: String,
    /// The replacement (supports `$1` capture refs).
    pub by: String,
}

/// Character-slice transform op. Indices are 0-based char offsets; a
/// negative or omitted bound clamps to the string end.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubstringOp {
    /// Inclusive start char index (default 0).
    #[serde(default)]
    pub start: Option<i64>,
    /// Exclusive end char index (default: end of string).
    #[serde(default)]
    pub end: Option<i64>,
}

/// Case-conversion transform op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvertOp {
    /// `lowerCamelCase`.
    LowerCamel,
    /// `UpperCamelCase`.
    UpperCamel,
    /// `snake_case`.
    Snake,
    /// `SCREAMING_SNAKE_CASE`.
    ScreamingSnake,
    /// `kebab-case`.
    Kebab,
    /// `lowercase`.
    Lower,
    /// `UPPERCASE`.
    Upper,
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
            rule.rule.atomic().unwrap(),
            Some(AtomicMatcher::Pattern("$SRC?.$KEY ?? $DEFAULT".into()))
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
            rule.rule.atomic().unwrap(),
            Some(AtomicMatcher::Regex("TODO".into()))
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
        assert!(rule.rule.atomic().is_err());
    }

    #[test]
    fn empty_matcher_is_detectable() {
        let rule = Rule::from_toml_str(
            r#"
            id = "x"
            language = "rust"
            [rule]
            "#,
        )
        .unwrap();
        // An empty node sets no atomic key (Ok(None)) and is flagged empty.
        assert_eq!(rule.rule.atomic().unwrap(), None);
        assert!(rule.rule.is_empty());
    }

    #[test]
    fn parses_relational_and_composite_keys() {
        let rule = Rule::from_toml_str(
            r#"
            id = "nested"
            language = "typescript"
            [rule]
            pattern = "let $NAME = $INIT"
            [rule.inside]
            kind = "statement_block"
            stopBy = "end"
            [rule.not.inside]
            kind = "try_statement"
            stopBy = "end"
            "#,
        )
        .expect("parses");
        assert!(rule.rule.inside.is_some());
        assert!(rule.rule.not.is_some());
        assert!(rule.rule.not.as_ref().unwrap().inside.is_some());
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

    #[test]
    fn parses_semantic_where_constraints() {
        let rule = Rule::from_toml_str(
            r#"
            id = "x"
            language = "harn"
            [rule]
            pattern = "$FN($ARG)"

            [[where]]
            metavar = "FN"
            resolvesTo = { name = "target", kind = "fn", line = 1, column = 4 }

            [[where]]
            metavar = "ARG"
            type = "int"
            "#,
        )
        .unwrap();
        assert_eq!(rule.where_constraints.len(), 2);
        let resolved = rule.where_constraints[0].resolves_to.as_ref().unwrap();
        assert_eq!(resolved.name.as_deref(), Some("target"));
        assert_eq!(resolved.kind.as_deref(), Some("fn"));
        assert_eq!(resolved.line, Some(1));
        assert_eq!(resolved.column, Some(4));
        assert_eq!(rule.where_constraints[1].type_.as_deref(), Some("int"));
    }
}
