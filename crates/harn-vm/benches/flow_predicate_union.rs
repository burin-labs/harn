//! Benchmarks for the cross-directory predicate union pipeline.
//!
//! Covers the path that turns discovered `invariants.harn` chains into the
//! resolved union a slice will evaluate against, plus the explosion-limit
//! check that fronts evaluation. Three fixtures are exercised:
//!
//! - `normal`: a small repo with shared ancestors and a handful of leaves.
//! - `high_fanout`: many sibling directories, each with its own predicates.
//! - `pathological`: hundreds of directories each declaring dozens of
//!   predicates — the regression target for #733's explosion limit.
//!
//! These benchmarks calibrate the default `PredicateCeiling` thresholds: the
//! union itself is microsecond-scale even at the pathological size, so the
//! ceiling is purely an operational/cognitive guard, not a perf guard.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use harn_lexer::Span;
use harn_vm::flow::{
    enforce_predicate_ceiling, resolve_predicates_for_touched_directories, DiscoveredInvariantFile,
    DiscoveredPredicate, PredicateCeiling, PredicateHash, PredicateKind,
};
use std::path::PathBuf;

fn discovered_predicate(name: &str) -> DiscoveredPredicate {
    DiscoveredPredicate {
        name: name.to_string(),
        kind: PredicateKind::Deterministic,
        fallback: None,
        archivist: None,
        retroactive: false,
        source_hash: PredicateHash::new(format!("sha256:bench/{name}")),
        span: Span::dummy(),
    }
}

fn invariants_file(relative_dir: &str, names: Vec<String>) -> DiscoveredInvariantFile {
    DiscoveredInvariantFile {
        path: PathBuf::from(relative_dir).join("invariants.harn"),
        relative_dir: relative_dir.to_string(),
        source: String::new(),
        predicates: names
            .iter()
            .map(|name| discovered_predicate(name))
            .collect(),
        diagnostics: Vec::new(),
    }
}

/// Build a fixture mirroring how Ship Captain assembles per-touched-dir chains.
///
/// Each chain shares a root `invariants.harn` with `shared_ancestor_count`
/// repo-wide rules so de-duplication can run, plus a leaf-specific file with
/// `leaf_count` sibling-only rules.
fn build_chains(
    touched_dirs: usize,
    shared_ancestor_count: usize,
    leaf_count: usize,
) -> Vec<Vec<DiscoveredInvariantFile>> {
    let shared_names: Vec<String> = (0..shared_ancestor_count)
        .map(|index| format!("repo_rule_{index:03}"))
        .collect();
    (0..touched_dirs)
        .map(|dir_index| {
            let leaf_dir = format!("services/svc_{dir_index:03}");
            let leaf_names: Vec<String> = (0..leaf_count)
                .map(|rule| format!("rule_{rule:03}"))
                .collect();
            vec![
                invariants_file(".", shared_names.clone()),
                invariants_file(&leaf_dir, leaf_names),
            ]
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    touched_dirs: usize,
    shared_ancestor_count: usize,
    leaf_count: usize,
}

const FIXTURES: &[Fixture] = &[
    // Small monorepo: one shared `repo_*` rule, four touched leaves with
    // a couple of leaf-specific rules each. Below the soft ceiling.
    Fixture {
        name: "normal",
        touched_dirs: 4,
        shared_ancestor_count: 1,
        leaf_count: 2,
    },
    // High-fanout slice: a refactor sweeping eight services, each with its
    // own twelve-rule policy file. Crosses the soft ceiling but not the
    // hard one — the kind of slice that should ask for a co-sign.
    Fixture {
        name: "high_fanout",
        touched_dirs: 24,
        shared_ancestor_count: 8,
        leaf_count: 12,
    },
    // Pathological: hundreds of sibling directories, dozens of rules each.
    // This is the regression target for #733: every leaf's predicates are
    // sibling-specific so de-dup cannot collapse them.
    Fixture {
        name: "pathological",
        touched_dirs: 64,
        shared_ancestor_count: 16,
        leaf_count: 32,
    },
];

fn bench_union(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow_predicate_union/resolve");
    for fixture in FIXTURES {
        let chains = build_chains(
            fixture.touched_dirs,
            fixture.shared_ancestor_count,
            fixture.leaf_count,
        );
        let expected_count =
            fixture.shared_ancestor_count + fixture.touched_dirs * fixture.leaf_count;
        group.throughput(Throughput::Elements(expected_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(fixture.name),
            &chains,
            |b, chains| {
                b.iter(|| {
                    let resolved =
                        resolve_predicates_for_touched_directories(black_box(chains.as_slice()));
                    black_box(resolved)
                });
            },
        );
    }
    group.finish();
}

fn bench_ceiling(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow_predicate_union/ceiling");
    for fixture in FIXTURES {
        let chains = build_chains(
            fixture.touched_dirs,
            fixture.shared_ancestor_count,
            fixture.leaf_count,
        );
        let resolved = resolve_predicates_for_touched_directories(&chains);
        group.throughput(Throughput::Elements(resolved.len() as u64));
        let ceiling = PredicateCeiling::default();
        group.bench_with_input(
            BenchmarkId::from_parameter(fixture.name),
            &resolved,
            |b, resolved| {
                b.iter(|| {
                    let outcome = enforce_predicate_ceiling(
                        black_box(resolved.as_slice()),
                        black_box(&ceiling),
                    );
                    black_box(outcome)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_union, bench_ceiling);
criterion_main!(benches);
