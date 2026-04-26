//! Hierarchical composition for Flow predicates.
//!
//! Discovery finds `invariants.harn` files. This module turns those files into
//! applicable predicate declarations and composes their evaluated verdicts
//! without letting a deeper directory relax a shallower rule.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::discovery::{DiscoveredInvariantFile, DiscoveredPredicate};
use super::result::{Approver, InvariantBlockError, InvariantResult, Verdict};
use crate::flow::{PredicateHash, PredicateKind};

/// Stable error code attached to a `Block` produced by ceiling enforcement.
pub const PREDICATE_COUNT_EXPLOSION_CODE: &str = "predicate_count_explosion";

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

/// Predicate-count explosion limits applied to a slice's resolved predicate
/// union before evaluation begins.
///
/// Cross-directory union semantics make it cheap to accidentally pull a
/// pathological number of predicates into one slice — touch ten leaf
/// directories that each declare a dozen sibling-specific invariants and
/// suddenly Ship Captain is paying serial wall-clock for every one of them.
/// The ceiling makes that cost visible:
///
/// - Below `require_approval_threshold` evaluation proceeds without ceremony.
/// - Between `require_approval_threshold` and `block_threshold` the slice
///   still ships, but only after a named approver co-signs (`RequireApproval`).
/// - At or above `block_threshold` Flow refuses to evaluate the union and
///   returns `Block` so a human can split the slice or prune predicates.
#[derive(Clone, Debug)]
pub struct PredicateCeiling {
    pub require_approval_threshold: usize,
    pub block_threshold: usize,
    /// Approver routed when only the soft ceiling is breached.
    pub approver: Approver,
}

impl PredicateCeiling {
    /// Default soft ceiling. Slices with this many predicates are large enough
    /// to warrant a human glance even when every predicate passes.
    pub const DEFAULT_REQUIRE_APPROVAL_THRESHOLD: usize = 256;
    /// Default hard ceiling. Beyond this the union is almost always a
    /// misconfigured `invariants.harn` tree, not a legitimate slice.
    pub const DEFAULT_BLOCK_THRESHOLD: usize = 1024;
    /// Default approver role for soft-ceiling escalations.
    pub const DEFAULT_APPROVER_ROLE: &'static str = "flow-platform";
}

impl Default for PredicateCeiling {
    fn default() -> Self {
        Self {
            require_approval_threshold: Self::DEFAULT_REQUIRE_APPROVAL_THRESHOLD,
            block_threshold: Self::DEFAULT_BLOCK_THRESHOLD,
            approver: Approver::role(Self::DEFAULT_APPROVER_ROLE),
        }
    }
}

/// One directory's contribution to a predicate-count explosion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryContribution {
    pub relative_dir: String,
    pub count: usize,
}

/// Severity tier of a [`PredicateCeilingViolation`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateCeilingLevel {
    RequireApproval,
    Block,
}

/// Structured detail emitted when a slice's predicate union exceeds the
/// ceiling. Callers may render it directly or convert it into the canonical
/// [`InvariantResult`] via [`PredicateCeilingViolation::to_invariant_result`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateCeilingViolation {
    pub level: PredicateCeilingLevel,
    pub count: usize,
    pub threshold: usize,
    /// Directories that contributed the most predicates, sorted by count
    /// descending. Truncated to keep messages readable.
    pub top_contributors: Vec<DirectoryContribution>,
}

impl PredicateCeilingViolation {
    /// Maximum number of contributing directories surfaced in messages and
    /// reports. Beyond this the breakdown is more noise than signal.
    pub const MAX_TOP_CONTRIBUTORS: usize = 5;

    /// Render the violation as the canonical [`InvariantResult`] used by
    /// callers that fold the explosion limit into the same verdict pipeline as
    /// real predicates.
    pub fn to_invariant_result(&self, approver: &Approver) -> InvariantResult {
        match self.level {
            PredicateCeilingLevel::Block => InvariantResult::block(InvariantBlockError::new(
                PREDICATE_COUNT_EXPLOSION_CODE,
                self.message(),
            )),
            PredicateCeilingLevel::RequireApproval => {
                InvariantResult::require_approval(approver.clone())
            }
        }
    }

    /// Operator-facing summary explaining the explosion. Stable across
    /// `Block` and `RequireApproval` variants so log scrapers can rely on it.
    pub fn message(&self) -> String {
        let mut breakdown = self
            .top_contributors
            .iter()
            .map(|item| format!("{} ({})", item.relative_dir, item.count))
            .collect::<Vec<_>>()
            .join(", ");
        if breakdown.is_empty() {
            breakdown = "(no contributing directories)".to_string();
        }
        let level = match self.level {
            PredicateCeilingLevel::RequireApproval => "soft",
            PredicateCeilingLevel::Block => "hard",
        };
        format!(
            "predicate union of {count} exceeds {level} ceiling {threshold}; \
             top contributors: {breakdown}",
            count = self.count,
            level = level,
            threshold = self.threshold,
            breakdown = breakdown,
        )
    }
}

