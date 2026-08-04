---
name: harn-apps
short: Build Harn apps with typed views, event handlers, and model jobs.
description: Keep app behavior in Harn while shared hosts draw the view and provide files, network, and browser features.
when_to_use: Use when building or testing an interactive app, drawing canvas, MCP App, or AI workflow in Harn.
---

# Build interactive Harn apps

Use this skill with [[harn-language]] for syntax, [[harn-testing]] for
deterministic checks, [[harn-providers]] for model access, and
[[harn-product-quality]] for proof through the real user path.

## Keep the app in Harn

Put application state, event handling, writing result files, reopening
unfinished work, recovery, and model-job rules in Harn. Import `std/ui` as a
namespace and use short names such as `ui.document`, `ui.event`, and
`ui.update`. The shared renderer turns that typed document into browser
controls, canvas operations, and accessibility information.

Do not add app-specific Rust or JavaScript unless a concrete user interaction
cannot be represented by the shared contract. Add a reusable `std/ui` element,
event, effect, or host capability when the same gap applies to other apps.

## Use module namespaces

```harn
import * as ui from "std/ui"
import { UiEvent } from "std/ui/contracts"

const incoming: UiEvent = ui.event(raw.event)
return ui.update(ui.document("Example", revision, elements))
```

Export short function names from a module and qualify them where they are used.
Do not repeat a module name in calls such as `ui.ui_event`. Keep `Ui` on public
type names because those types also appear in signatures outside the namespace.
Use `ui.test.run` for event-handler tests.

## Start the app

1. Read `docs/src/stdlib/ui.md` and the nearest complete example.
2. Define a named state record and one event handler for typed UI events.
3. Return a `UiUpdate` with the next document and any host effects.
4. Expose the event handler as a tool whose metadata names its UI resource.
5. Run `harn app run path/to/app.harn` and open the printed URL.

Start with `examples/apps/decision-card.harn` for forms and
`examples/apps/logo-studio.harn` for canvas input, model jobs, result files,
and restart recovery.

## Run one reducer in browser and server

Use `std/portable.compile` plus `ui.portable_app_resource` when frequent events
should update the view without a server round trip. The reducer receives
`{state, event}` and returns `{state, update}`. Keep its state to plain Harn
values.

The fallback event tool must run the same `PortableProgram`, prefer the
`raw.state` supplied by the renderer, and return the complete reducer result.
This keeps fallback on the latest browser state after a worker restart. Do not
copy the reducer into JavaScript, Rust, or a second Harn implementation.

The shared host owns the browser worker and Wasm runtime outside the untrusted
app view. The current reducer capability list accepts only `tools.invoke`.
Keep externally visible tool actions safe to retry. Read
`docs/src/cookbooks/run-app-logic-in-browser.md` for the full path and
`examples/apps/portable-counter.harn` for the smallest working app.

## Compose model work

Represent generated media with `MediaAsset`. Submit local and hosted work
through the same typed model-job contract. Prefer `model_job_submit_result`,
`model_job_step_result`, and `model_job_finish_result` when the interface must
show progress or allow cancellation. Use the synchronous helper only when the
caller does not need intermediate states.

Keep provider details in provider-specific code. The ComfyUI backend can upload
verified sketch assets before it builds an image-edit workflow. A hosted backend
receives the same request and returns the same result shape.

## Test without a browser first

Call `ui.test.run` with the event handler and input events. It follows
`send_event` effects in process and returns the final document, emitted effects,
and handled event trace. Assert stable element IDs and meaningful state. Use
mock providers and Harn's mock clock for model-job state changes. Do not use
wall-clock sleeps or polling in tests.

Then run the app through its real host. Prove four things:

- The expected tool fired.
- Effects completed.
- State survived a restart.
- The user can recover from a provider error, or cancel an active job.

Use a real model for quality claims and
repeat stochastic trials when the claim depends on output quality.

## Follow the protocols

Use the current [MCP Apps specification](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx)
for tool and resource metadata. Use provider-owned workflow examples, such as
the [ComfyUI FLUX.2 Klein guide](https://docs.comfy.org/tutorials/flux/flux-2-klein),
when constructing model graphs.

A custom View sends `protocolVersion`, `appCapabilities`, and `appInfo` in
its `ui/initialize` request. Include `fullscreen` in `availableDisplayModes`
when running in Harn's standalone host. Wait for the response, send
`ui/notifications/initialized`, then call tools or read resources. Do not send
`ui/notifications/sandbox-*`; those messages belong to the host's sandbox
frame. Check `hostCapabilities` before using optional host behavior, and do not
assume that `listChanged` is available when it is omitted.

## Verify the change

- Run the narrow conformance fixture for the changed UI or model-job contract.
- Run `make fmt-harn`, `make lint-harn`, and `make check-docs-snippets`.
- Run `harn skill validate` when skill or app guidance changed.
- Exercise `harn app run` in a real browser and capture evidence for material
  user-facing claims.
