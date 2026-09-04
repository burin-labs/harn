# HARN-LNT-076 — tool parameters spelled as a JSON Schema document

One key named `parameters` means two different things depending on which
function receives the descriptor. The tool registry reads every key of
`parameters` as a parameter name. The composition and agent-loop descriptor
paths read the same key as a complete JSON Schema.

A descriptor written for one and handed to the other is wrong in both
directions, and the silent direction is the dangerous one. Under the registry's
rule, `{type: "object", properties: {}}` is not a tool without parameters. It is
a tool with two parameters, named `type` and `properties`, and nothing reports
it: a consumer shipped exactly that. The other direction at least throws,
because a list-valued `required` is not a parameter definition.

This rule reports a `parameters` map whose top-level keys are drawn only from
`type`, `properties` and `required`, because a map like that is describing a
schema rather than naming parameters.

## How to fix

Spell a complete schema as `inputSchema`, which every descriptor reader already
accepts:

```harn
const tools = [
  {
    name: "read_file",
    inputSchema: {type: "object", required: ["path"]},
  },
]
```

Keep `parameters` for the per-parameter map, where each key is a parameter name:

```harn
tools = tool_define(tools, "read_file", "Read one file", {
  parameters: {path: {type: "string", required: true}},
  handler: read_file_handler,
})
```

## Severity

This reports as an error. The runtime refuses the same shape when a registry is
built, so a descriptor this rule accepts and a registry the runtime accepts
agree by construction.
