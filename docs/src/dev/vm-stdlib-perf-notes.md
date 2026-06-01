# VM and stdlib hot-path profile

This page captures the allocation profile behind issue #1426 and the
follow-on runtime/typechecker performance wave tracked by issue #2095.
The first sections are historical context for the May 2026 optimization
series; the post-#2095 section records which bottlenecks have since
landed so new work starts from the current shape instead of refiling
already-fixed hotspots. Reproduce locally with:

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

### 3. Regex builtins share compiled patterns via `Rc` instead of cloning

Issue #2796 surfaced this while porting a TypeScript repo-audit script:
a line-oriented scan of the repository was ~3.4× slower in Harn than the
Node baseline. Decomposing the scan (file walk / `read_text` / line split
/ `contains` / `regex_match`, each timed separately over ~740k lines)
isolated the cost entirely to `regex_match` at ~4.4 µs/call — file I/O,
splitting, and `contains` were all already cheap.

The pattern was cached, but `get_cached_regex` returned `regex::Regex::clone`
on every hit, and `regex::Regex::clone` deep-copies the compiled program and
its lazy-DFA match-cache pool. A standalone Rust probe confirmed the cost:
134k `find_iter` calls over a real file took 3.3 ms reusing one `Regex`,
466 ms cloning the `Regex` per call, and 2.7 ms cloning an `Rc<Regex>` per
call — i.e. the deep clone, not the match, was ~99% of the time.

The fix stores `Rc<regex::Regex>` in the thread-local cache so a hit is a
refcount bump, adds a single-slot "last pattern" memo that skips the
cache-key `format!` and HashMap hash when a scan loop reuses one pattern,
and switches the regex/`contains`/`split` family to borrow their subject and
needle (`VmValue::as_str_cow`) instead of `display()`-cloning per call.

Effect on `regex_scan_loop` (4,000 iterations × 10 lines of two `contains`
plus one `regex_match` — the canonical line scan shape):

| metric                    | baseline (Harn 0.8.60) | post-#2796 | delta  |
|---------------------------|-----------------------:|-----------:|-------:|
| `harn bench` 10-iter mean |                 367.9 ms |    36.6 ms | −90 %  |

A full-repository scan (~740k `regex_match` calls over 1,409 files) drops
its regex phase from ~4.0 s to ~1.2 s; the remaining cost is VM call
dispatch and result-list construction, not the regex engine. Conformance
(`regex*`, `string*`) was unchanged.

## Post-#2095 performance wave

The #1426 profile left a second wave of runtime and typechecker work.
Issue #2095 split that wave into small PRs so each hot path could land
with isolated measurements and conformance parity.

| Area | Issues / PRs | Change | Measured signal |
|---|---|---|---|
| Typechecker scope entry | #2093 / #2102 | Replaced deep-cloned `TypeScope.parent` chains with `Rc<TypeScope>` parents and shared root-scope children. | Synthetic one-line function corpus typecheck dropped from 69 ms to 3 ms at 500 fns and from 8.27 s to 29 ms at 10,000 fns. |
| Closure callbacks | #2086 / #2099 | Pushed callback closures onto the existing VM frame stack and drove them with `drive_until_frame_depth`, removing the per-callback boxed future and the frame/iterator/deadline `mem::take` isolation. | `list_map_filter` moved from the checked-in #1426 baseline mean of 298.64 ms to 76.08 ms in the PR bench table. |
| Named user calls | #2085 / #2101 | Split `Op::CallBuiltin` into a sync user-closure fast path and async fallback. | `function_call_loop` best-of-three minimum improved by 11.2%. |
| Tail calls | #2088 / #2103 | Split `Op::TailCall` into a sync TCO fast path and async fallback for tracked/generator/non-closure cases. | `recursive_countdown` best-of-three minimum improved by 5.6%. |
| Call argument packing | #2091 / #2107 | Bound regular closure, tail-call, pipe, and sync-builtin arguments directly from VM stack slices; materialized `Vec`s only for paths that need ownership. | Conformance stayed green, with targeted hot fixture smoke runs covering `function_call_loop`, `method_call_dispatch`, and `list_map_filter`. |
| `VmValue` layout | #2092 / #2100 | Boxed rare/large variants behind shared payloads and added a layout-budget test. | `VmValue` size budget tightened from 48 bytes to 32 bytes. |
| Method dispatch | #2087 / #2108 | Added sync method dispatch for optional nil, inline-cache hits, and pure receiver methods, leaving callable-backed methods on the async path. | `method_call_dispatch` release mean measured at 32.74 ms; `list_map_filter` stayed near 83 ms after the dispatch split. |
| `harn run` setup | #2094 / #2109 | Deferred LLM builtin registration and lazy-loaded setup-only runtime config. | Warm run-setup samples for `function_call_loop` settled at roughly 1 ms after first-touch initialization. |

## Historical pre-#2095 hotspots

`bench_vm_fixtures` numbers (allocations × wall-time per fixture run, on
the post-#1426 binary) were the input to #2095:

| fixture | alloc/run | bytes/run | median wall | shape |
|---|---:|---:|---:|---|
| `list_map_filter` | 10.9M | 4.43 GB | 376 ms | `list.filter(closure).map(closure)` in a loop |
| `local_variable_lookup` | 2.20M | 3.0 MB | 161 ms | bare local-slot reads |
| `function_call_loop` | 1.70M | 219 MB | 96 ms | tight `step(value)` recursion |
| `agent_tool_dispatch` | 1.54M | 261 MB | 53 ms | `agent_dispatch_tool_batch` over 6 calls × 500 iters |
| `comparison_loop` | 1.10M | 1.4 MB | 200 ms | numeric/string `<,==,!=` mix |
| `struct_field_read` | 0.90M | 3.3 MB | 94 ms | struct field access in a hot loop |
| `dict_merge_loop` | 0.85M | 96 MB | 45 ms | `result = result + {[k]: v}` accumulator |

Two patterns dominated that snapshot:

1. **Closure callbacks per element.** `list_map_filter` allocates ~2,725
   bytes and ~5,450 ops per iteration's worth of map+filter calls — the
   per-callback `VmEnv` clone-on-call probe (`bench_vmenv_clone`) shows
   each call constructs a fresh capture environment even for closures
   with zero captures. #2086 removed the per-callback boxed future and
   `mem::take` isolation, and the later method/call-argument work
   reduced the remaining callback dispatch overhead. Re-measure before
   filing more callback-specific work; the old `list_map_filter` numbers
   are no longer representative.

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
