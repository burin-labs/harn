# Code-index generation measurements

Measured for [issue #9771](https://github.com/burin-labs/harn/issues/9771),
following the [snapshot measurements](code-index-snapshot-measurements.md).
These are archived October 12–13, 2026 artifact comparisons. The recovered
branch's write-ordering correction and current dependencies have separate
correctness checks; the timings below remain specific to the recorded binaries.
The baseline is main with the #9743 snapshot format. The candidate restores
the symbol graph and the trigram and word postings as shared immutable
generations with copy-on-write edits and file tombstones, decodes graph nodes
in parallel sections, and writes snapshots straight from the live index. The
Every trial, both binary digests, the seed digest, phase timings, and
skipped-file inventories were recorded.

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
met the 0.25-core external-load bound.

| Operation | Baseline median | Candidate median | Paired ratio [95% interval] | Pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 0.525 s | 0.484 s | 0.923 [0.684, 0.941] | 5 |
| Body edit on restart | 1.380 s | 1.175 s | 0.880 [0.825, 0.883] | 3 |
| Resident body edit | 0.206 ms | 0.210 ms | 1.005 [0.981, 1.044] | 3 |

The snapshot shrinks from 191,222,003 to 143,450,072 bytes, and peak RSS for
an unchanged restart falls from 707 to 575 MiB. Every pair has identical
corpus, graph, and posting digests.

## Persisting after publication

The October 13 candidate published the reconciled index before rewriting the
snapshot on a background thread. Its write detached a copy under the index
lock (about 43 milliseconds here, mostly the file table), then encoded and
wrote outside the lock. The baseline was source `70bc0eee20`, binary SHA-256
`4a4aab73bf8d…`; the candidate was source `3c1d0cb59a`, binary `c7876135113b…`.
Measured on a quiet host against the per-file strings build, each write landing
and the registered queries that follow readiness were recorded.

| Operation | Baseline ready | Candidate ready | Ready plus queries, baseline / candidate | Write landed | Ready ratio [95% interval] |
| --- | --- | --- | --- | --- | --- |
| Body edit on restart | 1.121 s | 0.501 s | 1.134 / 0.554 s | 1.173 s | 0.447 [0.440, 0.450] |
| Declaration edit on restart | 1.114 s | 0.500 s | 1.123 / 0.552 s | 1.186 s | 0.448 [0.442, 0.451] |
| Changed HEAD | 1.275 s | 0.680 s | 1.287 / 0.735 s | 1.310 s | 0.536 [0.479, 0.542] |

Three accepted pairs per workload, with identical digests. Counting the first
queries, the recorded candidate was usable about twice as soon after an edit;
those queries took about 50 milliseconds longer because they overlapped the copy.
An earlier version encoded under the lock, so its first queries waited about
0.33 seconds; that measurement is superseded. Peak RSS rises by up to 31 MiB
while the detached copy exists.

## Cumulative quiet-host result

The final #9712 artifact, binary SHA-256 `785cded9e739…`, against the October 13
candidate `c7876135113b…` from source `3c1d0cb59a`, on a quiet host.

| Operation | #9712 median | October 13 candidate median | Paired ratio [95% interval] | Accepted pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 4.186 s | 0.489 s | 0.117 | 2 of 5 |
| Body edit on restart | 7.235 s | 0.498 s | 0.069 [0.068, 0.071] | 3 of 3 |
| Changed HEAD | 7.483 s | 0.675 s | 0.090 [0.078, 0.091] | 3 of 3 |
| Resident body edit | 1.3 ms | 0.2 ms | 0.163 [0.160, 0.168] | 3 of 3 |

The recorded candidate's unchanged restart was about 8.5 times faster, a restart
after an edit about 14 times faster, and peak RSS fell from 1,089 to 576 MiB.
Every pair has identical corpus, graph, and posting digests. Only two unchanged
pairs met the load bound; their ratio has no confidence interval.

## Residuals

- Decoding is now the largest unchanged-restart phase. Reading archived nodes
  in place would cut it further.
