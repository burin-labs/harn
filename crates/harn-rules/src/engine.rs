//! Compile a [`Rule`] into a runnable matcher and run it against source.
//!
//! The atomic tier supports three matcher forms, all reduced to a single
//! [`RuleMatch`] stream:
//!
//! - `pattern` → compiled to a tree-sitter query via [`crate::pattern`].
//! - `kind` → the trivial query `(<kind>) @__match`.
//! - `regex` → a text regex over the source, yielding spans with no AST
//!   metavar bindings.

use std::collections::BTreeMap;

use harn_hostlib::ast::Language;
use serde::Serialize;

use crate::constraint::CompiledConstraint;
use crate::error::RulesError;
use crate::evaluator::CompiledRuleTree;
use crate::fix::{interpolate, splice, AppliedEdit};
use crate::model::{Applicability, Rule, Safety, Severity};
use crate::semantic::enrich_harn_matches;
use crate::transform::CompiledTransform;

/// A byte + row/col span. Rows/cols are 0-based, matching the rest of the
/// Harn AST wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span {
    /// Start byte offset.
    pub start_byte: usize,
    /// End byte offset (exclusive).
    pub end_byte: usize,
    /// 0-based start row.
    pub start_row: usize,
    /// 0-based start column.
    pub start_col: usize,
    /// 0-based end row.
    pub end_row: usize,
    /// 0-based end column.
    pub end_col: usize,
}

impl Span {
    pub(crate) fn of(node: tree_sitter::Node<'_>) -> Self {
        let start = node.start_position();
        let end = node.end_position();
        Span {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
        }
    }
}

/// Semantic metadata for a Harn capture. Populated for Harn sources when the
/// engine can resolve the captured node to a local declaration/binding or infer
/// a simple static type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BindingMetadata {
    /// Resolved Harn declaration/binding identity for this capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedBinding>,
    /// Static type label for this capture.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
}

impl BindingMetadata {
    /// True when no semantic metadata is available.
    pub fn is_empty(&self) -> bool {
        self.resolved.is_none() && self.ty.is_none()
    }
}

/// A resolved Harn declaration or binding identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedBinding {
    /// Stable id: `<kind>:<name>@<line>:<column>` (1-based line/column).
    pub id: String,
    /// Binding name.
    pub name: String,
    /// Binding kind (`fn`, `pipeline`, `tool`, `struct`, `type`, `let`,
    /// `var`, `const`, `param`, ...).
    pub kind: String,
    /// Span of the declaration/binding name.
    #[serde(flatten)]
    pub span: Span,
}

/// A metavariable binding: the captured text plus where it lives.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The captured source text.
    pub text: String,
    /// The captured node's span.
    pub span: Span,
    /// Optional Harn semantic metadata for this capture.
    pub metadata: BindingMetadata,
}

impl Binding {
    pub(crate) fn new(text: String, span: Span) -> Self {
        Binding {
            text,
            span,
            metadata: BindingMetadata::default(),
        }
    }
}

/// One match of a rule against a file.
#[derive(Debug, Clone)]
pub struct RuleMatch {
    /// The rule that produced this match.
    pub rule_id: String,
    /// The whole matched range (the pattern root, or the regex span).
    pub span: Span,
    /// The matched source text.
    pub text: String,
    /// Metavar bindings, keyed by name (without the leading `$`). Empty for
    /// `kind` and `regex` matchers.
    pub bindings: BTreeMap<String, Binding>,
}

/// The result of applying a codemod rule's `fix` to a source string.
#[derive(Debug, Clone)]
pub struct CodemodResult {
    /// The rewritten source (equals the input when nothing matched).
    pub rewritten: String,
    /// The per-match edits that were spliced in, in document order.
    pub edits: Vec<AppliedEdit>,
    /// Whether the rewrite changed the source.
    pub changed: bool,
    /// The rule's declared safety tier.
    pub safety: Safety,
    /// Whether the fix may be auto-applied or is opt-in only.
    pub applicability: Applicability,
    /// Whether re-running the fix on `rewritten` yields no further change
    /// (a fix should reach a fixed point).
    pub idempotent: bool,
}

