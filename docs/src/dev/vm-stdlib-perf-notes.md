# VM and stdlib hot-path profile (issue #1426)

This page captures the allocation profile, the two optimizations that
landed, and the remaining hotspots that frame which Rust orchestration
work is realistic to port to Harn next. Reproduce locally with:

```bash
./scripts/bench_vm.sh --no-build --iterations 20
cargo bench -p harn-vm-perf --bench bench_vm_fixtures
cargo bench -p harn-orchestration-perf --bench bench_workflow_bundle
```

The fixture set covers the option-builder pipelines (dict merge, subscript
assign, `filter_nil`/`pick_keys`) the connector helpers and agent loops run
on every call, plus the workflow bundle export the host previews when it
ships a portable bundle.

## Landed optimizations

### 1. `SetSubscript` mutates in place via `Rc::make_mut`

The previous `out[k] = v` fast path cloned the entire backing
`BTreeMap`/`Vec` on every assignment because `active_local_slot_value`
returns the slot value by clone, leaving `Rc` strong count ≥ 2. The new
path looks up the slot by index, then mutates the contained `Rc<...>`
directly with `Rc::make_mut`, which is a no-op when the slot owns the
unique reference (the steady state for builder loops).

Effect on `dict_subscript_assign`:

| metric           | baseline (Harn 0.8.3) | post-#1426 | delta |
|------------------|----------------------:|-----------:|------:|
| allocations/run  |               684,058 |    328,058 | −52 % |
| allocated bytes  |            58,406,677 | 19,670,677 | −66 % |
| criterion median |               25.4 ms |    21.7 ms | −15 % |
| `bench_vm.sh` 3-pass mean |          ~30 ms |    15.9 ms | −47 % |

The closure-captured / env-fallback path is preserved — when the binding
lives in `env` (e.g. captured by a closure rather than a slot-resolved
local), `Rc::try_unwrap` keeps the no-other-references case
allocation-free.

### 2. Native option-builder helpers replace Harn `+ {[k]: v}` loops

`std/collections::filter_nil`, `std/collections::pick_keys`, and the
`std/json` `merge`, `pick`, `omit` helpers all expanded to a
`var result = {}` accumulator with `result = result + {[k]: v}` per
iteration — fresh `Rc<BTreeMap>` allocation per inserted entry plus a
per-call closure dispatch in `filter_nil`. Every connector wrapper
(`std/connectors/{github,linear,notion,slack}`), `std/context`,
`std/graphql`, the agents stdlib, and the workflow scaffolding leans on
these helpers.

Five new builtins under `crates/harn-vm/src/stdlib/collections.rs`
handle the work in one allocation:

- `__dict_filter_nil(d)` — drop `nil`, `""`, and the literal string
  `"null"`; returns the original `Rc` when nothing changes.
- `__dict_merge(a, b)` — `Rc::try_unwrap(a)` + `BTreeMap::extend`.
- `__dict_pick(data, keys)` — match `std/json::pick` semantics
  (drop missing + `nil`).
- `__dict_pick_keys(d, keys, drop_nil)` — match
  `std/collections::pick_keys` (preserve `nil` unless `drop_nil` is set).
- `__dict_omit(d, keys)` — `Rc::try_unwrap(d)` + `BTreeMap::retain`.

The Harn-level `pub fn`s in `stdlib_collections.harn` and
`stdlib_json.harn` now thin-wrap these so every existing
`import { filter_nil } from "std/collections"` consumer transparently
picks them up; the public API is unchanged.

Effect on `filter_nil_loop` (4,000 iterations of
`filter_nil(merge(config, overlay))` plus a `pick_keys` projection — the
canonical connector option-builder shape):

| metric           | baseline (Harn 0.8.3) | post-#1426 | delta |
|------------------|----------------------:|-----------:|------:|
| allocations/run  |             1,868,316 |    412,276 | −78 % |
| allocated bytes  |           535,181,340 | 34,187,963 | −94 % |
| criterion median |              161.9 ms |    25.5 ms | −84 % |
| `bench_vm.sh` 3-pass mean |          ~98 ms |    17.7 ms | −82 % |

Conformance was unchanged (`stdlib_collections`, `stdlib_json`, and the
broader 933-test suite all pass).

## Remaining hotspots

`bench_vm_fixtures` numbers (allocations × wall-time per fixture run, on
the optimized binary) tell us where to focus next:

