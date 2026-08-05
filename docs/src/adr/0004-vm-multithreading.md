# ADR 0004: VM multithreading via Send values + share-nothing isolates

## Status

Proposed. Records the strategy for the multithreading epic
[#2688](https://github.com/burin-labs/harn/issues/2688) and its phase
children ([#2689](https://github.com/burin-labs/harn/issues/2689),
[#2690](https://github.com/burin-labs/harn/issues/2690),
[#2691](https://github.com/burin-labs/harn/issues/2691),
[#2692](https://github.com/burin-labs/harn/issues/2692)). Phase 0 (the
ambient-state removal that unblocks this) shipped / is in flight as
[#2667](https://github.com/burin-labs/harn/issues/2667),
[#2686](https://github.com/burin-labs/harn/pull/2686), and
[#2687](https://github.com/burin-labs/harn/issues/2687).

## Context

The harn VM is **single-threaded by construction**. The CLI top-level
runtime is already `new_multi_thread`, but every line of VM execution is
pinned onto a `tokio::task::LocalSet` and dispatched via `spawn_local`,
because the core value type is `!Send`.

The root cause is `VmValue`: every heap-carrying variant wraps `Rc<...>`
(`String(Rc<str>)`, `List(Rc<Vec<VmValue>>)`, `Dict(Rc<BTreeMap<..>>)`,
`Closure(Rc<VmClosure>)`, `Iter(Rc<RefCell<VmIter>>)`, ...). `Rc` is
`!Send`/`!Sync`, so `VmValue`, `Vm`, `VmEnv`, every closure, and every
async-builtin future are `!Send`. ~393 `Rc<` sites across `harn-vm`.

We want true parallelism for three reasons: the embeddable agent SDK
([#2636](https://github.com/burin-labs/harn/issues/2636)) wants a `Send`
runtime handle; agent / tool / pool fan-out is real but every "parallel"
task is `spawn_local` and serializes CPU work on one core; and a cloud platform
wants to saturate a box from one process instead of one-VM-per-OS-thread.

## Survey

How do other embeddable VMs get `Send` and/or parallelism?

| Engine | `Send` story | Parallelism story |
| --- | --- | --- |
| **mlua / rlua** (Lua) | `send` feature adds a `Send` bound; VM access serialized by a reentrant mutex. | True parallelism needs **one Lua VM per thread**; data crosses by serialization or `Arc<Mutex<..>>` userdata. Maintainers note `LocalSet` is *more* efficient because it is lock-free. |
| **rquickjs** (QuickJS) | `Send`/`Sync` via a marker; runtime behind a mutex. | "QuickJS does not support threading, so the runtime is locked behind a mutex." Experimental `parallel` feature, use at own risk. |
| **boa** (Rust JS) | Contexts are thread-bound. | Objects can be shared between contexts only within the same thread. Single-threaded stance. |
| **deno_core / V8** | `JsRuntime` is **not `Send`** (isolate thread affinity). | **Per-thread isolate + message passing**; workers are "separate universes that communicate via messages." Sharing a runtime across threads segfaults. |

The pattern is unambiguous: **no embeddable VM gets real CPU
parallelism from a single shared-mutable instance.** The ones that scale
(V8/deno, mlua-for-parallelism) all use share-nothing per-thread
instances and pass values across a boundary. A global `Mutex<Vm>` (the
mlua `send` / rquickjs approach) buys the `Send` *marker* but serializes
execution on the lock — for a tree-walking interpreter that touches the
value graph on every instruction, the lock *is* the hot path.

## Decision

Adopt a **share-nothing isolate model for parallel execution, built on a
`Send` (movable) value type** — explicitly *not* a single shared VM
behind a global lock.

1. Make `VmValue` and the heap graph **movable across threads**
   (`Rc` → `Arc`; interior-mutable cells `RefCell`/`Cell` → a `Send`
   primitive such as `parking_lot::Mutex` / `AtomicBool`). This is the
   enabling primitive: values must be able to *move* across a channel or
   `spawn` boundary.
2. Build the parallel surface as **independent per-thread `Vm`
   isolates** that exchange `Send` `VmValue`s over the existing
   channel/pool plumbing — not as one `Mutex<Vm>` shared by all workers.
   Per-VM inline caches keep the shared compiled `Chunk` free of mutable
   execution state.

We pick share-nothing over shared-mutable because harn is a tree-walking
interpreter that mutates the value graph and per-`Chunk` inline caches on
essentially every instruction; a global `Mutex<Vm>` would serialize
exactly the work we are trying to parallelize. The `Arc` migration is
still mandatory — values must move — but `Arc` is the transport, not the
concurrency model.

### Phasing

- **Phase 0 — ambient-state removal (prerequisite, owned).** task_local
  cutover (#2667, merged), explicit `AsyncBuiltinCtx` ABI (#2686), remove
  the residual `ASYNC_BUILTIN_CTX` task-local (#2687).
- **Phase 1 — `Send` the value graph** (`Rc`→`Arc`), task #48 (#2689).
- **Phase 2 — `Send` the `Vm` / `Env` / dispatch + builtin fn ABIs**
  (#2690).
- **Phase 3 — a real parallel execution surface** (pool/agent fan-out on
  the multi-thread runtime) + a CPU-scaling benchmark (#2691), gated on a
  `thread_local!` work-stealing-safety audit (#2692).

## Consequences

- **Cost: `Arc` atomics.** `Arc` clone/drop is an atomic RMW vs. `Rc`'s
  non-atomic increment, and the interpreter clones values constantly.
  Phase 1 must measure the single-thread regression and decide
  feature-gate vs. unconditional from that number — small (rule of thumb
  < ~3-5%) → unconditional (avoid a `cfg`-split test matrix, the mlua
  pain point); large → gate behind a `send` feature.
- **GC unchanged.** harn is reference-counted with no cycle collector;
  `Arc` does not change that, and cycle collection stays out of scope.
- **Work-stealing correctness is a runtime, not a type, problem.** 40+
  `thread_local!` in `harn-vm` are safe under `LocalSet` but unsafe under
  a work-stealing runtime; #2692 classifies and converts the
  execution-state ones before Phase 3 flips `spawn_local` → `spawn`.
- **Determinism.** Parallel fan-out changes interleaving; conformance
  fixtures that assume single-thread ordering may need ordering
  tolerance.

## Revisit triggers

- If Phase 1 measures an `Arc` regression large enough to gate, revisit
  whether the parallel surface justifies the dual-build cost at all.
- If a future use case needs *shared mutable* cross-thread state (not
  share-nothing message passing), revisit the global-lock model for that
  narrow surface only — but the default stays share-nothing.

## Phase 1 result

Phase 1 went **unconditional**. The focused `bench_vmenv_clone`
single-thread hot-path benchmark kept the same allocation profile (one
8-byte allocation per call) and did not show an `Arc` regression:

| Capture count | `Rc` baseline median | `Arc` result median | Change |
| --- | ---: | ---: | ---: |
| 0 | 16.917 ns | 16.415 ns | -3.0% |
| 5 | 36.608 ns | 34.175 ns | -6.6% |
| 25 | 65.582 ns | 62.646 ns | -4.5% |
| 100 | 77.296 ns | 71.802 ns | -7.1% |

The measured result is below the 3-5% regression threshold, so adding a
`send` feature split would add test-matrix and API complexity without a
performance justification. Reference-count cycles remain unchanged and
out of scope: the value graph is still reference-counted rather than
tracing-GC-backed.
