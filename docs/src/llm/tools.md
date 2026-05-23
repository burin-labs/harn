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
output shape, and keep the handler body purely stdlib. For repeated
declarative specs, import `tool_define_many(...)` or `tool_registry_from(...)`
from `std/tools`.

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
    loop_until_done: true,
    tools: deterministic_tools(),
    max_iterations: 12,
  }
)

log(result.text)
```

## Why this works

- `math::calc`, `regex::match`, `strings::count_char`, and `crypto::sha256`
  stay fully deterministic because they are just stdlib code.
- `vision::ocr` now returns `StructuredText` instead of an opaque blob, so the
  model gets token, line, and block structure back in the tool result.
- The current OCR backend shells out to `tesseract`, but the runtime keeps the
  backend pluggable, runs it through the process sandbox under an active handler
  policy, and records OCR metadata plus structured output on `audit.vision_ocr`
  when an event log is active.

## Guidance

- Reach for typed stdlib tools before inventing a new MCP server or host bridge
  surface.
- Keep tool names product-facing and stable even if the handler body is simple.
- Make return schemas concrete enough that the model can branch on fields
  instead of scraping prose.
- When the result should be inspectable by later steps, return a dict or list,
  not a formatted string.

## Host-backed tool helpers

Harnesses that need filesystem, git, search, or command execution tools can use
the packaged hostlib wrappers instead of hand-rolling tool registries. Import
`std/agent/host_tools` and choose the narrowest helper:

```harn
import { agent_command_tools, agent_host_tools } from "std/agent/host_tools"

let tools = agent_command_tools(nil, {
  cwd: repo_root,
  max_inline_bytes: 12000,
  command_behavior: {
    background_after_ms: 750,
    progress_interval_ms: 3000,
    progress_max_inline_bytes: 1600,
  },
  allow_argv_prefixes: [
    ["git", "status"],
    ["git", "log"],
    ["git", "diff"],
    ["sed", "-n"],
  ],
})

agent_loop(task, system, {
  tools: tools,
  tool_format: "native",
  require_successful_tools: ["run_command", ["read_command_output", "read_command_output_tail"]],
})
```

For git-only workflows, prefer `std/git.git_tools(...)` when the model should
see individual local git operations instead of a generic command runner or the
coarse `git_inspect` operation switch:

```harn,ignore
import { git_tools } from "std/git"

let tools = git_tools(nil, {
  repo: repo_root,
  enabled_tools: ["git_status", "git_log", "git_switch"],
  names: {git_log: "release_git_log"},
})
```

`git_tools(...)` also accepts `defer_loading`, `namespace`, and `tool_config`,
so a large git surface can use the same Tool Vault path as any other registry.
When small/local models struggle to choose among many individual git schemas,
use the compact toolbox instead:

```harn,ignore
let tools = git_toolbox_tools(nil, {
  repo: repo_root,
  include_mutations: true,
})
```

That registry exposes `find_git_tool` and `run_git_tool`. The finder uses a
deterministic stdlib lexical scorer over a curated git-operation catalog; callers
can still plug richer search globally through `tool_search.strategy` when they
want BM25, hybrid ranking, embeddings, or an LLM reranker for deferred tools.

`agent_command_tools(...)` installs:

- `run_command` — argv-first process execution through
  `hostlib_tools_run_command`; can return an immediate progress snapshot while
  the command continues in the background
- `read_command_output` — range-read stdout, stderr, or combined artifacts by
  `command_id`, `handle_id`, or artifact path
- `read_command_output_tail` — read the last bytes of command output without
  requiring the model to calculate offsets

`agent_read_tools(...)` installs root-scoped `read_file`, `read_file_tail`,
`list_directory`, `get_file_outline`, `search_files`, and read-only
`git_inspect`. `agent_host_tools(...)` composes both groups.

Use `std/command` for deterministic script-side harness work, such as "run this
named step, retry according to this policy, keep a normalized step record, and
hand a compact failure reference to a recovery agent." Use
`std/agent/host_tools.agent_command_tools(...)` when the command runner itself
should be exposed as model-facing tools inside an agent loop. Both layers share
`hostlib_tools_run_command` and `hostlib_tools_read_command_output`, so command
artifacts, IDs, output paths, and range readers behave the same.

For JSON-emitting CLIs, keep parsing in the script-side command layer instead
of open-coding process execution plus `json_parse`:

```harn,ignore
import { command_json } from "std/command"

