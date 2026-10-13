# Code-index parallel build measurements

Measured for [issue #9744](https://github.com/burin-labs/harn/issues/9744),
following the [snapshot measurements](code-index-snapshot-measurements.md).
The baseline is `main` at the time of the change. The candidate reads, hashes,
tokenizes, and parses files on a bounded pool of worker threads, installs each
file's facts on one publisher in walk order, and builds trigram postings in one
pass on a separate thread. The [complete trial record](perf/code-index-9744.json)
keeps every trial, both binary digests, and per-phase timings.

## Workload and controls

The corpus is a private copy of the frozen #9712 corpus: 8,464 indexed files,
752,584 graph nodes, 5,036,843 edges, and 441,301 resolved calls. A cold build
is timed from the start of the walk to the end of the snapshot write, and every
trial must then answer the registered Cypher, word, trigram, and literal
queries.

Each of six rounds runs `main` and the candidate at 1, 2, 4, and 6 fact
workers, in an order rotated per round. All 30 trials have identical corpus,
graph, and posting digests and identical file, node, edge, and resolved-call
counts, so every worker count publishes the same index.

These runs are on a shared host. CI kept two to four cores busy through almost
every trial, so only one trial met the one-core external-load bound. Every
complete round is reported. Absolute times are about 28% slower than the
quiet #9712 baseline; the paired ratios are the result.

## Results

Ratios are the median paired candidate/`main` readiness with a 95% bootstrap
interval from 10,000 resamples, seed 9744.

| Arm | Readiness median | Paired ratio [95% interval] | CPU seconds | Peak RSS |
| --- | --- | --- | --- | --- |
| `main` | 31.93 s | 1 | 44.2 | 1,487 MiB |
| 1 worker | 29.67 s | 0.90 [0.82, 0.97] | 44.3 | 1,481 MiB |
| 2 workers | 15.21 s | 0.47 [0.40, 0.51] | 45.8 | 1,528 MiB |
| 4 workers | 9.97 s | 0.31 [0.27, 0.32] | 46.4 | 1,538 MiB |
| 6 workers | 9.14 s | 0.28 [0.24, 0.30] | 52.6 | 1,548 MiB |

Four workers make a cold build and persist about 3.2 times faster for 5% more
CPU time and 3% more peak memory. Six save another 8% for 13% more CPU, inside
overlapping intervals, so the default is four, capped by the host's available
parallelism. One worker is already 10% faster than `main`, because trigram
postings no longer go through per-file replacement on the publisher.

Reconciliation uses the same pool. Four alternating pairs of each restart
scenario show no regression, and every pair keeps graph and posting parity:

| Scenario | `main` median | Candidate median | Paired ratio [95% interval] |
| --- | --- | --- | --- |
| Changed HEAD, all 8,464 files content-checked | 3.34 s | 3.05 s | 0.91 [0.86, 1.18] |
| 64 edited files | 5.08 s | 4.78 s | 0.94 [0.81, 1.22] |

## Where the time goes

Median phases at four workers, in seconds. Worker phases add up across threads,
so they exceed wall time.

| Phase | `main` | 4 workers | Thread |
| --- | --- | --- | --- |
| Tree-sitter parse | 17.76 | 20.14 | workers |
| Call-site sweep | in graph install | 1.76 | workers |
| Trigram extraction | in trigram install | 1.75 | workers |
| Word tokenization | in word install | 1.20 | workers |
| Trigram install | 4.47 | off the publisher | installer |
| Word install | 1.61 | 1.80 | publisher |
| Graph install | 3.95 | 1.64 | publisher |
| Incoming references | 1.33 | 1.24 | publisher |
| Call resolution | 0.49 | 0.50 | publisher |
| Publisher waiting for facts | | 2.85 | publisher |
| Build total | 30.17 | 8.28 | |
| Snapshot capture and write | 1.63 | 1.57 | |

The remaining serial budget is the publisher's own work, about 5.4 seconds:
word postings, graph installation, incoming references, and call resolution,
plus 1.6 seconds of persistence. More workers shrink only the 2.85 seconds the
publisher spends waiting, which is why six workers barely help.

## Robustness

The tests exercise each failure path through the build and reconciliation
paths the product uses:

- Workers never claim more than eight files per worker beyond the publisher,
  so queued facts stay bounded however slowly publication runs.
- A publisher that stops or panics, and a worker that panics, stop every
  thread; nothing after the failed file is published.
- A checkpoint interrupted mid-reconciliation with four live workers
  restores a complete graph and resumes.
- A file the grammar rejects keeps a module-only graph slice; unreadable files
  are skipped and counted.
- Unchanged bytes under a newer timestamp skip extraction.

## Residuals

- A quiet-host repetition of the matrix.
- The single publisher. Word postings and incoming references could move to
  the bulk path the trigrams use, but references read word postings while the
  build publishes, so they need a different ordering argument.
- Persistence after the build, about 1.6 seconds. Publishing before persisting
  would remove it from readiness.
- A host-cancellable build. Code-index builtins receive no cancellation token
  today, so a build runs to completion once started; the pool is ready to stop
  as soon as one exists.
