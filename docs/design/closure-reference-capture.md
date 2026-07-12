# Closure reference capture (harn#4479)

## Decision

Harn closures switch from **by-value environment capture** to **reference
capture** of the specific outer bindings they use, matching JavaScript,
Python, and Swift. A closure that mutates a captured `let` — a scalar rebind
(`n = n + 1`), a field write (`d.n = ...`), or a subscript write
(`xs[i] = ...`) — now has that write observed by the enclosing scope and by
later invocations of the same closure.

### Why (archaeology verdict: no industry reason for by-value)

By-value capture was **deliberate but undocumented**. Its only stated
rationales were (a) "lexical, not dynamic, scoping" — which reference capture
*preserves* (JS/Python/Swift/Lua all reference-capture lexically; lexical vs
dynamic is about *which* binding you see, not whether its cell is shared), and
(b) the `Arc<BTreeMap>` env representation being a cheap-to-clone performance
choice. No determinism, reproducibility, or parallelism guarantee was ever
attached to closure capture, and no user-facing diagnostic documented the
lost-write behavior — it was simply silent.

By-value capture's failure mode is the worst kind: a **silently vanishing
write**. It is exactly the "looks right, behaves wrong" class Harn's soundness
program exists to eliminate, and it is the natural habit of the LLMs that write
most Harn code. It had already caused a latent bug in Harn's own stdlib
(`std/text extract_paths` dedup accumulator was a no-op; quietly replaced by
`.unique()`).

### Value semantics is preserved (orthogonal axis)

Reference capture shares *variable bindings* across a closure boundary. It does
**not** alias distinct variables: `let b = a` still copies, and
`let b = a; b[0] = x` still leaves `a` untouched. Swift is the precedent —
value-typed data + reference-captured `var`s coexist. Container value semantics
(load-bearing for the covariance soundness in #4495) is unchanged.

## Mechanism: captured bindings become shared cells, confined to the env layer

The env binding type changes from `(VmValue, bool)` to a `Binding` enum:

```rust
enum Binding {
    /// An ordinary, unshared binding. Read clones the value out; write
    /// replaces it (copy-on-assignment). Unchanged from today.
    Value { value: VmValue, mutable: bool },
    /// A binding captured by a nested closure. The value lives behind a
    /// shared cell; reads clone the inner value out (value semantics for
    /// reads preserved), writes go *through* the cell so every holder —
    /// the defining frame and every closure that captured it — observes
    /// the update. Cloning the env (per call, per closure mint) Arc-bumps
    /// the cell, so all holders share one cell.
    Cell { cell: Shared<VmMutex<VmValue>>, mutable: bool },
}
```

Key properties that make this small rather than large:

- **No new `VmValue` variant.** The cell is an env-internal storage detail; it
  never appears in a user-visible value, so the ~337 files matching `VmValue::`
  are untouched. Blast radius is `env.rs` + the compiler's capture analysis.
- **Writing through a cell sidesteps the scope-map copy-on-write entirely.**
  `assign` on a `Cell` does `*cell.lock() = v` — it does not replace the map
  entry, so the per-call `cloned_for_call` COW split of the scope map is
  irrelevant. The cell is shared regardless of how many times the env is
  cloned. This is why shared mutation works across a boundary that clones envs
  on every call.
- **Fresh activation ⇒ fresh cells.** A captured local is boxed into a fresh
  cell each time its `let`/`const` executes (once per activation). A closure
  minted in that activation captures those cells; the next activation makes new
  ones. Correct per-activation lexical semantics, including per-iteration loop
  captures.

### Compiler: free-variable analysis routes captured locals to the env

Non-captured locals keep the slot fast path unchanged. A local that appears as
a **free variable of a nested closure** is instead allocated in the env
(name-addressed) and boxed as a `Cell` at definition (`DefCell`). The closure
body already compiles free-variable references to `GetVar`/`SetVar` (env name
lookup), so it transparently reads and writes the shared cell that the enclosing
frame boxed. Only closure-*defining* scopes pay any cost, and only for the
specific locals a closure actually captures. The analysis over-approximates by
name (a mutable `let` sharing a name with a captured one is also boxed); that is
a negligible perf cost and never a correctness issue, since a boxed binding is
value-semantic on read and only shared on write.

**`parallel` / `spawn` bodies are capture scopes too.** They are not closure
*literals* in the AST — the parser desugars `parallel each xs { x -> ... }` into
a `Parallel { variable, body }` node — but `compile_parallel` /
`compile_spawn_expr` lower each body into a nested closure that captures the
enclosing environment, and every concurrent branch runs a `closure.clone()` that
shares the captured cells by `Arc`. The free-variable pre-pass therefore treats
`Parallel` and `SpawnExpr` bodies as capture scopes as well; omitting them would
re-introduce the silent-lost-write bug *only* for concurrent code (a mutable
local mutated in a branch would land in the branch's private env copy). This is
verified by the concurrency edge tests (`parallel each`/`parallel N`/`spawn`
each mutate a captured local and the write survives the fan-out).

## Multi-threaded compatibility

The cell type is `Arc<parking_lot::Mutex<VmValue>>` — the same `Shared` +
`VmMutex` pair the pre-existing `module_state` uses, and `Send + Sync` whenever
`VmValue` is `Send` (it is: `VmValue` is built on `Arc`, with no `Rc`/`RefCell`
anywhere). So the cutover introduces **no** new single-thread-only construct and
does not block a future move from the current cooperative single-threaded
scheduler (`spawn_local` on a `LocalSet`) to a genuinely multi-threaded runtime.

Cells are memory-safe under real threads by construction. What they cannot make
safe is the *logic* of sharing one mutable cell across concurrent branches: a
read-modify-write (`total = total + x`) interleaves at every `await` point today
and would be a true data race under threads. That hazard is inherent to shared
mutable state under concurrency, not to this representation — and it is exactly
what guardrail #2's lint surfaces.

## Guardrails (per control-agent steer)

1. **Corpus diff is the decision record.** The full conformance + stdlib +
   scripts corpus is run under the new VM; every fixture whose observed output
   changes is enumerated. Expectation: the diff is dominated by latent
   lost-write bugs the switch *fixes*. That diff — not this prose — is the
   justification of record.
2. **Mutable capture across a parallel boundary is a warning lint, not a hard
   error.** Shipped as `HARN-LNT-064` (`mutable-capture-across-parallel`): a
   `parallel`/`spawn` body that *reassigns* a variable captured from an
   enclosing scope is flagged, with a "return-and-combine" fix suggestion.
   Reassigning a body-local `let`, or merely *reading* a captured variable, is
   never flagged. A warning (not a `harn check` error) is the right severity
   because the concurrency model is cooperative single-threaded and module
   globals already share across the same boundary — the pattern is a footgun,
   not unsound; the value semantics that guarantee memory safety hold
   regardless.
3. **Replay determinism** is reconfirmed — Harn owns replay ordering.
4. **No stale footgun diagnostic to retract.** By-value capture shipped without
   a user-facing diagnostic documenting its lost-write behavior, so there is
   nothing to rewrite. (`HARN-OWN-005` on this branch is the unrelated
   ownership-*leak* lint — value escaping its owning scope — and is untouched.)
