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
