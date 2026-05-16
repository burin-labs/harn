---
name: context-maintenance
short: Queue context refresh and crystallization jobs from lifecycle hooks.
description: Host-neutral lifecycle hook recipe for non-blocking context maintenance.
when-to-use: Use when a coding host needs durable context.refresh or context.crystallize jobs.
---
# Context maintenance hooks

Install the `[[hooks]]` entries in `harn.toml`, keep hook handlers fast, and
run `context.refresh` / `context.crystallize` in a host-owned background lane.
Persist the returned `harn.context_maintenance.job_receipt.v1` receipts so
replay can include the job or emit a deterministic skipped receipt.
