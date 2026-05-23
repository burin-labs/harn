# LLM providers

## Built-in providers

Harn ships with built-in configs for Anthropic, OpenAI, OpenRouter, Ollama,
HuggingFace, Bedrock, Azure OpenAI, Vertex AI, and local OpenAI-compatible
servers. Set the appropriate environment variable to authenticate or point
Harn at an endpoint:

Run `harn quickstart` to detect existing credentials, local Ollama, free disk
space, and GPU availability, then write starter `providers.toml`, `harn.toml`,
and `.env` files.

Run `harn models recommend` to choose a starter model for the current hardware.
Run `harn models install qwen3.6-coding`, `harn models install devstral-small-2`,
or `harn models install ollama-gemma4` to resolve Harn aliases and pull the
matching Ollama model. For non-Ollama local runtimes, `harn models install
local-qwen3.6-gguf` and `harn models install local-qwen3.6-27b` print concrete
llama.cpp / MLX download, launch, context-window, endpoint, and
`provider-ready` verification commands.

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
| Gemini API | `GEMINI_API_KEY` or `GOOGLE_API_KEY` | `gemini-2.5-flash` or explicit Gemini model ID |
| Vertex AI | `VERTEX_AI_ACCESS_TOKEN` or `GOOGLE_APPLICATION_CREDENTIALS` | Gemini model ID |
| Ollama | `OLLAMA_HOST` (optional) | `qwen3.6-coding` when installed, otherwise `llama3.2` |
| Local server | `LOCAL_LLM_BASE_URL` | `LOCAL_LLM_MODEL` or explicit `model` |
| llama.cpp server | `LLAMACPP_BASE_URL` | explicit `model` from `/v1/models` |
| MLX OpenAI-compatible server | `MLX_BASE_URL` | `MLX_MODEL_ID` or `mlx-qwen36-27b` |

Ollama runs locally and doesn't require an API key. The default host is
`http://localhost:11434`.

On a fresh install, `harn run` and `harn playground`/`harn try` detect Harn
programs that call provider-backed LLM builtins such as `llm_call`,
`llm_stream_call`, or `agent_loop`. If no user or project provider config is
present and local Ollama responds at
`http://127.0.0.1:11434/api/tags`, Harn offers to write
`~/.config/harn/providers.toml` with Ollama as the default provider. Pass
`--yes` to accept that setup without an interactive prompt.

For a generic OpenAI-compatible local server, set `LOCAL_LLM_BASE_URL` to
something like `http://192.168.86.250:8000` and either pass
`{provider: "local", model: "qwen2.5-coder-32b"}` or set
`LOCAL_LLM_MODEL=qwen2.5-coder-32b`.

For llama.cpp / `llama-server`, Harn has a separate `llamacpp` provider so Qwen
thinking-template quirks can be modeled independently from other local
OpenAI-compatible servers. Set `LLAMACPP_BASE_URL` when it is not listening on
`http://127.0.0.1:8001`. `harn models install local-qwen3.6-gguf` prints the
recommended Qwen3.6 GGUF download and `llama-server` command. The generated
launch command uses a 65,536-token context, one server slot, quantized KV cache,
and the Harn text-tool contract by default.

For an Apple Silicon MLX OpenAI-compatible server, Harn uses
`MLX_BASE_URL` with a default of `http://127.0.0.1:8002`. Run
`harn provider-ready mlx --model mlx-qwen36-27b` to probe `/v1/models`
and verify that the configured model or alias is currently served. Harn
does not launch MLX scripts itself; `harn models install local-qwen3.6-27b`
prints the venv, model download, `mlx_vlm.server`, endpoint, and readiness
commands. `mlx-vlm` server flags vary by release; only add
`--served-model-name` when `python -m mlx_vlm.server --help` advertises it.
Hosts that support auto-start should run their launcher, report launcher
failures, then call the Harn readiness probe again.

### `harn local` runtime lifecycle

For interactive local-model setups, `harn local` wraps the assorted
provider CLIs (`ollama`, `llama-server`, `mlx_lm.server`) behind one
stable surface:

```bash
# Survey every local provider, with served models and loaded-model
# memory footprint (Ollama /api/ps).
harn local list

# Active selection + machine profile defaults derived from RAM/GPU.
harn local status

# Warm a model on its provider, evict conflicting local runtimes
# (drains Ollama's loaded set, stops tracked llama.cpp/MLX PIDs), and
# persist the selection to <state>/local/selection.json.
harn local switch qwen36-coder --ctx 65536 --keep-alive 1h

# Explain whether a model/runtime route is preferred, experimental, or
# quarantined on this machine profile.
harn local profile qwen3.6-coding --provider llamacpp --json

# Unload via keep_alive=0 (Ollama) or SIGTERM tracked PIDs.
harn local stop --all
```

