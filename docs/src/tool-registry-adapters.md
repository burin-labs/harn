# Tool registry adapter reference

`ToolRegistry` is Harn's executable integration contract. A registry owns each
operation's name, description, input and output schemas, handler, safety hints,
and presentation metadata. Harn projects that one value into MCP, a nested CLI,
and the versioned `harn-tools/1.0` catalog.

Do not maintain a separate MCP list, CLI dispatch table, or static operation
catalog. Generate or author one registry and choose presentation adapters at
deployment time.

## Registry declaration

```harn
import { ToolRegistry, tool_registry_from } from "std/tools"

fn widget_tools() -> ToolRegistry {
  return tool_registry_from([
    {
      name: "lookup_widget",
      description: "Fetch one widget by numeric id.",
      parameters: {
        widget_id: {schema: {type: "integer"}, required: true},
        verbose: {schema: {type: "boolean"}, required: false},
      },
      returns: {
        type: "object",
        properties: {id: {type: "integer"}, label: {type: "string"}},
        required: ["id", "label"],
        additionalProperties: false,
      },
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        idempotentHint: true,
        openWorldHint: true,
      },
      execution_policy: {kind: "fetch", side_effect_level: "network"},
      governance: {
        audiences: ["cli", "mcp", "catalog", "dashboard", "agent"],
      },
      cli: {command: ["widgets", "get"]},
      source: {
        kind: "openapi",
        id: "getWidget",
        binding: {method: "GET", path: "/widgets/{widget_id}"},
      },
      handler: {args -> {id: args.widget_id, label: "example"}},
    },
  ], {
    name: "widgets",
    version: "1.0.0",
    description: "Widget integration",
  })
}

fn main(harness: Harness) {
  harness.tools.mcp_tools(widget_tools())
}
```

The publishing call makes the registry available to script-backed adapters. It
does not make MCP the semantic owner. `harn serve mcp` and `harn tool run` load
the same published registry and invoke the same closures in the same VM.

The registry's `annotations` record also carries Harn runtime metadata. The
portable catalog selects only `title` and the four MCP presentation hints;
lifecycle, inline-result, and artifact-emission annotations remain runtime-only.
Legacy `kind` and `side_effect_level` values normalize into the typed `policy`
field rather than presentation metadata.

## CLI projection

```bash
harn tool run server.harn widgets get --widget-id 42 --verbose false # harn-doc-cli: allow-stale
harn tool run server.harn widgets get --help
```

Each input property becomes a long flag. Underscores become hyphens in the
flag spelling and retain their original names in the handler argument object.
Harn coerces `string`, `integer`, `number`, and `boolean` values. Pass JSON for
`object`, `array`, union, or untyped properties.

Every leaf also accepts:

- `--harn-input <JSON>` for a base argument object
- `--harn-input @path.json` to read that object from a file
- `--harn-input -` to read it from standard input
- `--harn-output json|pretty|text` to select output encoding
- `--json` as an explicit alias for compact JSON output when the tool does not
  declare a `json` input property

Individual flags override properties supplied through `--harn-input`. The
merged object is validated against the registry's complete JSON Schema before
the handler runs. Invalid required fields, enums, ranges, nested shapes, and
additional properties fail at the CLI boundary.

A tool with a `json` input keeps `--json` for that input. Use
`--harn-output json` to select compact output in that case.

`cli.command` must be a non-empty list of command names. A tool with no `cli`
field defaults to `[namespace, name]` when `namespace` is present and `[name]`
otherwise. Dots in an implicit namespace or tool name become nested commands,
so `harn.code.search_examples` projects to `harn code search_examples`.
`cli.hidden: true` removes it from generated help but does not block explicit
invocation. Duplicate paths and leaf/parent path conflicts are errors.

## MCP projection

```bash
harn serve mcp server.harn
harn serve mcp --surface script --watch server.harn
harn serve mcp --surface exports library.harn
```

MCP `tools/list` preserves the catalog's identity and presentation metadata.
Its input schema, output schema, and structured result are standalone semantic
projections of the catalog contract. MCP-only wire fields are projections;
they are not a second declaration.

MCP schemas are semantic standalone projections, not byte-for-byte catalog
copies. Harn follows every reachable `#/components/schemas/...` reference,
places those schemas under the tool's `$defs`, and rebases local references.
If an output schema doesn't guarantee an object, MCP advertises an object
schema with one required `result` property. The matching
`structuredContent` value is `{result: <handler-result>}`. Object outputs keep
their original shape.

The portable catalog accepts Draft 2020-12 schemas, including features that
cannot yet move safely between JSON Schema resources. When a catalog has
components, MCP projection rejects `$id`, `$anchor`, `$dynamicAnchor`,
`$dynamicRef`, and external `$ref` values that could change meaning after
bundling. Server preparation fails instead of publishing a misleading schema.
Resource-aware bundling can add those cases later without weakening the
portable catalog.

Plain `tools/call` responses and completed MCP tasks use the same
`CallToolResult` projection. Their `content`, `structuredContent`, and
`isError` fields therefore follow one result-shaping path.

The `exports` surface derives its MCP tools from public Harn functions through
the same canonical catalog entries as `harn tool schema --surface exports`.
The `script` surface reads an explicitly published `ToolRegistry`. `auto`
selects exports when the module has public functions and otherwise loads the
script surface.

