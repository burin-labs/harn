# Governed code mode

Governed Code Mode is executable tool composition with a small API surface and
normal Harn audit. A model sees a binding manifest, writes a short Harn snippet
against those bindings, and Harn runs that snippet inside a read-only executor.
Every binding call is still a child tool operation with policy context,
`ToolAnnotations`, input, output, and failure category.

It is not "let the agent write arbitrary Harn and run it." The MVP executor
rejects imports, subprocesses, network calls, workspace writes, HITL calls,
parallel/spawn, and calls outside the injected manifest bindings plus pure data
helpers. Denied or approval-gated bindings either stay out of the default
manifest or fail closed with a child audit record when included for review.

## What is new

Ordinary `agent_loop` tool use asks the model to choose one tool call at a time.
Code Mode lets one reviewed parent operation do small read-only control flow:
loop over search results, read the top matches, filter JSON, sort findings, and
return one structured result. The child operations remain visible to replay,
policy, transcripts, and hosts.

`bundle` or batched read tools are still better when the operation shape is
fixed and already known. Code Mode is for ad hoc read-only composition over a
large or deferred tool surface.

`tool_synthesize` creates a deterministic callable tool from a natural-language
spec and pins that generated tool for the run. Code Mode does not synthesize a
new tool; it executes a per-run snippet against existing bindings.

Workflow patches and crystallization are the durable path. A useful repeated
snippet should be projected into crystallization input, shadow replayed, and
promoted as reviewed Harn workflow code. Code Mode is exploration and
composition; crystallization is promotion.

TypeScript/JavaScript is optional front-end sugar. Harn can emit `.d.ts`
declarations from the same binding manifest, but Harn-native execution and the
binding manifest remain the runtime authority until a separate sandbox proves
identical child events, policy behavior, and replay fidelity.

## Binding manifest

`composition_binding_manifest(tools, options?)` consumes ordinary Harn tool
registries, MCP `tools/list`-style objects, provider-native tool arrays, and
deferred tool entries:

```harn
pipeline main() {
  let tools = [
    {
      name: "read_file",
      description: "Read a workspace file",
      parameters: {type: "object", required: ["path"]},
      annotations: {
        kind: "read",
        side_effect_level: "read_only",
        arg_schema: {path_params: ["path"]},
        capabilities: {workspace: ["read_text"]},
      },
    },
  ]

  let manifest = composition_binding_manifest(tools)
  return manifest.bindings[0].binding
}
```

The full form includes schemas, side-effect level, capabilities, path argument
metadata, source (`harn`, `host_bridge`, `mcp_server`, `provider_native`, or
`deferred`), examples, and policy status. Use `{form: "compact"}` when a prompt
only needs names, descriptions, policy status, and examples. Use
`composition_harn_api(manifest)` to produce the typed `.harn` wrapper surface
the model should write against, or `composition_typescript_declarations(manifest)`
to produce declaration-only TypeScript affordances for editor/model ergonomics.

By default, bindings above the requested side-effect ceiling are omitted. Pass
`{include_denied: true}` only for diagnostics or audit examples where the model
should see why a binding is not callable.

## Read-only executor

`composition_execute(snippet, manifest, options?)` runs a Harn snippet inside a
generated `pipeline main()` with one wrapper function per manifest binding:

```harn
pipeline main() {
  let manifest = composition_binding_manifest([
    {
      name: "read_file",
      parameters: {type: "object", required: ["path"]},
      annotations: {kind: "read", side_effect_level: "read_only"},
      metadata: {mock_output: {text: "hello"}},
    },
  ])

  return composition_execute(
    "let file = read_file({path: \"README.md\"})\nreturn file.text",
    manifest,
    {run_id: "docs-code-mode", max_operations: 8, timeout_ms: 1000},
  )
}
```

The report contains:

- `run`: parent envelope with snippet hash, binding-manifest hash, result,
  stdout/stderr/artifacts, duration, and failure category.
- `child_calls`: ordered child binding invocations with raw input and policy
  context.
- `child_results`: ordered child statuses with raw output or error.

The current executor is intentionally read-only. It enforces child-call,
timeout, output-size, global concurrency, and per-MCP-server concurrency limits.
`map_bounded(items, { item -> ... }, {concurrency: N})` is available inside
snippets for settled fan-out; each item returns a `Result.Ok` or `Result.Err`
entry plus `succeeded` and `failed` counts, while child calls still pass through
the same audit and policy path.