`--ctx` / `--keep-alive` default to a machine profile derived from
RAM and accelerator presence — a 48 GB Apple Silicon laptop picks a
wider context window than a low-RAM Linux box. Override either by
passing the flag explicitly. State lives under
`<state_root>/local/` (`HARN_STATE_DIR` honored).

Harn also carries local runtime risk profiles for hybrid-cache families such
as Qwen3.6 and Gemma4. The profile table records preferred runtimes, required
probes, known cache/parser risks, and workarounds for Ollama, llama.cpp, and
MLX. `harn local switch` refuses experimental or quarantined combinations
unless the required probes are supplied with `--probe-result` /
`--passed-probe` or the user passes `--force`.

Use the one-tool conformance probe to produce the JSON receipt consumed by
local lifecycle gates and eval harnesses:

```bash
harn provider-tool-probe ollama --model qwen3.6-coding --mode both --json
harn local switch ollama-gemma4 --probe-result gemma4-tool-probe.json
```

The report classifies each mode as a structured native tool call, parseable
Harn text tool call, raw model-specific tag, prose-only response, malformed
arguments, empty response, HTTP error, or transport error. Its
`tool_calling.fallback_mode` is the machine-readable choice downstream
systems should record: `native`, `text`, or `disabled`.

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

The native Gemini API uses Google's `generateContent` shape directly. Harn
lowers native tools to `functionDeclarations`, records model-emitted
`functionCall` parts, returns tool observations as `functionResponse` parts,
and preserves Gemini thought signatures in conversation history without
showing those opaque bytes as visible reasoning. `llm_call(..., {schema: ...})`
uses Gemini's JSON response controls (`responseMimeType` plus JSON schema),
and response usage maps `cachedContentTokenCount` to Harn's cache-read token
field.

Vertex AI also serves Gemini models through `generateContent`, but it is a
Google Cloud route with OAuth/service-account authentication and project /
location scoping. The built-in Vertex adapter shares the Google function
declaration schema for native tool definitions while keeping its existing
Google Cloud request envelope. OpenAI-compatible routes that serve Gemini
model IDs, such as OpenRouter or a local proxy, remain OpenAI-wire routes:
they use OpenAI-style `tool_calls` / `tools` and OpenAI-style structured-output
parameters rather than Gemini `functionCall`, `functionResponse`, or
`responseJsonSchema` parts.

### Capability matrix + `harn.toml` overrides

The provider support table above is **not** hard-coded: it's the output
of a shipped data file (`crates/harn-vm/src/llm/capabilities.toml`)
matched against the `(provider, model)` pair at call time. Scripts
can query the effective capability surface without carrying
vendor-specific knowledge:

```harn
let caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   native_tools: true, text_tool_wire_format_supported: true,
//   tools: true, defer_loading: true,
//   tool_search: ["bm25", "regex"], max_tools: 10000,
//   prompt_caching: true, thinking: true, vision_supported: true,
//   interleaved_thinking_supported: true,
//   message_wire_format: "anthropic",
//   native_tool_wire_format: "anthropic",
//   prefers_xml_scaffolding: true,
//   structured_output_mode: "xml_tagged",
//   supports_assistant_prefill: false,
//   prefers_xml_tools: true,
//   thinking_block_style: "thinking_blocks",
// }

// Gate on `tools` for "can this route call tools at all" — true for either
// native or text-format tool wire. Inspect `native_tools` or
// `text_tool_wire_format_supported` directly when you need to distinguish.
if caps.tools && "bm25" in caps.tool_search {
  llm_call(prompt, sys, {
    tools: registry,
    tool_search: "bm25",
  })
}
```

The same matrix is the source of truth for Harn's default tool-calling
mode. Alias-level `tool_format` still wins when set explicitly, but
otherwise a matrix row with `native_tools = true` makes
`agent_loop()` and `model-info` choose native provider tools for that
provider/model route. Rows can also set `text_tool_wire_format_supported = true`
for runtimes where Harn's text-tool contract is the reliable tool path even
though native provider tool calls are unavailable or too flaky. Model-catalog
display tags are derived from this matrix too; legacy `models.*.capabilities`
entries are parsed for backwards compatibility but do not override runtime
capability resolution.

The matrix also records format preferences that prompt renderers can use
without branching on provider names: XML vs. Markdown section scaffolding,
native JSON vs. delimited/XML-tagged structured-output preference, assistant
prefill support, developer-role instruction preference, XML text-tool prompt
preference, and the preferred thinking-block representation.

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

Provider-wide defaults can be declared under
`[capabilities.provider_defaults.<name>]`; rule entries override those
defaults for matching models. Each `[[capabilities.provider.<name>]]`
entry accepts these fields:

