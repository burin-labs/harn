---
name: local-a2a-dispatch
short: Switch one trigger handler between local and A2A dispatch.
description: Demonstrates the same issue-triage handler behind local and remote A2A trigger manifests.
when-to-use: Use when testing trust-boundary migration for a trigger handler.
---
# Local / A2A dispatch

Use `harn.local.toml` for in-process handler dispatch and `harn.remote.toml`
for the remote A2A handler. The default `harn.toml` keeps the local shape so
the trigger example library check can validate `lib.harn` consistently.