/// Outcome of running [`enforce_predicate_ceiling`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PredicateCeilingOutcome {
    /// Slice is within budget. Evaluation may proceed.
    Within { count: usize },
    /// Slice exceeded the soft or hard ceiling.
    Exceeded(PredicateCeilingViolation),
}

impl PredicateCeilingOutcome {
    pub fn count(&self) -> usize {
        match self {
            Self::Within { count } => *count,
            Self::Exceeded(violation) => violation.count,
        }
    }

    pub fn violation(&self) -> Option<&PredicateCeilingViolation> {
        match self {
            Self::Within { .. } => None,
            Self::Exceeded(violation) => Some(violation),
        }
    }
}

/// Apply a [`PredicateCeiling`] to the resolved predicate union for a slice.
///
/// The check is purely a count comparison — it never inspects predicate
/// bodies — and runs in O(n) over `resolved`. Pair it with
/// [`resolve_predicates_for_touched_directories`] before invoking the
/// executor; both operations preserve union semantics so shared ancestors are
/// counted once and sibling-specific predicates are kept distinct.
pub fn enforce_predicate_ceiling(
    resolved: &[ResolvedPredicate],
    ceiling: &PredicateCeiling,
) -> PredicateCeilingOutcome {
    let count = resolved.len();
    let level = if ceiling.block_threshold > 0 && count >= ceiling.block_threshold {
        Some((PredicateCeilingLevel::Block, ceiling.block_threshold))
    } else if ceiling.require_approval_threshold > 0 && count >= ceiling.require_approval_threshold
    {
        Some((
            PredicateCeilingLevel::RequireApproval,
            ceiling.require_approval_threshold,
        ))
    } else {
        None
    };

    let Some((level, threshold)) = level else {
        return PredicateCeilingOutcome::Within { count };
    };

    PredicateCeilingOutcome::Exceeded(PredicateCeilingViolation {
        level,
        count,
        threshold,
        top_contributors: top_contributors(
            resolved,
            PredicateCeilingViolation::MAX_TOP_CONTRIBUTORS,
        ),
    })
}