| Field | Type | Purpose |
|---|---|---|
| `model_match` | glob string | Required. Matched against the lowercased model ID. Leading/trailing `*` or a single middle `*` supported. |
| `version_min` | `[major, minor]` | Narrows the match to a parseable version (Anthropic / OpenAI extractors). Rules where `version_min` is set but the model ID won't parse are skipped. |
| `native_tools` | bool | Whether the provider accepts a native tool-call wire shape. |
| `message_wire_format` | string | Shared request/response message format: `openai`, `anthropic`, `gemini`, or `ollama`. |
| `native_tool_wire_format` | string | Native tool definition shape for shared helpers: `openai` or `anthropic`. Gemini and Vertex accept Harn's canonical tool definitions and their adapters emit Google `functionDeclarations`. |
| `defer_loading` | bool | Whether `defer_loading: true` on tool definitions is honored server-side. |
| `tool_search` | list of strings | Native `tool_search` variants, preferred first. Anthropic: `["bm25", "regex"]`. OpenAI: `["hosted", "client"]`. Empty = no native support (client fallback only). |
| `max_tools` | int | Cap on tool count. `harn lint` will warn if a registry exceeds the smallest cap any active provider advertises. |
| `prompt_caching` | bool | `cache_control` blocks honored. |
| `prefers_xml_scaffolding` | bool | Logical prompt sections should prefer XML tags such as `<task>` / `<examples>`. |
| `prefers_markdown_scaffolding` | bool | Logical prompt sections should prefer Markdown headings such as `## Task`. |
| `structured_output_mode` | string | Preferred logical structured-output shape: `native_json`, `delimited`, `xml_tagged`, or `none`. Separate from the transport-level `structured_output` strategy. |
| `supports_assistant_prefill` | bool | Provider/model route accepts an assistant-role prefill message. |
| `prefers_role_developer` | bool | Durable instructions should use OpenAI's `developer` role rather than `system`. |
| `prefers_xml_tools` | bool | Text-rendered tool specs should use XML wrappers rather than JSON-schema prose. |
| `thinking_block_style` | string | Preferred transcript thinking style: `none`, `thinking_blocks`, `reasoning_summary`, or `inline`. |
| `thinking_modes` | list of strings | Supported script-facing thinking modes. Values are `enabled`, `adaptive`, or `effort`. |
| `reasoning_wire_format` | string | Non-standard OpenAI-compatible reasoning request shape: `openrouter` or `enabled`. |
| `reasoning_effort_supported` | bool | Provider accepts a `reasoning_effort` request field for effort-capable models. |
| `reasoning_none_supported` | bool | Provider accepts `reasoning_effort: "none"` as true reasoning-off instead of flooring at `minimal`. |
| `interleaved_thinking_supported` | bool | `thinking: true` can request Anthropic's `interleaved-thinking-2025-05-14` beta header. |
| `anthropic_beta_features` | list of strings | Anthropic beta feature names always requested for this provider/model route. |
| `vision_supported` | bool | Image content accepted by the provider/model route. |
| `image_url_input_supported` | bool | Image content may reference remote URLs. Set false for routes that require base64 images. |
| `file_upload_wire_format` | string | Upload API family used by `files.upload`: `anthropic` or `gemini`. |
| `seed_supported`, `top_k_supported`, `frequency_penalty_supported`, `presence_penalty_supported` | bool | Generation option support flags used for warnings and provider-neutral validation. |
| `thinking_disable_directive` | string | In-prompt directive (e.g. `"/no_think"` for Qwen3 chat templates) auto-prepended to the system message when the resolved `thinking` is `Disabled`. Lets script authors write `thinking: false` uniformly across providers without learning per-template prompt directives. Idempotent — never injected twice. |

First match wins. User rules for a given provider are consulted
before the shipped rules — so the order inside the TOML file matters
(place more specific patterns above wildcards).

`[provider_family]` declares sibling providers that inherit rules
from a canonical family. The shipped table routes OpenRouter,
Together, Groq, DeepSeek, Fireworks, HuggingFace, DashScope, local
vLLM, llama.cpp, and MLX to `[[provider.openai]]` by default.

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

The provider files in steps 2-4 are overlays, so a starter file can set
`default_provider` or aliases without copying every built-in provider
definition. That gives packages a stable, declarative way to ship provider
adapters and model aliases without editing Rust-side registration code.

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
- Qwen3.6, Devstral Small 2, and Gemma4 local aliases default to Harn's
  text-tool contract. Native tool calling remains opt-in for model-specific
  experiments, because local runtime parsers can lag current model templates.