| fixture | alloc/run | bytes/run | median wall | shape |
|---|---:|---:|---:|---|
| `list_map_filter` | 10.9M | 4.43 GB | 376 ms | `list.filter(closure).map(closure)` in a loop |
| `local_variable_lookup` | 2.20M | 3.0 MB | 161 ms | bare local-slot reads |
| `function_call_loop` | 1.70M | 219 MB | 96 ms | tight `step(value)` recursion |
| `agent_tool_dispatch` | 1.54M | 261 MB | 53 ms | `agent_dispatch_tool_batch` over 6 calls × 500 iters |
| `comparison_loop` | 1.10M | 1.4 MB | 200 ms | numeric/string `<,==,!=` mix |
| `struct_field_read` | 0.90M | 3.3 MB | 94 ms | struct field access in a hot loop |
| `dict_merge_loop` | 0.85M | 96 MB | 45 ms | `result = result + {[k]: v}` accumulator |

Two patterns dominate the remaining cost:

1. **Closure callbacks per element.** `list_map_filter` allocates ~2,725
   bytes and ~5,450 ops per iteration's worth of map+filter calls — the
   per-callback `VmEnv` clone-on-call probe (`bench_vmenv_clone`) shows
   each call constructs a fresh capture environment even for closures
   with zero captures. Native `list.filter`/`list.map` already exists,
   but the Harn-level callback dispatch is the bottleneck. Worth
   exploring: a `flat`-shape intrinsic for arithmetic predicates (e.g.
   `value % 2 == 0`), or a peephole that lifts trivial closures to
   inline VM ops.

2. **`Rc::try_unwrap` defeated by the slot/stack double-hold.** The
   `dict + dict` operator already does `Rc::try_unwrap` for the unique
   case, but `result = result + {[k]: v}` always sees the slot still
   holding the value while the operator runs (slot ref + stack ref). The
   right answer is either a `var <op>= rhs` peephole that emits a
   "swap-take" sequence, or a compiler pass that moves a slot value
   onto the stack when it knows the slot is about to be overwritten.
   Cheaper interim is to keep migrating Harn helpers to subscript-store
   (now allocation-free) instead of the `+` accumulator.

## Workflow-bundle export profile

`bench_workflow_bundle` exercises the validation + graph normalization +
portable-bundle export path (`crates/harn-vm/src/orchestration/workflow_bundle.rs`).
Allocation counts on a representative 6-node, 4-trigger, 2-connector,
2-capsule fixture:

| stage | alloc/run | bytes/run | criterion median |
|---|---:|---:|---:|
| validate | 205 | 86 KB | 18 µs |
| preview | 2,567 | 310 KB | 102 µs |
| export_graph | 2,408 | 277 KB | 88 µs |

`export_workflow_bundle_graph` clones every per-node
`editable_fields` slot once into the node and once into the global
list. Those clones are correctness-preserving today (the global list is
sorted afterwards), but they're an obvious follow-up if this gets hot in
real CI loads. Numbers here are baseline for the new fixture; reproduce
with `cargo bench -p harn-orchestration-perf --bench bench_workflow_bundle`.

## What's now realistic to port from Rust to Harn

With the option-builder cost paid natively and `out[k] = v` running
allocation-free, several control-plane paths previously kept in Rust on
performance grounds become reasonable Harn candidates:

1. **Trigger preflight wiring.** `crates/harn-vm/src/triggers/dispatcher`
   builds option dicts the same way connectors do; the bookkeeping is
   trivially expressible in Harn now that builder loops are cheap.
2. **Workflow stage option assembly.** `assemble_stage_options` in
   `orchestration/stage_options.rs` does dozens of small
   `merge`/`filter_nil` style merges on every stage start. Moving this
   to a Harn helper that delegates to `__dict_*` builtins keeps the
   Rust crate boundary clean for the actual orchestrator while pushing
   the editorial work into Harn.
3. **Connector setup-status normalization.** `connectors/shared.harn`
   already runs in Harn but used to be cost-prohibitive for high-fan-out
   trigger packs; the option-builder cost is no longer the bottleneck.

Areas still better served by Rust: `workflow_bundle` graph normalization
(needs serde, deterministic sort, and SHA-256 digests in one place);
agent tool dispatch (touches the host bridge and tool annotation cache);
flow store atom emission (Ed25519 + SQLite). Profile reruns will tell us
when any of those tip over.
