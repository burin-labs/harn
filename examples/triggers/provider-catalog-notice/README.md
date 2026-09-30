# Provider catalog notice

Optional six-hour cron wrapper for the trusted provider-notice workflow. A
public email, webhook, or document-store adapter writes one neutral
`ProviderNotice` JSON file, then sets:

- `HARN_PROVIDER_NOTICE_WORKTREE`: a clean, dedicated Harn worktree;
- `HARN_PROVIDER_NOTICE_FILE`: the adapter's notice JSON path;
- `HARN_PROVIDER_NOTICE_EXTRACTION_FILE`: optional deterministic extraction
  replay for testing.
- `HARN_PROVIDER_NOTICE_EXTRACTION_PROVIDER` and
  `HARN_PROVIDER_NOTICE_EXTRACTION_MODEL`: optional own-key inference route.
  Set both to a catalogued route whose notice extraction has been measured;
  otherwise the workflow uses that provider's catalog default.
- `HARN_PROVIDER_NOTICE_TOOL_PROBE_REPORT`: optional live Harn tool-probe
  report for the notice's exact provider and model. Enabling native tools
  requires this evidence; saved responses and raw endpoint overrides cannot
  satisfy it.

The handler invokes `scripts/provider_catalog_notice.harn --apply --open-pr`.
That workflow validates provenance and current catalog state, applies only a
typed constrained edit, runs the catalog checks, and opens a draft PR. It never
merges.

To supply native-tool evidence, run `harn provider tool-probe <provider>
--model <model> --tool-format native --json` and save its report. The workflow
validates that report through `harn provider tool-scorecard` and records the
result in its notice receipt. Missing or mismatched evidence produces an
incomplete proposal without applying a catalog change or opening a PR.

## Verify

```sh
harn check lib.harn
```
