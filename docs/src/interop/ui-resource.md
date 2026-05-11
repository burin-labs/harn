# MCP Apps UI resource envelopes

`std/ui_resource` packages interactive HTML widgets as portable UI resource
envelopes that follow the [MCP Apps overview][mcp-apps] and degrade cleanly
to text or structured tool output when a host does not advertise UI support.

[mcp-apps]: https://modelcontextprotocol.io/extensions/apps/overview

```harn
import {
  ui_resource,
  ui_select_for_host,
  ui_structured_fallback,
  ui_tool_result,
  ui_tool_result_validate,
} from "std/ui_resource"

let resource = ui_resource(
  "ui://harn-dashboard/kpis@v1",
  "Weekly KPIs",
  weekly_kpi_html,
  {permissions: ["tools/call"], capabilities: ["tools/call", "context/read"]},
)

let result = ui_tool_result(
  resource,
  {structured_fallback: ui_structured_fallback({signups: 42, churn: 3})},
)

ui_tool_result_validate(result)
let rendered = ui_select_for_host(result, host_capabilities)
```

## Resource envelope

`ui_resource(uri, name, html, options?)` returns
`harn.ui_resource.v1`:

| Field | Purpose |
|---|---|
| `uri` | `ui://...` resource URI; hosts fetch this through their MCP resource interface |
| `mime_type` | Defaults to `text/html;profile=mcp-app`, matching the MCP Apps profile contract |
| `contents` / `contents_encoding` | UTF-8 (default) or base64-encoded HTML |
| `content_sha256` / `size_bytes` | Integrity hash and size for host caches and audit |
| `permissions` | Host-mediated permissions the resource will request (`tools/call`, `context/update`, etc.) |
| `capabilities` | JSON-RPC methods the resource may use over `postMessage` |
| `csp` | Source-list directives Harn surfaces back as a `Content-Security-Policy` header value via `ui_resource_csp_header` and a sandbox attribute via `ui_resource_sandbox_attr` |
| `validation` | Summary of the embedded `std/artifact/web` validation: `ok`, `error_codes`, `warning_codes` |
| `meta` | Free-form metadata for host-specific extensions |

Validation reuses [`std/artifact/web`](../modules.md#stdartifactweb)
so embedded UI payloads share the same network/secret/dangerous-navigation
rules used by safe artifact patching. The validator defaults to
`allow_host_bridge: true` because MCP Apps explicitly use
`parent.postMessage` as the host bridge; tighten the policy by passing
`{validation: {allow_host_bridge: false}}` to `ui_resource`.

## Tool-declaration metadata

`ui_tool_meta(resource, options?)` returns a `harn.ui_tool_meta.v1`
envelope and `ui_tool_meta_to_mcp(meta)` serializes it into the
camelCase dict shape MCP Apps hosts read from `_meta.ui`:

| MCP key | Harn field |
|---|---|
| `resourceUri` | `ui.resource_uri` |
| `resourceName` | `ui.resource_name` |
| `profile` | `ui.profile` (defaults to `mcp-app`) |
| `visibility` | `ui.visibility` (`app_only`, `model_visible`, or `always_visible`) |
| `initialView` | `ui.initial_view` (host-side initial state, never trusted as durable storage) |
| `permissions` / `capabilities` | Mirrored from the resource envelope |

Use `visibility: "app_only"` for UIs the model should not see in its
own context, `model_visible` when the structured fallback is also model
context, and `always_visible` when the same payload is useful in both
places.

## Fallbacks

`ui_tool_result(resource, options?)` wraps a resource with a mandatory
text fallback (defaulting to a `web_artifact_text_fallback` projection
of the resource HTML) and an optional `ui_structured_fallback`.
Hosts without UI support receive both fallbacks instead of the
resource:

| Host capability | `ui_select_for_host` selection |
|---|---|
| `apps: true` and resource validation passed | `ui_resource` |
| Otherwise, structured fallback present | `structured_fallback` |
| Otherwise | `text_fallback` |

`ui_host_capabilities` accepts the MCP `client_capabilities.apps`
shape, the OpenAI Apps SDK `ui.apps` shape, or a bare `{apps: true}`
record. `ui_host_supports_apps(caps)` returns whether the host can
render the `mcp-app` profile.

## Message envelopes

`ui_tool_call_envelope(name, params?, options?)` produces the
host→guest JSON-RPC `tools/call` payload a sandboxed iframe receives
through `window.parent.postMessage`. `ui_context_update_envelope(key,
value, options?)` produces the guest→host envelope used to keep
model-visible context in sync as the user interacts with the widget.
Both default to `model_visible: true` for context updates so the model
sees the same state the UI does.

## Validation contract

`ui_tool_result_validate(result)` rejects:

- Missing or empty text fallbacks.
- Tool-meta blocks with the wrong schema.
- UI resources whose HTML failed validation (network calls, host
  bridge abuses, dangerous navigation, embedded secrets).
- Structured fallbacks that do not match the
  `harn.ui_fallback.structured.v1` schema.

`ui_tool_result` already withholds the resource when validation fails,
so the typical flow is: build the resource, build the result, validate,
then dispatch through `ui_select_for_host`. Set
`allow_invalid_resource: true` for preview-only renders where the
host needs to surface validation errors without shipping the resource;
`ui_tool_result_validate` still refuses that record so previews stay
explicit.

See [`examples/ui_resource/dashboard-widget.harn`](https://github.com/burin-labs/harn/blob/main/examples/ui_resource/dashboard-widget.harn)
and [`examples/ui_resource/review-form.harn`](https://github.com/burin-labs/harn/blob/main/examples/ui_resource/review-form.harn)
for end-to-end vanilla-JS examples.
