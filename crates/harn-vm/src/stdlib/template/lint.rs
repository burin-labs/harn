//! AST surface that `harn-lint` consumes to enforce `.harn.prompt`
//! drift-prevention rules (#1669).
//!
//! The template parser and AST are otherwise internal — exposing a
//! shallow read-only view through this module keeps the lint crate
//! free of template-engine internals while still giving rules enough
//! structure to walk conditionals, sections, and includes.

use super::ast::{BinOp, Expr, Node, PathSeg};
use super::parser::parse as parse_template;

/// Parse a template source string into a flat list of lintable
/// constructs (conditionals + sections). Returns `Err` when the
/// template doesn't parse — callers should surface the underlying
/// `validate_template_syntax` error to the user before linting.
pub fn parse(src: &str) -> Result<Vec<LintConstruct>, String> {
    let nodes = parse_template(src).map_err(|error| error.message())?;
    let mut out = Vec::new();
    walk_nodes(&nodes, &mut out);
    Ok(out)
}

/// One lintable construct, materialized in source order so rules can
/// reason about counts (e.g. branch-explosion) and individual call
/// sites (e.g. provider-identity comparisons).
#[derive(Debug, Clone)]
pub enum LintConstruct {
    /// An `{{ if .. }}` / `{{ elif }}` chain. One entry per condition
    /// in the chain (the trailing `{{ else }}` is implicit and not
    /// listed). Conditions are flattened across `elif` to make
    /// branch-count rules straightforward.
    IfChain { branches: Vec<IfBranch> },
    /// A `{{ section "..." }}` block. Sections are themselves
    /// capability-adaptive but never look identity-driven; rules use
    /// this to count capability-aware partials.
    Section {
        name: String,
        line: usize,
        col: usize,
    },
}

#[derive(Debug, Clone)]
pub struct IfBranch {
    pub line: usize,
    pub col: usize,
    pub condition: ConditionShape,
}

/// Coarse classification of an `{{ if expr }}` condition. The lint
/// rules don't need to evaluate or fully reconstruct expressions —
/// just enough structure to detect the two failure patterns called
/// out in #1669:
///
/// - Identity comparisons (`llm.provider == "..."`).
/// - Capability-flag branches (`llm.capabilities.<flag>`), which the
///   variant-explosion rule counts.
///
/// Conditions outside these shapes resolve to `Other` and don't
/// participate in either rule.
#[derive(Debug, Clone)]
pub enum ConditionShape {
    /// `llm.provider == "..."` / `llm.model == "..."` /
    /// `llm.family == "..."` (or `!=`).
    ProviderIdentity(IdentityField),
    /// Any path-based condition mentioning `llm.capabilities.<flag>`
    /// (including negation and use as a comparison operand). The
    /// variant-explosion rule counts every branch with this shape.
    /// Source position lives on the surrounding [`IfBranch`].
    CapabilityFlag {
        flag: String,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityField {
    Provider,
    Model,
    Family,
}

impl IdentityField {
    pub fn as_str(self) -> &'static str {
        match self {
            IdentityField::Provider => "provider",
            IdentityField::Model => "model",
            IdentityField::Family => "family",
        }
    }
}

fn walk_nodes(nodes: &[Node], out: &mut Vec<LintConstruct>) {
    for node in nodes {
        walk_node(node, out);
    }
}

fn walk_node(node: &Node, out: &mut Vec<LintConstruct>) {
    match node {
        Node::Text(_) | Node::Expr { .. } | Node::LegacyBareInterp { .. } => {}
        Node::If {
            branches,
            else_branch,
            line: _,
            col: _,
        } => {
            let mut summary = Vec::with_capacity(branches.len());
            for branch in branches {
                summary.push(IfBranch {
                    line: branch.line,
                    col: branch.col,
                    condition: classify_condition(&branch.cond),
                });
                walk_nodes(&branch.body, out);
            }
            out.push(LintConstruct::IfChain { branches: summary });
            if let Some(else_body) = else_branch {
                walk_nodes(else_body, out);
            }
        }
        Node::For { body, empty, .. } => {
            walk_nodes(body, out);
            if let Some(empty) = empty {
                walk_nodes(empty, out);
            }
        }
        Node::Include { .. } => {
            // Include resolution happens at render time. Linting only
            // walks the calling template; the included partial gets
            // linted independently when the linter encounters it.
        }
        Node::Section {
            name,
            body,
            line,
            col,
            ..
        } => {
            out.push(LintConstruct::Section {
                name: name.clone(),
                line: *line,
                col: *col,
            });
            walk_nodes(body, out);
        }
    }
}

/// Classify the top-level shape of an `{{ if expr }}` condition.
fn classify_condition(expr: &Expr) -> ConditionShape {
    if let Some(identity) = match_identity_compare(expr) {
        return ConditionShape::ProviderIdentity(identity);
    }
    if let Some(capability) = match_capability_path(expr) {
        return capability;
    }
    ConditionShape::Other
}

/// Match `llm.<provider|model|family> == "..."` or `!= "..."`,
/// returning the LHS identity field that was compared.
fn match_identity_compare(expr: &Expr) -> Option<IdentityField> {
    let Expr::Binary(op, lhs, rhs) = expr else {
        return None;
    };
    if !matches!(op, BinOp::Eq | BinOp::Neq) {
        return None;
    }
    let path = match (lhs.as_ref(), rhs.as_ref()) {
        (Expr::Path(p), Expr::Str(_)) | (Expr::Str(_), Expr::Path(p)) => p,
        _ => return None,
    };
    if !path_starts_with_llm(path) {
        return None;
    }
    match path.get(1) {
        Some(PathSeg::Field(name) | PathSeg::Key(name)) if name == "provider" => {
            Some(IdentityField::Provider)
        }
        Some(PathSeg::Field(name) | PathSeg::Key(name)) if name == "model" => {
            Some(IdentityField::Model)
        }
        Some(PathSeg::Field(name) | PathSeg::Key(name)) if name == "family" => {
            Some(IdentityField::Family)
        }
        _ => None,
    }
}

/// Match `llm.capabilities.<flag>` (possibly negated by `!`) or
/// `llm.capabilities.<flag> == <literal>`, returning the flag name.
fn match_capability_path(expr: &Expr) -> Option<ConditionShape> {
    fn find_capability_path(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Path(path) => capability_flag_from_path(path),
            Expr::Unary(_, inner) => find_capability_path(inner),
            Expr::Binary(_, lhs, rhs) => {
                find_capability_path(lhs).or_else(|| find_capability_path(rhs))
            }
            Expr::Filter(inner, _, _) => find_capability_path(inner),
            _ => None,
        }
    }
    let flag = find_capability_path(expr)?;
    Some(ConditionShape::CapabilityFlag { flag })
}

