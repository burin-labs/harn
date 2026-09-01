import type {
  SideEffectLevel,
  ToolAudience,
  ToolCatalog,
  ToolKind,
  ToolTaskSupport,
} from "../../spec/protocol-artifacts/harn-tools";

const audience: ToolAudience = "mcp";
const kind: ToolKind = "fetch";
const sideEffect: SideEffectLevel = "network";
const taskSupport: ToolTaskSupport = "optional";

const catalog: ToolCatalog = {
  schema_version: "harn-tools/1.0",
  info: {
    name: "widgets",
    version: "1.0.0",
    description: "Widget integration",
  },
  tools: [
    {
      name: "get_widget",
      title: "Get widget",
      description: "Fetch one widget by id.",
      inputSchema: {
        type: "object",
        properties: { widget_id: { type: "integer" } },
        required: ["widget_id"],
        additionalProperties: false,
      },
      outputSchema: {
        type: "object",
        properties: { id: { type: "integer" } },
        required: ["id"],
        additionalProperties: false,
      },
      annotations: {
        title: "Get widget",
        readOnlyHint: true,
        destructiveHint: false,
        idempotentHint: true,
        openWorldHint: true,
      },
      icons: [
        {
          src: "https://example.com/widget.svg",
          mimeType: "image/svg+xml",
          sizes: ["any"],
          theme: "light",
        },
      ],
      execution: { taskSupport },
      governance: { audiences: ["cli", audience, "catalog"] },
      cli: { command: ["widgets", "get"], hidden: false, arguments: {} },
      namespace: "widgets",
      deferLoading: false,
      source: {
        kind: "openapi",
        id: "getWidget",
        binding: { method: "GET", path: "/widgets/{widget_id}" },
      },
      policy: { kind, side_effect_level: sideEffect },
      _meta: { "example.com/catalog-version": 1 },
    },
  ],
  components: {
    schemas: {
      Widget: {
        type: "object",
        properties: { id: { type: "integer" } },
        required: ["id"],
      },
    },
  },
};

const tool = catalog.tools[0];
if (
  tool === undefined ||
  catalog.schema_version !== "harn-tools/1.0" ||
  catalog.tools.length !== 1 ||
  tool.name !== "get_widget" ||
  tool.governance.audiences[1] !== audience ||
  tool.execution?.taskSupport !== taskSupport ||
  tool.policy?.kind !== kind ||
  tool.policy.side_effect_level !== sideEffect ||
  tool.cli.command.join(" ") !== "widgets get" ||
  catalog.components?.schemas.Widget === undefined
) {
  throw new Error("generated harn-tools binding lost the typed catalog fixture");
}
