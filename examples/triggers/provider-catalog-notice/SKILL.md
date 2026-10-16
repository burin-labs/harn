---
name: provider-catalog-notice
short: Schedule trusted provider notices as review-only catalog PRs.
description: Transport-neutral cron adapter for provider catalog change notices.
when-to-use: Use when a public adapter can produce ProviderNotice JSON files.
---
# Provider catalog notice

Keep transport authentication in the adapter. Give the scheduled Harn workflow
only a neutral notice file, a clean catalog worktree, and draft-PR authority.
Do not add mailbox- or product-specific parsing to the workflow.

Configure the extraction provider and model through the existing trigger
environment settings. For a native-tool promotion, supply the exact route's
live Harn adapter report with `HARN_PROVIDER_NOTICE_TOOL_PROBE_REPORT`.
Use `harn provider tool-probe` and validate the report with
`harn provider tool-scorecard --tool-probe-report <report> --json`.
Saved responses, legacy reports, and raw endpoint overrides remain unverified;
the workflow records an incomplete proposal until adapter evidence is available.
