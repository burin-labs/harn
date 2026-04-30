# LLM providers

## Built-in providers

Harn ships with built-in configs for Anthropic, OpenAI, OpenRouter, Ollama,
HuggingFace, Bedrock, Azure OpenAI, Vertex AI, and local OpenAI-compatible
servers. Set the appropriate environment variable to authenticate or point
Harn at an endpoint:

For model-specific feature support, see the generated
[provider capability matrix](../provider-matrix.md).

| Provider | Environment variable | Default model |
|---|---|---|
| Anthropic (default) | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-sonnet-4.6` |
| HuggingFace | `HF_TOKEN` or `HUGGINGFACE_API_KEY` | explicit `model` |
| Bedrock | AWS env/profile/instance role | explicit Bedrock `model` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY` or `AZURE_OPENAI_AD_TOKEN` | deployment name in `model` |
| Vertex AI | `VERTEX_AI_ACCESS_TOKEN` or `GOOGLE_APPLICATION_CREDENTIALS` | Gemini model ID |
| Ollama | `OLLAMA_HOST` (optional) | `llama3.2` |
| Local server | `LOCAL_LLM_BASE_URL` | `LOCAL_LLM_MODEL` or explicit `model` |
| MLX OpenAI-compatible server | `MLX_BASE_URL` | `MLX_MODEL_ID` or `mlx-qwen36-27b` |

Ollama runs locally and doesn't require an API key. The default host is
`http://localhost:11434`.

For a generic OpenAI-compatible local server, set `LOCAL_LLM_BASE_URL` to
something like `http://192.168.86.250:8000` and either pass
`{provider: "local", model: "qwen2.5-coder-32b"}` or set
`LOCAL_LLM_MODEL=qwen2.5-coder-32b`.

For an Apple Silicon MLX OpenAI-compatible server, Harn uses
`MLX_BASE_URL` with a default of `http://127.0.0.1:8002`. Run
`harn provider-ready mlx --model mlx-qwen36-27b` to probe `/v1/models`
and verify that the configured model or alias is currently served. Harn
does not launch MLX scripts itself; hosts that support auto-start should
run their launcher, report launcher failures, then call the Harn readiness
probe again.

### Enterprise providers

Bedrock uses the AWS credential chain. Harn checks `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, and optional `AWS_SESSION_TOKEN` first, then the
selected `AWS_PROFILE` or default profile under `~/.aws/credentials`, then
container credentials, then EC2 instance profile credentials. Set
`AWS_REGION`, `AWS_DEFAULT_REGION`, or `BEDROCK_REGION`. The model is a
Bedrock model ID such as `anthropic.claude-3-5-sonnet-20240620-v1:0` or
`meta.llama3-70b-instruct-v1:0`.

Azure OpenAI requires `AZURE_OPENAI_ENDPOINT`, for example
`https://my-resource.openai.azure.com`. Harn routes the request to
`/openai/deployments/{deployment}/chat/completions` and uses the Harn
`model` value as the deployment name unless `AZURE_OPENAI_DEPLOYMENT` is
set. `AZURE_OPENAI_API_VERSION` defaults to `2024-10-21`. Authentication
uses `AZURE_OPENAI_API_KEY` via the `api-key` header, or
`AZURE_OPENAI_AD_TOKEN` / `AZURE_OPENAI_BEARER_TOKEN` as a bearer token.

Vertex AI requires a project and location. Set `VERTEX_AI_PROJECT` or
`GOOGLE_CLOUD_PROJECT`; set `VERTEX_AI_LOCATION` when the default
`us-central1` is not correct. Authentication uses
`VERTEX_AI_ACCESS_TOKEN` / `GOOGLE_OAUTH_ACCESS_TOKEN`, or a service-account
JSON file through `GOOGLE_APPLICATION_CREDENTIALS`. Harn exchanges service
account keys for a short-lived OAuth token with the cloud-platform scope.

### Capability matrix + `harn.toml` overrides

The provider support table above is **not** hard-coded: it's the output
of a shipped data file (`crates/harn-vm/src/llm/capabilities.toml`)
matched against the `(provider, model)` pair at call time. Scripts
can query the effective capability surface without carrying
vendor-specific knowledge:

