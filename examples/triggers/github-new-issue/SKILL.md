---
name: github-new-issue
short: Customize a GitHub issue triage webhook pipeline.
description: GitHub webhook trigger example with a local predicate and handler.
when-to-use: Use when reacting to new GitHub issues from Harn.
---
# GitHub new issue

Wire the webhook secret in `harn.toml`, keep `dedupe_key = "event.dedupe_key"`,
and customize `lib.harn` to classify or route the issue.
