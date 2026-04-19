# Harn portal

`harn portal` launches a local observability UI for persisted Harn runs.

The portal frontend is now a Vite-built React application embedded into
`harn-cli` as static assets. Running `harn portal` does not require Node once
those built assets are present in the repository, but editing the portal UI
does.

The portal treats `.harn-runs/` as the source of truth and gives you one place
to inspect:

- run history
- the derived action-graph / planner observability artifact
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

- serves from `http://127.0.0.1:4721`
- watches `.harn-runs`
- opens a browser automatically

For a fresh source checkout, the simplest local setup is:

```bash
./scripts/dev_setup.sh
make portal
```

For portal frontend work specifically:

```bash
npm --prefix crates/harn-cli/portal install
npm run portal:build
npm run portal:test
```

Useful flags:

```bash
harn portal --dir runs/archive
harn portal --host 0.0.0.0 --port 4900
harn portal --open false
```

For frontend development with Vite, `npm run portal:dev` starts:

- the Rust portal server on `http://127.0.0.1:4721`
- the Vite UI on `http://127.0.0.1:4723` with `/api` proxied to the Rust server

## Quick demo

To generate a purpose-built demo dataset and launch the portal against it:

```bash
make portal-demo
```

That script creates `.harn-runs/portal-demo/` with:

- a successful workflow-graph run
- a deterministic replay of that run
- a failed verification run with failure context in the run list

If you only want the data without launching the server:

```bash
./scripts/portal_demo.sh --generate-only
cargo run --bin harn -- portal --dir .harn-runs/portal-demo --open false
```

If you want to regenerate that dataset from scratch, pass `--refresh`.

## How to read it

The UI is organized around a few simple ideas:

- `Launch` is a dedicated workspace for playground runs and script execution
- `Runs` is a dedicated paginated library for persisted run records
- `Run detail` is a separate inspector page for one run at a time
- the top of the detail view is the quick read
- the action-graph panel is the "debug this run from one artifact" view:
  planner rounds, research facts, worker lineage, verification outcomes, and
  transcript pointers all come from the same derived block in the saved run
- the policy panel shows the effective run ceiling plus saved validation output
- the replay panel shows whether a run already carries replay/eval assertions
- the flamegraph shows where time went
- the activity feed shows what the runtime actually did
- the transcript story shows the human-visible text that was preserved
- the stage detail drawers expose persisted per-stage policy, contracts, worker,
  prompt, and rendered-context metadata

The portal is intentionally generic. It does not assume a particular editor,
client, or host integration. If Harn persisted the run, the portal can inspect
it.

## Live updates

The portal polls conservatively instead of hammering the run directory:

- the runs index refreshes on a slower cadence
- the selected run detail refreshes faster only while that run is still active
- hidden browser tabs do not poll

The portal also supports:

- deep-linking to a selected run via the URL
- manual refresh without waiting for the poll interval
- comparing a run against any other run of the same workflow, not just the
  latest earlier one
- surfacing action-graph, worker-lineage, transcript-pointer, and tool-result
  diffs alongside stage-level drift

## Launch and playground

The portal can also launch Harn directly through a small control panel at the
top of the page.

It supports three modes:

- existing `.harn` files from `examples/` and `conformance/tests/`
- inline Harn source through the script editor
- a lightweight playground that turns a task plus provider/model selection into
  a real persisted workflow run

For local model servers, the launch UI also exposes the provider's endpoint
override env when one exists, so you can point `local` or similar providers at
another localhost or LAN address without editing config files first.

The portal now shows both roots explicitly in the launch panel:

- `Workspace root`: the directory where `harn portal` was started, and the
  current working directory for launches
- `Run artifacts`: the watched run directory passed via `--dir`

Inline and playground launches create a concrete per-job workspace under the
watched run directory:

```text
.harn-runs/playground/<job-id>/
  workflow.harn
  task.txt
  launch.json
  run.json
  run-llm/llm_transcript.jsonl
```

That keeps the portal useful even before building a larger hosted playground:
you get an inspectable source file, launch metadata, and a real run record that
the debugger can reopen later.

Security and privacy constraints:

- env overrides are passed only to the child `harn run` process
- env overrides are validated as uppercase shell-style keys
- env values are not persisted in portal job state or run metadata
- launch file paths must stay inside the current workspace
- run inspection paths must stay inside the configured run directory

The transcript sidecar is only populated for runtime paths that currently emit
`HARN_LLM_TRANSCRIPT_DIR` output. Agent-loop traffic supports this today;
generic workflow-stage model calls may still only appear in the persisted run
record itself.

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

## Skill observability

Each run detail page renders three skill-focused panels above the
replay/eval section:

- **Skill timeline** — horizontal bars showing which skills activated
  on which agent-loop iteration and when they deactivated. Hover a
  bar for the matcher score and the reason the skill was promoted.
- **Tool-load waterfall** — one row per `tool_search_query` event,
  pairing each query with the `tool_search_result` that followed so
  you can see which deferred tools entered the LLM's context in each
  turn.
- **Matcher decisions** — per-iteration expansions showing every
  candidate the matcher considered, its score, and the working-file
  snapshot it scored against.

The runs index also accepts a `skill=<name>` query parameter (and
exposes it as a filter input on the runs page), so you can narrow
evals to runs where a specific skill was active — useful when
validating that a new skill attracts the right prompts.
