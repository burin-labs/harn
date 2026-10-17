# Code-index latency architecture

The priority is to stop reconstructing facts and projections already produced
by the index owner. Lower-level acceleration follows the remaining measured
phase budget. These are engineering recommendations, not an end-to-end
performance ranking against another indexer.

## Restoration ownership

The index has two posting semantics: membership (a file occurs once per
trigram), and occurrences (every word hit retains its file and line, including
duplicates). One posting store owns forward lists, their file-removal
projection, replacement, recovery, and memory estimates. The hit type owns
normalization and removal semantics. Words share one immutable term allocation
between forward and reverse projections. Tokenization groups borrowed tokens
per file rather than allocating strings for every occurrence.

Recovery also derives imported-module roots and their reverse projection from
saved file imports through the same resolver as live edits. Explicit dependency
snapshot rows do not retain whole-module visibility. Keeping that projection
restored prevents a later declaration edit from dropping callers in unchanged
importing files. Resolution borrows the saved file facts instead of cloning
their complete symbol outline.

Recovery adopts contiguous posting vectors, derives the removal projection
once, and reserves the graph's known node count. Atomic snapshot replacement,
graph validation, content reconciliation, checkpoint recovery, and query
freshness remain at their existing owners. JSON slice decoding can use a
faster parser path, at the cost of temporarily holding the serialized file;
operation peak RSS must be measured before deciding whether to ship it.
[serde_json documents that tradeoff](https://docs.rs/serde_json/latest/serde_json/fn.from_reader.html).

## Another factor of two

A phase consuming less than half of total latency cannot deliver a second
factor of two even if eliminated. Nested phase totals are not additive.
Use exclusive phase costs and include persistence and reconciliation in the
readiness boundary.

The snapshot is a sectioned binary file: file facts, the symbol graph, trigram
postings, and word postings, each length-prefixed so the four decode on
separate threads. Graph, trigram, and word restoration also run side by side,
and the restore-path maps use a seeded fast hasher. On the #9712 corpus this
made unchanged restart about 3.2 times faster and every restart workload at
least twice as fast; the [snapshot measurements](code-index-snapshot-measurements.md)
record the paired results and their shared-host limits.

The symbol graph and both posting stores now restore as shared immutable
generations, the structure of [Tantivy's segment architecture](https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md)
applied to the existing owners rather than a second index. The graph's base
holds nodes sorted by id, an id-to-slot table, and forward and reverse
adjacency as contiguous arrays with offsets. The name index and the posting
maps are shared bases too. Edits go to a copy-on-write delta: replaced edge
lists and name buckets, added nodes, and tombstones for removed nodes and for
files whose postings changed. Reads consult the delta first. Only the delta
keeps a per-file removal index, so restoring postings no longer derives one for
15 million pairs. Clones, including branch overlays, share the base. Every
mutation path ends at one batch boundary that folds the delta into a fresh base
once it is a quarter of the base. Snapshots encode straight from the live index
in parallel sections, and graph nodes decode in parallel runs. The
[generation measurements](code-index-generation-measurements.md) put unchanged
restart at about 0.65 seconds on a loaded host, from 2.09 for the same host's
baseline, with peak memory down by about 29%.

Read, decode, and restore are now of similar size. Graph nodes share their
file's path and use the registry's static language name, and the snapshot
stores both once per file. The next decoding cut is reading archived nodes in
place. A
[validated zero-copy archive](https://rkyv.org/validation.html) remains the
candidate for the latter. Safe archive access checks pointer bounds,
alignment, and ownership ranges, but not graph identities, edge integrity, or
workspace freshness, and the graph's borrowed-node API would need a reader view
over archived nodes. Mapping the file is unsafe without protection against
out-of-process modification, as
[memmap2 documents](https://docs.rs/memmap2/latest/memmap2/struct.MmapOptions.html).
A restore that reconciled changes publishes the index first and rewrites the
snapshot on a background thread that `wait_until_idle` waits for. The write
holds the index lock only to detach a copy, which the shared generations make
possible without rebuilding those generations, then encodes outside the lock.
The capability's warm coordinator waits for earlier writes before admitting
another restore, rebuild, or explicit write. A restore can therefore write its
reconciliation checkpoints without an older background write replacing them.
Published-index readers can proceed while a write finishes. Independent
capabilities and processes writing the same root remain separate owners.

For cold build, collect independent parsed-file facts on a bounded worker pool,
then apply them through one graph owner and resolve calls after the complete
declaration set exists. Source reading, parser selection, and extracted facts
should have one boundary. Keep the live generation queryable until the next
complete generation is published. Benchmark both single-core and bounded
parallel behavior, peak memory, cancellation, and checkpoint recovery. Merely
parallelizing parsing leaves ingestion, graph assembly, resolution, and
persistence as the serial limit.

For resident edits, first remove workspace-wide projections from per-file
mutation. The previous release's tiny fixture spent roughly 8 milliseconds of
its 9-millisecond update revisiting every file's explicit import edges. The
current mutation projects the edited file's outgoing dependencies and its
direct importers, because replacement of its Module node drops incoming edges
as well. Bulk and new-path changes use the same projection over all files.
Module-wide visibility remains at the dependency owner; it is never expanded
into a cross-product of graph edges.

Module membership is a pure function of the path table and the declared Go
modules, and the index stores both. The workspace walk reports every `go.mod`
it passes, so reconciliation sees created, changed, and deleted manifests,
including nested ones, without probing source directories. The snapshot
persists the declarations its call graph was linked under. When a complete
walk or a host reindex of a manifest changes them, only files whose import
resolution changed are re-resolved and have their calls relinked. Equal
declarations do no work, and known-file edits no longer rebuild membership.

After that measured bottleneck, retain syntax trees under a memory budget and
reuse them when the grammar and source revision match. Incremental syntax and
dependency invalidation should share parsed facts with graph and tool consumers
rather than adding a second parser cache. A tiny fixture does not establish
large-file edit percentiles. [rust-analyzer's architecture](https://github.com/rust-lang/rust-analyzer/blob/master/docs/book/src/contributing/guide.md)
provides the useful pattern of change application followed by incremental,
on-demand queries.

## Application policy

Filesystem watching, debouncing, background warming, and joining an in-flight
build already exist. Adding another watcher or another warm path is lower
priority than sharing one live content index for a canonical workspace across
sessions. The current capability clone also shares the current-agent slot,
advisory locks, versions, overlays, and resolver projection. It is not a
content-only cache. Separate the workspace facts and read generation from
request identity, resolver policy, and session overlays before sharing it;
cross-session negative controls must prove those boundaries. Sharing factual
content must not share session approval grants or execution-policy state. Bind
reuse to the canonical workspace, content filters, and parsing configuration;
apply session authority when serving queries.

Keep the visible workspace resident, warm recently used roots when idle, and
evict by measured memory pressure and recency rather than a fixed timer. Apply
changed paths as a coalesced batch through the index owner. Cancel superseded
generations; never label an incomplete or stale generation as fully ready.
Literal search can remain available during warming, but index-required graph
operations need an explicit unavailable/building result and progress.

A persistent watcher cursor can avoid an unnecessary restart scan only where
the operating system guarantees replay. [Apple's FSEvents guide](https://developer.apple.com/library/archive/documentation/Darwin/Conceptual/FSEvents_ProgGuide/UsingtheFSEventsFramework/UsingtheFSEventsFramework.html)
supports replay from event IDs and requires rescans after dropped events.
Inotify is a live notification mechanism; it is not an equivalent durable
history across process downtime. Unknown history, overflow, changed filters,
or changed parsing configuration requires reconciliation. Do not substitute
mtime heuristics for an explicit freshness contract.

On NTFS, [the Windows USN change journal](https://learn.microsoft.com/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal)
can supply replay, provided the journal identity and retained cursor range are
validated. A deleted or recreated journal requires a scan. Platform adapters
should normalize replay validity and changed paths into one freshness contract.

Schedule foreground queries at interactive priority and warming at background
priority, using [Apple's quality-of-service classes](https://developer.apple.com/documentation/Dispatch/DispatchQoS/userInitiated)
where available. Do not change debounce defaults without measuring the host's
watch-to-query path: native watcher and maintenance coalescing already exist,
and not every path applies both timers. Millisecond index improvements cannot
remove hundreds of milliseconds of scheduling delay by themselves.

## Hardware and mature techniques

Prioritize these experiments using the remaining phase budget:

1. **Compact reader layout and shared strings.** Sorted integer postings,
   contiguous adjacency, and interned strings reduce allocation, hashing,
   pointer chasing, serialized bytes, and cache misses together.
2. **SIMD decoding when JSON remains material.**
   [simd-json](https://github.com/simd-lite/simd-json/blob/main/README.md) supports
   runtime x86 AVX2/SSE4.2 selection and ARM NEON. Measure typed deserialization,
   allocation, malformed input, and peak memory on the actual snapshot. Generic
   JSON throughput is not the cost of constructing this index.
3. **Density-aware integer representation.**
   [Roaring's compressed bitmap design](https://arxiv.org/abs/1709.07821) is useful
   for dense file-membership lists. Sparse vectors may be cheaper; word lines
   and duplicate occurrences still need their own exact representation. Measure
   the corpus distribution before selecting a container policy.
4. **Bounded parallel parsing and incremental dependency queries.** Exploit
   independent files while preserving deterministic graph facts and one owner
   for publication, cancellation, and recovery.
5. **Read-ahead and asynchronous I/O after proving storage wait dominates.**
   [posix_fadvise](https://man7.org/linux/man-pages/man2/posix_fadvise.2.html) and
   [madvise](https://man7.org/linux/man-pages/man2/madvise.2.html) provide access
   hints. [io_uring](https://man7.org/linux/man-pages/man7/io_uring.7.html) can
   batch I/O; registered buffers and polling target high-throughput workloads
   and consume resources. They do not accelerate CPU-bound graph reconstruction.

PMU counters (cycles, instructions, cache/branch misses), page faults, actual
external CPU activity, I/O pressure, and RSS distinguish CPU, memory, and
storage limits. Record denied counters rather than silently omitting them.
Use runtime CPU dispatch for distributable binaries: architecture-specific
compiler flags can defeat portability, as [simdjson's implementation guide](https://github.com/simdjson/simdjson/blob/master/doc/implementation-selection.md)
explains. GPU or neural-accelerator offload is lower priority: pointer-heavy,
mutable graph reconstruction is a poor match, and no measured transfer or
parallelism budget currently justifies another execution path.

## Comparison boundary

[ripgrep](https://github.com/BurntSushi/ripgrep/blob/master/README.md) combines
SIMD matching with buffered/mapped input and parallel traversal. It is a good
literal-search fallback and an architectural reference. Its throughput is not
a calibrated comparison to AST parsing, a multi-million-edge graph, posting
construction, and durable recovery. A numerical state-of-the-art claim needs
the same corpus, query semantics, cold/warm boundary, cache conditions,
hardware, and correctness criteria on each implementation.
