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

use crate::constraint::CompiledConstraint;
use crate::error::RulesError;
use crate::evaluator::CompiledRuleTree;
use crate::fix::{interpolate, splice, AppliedEdit};
use crate::model::Rule;
use crate::transform::CompiledTransform;

/// A byte + row/col span. Rows/cols are 0-based, matching the rest of the
/// Harn AST wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A metavariable binding: the captured text plus where it lives.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The captured source text.
    pub text: String,
    /// The captured node's span.
    pub span: Span,
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
        })
    }

    /// The language this rule targets.
    pub fn language(&self) -> Language {
        self.language
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
        if !self.constraints.is_empty() {
            matches.retain(|m| self.satisfies_constraints(m));
        }
        Ok(matches)
    }

    /// True when every `where` constraint holds for this match. A
    /// constraint whose metavar is unbound (not captured) fails closed.
    fn satisfies_constraints(&self, m: &RuleMatch) -> bool {
        self.constraints.iter().all(|c| {
            m.bindings
                .get(&c.metavar)
                .is_some_and(|b| c.evaluate(&b.text))
        })
    }

    /// Apply this codemod rule's `fix` to `source`, returning the rewritten
    /// text plus the per-match edits. Each match's `fix` template is
    /// interpolated from its captured metavars plus any `transform`-derived
    /// ones. Errors if the rule has no `fix`.
    pub fn apply(&self, source: &str) -> Result<CodemodResult, RulesError> {
        let template = self
            .fix
            .as_ref()
            .ok_or_else(|| RulesError::PatternCompile {
                rule: self.rule_id.clone(),
                message: "apply() requires a `fix` template; this rule has none".into(),
            })?;

        let matches = self.run(source)?;
        let edits: Vec<AppliedEdit> = matches
            .iter()
            .map(|m| {
                let vars = self.metavars_for(m);
                AppliedEdit {
                    span: m.span,
                    before: m.text.clone(),
                    replacement: interpolate(template, &vars),
                }
            })
            .collect();

        let rewritten = splice(source, &edits);
        let changed = rewritten != source;
        Ok(CodemodResult {
            rewritten,
            edits,
            changed,
        })
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
        for m in regex.find_iter(source) {
            let span = byte_span(source, m.start(), m.end());
            matches.push(RuleMatch {
                rule_id: self.rule_id.clone(),
                span,
                text: m.as_str().to_string(),
                bindings: BTreeMap::new(),
            });
        }
        matches
    }
}

/// Compute a [`Span`] for a byte range by counting rows/cols. Used by the
/// regex matcher, which has no tree-sitter node to read positions from.
fn byte_span(source: &str, start: usize, end: usize) -> Span {
    let (start_row, start_col) = row_col(source, start);
    let (end_row, end_col) = row_col(source, end);
    Span {
        start_byte: start,
        end_byte: end,
        start_row,
        start_col,
        end_row,
        end_col,
    }
}

fn row_col(source: &str, byte: usize) -> (usize, usize) {
    let mut row = 0;
    let mut col = 0;
    for (i, ch) in source.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
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
}
