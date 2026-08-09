# Provider catalog notice

Optional six-hour cron wrapper for the trusted provider-notice workflow. A
public email, webhook, or document-store adapter writes one neutral
`ProviderNotice` JSON file, then sets:

- `HARN_PROVIDER_NOTICE_WORKTREE`: a clean, dedicated Harn worktree;
- `HARN_PROVIDER_NOTICE_FILE`: the adapter's notice JSON path;
- `HARN_PROVIDER_NOTICE_EXTRACTION_FILE`: optional deterministic extraction
  replay for testing.

The handler invokes `scripts/provider_catalog_notice.harn --apply --open-pr`.
That workflow validates provenance and current catalog state, applies only a
typed constrained edit, runs the catalog checks, and opens a draft PR. It never
merges.

## Verify

```sh
harn check lib.harn
```