fn top_contributors(resolved: &[ResolvedPredicate], limit: usize) -> Vec<DirectoryContribution> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for predicate in resolved {
        *counts
            .entry(predicate.source.relative_dir.as_str())
            .or_insert(0) += 1;
    }
    let mut ranked = counts
        .into_iter()
        .map(|(dir, count)| DirectoryContribution {
            relative_dir: dir.to_string(),
            count,
        })
        .collect::<Vec<_>>();
    // Higher counts first; break ties by lexicographic directory order so the
    // output is deterministic regardless of input ordering.
    ranked.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.relative_dir.cmp(&right.relative_dir))
    });
    ranked.truncate(limit);
    ranked
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

    fn predicate_in(relative_dir: &str, name: &str, source_order: usize) -> ResolvedPredicate {
        ResolvedPredicate {
            qualified_name: qualified_name(relative_dir, name),
            logical_name: name.to_string(),
            source: PredicateSource::new(relative_dir),
            source_order,
            fallback_hash: None,
            predicate: predicate(name),
        }
    }

    fn synthetic_union(rules_per_dir: usize, dirs: &[&str]) -> Vec<ResolvedPredicate> {
        let mut order = 0;
        let mut resolved = Vec::with_capacity(dirs.len() * rules_per_dir);
        for dir in dirs {
            for index in 0..rules_per_dir {
                resolved.push(predicate_in(dir, &format!("rule_{index}"), order));
                order += 1;
            }
        }
        resolved
    }

    #[test]
    fn enforce_returns_within_when_under_thresholds() {
        let resolved = synthetic_union(4, &["a", "b", "c"]);
        let outcome = enforce_predicate_ceiling(&resolved, &PredicateCeiling::default());
        assert!(matches!(outcome, PredicateCeilingOutcome::Within { count } if count == 12));
    }

    #[test]
    fn enforce_emits_require_approval_at_soft_ceiling() {
        let resolved = synthetic_union(8, &["a", "b", "c"]);
        let ceiling = PredicateCeiling {
            require_approval_threshold: 16,
            block_threshold: 64,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = match outcome {
            PredicateCeilingOutcome::Exceeded(violation) => violation,
            other => panic!("expected Exceeded, got {other:?}"),
        };
        assert_eq!(violation.level, PredicateCeilingLevel::RequireApproval);
        assert_eq!(violation.threshold, 16);
        assert_eq!(violation.count, 24);
        let result = violation.to_invariant_result(&ceiling.approver);
        assert!(matches!(
            result.verdict,
            Verdict::RequireApproval {
                approver: Approver::Role { ref name }
            } if name == "flow-platform"
        ));
    }

    #[test]
    fn enforce_emits_block_at_hard_ceiling() {
        let resolved = synthetic_union(40, &["a", "b", "c"]);
        let ceiling = PredicateCeiling {
            require_approval_threshold: 16,
            block_threshold: 64,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = match outcome {
            PredicateCeilingOutcome::Exceeded(violation) => violation,
            other => panic!("expected Exceeded, got {other:?}"),
        };
        assert_eq!(violation.level, PredicateCeilingLevel::Block);
        assert_eq!(violation.threshold, 64);
        assert_eq!(violation.count, 120);
        let result = violation.to_invariant_result(&ceiling.approver);
        let error = result.block_error().expect("block carries error");
        assert_eq!(error.code, PREDICATE_COUNT_EXPLOSION_CODE);
        assert!(error.message.contains("hard ceiling"));
        assert!(error.message.contains("120"));
    }

    #[test]
    fn enforce_lists_top_contributors_in_descending_order() {
        let mut resolved = synthetic_union(6, &["alpha", "bravo"]);
        resolved.extend(synthetic_union(2, &["charlie", "delta"]));
        let ceiling = PredicateCeiling {
            require_approval_threshold: 8,
            block_threshold: 32,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = outcome
            .violation()
            .cloned()
            .expect("expected explosion outcome");
        let dirs: Vec<_> = violation
            .top_contributors
            .iter()
            .map(|item| (item.relative_dir.as_str(), item.count))
            .collect();
        assert_eq!(
            dirs,
            vec![("alpha", 6), ("bravo", 6), ("charlie", 2), ("delta", 2),]
        );
    }

    #[test]
    fn enforce_truncates_top_contributors_to_max() {
        let dirs: Vec<String> = (0..PredicateCeilingViolation::MAX_TOP_CONTRIBUTORS + 3)
            .map(|index| format!("d{index:02}"))
            .collect();
        let dir_refs: Vec<&str> = dirs.iter().map(String::as_str).collect();
        let resolved = synthetic_union(4, &dir_refs);
        let ceiling = PredicateCeiling {
            require_approval_threshold: 4,
            block_threshold: 9999,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = outcome
            .violation()
            .cloned()
            .expect("expected explosion outcome");
        assert_eq!(
            violation.top_contributors.len(),
            PredicateCeilingViolation::MAX_TOP_CONTRIBUTORS
        );
    }

    #[test]
    fn enforce_zero_threshold_disables_a_level() {
        let resolved = synthetic_union(8, &["a"]);
        let ceiling = PredicateCeiling {
            require_approval_threshold: 0,
            block_threshold: 4,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = outcome
            .violation()
            .cloned()
            .expect("hard ceiling alone should still trigger");
        assert_eq!(violation.level, PredicateCeilingLevel::Block);

        let ceiling = PredicateCeiling {
            require_approval_threshold: 4,
            block_threshold: 0,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = outcome
            .violation()
            .cloned()
            .expect("soft ceiling alone should still trigger");
        assert_eq!(violation.level, PredicateCeilingLevel::RequireApproval);
    }

    #[test]
    fn enforce_uses_hard_ceiling_when_both_thresholds_match() {
        let resolved = synthetic_union(64, &["a"]);
        let ceiling = PredicateCeiling {
            require_approval_threshold: 64,
            block_threshold: 64,
            approver: Approver::role("flow-platform"),
        };
        let outcome = enforce_predicate_ceiling(&resolved, &ceiling);
        let violation = outcome.violation().cloned().expect("expected explosion");
        // Block must take precedence so a misconfigured equal pair still
        // refuses the slice rather than asking for a co-sign.
        assert_eq!(violation.level, PredicateCeilingLevel::Block);
    }

    #[test]
    fn cross_directory_union_with_ceiling_blocks_when_pathological() {
        let chains: Vec<Vec<DiscoveredInvariantFile>> = (0..32)
            .map(|index| {
                let dir = format!("services/svc_{index:02}");
                let names: Vec<String> = (0..40).map(|rule| format!("rule_{rule:02}")).collect();
                let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
                vec![file(".", &["repo"]), file(&dir, &name_refs)]
            })
            .collect();
        let resolved = resolve_predicates_for_touched_directories(&chains);
        // 1 shared ancestor + 32 dirs × 40 rules each = 1281 predicates total.
        assert_eq!(resolved.len(), 1281);
        let outcome = enforce_predicate_ceiling(&resolved, &PredicateCeiling::default());
        let violation = outcome.violation().cloned().expect("union should explode");
        assert_eq!(violation.level, PredicateCeilingLevel::Block);
        // Top contributors should all be sibling services, not the root.
        for contribution in &violation.top_contributors {
            assert!(contribution.relative_dir.starts_with("services/svc_"));
            assert_eq!(contribution.count, 40);
        }
    }
}