- Harn applies shared runtime settings to Ollama chat, completion,
  context-window fallback, and warmup requests. `HARN_OLLAMA_NUM_CTX` wins over
  `OLLAMA_CONTEXT_LENGTH` and `OLLAMA_NUM_CTX`, then defaults to `32768`.
  `HARN_OLLAMA_KEEP_ALIVE` wins over `OLLAMA_KEEP_ALIVE`, then defaults to
  `30m`; `forever`, `infinite`, and `-1` normalize to numeric `-1`, while
  `default` normalizes to `30m`. Hosts that persist IDE preferences should pass
  the raw stored values via `HARN_OLLAMA_*` and let Harn own validation and
  defaults. `HARN_OLLAMA_UNLOAD_GRACE_MS` wins over `OLLAMA_UNLOAD_GRACE_MS`
  and defaults to `10000`; when an Ollama stream produces no chunks for longer
  than this after the request starts, Harn emits one progress notification that
  the model is warming up.

#### Effective vs. loaded context (`num_ctx` semantics)

Ollama sets `num_ctx` once, when a model is **loaded** into memory. After
that, the runner keeps the same context window for its lifetime — a chat
request with a different `num_ctx` does *not* shrink an already-loaded
runner; Ollama unloads and reloads only when the requested value changes
substantially across requests.

`ollama ps` (and `GET /api/ps`) report `context_length` for each loaded
runner. That number is the **effective** context the runner will use, not
the model's declared maximum.

Common gotcha: a model whose Modelfile defaults to a large context (e.g.
`qwen3.6:35b-a3b-coding-nvfp4` defaults to `262144`) will be loaded at
that maximum if the first request to load it does not pass an explicit
`num_ctx`. Subsequent Harn calls with `HARN_OLLAMA_NUM_CTX=32768` then
appear to be ignored — they are not, but Ollama is reusing the larger
runner.

Inspect what is actually loaded vs. what Harn would request:

```bash
harn model-info qwen3.6-coding --verify --warm
```

The JSON output includes:

- `expected.num_ctx` / `expected.keep_alive` — what Harn injects into
  request bodies for this model.
- `loaded_runner.context_length` — what `/api/ps` reports for the
  matched runner, when present.
- `context_drift` — a remediation message when the two diverge.

If `context_drift` is set, force a reload with:

```bash
ollama stop qwen3.6:35b-a3b-coding-nvfp4
harn model-info qwen3.6-coding --verify --warm
```

The new warmup correctly passes `options.num_ctx`, so the next load
respects `HARN_OLLAMA_NUM_CTX` (or the catalog's
`runtime_context_window`, in that priority order).

### Local OpenAI-compatible server

- Endpoint: `<LOCAL_LLM_BASE_URL>/v1/chat/completions`
- Default host: `http://localhost:8000`
- No authentication required
- Same message format as OpenAI

### llama.cpp OpenAI-compatible server

- Endpoint: `<LLAMACPP_BASE_URL>/v1/chat/completions`
- Default host: `http://127.0.0.1:8001`
- No authentication required
- Qwen3 and Devstral capability rules enable Harn's text-tool contract by
  default. Native llama-server tool calls remain opt-in because upstream
  llama.cpp has current OpenAI-compatible parser edge cases for malformed or
  leaked tool-call JSON with these templates.
- Qwen3 rules still enable `chat_template_kwargs` and `/no_think` handling
  when the model ID matches Qwen

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
llm_call("...", nil, {model: "claude-sonnet-4-5-20241022"})

// Or via environment
// export HARN_LLM_MODEL=gpt-4o
```

The `HARN_LLM_MODEL` environment variable sets the default model when none
is specified in the script.

### Serverless vs. dedicated routes

Each catalog row carries an `availability` field that distinguishes the
provider's serverless surface from routes that require a dedicated
endpoint:

| value | meaning |
|---|---|
| `serverless` | Reachable through the provider's normal API-key path. The default for cataloged rows. |
| `dedicated` | Listed by the provider but only callable once the caller has provisioned a dedicated endpoint (e.g. some Together `/v1/models` entries). Hosts must not auto-route to it. |
| `unknown` | Surfaced dynamically (e.g. from `/v1/models`) without a static claim from Harn or the user. |

Override the field in `harn.toml` overlays when shipping a provider
adapter for routes that need explicit provisioning:

```toml
[models."Qwen/Qwen3-Coder-Next-FP8"]
name = "Qwen3 Coder Next FP8 (dedicated)"
provider = "together"
context_window = 262144
availability = "dedicated"
```

A runtime call that hits a non-serverless Together route also classifies
as `model_unavailable` (not the generic `invalid_request`) so fallback
logic can route around the dedicated-only model.

## Rate limiting

Harn supports per-provider rate limiting (requests per minute):

```bash
# Set via environment
export HARN_RATE_LIMIT_ANTHROPIC=60
export HARN_RATE_LIMIT_OPENAI=120
```

Or in code:

```harn
llm_rate_limit("anthropic", {rpm: 60})
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
