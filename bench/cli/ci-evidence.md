# Cold-start CI evidence

The `CLI cold-start budget` workflow has independent ceilings: 45 minutes
for release-build setup, five minutes for measurement, and 55 minutes for
the entire job. These limits do not change the per-command startup budgets
in `budgets.toml`.

The reporting step writes `cold-start-evidence.json` and adds it to the job
summary. The following upload step saves it and any measurement receipt in
the `cold-start-evidence` artifact. Its `status` is one of:

| Status | Meaning |
| --- | --- |
| `setup_failed` | Release-build setup did not succeed; no startup verdict is claimed. |
| `measurement_failed` | Measurement did not complete successfully with a complete, matching receipt. |
| `budget_failed` | The benchmark completed its measurements and reported startup-budget or baseline regressions. |
| `passed` | Setup and measurement succeeded, every expected command was measured, and no regression was reported. |

The report includes the source revision, both workflow step outcomes,
expected and measured counts, pending commands, unexpected commands, and
the benchmark's failure reasons. Counts are null when the measurement
receipt is unreadable, rather than zero. Missing, empty, partial, or
wrong-revision receipts cannot produce `passed`.

`cold-start-measurement.json` is the Harn benchmark's own receipt. It
contains the selected commands, their measured `cold_ms` values, source
revision, and failure reasons. `HARN_EXT_CLI_COLD_START_REPORT` selects its output
path for local runs; omitting that variable preserves the normal console
and baseline-file behavior. A subprocess failure or timeout can prevent
this receipt from being written and is reported as `measurement_failed`,
not as a measured budget regression.
