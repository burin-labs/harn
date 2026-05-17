# Portable workflow bundles

A workflow bundle is Harn's local-first artifact for durable engineering
automations. It is designed to run on a trusted laptop under Burin/Harn and to
remain importable into Harn Cloud later without changing the workflow's durable
identity, graph, policy, or replay metadata.

The canonical package format is `.harnpack`: a deterministic `tar.zst` archive
with `harnpack.json` at the archive root. The manifest can also be read as
plain JSON during authoring. The current schema version is `2`.

For an end-to-end walkthrough that authors a bundle, validates it,
previews the graph, and runs a deterministic local receipt — all
without paid credentials — see the
[workflow authoring quickstart](./workflow-authoring-quickstart.md).

```bash
harn workflow validate --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --json
harn workflow preview --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --json
harn workflow preview --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --mermaid
harn workflow run --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json --json
```

## Contract

Top-level bundle fields:

| Field | Purpose |
|---|---|
| `schema_version` | Bundle schema version. Must be `2`. |
| `entrypoint` | Relative path to the entry Harn module inside the package. |
| `transitive_modules` | Sorted module manifest entries with source and bytecode BLAKE3 hashes. |
| `stdlib_version` / `harn_version` | Runtime and standard library versions used to build the package. |
| `provider_catalog_hash` | BLAKE3 hash of the provider catalog used at build time. |
| `tool_manifest` | Tool names, providers, optional annotations, and schema hashes captured for review. |
| `sbom` | SBOM document for package dependencies. |
| `signature` | Optional Ed25519 signature slot; signing is filled by the signing workflow. |
| `parent_trust_record_id` | Optional link into a parent OpenTrustGraph chain. |
| `id` / `version` | Stable bundle identity for hosts and importers. |
| `triggers` | Declarations for GitHub, cron, delay, manual, webhook, or MCP wakeups. |
| `workflow` | A normalized Harn `WorkflowGraph` with stable node ids. |
| `prompt_capsules` | Self-contained continuation prompts keyed by capsule id. |
| `policy` | Autonomy tier, tool policy, approval, retry, and catchup behavior. |
| `connectors` | Provider ids, scopes, and setup/status requirements. |
| `environment` | Repo setup profile, worktree policy, and command gates. |
| `receipts` | Replay metadata such as run id, event ids, workflow version, and graph digest. |

Trigger kinds:

| Kind | Required fields |
|---|---|
| `github` | `provider: "github"` and one or more `events`. |
| `cron` | `schedule`. |
| `delay` | `delay`. |
| `manual` | No additional fields. |
| `webhook` | `webhook_path`. |
| `mcp` | `mcp_tool`. |

`harn workflow validate` checks schema version, manifest metadata, relative
package paths, BLAKE3 hash syntax, stable workflow/node ids, workflow graph
validity, trigger references, prompt capsule references, policy values,
connector identity, and environment worktree policy. The validation report
includes a stable `graph_digest` over canonical graph JSON so receipts and
replay runs can pin the exact workflow graph.

Bundle identity for `.harnpack` archives is BLAKE3 over the canonical manifest
bytes plus the sorted content hashes. Re-packing the same manifest and content
produces the same bundle hash.

## Preview

`harn workflow preview --json` emits the contract Burin GUI/TUI surfaces need
before committing autonomous resources:

- bundle and workflow identity
- graph digest
- validation diagnostics
- trigger declarations
- connector requirements
- environment requirements
- normalized `graph.nodes` for triggers, actions, agents/subagents, waits,
  approvals, connector calls, notifications, catchup/DLQ branches, and terminal
  states
- normalized `graph.edges` connecting connector bindings, trigger dispatch,
  workflow control flow, catchup, DLQ, and terminal outcomes
- node-scoped `graph.diagnostics` so hosts can annotate the exact workflow node
  instead of showing opaque bundle errors
- `graph.editable_fields` JSON pointers for trigger config, prompt capsules,
  model/tool/approval/retry policy, catchup policy, and connector binding
  surfaces
- `graph.mermaid` plus the top-level `mermaid` string for a low-cost debug
  rendering

JSON is the product contract. `--mermaid` prints only the Mermaid view for
quick debugging and docs snippets; hosts should use `--json` for editing and
validation.

## Local run receipts

`harn workflow run --bundle <path> --json` materializes a deterministic local
receipt for the current bundle. The MVP runner walks the reachable graph from
the entry node, records completed node receipts, attaches trigger/event ids, and
emits the connector, policy, environment, workflow version, and graph digest
needed for replay.

The command does not call cloud services or run mutating tools by itself. Hosts
remain responsible for approval UX, concrete file mutations, and notifications;
Harn owns the portable contract, graph digest, deterministic receipt shape, and
replay metadata.

Use `--event-id` and `--trigger-id` to pin a replayed event:

```bash
harn workflow run \
  --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json \
  --trigger-id github-pr-updated \
  --event-id github:event:42 \
  --json
```

The same bundle and replayed event produce the same receipt bytes, which lets
Burin compare local executions and later cloud imports against one stable
artifact.

## Authoring skill pack

