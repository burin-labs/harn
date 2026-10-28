# Code-index snapshot measurements

Measured for [issue #9743](https://github.com/burin-labs/harn/issues/9743),
following the [restoration measurements](code-index-restoration-measurements.md).
The baseline is the final #9712 optimized release probe. The candidate replaces
the JSON snapshot with a sectioned binary file, decodes and restores its
sections in parallel, and uses a seeded fast hasher on the restore-path maps.
Every trial, both binary digests, the seed digest, phase timings, and
skipped-file inventories were recorded.

## Workload and controls

The corpus, fixture edits, 64-file selection, readiness boundary, and
registered queries are the same as in #9712: 8,464 indexed files, 752,584
graph nodes, 5,036,843 edges, and 441,301 resolved calls. Each binary prepares
its own snapshot from one shared JSON seed outside the timer, and pair order
alternates.

These runs are on a shared host. CI jobs kept running on the measurement
machine, because pausing its runners was not authorized for this session.
Each trial waited for at least 60% idle CPU over three seconds, and the record
keeps each trial's external CPU load. No trial met the 0.25-core external-load
bound the #9712 protocol used, so every complete pair is reported and none is
labelled accepted. Absolute times are therefore slower and noisier than the
figures from issue #9712; the paired ratios are the result. A quiet-host rerun has not
been done.

## Results

Ratios are the median paired candidate/baseline readiness with a 95% bootstrap
interval from 10,000 resamples, seed 9743.

| Operation | Baseline median | Candidate median | Paired ratio [95% interval] | Pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 5.154 s | 1.550 s | 0.308 [0.279, 0.339] | 5 |
| Body edit on restart | 8.127 s | 3.470 s | 0.413 [0.382, 0.427] | 3 |
| Declaration edit on restart | 8.422 s | 3.243 s | 0.408 [0.374, 0.459] | 3 |
| Changed HEAD | 8.886 s | 3.543 s | 0.404 [0.321, 0.408] | 3 |
| 64-file reconciliation | 10.741 s | 5.405 s | 0.503 [0.497, 0.510] | 3 |
| Resident body edit | 1.357 ms | 0.418 ms | 0.296 [0.266, 0.315] | 3 |
| Resident declaration edit | 1.393 ms | 0.645 ms | 0.463 [0.268, 0.506] | 3 |
| Cold build and persist | 27.295 s | 25.348 s | 0.959 [0.867, 0.995] | 3 |

Unchanged restart is about 3.2 times faster, and every restart workload is at
least twice as fast. Every pair has identical corpus, graph, and posting
digests and identical file, node, edge, and resolved-call counts, and every
trial answers all four registered queries. The snapshot shrinks from
583,840,101 to 191,221,954 bytes. Peak RSS for unchanged restart falls from
1,089 to 993 MiB; edited restarts are within 1% of the baseline.

## Where the time goes

Median unchanged-restart phases, in seconds. Parallel phases overlap, so the
rows do not sum to readiness.

| Phase | Baseline | Candidate |
| --- | --- | --- |
| Read | 0.350 | 0.112 |
| Decode | 2.384 | 0.362 |
| Decode: graph section | | 0.362 |
| Decode: word section | | 0.108 |
| Decode: trigram section | | 0.078 |
| Decode: file facts | | 0.062 |
| Restore | 2.278 | 0.994 |
| Restore: graph | 1.558 | 0.967 |
| Restore: trigrams | 0.498 | 0.345 |
| Restore: words | 0.193 | 0.185 |
| Reconcile | 0.062 | 0.058 |

Graph reconstruction is now the long pole: 0.97 of 1.55 seconds. Trigram and
word restoration run beside it and no longer add to readiness. Graph decoding
is the next largest phase.

After an edit, a restart also captures and rewrites the whole snapshot. The
write falls from about 2.5 to 0.95 seconds, but capture (about 0.65 seconds)
and write together are still nearly half of an edited restart. Cold build
improves only through the faster write; parsing dominates it.

## Quiet-host confirmation

The October 13, 2026 matrix compared #9712 binary SHA-256 `785cded9e739…` with
the #9743 binary `0dfd5c397665…`. It was repeated while CI happened to be idle, so no
runners were paused. CI returned partway through, so the summary uses only pairs
whose two trials stayed under the 0.25-core external-load bound. The paired
ratios agree with the shared-host results above.

| Operation | Baseline median | Candidate median | Paired ratio [95% interval] | Accepted pairs |
| --- | --- | --- | --- | --- |
| Unchanged restart | 4.178 s | 1.338 s | 0.320 [0.315, 0.329] | 4 of 5 |
| Body edit on restart | 7.367 s | 2.888 s | 0.392 | 1 of 3 |
| Declaration edit on restart | 7.333 s | 2.895 s | 0.395 | 1 of 3 |
| Changed HEAD | 7.537 s | 3.037 s | 0.407 [0.314, 0.411] | 3 of 3 |
| 64-file reconciliation | 9.304 s | 4.798 s | 0.515 [0.514, 0.520] | 3 of 3 |
| Resident body edit | 1.3 ms | 0.4 ms | 0.311 [0.310, 0.327] | 3 of 3 |
| Resident declaration edit | 1.3 ms | 0.5 ms | 0.394 [0.372, 0.398] | 3 of 3 |
| Cold build and persist | 24.041 s | 22.336 s | 0.931 [0.876, 0.934] | 3 of 3 |

Every pair, accepted or not, has identical corpus, graph, and posting digests.
The baseline's quiet unchanged restart, 4.18 seconds, matches the 4.157
seconds recorded for it in issue #9712. The body and declaration rows each have
only one accepted pair, so their ratios have no confidence interval. These
archived results describe those binaries and exclude later ordering changes.

## Residuals

- More accepted body and declaration pairs on a quiet host.

Graph reconstruction and the snapshot capture and write on edited restarts,
the other residuals this page originally listed, were addressed afterwards;
see the [generation measurements](code-index-generation-measurements.md).
