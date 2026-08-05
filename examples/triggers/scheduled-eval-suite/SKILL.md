---
name: scheduled-eval-suite
short: Run an eval pack from a cron trigger.
description: Cron trigger example that binds directly to an eval pack handler.
when-to-use: Use when scheduling regression evals through Harn triggers.
---
# Scheduled eval suite

Customize `harn.eval.toml`, keep `handler = "eval_pack://scheduled-regression"`,
and tune `budget` / `concurrency` in `harn.toml` for the expected eval cost.