MCP binding calls are wrapped with per-call timeout, retry, and bulkhead policy.
Retries are only automatic when a server is trusted and its MCP annotations mark
the tool as `readOnlyHint` or `idempotentHint`; destructive, non-idempotent, or
untrusted tools are not retried unless the call input carries an idempotency key
(`idempotency_key`, `idempotencyKey`, or `_meta.idempotencyKey`). Backoff honors
`Retry-After` on retryable 429/503-style failures and caps attempts and delay.
When a binding declares `outputSchema`, the executor validates the structured
child output before returning it to the snippet.

Pass `session_id` in the execute options when
the caller wants Harn to emit the corresponding `composition_start`,
`composition_child_call`, `composition_child_result`, and terminal composition
events to the live agent event sinks. Future frontends must route every binding
call through the same child dispatch path.

### Host dispatcher

By default, every binding call resolves through `metadata.mock_output` or a
structured echo, which is the right behavior for replay fixtures and offline
audits. Hosts that want real tool dispatch pass a `dispatcher` closure on the
options dict:

```harn
let dispatch = { binding_name, input ->
  if binding_name == "read_file" {
    return host_call("workspace.read_text", input)
  }
  return {error: "unknown binding " + binding_name}
}
let report = composition_execute(snippet, manifest, {dispatcher: dispatch})
```

The dispatcher receives `(binding_name: string, input: dict)` for each child
call and may return any value or raise a runtime error to fail the child. The
closure executes on a fresh clone of the outer VM, so host bridge builtins
(`host_call`, MCP, pipeline imports) resolve normally. The inner composition VM
still only sees manifest bindings plus the curated pure-helper list, so the
snippet itself cannot bypass policy by reaching for those builtins directly.

## MCP profile

For large connector packages, expose a compact Code Mode profile instead of
eagerly listing every endpoint:

```harn
import { composition_mcp_tools } from "std/composition"

pipeline main() {
  mcp_tools(composition_mcp_tools())
}
```

The profile registers:

- `harn.code.search_examples`: returns curated snippets and examples.
- `harn.code.generate_harn_api`: returns a manifest plus typed `.harn`
  wrapper declarations for the MCP tool surface.
- `harn.code.execute_composition`: executes a read-only snippet against the
  supplied binding manifest and returns a reduced result envelope.

Hybrid servers can expose ordinary tools plus the Code Mode profile by passing
an existing registry into `composition_mcp_tools(registry)`.

Inside an agent loop, `composition_mcp_api(tools, {query?, limit?})` builds the
same manifest/API pair from only MCP-served tools. The query path uses the same
BM25/regex/hybrid scorer as `tool_search`, so large MCP servers can stay
deferred until the agent asks for a slice:

```harn
import { composition_mcp_api, composition_mcp_execute } from "std/composition"

let api = composition_mcp_api(tool_registry, {query: "issues", limit: 5})
let output = composition_mcp_execute(
  "let hits = github_search_issues({query: \"is:open label:bug\"})\nreturn hits",
  tool_registry,
  {
    manifest: api.manifest,
    max_operations: 16,
    max_concurrent: 8,
    max_concurrent_per_server: 4,
    trusted_servers: api.servers,
    retry: {max_attempts: 3, base_delay_ms: 100, max_delay_ms: 2000},
  },
)
```

`composition_mcp_execute(...)` dispatches every child binding through the normal
agent tool dispatcher, not a side channel. That means MCP routing, schema
validation, policy checks, approval hooks, call budgets, allowlists, and circuit
breakers stay attached to the individual child calls. By default the helper and
profile tool return only `{ok, result, error, failure_category, run_id,
child_call_count}` so large MCP responses do not leak back to the model after
the snippet has reduced them. Pass `{include_report: true}` only for diagnostics
that need the full child audit report.

## Crystallization

`composition_crystallization_trace(report, options?)` and the stdlib alias
`composition_crystallization_input(report, options?)` convert a successful
composition report into the same trace shape consumed by `harn crystallize`.
The trace keeps the source composition run, snippet hash, manifest hash, child
operation sequence, capabilities, inputs, outputs, and policy context. Repeated
read-only snippets can then be mined, shadow checked, and promoted through the
normal review workflow.

## Choosing a surface

Use ordinary tools when one call is enough and the model should decide each next
step. Use a bundle or batched read tool when the operation is fixed. Use Code
Mode when a read-only task needs small control flow over a manifest. Use
crystallization when the same Code Mode pattern recurs and should become
durable reviewed workflow code.
