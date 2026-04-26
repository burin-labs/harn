//! Hierarchical composition for Flow predicates.
//!
//! Discovery finds `invariants.harn` files. This module turns those files into
//! applicable predicate declarations and composes their evaluated verdicts
//! without letting a deeper directory relax a shallower rule.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::discovery::{DiscoveredInvariantFile, DiscoveredPredicate};
use super::result::{InvariantResult, Verdict};
use crate::flow::{PredicateHash, PredicateKind};

/// Source location of a predicate within the directory hierarchy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PredicateSource {
    /// Directory relative to the discovery root. The root is represented as
    /// `"."`, matching [`DiscoveredInvariantFile::relative_dir`].
    pub relative_dir: String,
    /// Root is depth 0; each descendant component increments the depth.
    pub depth: usize,
}

impl PredicateSource {
    pub fn new(relative_dir: impl Into<String>) -> Self {
        let relative_dir = normalize_relative_dir(relative_dir.into());
        let depth = directory_depth(&relative_dir);
        Self {
            relative_dir,
            depth,
        }
    }

    fn is_ancestor_of_or_same(&self, other: &Self) -> bool {
        is_ancestor_dir(&self.relative_dir, &other.relative_dir)
    }
}

/// One predicate declaration after hierarchical resolution.
#[derive(Clone, Debug)]
pub struct ResolvedPredicate {
    /// Stable UI/logging name, e.g. `services/api::no_pii`.
    pub qualified_name: String,
    /// Function name used to identify ancestor/child override lineages.
    pub logical_name: String,
    pub source: PredicateSource,
    /// Stable source-order index within the resolved set.
    pub source_order: usize,
    /// Resolved source hash of a semantic predicate's deterministic fallback.
    pub fallback_hash: Option<PredicateHash>,
    pub predicate: DiscoveredPredicate,
}

/// Strictness rank for merging verdicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum VerdictStrictness {
    Allow,
    Warn,
    RequireApproval,
    Block,
}

/// A predicate evaluation stamped with its source depth.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateEvaluation {
    pub qualified_name: String,
    pub logical_name: String,
    pub source: PredicateSource,
    pub result: InvariantResult,
}

impl PredicateEvaluation {
    pub fn new(resolved: &ResolvedPredicate, result: InvariantResult) -> Self {
        Self {
            qualified_name: resolved.qualified_name.clone(),
            logical_name: resolved.logical_name.clone(),
            source: resolved.source.clone(),
            result,
        }
    }
}

/// Effective verdict for one predicate evaluation after ancestor composition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposedPredicateEvaluation {
    pub qualified_name: String,
    pub logical_name: String,
    pub source: PredicateSource,
    /// The predicate declaration whose verdict governs this evaluation after
    /// applying strictness and shallower-tie rules.
    pub selected_qualified_name: String,
    pub selected_source: PredicateSource,
    pub result: InvariantResult,
}

/// Resolve the applicable predicates for a single root-to-leaf discovery chain.
///
/// Unlike the older override resolver, this keeps ancestor and child
/// declarations. Both must be evaluated so a child can tighten a parent but
/// cannot relax an ancestor's blocking verdict.
pub fn resolve_predicates(files: &[DiscoveredInvariantFile]) -> Vec<ResolvedPredicate> {
    let mut resolved = Vec::new();
    let mut visible_deterministic = BTreeMap::<String, PredicateHash>::new();
    for file in files {
        let source = PredicateSource::new(&file.relative_dir);
        for predicate in &file.predicates {
            if predicate.kind == PredicateKind::Deterministic {
                visible_deterministic.insert(predicate.name.clone(), predicate.source_hash.clone());
            }
        }
        for predicate in &file.predicates {
            let fallback_hash = predicate
                .fallback
                .as_ref()
                .and_then(|fallback| visible_deterministic.get(fallback))
                .cloned();
            resolved.push(ResolvedPredicate {
                qualified_name: qualified_name(&file.relative_dir, &predicate.name),
                logical_name: predicate.name.clone(),
                source: source.clone(),
                source_order: resolved.len(),
                fallback_hash,
                predicate: predicate.clone(),
            });
        }
    }
    resolved
}

