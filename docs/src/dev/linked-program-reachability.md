# Linked-program reachability

Harn removes unused Harn exports when it builds a closed native program. The
package linker owns this decision because it can see the entrypoint, resolved
module graph, namespace uses, re-exports, and final artifact together. The
parser reports namespace demand, and the runtime installs the linker's result;
neither maintains a second reachability algorithm.

## Safety boundary

Every imported module remains in the graph and every module initializer runs.
The linker removes only declarations that are not observable from the closed
program:

- selective imports demand their named exports;
- static namespace properties demand their named members;
- selected callables retain their transitive private callable dependencies;
- selected public types retain the matching type-schema initialization;
- escaped, dynamically indexed, or publicly re-exported namespaces retain the
  whole public surface.

Ordinary source-keyed bytecode and module caches remain complete. A specialized
module is valid only inside the graph-bound linked program that selected it.

## Artifact surfaces

| Surface | Reachability owner | Disposition |
|---|---|---|
| Native schema-v3 `.harnpack` | Harn package linker | Uses one graph-bound `program.harnlink` artifact and the deterministic link report. |
| Embedded standard-library modules used by that pack | Harn package linker | Uses the same module and symbol reachability pass as user modules. Unreachable standard-library modules are absent; reachable modules can have unused exports removed. |
| Legacy schema-v2 `.harnpack` | Source/module cache loaders | Deliberately remains complete and relocatable. Existing signed archives keep their historical shape and use the legacy replay adapter. |
| Portal React application | Vite and Rollup | Already uses the production JavaScript module bundler. A second Rust or Harn tree shaker would duplicate that owner. |
| App-host HTML, CSS, and JavaScript | Direct Rust embedding | Six directly referenced assets have no import graph. Removing content without a JavaScript-aware bundler would be unsafe and has no demonstrated shared-cause win. |
| Browser/Wasm portable kernel | Rust release linker and `wasm-pack` | Deliberately separate. It runs the authority-free portable program-artifact contract, which does not support native module imports. Native linked-program specialization would conflate two artifact contracts. |
| Harn executable and other `include_str!` assets | Rust compiler and release linker | Out of scope. Binary-level section elimination and compression are release-build concerns, not Harn symbol reachability. |

This division keeps one optimizer per semantic graph. New native closed-program
surfaces should call `harn_vm::linked_program::link_program`; they should not
copy namespace scans or specialize ordinary cache entries.

## Inspecting a decision

`harn pack --json` and `harn pack verify --json` return the same hash-bound
`link_report`. For each module it records demand, input and output byte counts,
retained symbols and reasons, removed symbols, initializer and type-schema
bytes, and any conservative widening. `harn run --json` adds the installed
artifact state and decode time to its `pack_run` event.

These records answer why a symbol survived without disassembling bytecode. A
missing, corrupt, graph-mismatched, or incompatible schema-v3 artifact fails
closed unless its signed descriptor explicitly allows exact-source fallback.
