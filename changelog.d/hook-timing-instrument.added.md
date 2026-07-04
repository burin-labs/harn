Added a git-hook duration instrument: `.githooks/lib.sh` now appends one
NDJSON line per pre-commit/pre-push invocation to `~/.burin/hook-timings.ndjson`
(zero-dep, never changes the hook's exit code, degrades silently if the log
directory is unavailable). Added `scripts/hook_timings_report.sh` to print
p50/p95/max duration per (repo, hook), and `scripts/gha_spend_report.sh` to
print estimated GitHub Actions spend per repo/workflow.
