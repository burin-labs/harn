---
name: eval-suite-regression-notifier
short: Notify Slack when a scheduled eval suite regresses or improves.
description: Run a cron eval pack, gate against prior ledger rows, and post Slack only on gate flips.
when-to-use: Use when scheduling regression evals that should alert a Slack channel only on material verdict changes.
---
# Eval suite regression notifier

Customize `harn.eval.toml` for the suite, keep `notify_on_eval_gate_flip` as the
cron handler, and configure the Slack channel/token secret in the trigger event
payload or deployment wrapper.
