# Code-index parallel build measurements

Measured for [issue #9744](https://github.com/burin-labs/harn/issues/9744),
following the [snapshot measurements](code-index-snapshot-measurements.md).
The baseline is `main` after the generation storage of #9790. The candidate
reads, hashes, tokenizes, and parses files on a bounded pool of worker threads,
installs each file's facts on one publisher in walk order, and builds trigram
postings in one pass on a separate thread. The
Every trial, both
binary digests, and per-phase timings were recorded.

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

These runs are on a shared host. CI kept one to seven cores busy through almost
every trial, so only two trials met the one-core external-load bound. Every
complete round is reported. Absolute times are about 29% slower than the
quiet baseline from issue #9712; the paired ratios are the result. An earlier matrix on `main`
before #9790 gave the same ratios within their intervals.

## Results

Ratios are the median paired candidate/`main` readiness with a 95% bootstrap
interval from 10,000 resamples, seed 9744.

| Arm | Readiness median | Paired ratio [95% interval] | CPU seconds | Peak RSS |
| --- | --- | --- | --- | --- |
| `main` | 32.27 s | 1 | 46.1 | 1,412 MiB |
| 1 worker | 30.04 s | 0.90 [0.83, 1.02] | 45.5 | 1,344 MiB |
| 2 workers | 15.48 s | 0.49 [0.44, 0.51] | 45.3 | 1,383 MiB |
| 4 workers | 9.79 s | 0.32 [0.27, 0.39] | 45.2 | 1,399 MiB |
| 6 workers | 8.64 s | 0.27 [0.25, 0.31] | 48.9 | 1,420 MiB |

Six workers make a cold build and persist about 3.7 times faster for 6% more
CPU time and 1% more peak memory. Six beat four in all twelve paired rounds of
this matrix and the earlier one, by 9% to 13%, so the default is six. The
default leaves one core each for the publisher and the trigram installer, so a
host with fewer than eight hardware threads runs fewer workers. One worker is
already 10% faster than `main`, because trigram postings no longer go through
per-file replacement on the publisher.

Reconciliation uses the same pool. Four alternating pairs of each restart
scenario, at four workers, show no regression, and every pair keeps graph and
posting parity:

| Scenario | `main` median | Candidate median | Paired ratio [95% interval] |
| --- | --- | --- | --- |
| Changed HEAD, all 8,464 files content-checked | 1.55 s | 1.46 s | 0.95 [0.90, 0.99] |
| 64 edited files | 3.13 s | 3.22 s | 1.02 [0.83, 1.13] |

## Where the time goes

Median phases at six workers, in seconds. Worker phases add up across threads,
so they exceed wall time.

| Phase | `main` | 6 workers | Thread |
| --- | --- | --- | --- |
| Tree-sitter parse | 17.90 | 23.26 | workers |
| Call-site sweep | in graph install | 2.02 | workers |
| Trigram extraction | in trigram install | 1.95 | workers |
| Word tokenization | in word install | 1.33 | workers |
| Trigram install | 4.37 | off the publisher | installer |
| Word install | 1.63 | 1.69 | publisher |
| Graph install | 4.09 | 1.54 | publisher |
| Incoming references | 1.19 | 1.11 | publisher |
| Graph compaction | 0.72 | 0.69 | publisher |
| Call resolution | 0.49 | 0.49 | publisher |
| Publisher waiting for facts | | 1.90 | publisher |
| Build total | 31.37 | 7.73 | |
| Snapshot encode and write | 0.64 | 0.68 | |

The remaining serial budget is the publisher's own work, about 5.8 seconds:
word postings, graph installation and compaction, incoming references, and
call resolution, plus 0.7 seconds of persistence. More workers shrink only the
1.9 seconds the publisher spends waiting.

## Robustness

The tests exercise each failure path through the build and reconciliation
paths the product uses:

- Workers never claim more than eight files per worker beyond the publisher,
  so queued facts stay bounded however slowly publication runs.
- A publisher that stops or panics, and a worker that panics, stop every
  thread; nothing after the failed file is published, even when several
  workers fail.
- A checkpoint interrupted mid-reconciliation with four live workers
  restores a complete graph and resumes.
- A file the grammar rejects keeps a module-only graph slice; unreadable files
  are skipped and counted.
- Unchanged bytes under a newer timestamp skip extraction, in reconciliation
  and in a host's single-file reindex.

## Residuals

- A quiet-host repetition of the matrix.
- The single publisher. Word postings and incoming references could move to
  the bulk path the trigrams use, but references read word postings while the
  build publishes, so they need a different ordering argument.
- A host-cancellable build. Code-index builtins receive no cancellation token
  today, so a build runs to completion once started; the pool is ready to stop
  as soon as one exists.
