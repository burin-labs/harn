# Code-index release measurements

Measured on 2026-10-11 for [issue #9703](https://github.com/burin-labs/harn/issues/9703).
The measurement covers all 58 trials,
phase totals and invocation counts, graph and corpus digests, skipped paths,
host load, memory readings, compiler settings, and binary/source identities.

## Workload and boundary

The frozen downstream corpus contains 8,463 indexed files plus one controlled Rust
fixture, 88,173,033 source bytes, 752,584 graph nodes, 5,036,843 edges, and
441,301 resolved calls. Forty XML trace files are skipped, distinguishing attempted reads
from successful ingestion.

The benchmark host is an AMD Ryzen 5 3600X with 12 logical CPUs and about
60 GiB reported RAM. Both binaries use Rust 1.95.0 and the repository release
profile: optimization level 3, thin LTO, and 16 codegen units. Their common
source base is `059ef4c45271aed0f975c695082875724dfa55ae`; runtime phase
instrumentation is published separately in commit
`7e21a3232ec04c375066282c8662780f108342d8`.

`crates/harn-hostlib/examples/code_index_latency.rs` calls the owning
`IndexState::build_from_root` or `CodeIndexCapability::restore_from_disk` path
and the registered query/reindex handlers. Cold means a new in-memory index;
the operating-system file cache is not flushed. Restore readiness includes
reconciliation and required persistence. Cold time includes final persistence.
Resident edits exclude the initial restore and do not include an automatic
snapshot write. Fixture preparation, compilation, lease waits, and graph
fingerprinting are outside the reported operation time.

Body and declaration edits change the small fixture. The 64-file case forces
actual parsing and graph replacement of selected call-heavy files by
invalidating their stored content hashes, without changing their source bytes.
The changed-HEAD case makes a real empty Git commit with unchanged source.
These are controlled index workloads, not representative distributions of
user edits. The benchmark requires an explicit disposable-corpus marker
beside its `/tmp` root because preparation rewrites the fixture, snapshots,
and Git HEAD.

The embedding host's optional Harn semantic reference resolver is excluded.
These measurements do not establish whole-IDE startup latency or a comparison
against another language server.

## Release results

Each row has three complete trials. Seconds are median [minimum, maximum].

| Operation | Before | After |
| --- | --- | --- |
| Cold build and persist | 149.852 [100.183, 162.324] | 55.632 [52.893, 57.357] |
| Unchanged restart | 10.732 [9.093, 12.033] | 14.734 [14.454, 14.825] |
| Body edit on restart | 17.683 [16.870, 18.037] | 15.194 [13.337, 19.027] |
| Declaration edit on restart | 16.530 [16.078, 21.712] | 13.113 [12.614, 16.283] |
| Changed HEAD, unchanged content | 16.455 [14.633, 18.246] | 19.935 [12.739, 42.750] |
| 64-file reconciliation on restart | 65.949 [64.850, 70.200] | 14.326 [14.208, 14.396] |
| Resident body edit | 0.011199 [0.010412, 0.011880] | 0.009376 [0.009273, 0.009530] |
| Resident declaration edit | 0.009877 [0.009660, 0.013617] | 0.009427 [0.009284, 0.009956] |

Cold median time falls 62.9% (2.69 times faster); the 64-file restart falls
78.3% (4.60 times faster). The decisive phase is outgoing identifier references:
cold median 97.886 to 1.898 seconds, and 64-file median 40.088 to 0.080 seconds.
Cold parsing remains comparable at 25.666 versus 26.455 seconds. In the
64-file case, total reconciliation falls from 48.093 to 2.025 seconds. It also
finishes before the existing 30-second checkpoint threshold, avoiding an
intermediate whole-snapshot write; the checkpoint policy itself is unchanged.

Name lookup now has one owner that derives all-node, declaration, call-site,
and per-file function candidates together. Reference expansion reads only
name-addressable declarations and processes each distinct identifier once per
file. Removal updates each affected name bucket once. Declaration eligibility,
function visibility, and graph relationships retain their prior rules.

All 24 old/new comparisons match the complete normalized node/edge multiset,
corpus facts, node/edge counts, and resolved-call count. Every restart/edit query
returns the current fixture declaration. The normalized digest preserves
duplicate facts while ignoring allocation-specific node IDs. Cold snapshots
remain 583,840,101 bytes. Cold operation peak RSS is approximately 1.693 GiB
before and 1.705 GiB after; the typed lookup costs about 13 MiB at this peak.

## Restart control and limits

The first matrix ran on a shared host with changing CI load. Five additional
old/new unchanged-restart pairs used the preserved binaries, alternating which
binary ran first, under one measurement lease. All ten graph/corpus comparisons
match. Before median readiness is 7.902 seconds [7.845, 8.876]; after is 8.920
seconds [7.944, 12.147], an observed 12.9% median increase. The median paired
graph-restoration delta is +0.025 seconds. Much larger differences occur in
unchanged snapshot loading and trigram/word restoration, including a 1.9-second
trigram-restoration difference in the final pair.

These trials do not establish warm-path equivalence or isolate the cause of
the whole-restart increase. No warm restart speedup is claimed. They support
the large reference-expansion improvement, whose phase reduction explains the
cold gain and most of the 64-file gain. Resident fixture edits remain around
10 milliseconds; they do not establish large-file edit percentiles.

`operation_peak_rss_kib` is the benchmark process's Linux high-water mark read
before fingerprinting. Resident cases include their untimed initial restore in
that high-water mark. `process_peak_rss_kib` also includes the Cargo wrapper,
compilation when present, and verification; it is not an operation-memory
comparison. The index's `estimated_bytes` is an estimate, not observed RSS.
Nested phase totals overlap and must not be added to their parents.

The next latency priority is snapshot/posting restoration, with a reserved-host
baseline to separate runtime costs from shared-host variation before choosing
a storage change. Parsing is the next cold-build target. Neither is rewritten
in this change: reference expansion accounts for the largest measured
avoidable cost, while storage and parsing need different ownership and
verification decisions.

## Behavior verification

The owning regression parses 256 same-named calls and checks that the reference
candidate scan contains only declarations, including after snapshot restoration,
declaration replacement, and removal. Existing checks cover import visibility,
ambiguous calls, reference order, and recovery from intermediate checkpoints.
The rebuilt canonical CLI passes the code-librarian conformance cases for
dependency views, member surfaces, and external callers with no skipped cases.
Host-library tests, lint, and source/binary drift checks pass. No provider API
spend is involved.
