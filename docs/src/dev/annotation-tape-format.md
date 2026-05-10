# Annotation tape format

An annotation file (`<tape>.annotations.jsonl`) is the durable form of
human judgment over a recorded testbench run. It pairs with a
[unified event tape](tape-format.md) and lets humans (or agents
proxying for humans) attach structured `correct` / `incorrect` /
`hypothesis` / `friction` / `crystallize_here` records to specific
events. The same JSONL feeds the eval rubric, friction roll-up, and
crystallization candidate detector — no second labeling pipeline.

This is the substrate behind issue
[#1474](https://github.com/burin-labs/harn/issues/1474). The full
runtime lives at [`harn_vm::testbench::annotations`][module].

[module]: https://docs.rs/harn-vm/latest/harn_vm/testbench/annotations/index.html

## File layout

```text
run.tape                    # the unified event tape
run.tape.annotations.jsonl  # annotation sidecar (this format)
run.tape.cas/               # tape's own content-addressed sidecar
```

The annotations file is line-delimited JSON. Empty lines and lines
starting with `#` are tolerated so authoring tools can group records
visually.

The first line is always a header:

```json
{
  "type": "header",
  "schema_version": 1,
  "tape_path": "run.tape",
  "tape_content_hash": "<blake3>",
  "harn_version": "0.8.6"
}
```

`tape_content_hash` is optional but recommended — when present, the
validator catches tape edits that invalidate `event_id` references.

Subsequent lines are annotations:

```json
{
  "type": "annotation",
  "id": "ann_001",
  "event_id": 42,
  "kind": "hypothesis",
  "evidence": "checkout incident — see runbook",
  "author": {"id": "alice", "kind": "human", "surface": "burin-code"},
  "timestamp": "2026-05-10T17:00:00Z",
  "hypothesis_status": "active",
  "span": {"start_event_id": 42, "end_event_id": 50},
  "links": [{"label": "runbook", "url": "..."}],
  "metadata": {}
}
```

## Annotation kinds

| Kind | Required extras | Consumer |
|---|---|---|
| `correct` | — | persona eval rubric ground truth |
| `incorrect` | — | persona eval rubric ground truth |
| `alternative` | `suggested_fix` (recommended) | replay-for-teaching, presenter mode |
| `note` | — | free-text commentary |
| `marker` | `span` (often) | replay-for-teaching anchor |
| `mute` | — | suppress a flake on dashboards |
| `hypothesis` | `hypothesis_status` (`active` \| `verifying` \| `confirmed` \| `disproven` \| `stale`) | human-prior-to-verify loop ([harn-cloud#54](https://github.com/burin-labs/harn-cloud/issues/54)) |
| `friction` | `friction_kind` (must match the [friction taxonomy](#friction-kind-taxonomy)) | friction roll-up ([harn#452](https://github.com/burin-labs/harn/issues/452)), context-pack candidates |
| `crystallize_here` | `span` (recommended) | crystallization candidate detector ([harn#451](https://github.com/burin-labs/harn/issues/451)) |

Records carry a stable `id` (unique within the file), an `event_id`
matching a [`TapeRecord::seq`][record], an optional `span`, an
`author`, and an `evidence` string. New kinds can be added without
breaking older readers — unknown kinds deserialize as
`AnnotationKind::Unknown` so a validator can still report on the rest
and the file loader does not refuse to open it.

[record]: https://docs.rs/harn-vm/latest/harn_vm/testbench/tape/struct.TapeRecord.html

### Friction-kind taxonomy

`friction_kind` values must match the existing friction-event taxonomy
in [`harn_vm::orchestration::friction`][friction] so a bag of friction
annotations and a bag of natively-emitted `FrictionEvent`s are
interchangeable for downstream consumers:

- `repeated_query`
- `repeated_clarification`
- `approval_stall`
- `missing_context`
- `manual_handoff`
- `tool_gap`
- `failed_assumption`
- `expensive_model_used_for_deterministic_step`
- `human_hypothesis`

[friction]: https://docs.rs/harn-vm/latest/harn_vm/orchestration/friction/index.html

## CLI surface

### Surface annotations during replay

```sh
harn test-bench replay script.harn \
    --process-tape run.process.json \
    --annotations run.tape.annotations.jsonl
```

The runner loads + pre-validates the annotations sidecar, then prints
each annotation grouped by its referenced event in the run-summary
block. The exit code is `2` (CI-gateable) if validation surfaces any
problems.

### Validate a sidecar against its tape

```sh
harn test-bench validate-annotations \
    --tape run.tape \
    --report validation.json \
    run.tape.annotations.jsonl
```

The report enumerates `unknown_event_id`, `hypothesis_status_missing`,
`friction_kind_unknown`, `invalid_span`, `duplicate_id`, and
`tape_digest_mismatch` problems with stable `code` tags so CI can gate
on specific failure classes. The command exits non-zero (status `2`)
when problems exist.

### Export annotations by kind

```sh
# Re-emit hypothesis annotations as JSONL for the human-prior pipeline.
harn test-bench export-annotations run.tape.annotations.jsonl \
    --kind hypothesis --format jsonl

# Re-emit friction annotations as `FrictionEvent`s ready to feed
# `orchestration::generate_context_pack_suggestions`.
harn test-bench export-annotations run.tape.annotations.jsonl \
    --kind friction --format friction
```

`--kind` is repeatable and the union of matching annotations is
written. `--format friction` only emits records that successfully
adapt to a `FrictionEvent` (i.e. `kind == friction` with a recognised
`friction_kind`). `--format jsonl` (the default) emits the raw
annotation rows so the file is bundle-compatible with the v0.8.4
portable workflow bundle pattern.

## Round-trip + bundling

Annotations survive serialize/deserialize byte-identically — the
`AnnotationLine` enum is the only producer of on-disk shapes, and
optional fields default to `None` so missing-field round-trips are
stable. Bundling a tape with its annotations is two files plus the
optional CAS sidecar:

```text
run.tape
run.tape.cas/<blake3>...
run.tape.annotations.jsonl
```

The conformance suite exercises this round-trip via
`conformance/tests/testbench/testbench_replay_fidelity.annotations.jsonl`,
which the runner validates against the emitted tape after the fidelity
check.

## Versioning contract

The header's `schema_version` integer gates compatibility:

- Loaders accept files with `schema_version <= ANNOTATION_SCHEMA_VERSION`
  and refuse newer files with a structured error.
- Adding a kind is non-breaking: older readers see it as
  `AnnotationKind::Unknown` and the validator emits a single
  `unknown_kind` problem per affected record.
- Renaming or repurposing a field is breaking and requires a version
  bump.

When you bump the version, add a "Changes from v\<previous\>" section
below.

## Cross-references

- [Tape format](tape-format.md) — the artifact this sidecar references.
- [Testbench mode](testbench.md) — how recording is activated.
- Issue [#1474](https://github.com/burin-labs/harn/issues/1474) —
  design rationale and acceptance criteria.
- Issue [#451](https://github.com/burin-labs/harn/issues/451) /
  [#452](https://github.com/burin-labs/harn/issues/452) — downstream
  consumers (crystallization, friction events).
- [burin-code#599](https://github.com/burin-labs/burin-code/issues/599)
  — the IDE authoring surface that produces these files.
