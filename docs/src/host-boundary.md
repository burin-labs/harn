# Host boundary

Harn is the orchestration layer. Hosts supply facts and platform effects.

The boundary should stay narrow:

- Hosts expose typed capabilities such as project scan data, editor state,
  diagnostics, git facts, approval decisions, and persistence hooks.
- Harn owns orchestration policy: workflow topology, retries, verification,
  transcript lifecycle, context assembly, contract enforcement, replay, evals,
  and worker semantics.

What belongs in Harn `std/*` modules or the VM:

- Generic runtime wrappers like `runtime_task()`, `process_run()`, or
  `interaction_ask()`
- Reusable metadata/scanner helpers and product-agnostic project-state normalization
- Transcript schemas, assets, compaction, and replay semantics
- Context/artifact assembly rules that are product-agnostic
- Structured contract enforcement and eval/replay helpers
- Test-time capability fixtures owned by `HarnessTesting` when behavior is a
  runtime fixture for host-backed flows rather than a product-specific bridge
- Mutation-session identity and audit provenance for write-capable workflows
  and delegated workers

What should stay in host-side `.harn` scripts:

- Product-specific prompts and instruction tone
- IDE-specific flows such as edit application, approval UX, repo enrichment,
  or bespoke tool choreography
- Host-owned filesystem and edit adapters exposed as typed harness capabilities
- Host-owned editor, diagnostics, git, learning, and project-context wrappers
- Concrete undo/redo stacks and editor-native mutation application
- Proprietary ranking, routing, or heuristics tied to one host product
- Features that depend on host-only commercial, account, or app lifecycle rules

Rule of thumb:

- If a behavior decides how an agent or workflow should think, continue,
  verify, compact, replay, or select context, it probably belongs in Harn.
- If a behavior fetches facts from a specific editor or app surface, asks the
  user for approval, or performs a host-only side effect, it belongs in the
  host.

Keep advanced host-side `.harn` modules local to the host when they encode
host-only UX, proprietary behavior, or app-specific heuristics. Move a helper
into Harn only when it is general enough to be useful across hosts.

## Trust boundary

Harn should own the audit contract for mutations:

- mutation-session IDs
- workflow/worker/session lineage
- tool-gate mutation classification and declared scope
- artifact and run-record provenance

Hosts should own the concrete UX:

- apply/approve/deny flows
- patch previews
- editor undo/redo semantics
- trust UI around which worker or session produced a change

## Typed host capabilities

Typed capabilities surfaced through nominal handles such as `HarnessFs`,
`HarnessProcess`, and `HarnessInteraction` let Harn delegate platform effects
without coupling scripts to one host. Ordinary scripts never call
`host_call(...)`; the capability registry projects each operation onto the
appropriate `harness.*` handle. Notable capability families include:

- `process.exec` and the `process.*` shell helpers — process execution.
- `template.render` — host-resolved template rendering.
- `interaction.ask` — synchronous user prompts.
- `memory.embed` — host-provided text embeddings used by the `std/memory`
  vector and hybrid backends. Request shape: `{text, model_hint}`. Response
  shape: `{vector: list<float>, model?: string, dim?: int}`. Hosts pick the
  model and own rate limiting and cost accounting; Harn caches embeddings
  per `(model_hint, content_hash)` so replays are deterministic. See
  [Memory](memory.md) for the recall and storage contract.

Tests satisfy registered capabilities through the `HarnessTesting` handle:
`harness.testing.respond(...)`, `respond_error(...)`, and `calls()`. Unknown
capability methods fail closed, so typos cannot create a mock-only interface.
Embedders ship richer behavior via the async `HostCallBridge` trait described in
`crates/harn-vm/src/stdlib/host/bridge.rs`. ACP installs that bridge and keeps the
privileged wire dispatch, fixture routing, command-policy preflight, the
process-handle registry, and the per-turn memo on one internal path. Only
stamped privileged modules may name `host_call`; provenance is transitive and
cannot be re-exported as a closure to ordinary code. Script-facing process
execution remains `harness.process.*` in every embedder.

## Process sandbox

`harn run` installs a default `worktree` capability ceiling before
executing user code. The runtime confines every subprocess it spawns
under that ceiling unless the operator passes `--no-sandbox`. The
default profile is workspace-root path enforcement plus best-effort
OS-level confinement (Linux Landlock + default-deny seccomp allowlist,
macOS sandbox-exec, Windows AppContainer + Job Object), with network
side effects denied by the default ceiling. Pipelines that spawn
untrusted code opt into `os_hardened`, which makes the OS confinement
*required* and turns every spawn into a `tool_rejected` if the platform
mechanism is missing.

See [Process sandboxing](./sandboxing.md) for the full per-platform
capability → kernel-knob mapping table, profile selection examples,
and replay semantics.

## Contract surfaces

Harn now ships machine-readable contract exports so hosts do not need to
reverse-engineer runtime assumptions:

- `harn contracts builtins` for the builtin registry and parser/runtime drift
- `harn contracts host-capabilities` for the effective host manifest used by
  preflight validation
- `harn contracts bundle` for entry modules, imported modules, prompt/template
  assets, explicit module-dependency edges, required host capabilities, literal
  execution directories, worker repo dependencies, and stable summary counts

Those surfaces are intended to be the generic boundary for embedded hosts such
as editors or native apps. Product-specific packaging logic should build on top
of them rather than re-implementing Harn’s import, asset, and host-capability
resolution rules independently.