/// Resolve predicate declarations for every touched directory and union them.
///
/// Shared ancestors are de-duplicated by `(source_dir, predicate_name)`, while
/// sibling directories keep same-named predicates as independent declarations.
pub fn resolve_predicates_for_touched_directories(
    chains: &[Vec<DiscoveredInvariantFile>],
) -> Vec<ResolvedPredicate> {
    let mut by_source_and_name: BTreeMap<(String, String), ResolvedPredicate> = BTreeMap::new();

    for chain in chains {
        for resolved in resolve_predicates(chain) {
            let key = (
                resolved.source.relative_dir.clone(),
                resolved.logical_name.clone(),
            );
            let source_order = by_source_and_name.len();
            by_source_and_name
                .entry(key)
                .or_insert_with(|| ResolvedPredicate {
                    source_order,
                    ..resolved
                });
        }
    }

    let mut resolved = by_source_and_name.into_values().collect::<Vec<_>>();
    resolved.sort_by(|left, right| {
        left.source
            .depth
            .cmp(&right.source.depth)
            .then_with(|| left.source.relative_dir.cmp(&right.source.relative_dir))
            .then_with(|| left.source_order.cmp(&right.source_order))
            .then_with(|| left.logical_name.cmp(&right.logical_name))
    });
    resolved
}

/// Compose evaluated predicate results under stricter-child / shallower-tie
/// semantics.
///
/// For each evaluation, only same-name predicates from ancestor directories are
/// eligible to govern it. The strictest verdict wins; equal strictness selects
/// the shallower source. This prevents a leaf `Allow` from shadowing a repo-wide
/// `Block`, while still letting a leaf `Block` tighten an ancestor `Warn`.
pub fn compose_predicate_results(
    evaluations: &[PredicateEvaluation],
) -> Vec<ComposedPredicateEvaluation> {
    let mut composed = Vec::with_capacity(evaluations.len());

    for evaluation in evaluations {
        let selected = evaluations
            .iter()
            .filter(|candidate| {
                candidate.logical_name == evaluation.logical_name
                    && candidate.source.is_ancestor_of_or_same(&evaluation.source)
            })
            .max_by(|left, right| compare_evaluations(left, right))
            .unwrap_or(evaluation);

        composed.push(ComposedPredicateEvaluation {
            qualified_name: evaluation.qualified_name.clone(),
            logical_name: evaluation.logical_name.clone(),
            source: evaluation.source.clone(),
            selected_qualified_name: selected.qualified_name.clone(),
            selected_source: selected.source.clone(),
            result: selected.result.clone(),
        });
    }

    composed
}

pub fn verdict_strictness(verdict: &Verdict) -> VerdictStrictness {
    match verdict {
        Verdict::Allow => VerdictStrictness::Allow,
        Verdict::Warn { .. } => VerdictStrictness::Warn,
        Verdict::RequireApproval { .. } => VerdictStrictness::RequireApproval,
        Verdict::Block { .. } => VerdictStrictness::Block,
    }
}

fn compare_evaluations(
    left: &PredicateEvaluation,
    right: &PredicateEvaluation,
) -> std::cmp::Ordering {
    let left_strictness = verdict_strictness(&left.result.verdict);
    let right_strictness = verdict_strictness(&right.result.verdict);
    left_strictness
        .cmp(&right_strictness)
        // `max_by` keeps the greater value, so reverse depth for shallower ties.
        .then_with(|| right.source.depth.cmp(&left.source.depth))
        .then_with(|| right.qualified_name.cmp(&left.qualified_name))
}

fn qualified_name(relative_dir: &str, name: &str) -> String {
    let relative_dir = normalize_relative_dir(relative_dir.to_string());
    if relative_dir == "." {
        name.to_string()
    } else {
        format!("{relative_dir}::{name}")
    }
}

