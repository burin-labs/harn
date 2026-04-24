---
name: cron-daily-digest
short: Customize a weekday cron digest trigger pipeline.
description: Scheduled Harn trigger example for daily reports and summaries.
when-to-use: Use when adding a simple cron-triggered digest or report.
---
# Cron daily digest

Use `harn.toml` for the schedule and `lib.harn` for the handler body. Keep the
cron handler deterministic and move provider-specific posting into the final
customization step.