The `examples/skill-packs/workflow-authoring/` directory ships a Harn skill
pack that teaches an agent (including 4–8B local models such as qwen, gemma,
or llama.cpp) how to author bundles that pass `harn workflow validate`. It
contains:

- `SKILL.md` — a Claude Code / Agent Skills compatible card the model loads
  before responding.
- `prompting.md` — explicit XML output envelope (`<bundle>`, `<rationale>`,
  `<verify>`), a hard-rule checklist that mirrors the validator, and a
  validation-and-retry loop.
- `recipes/{pr-monitor,pr-repair}/bundle.json` — validated golden bundles for
  the two steel-thread workflows from the parent epic.
- `cases/*.case.json` — eval cases pinning each prompt to its golden bundle
  and a list of structural assertions (entry node id, required trigger kinds,
  required approval nodes, etc.).
- `eval.harn` — a Harn driver that feeds a case to any provider/model,
  extracts the `<bundle>` block, runs the validate → preview → run pipeline,
  and emits a JSON report.

Run the offline eval (no network — replays the golden):

```bash
harn run examples/skill-packs/workflow-authoring/eval.harn -- \
  --case examples/skill-packs/workflow-authoring/cases/pr-monitor.case.json
```

Run a live eval against any provider / model (point `HARN_BIN` at the binary
under test if it is not on `PATH`):

```bash
harn run examples/skill-packs/workflow-authoring/eval.harn -- \
  --case examples/skill-packs/workflow-authoring/cases/pr-monitor.case.json \
  --provider ollama --model qwen3:4b
```

`crates/harn-cli/tests/workflow_authoring_eval.rs` is the CI regression gate.
It validates every recipe golden and every case's structural assertions, so a
new case automatically extends the gate.

## Workflow patch proposals

Once a bundle exists, agents can propose **bounded, auditable edits** with a
workflow patch instead of regenerating the whole bundle. A patch is a flat
list of operations Harn applies to a copy of the bundle, then re-runs the
validator and computes a structural diff plus a capability-ceiling delta.
The patch contract is intentionally small — each op maps directly onto an
"insert a verifier here" or "narrow this node's tool policy" intent.

| `op` | What it does |
|---|---|
| `insert_node` | Inserts a workflow node (`agent`, `action`, `approval`, `notification`, …). |
| `add_edge` | Adds an edge between two existing nodes. |
| `upsert_prompt_capsule` | Inserts or replaces a prompt capsule for a node. |
| `update_node_policy` | Patches `task_label` / `prompt` / `system` / `tools` / `model_policy` / `capability_policy` / `approval_policy` on an existing node. |
| `update_bundle_policy` | Patches `autonomy_tier` / `tool_policy` / `approval_required` / `retry` / `catchup` at the bundle level. |

```bash
harn workflow patch validate \
  --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json \
  --patch  docs/fixtures/workflow-bundles/pr-monitor-verifier.patch.json \
  --parent-ceiling docs/fixtures/workflow-bundles/parent-act-with-approval.policy.json \
  --json

harn workflow patch apply --bundle ... --patch ... --out ...
harn workflow patch preview --bundle ... --patch ... --mermaid
```

Failure modes the validator enforces:

- Empty `operations` list (patches must do something — silent no-ops are
  rejected).
- Duplicate `insert_node` ids; unknown endpoints in `add_edge`; duplicate
  edges; collisions on `upsert_prompt_capsule.node_id`.
- Any patch that **widens** the parent ceiling along *tools*,
  *capabilities*, *side-effect level*, *workspace roots*, *connector scopes*,
  *command gates*, or *autonomy tier*. Each violation lands in the report's
  `capability_delta.widening` array with a stable `kind` discriminator.

### Safe Harn function tools

`harn workflow function-tools --json` enumerates the allowlisted Harn
functions an agent may call from inside the patch-authoring loop. Each
descriptor carries an ACP-aligned `ToolAnnotations` block (kind +
side-effect level + capability requirements) so a host can wire the tool
straight into a model surface. The current allowlist is read-only or
pure-think only:

- `workflow_bundle_validate` / `workflow_bundle_preview` /
  `workflow_bundle_capability_ceiling` — inspect a bundle on disk.
- `workflow_patch_validate` — apply + validate a patch in memory and return
  the report.

Adding a function to the allowlist is a deliberate, reviewed change in
`crates/harn-vm/src/orchestration/safe_function_tools.rs`. Anything outside
the list is not exposed to agents.

### Nested invocation ceiling

When a script or host launches another Harn invocation (`harn run`,
`harn workflow run`, `harn supervisor fire/replay`, a Burin harness),
Harn projects the target's requested ceiling and rejects launches that
would widen the parent's. `harn workflow nested-ceiling --bundle <path>
--parent <policy>` exposes the same scanner so hosts can sanity-check
before launch:

```bash
harn workflow nested-ceiling \
  --bundle docs/fixtures/workflow-bundles/github-pr-monitor.bundle.json \
  --parent docs/fixtures/workflow-bundles/parent-act-with-approval.policy.json
```

The scanner also accepts a Harn script source (token-level capability
projection) and a Burin harness manifest (explicit `capability_ceiling`
block, falling back to "request everything" if the manifest is silent —
silence is treated as the most invasive request, so the parent rejects
rather than rubber-stamps).
