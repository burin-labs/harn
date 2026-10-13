# Code-index generation measurements

Measured for [issue #9771](https://github.com/burin-labs/harn/issues/9771),
following the [snapshot measurements](code-index-snapshot-measurements.md).
The baseline is main with the #9743 snapshot format. The candidate restores
the symbol graph and the trigram and word postings as shared immutable
generations with copy-on-write edits and file tombstones, decodes graph nodes
in parallel sections, and writes snapshots straight from the live index. The
[complete trial record](perf/code-index-9771.json) keeps every trial, both
binary digests, the seed digest, phase timings, and skipped-file inventories.

## Workload and controls

The corpus, fixture edits, 64-file selection, readiness boundary, and
registered queries match the earlier measurements: 8,464 indexed files,
752,584 graph nodes, 5,036,843 edges, and 441,301 resolved calls. Each binary
prepares its own snapshot from one shared JSON seed outside the timer, and pair
order alternates.

These runs are on a shared host. CI jobs and another session's work kept 5
to 11 cores busy, and trials started without waiting for an idle host because
the host was never idle for long. The record keeps each trial's external load.
Absolute times are slower and noisier than on a quiet host; the paired ratios
are the result. One unchanged-restart baseline trial took 17.3 seconds during
a load spike. Its pair is retained, which widens that row's interval.

## Results

Ratios are the median paired candidate/baseline readiness, with the 95%
bootstrap interval from 10,000 resamples, seed 9743.

| Operation | Baseline median | Candidate median | Paired ratio [95% interval] | Pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 2.089 s | 0.652 s | 0.354 [0.034, 0.363] | 5 |
| Body edit on restart | 3.802 s | 1.769 s | 0.456 [0.425, 0.465] | 3 |
| Declaration edit on restart | 3.581 s | 1.729 s | 0.461 [0.404, 0.511] | 3 |
| Changed HEAD | 5.030 s | 1.797 s | 0.412 [0.260, 0.429] | 3 |
| 64-file reconciliation | 7.888 s | 4.445 s | 0.564 [0.543, 0.613] | 3 |
| Resident body edit | 0.472 ms | 0.275 ms | 0.606 [0.566, 0.651] | 3 |
| Resident declaration edit | 0.818 ms | 0.278 ms | 0.382 [0.340, 0.456] | 3 |
| Cold build and persist | 40.209 s | 36.508 s | 0.966 [0.885, 1.113] | 3 |

Unchanged restart is about 2.8 times faster by paired ratio, and restarts
after an edit are 1.8 to 2.4 times faster. Every pair has identical corpus, graph, and
posting digests and identical file, node, edge, and resolved-call counts, and
every trial answers all four registered queries. Peak RSS for unchanged restart
falls from 993 to 707 MiB, for single-file and HEAD restarts from about 1,428
to between 959 and 987 MiB, and for the 64-file case from 1,436 to 1,227 MiB.
Cold build shows no clear change within this host's noise; it now ends with a
graph compaction of 0.8 to 1.0 seconds, and its peak RSS rises from 1,487 to
1,530 MiB while compaction holds both generations.

## Where the time goes

Median unchanged-restart phases, in seconds. Parallel phases overlap, so the
rows do not sum to readiness.

| Phase | Baseline | Candidate |
| --- | --- | --- |
| Read | 0.134 | 0.115 |
| Decode | 0.444 | 0.231 |
| Restore | 1.166 | 0.210 |
| Restore: graph | 1.133 | 0.183 |
| Restore: trigrams | 0.348 | 0.045 |
| Restore: words | 0.255 | 0.054 |
| Reconcile | 0.378 | 0.074 |

Restoration no longer builds per-node maps or a per-file removal index for
15 million posting pairs. It validates ids into a slot table, groups edges into
contiguous forward and reverse arrays, and adopts the posting rows as the
shared base. Read, decode, and restore are now of similar size.

After an edit, the baseline copies the whole index into an owned snapshot
(0.72 seconds for a body edit) and then writes it (1.17 seconds). The candidate
encodes sections in parallel straight from the live index (0.21 seconds) and
writes them (0.57 seconds).

## Per-file strings follow-up

[Pull request #9810](https://github.com/burin-labs/harn/pull/9810) stores each
file's path and language once rather than on every node: in memory as one
shared path and the registry's static language name, and in the snapshot as a
per-file table. It was measured against main on a quiet host, where every trial
met the 0.25-core external-load bound; the
[trial record](perf/code-index-node-strings.json) has the details.

| Operation | Baseline median | Candidate median | Paired ratio [95% interval] | Pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 0.525 s | 0.484 s | 0.923 [0.684, 0.941] | 5 |
| Body edit on restart | 1.380 s | 1.175 s | 0.880 [0.825, 0.883] | 3 |
| Resident body edit | 0.206 ms | 0.210 ms | 1.005 [0.981, 1.044] | 3 |

The snapshot shrinks from 191,222,003 to 143,450,072 bytes, and peak RSS for
an unchanged restart falls from 707 to 575 MiB. Every pair has identical
corpus, graph, and posting digests.

## Residuals

- A quiet-host repetition of this matrix.
- Decoding is now the largest unchanged-restart phase. Reading archived nodes
  in place would cut it further.
- The file write after an edit. Writing sections as they finish encoding, or
  persisting after the index is published, would remove most of it.