fn normalize_relative_dir(value: String) -> String {
    let parts = value
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .collect::<Vec<_>>();
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn directory_depth(relative_dir: &str) -> usize {
    if relative_dir == "." {
        0
    } else {
        relative_dir
            .split('/')
            .filter(|part| !part.is_empty())
            .count()
    }
}

fn is_ancestor_dir(ancestor: &str, descendant: &str) -> bool {
    let ancestor = normalize_relative_dir(ancestor.to_string());
    let descendant = normalize_relative_dir(descendant.to_string());
    if ancestor == "." || ancestor == descendant {
        return true;
    }
    descendant
        .strip_prefix(&ancestor)
        .is_some_and(|remaining| remaining.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{Approver, InvariantBlockError, PredicateHash, PredicateKind};
    use harn_lexer::Span;
    use std::path::PathBuf;

    fn predicate(name: &str) -> DiscoveredPredicate {
        DiscoveredPredicate {
            name: name.to_string(),
            kind: PredicateKind::Deterministic,
            fallback: None,
            archivist: None,
            retroactive: false,
            source_hash: PredicateHash::new(format!("sha256:{name}")),
            span: Span::dummy(),
        }
    }

    fn file(relative_dir: &str, names: &[&str]) -> DiscoveredInvariantFile {
        DiscoveredInvariantFile {
            path: PathBuf::from(relative_dir).join("invariants.harn"),
            relative_dir: relative_dir.to_string(),
            source: String::new(),
            predicates: names.iter().map(|name| predicate(name)).collect(),
            diagnostics: Vec::new(),
        }
    }

    fn evaluation(
        qualified_name: &str,
        logical_name: &str,
        relative_dir: &str,
        result: InvariantResult,
    ) -> PredicateEvaluation {
        PredicateEvaluation {
            qualified_name: qualified_name.to_string(),
            logical_name: logical_name.to_string(),
            source: PredicateSource::new(relative_dir),
            result,
        }
    }

    #[test]
    fn resolve_predicates_keeps_ancestor_and_child_declarations() {
        let resolved = resolve_predicates(&[file(".", &["shared"]), file("src", &["shared"])]);
        let qualified = resolved
            .iter()
            .map(|predicate| predicate.qualified_name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(qualified, vec!["shared", "src::shared"]);
        assert_eq!(resolved[0].source.depth, 0);
        assert_eq!(resolved[1].source.depth, 1);
    }

    #[test]
    fn override_narrowing_allows_deeper_stricter_verdict() {
        let evaluations = vec![
            evaluation(
                "security",
                "security",
                ".",
                InvariantResult::warn("repo warning"),
            ),
            evaluation(
                "src::security",
                "security",
                "src",
                InvariantResult::block(InvariantBlockError::new(
                    "leaf_policy",
                    "leaf policy blocks this slice",
                )),
            ),
        ];

        let composed = compose_predicate_results(&evaluations);
        let child = composed
            .iter()
            .find(|item| item.qualified_name == "src::security")
            .unwrap();
        assert_eq!(child.selected_qualified_name, "src::security");
        assert_eq!(
            verdict_strictness(&child.result.verdict),
            VerdictStrictness::Block
        );
    }

    #[test]
    fn override_relaxing_keeps_shallower_block() {
        let evaluations = vec![
            evaluation(
                "security",
                "security",
                ".",
                InvariantResult::block(InvariantBlockError::new(
                    "repo_policy",
                    "repo policy blocks this slice",
                )),
            ),
            evaluation("src::security", "security", "src", InvariantResult::allow()),
        ];

        let composed = compose_predicate_results(&evaluations);
        let child = composed
            .iter()
            .find(|item| item.qualified_name == "src::security")
            .unwrap();
        assert_eq!(child.selected_qualified_name, "security");
        assert_eq!(
            verdict_strictness(&child.result.verdict),
            VerdictStrictness::Block
        );
    }

    #[test]
    fn equal_strictness_ties_go_to_shallower_predicate() {
        let evaluations = vec![
            evaluation(
                "review",
                "review",
                ".",
                InvariantResult::require_approval(Approver::role("platform")),
            ),
            evaluation(
                "src::review",
                "review",
                "src",
                InvariantResult::require_approval(Approver::role("local")),
            ),
        ];

        let composed = compose_predicate_results(&evaluations);
        let child = composed
            .iter()
            .find(|item| item.qualified_name == "src::review")
            .unwrap();
        assert_eq!(child.selected_qualified_name, "review");
    }

    #[test]
    fn cross_directory_union_deduplicates_shared_ancestors_only() {
        let api_chain = vec![
            file(".", &["repo"]),
            file("services/api", &["api", "shared_name"]),
        ];
        let web_chain = vec![
            file(".", &["repo"]),
            file("services/web", &["web", "shared_name"]),
        ];

        let resolved = resolve_predicates_for_touched_directories(&[api_chain, web_chain]);
        let qualified = resolved
            .iter()
            .map(|predicate| predicate.qualified_name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            qualified,
            vec![
                "repo",
                "services/api::api",
                "services/api::shared_name",
                "services/web::web",
                "services/web::shared_name"
            ]
        );
    }

    #[test]
    fn sibling_same_name_predicates_do_not_shadow_each_other() {
        let evaluations = vec![
            evaluation(
                "services/api::guard",
                "guard",
                "services/api",
                InvariantResult::block(InvariantBlockError::new("api", "api blocked")),
            ),
            evaluation(
                "services/web::guard",
                "guard",
                "services/web",
                InvariantResult::allow(),
            ),
        ];

        let composed = compose_predicate_results(&evaluations);
        let web = composed
            .iter()
            .find(|item| item.qualified_name == "services/web::guard")
            .unwrap();
        assert_eq!(web.selected_qualified_name, "services/web::guard");
        assert_eq!(
            verdict_strictness(&web.result.verdict),
            VerdictStrictness::Allow
        );
    }
}
