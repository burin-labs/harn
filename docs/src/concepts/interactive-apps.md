# Why Harn apps separate behavior from pixels

A Harn app is a Harn program with an interactive view. The program owns the
decisions: state, validation, model calls, restoring work, replay, and recovery.
A shared renderer owns browser work such as updating controls, painting the
canvas, following the pointer, and downloading files.

This split keeps application code portable. The same Harn event handler can
run in the standalone browser host, an MCP Apps client, a test, or a future
native host. It does not import React, browser globals, or a Rust product
module.

## The round trip

1. Harn returns a typed `UiDocument`.
2. The renderer draws its elements.
3. The renderer turns user input into a typed `UiEvent`.
4. A Harn tool validates the event, changes state, and returns a `UiUpdate`.
5. Optional effects ask the renderer to schedule another event, capture a
   canvas, download bytes, or perform another host-owned presentation task.

Canvas pointer movement is local while the pointer is down. The renderer sends
one `canvas.stroke` event with coordinates from `0.0` to `1.0` when the stroke
ends. Harn therefore owns the stroke and undo history without paying for
a tool call per pixel.

For high-frequency state changes, `ui.portable_app_resource` runs the same Harn
reducer in a host-owned browser worker. Each update includes the next plain
state. If browser execution is unavailable, the renderer sends that latest
state to the standard Harn event tool, which runs the same artifact. The view
still has no direct worker, file, network, or model authority.

## What belongs where

| Layer | Owns |
|---|---|
| Harn app | Product state, event handling, prompts, jobs, file rules, replay |
| `std/ui` | Typed documents, events, effects, validation, shared renderer |
| `std/media` | Durable assets, parent lineage, exact-text design documents, SVG/PNG export |
| Harn host | Files, network, process isolation, MCP transport, browser startup and shutdown |
| Browser | Pointer capture, canvas rasterization, DOM, accessibility primitives |

App-specific Rust is a design failure: add the missing reusable host
capability instead. App-specific JavaScript needs the same evidence. This rule
lets later image editors, audio tools, eval explorers, and review consoles use
one tested foundation.

Continue with [Build an interactive Harn app](../cookbooks/build-interactive-app.md)
or [Run Harn app logic in the browser](../cookbooks/run-app-logic-in-browser.md).
Use the [`std/ui` reference](../stdlib/ui.md) for exact types and limits.
