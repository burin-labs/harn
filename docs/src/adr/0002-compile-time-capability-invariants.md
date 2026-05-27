# ADR 0002: compile-time capability invariants

## Status

Accepted. Narrow version shipped in [#378](https://github.com/burin-labs/harn/pull/378)
(closes [#279](https://github.com/burin-labs/harn/issues/279)).

This ADR closes the speculative research issue
[#925](https://github.com/burin-labs/harn/issues/925) by recording the
design decision behind the shipped prototype.

## Context

Harn already has a runtime [capability ceiling](../workflow-runtime.md#capability-ceilings):
host applications express tools, capabilities, workspace roots, and
side-effect levels in `CapabilityPolicy`, and validation intersects
node, workflow, and host policies before execution. That layer is real
but reactive — it cannot reject programs at `harn check` time, only
when they run.

[Strategic Positioning V1 §"Deeper moats" #5][positioning] called out
"formal methods / invariants over agent behavior" as a structural moat
that vendor platforms are unlikely to build for their customers.
Harn is a typed DSL with a single AST and a single check pass, so
some compile-time invariants over agent behavior are tractable here in
a way they are not in general-purpose hosts. Examples we want to make
compile-time errors:

- *"this agent never calls `fs::delete` outside `/workspace/scratch`"*
- *"`budget.remaining` is preserved or decreased on every code path"*
- *"no handler can perform a side effect without an approval record on
  every reachable path"*

The original epic [#285][epic] sketched a richer design — an effect
system layered on the typechecker (`requires`/`forbids` clauses) plus
linear types for `Approval<T>` so approval tokens are consumed exactly
once. That epic was deliberately closed in favor of the narrow scope
captured here.

[positioning]: https://www.notion.so/3477d224c632809c8959cb4985016cf6
[epic]: https://github.com/burin-labs/harn/issues/285

## Survey

Three design axes mattered:

1. **Where the analysis lives.** Inside the existing typechecker
   (effect rows on every callable signature) vs. in a separate IR
   crate (lower handlers into a small CFG and walk that). The first is
   more expressive but forces every existing builtin signature to grow
   effect annotations and breaks the gradual-typing story.
2. **What the syntax looks like.** Function-level `requires`/`forbids`
   keywords vs. an attribute-based annotation. Keywords intrude on
   parser/lexer/spec/tree-sitter; attributes already exist (`@deprecated`,
   `@test`, `@step`, …) and are deliberately easy to add new kinds of.
3. **How decidable the checks are.** Symbolic execution / SMT (precise
   but slow, brittle, hard to explain) vs. a flow-sensitive walk over
   a small CFG with literal/identifier data-flow summaries (decidable,
   fast, explainable, and matches the kind of evidence a reviewer
   actually wants — "the violating call is on this path").

We surveyed a handful of representative handlers: workspace-mutating
tools (`write_file`, `apply_edit`, `delete_file`), HITL-gated pipelines
that already call `request_approval` / `dual_control`, and budgeted LLM
loops that thread `llm_budget_remaining()` through their state. The
common shape is: a handful of side-effecting builtin or tool calls,
guarded by approval primitives and bounded by budget reads, with
straight-line or simple branching control flow. That shape is exactly
what a small CFG analysis can decide; it does not need a full effect
system.

## Decision

Ship the **narrow version**: a separate `harn-ir` crate that lowers
attributed handlers into a compact CFG plus literal data-flow
summaries, with three pluggable invariants and an attribute-based
opt-in.

### Surface

- **Annotation:** `@invariant("name", ...)` on `fn`, `tool`, or
  `pipeline` declarations. Multiple `@invariant` stacks compose.
- **Opt-in CLI:** `harn check --invariants` runs the analysis;
  unannotated programs and plain `harn check` are unaffected.
- **Explain path:** `harn explain --invariant <name> <handler> <file>`
  prints the concrete CFG steps that reach the violating node.
- **Editor surface:** the LSP wires invariant diagnostics into
  publishable diagnostics, so editors get red squiggles on the same
  spans `harn check --invariants` reports.

### Initial invariant catalog

| Invariant | Meaning |
|---|---|
| `@invariant("fs.writes", "src/**")` | Every reachable filesystem-writing call (`write_file`, `apply_edit`, `delete_file`, …, plus `mcp_call`/`host_tool_call` whose tool name pattern-matches a writing verb) must target a literal path proven to stay within at least one declared glob. |
| `@invariant("budget.remaining", target: "remaining")` | Assignments to the named budget variable may only preserve it, decrement it, or refresh it from `llm_budget_remaining()`. |
| `@invariant("approval.reachability")` | Every reachable side-effecting call must be preceded by `request_approval(...)` or enclosed by a `dual_control(...)` approval scope on every path from the handler entry. |
| `@invariant("capability.policy", allow: "fs.write,llm.model", ...)` | Every reachable effect must be declared in a canonical capability allow-list, and selected capabilities can require workspace globs, approval gates, budget evidence, autonomy policy, execution policy, command policy, egress policy, or tool approval policy. |

These map onto the three concrete demands from the closed epic
without committing the language to a general effect system. They are
simple enough that a reader can audit the implementation in
[crates/harn-ir/src/lib.rs](https://github.com/burin-labs/harn/blob/main/crates/harn-ir/src/lib.rs)
in one sitting and convince themselves the analysis is sound for the
shape of program it accepts.

### Soundness story

The analysis is intentionally **conservative and decidable**:

- **Literal-only path proofs.** `fs.writes` only proves a write is
  in-glob when the path argument is a string literal. Dynamic paths
  fail the check — either as "outside the allowed glob(s)" (when the
  IR captured an identifier name that does not match) or "could not
  prove" (when the path expression is more complex). There is no
  symbolic reasoning over string concatenation, format strings, or
  runtime inputs.
- **Capability lattice is explicit.** `capability.policy` normalizes
  user-facing aliases onto `fs.write`, `process.exec`,
  `network.access`, `mcp.connector`, `llm.model`, `worker.dispatch`,
  `human.approval`, and `autonomy.policy`. Bridge calls can carry more
  than one effect; for example, an MCP edit tool is both connector
  access and workspace mutation.
- **Flow-sensitive over the CFG, path-insensitive within nodes.** The
  `approval.reachability` checker walks every CFG path with a small
  state `(explicit_approval, scoped_approval_depth)`. It reports a
  violation the first time it reaches an unguarded side-effecting
  call on any path. Loops and joins are handled by the `(node, state)`
  visited set.
- **No interprocedural analysis.** Each handler is checked in
  isolation. A function that helpfully wraps `write_file` is opaque
  to `fs.writes`. This is a deliberate trade: it keeps the analysis
  local and fast, and it forces invariants to live on the leaf
  handler that actually performs the side effect, where reviewers can
  see them.

### Why this scope and not the broader epic

We considered and **deferred**:

- **A full effect system (`requires` / `forbids` keywords).** Would
  require coordinated changes across lexer, parser, spec, tree-sitter,
  every stdlib signature, conformance, the formatter, and the LSP. The
  marginal expressivity over the attribute form does not justify that
  blast radius for a feature that is, today, opt-in for security-
  sensitive handlers.
- **Linear `Approval<T>` types.** Linear types are notoriously hard to
  teach and would break the gradual-typing story. The
  `approval.reachability` invariant achieves the same compile-time
  guarantee for the cases that matter (every reachable side effect
  is preceded by `request_approval` or wrapped in `dual_control`)
  without introducing a second type discipline.
- **Symbolic execution / SMT-backed checks.** Out of scope for the
  first version. The CFG-based analysis can be replaced
  invariant-by-invariant later without changing the
  `@invariant(...)` surface.
- **Auto-derivation of invariants from examples.** Speculative; a
  research partnership concern, not a runtime concern.

### Integration with runtime capability policy

The runtime `CapabilityPolicy` stays. Compile-time invariants are
strictly additive: they reject programs that would otherwise pass
runtime capability checks (e.g., a handler whose path is *technically*
allowed by the runtime ceiling but not provably so at check time).
Programs that pass `harn check --invariants` still go through runtime
capability intersection at execution.

## Consequences

### Positive

- **Existing programs keep working.** `@invariant` is opt-in;
  `harn check` (without `--invariants`) keeps its prior contract.
  Conformance fixtures without invariants are unchanged.
- **Auditable security claims.** A handler annotated with
  `@invariant("fs.writes", "/workspace/scratch/**")` carries a
  mechanically checked claim that no reachable write escapes that
  glob — useful for security reviews and trust-graph attestations
  without requiring auditors to read the entire dependency tree.
- **Editor-grade UX from day one.** Diagnostics carry a path of
  `(span, label)` steps so the LSP and `harn explain` can show *why*
  a violation is reachable, not just *that* it is.
- **Replaceable per-invariant.** The `Invariant` trait
  (`fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic>`)
  isolates each check. Future invariants — and future, more precise
  back-ends for existing invariants — slot in without changing the
  user-visible surface.

### Negative / risks

- **Conservatism produces false positives** on dynamic paths and
  helper-wrapped writes. The escape valve today is to either inline
  the side effect into the annotated handler or accept the warning;
  there is no `unsafe_assert_effect(...)` yet (the closed epic
  suggested one — left as a follow-up if real users need it).
- **Builtin classification is hardcoded** in `classify_call` /
  `classify_tool_call`. A new write-shaped builtin or tool can slip
  through `fs.writes` until it is added to the classifier. Mitigation:
  the classifier already has a heuristic over tool-name verbs
  (`contains("write")`, `contains("delete")`, …) for `mcp_call` /
  `host_tool_call`; named builtins must be added explicitly when
  introduced.
- **No interprocedural reasoning.** A handler that delegates to a
  helper is invisible to the analysis. The mitigation today is to
  keep the `@invariant` on the handler that actually performs the
  side effect.

### Deferred

- `unsafe_assert_effect(...)` audit-logged escape hatch.
- Linear `Approval<T>` types and per-approval-class consumption.
- A dedicated `forbids` invariant matching the V1 §"Deeper moats"
  example *"this agent will NEVER call `fs::delete`"*. Today this is
  partially covered: `@invariant("fs.writes", "<glob>")` rejects every
  reachable filesystem mutation that is not provably in-glob, so a
  sufficiently narrow glob acts as a hard ban. A first-class
  `@invariant("forbids", "fs.delete")` syntax would still be cleaner
  and is left as a follow-up if real handlers want it.
- Research partnerships with academic alignment groups (METR, Apollo).
  Tracked separately under the moat-research label.

## Verification

The shipped implementation is exercised by:

- Unit tests in [crates/harn-ir/src/lib.rs](https://github.com/burin-labs/harn/blob/main/crates/harn-ir/src/lib.rs)
  covering positive and negative cases for each invariant plus the
  explain path.
- CLI integration tests for `harn check --invariants` and
  `harn explain --invariant`.
- LSP diagnostic surfacing tests.
- Conformance fixtures under `conformance/tests/scenarios/` and
  `conformance/tests/stdlib/` that exercise the `@invariant` parser
  and Flow-predicate companion attributes (`@deterministic`,
  `@semantic`, `@archivist`, `@retroactive`).

The catalog is documented in
[language-spec.md §"`@invariant`"](../language-spec.md) and the CLI
flags in [cli-reference.md](../cli-reference.md).
