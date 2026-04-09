# Host boundary

Harn is the orchestration layer. Hosts supply facts and platform effects.

The boundary should stay narrow:

- Hosts expose typed capabilities such as project scan data, editor state,
  diagnostics, git facts, approval decisions, and persistence hooks.
- Harn owns orchestration policy: workflow topology, retries, verification,
  transcript lifecycle, context assembly, contract enforcement, replay, evals,
  and worker semantics.

What belongs in Harn `std/*` modules or the VM:

- Generic runtime wrappers like `runtime_task()`, `process_exec()`, or
  `interaction_ask()`
- Reusable metadata/scanner helpers and product-agnostic project-state normalization
- Transcript schemas, assets, compaction, and replay semantics
- Context/artifact assembly rules that are product-agnostic
- Structured contract enforcement and eval/replay helpers
- Test-time typed host mocks such as `host_mock(...)` when the behavior is a
  runtime fixture for host-backed flows rather than a product-specific bridge
- Mutation-session identity and audit provenance for write-capable workflows
  and delegated workers

What should stay in host-side `.harn` scripts:

- Product-specific prompts and instruction tone
- IDE-specific flows such as edit application, approval UX, repo enrichment,
  or bespoke tool choreography
- Host-owned filesystem and edit wrappers built on capability-aware `host_call(...)`
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
