# CLI cold-start budget gate

This directory hosts the cold-start budget infrastructure for ported CLI
subcommands tracked by the self-host epic [#2293] (G5, [#2298]).

The goal is narrow: catch regressions in the fixed dispatch + parse +
typecheck + compile + bytecode-load cost that a user pays on *every*
invocation of a `.harn`-ported subcommand. It is **not** an end-to-end
wall-clock budget for the work the subcommand performs.

[#2293]: https://github.com/burin-labs/harn/issues/2293
[#2298]: https://github.com/burin-labs/harn/issues/2298

## Status

This harness is intentionally **minimal**. G5 ships the skeleton — the
runner, the budgets file, an empty baseline, and a CI workflow stub.
Per-command budgets and recorded baselines land alongside each W ticket
(W1-W13) as the corresponding Rust handler is ported to `.harn`, since
each port has its own "before" and "after" cold-start number that the
ticket author records here.

The GitHub Actions workflow exists (`cli-cold-start-budget.yml`) but is
**not yet a required status check**. It runs on PRs that touch the
dispatch wedge, the embedded `std/cli` scripts, or the bytecode cache,
and reports its result for review. Promoting it to a required check
happens once at least one ported command has its baseline committed and
the runner pool is stable.

## What "cold start" means here

For each tracked subcommand the runner:

1. Wipes `HARN_CACHE_DIR` (a process-local temp directory, never
   `~/.cache`).
2. Sets `HARN_BYTECODE_CACHE=0` so any cache the binary writes is
   ignored on the next invocation.
3. Times the full `harn <subcommand> [args]` process from `fork` to
   `exit`.
4. Repeats `N` times (default `20`) and reports the median.

Wall time is measured by `hyperfine` when it is on `$PATH`; the Harn
controller falls back to `monotonic_ms()` around the isolated subprocess
otherwise. The fallback is intentional — `hyperfine` is not in the default
dev-setup install on macOS, and we do not want the gate to silently regress
to "skipped" when contributors run it locally.

## Running locally

```bash
make bench-cli-cold-start
```

That target is a thin wrapper around:

```bash
./scripts/bench_cli_cold_start.sh
```

Useful flags:

- `--iterations N` (default 20) — number of timed runs per subcommand.
- `--no-build` — skip `cargo build --release --bin harn` when you have
  already built it.
- `--baseline FILE` (default `perf/cli/baselines/main.json`) — JSON file
  to compare against and update on a passing run.
- `--budgets FILE` (default `perf/cli/budgets.toml`) — per-command
  budget table.
- `--commands name1,name2` — restrict the run to a subset of the
  configured commands. Useful when iterating on one W ticket.
- `--update-baseline` — explicitly overwrite the current commit's slot
  in the baseline file with this run's medians. Without this flag the
  runner only fills in slots that were previously empty.

Set `HARN_BIN=/path/to/harn` to use a binary outside `target/release/`.

## Comparison rules

A subcommand fails the gate when *either* of these holds:

- Median cold start exceeds the budget in `budgets.toml`
  (`commands.<name>.cold_ms`, falling back to `defaults.cold_ms`).
- A baseline exists in `baselines/main.json` for the most recent
  committed `main` commit, and the current run's median is greater
  than `baseline * regression_x` (default `1.25`).

The runner prints both numbers and the delta for every command, then
exits non-zero if any command failed. A passing run writes its medians
to `baselines/main.json` under the current `HEAD` commit SHA, but only
for keys that were not already populated. This keeps the baseline
file a monotonic append-only ledger — adding noisy retroactive
overwrites breaks bisect on regression triage. Use
`--update-baseline` only when you deliberately want to refresh a slot
(e.g. you tightened a budget and need a fresh reference).

## Baseline file format

```json
{
  "<commit_sha>": {
    "host": "<uname -s + arch>",
    "harn_version": "<harn --version>",
    "captured_at": "<ISO 8601 UTC>",
    "commands": {
      "version":      { "cold_ms": 12.4 },
      "try --help":   { "cold_ms": 11.9 },
      "trace import": { "cold_ms": 13.7 }
    }
  }
}
```

The baseline file commits cleanly as an empty JSON object (`{}`) and
is filled in over time. We deliberately keep all historical entries
rather than truncating to a sliding window — a full record makes it
straightforward to graph the trend per command and to bisect a
regression to a specific merge.

A `warm_ms` field is allowed alongside `cold_ms` for future use, but
G5 only writes `cold_ms`. Per-command warm-start tracking is part of
the W-ticket work, not the G5 skeleton.

## Tracked commands (initial set)

The harness starts with the two currently enabled smallest,
most-deterministic subcommands. Each one exits in well under a budget's
worth of work, so the measured time is dominated by CLI fixed cost — exactly
what we want to budget.

| Bench key       | Invocation                                                          | Source ticket |
| --------------- | ------------------------------------------------------------------- | ------------- |
| `version`       | `harn version`                                                      | W1 (#2297)    |
| `try --help`    | `harn try --help`                                                   | W2 placeholder |

`trace import` is intentionally not enrolled yet; `budgets.toml` carries the
current sandbox rationale and re-enable plan.

Future W tickets append rows here when they enroll a command. Two
rules:

- The invocation must terminate quickly (well under the budget) and
  must not require network, an LLM provider, or a real workspace.
- The bench key must match the `[commands.<key>]` section in
  `budgets.toml` so the runner can resolve the per-command budget.

## Why this is minimal-first

The G5 epic comment (#2298) explicitly calls for baselines on every
W-ticket command. We chose to land the skeleton first so:

- The shape (TOML budgets + JSON-per-commit baselines + bash runner +
  CI workflow) is reviewable in isolation, not buried under 13
  baseline values that nothing reads yet.
- Each W ticket adds *its* row, *its* budget, and *its* recorded
  baseline in the same PR that ports the command. The author of that
  PR has the strongest signal about a sane budget — they just stared
  at the dispatch code.
- The bench host story (self-hosted macOS pool, see the runner-fleet
  notes in the epic) can stabilize independently before we make a
  threshold gate required.

When W1 lands its baseline, we promote the workflow to a required
check.