let repo = command_json(["gh", "api", "repos/burin-labs/harn"], {
  capture: {max_inline_bytes: 65536},
})
```

The helpers are deliberately configurable so harness authors can keep their
script surface product-specific without duplicating implementation details:

```harn,ignore
let tools = agent_host_tools(nil, {
  root: repo_root,
  cwd: repo_root,
  max_inline_bytes: 12000,
  search_max_matches: 50,
  exclude_globs: [".harn-runs/**", "logs/**"],
  names: {
    run_command: "release_run",
    read_command_output_tail: "release_log_tail",
  },
  descriptions: {
    run_command: "Run one bounded release inspection command.",
  },
  enabled_tools: ["read", "command"],
  disabled_tools: ["git_inspect"],
  annotations: {
    run_command: {workflow_phase: "release_audit"},
  },
  namespace: "release",
  defer_loading: true,
  output_format: "json",
  command_policy: { request, _args ->
    if request.mode == "argv" && request.argv[0] == "git" {
      return true
    }
    return "only git inspection commands are allowed in this harness"
  },
})
```

Useful options:

| Option | Meaning |
|---|---|
| `root`, `cwd` | Scope read/search/git paths plus command working directories. Out-of-root absolute command `cwd` or git `repo` paths become rejected tool results instead of being executed. |
| `names` | Rename logical tools while preserving correct result-reader links. |
| `descriptions` | Override model-facing descriptions per logical tool. |
| `enabled_tools`, `disabled_tools` | Include logical keys (`run_command`) or groups (`read`, `command`). |
| `max_inline_bytes` | Cap inline command output. If the model asks for a larger inline capture, the helper clamps it to this ceiling so full output still flows through command-output artifact readers. |
| `command_behavior` / `run_command_behavior` | Defaults for every generated `run_command` request. `background_after_ms` gives the command a short foreground startup window, then returns `{status: "running", stdout, stderr, output_path, ...}` while progress events continue in the session. `progress_interval_ms` controls later feedback cadence and `progress_max_inline_bytes` caps snippets. |
| `search_max_matches` / `max_search_matches` | Default `search_files.max_matches` when the model omits one; callers can still request a different cap per call. |
| `exclude_globs` / `search_exclude_globs` | Baseline exclusions for `search_files` using root-relative globs. Tool-call `exclude_globs` are merged with these defaults, not substituted for them. |
| `allow_argv_prefixes` | Deny `run_command` calls unless `argv` starts with a listed string or list prefix. |
| `command_policy` / `allow_command` | Closure hook for custom command approval, denial, or rewrite. |
| `annotations` | Merge extra annotations into generated tool definitions. |
| `tool_config` | Advanced raw `tool_define(...)` config overrides per logical tool or `"*"`. |
| `namespace`, `defer_loading` | Apply Tool Vault metadata to generated tools. |
| `output_format`, `result_formatter` | Return JSON strings by default, or customize rendering. |

## Tool vault

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
| `tool_search: "hybrid"` | Client-mode BM25 plus field-weighted ranking. |
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
| `"hybrid"` | VM | Reciprocal-rank fusion over BM25 and field-weighted name/description/parameter matches. Useful for coding-agent tools where exact names like `run_command` should beat broad prose matches. |
| closure | Harn | Called as `strategy(query, deferred_tools, state)`; return a list of tool names, a JSON string, or `{tool_names: [...]}`. |
| `{handler: closure, name?: string}` | Harn | Named custom searcher with the same return shapes as a closure. Use this to wire embeddings, LLM rerankers, trigram indexes, project-specific ontologies, or host calls from ordinary Harn code. |

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

## Provider-hosted tools

Use normal Harn `tools` when Harn should execute the operation locally,
including local MCP servers, approval hooks, mutation-session audit, and
deterministic tool receipts. Use OpenAI Responses `provider_tools` only when
the provider should execute a hosted tool or remote MCP connector:

```harn
let result = llm_call("Find the current policy and summarize it.", nil, {
  provider: "openai",
  model: "gpt-5.4",
  api_mode: "responses",
  provider_tools: [
    {type: "web_search"},
    {type: "mcp", server_label: "docs", server_url: "https://mcp.example.com", require_approval: "always"},
  ],
})
```

Provider-hosted calls are not dispatchable Harn tool calls. Harn records them
as `provider_tool_call` blocks with `executor: "provider_native"`,
`provider_tool_id`, `call_id`, `provider_tool_type`, `tool_kind`, and the raw
`provider_metadata`. Remote MCP approval and execution are mediated by OpenAI
according to the provider tool config; choose Harn MCP tools instead when Harn
must own each approval and tool receipt.

Standalone OpenAI Responses compaction uses the same mode with `compact: true`.
Harn posts the request to `/responses/compact` and records returned opaque
`compaction` items as private blocks rather than rewriting the Harn transcript
implicitly.

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