/// A rule whose matcher has been compiled and is ready to run.
pub struct CompiledRule {
    rule_id: String,
    language: Language,
    execution: Execution,
    /// `where` predicates; a match survives only when all hold.
    constraints: Vec<CompiledConstraint>,
    /// `transform` definitions: (new metavar name, compiled transform).
    transforms: Vec<(String, CompiledTransform)>,
    /// The `fix` replacement template, if this is a codemod.
    fix: Option<String>,
    /// When set, the `fix` replaces only this capture's span (surgical
    /// sub-node rewrite) rather than the whole matched node.
    fix_target: Option<String>,
    /// The fix's safety tier (gates auto-apply).
    safety: Safety,
    /// The diagnostic message (empty for search-only rules).
    message: String,
    /// The diagnostic severity.
    severity: Severity,
}

/// A diagnostic produced by running a rule — the mapping surface onto the
/// linter's `LintDiagnostic` / `FixEdit` (Epic C / the LSP reuse this).
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// The rule id (also the diagnostic code).
    pub rule_id: String,
    /// The diagnostic message.
    pub message: String,
    /// The severity.
    pub severity: Severity,
    /// The flagged span.
    pub span: Span,
    /// Whether a fix, if present, is auto-applicable or a suggestion.
    pub applicability: Applicability,
    /// The interpolated fix replacement for this match, if the rule has a
    /// `fix`.
    pub fix: Option<String>,
}

enum Execution {
    /// A top-level pure-`regex` rule: scan the raw source text (grep-style),
    /// independent of the tree. Its match span is the regex match range.
    SourceRegex(regex::Regex),
    /// The full matching algebra (atomic + relational + composite).
    Tree(Box<CompiledRuleTree>),
}