fn capability_flag_from_path(path: &[PathSeg]) -> Option<String> {
    if !path_starts_with_llm(path) {
        return None;
    }
    let Some(PathSeg::Field(name) | PathSeg::Key(name)) = path.get(1) else {
        return None;
    };
    if name != "capabilities" {
        return None;
    }
    let Some(PathSeg::Field(flag) | PathSeg::Key(flag)) = path.get(2) else {
        return None;
    };
    Some(flag.clone())
}

fn path_starts_with_llm(path: &[PathSeg]) -> bool {
    matches!(
        path.first(),
        Some(PathSeg::Field(name)) if name == "llm",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Vec<LintConstruct> {
        parse(src).expect("template should parse")
    }

    fn first_if(constructs: &[LintConstruct]) -> &[IfBranch] {
        match constructs
            .iter()
            .find(|c| matches!(c, LintConstruct::IfChain { .. }))
            .expect("if chain present")
        {
            LintConstruct::IfChain { branches } => branches.as_slice(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn provider_identity_eq_detected() {
        let constructs = parse_ok("{{ if llm.provider == \"anthropic\" }}x{{ else }}y{{ end }}");
        let branches = first_if(&constructs);
        assert_eq!(branches.len(), 1);
        assert!(matches!(
            branches[0].condition,
            ConditionShape::ProviderIdentity(IdentityField::Provider)
        ));
    }

    #[test]
    fn model_identity_neq_detected() {
        let constructs = parse_ok("{{ if llm.model != \"gpt-5\" }}x{{ end }}");
        let branches = first_if(&constructs);
        assert!(matches!(
            branches[0].condition,
            ConditionShape::ProviderIdentity(IdentityField::Model)
        ));
    }

    #[test]
    fn capability_flag_detected_in_negation_and_filter() {
        let constructs = parse_ok(
            "{{ if !llm.capabilities.native_tools }}x{{ end }}\
             {{ if llm.capabilities.prefers_xml_scaffolding | default: false }}y{{ end }}",
        );
        let if_chains: Vec<_> = constructs
            .iter()
            .filter_map(|c| match c {
                LintConstruct::IfChain { branches } => Some(branches.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(if_chains.len(), 2);
        assert!(matches!(
            if_chains[0][0].condition,
            ConditionShape::CapabilityFlag { ref flag, .. } if flag == "native_tools"
        ));
        assert!(matches!(
            if_chains[1][0].condition,
            ConditionShape::CapabilityFlag { ref flag, .. } if flag == "prefers_xml_scaffolding"
        ));
    }

    #[test]
    fn elif_chain_lifts_per_branch_condition() {
        let constructs = parse_ok(
            "{{ if llm.provider == \"openai\" }}a\
             {{ elif llm.capabilities.native_tools }}b\
             {{ else }}c{{ end }}",
        );
        let branches = first_if(&constructs);
        assert_eq!(branches.len(), 2);
        assert!(matches!(
            branches[0].condition,
            ConditionShape::ProviderIdentity(IdentityField::Provider)
        ));
        assert!(matches!(
            branches[1].condition,
            ConditionShape::CapabilityFlag { ref flag, .. } if flag == "native_tools"
        ));
    }

    #[test]
    fn unrelated_condition_falls_through_to_other() {
        let constructs = parse_ok("{{ if score > 0.5 }}a{{ end }}");
        let branches = first_if(&constructs);
        assert!(matches!(branches[0].condition, ConditionShape::Other));
    }

    #[test]
    fn sections_listed_in_source_order() {
        let constructs = parse_ok(
            "{{ section \"task\" }}t{{ endsection }}\
             {{ section \"output_format\" }}o{{ endsection }}",
        );
        let names: Vec<_> = constructs
            .iter()
            .filter_map(|c| match c {
                LintConstruct::Section { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["task", "output_format"]);
    }
}
