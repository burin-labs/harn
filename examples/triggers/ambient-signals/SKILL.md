---
name: ambient-signals
short: Fire ambient signals from GitHub monitors and alert webhooks.
description: Monitor pack that polls GitHub CI, pull requests, runners, and rate limits, and normalizes Sentry, Grafana, and Honeycomb alerts into signals.
when-to-use: Use when running agents should hear about red CI, review backlogs, offline runners, exhausted rate limits, or production alerts.
---
# Ambient signals monitor pack

Edit `MONITORS` in `lib.harn` to name repositories and thresholds. Each
monitor calls a `std/signals` evaluator and `signal_apply`, so repeated polls of
an unchanged condition deliver nothing and each change delivers one reminder.
