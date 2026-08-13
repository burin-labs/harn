# `std/ui` reference

`std/ui` is the typed application layer over [MCP Apps](../interop/ui-resource.md).
It exports `std/ui/contracts`, `std/ui/renderer`, and `std/ui/testing`.

Import the module as a namespace. This keeps each call short without putting
generic names into the file's top-level scope:

```harn
import * as ui from "std/ui"

const next = ui.update(ui.document("Decision Card", revision, elements))
```

## Documents

`ui.document(title, revision, elements) -> UiDocument` checks an ordered list
of elements. A child names an earlier `row` or `column` through `parent`.

Supported element kinds are `column`, `row`, `heading`, `text`, `button`,
`text_area`, `field`, `select`, `status`, `canvas`, `image`, and `divider`.
`variant` selects a shared presentation role such as `workspace`, `work`,
`panel`, `toolbar`, `canvas-area`, `primary`, or `grow`. Set a heading's
`level` from `1` to `6`; the default is `2`.

Canvas strokes use coordinates from `0.0` to `1.0`:

```harn
const stroke: UiStroke = {
  id: "stroke-1",
  points: [{x: 0.1, y: 0.2}, {x: 0.4, y: 0.5}],
  color: "#486449",
  width: 7.0,
}
```

Canvas width and height are each limited to 8,192 pixels. Each stroke contains
2–4,096 finite points, and one document may contain at most 65,536 stroke
points. `ui.document` applies these checks to reopened work before a browser
allocates the canvas.

## Events

`ui.event(raw) -> UiEvent` accepts `ready`, `click`, `input`, `canvas.stroke`,
and `canvas.snapshot`. Strokes contain 2–4,096 points. Each coordinate must be
between `0.0` and `1.0`.

## Updates and effects

`ui.update(document, effects?) -> UiUpdate` is the event handler's return value.

| Effect | Purpose |
|---|---|
| `send_event` | Send a typed event now or after `after_ms` |
| `capture_canvas` | Capture one canvas as PNG and send a `canvas.snapshot` event |
| `download` | Ask the browser to download base64 bytes |

Use a scheduled `send_event` with
[`model_job_step_result`](./model-jobs.md#run-a-job-from-an-interactive-app) to show
progress and cancellation without a blocking polling loop.

## Renderer resource

`ui.renderer_html(tool_name) -> string` returns the shared renderer.
`ui.app_resource(uri, name, tool_name, options?) -> UiResource` validates it
and packages it for `harness.tools.mcp_resource`. The resource declares only
`tools/call`, because that is the only host request the shared renderer sends.

`ui.tool_metadata(resource, options?)` returns the MCP tool metadata that opens
the app. `ui.mcp_resource(resource, options?)` returns the config accepted by
`harness.tools.mcp_resource`.

The renderer contains browser implementation code because the browser owns DOM
and canvas APIs. Applications do not copy or modify that code. Their behavior
remains Harn.

## Browser reducers

`ui.portable_app_resource(uri, name, fallback_tool, program, state,
capabilities?, options?) -> UiResource` runs one `PortableProgram` in the
standalone host's browser worker. The reducer receives `{state, event}` and
must return `{state, update}`. The worker sends the next state with every
update.

The fallback tool must run the same artifact. When browser execution is
unavailable, the renderer calls it with `{event, state}` and requires the same
`{state, update}` result. Returning the full reducer result keeps later
fallback events on the latest state.

The optional capability list currently accepts only `tools.invoke`. The host
performs that request through the standard MCP `tools/call` method, then
resumes the paused program with the matching result. The app view has no worker
or Harn-runtime network authority; the trusted sandbox owns those resources.

Follow [Run Harn app logic in the browser](../cookbooks/run-app-logic-in-browser.md)
for a complete reducer, fallback, resource, and verification path. See
[`std/portable`](../portable-kernel-reference.md) for the artifact and execution
contract.

## Event handler tests

`ui.test.run(handle, events, options?) -> UiTestTrace` drives a Harn event
handler in process. By default it follows `send_event` effects immediately and
ignores their delay. A polling or recovery flow therefore completes without
waiting for real time. Set `max_steps` to turn a repeating event loop into an
exact failure.

`ui.test.element(document, id)` finds a rendered element, and
`ui.test.effect_count(trace, kind)` counts effects across the trace. Canvas
rasterization remains a browser responsibility; pass a `canvas.snapshot`
fixture when testing the Harn behavior that consumes it.
