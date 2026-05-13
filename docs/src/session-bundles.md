# Session bundles

A **session bundle** is the portable JSON envelope for moving a
persisted Harn run between local debugging, support handoff, sharing,
and replay workflows. It is built from the existing run record instead
of introducing a second persistence model, so transcripts, tool call
recordings, HITL questions, event-log pointers, replay fixtures, trace
spans, and workflow checkpoints keep one canonical shape.

The current bundle type is `harn_session_bundle` with
`schema_version: 1`. The checked schema lives at
`spec/schemas/session-bundle.v1.schema.json`; regenerate it with:

```bash
make gen-session-bundle-schema
make check-session-bundle-schema
```

## Export modes

`harn session export` reads a persisted run-record JSON file and writes a
bundle:

```bash
harn session export .harn-runs/<run>/run.json --out run.bundle.json
harn session export .harn-runs/<run>/run.json --local --out run.local.bundle.json
harn session export .harn-runs/<run>/run.json --replay-only --out run.replay.bundle.json
```

The default mode is **sanitized**. It walks the whole bundle with the
runtime redaction policy and records every replacement in
`redaction.entries`. Bundle export also treats local path fields such
as `persisted_path`, `project_root`, `source_root`, and `run_path` as
share-sensitive. This is the right mode for support tickets, PR
attachments, issue reproductions, and any hosted share link.

`--local` preserves the raw run record content. It is useful for
workstation-to-workstation debugging inside the same trust boundary.
Import and validate still reject high-confidence secret patterns unless
`--allow-unsafe-secret-markers` is passed.

`--replay-only` applies redaction and then withholds prompt/tool payload
fields such as `content`, `prompt`, `system_prompt`, `arguments`,
`result`, and `private_reasoning`. It keeps deterministic replay
metadata, tool-call hashes, stage transitions, checkpoints, trace spans,
and replay fixtures, so another host can inspect or rerun structure
without receiving prompt text.

Artifacts are omitted unless `--include-attachments` is passed. The
bundle always includes attachment metadata that already exists inside the
embedded run record; the top-level `attachments` array is reserved for
payloads that callers intentionally include.

## Import and validation

Use `harn session validate` to check the envelope without writing
anything:

```bash
harn session validate run.bundle.json
harn session validate run.bundle.json --json
```

Validation rejects:

- missing required schema fields,
- `_type` values other than `harn_session_bundle`,
- unsupported future `schema_version` values,
- unredacted high-confidence secret markers unless explicitly allowed.

Use `harn session import` to materialize a local run record:

```bash
harn session import run.bundle.json --out imported-run.json
```

Import prefers the embedded `replay.run_record` because that preserves
the complete run record. If a bundle has no embedded run record but does
carry a replay fixture and transcript sections, import reconstructs a
minimal replayable run record from those canonical sections.

## Fixtures

Contract fixtures live under `spec/session-bundles/fixtures/`:

- `sample-run-record.json` is the source run record.
- `sample-local.bundle.json` preserves local-only fields.
- `sample-sanitized.bundle.json` demonstrates the redaction manifest.
- `sample-replay-only.bundle.json` demonstrates prompt/tool payload
  withholding.

The runtime unit tests validate and import these checked fixtures, while
the negative tests cover missing required fields, future schema versions,
and unsafe secret markers.

## Migration

Before session bundles, the closest export story was "attach the raw
run record plus whatever sidecars seemed useful." That leaked trust
boundary decisions into every caller and made replay handoffs brittle.
New callers should export a session bundle instead of sharing raw
`.harn-runs/*` directories. Existing run records remain the source of
truth; migration is a direct cutover at the boundary where a run leaves
the local machine.
