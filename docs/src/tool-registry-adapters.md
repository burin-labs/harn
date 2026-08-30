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

Individual flags override properties supplied through `--harn-input`. The
merged object is validated against the registry's complete JSON Schema before
the handler runs. Invalid required fields, enums, ranges, nested shapes, and
additional properties fail at the CLI boundary.

`cli.command` must be a non-empty list of command names. A tool with no `cli`
field defaults to `[namespace, name]` when `namespace` is present and `[name]`
otherwise. Dots in an implicit namespace or tool name become nested commands,
so `harn.code.search_examples` projects to `harn code search_examples`.
`cli.hidden: true` removes it from generated help but does not block explicit
invocation. Duplicate paths and leaf/parent path conflicts are errors.

## MCP projection

```bash
harn serve mcp server.harn
```

MCP `tools/list` receives the same name, description, input schema, output
schema, annotations, icons, execution metadata, and `_meta` values as the
canonical catalog. MCP-only wire fields are projections; they are not a second
declaration.

## Static catalog

```bash
harn tool schema server.harn --pretty
```

The result has `schema_version: "harn-tools/1.0"`, optional registry `info`,
and a `tools` list. Handlers and capability values are intentionally absent.
Registry `info` supplies the default MCP server name/version/instructions and
the generated CLI name/version/description. Explicit transport metadata wins.
Each tool retains:

- `name` plus optional `title` and `description`
- `inputSchema` and optional `outputSchema`
- MCP `annotations` and Harn execution `policy`
- `cli`, `namespace`, and `deferLoading`
- optional typed `source` coordinates
- `icons`, `execution`, and namespaced `_meta`

Use the catalog for documentation, language bindings, completion generation,
and compatibility checks. Use the live registry for execution.

## Metadata types

`std/tools` exports these closed presentation records:

- `ToolCliSpec`: `{command: list<string>, hidden?: bool}`
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

## Validation boundaries

Registry construction and adapter loading reject:

- malformed `cli`, `source`, output schema, icon, and metadata shapes
- invalid JSON Schemas and runtime-only values in serializable fields
- empty or invalid command parts
- duplicate command paths
- commands that are both a leaf tool and a parent group
- CLI flag collisions after underscore-to-hyphen normalization
- unresolved parameter `$ref` values at CLI construction
- non-JSON handler results at the CLI output boundary

This validation runs before handler execution. A missing registry publication
also fails even if an earlier in-process run published one.
