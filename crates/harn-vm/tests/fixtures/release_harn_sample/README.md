# release_harn Crystallization Input — sample

This is a tiny self-contained fixture bundle in the same format
(`release_harn.crystallization_input.v1`) that
[`release_harn.harn`](https://github.com/burin-labs/harn-bump-fleet/blob/main/release_harn.harn)
emits at `${RUN_ROOT}/<run-id>/crystallization-input/`.

It exists so Harn's release-fixture ingest path
([harn#1146](https://github.com/burin-labs/harn/issues/1146)) can be
exercised without depending on a live release run.

## Files

- `manifest.json` — schema/version metadata, release identity, file map.
- `deterministic-events.jsonl` — release facts, findings, step records.
- `agent-events.jsonl` — model-authored review + recovery advice.
- `tool-observations.jsonl` — shell/read observations.

## Source split

Rows with `"source":"deterministic"` are harness-owned facts or
command results. Rows with `"source":"agent"` are model-authored text
or tool summaries and must be treated as advisory.

## Release rehearsed

- Repo: `/Users/example/projects/harn`
- Mode: `ship-pr`
- Mock: `true`
- Version: `0.7.52` → `0.7.53`
- Branch: `main` → `release/v0.7.53`
- Latest tag: `v0.7.52`
- One push failure (`hook_budget_exceeded`) is followed by an agent
  recovery-advice loop and a successful re-push with `--no-verify`.
