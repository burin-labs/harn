# Run Harn app logic in the browser

Use a browser reducer when pointer, keyboard, or form events should update the
view without a server round trip. The same compiled Harn program also runs in
the server fallback, so the app has one behavior to test and maintain.

The complete example is
[`examples/apps/portable-counter.harn`](https://github.com/burin-labs/harn/blob/main/examples/apps/portable-counter.harn).

## 1. Write a reducer

A reducer receives the current state and one event. It returns the next state
and one `UiUpdate`:

```harn
fn reduce(input) {
  let count = input.state.count
  if input.event.kind == "click" && input.event.target == "add" {
    count = count + 1
  }
  const state = {count: count, revision: input.state.revision + 1}
  return {
    state: state,
    update: {
      schema: "harn.ui_update.v1",
      document: render(state),
      effects: [],
    },
  }
}
```

Keep the reducer deterministic. Browser objects, files, network clients, and
model clients stay outside its state. Use plain records, lists, strings,
numbers, booleans, bytes, and `nil`.

## 2. Compile it once

Compile the reducer with `std/portable` and stop on a diagnostic:

```harn
import * as portable from "std/portable"

const compiled = portable.compile(REDUCER_SOURCE, "reduce")
if !is_ok(compiled) {
  throw "reducer did not compile: " + json_stringify(unwrap_err(compiled))
}
const program = unwrap(compiled)
```

This artifact is the behavior shared by the browser and server. Do not write a
second JavaScript or Rust reducer.

## 3. Keep the server fallback on the same artifact

The host passes the browser's latest state when it must fall back to the event
tool. Run the same artifact with that state and return the complete reducer
result:

```harn
let state = {count: 0, revision: 0}

fn handle_event(raw) {
  const input_state = raw.state ?? state
  const execution = portable.start(program, {state: input_state, event: raw.event})
  if execution.status != "completed" {
    throw "reducer failed: " + json_stringify(execution)
  }
  state = execution.value.state
  return execution.value
}
```

Using `raw.state` prevents a worker restart from rewinding earlier browser
events. Keep the local `state` value for hosts that call only the server tool.

## 4. Register one app resource

`ui.portable_app_resource` packages the artifact, initial state, and fallback
tool with the shared renderer:

```harn
import * as ui from "std/ui"

const resource = ui.portable_app_resource(
  "ui://example/counter",
  "Counter",
  "counter.handle_event",
  program,
  state,
)
```

Register `counter.handle_event` and `resource` as shown in
[Build an interactive Harn app](./build-interactive-app.md#4-register-the-tool-and-resource).

The app view cannot create workers or fetch the Harn runtime. The standalone
host owns a worker in its trusted sandbox and accepts only the typed portable
messages. An MCP Apps host without that worker support uses the standard event
tool immediately.

## 5. Call a host tool when needed

Pass `["tools.invoke"]` as the capability list when the reducer must call a
registered tool. The portable kernel pauses, the host performs the exact tool
call, and the kernel continues with its typed result. No other browser
capability is accepted by `std/ui` today.

Keep state changes after the tool result. Make externally visible tool actions
safe to retry because a browser or process can stop after the action succeeds
but before the final view update arrives.

## 6. Run and prove both paths

Start the app:

```console
harn app run examples/apps/portable-counter.harn
```

For a browser claim, click the controls and confirm the view reports the
portable runtime in its `data-runtime` attribute. A changed counter alone is
not proof because the server fallback produces the same pixels.

Test the reducer through `portable.start` with exact state and events. Then
call the fallback tool with the same state and compare its `{state, update}`
result. The browser worker tests run through `make wasm-check`; the fast worker
ordering and suspend/resume tests run through `make check-app-host`.

See the [`std/ui` reference](../stdlib/ui.md#browser-reducers) for the exact
resource signature and the
[portable kernel contract](../portable-kernel-reference.md) for supported Harn
constructs, limits, grants, and diagnostics.
