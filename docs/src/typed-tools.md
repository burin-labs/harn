# Typed tools for agent loops

`agent_loop(...)` does not need a bespoke host tool for every deterministic
operation. The fastest path is usually to wrap pure stdlib logic in a typed
tool and let the model call that tool directly.

This keeps the tool contract explicit:

- inputs are typed in the tool schema
- outputs are structured and replayable
- the implementation stays deterministic because it is ordinary Harn stdlib

## Pattern

Build a registry with `tool_define(...)`, give each tool a precise input and
output shape, and keep the handler body purely stdlib:

```harn
import "std/vision"

fn deterministic_tools() {
  var tools = tool_registry()

  tools = tool_define(tools, "math::calc", "Deterministic arithmetic", {
    parameters: {
      lhs: {type: "number"},
      rhs: {type: "number"},
      op: {type: "string", enum: ["add", "sub", "mul", "div"]},
    },
    returns: {type: "number"},
    handler: { args ->
      if args.op == "add" { return args.lhs + args.rhs }
      if args.op == "sub" { return args.lhs - args.rhs }
      if args.op == "mul" { return args.lhs * args.rhs }
      if args.op == "div" { return args.lhs / args.rhs }
      throw "unsupported op"
    },
  })

  tools = tool_define(tools, "regex::match", "Regex search over text", {
    parameters: {
      pattern: {type: "string"},
      text: {type: "string"},
    },
    returns: {type: "array", items: {type: "string"}},
    handler: { args -> return regex_match(args.pattern, args.text) ?? [] },
  })

  tools = tool_define(tools, "strings::count_char", "Count a single character", {
    parameters: {
      text: {type: "string"},
      char: {type: "string", minLength: 1, maxLength: 1},
    },
    returns: {type: "integer"},
    handler: { args ->
      require len(args.char) == 1, "char must be exactly one character"
      return split(args.text, args.char).count() - 1
    },
  })

  tools = tool_define(tools, "crypto::sha256", "Hash text as lowercase hex", {
    parameters: {
      text: {type: "string"},
    },
    returns: {type: "string"},
    handler: { args -> return sha256(args.text) },
  })

  tools = tool_define(tools, "vision::ocr", "Read text from an image", {
    parameters: {
      image: {
        description: "Path string or image dict accepted by std/vision.ocr",
      },
      options: {
        type: "object",
        properties: {
          language: {type: "string"},
        },
      },
    },
    returns: {
      type: "object",
      properties: {
        _type: {type: "string"},
        text: {type: "string"},
        blocks: {type: "array"},
        lines: {type: "array"},
        tokens: {type: "array"},
        source: {type: "object"},
        backend: {type: "object"},
        stats: {type: "object"},
      },
    },
    handler: { args -> return ocr(args.image, args.options) },
  })

  return tools
}
```

Then hand the registry to `agent_loop(...)`:

```harn
let result = agent_loop(
  "Read the screenshot, hash the extracted order id, and summarize the UI state.",
  "Use deterministic tools first. Prefer pure stdlib tools over free-form reasoning when possible.",
  {
    persistent: true,
    tools: deterministic_tools(),
    max_iterations: 12,
  }
)

println(result.text)
```

## Why this works

- `math::calc`, `regex::match`, `strings::count_char`, and `crypto::sha256`
  stay fully deterministic because they are just stdlib code.
- `vision::ocr` now returns `StructuredText` instead of an opaque blob, so the
  model gets token, line, and block structure back in the tool result.
- The current OCR backend shells out to `tesseract`, but the runtime keeps the
  backend pluggable and records the canonical OCR input plus structured output
  on `audit.vision_ocr` when an event log is active.

## Guidance

- Reach for typed stdlib tools before inventing a new MCP server or host bridge
  surface.
- Keep tool names product-facing and stable even if the handler body is simple.
- Make return schemas concrete enough that the model can branch on fields
  instead of scraping prose.
- When the result should be inspectable by later steps, return a dict or list,
  not a formatted string.
