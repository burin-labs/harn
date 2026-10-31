# Event tape format

The event tape is the canonical artifact behind `harn test-bench
--emit-tape`. Every non-deterministic input a script consumed during a
run — clock reads, sleeps, LLM responses, FS reads/writes, subprocess
spawns — lands as a typed [`TapeRecord`][record] with a logical
sequence number and a virtual-time stamp. Tapes are diffable, content
addressed, and versioned so they survive runtime upgrades.

[record]: https://docs.rs/harn-vm/latest/harn_vm/testbench/tape/struct.TapeRecord.html

This document is the source of truth for the on-disk schema. Bumping
[`TAPE_FORMAT_VERSION`][version] requires an entry here.

[version]: https://docs.rs/harn-vm/latest/harn_vm/testbench/tape/constant.TAPE_FORMAT_VERSION.html

## File layout

```text
run.tape           # NDJSON: one header line + one record line per event
run.tape.cas/      # content-addressed sidecar (BLAKE3 hex names)
```

`run.tape` is line-delimited JSON. Each line is one of two shapes:

- **Header** (always the first line):

  ```json
  {
    "type": "header",
    "version": 5,
    "harn_version": "0.10.182",
    "started_at_unix_ms": 1700000000000,
    "script_path": "examples/cron-rollup.harn",
    "argv": ["--mode=daily"]
  }
  ```

- **Record** (zero or more, after the header):

  ```json
  {
    "type": "record",
    "seq": 0,
    "phase": "user_script",
    "virtual_time_ms": 1700000000000,
    "monotonic_ms": 0,
    "kind": {
      "kind": "clock_sleep",
      "duration_ms": 250
    }
  }
  ```

