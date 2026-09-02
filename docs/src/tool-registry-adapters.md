# Tool registry adapter reference

`ToolRegistry` is Harn's executable integration contract. A registry owns each
operation's name, description, input and output schemas, handler, safety hints,
and presentation metadata. Harn projects that one value into MCP, a nested CLI,
and the versioned `harn-tools/2.0` catalog.

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
      returns: {"$ref": "#/components/schemas/Widget"},
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
      cli: {
        command: ["widgets", "get"],
        aliases: ["show"],
        arguments: {
          widget_id: {
            position: 0,
            value_name: "WIDGET_ID",
            help: "Numeric widget id",
          },
          verbose: {short: "v", help_group: "Display"},
        },
      },
      source: {
        kind: "openapi",
        id: "getWidget",
        binding: {method: "GET", path: "/widgets/{widget_id}"},
      },
      handler: {args -> {id: args.widget_id, label: "example"}},
    },
  ], {
    info: {
      name: "widgets",
      version: "1.0.0",
      description: "Widget integration",
    },
    components: {
      schemas: {
        Widget: {
          type: "object",
          properties: {id: {type: "integer"}, label: {type: "string"}},
          required: ["id", "label"],
          additionalProperties: false,
        },
      },
    },
    cli: {commands: [{
      command: ["widgets"],
      title: "Manage widgets",
      aliases: ["w"],
    }]},
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
harn tool run server.harn widgets get 42 -v false # harn-doc-cli: allow-stale
harn tool run server.harn widgets get --help
harn tool completions server.harn --shell zsh > _widgets
```

Each input property becomes a long flag by default. Underscores become hyphens
in the flag spelling and retain their original names in the handler argument
object. `cli.arguments` can make a property positional, change its long flag,
add one short flag and long aliases, group and order help, mark an array as a
repeatable flag, or attach a portable completion hint. These settings change
presentation only; the input JSON Schema remains the validation owner.
Harn coerces `string`, `integer`, `number`, and `boolean` values. Pass JSON for
`object`, `array`, union, or untyped properties.

Boolean inputs use the explicit `value` style by default, so
`--verbose false` remains distinct from omission. Set `boolean_style` to
`set_true` or `set_false` for a presence option such as `--verbose` or
`--no-cache`. Presence options insert a value only when the token occurs; they
never synthesize a property when omitted. The metadata must choose the spelling
instead of deriving English negation rules.

Numeric options accept hyphen-leading values such as `--offset -1`.
`completions` supplies ordered suggestions without restricting input; string
`enum` values contribute schema-owned suggestions and remain enforced by the
prepared schema validator. A repeated positional must be last, and an optional
positional cannot precede a required positional because neither shape has a
portable token interpretation.

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

`cli.command` must be a non-empty list of command names. `cli.aliases` gives the
leaf command alternate spellings under the same parent. A tool with no `cli`
field defaults to `[namespace, name]` when `namespace` is present and `[name]`
otherwise. Dots in an implicit namespace or tool name become nested commands,
so `harn.code.search_examples` projects to `harn code search_examples`.
`cli.hidden: true` removes it from generated help but does not block explicit
invocation. Duplicate paths and leaf/parent path conflicts are errors.

Catalog-level `cli.commands` supplies metadata for parent commands. A parent
can set `title`, `description`, aliases, visibility, and display order without
duplicating a leaf tool schema. `harn tool completions` emits Bash, Zsh, Fish,
or PowerShell command and option completion from this same prepared tree. Bash,
Zsh, and Fish also receive value-hint behavior plus static enum and advisory
value candidates. The upstream PowerShell ahead-of-time generator currently
omits argument-value candidates; parsing and schema validation remain identical
on PowerShell.

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

Harn compiles each standalone input, output, and application-error validator
once when it prepares the registry. Plain `tools/call`, completed MCP tasks,
generated CLI calls, and
export-backed adapters all validate against that prepared catalog. Invalid
arguments never reach the handler. Invalid or non-JSON results become tool
failures and are never stored or reported as successful task results.

Set `error_schema` on a handwritten definition to declare the portable shape
of values its handler deliberately throws. Public Harn functions project their
language-level `throws E` type to the same catalog field. A matching raw throw
is typed application data; an undeclared throw remains a runtime failure, and
a declared throw that does not match is a contract failure. Harn never treats
`Result<T, E>` as a throw declaration. Runtime summaries classify undeclared
throws without rendering their values, so a legacy string or object cannot
leak through CLI stderr, MCP content, HTTP messages, A2A history, or trust
records. A declared `throws` type that cannot be represented as Draft 2020-12
JSON Schema prevents catalog publication instead of silently dropping the
error contract.

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
`schema_version: "harn-tools/2.0"`, optional registry `info`, optional parent
command metadata under `cli.commands`, a `tools` list, and optional reusable
schemas under `components.schemas`. Handlers and capability values are
intentionally absent.
Registry `info` supplies the default MCP server name/version/instructions and
the generated CLI name/version/description. Explicit transport metadata wins.
Each tool retains:

- `name` plus optional `title` and `description`
- `inputSchema`, optional `outputSchema`, and optional `errorSchema`
- MCP `annotations` and Harn execution `policy`
- adapter `governance`
- `cli`, `namespace`, and `deferLoading`
- optional typed `source` coordinates
- `icons`, `execution`, and namespaced `_meta`

Use the catalog for documentation, language bindings, completion generation,
and compatibility checks. Use the live registry for execution.

The generated contract files are:

- `spec/protocol-artifacts/schemas/harn-tools-v2.schema.json` for structural
  envelope validation and language generators;
- `spec/protocol-artifacts/harn-tools.ts` for strict TypeScript consumers.

Run `make check-protocol-artifacts` to detect drift between those files and the
runtime contract. The artifact manifest records their paths and catalog
version.

Version 2 is the sole emitted and accepted catalog version. Version 1 readers
reject the new `errorSchema` field, and version 2 readers reject version 1's
discriminator. Regenerate schemas and language bindings atomically with the
catalog producer.

The generated schema checks required fields, closed owned records, primitive
types, and enum values. It treats embedded JSON Schema documents as open
objects. Consumers that accept catalogs for execution must also deserialize
them through Harn's `ToolCatalog` parser. That second stage checks Draft
2020-12 semantics, local and component reference closure, command uniqueness,
and other cross-entry invariants.

## Metadata types

`std/tools` exports these closed presentation records:

- `ToolRegistryOptions`: `{info?, components?, cli?}`
- `ToolCliTreeSpec`: `{commands: list<ToolCliCommandSpec>}`
- `ToolCliCommandSpec`: parent path, copy, aliases, visibility, and order
- `ToolCliSpec`: `{command, aliases?, hidden?, arguments?}` for one leaf tool
- `ToolCliArgumentSpec`: token mapping, boolean style, help visibility/order,
  repeatability, advisory completions, and value hint
- `ToolCliBooleanStyle`: `value`, `set_true`, or `set_false`
- `ToolCliValueHint`: `file`, `directory`, `path`, `url`, `email`, `username`,
  `hostname`, `command`, or `other` (disable shell-default value completion)
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
in typed registry fields rather than `_meta`. The reverse-DNS key
`_meta["com.harnlang/toolContract"]` is reserved for Harn's generated MCP
adapter projection.
Other catalog `_meta` keys must follow MCP's metadata-key grammar. Prefer a
reverse-DNS vendor prefix such as `com.example/toolVersion`; prefixes whose
second label is `mcp` or `modelcontextprotocol` are reserved by MCP.

All other owned records reject unknown fields. JSON Schema values remain open
because their vocabulary evolves independently. They must be valid Draft
2020-12 documents. Use `input_schema` for a complete object-root input schema;
`parameters` remains the legacy per-parameter shorthand, whose nested `schema`
field accepts either a boolean schema or a schema object. Output, error, and
component positions also accept both standard forms. `components.schemas`
holds named reusable schemas, and catalog input, output, or error schemas can
reference them with `#/components/schemas/<name>`.

## Declared failures

The generated CLI keeps successful output unchanged. A declared application
error exits nonzero. JSON and pretty output write this stable envelope to
stdout:

```json
{"ok":false,"error":{"kind":"application","tool":"lookup","data":{"variant":"NotFound"}}}
```

Text output writes only a generic summary to stderr. It does not read or
stringify the complete error object.

MCP discovery carries the standalone error schema at
`tools[]._meta["com.harnlang/toolContract"].errorSchema`. A matching failure is a normal
`CallToolResult`, not a JSON-RPC error:

```json
{
  "content": [{"type":"text","text":"tool \"lookup\" failed: declared application error"}],
  "isError": true,
  "_meta": {
    "com.harnlang/toolContract": {
      "applicationError": {
        "tool": "lookup",
        "data": {"variant":"NotFound"}
      }
    }
  }
}
```

The same result shape is stored for MCP task completion. The task status is
`completed`: `isError` describes the tool result, while task `failed` is
reserved for failure to produce a result, as required by
[SEP-2663](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/seps/2663-tasks-extension.md).
Invalid input remains a JSON-RPC invalid-params error. Runtime and contract
failures remain untyped tool failures and never acquire application-error
metadata.

The A2A adapter stores the same `{tool, data}` record at
`task.metadata.harn.applicationError` and terminates the task as `failed`.
Task history contains only the generic application-error summary, never error
fields.

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
- sparse or duplicate positional indexes
- CLI command, long-flag, alias, short-flag, and framework-reserved collisions
- repeatable arguments whose schema is not one array with an item schema
- non-terminal repeated positionals and required positionals after optional ones
- boolean presence styles on non-boolean, positional, or repeatable arguments
- empty or duplicate advisory completion candidates
- argument metadata that names no input-schema property
- unresolved parameter `$ref` values at CLI construction
- non-JSON handler results at the CLI output boundary
- call arguments and handler results that violate the prepared catalog on CLI,
  MCP, task, and export-backed dispatch paths

Input validation runs before handler execution. Output validation runs before
success receipts, replay/cache writes, task completion, or transport shaping.
A missing registry publication also fails even if an earlier in-process run
published one.
