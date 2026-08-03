# Run a portable reducer in a browser

This guide builds the core WebAssembly adapter and runs a two-module reducer
package in a dedicated browser worker. It does not depend on the interactive
app stack.

## Prerequisites

- the repository setup from `make setup`
- a current Node.js installation
- a current Chrome installation for the automated worker test

The Make targets install pinned `wasm-pack` and `wasm-tools` binaries plus the
Rust Wasm target outside the checkout. Mutable Cargo and Wasm outputs remain
worktree-local.

## Build and verify

From the repository root:

```console
make wasm-check
```

This validates the WIT projection, builds the release `wasm32-unknown-unknown`
module, rejects unreviewed imports, and runs the conformance corpus in a real
headless Chrome dedicated worker. It does not substitute Node tests for the
browser path.

## Open the reducer

Start the cross-platform demo server:

```console
make wasm-demo
```

Open `http://127.0.0.1:8765`. The page loads `demo/worker.js`; the worker loads
the generated ES module and `demo/package.json`, compiles the root reducer plus
its imported math module with `compilePackage`, and retains reducer state between
typed events. “Restore saved state” sends a structured clone back to the worker
to demonstrate serializable application state. The manifest is generated from
`demo/package-root.harn` and `demo/package-reducer-math.harn`; run
`make check-portable-demo-package` when changing either source.

The page owns HTML, controls, and presentation. Harn owns reducer policy. The
Wasm kernel owns deterministic execution. No DOM or canvas object crosses the
worker boundary.

For a quick manual proof, press **Increment** and then **Add ten**. The worker
returns count `11` with history `[1, 11]`. Press **Reset**, then **Restore saved
state**; the structured-cloned initial state returns. Expanding the source panel
also shows syntax highlighting generated from Harn's canonical language
vocabulary rather than a demo-owned keyword list.

The adapter does not require `SharedArrayBuffer` or Wasm threads. For parallel
dispatch, create multiple dedicated workers, reuse the same artifact bytes,
and keep each worker's reducer state independent. Do not run CPU-heavy
transitions on the browser main thread.

## Use the generated adapter

The worker path is intentionally small:

```js
import init, { compilePackage, start } from "../pkg/harn_wasm.js"

await init()
const manifest = await fetch("./package.json").then((response) => response.text())
const compiled = compilePackage(manifest, "reduce", "function")
if (!compiled.ok) throw new Error(compiled.diagnosticsJson())

const result = start(
  compiled.artifactBytes(),
  JSON.stringify({ state, event }),
  '{"capabilities":[]}',
)

if (result.status === "completed") {
  state = JSON.parse(result.valueJson())
}
```

Do not silently ignore `suspended` or `failed`. A privileged reducer must pass
an exact grant document with a host-generated snapshot key, retain the returned
snapshot bytes, perform the typed request outside Wasm, and call `resume` with
the matching result.

For native and browser latency measurements, receipt fields, and a recorded
worker procedure, see
[Benchmark the portable kernel](./portable-kernel-benchmarking.md).