`run.tape.cas/` holds payload bytes that are too large to inline (see
[CAS thresholds](#content-addressed-storage)). The directory is
optional — tapes that never spilled a payload have no sidecar.

## Record kinds

The current schema (`version = 5`) emits the kinds below. Unknown
record kinds in newer tapes deserialize as `unknown` so older
fidelity checkers still produce a structured report.

| Kind | Payload fields | Source |
|---|---|---|
| `clock_read` | `source` (`"wall"` or `"monotonic"`), `value_ms` | `harness.clock.now_ms()` / `harness.clock.monotonic_ms()` methods |
| `clock_sleep` | `duration_ms` | `harness.clock.sleep_ms(...)` / `harness.testing.clock_advance(...)` |
| `llm_call` | `request_digest`, `response`, optional `call_index`, `request`, `messages`, `attribution` | LLM provider interception |
| `cache_lookup` | `namespace`, `key`, `after_calls`, optional `value` | Persistent model-cache lookup |
| `prompt_render` | `render_id`, `template_uri`, `rendered`, `spans` | Prompt rendering and source attribution |
| `tool_call` | `call_id`, `tool`, `args_digest`, `after_calls`, `outcome`, `effects`, `transcript_changes`, optional `unservable` | Recorded tool dispatch |
| `decision_evaluation` | `request_digest`, `response` | Typed evaluation request and receipt |
| `payload_redaction` | none | A full run payload required credential redaction |
| `file_read` | `path`, `content_hash`, `len_bytes` | `harness.fs.read_text(...)` builtins |
| `file_write` | `path`, `content_hash`, `len_bytes` | `harness.fs.write_text(...)`, `harness.fs.append(...)`, `harness.fs.append_locked(...)` |
| `file_delete` | `path` | `remove_file(...)` |
| `process_spawn` | `program`, `args`, `cwd`, `exit_code`, `duration_ms`, `stdout_payload`, `stderr_payload` | Sandboxed subprocess invocation |
| `mcp_json_rpc` | `server`, `method`, digests, latency, redacted request/response payloads | MCP client exchanges |
| `model_job` | `job_id`, `request_id`, `backend`, `state`, `event_kind`, event payload, `asset_digests` | Asynchronous model-job lifecycle events and output-asset lineage |

Every record carries the wrapping fields:

- `seq` — monotonic logical sequence number (assigned at record time).
- `phase` — `user_script` for records produced by the script body, or
  `runtime_finalize` for records produced while the runtime drains
  finish/resume/finalizer lifecycle work.
- `virtual_time_ms` — UNIX-epoch ms observed via the unified mock clock.
- `monotonic_ms` — ms since the testbench session activated.

Two tapes that differ only in real-time stamps (e.g. CI machines with
different NTP skew) diff cleanly on the logical structure.

## Content-addressed storage

Records whose serialized payload exceeds **4 KiB** (the
[`MAX_INLINE_BYTES`][threshold] threshold) spill to the sidecar. The
inline JSON carries:

```json
{ "content_hash": "<blake3-hex>", "len_bytes": 12345 }
```

…and the bytes themselves live at `run.tape.cas/<blake3-hex>`. Reusing
the same payload across records — e.g. an idempotent LLM response
served to two callers — stores it once.

Smaller payloads stay inline:

```json
{ "content_hash": "<blake3-hex>", "text": "...stdout..." }
```

Run tapes apply the runtime's credential redaction policy before persisting
payloads. A tape with a `payload_redaction` record remains readable but cannot
serve an exact branch prefix. Secret-bearing file effects are omitted rather
than replayed with altered contents. Branch workspaces must be existing copies;
replayed writes and deletions use the sandbox's root-relative mutation path.

`content_hash` is a hex BLAKE3 digest of the persisted bytes. The fidelity
oracle compares hashes only; it never re-hashes payloads at compare
time.

MCP client calls appear as `mcp_json_rpc` records. Each record carries
the server name, method, request/response digests, latency, and redacted
request/response payloads. This lets a single unified tape catch MCP
schema or behavior drift alongside LLM, subprocess, filesystem, and
clock drift.

[threshold]: https://docs.rs/harn-vm/latest/harn_vm/testbench/tape/constant.MAX_INLINE_BYTES.html

## Versioning contract

The header's `version` integer gates compatibility:

- Loaders accept tapes with `version <= TAPE_FORMAT_VERSION` and refuse
  newer tapes with a structured error so a downgrade doesn't silently
  drop records.
- Adding a record kind is non-breaking: older fidelity checkers see
  the unknown kind as `TapeRecordKind::Unknown` and emit a divergence
  with category `unknown_kind`.
- Renaming or repurposing an existing field is breaking and requires a
  version bump.

Loading an older tape does not establish that it supports branching. Branch
admission also requires the current recording metadata and complete evidence
for the requested prefix; it refuses missing or incompatible evidence.

### Changes from v4 to v5

`tool_call.transcript_changes` includes typed `lifecycle` entries alongside
`fields` entries. An `opened` entry carries the initial versioned transcript;
a `closed` entry ends that dispatch-local lifetime. A closed child is validated
off-table during replay without reopening a live session or rerunning hooks.
Unsupported lifetimes are marked `unservable` when recorded. Replay validates
the full lifetime sequence, live target identities, write eligibility, and field
changes before applying recorded file effects. This does not provide filesystem
rollback if a later file write fails.

### Changes from v3 to v4

Each tool dispatch records ordered, typed transcript field changes through the
canonical transcript writer. `set`, `append`, and `remove` operations preserve
the changes made by hooks and other transcript writers when the tool is served.

### Changes from v2 to v3

Run recordings include their durable state location so branch execution can
redirect state into its isolated workspace.

### Changes from v1 to v2

`llm_call.request_digest` covers the whole model request rather than its former
narrower shape. A v1 digest cannot be compared with a v2 digest as evidence of
request identity.

## Fidelity oracle

`harn test-bench fidelity` compares two tapes under one of four modes:

- **`byte-identical`** (default, strictest). Every record matches by
  position, kind, content hash, and timing. The mode CI uses to gate
  "this PR did not regress replay determinism."
- **`semantic`**. Ignores diffs that are non-meaningful by construction:
  monotonic-only sequence stamps, pure virtual-time drift, and recorded
  `monotonic_ms` deltas. Content hashes still gate every payload.
- **`outcome`** (loosest). Compares only the script's externally
  observable result: the final FS write set, the exit status of the
  last subprocess, and the count of LLM calls. Useful for stochastic
  LLM runs where intermediate token streams legitimately diverge.
- **`phase-aware`**. Compares `user_script` records byte-identically and
  `runtime_finalize` records semantically. Runtime-finalize clock reads
  are ignored so internal lifecycle observability can grow without
  regenerating user-script fidelity fixtures; runtime finalization
  file/process/LLM effects still participate in the semantic diff.

The CLI emits a structured JSON report (a [`FidelityReport`][report])
listing every diverging record with a stable category tag. CI pipelines
gate on `divergences == []`; the public leaderboard ([harn-cloud#19])
ingests the score directly.

[report]: https://docs.rs/harn-vm/latest/harn_vm/testbench/fidelity/struct.FidelityReport.html
[harn-cloud#19]: https://github.com/burin-labs/harn-cloud/issues/19

### Example

Diff two recorded tapes:

```sh
harn test-bench fidelity recorded.tape replay.tape --mode byte-identical
```

Re-run a script under testbench replay and compare against the
recorded tape:

```sh
harn test-bench fidelity script.harn --against recorded.tape \
    --mode semantic --report fidelity.json
```

Both forms exit non-zero (status `2`) when the report has any
divergences. CI gates can therefore rely on the exit code without
parsing JSON.

## Producing a tape

```sh
harn test-bench run script.harn \
    --clock paused --start-at 1767225600000 \
    --emit-tape run.tape
```

The tape is a byproduct of a normal testbench run; the script executes
unmodified, and the recorder pushes a record at every host-capability
boundary it crosses. When the run finishes, the tape (and its CAS
sidecar) lands at the requested path.

## Out of scope (v1)

The first version intentionally ships a small but principled set of
record kinds. The following are tracked separately:

- HTTP request/response capture (independent of LLM calls). The
  necessary capture point is the egress allowlist hook, but plumbing
  through every connector is its own ticket.
- Tape compaction / GC. The current encoding stores every record;
  pruning can come once tapes get large in practice.
- Cross-runtime tape exchange (Inngest/Temporal interop). Lives in the
  leaderboard harness ([harn-cloud#19]), not this crate.

## See also

- [Testbench mode](testbench.md) — the composition primitive that
  installs the tape recorder.
- [Annotation tape format](annotation-tape-format.md) — sidecar that
  attaches structured human judgment to tape events.
- [Testing](testing.md) — approved deterministic test patterns.
- [Issue #1441](https://github.com/burin-labs/harn/issues/1441) —
  design rationale for the unified tape and fidelity oracle.