impl CompiledRule {
    /// Resolve the rule's language and grammar, then compile its matcher.
    pub fn compile(rule: &Rule) -> Result<Self, RulesError> {
        let language =
            Language::from_name(&rule.language).ok_or_else(|| RulesError::UnknownLanguage {
                rule: rule.id.clone(),
                language: rule.language.clone(),
            })?;

        // A top-level pure-`regex` rule greps the source text directly; any
        // other shape (pattern / kind / relational / composite) compiles to
        // the tree-walking algebra.
        let execution = if rule.rule.is_pure_regex() {
            let pattern = rule.rule.regex.as_ref().expect("pure regex");
            Execution::SourceRegex(regex::Regex::new(pattern).map_err(|err| {
                RulesError::PatternCompile {
                    rule: rule.id.clone(),
                    message: format!("invalid regex `{pattern}`: {err}"),
                }
            })?)
        } else {
            Execution::Tree(Box::new(CompiledRuleTree::compile(
                &rule.id,
                language,
                &rule.rule,
                &rule.utils,
            )?))
        };

        let constraints = rule
            .where_constraints
            .iter()
            .map(|c| CompiledConstraint::compile(&rule.id, language, c))
            .collect::<Result<Vec<_>, _>>()?;

        let transforms = rule
            .transform
            .iter()
            .map(|(name, t)| {
                CompiledTransform::compile(&rule.id, name, t).map(|c| (name.clone(), c))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(CompiledRule {
            rule_id: rule.id.clone(),
            language,
            execution,
            constraints,
            transforms,
            fix: rule.fix.clone(),
            fix_target: rule.fix_target.clone(),
            safety: rule.safety,
            message: rule.message.clone(),
            severity: rule.severity,
        })
    }

    /// The language this rule targets.
    pub fn language(&self) -> Language {
        self.language
    }

    /// The fix's declared safety tier.
    pub fn safety(&self) -> Safety {
        self.safety
    }

    /// Whether this rule's fix may be auto-applied (machine-applicable) or
    /// is opt-in only (suggestion).
    pub fn applicability(&self) -> Applicability {
        self.safety.applicability()
    }

    /// The rule's id.
    pub fn id(&self) -> &str {
        &self.rule_id
    }

    /// The rule's diagnostic severity (the default for any diagnostic the
    /// rule produces).
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// The rule's static diagnostic message (empty for a search-only rule).
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Run the compiled rule against `source`, returning matches in
    /// document order. Matches that fail any `where` constraint are dropped.
    pub fn run(&self, source: &str) -> Result<Vec<RuleMatch>, RulesError> {
        let mut matches = match &self.execution {
            Execution::SourceRegex(regex) => self.run_regex(regex, source),
            Execution::Tree(tree) => tree
                .find(&self.rule_id, self.language, source)?
                .into_iter()
                .map(|m| RuleMatch {
                    rule_id: self.rule_id.clone(),
                    span: m.span,
                    text: m.text,
                    bindings: m.bindings,
                })
                .collect(),
        };
        if self.language == Language::Harn && !matches.is_empty() {
            enrich_harn_matches(source, &mut matches).map_err(|message| {
                RulesError::SourceParse {
                    rule: self.rule_id.clone(),
                    message,
                }
            })?;
        }
        if !self.constraints.is_empty() {
            matches.retain(|m| self.satisfies_constraints(m));
        }
        Ok(matches)
    }

    /// True when every `where` constraint holds for this match. A
    /// constraint whose metavar is unbound (not captured) fails closed.
    fn satisfies_constraints(&self, m: &RuleMatch) -> bool {
        self.constraints
            .iter()
            .all(|c| m.bindings.get(&c.metavar).is_some_and(|b| c.evaluate(b)))
    }

    /// Apply this codemod rule's `fix` to `source`, returning the rewritten
    /// text plus the per-match edits. Each match's `fix` template is
    /// interpolated from its captured metavars plus any `transform`-derived
    /// ones. Errors if the rule has no `fix`.
    ///
    /// This computes the preview only — it does not enforce the safety gate.
    /// Use [`CompiledRule::auto_apply`] to refuse non-machine-applicable
    /// fixes, or [`CompiledRule::apply_checked`] to also assert idempotency.
    pub fn apply(&self, source: &str) -> Result<CodemodResult, RulesError> {
        let (rewritten, edits) = self.rewrite(source)?;
        let changed = rewritten != source;
        // Idempotency: re-running the fix on its own output must not change
        // it further (the fix should reach a fixed point).
        let (twice, _) = self.rewrite(&rewritten)?;
        let idempotent = twice == rewritten;
        Ok(CodemodResult {
            rewritten,
            edits,
            changed,
            safety: self.safety,
            applicability: self.applicability(),
            idempotent,
        })
    }

    /// Like [`CompiledRule::apply`], but refuses to apply a fix whose
    /// `safety` is above the machine-applicable threshold (`scope-local` and
    /// riskier). This is the gate `harn codemod --apply` uses by default.
    pub fn auto_apply(&self, source: &str) -> Result<CodemodResult, RulesError> {
        if !self.safety.is_auto_applicable() {
            return Err(RulesError::NotAutoApplicable {
                rule: self.rule_id.clone(),
                safety: format!("{:?}", self.safety),
            });
        }
        self.apply(source)
    }

    /// Like [`CompiledRule::apply`], but fails if the fix is not idempotent.
    /// Used by the codemod runner and the rule-test harness to assert a fix
    /// reaches a fixed point.
    pub fn apply_checked(&self, source: &str) -> Result<CodemodResult, RulesError> {
        let result = self.apply(source)?;
        if !result.idempotent {
            return Err(RulesError::NotIdempotent {
                rule: self.rule_id.clone(),
            });
        }
        Ok(result)
    }

    /// Run the rule and produce one [`Diagnostic`] per match — the surface
    /// the linter (Epic C) and the LSP convert into `LintDiagnostic` /
    /// `FixEdit`. Each diagnostic carries the interpolated fix (if any) and
    /// its applicability tier.
    pub fn diagnostics(&self, source: &str) -> Result<Vec<Diagnostic>, RulesError> {
        let applicability = self.applicability();
        let matches = self.run(source)?;
        matches
            .iter()
            .map(|m| {
                // With a `fixTarget`, the fix replaces only the target
                // capture's span, so the diagnostic (which the LSP applies
                // `fix` over) is anchored there; otherwise the whole match.
                let span = if self.fix.is_some() && self.fix_target.is_some() {
                    self.edit_target(m)?.0
                } else {
                    m.span
                };
                Ok(Diagnostic {
                    rule_id: self.rule_id.clone(),
                    message: self.message.clone(),
                    severity: self.severity,
                    span,
                    applicability,
                    fix: self.fix.as_ref().map(|template| {
                        let vars = self.metavars_for(m);
                        interpolate(template, &vars)
                    }),
                })
            })
            .collect()
    }

    /// The core rewrite: run the rule and splice each match's interpolated
    /// fix. Returns the rewritten text and the edits.
    fn rewrite(&self, source: &str) -> Result<(String, Vec<AppliedEdit>), RulesError> {
        let template = self
            .fix
            .as_ref()
            .ok_or_else(|| RulesError::PatternCompile {
                rule: self.rule_id.clone(),
                message: "apply requires a `fix` template; this rule has none".into(),
            })?;

        let matches = dedupe_overlapping(self.run(source)?);
        let edits: Vec<AppliedEdit> = matches
            .iter()
            .map(|m| {
                let vars = self.metavars_for(m);
                let (span, before) = self.edit_target(m)?;
                Ok(AppliedEdit {
                    span,
                    before,
                    replacement: interpolate(template, &vars),
                })
            })
            .collect::<Result<_, RulesError>>()?;
        Ok((splice(source, &edits), edits))
    }

    /// The span + original text a match's `fix` replaces. Honors `fixTarget`
    /// (surgical sub-node rewrite); defaults to the whole matched node.
    fn edit_target(&self, m: &RuleMatch) -> Result<(Span, String), RulesError> {
        match &self.fix_target {
            None => Ok((m.span, m.text.clone())),
            Some(name) => {
                let binding =
                    m.bindings
                        .get(name)
                        .ok_or_else(|| RulesError::PatternCompile {
                            rule: self.rule_id.clone(),
                            message: format!(
                                "fixTarget `{name}` is not bound by this match; name a capture the matcher always binds"
                            ),
                        })?;
                Ok((binding.span, binding.text.clone()))
            }
        }
    }

    /// Build the full metavar map for a match: captured bindings plus the
    /// `transform`-synthesized metavars (which may shadow captures).
    fn metavars_for(&self, m: &RuleMatch) -> BTreeMap<String, String> {
        let mut vars: BTreeMap<String, String> = m
            .bindings
            .iter()
            .map(|(name, binding)| (name.clone(), binding.text.clone()))
            .collect();
        for (name, transform) in &self.transforms {
            let input = m
                .bindings
                .get(&transform.source)
                .map(|b| b.text.as_str())
                .unwrap_or("");
            vars.insert(name.clone(), transform.apply(input));
        }
        vars
    }

    fn run_regex(&self, regex: &regex::Regex, source: &str) -> Vec<RuleMatch> {
        let mut matches = Vec::new();
        // `find_iter` yields non-overlapping matches in ascending byte order, so
        // a single forward-walking cursor computes every row/col without
        // rescanning from the start of the document for each match (which made
        // this O(matches × len) — quadratic on files with many hits).
        let mut cursor = RowColCursor::new(source);
        for m in regex.find_iter(source) {
            let (start_row, start_col) = cursor.advance_to(m.start());
            let (end_row, end_col) = cursor.advance_to(m.end());
            matches.push(RuleMatch {
                rule_id: self.rule_id.clone(),
                span: Span {
                    start_byte: m.start(),
                    end_byte: m.end(),
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                },
                text: m.as_str().to_string(),
                bindings: BTreeMap::new(),
            });
        }
        matches
    }
}

/// Drop matches that overlap an already-kept match, keeping the **outermost**
/// (and, among equal extents, the first). A tree query naturally yields nested
/// matches — e.g. `$X + $Y` matches both `(a+b)+c` and its inner `a+b` — and
/// splicing both would apply overlapping byte edits, corrupting the output or
/// panicking `replace_range` on a stale offset. A codemod must rewrite each
/// region once, so we keep the enclosing match and discard anything nested in
/// or straddling it. Matches are returned in document (start-byte) order.
fn dedupe_overlapping(mut matches: Vec<RuleMatch>) -> Vec<RuleMatch> {
    // Outermost-first: smallest start, then largest end (widest span wins a tie
    // on start). After filtering we restore document order.
    matches.sort_by(|a, b| {
        a.span
            .start_byte
            .cmp(&b.span.start_byte)
            .then(b.span.end_byte.cmp(&a.span.end_byte))
    });
    let mut kept: Vec<RuleMatch> = Vec::with_capacity(matches.len());
    let mut covered_to = 0usize; // exclusive end of the last kept span
    for m in matches {
        // Keep only when this match starts at or after the end of the last kept
        // one (adjacency is fine; any earlier start means it is nested in, or
        // straddles, an already-kept span).
        if m.span.start_byte >= covered_to {
            covered_to = m.span.end_byte.max(covered_to);
            kept.push(m);
        }
    }
    kept
}

/// Forward-only cursor that maps byte offsets to `(row, col)` while walking a
/// document at most once. The regex matcher has no tree-sitter node to read
/// positions from and visits offsets in ascending order, so advancing this
/// cursor avoids the per-match rescan a stateless lookup would cost.
struct RowColCursor<'a> {
    source: &'a str,
    byte: usize,
    row: usize,
    col: usize,
}

impl<'a> RowColCursor<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            byte: 0,
            row: 0,
            col: 0,
        }
    }

    /// Advance to `target` (which must be `>=` the current position and a char
    /// boundary) and return the row/col there. `row` counts preceding newlines;
    /// `col` counts characters since the last newline.
    fn advance_to(&mut self, target: usize) -> (usize, usize) {
        for ch in self.source[self.byte..target].chars() {
            if ch == '\n' {
                self.row += 1;
                self.col = 0;
            } else {
                self.col += 1;
            }
        }
        self.byte = target;
        (self.row, self.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    fn rule(toml: &str) -> CompiledRule {
        let parsed = Rule::from_toml_str(toml).expect("rule parses");
        CompiledRule::compile(&parsed).expect("rule compiles")
    }

    #[test]
    fn pattern_rule_binds_metavars() {
        let compiled = rule(
            r#"
            id = "destructure-default"
            language = "typescript"
            fix = "{ $KEY: $SRC }"
            [rule]
            pattern = "$SRC?.$KEY ?? $DEFAULT"
            "#,
        );
        let matches = compiled
            .run("const a = cfg?.timeout ?? 30;\nconst b = opts?.retries ?? 3;\n")
            .unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].bindings["SRC"].text, "cfg");
        assert_eq!(matches[0].bindings["KEY"].text, "timeout");
        assert_eq!(matches[0].bindings["DEFAULT"].text, "30");
        assert_eq!(matches[1].bindings["SRC"].text, "opts");
        // The match span covers the whole expression.
        assert_eq!(matches[0].text, "cfg?.timeout ?? 30");
        assert_eq!(matches[0].span.start_row, 0);
        assert_eq!(matches[1].span.start_row, 1);
    }

    #[test]
    fn nested_matches_do_not_corrupt_or_panic_on_apply() {
        // `$X + $Y` matches the outer `(a+b)+c` AND the inner `a+b` (distinct
        // spans). Without overlap resolution, splicing both byte-edits panics
        // `replace_range` on a stale offset or corrupts the output. The engine
        // must keep the outermost match and rewrite the region exactly once.
        let compiled = rule(
            r#"
            id = "sum-binop"
            language = "typescript"
            fix = "sum($X, $Y)"
            [rule]
            pattern = "$X + $Y"
            "#,
        );
        // More than one match exists (outer + inner) before dedup.
        assert!(compiled.run("const z = a + b + c;\n").unwrap().len() >= 2);
        let result = compiled.apply("const z = a + b + c;\n").unwrap();
        // Exactly one outer rewrite; inner `a + b` survives verbatim inside $X.
        assert_eq!(result.rewritten, "const z = sum(a + b, c);\n");
        assert_eq!(result.edits.len(), 1);
        assert!(result.changed);
    }

    #[test]
    fn dedupe_overlapping_keeps_outermost_in_document_order() {
        let span = |s: usize, e: usize| Span {
            start_byte: s,
            end_byte: e,
            start_row: 0,
            start_col: s,
            end_row: 0,
            end_col: e,
        };
        let m = |s: usize, e: usize| RuleMatch {
            rule_id: "r".into(),
            span: span(s, e),
            text: String::new(),
            bindings: BTreeMap::new(),
        };
        // outer [0,9) contains inner [0,5); [10,14) is disjoint.
        let kept = dedupe_overlapping(vec![m(0, 5), m(0, 9), m(10, 14)]);
        let spans: Vec<_> = kept
            .iter()
            .map(|m| (m.span.start_byte, m.span.end_byte))
            .collect();
        assert_eq!(spans, vec![(0, 9), (10, 14)]);
    }

    #[test]
    fn kind_rule_matches_node_kind() {
        let compiled = rule(
            r#"
            id = "find-calls"
            language = "python"
            [rule]
            kind = "call"
            "#,
        );
        let matches = compiled.run("print(x)\nlog(y)\n").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].text, "print(x)");
        assert!(matches[0].bindings.is_empty());
    }

    #[test]
    fn regex_rule_matches_text() {
        let compiled = rule(
            r#"
            id = "todo"
            language = "rust"
            message = "Found a TODO"
            [rule]
            regex = "TODO\\(\\w+\\)"
            "#,
        );
        let matches = compiled
            .run("fn f() {\n    // TODO(ken) fix\n    // todo lower\n}\n")
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "TODO(ken)");
        assert_eq!(matches[0].span.start_row, 1);
    }

    #[test]
    fn raw_query_with_fix_target_rewrites_only_the_capture() {
        // Surgical sub-node rewrite: swap the `let` keyword to `const` while
        // preserving the type annotation and initializer — the failure mode a
        // whole-match `pattern`+`fix` cannot avoid (it drops the un-captured
        // `: Int`). The raw query roots the match at `let_binding` (`@__match`)
        // and captures the keyword token (`@kw`); `fixTarget` splices only it.
        let compiled = rule(
            r#"
            id = "let-to-const"
            language = "harn"
            fix = "const"
            fixTarget = "kw"
            [rule]
            query = '(let_binding "let" @kw) @__match'
            "#,
        );
        let result = compiled
            .apply("fn f() {\n  let x: Int = 1\n  let y = 2\n}\n")
            .unwrap();
        assert_eq!(
            result.rewritten,
            "fn f() {\n  const x: Int = 1\n  const y = 2\n}\n"
        );
        assert_eq!(result.edits.len(), 2);
        // Each edit replaced exactly the 3-byte `let` keyword, not the binding.
        assert!(result.edits.iter().all(|e| e.before == "let"));
        assert!(result.changed);
        // Re-running is a no-op (there are no more `let` keywords).
        assert!(compiled.apply(&result.rewritten).unwrap().rewritten == result.rewritten);
    }

    #[test]
    fn raw_query_without_root_capture_is_a_compile_error() {
        let parsed = Rule::from_toml_str(
            r#"
            id = "no-root"
            language = "harn"
            fix = "const"
            [rule]
            query = '(let_binding "let" @kw)'
            "#,
        )
        .unwrap();
        let compiled = CompiledRule::compile(&parsed);
        assert!(
            matches!(&compiled, Err(RulesError::PatternCompile { message, .. }) if message.contains("__match")),
            "expected a missing-@__match compile error"
        );
    }

    #[test]
    fn fix_target_naming_an_unbound_capture_errors_on_apply() {
        let compiled = rule(
            r#"
            id = "bad-target"
            language = "harn"
            fix = "const"
            fixTarget = "missing"
            [rule]
            query = '(let_binding "let" @kw) @__match'
            "#,
        );
        let result = compiled.apply("fn f() {\n  let x = 1\n}\n");
        assert!(
            matches!(&result, Err(RulesError::PatternCompile { message, .. }) if message.contains("missing")),
            "expected an unbound-fixTarget error"
        );
    }

    #[test]
    fn unknown_language_is_an_error() {
        let parsed = Rule::from_toml_str(
            r#"
            id = "x"
            language = "cobol"
            [rule]
            kind = "foo"
            "#,
        )
        .unwrap();
        assert!(matches!(
            CompiledRule::compile(&parsed),
            Err(RulesError::UnknownLanguage { .. })
        ));
    }

    #[test]
    fn invalid_pattern_surfaces_compile_error() {
        let parsed = Rule::from_toml_str(
            r#"
            id = "x"
            language = "typescript"
            [rule]
            pattern = "foo($$$ARGS)"
            "#,
        )
        .unwrap();
        assert!(matches!(
            CompiledRule::compile(&parsed),
            Err(RulesError::PatternCompile { .. })
        ));
    }

    #[test]
    fn harn_resolves_same_named_call_sites_by_binding_identity() {
        let compiled = rule(
            r#"
            id = "top-level-target"
            language = "harn"
            [rule]
            pattern = "$FN($ARG)"

            [[where]]
            metavar = "FN"
            resolvesTo = { name = "target", kind = "fn", line = 1 }
            "#,
        );
        let source = r"fn target(value: int) -> int {
  return value
}