```harn
let caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   native_tools: true, defer_loading: true,
//   tool_search: ["bm25", "regex"], max_tools: 10000,
//   prompt_caching: true, thinking: true, vision_supported: true,
// }

if "bm25" in caps.tool_search {
  llm_call(prompt, sys, {
    tools: registry,
    tool_search: "bm25",
  })
}
```

Projects override or extend the shipped table in `harn.toml` — useful
for flagging a proxied OpenAI-compat endpoint as supporting
`tool_search` ahead of a Harn release that knows about it natively:

```toml
# harn.toml
[[capabilities.provider.my-proxy]]
model_match = "*"
native_tools = true
defer_loading = true
tool_search = ["hosted"]
prompt_caching = true
thinking_modes = ["effort"]

# Shadow the built-in Anthropic rule to force client-executed
# fallback on every Opus call (e.g. while a regional outage is
# active):
[[capabilities.provider.anthropic]]
model_match = "claude-opus-*"
native_tools = true
defer_loading = false
tool_search = []
prompt_caching = true
thinking_modes = ["enabled"]
```

Each `[[capabilities.provider.<name>]]` entry accepts these fields:

| Field | Type | Purpose |
|---|---|---|
| `model_match` | glob string | Required. Matched against the lowercased model ID. Leading/trailing `*` or a single middle `*` supported. |
| `version_min` | `[major, minor]` | Narrows the match to a parseable version (Anthropic / OpenAI extractors). Rules where `version_min` is set but the model ID won't parse are skipped. |
| `native_tools` | bool | Whether the provider accepts a native tool-call wire shape. |
| `defer_loading` | bool | Whether `defer_loading: true` on tool definitions is honored server-side. |
| `tool_search` | list of strings | Native `tool_search` variants, preferred first. Anthropic: `["bm25", "regex"]`. OpenAI: `["hosted", "client"]`. Empty = no native support (client fallback only). |
| `max_tools` | int | Cap on tool count. `harn lint` will warn if a registry exceeds the smallest cap any active provider advertises. |
| `prompt_caching` | bool | `cache_control` blocks honored. |
| `thinking_modes` | list of strings | Supported script-facing thinking modes. Values are `enabled`, `adaptive`, or `effort`. |
| `vision_supported` | bool | Image content accepted by the provider/model route. |

First match wins. User rules for a given provider are consulted
before the shipped rules — so the order inside the TOML file matters
(place more specific patterns above wildcards).

`[provider_family]` declares sibling providers that inherit rules
from a canonical family. The shipped table routes OpenRouter,
Together, Groq, DeepSeek, Fireworks, HuggingFace, and local vLLM to
`[[provider.openai]]` by default.

Two programmatic helpers mirror the `harn.toml` path for cases where
editing the manifest is awkward:

- `provider_capabilities_install(toml_src)` — install overrides from
  a TOML string (same layout as `capabilities.toml`, without the
  `capabilities.` prefix: just `[[provider.<name>]]`). Useful when a
  script detects a proxied endpoint at runtime.
- `provider_capabilities_clear()` — revert to shipped defaults.

### Packaged provider adapters via `[llm]`

Projects and installed packages can also contribute provider definitions,
aliases, inference rules, and model defaults directly from `harn.toml`
under `[llm]`. The schema matches `providers.toml`, but the merge is
scoped to the current run:

```toml
[llm.providers.my_proxy]
base_url = "https://llm.example.com/v1"
chat_endpoint = "/chat/completions"
completion_endpoint = "/completions"
auth_style = "bearer"
auth_env = "MY_PROXY_API_KEY"

[llm.aliases]
my-fast = { id = "vendor/model-fast", provider = "my_proxy" }
```

Load order is:

1. built-in defaults
2. `HARN_PROVIDERS_CONFIG` when set, otherwise `~/.config/harn/providers.toml`
3. installed package `[llm]` tables from `.harn/packages/*/harn.toml`
4. the root project's `[llm]` table

That gives packages a stable, declarative way to ship provider adapters
and model aliases without editing Rust-side registration code.

## Provider API details

### Anthropic

- Endpoint: `https://api.anthropic.com/v1/messages`
- Auth: `x-api-key` header
- API version: `2023-06-01`
- System message sent as a top-level `system` field

### OpenAI

- Endpoint: `https://api.openai.com/v1/chat/completions`
- Auth: `Authorization: Bearer <key>`
- System message sent as a message with `role: "system"`

### OpenRouter

- Endpoint: `https://openrouter.ai/api/v1/chat/completions`
- Auth: `Authorization: Bearer <key>`
- Same message format as OpenAI