With `--watch`, Harn validates a complete replacement registry and VM before
swapping the live stdio server. Successful reloads keep the client connection
open and emit the standard tool, resource, and prompt list-change
notifications. Compilation, initialization, and registry-validation failures
leave the previous handlers live.

## Static catalog

```bash
harn tool schema server.harn --pretty
harn tool schema server.harn --surface script --pretty
harn tool schema library.harn --surface exports --pretty
```

`--surface script` runs the script and reads its published registry. It is the
default for compatibility. `--surface exports` compiles the module and derives
entries from its public functions without running `main`, invoking handlers,
acquiring capabilities, or contacting connectors. Use the exports surface for
offline SDK generation and compatibility checks.

Both surfaces return the same catalog shape. The result has
`schema_version: "harn-tools/1.0"`, optional registry `info`, a `tools` list,
and optional reusable schemas under `components.schemas`. Handlers and
capability values are intentionally absent.
Registry `info` supplies the default MCP server name/version/instructions and
the generated CLI name/version/description. Explicit transport metadata wins.
Each tool retains:

- `name` plus optional `title` and `description`
- `inputSchema` and optional `outputSchema`
- MCP `annotations` and Harn execution `policy`
- adapter `governance`
- `cli`, `namespace`, and `deferLoading`
- optional typed `source` coordinates
- `icons`, `execution`, and namespaced `_meta`

Use the catalog for documentation, language bindings, completion generation,
and compatibility checks. Use the live registry for execution.

The generated contract files are:

- `spec/protocol-artifacts/schemas/harn-tools-v1.schema.json` for structural
  envelope validation and language generators;
- `spec/protocol-artifacts/harn-tools.ts` for strict TypeScript consumers.

Run `make check-protocol-artifacts` to detect drift between those files and the
runtime contract. The artifact manifest records their paths and catalog
version.

The generated schema checks required fields, closed owned records, primitive
types, and enum values. It treats embedded JSON Schema documents as open
objects. Consumers that accept catalogs for execution must also deserialize
them through Harn's `ToolCatalog` parser. That second stage checks Draft
2020-12 semantics, local and component reference closure, command uniqueness,
and other cross-entry invariants.

## Metadata types

`std/tools` exports these closed presentation records:

- `ToolCliSpec`: `{command: list<string>, hidden?: bool}`
- `ToolAudience`: `"cli" | "mcp" | "catalog" | "dashboard" | "agent"`
- `ToolGovernance`: `{audiences: list<ToolAudience>}`
- `ToolSource`: `{kind: string, id?: string, binding?: dict}`
- `ToolPolicy`: canonical `kind` and `side_effect_level` literals
- `ToolExecution`: `{taskSupport: "forbidden" | "optional" | "required"}`
- `ToolIcon`: `{src, mimeType?, sizes?, theme?}`

Set a tool definition's typed classification with `execution_policy`. The
static catalog exposes that classification as `policy`. The older `policy`
definition field remains the runtime's routing and argument policy and is not
mistaken for an execution classification.

`source.binding` is the protocol-specific escape hatch. `_meta` remains the
namespaced extension point for presentation protocols. Put behavior and policy
in typed registry fields rather than `_meta`.

All other owned records reject unknown fields. JSON Schema values remain open
because their vocabulary evolves independently. They must be valid Draft
2020-12 documents. `components.schemas` holds named reusable schemas, and
catalog input or output schemas can reference them with
`#/components/schemas/<name>`.

## Adapter governance

`governance.audiences` is the closed adapter exposure contract for one tool.
Its allowed values are `cli`, `mcp`, `catalog`, `dashboard`, and `agent`.
`agent` is the model-facing agent-loop projection. Each adapter filters
discovery and invocation from the same normalized registry entry, so a tool
omitted from MCP cannot be called by sending its name directly to `tools/call`,
and a tool omitted from `agent` cannot be recovered by a forged model tool
call. The generated CLI applies the same rule before building its command tree.

The list must be non-empty and contains no duplicates. Unknown audiences and
unknown governance fields fail registry registration. Harn sorts the list into
canonical order in the static catalog, which lets generators and dashboards
consume the policy without copying it. Registries that omit `governance`
remain visible to all five adapters for compatibility.

Use a narrow list for operator-only projections. For example,
`{audiences: ["cli", "catalog"]}` keeps a command available to local operators
and schema generators without advertising or accepting it over MCP.

`tool_project(registry, audience)` returns the executable projection while
preserving its handler closures and outer registry metadata. Adapter and agent
infrastructure should project once at its input boundary, then pass only that
value to discovery, prompt, search, narrowing, and invocation consumers. The
agent lifecycle composer owns the closure-preserving `agent` projection before
it derives model controls; `agent_loop` delegates to that seam. The direct
`harness.llm.call(...)` option normalizer applies the same projection before
model-facing prompt guidance or tool schemas are assembled.

## Validation boundaries

Registry construction and adapter loading reject:

- malformed `cli`, `source`, output schema, icon, and metadata shapes
- empty, duplicate, or unknown `governance.audiences` values and unknown
  governance fields
- invalid JSON Schemas and runtime-only values in serializable fields
- empty or invalid command parts
- duplicate command paths
- commands that are both a leaf tool and a parent group
- CLI flag collisions after underscore-to-hyphen normalization
- unresolved parameter `$ref` values at CLI construction
- non-JSON handler results at the CLI output boundary

This validation runs before handler execution. A missing registry publication
also fails even if an earlier in-process run published one.