fn call_shadowed(target: fn(int) -> int) {
  target(1)
}

fn call_global() {
  target(2)
}
";
        let matches = compiled.run(source).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text, "target(2)");
        let binding = &matches[0].bindings["FN"];
        let resolved = binding.metadata.resolved.as_ref().unwrap();
        assert_eq!(resolved.name, "target");
        assert_eq!(resolved.kind, "fn");
        assert_eq!(resolved.span.start_row, 0);
        assert_eq!(binding.metadata.ty.as_deref(), Some("fn(int) -> int"));
    }

    #[test]
    fn harn_capture_type_constraint_filters_matches() {
        let compiled = rule(
            r#"
            id = "int-logs"
            language = "harn"
            [rule]
            pattern = "log($VALUE)"

            [[where]]
            metavar = "VALUE"
            type = "int"
            "#,
        );
        let source = r#"fn main() {
  let count: int = 1
  let label: string = "one"
  log(count)
  log(label)
}
"#;
        let matches = compiled.run(source).unwrap();
        assert_eq!(matches.len(), 1);
        let value = &matches[0].bindings["VALUE"];
        assert_eq!(value.text, "count");
        assert_eq!(value.metadata.ty.as_deref(), Some("int"));
        assert_eq!(
            value
                .metadata
                .resolved
                .as_ref()
                .map(|resolved| resolved.kind.as_str()),
            Some("let")
        );
    }

    #[test]
    fn harn_initializer_uses_outer_binding_scope() {
        let compiled = rule(
            r#"
            id = "outer-initializer"
            language = "harn"
            [rule]
            pattern = "log($VALUE)"

            [[where]]
            metavar = "VALUE"
            resolvesTo = { name = "value", kind = "let", line = 2 }
            "#,
        );
        let source = r"fn main() {
  let value: int = 1
  if true {
    let value: string = log(value)
  }
}
";
        let matches = compiled.run(source).unwrap();
        assert_eq!(matches.len(), 1);
        let value = &matches[0].bindings["VALUE"];
        assert_eq!(value.text, "value");
        assert_eq!(
            value
                .metadata
                .resolved
                .as_ref()
                .map(|resolved| resolved.span.start_row),
            Some(1)
        );
    }
}
