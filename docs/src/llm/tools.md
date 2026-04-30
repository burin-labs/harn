# LLM tools

## Typed tools for agent loops

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

```harn,ignore
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

## Tool Vault

Harn's Tool Vault is the progressive-tool-disclosure primitive: tool
definitions that stay out of the model's context until they're
surfaced by a search call. This keeps context cheap for agents with
hundreds of tools (coding agents, MCP-heavy setups) without requiring
the integrator to hand-filter tools per turn.

### Per-tool flag: `defer_loading`

Any tool registered via `tool_define` (or the `tool { … }` language
form) can opt out of eager loading:

```harn
var registry = tool_registry()
registry = tool_define(registry, "deploy", "Deploy to production", {
  parameters: {env: {type: "string"}},
  defer_loading: true,
  handler: { args -> shell("deploy " + args.env) },
})
```

Deferred tools never appear in the model's context unless a
tool-search call surfaces them. They *are* sent to the provider (so
prompt caching stays warm on Anthropic — the schemas live in the
API prefix but not the model's context).

### Call-level option: `tool_search`

Turning progressive disclosure on is one option away:

```harn
let r = llm_call(prompt, sys, {
  provider: "anthropic",
  model: "claude-opus-4-7",
  tools: registry,
  tool_search: "bm25",
})
```

Accepted shapes:

| Shape | Meaning |
|---|---|
| `tool_search: true` | Default: `bm25` variant, mode `auto`. |
| `tool_search: "bm25"` | Natural-language queries. |
| `tool_search: "regex"` | Python-regex queries. |
| `tool_search: false` | Explicit off (same as omitting). |
| `tool_search: {variant, mode, strategy, always_loaded, budget_tokens, name, include_stub_listing}` | Explicit dict form. |

`mode` options:

- `"auto"` (default) — use native if the provider supports it,
  otherwise fall back to the client-executed path (no error).
- `"native"` — force the provider's native mechanism. Errors if
  unsupported.
- `"client"` — force the client-executed path even on providers with
  native support. Useful for A/B-ing strategies or pinning behavior
  across heterogeneous provider fleets.

### Provider support

| Provider | Native `tool_search` | Variants / modes |
|---|---|---|
| Anthropic Claude Opus/Sonnet 4.0+, Haiku 4.5+ | ✓ | `bm25`, `regex` |
| Anthropic 3.x or earlier 4.x Haiku | ✗ (uses client fallback) | — |
| OpenAI Responses API — GPT 5.4+ | ✓ | `hosted` (default), `client` |
| OpenAI pre-5.4 (`gpt-4o`, `gpt-4.1`, …) | ✗ | client fallback works today |
| OpenRouter / Together / Groq / DeepSeek / Fireworks / HuggingFace / local | ✓ when routed model matches `gpt-5.4+` upstream | hosted forwarded; escape hatch below for proxies |
| Gemini, Ollama, mock (default model) | ✗ | client fallback works today |

The OpenAI native path emits a flat `{"type": "tool_search",
"mode": "hosted"}` meta-tool at the front of the tools array, alongside
`defer_loading: true` on the wrapper of each user tool. The server runs
the search and replies with `tool_search_call` / `tool_search_output`
entries that Harn parses into the same transcript event shape as the
Anthropic path (replays are indistinguishable across providers).

#### Namespace grouping

OpenAI's `tool_search` can group deferred tools into namespaces; pass
`namespace: "<label>"` on `tool_define(...)` to tag a tool. Harn collects
the distinct set into the meta-tool's `namespaces` field. Anthropic
ignores the label — harmless passthrough for replay fidelity.

```harn
tool_define(registry, "deploy_api", "Deploy the API", {
  parameters: {env: {type: "string"}},
  defer_loading: true,
  namespace: "ops",
  handler: { args -> shell("deploy api " + args.env) },
})
```

#### Escape hatch for proxied OpenAI-compat endpoints

Self-hosted routers and enterprise gateways sometimes advertise a model
ID Harn cannot parse (`my-internal-gpt-clone-v2`) yet forward the OpenAI
Responses payload unchanged. Opt into the hosted path with:

```harn
llm_call(prompt, sys, {
  provider: "openrouter",
  model: "my-custom/gpt-forward",
  tools: registry,
  tool_search: {mode: "native"},
  openrouter: {force_native_tool_search: true},
})
```

The override is keyed by the provider name (the same dict you'd use for
any provider-specific knob).

For the authoritative capability table, `harn.toml` override schema, and
packaged provider adapter config, see
[LLM providers](./providers.md#capability-matrix--harntoml-overrides).

### Client-executed fallback

On providers without native `defer_loading`, Harn falls back to an
in-VM execution path.
The fallback is identical to the native path from a script's point of
view: same option surface, same transcript events, same promotion
behavior across turns. Internally, Harn injects a synthetic tool
called `__harn_tool_search` — when the model calls it, the loop runs
the configured strategy against the deferred-tool index, promotes the
matching tools into the *next* turn's schema list, and emits the
same `tool_search_query` / `tool_search_result` transcript events as
native mode (tagged `mode: "client"` in metadata so replays can
distinguish paths).

Strategies (client mode only):

| `strategy` | Runs in | Notes |
|---|---|---|
| `"bm25"` *(default)* | VM | Tokenized BM25 over `name + description + param text`. Matches `open_file` from query `open file`. |
| `"regex"` | VM | Case-insensitive Rust-regex over the same corpus. No backreferences, no lookaround. |
| `"semantic"` | Host (bridge) | Delegated to the host via `tool_search/query` so integrators can wire embeddings without Harn pulling in ML crates. |
| `"host"` | Host (bridge) | Pure host-side; the VM round-trips the query and promotes whatever the host returns. |

Extra client-mode knobs:

- `budget_tokens: N` — soft cap on the total token footprint of
  promoted tool schemas. Oldest-first eviction when exceeded. Omit to
  keep every promoted schema for the life of the call.
- `name: "find_tool"` — override the synthetic tool's name. Handy
  when a skill's vocabulary suggests a more natural verb (`discover`,
  `lookup`, …).
- `always_loaded: ["read_file", "run"]` — pin tool names to the eager
  set even if `defer_loading: true` is set on their registry entries.
- `include_stub_listing: true` — append a short list of deferred tool
  names + one-line descriptions to the tool-contract prompt so the
  model can eyeball what's available without a search call. Off by
  default to match Anthropic's native ergonomic.

### Pre-flight validation

- At least one user tool must be non-deferred. Harn errors before the
  API call is made, matching Anthropic's documented 400.
- `defer_loading` must be a bool — typos like `defer_loading: "yes"`
  error at `tool_define` time rather than silently falling back to
  the "no defer" default.

### Transcript events

Every native tool-search round-trip emits structured events in the
run record:

- `tool_search_query` — the search tool's invocation (input query,
  search-tool id).
- `tool_search_result` — the references returned by the server (which
  deferred tools got promoted on this turn).

These are stable shapes; replay / eval can reconstruct which tools
were available when without re-running the call.

## MCP server tools

Use `mcp_servers` when an agent should use an MCP server's tool catalog without
manually calling `mcp_connect`, `mcp_list_tools`, and `mcp_call`.

```harn
let result = agent_loop(
  "Summarize the latest open issue and draft a reply.",
  "You are a concise triage assistant.",
  {
    provider: "openai",
    model: "gpt-5.4",
    mcp_servers: [
      {name: "github", transport: "http", url: "http://localhost:3030/mcp"},
      {name: "local_fs", transport: "stdio", command: ["mcp-filesystem", "/tmp/project"]},
    ],
    max_iterations: 8,
  },
)
```

Discovered tools are always prefixed with the server name, for example
`github__search_issues` or `local_fs__read_file`. The prefix makes collisions
deterministic when two servers both export a tool named `search` or when a
server tool would otherwise overlap a local Harn tool. The actual MCP
`tools/call` request still uses the original unprefixed MCP tool name.
