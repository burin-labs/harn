# Code-index restoration measurements

Measured for [issue #9712](https://github.com/burin-labs/harn/issues/9712),
following the [reference-expansion measurements](code-index-release-measurements.md).
The paired comparisons use the owning restore, reconciliation, registered query,
and reindex paths on the same frozen corpus. Whole-application startup and the
embedding host's optional semantic reference resolver are outside this boundary.
The measurement covers accepted and rejected trials, counter readings,
identities, phase counts, and inventories.

## Workload and identities

The unchanged seed contains 8,464 indexed files, 88,173,033 source bytes, 752,584 graph
nodes, 5,036,843 edges, and 441,301 resolved calls. Forty XML trace files are
skipped. The controlled Rust fixture changes its body or declaration; the
64-file case forces parsing of selected call-heavy files without changing their
source bytes. Changed HEAD is a real empty Git commit. Cold means a fresh
in-memory index with final persistence, without flushing the operating-system
file cache. Resident edit timing excludes the initial restore and does not
include automatic persistence.

Readiness ends when restore or mutation returns, before query execution. Query
latencies are recorded separately, and all four registered query assertions
must pass before a trial counts. The host counters at the reported readiness
marker also include those queries. Peak RSS is the process high-water mark
read before full-fact fingerprinting; resident-edit readings include the
untimed initial restore and therefore do not isolate edit memory allocation.

Both structural artifacts use source base
`46a06c7298178fc597af68415a55e006a8c8f582`, Rust 1.95.0, optimization level 3,
thin LTO, and 16 codegen units. They were built on a separate host and measured
on an AMD Ryzen 5 3600X with 12 logical CPUs, about 60 GiB RAM, and the
`schedutil` governor. No native-CPU compiler flags were set. The unchanged
snapshot is 583,840,101 bytes; the record retains its seed digest and each
structural trial's input and output snapshot digests.

The baseline release binary SHA-256 is
`6230aa830c178f7c7ccc99b392d4bb4a3b38902db97d089cd1e050eaeb639cfa`;
the optimized binary is
`785cded9e73994747cce339530bfdfaf722d8f6ccfd1fc4f3f0d7e6d8546c786`.
The record identifies the exact source files, compiler profile, corpus,
64-file selection, snapshots, and binaries. All eight optimized source-file
digests match the reviewed source. Hostnames and disposable absolute roots use
public placeholders.

## Quiet-host controls

Idle CI job listeners were temporarily paused after their active jobs finished.
A separate timed recovery restored scheduling if the measurement session ended.
The benchmark also verified the reservation against the host's boot ID and the
three listener process identities and paused states. A host reboot interrupted
the original-binary comparison after six accepted pairs; the remaining pairs
completed after reserving the host again.

Every trial prepares its fixture and snapshot outside the operation timer,
then requires three seconds with at least 98% CPU idle, at most four swapped
pages, and less than 0.5% system-wide SOME I/O pressure in the final matrix. Acceptance requires at
most 0.25 external busy CPU cores at readiness and process completion, at most
32 swapped pages, and the applicable I/O-pressure bound. Foreground CPU is read
from the actual child and must be nonzero. A pair counts only when both trials
are accepted; pair order alternates. Compilation, reservation, lease waits,
preparation, hashing, and full-fact fingerprinting are outside operation timing.

The original unchanged comparison uses system-wide FULL I/O pressure. Initial
write trials were rejected because that includes the benchmark's persistence.
A posting-stage rerun subtracted own-cgroup FULL pressure, but FULL describes
all nonidle tasks and that subtraction does not establish an external-pressure
bound. Its readings and original acceptance flags remain historical evidence.

All eight workloads were rerun prospectively on the final artifact using SOME
pressure, which records intervals with any stalled task. The final gate bounds
the nonnegative difference between global and own-cgroup SOME fractions at 2%,
at readiness and completion. This is a heuristic estimate: overlapping external
I/O and counter aggregation or windowing limit attribution. The actual child
must belong to the measured cgroup. Material snapshot writes must produce a
nonzero own-SOME reading. An independent synchronous writer produced a nonzero
reading through the same observation path and fired the gate at 16.7%.
Reservation, CPU, swap, and alternating-order controls remain in force.
An initial final-matrix capacity wait produced zero trials and is recorded as
no measurement. CI scheduling was restored after the completed matrix.

## Original warm-restart increase

Ten accepted old-build pairs out of fifteen attempts give median readiness
7.209 versus 7.427 seconds. The median paired after/before ratio is 1.0306,
with paired 95% bootstrap interval [1.0248, 1.0366]: about a 3% increase under
these controls, rather than the previous shared-host observation of 12.9%.
The earlier same-binary negative control rose from 7.375 to 16.240 seconds
while external CPU rose from 0.010 to 3.969 cores, demonstrating that host load
can overwhelm the smaller difference. It does not establish the exact cause
of the historical 12.9% result.

Most of the controlled increase is in unchanged trigram restoration, rather
than graph reconstruction: median paired phase deltas are +169 milliseconds
for trigrams, +37 milliseconds for loading, +10 milliseconds for the graph,
and +3 milliseconds for words. Medians of phase deltas need not sum to the
median total delta. Timing alone does not identify the allocator or cache
mechanism. Unprivileged performance counters were denied; a privileged
known-nonzero probe succeeded. Phase-attributed hardware counters were not
collected for these latency comparisons.

## Structural improvement

There are 24 accepted final structural pairs out of 27 attempts, three per
workload. The earlier posting-stage comparison remains separately identified
in the trial record. Ratios are the median paired
after/before ratio with a 95% bootstrap interval from 10,000 resamples, seed
9712. Three-pair intervals have limited precision; the raw ranges and query
timings remain in the record.

| Operation | Before median | After median | Paired ratio [95% interval] | Pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 7.248 s | 4.157 s | 0.580 [0.562, 0.582] | 3/3 |
| Body edit on restart | 10.476 s | 7.184 s | 0.686 [0.678, 0.688] | 3/5 |
| Declaration edit on restart | 10.454 s | 7.311 s | 0.701 [0.680, 0.708] | 3/3 |
| Changed HEAD | 10.659 s | 7.518 s | 0.695 [0.637, 0.705] | 3/4 |
| 64-file reconciliation | 12.140 s | 10.161 s | 0.836 [0.814, 0.845] | 3/3 |
| Resident body edit | 8.614 ms | 1.239 ms | 0.144 [0.143, 0.147] | 3/3 |
| Resident declaration edit | 8.635 ms | 1.230 ms | 0.143 [0.142, 0.143] | 3/3 |
| Cold build and persist | 24.976 s | 24.990 s | 1.003 [0.956, 1.006] | 3/3 |

Warm restart is about 1.7 times faster; resident fixture edits are about seven
times faster. Cold build shows no clear speedup. Its peak RSS falls from
1,752.8 to 1,497.7 MiB. All final pairs have identical normalized full-posting,
corpus, file, and node facts. Full graph facts match in seven workloads.
The baseline forced 64-file reparse loses 23,713 call links after recovery:
5,013,130 edges and 417,588 resolved calls. The optimized artifact instead
matches the full fresh-build digest, with 5,036,843 edges and 441,301 calls,
in every pair. This is a correctness improvement as well as a latency result.
Every trial reaches a fresh Cypher answer, two word hits, a trigram candidate
containing the fixture, and one filtered literal result with two occurrences.

Unchanged restart median readiness falls from 7.248 to 4.157 seconds. Trigram
restoration falls from 2.017 to 0.403 seconds, word restoration from 0.996 to
0.158 seconds, and graph restoration from 1.308 to 1.209 seconds. Snapshot
loading falls from 2.828 to 2.306 seconds; the optimized load separately reports
0.303 seconds reading and 1.953 seconds decoding. Reconciliation remains about
0.052 seconds. Phase parents contain their children; these totals are not
additive.

One posting store now owns membership and occurrence lists, per-file removal,
replacement, restoration, and memory estimates. Recovery adopts contiguous
vectors instead of hashing every membership into two sets. Word keys share one
immutable allocation across their removal projection, and tokenization groups
borrowed words per file rather than allocating keys per occurrence. Graph
restoration reserves its known node count. Snapshot decoding uses a byte slice,
then drops that buffer before index reconstruction. The snapshot format and
atomic publication contract remain at their existing owners.

Recovery also reconstructs imported-module visibility from saved file facts
through the same resolver used for live edits, deriving both directions once.
Dependency refresh borrows the saved outline instead of cloning it. An exact
Swift regression has two competing declarations, so global unique-name fallback
cannot conceal missing imports. It fails before the fix on restored visibility,
then passes through real recovery and the registered declaration-only reindex
handler while keeping the unchanged caller linked to the imported declaration.

Resident mutation projects explicit import edges for the edited file and its
direct importers. Replacement of a file's Module node drops incoming edges as
well as outgoing edges, so both directions are restored. New paths and bulk
changes use the same projection over the workspace; module-wide visibility
does not become a cross-product of stored graph edges.
The resident import phase falls from 7.544 and 7.552 milliseconds to 0–1
microseconds, with a nonzero invocation count in every
optimized trial. The exact regression replaces a module repeatedly and checks
its incoming and outgoing imports, then adds a previously missing import
target. The canonical CLI passes the architecture, surface, and symbol-contract
conformance paths with three passes, zero failures, and zero skips. Owning
integration checks exercise same-size edits, deletion from all sub-indexes,
unchanged-tree avoidance, partial-checkpoint recovery, and invalidation after
another HEAD change.

Unchanged operation peak RSS before fingerprinting falls from 1,151.0 to
1,092.9 MiB. The temporary serialized buffer does not increase peak memory on
this corpus, but a different graph/posting distribution can have a different
peak. These readings describe the measured operation, not the later
fingerprinting process or a universal memory ceiling.

The reported index memory estimate now includes posting-vector and map
capacity plus shared word allocations. Its accounting changed, so old and new
estimates are not a substitute for these RSS readings. Query checks use one
selective fixture and establish freshness and answers; they do not establish
throughput for every posting density or a general query-speed improvement.

Compact sorted membership vectors trade cheap recovery for shifting elements
during removal and reinsertion. In the 64-file comparison, median trigram
indexing rises from 0.153 to 0.370 seconds and total reconciliation from 1.796
to 2.052 seconds; the complete restart still falls from 12.140 to 10.161
seconds while also restoring the missing call links. This is a real update
cost, not a universal reindex speedup. A future
per-file contribution update should preserve unchanged memberships and apply
only their difference at the same posting owner. That experiment ranks below
the seconds of decoding, graph recovery, and full-snapshot publication.

## Next factor of two

The remaining warm budget is approximately 1.95 seconds decoding and 1.21
seconds rebuilding the graph, about 76% of readiness. A further 2× requires
saving roughly 2.08 seconds. Eliminating the remaining posting restoration
alone saves only about 13%; halving JSON decoding alone saves about one second.
The next priority is a validated snapshot already arranged for queries, with
compact adjacency, shared strings, and postings, plus a per-file mutable delta
behind the same query owner. It must subsume the reconstruction path and
preserve atomic generations, freshness, and interrupted recovery.

Cold readiness is about 25 seconds: parsing takes 10.724 seconds and ingestion
5.730 seconds, including 3.919 seconds of trigrams and 1.467 seconds of words.
Parsing alone is roughly 43% of readiness, so even eliminating it cannot
deliver another 2×. A bounded parsed-file-facts pool should include independent
posting extraction, while one publisher owns graph assembly and resolution.
Measure worker counts 1, 2, 4, and 6, exclusive phase costs, cancellation,
checkpoint recovery, and peak memory before selecting a concurrency policy.
For restart after an edit, whole-snapshot capture and writing remain another
several seconds; generation/delta publication should address that cost at the
same storage seam as reader restoration.

The [latency architecture](code-index-latency-architecture.md) records the
application policies, hardware APIs, mature indexing techniques, and comparison
limits. Workspace index reuse across sessions is the next embedding-host
priority; it requires measuring session lifetimes, freshness, and memory
pressure at that owner's boundary. A numerical state-of-the-art ranking
requires the same corpus, semantics, hardware, cache conditions, and boundary
on each implementation.
