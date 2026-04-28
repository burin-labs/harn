# Tutorial: Build an MCP server

This tutorial builds a small MCP server in Harn. The same program can expose
tools, static resources, resource templates, and prompts over stdio or
Streamable HTTP.

Use the companion example as a baseline:

```bash
cargo run --bin harn -- serve mcp examples/mcp_server.harn
```

## 1. Register tools

Start by creating a tool registry and attaching a few tools with explicit
schemas:

```harn
pipeline main(task) {
  var tools = tool_registry()

  tools = tool_define(tools, "greet", "Greet someone by name", {
    params: { name: "string" },
    handler: { args -> "Hello, " + args.name + "!" },
    annotations: {
      title: "Greeting Tool",
      readOnlyHint: true,
      destructiveHint: false,
    }
  })

  tools = tool_define(tools, "add", "Add two numbers", {
    params: { a: "number", b: "number" },
    handler: { args -> to_string(args.a + args.b) }
  })

  mcp_tools(tools)
}
```

Keep tool names short and descriptive. The description should be written for a
model, not for a human reading source code.

## 2. Add resources and templates

Resources are good for static content, while resource templates are better for
parameterized data.

```harn
pipeline main(task) {
  mcp_resource({
    uri: "docs://readme",
    name: "README",
    mime_type: "text/markdown",
    text: "# Harn MCP Demo\n\nThis server is implemented in Harn."
  })

  mcp_resource_template({
    uri_template: "config://{key}",
    name: "Configuration values",
    mime_type: "text/plain",
    handler: { args ->
      if args.key == "version" {
        "0.6.0"
      } else if args.key == "name" {
        "harn-demo"
      } else {
        "unknown key: " + args.key
      }
    }
  })
}
```

That pattern is useful for docs, policy data, generated summaries, and other
state you want to expose without writing a dedicated tool for each lookup.

## 3. Add prompts

Prompts let the client ask the server for structured guidance:

```harn
pipeline main(task) {
  mcp_prompt({
    name: "code_review",
    description: "Review code for correctness and maintainability",
    arguments: [
      { name: "code", description: "The code to review", required: true },
      { name: "language", description: "Programming language" }
    ],
    handler: { args ->
      let lang = args.language ?? "unknown"
      "Please review this " + lang + " code for correctness, bugs, and tests:\n\n" + args.code
    }
  })
}
```

Prompts are a good way to standardize a client workflow while still letting the
client supply the final payload.

## 4. Run it

Once the pipeline calls `mcp_tools()`, `mcp_resource()`, or `mcp_prompt()`,
launch the server with:

```bash
harn serve mcp examples/mcp_server.harn
```

`harn serve mcp` automatically detects whether the script defines its
surface through `pub fn` exports (the recommended path) or through the
`mcp_tools(...)` / `mcp_resource(...)` / `mcp_prompt(...)` registration
builtins shown above and serves the appropriate one over the requested
transport.
Use `--transport http` to expose the same MCP surface over Streamable HTTP.

All user-visible output goes to stderr; the MCP transport stays on stdout.
That keeps the server compatible with Claude Desktop, Cursor, and other MCP
clients.

## 5. Keep the surface small

A good MCP server has a narrow surface area:

- expose only the operations the client truly needs
- keep tool names and schemas stable
- prefer explicit resources over ad hoc text blobs
- use resource templates when one static resource is not enough

If you want the server to be consumable from a desktop client, add a short
launch snippet in the client config and test the tool list before expanding the
surface.
