# Harn Portal

`harn portal` launches a local observability UI for persisted Harn runs.

The portal treats `.harn-runs/` as the source of truth and gives you one place
to inspect:

- run history
- workflow stages
- nested trace spans
- transcript/story sections
- delegated child runs
- token/call usage

## Start the portal

```bash
harn portal
make portal
```

By default the portal:

- serves from `http://127.0.0.1:4621`
- watches `.harn-runs`
- opens a browser automatically

For a fresh source checkout, the simplest local setup is:

```bash
./scripts/dev_setup.sh
make portal
```

Useful flags:

```bash
harn portal --dir runs/archive
harn portal --host 0.0.0.0 --port 4900
harn portal --open false
```

## How to read it

The UI is organized around a few simple ideas:

- the left side is the run library
- the top of the detail view is the quick read
- the flamegraph shows where time went
- the activity feed shows what the runtime actually did
- the transcript story shows the human-visible text that was preserved

The portal is intentionally generic. It does not assume a particular editor,
client, or host integration. If Harn persisted the run, the portal can inspect
it.

## Live updates

The first version polls the run directory and refreshes the selected run
automatically. That makes it useful for runs that are still being written or
updated on disk.

## Saved model-turn detail

If a run has a sibling transcript sidecar directory named like:

```text
.harn-runs/<run-id>.json
.harn-runs/<run-id>-llm/llm_transcript.jsonl
```

the portal will automatically render step-by-step model turns, including:

- kept vs newly added context
- saved request messages
- reply text
- tool calls
- token counts
- span ids

For richer live observability, Harn already exposes ACP `session/update`
notifications with:

- `call_start`
- `call_progress`
- `call_end`
- `worker_update`

Those can power a future streaming view without inventing a second provenance
system alongside run records.
