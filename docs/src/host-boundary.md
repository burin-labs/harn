# Host Boundary

Harn is the orchestration layer. Hosts supply facts and platform effects.

The boundary should stay narrow:

- Hosts expose typed capabilities such as project scan data, editor state,
  diagnostics, git facts, approval decisions, and persistence hooks.
- Harn owns orchestration policy: workflow topology, retries, verification,
  transcript lifecycle, context assembly, contract enforcement, replay, evals,
  and worker semantics.

What belongs in Harn `std/*` modules or the VM:

- Generic host wrappers like `project_host_scan()`, `git_diff()`,
  `workspace_read_text()`, or `process_exec()`
- Reusable project-state normalization and packaging
- Transcript schemas, assets, compaction, and replay semantics
- Context/artifact assembly rules that are product-agnostic
- Structured contract enforcement and eval/replay helpers
- Multi-root workspace contracts and fallbacks such as `workspace_roots()`
- Test-time typed host mocks such as `host_mock(...)` when the behavior is a
  runtime fixture for host-backed flows rather than a product-specific bridge
- Mutation-session identity and audit provenance for write-capable workflows
  and delegated workers

What should stay in host-side `.harn` scripts:

- Product-specific prompts and instruction tone
- IDE-specific flows such as edit application, approval UX, or bespoke tool
  choreography
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