### HuggingFace

- Endpoint: `https://router.huggingface.co/v1/chat/completions`
- Auth: `Authorization: Bearer <key>`
- Use `HF_TOKEN` or `HUGGINGFACE_API_KEY`
- Same message format as OpenAI

### Ollama

- Endpoint: `<OLLAMA_HOST>/api/chat`
- Default host: `http://localhost:11434`
- No authentication required
- Same message format as OpenAI
- Harn applies shared runtime settings to Ollama chat, completion,
  context-window fallback, and warmup requests. `HARN_OLLAMA_NUM_CTX` wins over
  `OLLAMA_CONTEXT_LENGTH` and `OLLAMA_NUM_CTX`, then defaults to `32768`.
  `HARN_OLLAMA_KEEP_ALIVE` wins over `OLLAMA_KEEP_ALIVE`, then defaults to
  `30m`; `forever`, `infinite`, and `-1` normalize to numeric `-1`, while
  `default` normalizes to `30m`. Hosts that persist IDE preferences should pass
  the raw stored values via `HARN_OLLAMA_*` and let Harn own validation and
  defaults.

### Local OpenAI-compatible server

- Endpoint: `<LOCAL_LLM_BASE_URL>/v1/chat/completions`
- Default host: `http://localhost:8000`
- No authentication required
- Same message format as OpenAI

### MLX OpenAI-compatible server

- Endpoint: `<MLX_BASE_URL>/v1/chat/completions`
- Readiness probe: `<MLX_BASE_URL>/v1/models`
- Default host: `http://127.0.0.1:8002`
- Default alias: `mlx-qwen36-27b`
- No authentication required

## Provider resolution order

When you call `llm_call()` or start an `agent_loop()`, Harn resolves the
provider in this order:

1. **Explicit option** — `llm_call({provider: "openai", ...})` in your script
2. **Environment variable** — `HARN_LLM_PROVIDER`
3. **Inferred from model name** — e.g. `gpt-4o` → OpenAI, `claude-3` → Anthropic
4. **Default** — `anthropic`
5. **Fallback** — if Anthropic key is missing, tries `ollama` then `local`

## API key resolution

Each provider defines an `auth_style` and one or more environment variables:

| Provider | Environment Variable(s) | Auth Style |
|----------|------------------------|------------|
| Anthropic | `ANTHROPIC_API_KEY` | header |
| OpenAI | `OPENAI_API_KEY` | bearer |
| OpenRouter | `OPENROUTER_API_KEY` | bearer |
| HuggingFace | `HF_TOKEN`, `HUGGINGFACE_API_KEY` | bearer |
| Bedrock | AWS credential chain | SigV4 |
| Azure OpenAI | `AZURE_OPENAI_API_KEY`, `AZURE_OPENAI_AD_TOKEN` | api-key or bearer |
| Vertex AI | `VERTEX_AI_ACCESS_TOKEN`, `GOOGLE_APPLICATION_CREDENTIALS` | bearer |
| Ollama | (none) | none |
| Local | (none) | none |

## Model selection

Set the model explicitly or via environment:

```harn
// In code
llm_call({model: "claude-sonnet-4-5-20241022", prompt: "..."})

// Or via environment
// export HARN_LLM_MODEL=gpt-4o
```

The `HARN_LLM_MODEL` environment variable sets the default model when none
is specified in the script.

## Rate limiting

Harn supports per-provider rate limiting (requests per minute):

```bash
# Set via environment
export HARN_RATE_LIMIT_ANTHROPIC=60
export HARN_RATE_LIMIT_OPENAI=120
```

Or in code:

```harn
llm_rate_limit("anthropic", 60)
```

The rate limiter uses a token-bucket algorithm and will pause before sending
requests that would exceed the configured RPM.

## Troubleshooting

- **"No API key found"** — Check that the correct environment variable is
  set for your provider. Run `echo $ANTHROPIC_API_KEY` to verify.
- **Wrong provider selected** — Set `HARN_LLM_PROVIDER` explicitly to
  override automatic detection.
- **Rate limit errors** — Use `HARN_RATE_LIMIT_<PROVIDER>` to throttle
  requests below your plan's limit.
- **Debug message shapes** — Set `HARN_DEBUG_MESSAGE_SHAPES=1` to log
  the structure of messages sent to the LLM provider.
