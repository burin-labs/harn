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

use harn_hostlib::ast::{api, Language};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

use crate::constraint::CompiledConstraint;
use crate::error::RulesError;
use crate::fix::{interpolate, splice, AppliedEdit};
use crate::model::{AtomicMatcher, Rule};
use crate::pattern::{compile_pattern, ROOT_CAPTURE};
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
    fn of(node: tree_sitter::Node<'_>) -> Self {
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
    matcher: CompiledMatcher,
    /// `where` predicates; a match survives only when all hold.
    constraints: Vec<CompiledConstraint>,
    /// `transform` definitions: (new metavar name, compiled transform).
    transforms: Vec<(String, CompiledTransform)>,
    /// The `fix` replacement template, if this is a codemod.
    fix: Option<String>,
}

enum CompiledMatcher {
    /// A tree-sitter query plus the metavar names to extract. Covers both
    /// `pattern` and `kind` forms.
    Query { query: Query, metavars: Vec<String> },
    /// A text regex over the whole source.
    Regex(regex::Regex),
}

impl CompiledRule {
    /// Resolve the rule's language and grammar, then compile its matcher.
    pub fn compile(rule: &Rule) -> Result<Self, RulesError> {
        let language =
            Language::from_name(&rule.language).ok_or_else(|| RulesError::UnknownLanguage {
                rule: rule.id.clone(),
                language: rule.language.clone(),
            })?;

        let matcher = match rule
            .rule
            .resolve()
            .map_err(|message| RulesError::PatternCompile {
                rule: rule.id.clone(),
                message,
            })? {
            AtomicMatcher::Pattern(snippet) => {
                let ts_language =
                    language
                        .ts_language()
                        .ok_or_else(|| RulesError::GrammarUnavailable {
                            rule: rule.id.clone(),
                            language: language.name().to_string(),
                        })?;
                let compiled = compile_pattern(&snippet, language).map_err(|message| {
                    RulesError::PatternCompile {
                        rule: rule.id.clone(),
                        message,
                    }
                })?;
                let query = Query::new(&ts_language, &compiled.query).map_err(|err| {
                    RulesError::QueryRejected {
                        rule: rule.id.clone(),
                        message: err.to_string(),
                        query: compiled.query.clone(),
                    }
                })?;
                CompiledMatcher::Query {
                    query,
                    metavars: compiled.metavars,
                }
            }
            AtomicMatcher::Kind(kind) => {
                let ts_language =
                    language
                        .ts_language()
                        .ok_or_else(|| RulesError::GrammarUnavailable {
                            rule: rule.id.clone(),
                            language: language.name().to_string(),
                        })?;
                let query_text = format!("({kind}) @{ROOT_CAPTURE}");
                let query = Query::new(&ts_language, &query_text).map_err(|err| {
                    RulesError::QueryRejected {
                        rule: rule.id.clone(),
                        message: err.to_string(),
                        query: query_text.clone(),
                    }
                })?;
                CompiledMatcher::Query {
                    query,
                    metavars: Vec::new(),
                }
            }
            AtomicMatcher::Regex(pattern) => {
                let regex =
                    regex::Regex::new(&pattern).map_err(|err| RulesError::PatternCompile {
                        rule: rule.id.clone(),
                        message: format!("invalid regex `{pattern}`: {err}"),
                    })?;
                CompiledMatcher::Regex(regex)
            }
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
            matcher,
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
        let mut matches = match &self.matcher {
            CompiledMatcher::Query { query, metavars } => {
                self.run_query(query, metavars, source)?
            }
            CompiledMatcher::Regex(regex) => self.run_regex(regex, source),
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

    fn run_query(
        &self,
        query: &Query,
        metavars: &[String],
        source: &str,
    ) -> Result<Vec<RuleMatch>, RulesError> {
        let tree =
            api::parse_tree(source, self.language).map_err(|err| RulesError::SourceParse {
                rule: self.rule_id.clone(),
                message: err.to_string(),
            })?;
        let names: Vec<&str> = query.capture_names().to_vec();
        let bytes = source.as_bytes();

        let mut cursor = QueryCursor::new();
        let mut it = cursor.matches(query, tree.root_node(), bytes);
        let mut matches = Vec::new();
        while let Some(m) = it.next() {
            let mut root: Option<Span> = None;
            let mut root_text = String::new();
            let mut bindings: BTreeMap<String, Binding> = BTreeMap::new();
            for cap in m.captures {
                let name = names[cap.index as usize];
                let span = Span::of(cap.node);
                let text = source[cap.node.start_byte()..cap.node.end_byte()].to_string();
                if name == ROOT_CAPTURE {
                    root = Some(span);
                    root_text = text;
                } else if metavars.iter().any(|m| m == name) {
                    // Canonical metavar capture; unification helpers carry a
                    // `.` and never appear in `metavars`, so they are skipped.
                    bindings
                        .entry(name.to_string())
                        .or_insert(Binding { text, span });
                }
            }
            if let Some(span) = root {
                matches.push(RuleMatch {
                    rule_id: self.rule_id.clone(),
                    span,
                    text: root_text,
                    bindings,
                });
            }
        }
        // Tree-sitter yields matches in query-eval order; sort to document
        // order for a stable, intuitive result.
        matches.sort_by_key(|m| (m.span.start_byte, m.span.end_byte));
        Ok(matches)
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
