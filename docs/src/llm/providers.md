# LLM providers

## Built-in providers

Harn ships with built-in configs for Anthropic, OpenAI, OpenRouter, Baseten,
HuggingFace, Together, DeepInfra, NVIDIA NIM, Bedrock, Azure OpenAI, Vertex AI,
and local OpenAI-compatible servers. Set the appropriate environment variable
to authenticate or point Harn at an endpoint:

Run `harn quickstart` to detect existing credentials, local Ollama, free disk
space, and GPU availability, then write starter `providers.toml`, `harn.toml`,
and `.env` files.

Run `harn models recommend` to choose a starter model for the current hardware.
Run `harn provider catalog recommend --json` to inspect the coding-agent readiness
evidence that orders local provider/model presets for quickstart. The report
reads the latest `harn eval coding-agent --include-local` output when present
and falls back to bundled seed evidence, while keeping runtime transport
failures separate from model task failures.
Run `harn models install devstral-small-2` or `harn models install
ollama-gemma4` to resolve Harn aliases and pull the matching Ollama model.
Ollama has no working qwen3.x route — its qwen3.5-family server-side tool-call
parser 500s on Harn's text-tool output — so use the llamacpp provider for local
qwen3.x. For non-Ollama local runtimes, `harn models install
local-qwen3.6-gguf` and `harn models install mlx-qwen3.6` print concrete
llama.cpp / MLX download, launch, context-window, endpoint, and
`provider-ready` verification commands.

Related references: the generated [provider capability
matrix](../provider-matrix.md) for per-model feature support, and
[provider support recommendations](../provider-support.md) for
family-level guidance, endpoint notes, and downstream JSON support data.

| Provider | Environment variable | Default model |
|---|---|---|
| Anthropic (default) | `ANTHROPIC_API_KEY` | `claude-sonnet-5` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-sonnet-4.6` |
| Baseten Model APIs | `BASETEN_API_KEY` | `baseten-glm-5.2` or explicit `baseten/<wire-id>` |
| Together AI | `TOGETHER_AI_API_KEY` | explicit Together model ID |
| DeepInfra | `DEEPINFRA_API_KEY` or `DEEPINFRA_TOKEN` | explicit `deepinfra/<wire-id>` |
| NVIDIA NIM | `NVIDIA_API_KEY` or `NIM_API_KEY` | explicit `nvidia/<wire-id>` |
| Nebius Token Factory | `NEBIUS_API_KEY` | explicit model ID from `/v1/models` |
| FlexAI Token Factory | `FLEXAI_API_KEY` | explicit model ID from model discovery |
| Hyperbolic | `HYPERBOLIC_API_KEY` | explicit model ID from `/v1/models` |
| SiliconFlow | `SILICONFLOW_API_KEY` | explicit model ID from `/v1/models` |
| Parasail | `PARASAIL_API_KEY` | explicit model ID from `/v1/models` |
| Atlas Cloud | `ATLAS_API_KEY` or `ATLASCLOUD_API_KEY` | explicit model ID from `/v1/models` |
| HuggingFace | `HF_TOKEN` or `HUGGINGFACE_API_KEY` | explicit `model` |
| Bedrock | AWS env/profile/instance role | explicit Bedrock `model` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY` or `AZURE_OPENAI_AD_TOKEN` | deployment name in `model` |
| Gemini API | `GEMINI_API_KEY` or `GOOGLE_API_KEY` | `gemini-2.5-flash` or explicit Gemini model ID |
| Vertex AI | `VERTEX_AI_ACCESS_TOKEN` or `GOOGLE_APPLICATION_CREDENTIALS` | Gemini model ID |
| Ollama | `OLLAMA_HOST` (optional) | `devstral-small-2` when installed, otherwise `llama3.2` |
| Local server | `LOCAL_LLM_BASE_URL` | `LOCAL_LLM_MODEL` or explicit `model` |
| llama.cpp server | `LLAMACPP_BASE_URL` | explicit `model` from `/v1/models` |
| MLX OpenAI-compatible server | `MLX_BASE_URL` | `MLX_MODEL_ID` or `mlx-qwen3.6` |
| vLLM OpenAI-compatible server | `VLLM_BASE_URL` | explicit `model` from `/v1/models` |

Baseten Model APIs use `https://inference.baseten.co/v1`. The built-in catalog
includes current Baseten rows for GLM 5.2, Kimi K2.7 Code, DeepSeek V4 Pro,
GPT-OSS 120B, Nemotron, and prior GLM/Kimi routes, keyed as
`baseten/<provider>/<model>` so they can be compared with the same weights on
other hosts. Harn pins `baseten-glm-5.2` to text-format tools because live
native-tool probes returned GLM XML in assistant content instead of an OpenAI
`tool_calls` array; the Kimi, DeepSeek, GPT-OSS, and Nemotron Baseten aliases
pin native tools.

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
recommended Qwen3.6 GGUF download. Use `harn local launch local-qwen3.6
--provider llamacpp --model-source <path-to-gguf>` to start a Harn-managed
server, record its PID/log, verify readiness, and make `harn local stop`
responsible for cleanup.

For an Apple Silicon MLX OpenAI-compatible server, Harn uses
`MLX_BASE_URL` with a default of `http://127.0.0.1:8002`. Run
`harn provider ready mlx --model mlx-qwen3.6` to probe `/v1/models`
and verify that the configured model or alias is currently served. `harn local
launch mlx-qwen3.6 --provider mlx --model-source <mlx-path-or-hf-repo>`
uses the catalog's MLX launch shape (`mlx_lm.server`, host, port, readiness)
and stores a tracked PID for `harn local stop`.

### `harn local` runtime lifecycle

For interactive local-model setups, `harn local` unifies the per-provider
CLIs (`ollama`, `llama-server`, `mlx_lm.server`) under one surface:

```bash
# Survey every local provider, with served models and loaded-model
# memory footprint (Ollama /api/ps).
harn local list

# Active selection + machine profile defaults derived from RAM/GPU.
harn local status

# Bring up a model through the provider's cataloged lifecycle:
# Ollama warms the daemon; llama.cpp/MLX launch a tracked process.
harn local launch devstral-small-2:24b --provider ollama
harn local launch local-qwen3.6 --provider llamacpp --model-source ~/models/qwen3.6/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf --ctx 8192
harn local launch mlx-qwen3.6 --provider mlx --model-source unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit
harn local launch local-gemma4-e4b --provider vllm \
  --model-source google/gemma-4-e4b-it --lora-adapter tools=org/tools-lora

# Warm a model on its provider, evict conflicting local runtimes
# (drains Ollama's loaded set, stops tracked llama.cpp/MLX PIDs), and
# persist the selection to <state>/local/selection.json.
harn local switch qwen36-coder --ctx 65536 --keep-alive 1h

# Explain whether a model/runtime route is preferred, experimental, or
# quarantined on this machine profile.
harn local profile local-qwen3.6 --provider llamacpp --json

# Unload via keep_alive=0 (Ollama) or SIGTERM tracked PIDs.
harn local stop --all
```

`--ctx` / `--keep-alive` default to a machine profile derived from
RAM and accelerator presence — a 48 GB Apple Silicon laptop picks a
wider context window than a low-RAM Linux box. Override either by
passing the flag explicitly. State lives under
`<state_root>/local/` (`HARN_STATE_DIR` honored).

Model-specific local memory hints live under
`[models.<id>.local_memory]`. Harn treats them as conservative launch
guardrails: base resident GiB plus an approximate KV-cache GiB-per-1K-context
term, scaled by cache type and a safety margin. `harn local launch` uses those
facts to block obviously risky starts before spawning a process, recommends a
smaller `--ctx` when it can, and includes the `memory_plan` in `--json` output.
Pass `--allow-memory-risk` only when you have manually freed RAM or know the
catalog estimate is too conservative for your runtime build.

Local runtime launch mechanics live in the provider catalog under
`[providers.<id>.local_runtime]`, not in CLI-only code. The bundled rows cover
Ollama's daemon API, llama.cpp's `llama-server`, MLX-LM's `mlx_lm.server`, and
vLLM's `vllm serve`; user or project provider overlays can change command names,
default ports, arg names, prefix args, model-source environment variables, and
LoRA flag names for local runtime versions or platform-specific installs.

For runtime rows that declare LoRA launch flags, `harn local launch` accepts
repeatable `--lora-adapter NAME=PATH_OR_REPO` values and forwards them through
the cataloged runtime shape. Use `harn models lora inspect --base <model>
<adapter> --provider <provider>` to check local PEFT adapter metadata and print
the corresponding Harn-managed launch command before starting the server. If
the adapter config declares a rank and the provider catalog has a max-rank
runtime flag, the generated command includes `--max-lora-rank` as well.
Use `harn models lora plan --base <model> --provider <provider>
--tool-format auto --corpus <path>` before training a new adapter. It resolves
the same provider capability matrix as runtime calls, records the effective
tool-call format, and prints a portable LoRA/QLoRA training, validation, eval,
inspect, and launch recipe without assuming a local GPU or machine-specific
paths. `--teacher <model>` adds a corpus refresh/distillation plan with
provenance manifest fields, hard-negative slices, and holdout gates; it does not
start training or inference. The plan also reports the template convention to
train against: native Gemma 4 or FunctionGemma tool templates stay distinct from
Harn text/json `<tool_call>` adapters, and the trainer contract calls out
assistant-only loss masks plus `messages`/`tools` columns, so train and serving
do not silently cross tool-call contracts. `harn models lora export` carries
the same contract into dataset rows and manifests with a stable contract id
derived from the base model, provider, effective tool format, dataset format,
and chat template.

Harn maintains local runtime risk profiles for hybrid-cache families
(Qwen3.6, Gemma4). The profile table records preferred runtimes,
required probes, known cache/parser risks, and workarounds for Ollama,
llama.cpp, and MLX. `harn local switch` refuses experimental or quarantined combinations
unless the required probes are supplied with `--probe-result` /
`--passed-probe` or the user passes `--force`.

Use the one-tool conformance probe to produce the JSON receipt consumed by
local lifecycle gates and eval harnesses:

```bash
harn provider tool-probe ollama --model devstral-small-2 --mode both --json
harn provider tool-probe dashscope --model qwen3.6-35b-a3b --mode non-streaming --repeat 5 --json
harn local switch ollama-gemma4 --probe-result gemma4-tool-probe.json
```

The report classifies each mode as a structured native tool call, parseable
Harn text tool call, raw model-specific tag, prose-only response, malformed
arguments, empty response, HTTP error, or transport error. Its
`tool_calling.fallback_mode` is the machine-readable choice downstream
systems should record: `native`, `text`, or `disabled`. Use `--repeat` for
provider reliability measurements; repeated summaries only pass when every
attempt for that mode succeeds.

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

### OpenAI Responses API

OpenAI has two Harn paths. The default path is the generic
OpenAI-compatible chat-completions adapter. The native Responses path is
selected explicitly with
`llm_call(..., {provider: "openai", api_mode: "responses"})`.

Responses mode is for OpenAI-native hosted tools, remote MCP connectors,
previous-response chaining, background jobs, and provider-side
truncation/compaction controls. Ordinary Harn `tools` still work in this mode
and Harn executes, approves, and audits them locally. Use `provider_tools` (or
`hosted_tools`) only when OpenAI should execute the hosted tool or remote MCP
connector. In that case OpenAI owns per-tool execution and approval according
to the tool config; Harn records provider-native IDs, normalized
`provider_tool_call` blocks, and `provider_response_id`, but it does not
locally mediate each remote call.

### Capability matrix + `harn.toml` overrides

The provider support table is generated from
`crates/harn-vm/src/llm/capabilities.toml` and matched against the
`(provider, model)` pair at call time. Scripts can query the effective
capability surface without carrying vendor-specific knowledge:

```harn
let caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   native_tools: true, text_tool_wire_format_supported: true,
//   preferred_tool_format: "native", tool_mode_parity: "unknown",
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
// Presets use `preferred_tool_format` when it is present, which keeps known
// native/text divergences in capability data instead of provider-name branches.
// `agent_loop` uses the same field when `tool_format` is unset or `"auto"`;
// missing recommendations fall back to text tools and emit `capability_gap`.
if caps.tools && "bm25" in caps.tool_search {
  llm_call(prompt, sys, {
    tools: registry,
    tool_search: "bm25",
  })
}
```

OpenAI Responses-capable rows also expose `responses_api`, `hosted_tools`,
`remote_mcp`, `conversation_state`, `compaction`, `background_mode`, and
`tool_approval_policy`.

The same matrix is the source of truth for Harn's default tool-calling
mode. Alias-level `tool_format` still wins when set explicitly, but
otherwise `preferred_tool_format` chooses `agent_loop()` and `models info`
tool mode for that provider/model route. Rows that do not set it infer
`native` when `native_tools = true` and `text` otherwise. Rows can set
`text_tool_wire_format_supported = true` for runtimes where Harn's text-tool
contract is the reliable tool path, and can mark `tool_mode_parity` /
`tool_mode_parity_notes` when native and text modes are known not to be
interchangeable. If a caller explicitly forces a conflicting `tool_format`,
the agent loop emits a `tool_format_override` transcript event; pass
`tool_format_override_reason` when intentionally forcing a catalog-marked
unreliable side. Model-catalog display tags are derived from this matrix too;
legacy `models.*.capabilities` entries are parsed for backwards compatibility
but do not override runtime capability resolution.

`harn eval coding-agent` now emits
`.harn-runs/coding-agent-bench/latest/tool_mode_parity_overlay.toml`, and
`harn provider capabilities promote-from-eval <overlay>` applies those
deterministic parity verdicts back into
`crates/harn-vm/src/llm/capabilities.toml`.

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
defaults for matching models. By default the **first matching rule wins** and
every field it leaves unset resolves from provider / built-in defaults. A rule
that sets `extends = true` instead contributes only the fields it names and
lets resolution continue to later matching rules (then the `provider_family`
chain, then defaults) to fill the rest — so an overlay can tweak one field of a
shipped row without copying the whole row verbatim and freezing its other
fields against catalog updates. Each `[[capabilities.provider.<name>]]` entry
accepts these fields:

| Field | Type | Purpose |
|---|---|---|
| `model_match` | glob string | Required. Matched against the lowercased model ID. Leading/trailing `*` or a single middle `*` supported. |
| `version_min` | `[major, minor]` | Narrows the match to a parseable version (Anthropic / OpenAI extractors). Rules where `version_min` is set but the model ID won't parse are skipped. |
| `extends` | bool | When true this matching rule contributes only the fields it sets and resolution continues to later matching rules and defaults for the rest (field-wise fall-through). Omitted / false keeps first-match-wins. |
| `native_tools` | bool | Whether the provider accepts a native tool-call wire shape. |
| `text_tool_wire_format_supported` | bool | Whether the provider/model route can use Harn's text-tool contract. Defaults to true for shipped rules unless disabled. |
| `preferred_tool_format` | string | Optional preset default, `native` or `text`; inferred from `native_tools` when omitted. |
| `tool_mode_parity` | string | Native/text interchangeability status: `interchangeable`, `unknown`, `native_unreliable`, `text_unreliable`, `native_only`, `text_only`, or `unsupported`. |
| `tool_mode_parity_notes` | string | Optional explanation for known non-interchangeable routes. |
| `message_wire_format` | string | Shared request/response message format: `openai`, `anthropic`, `gemini`, or `ollama`. |
| `native_tool_wire_format` | string | Native tool definition shape for shared helpers: `openai` or `anthropic`. Gemini and Vertex accept Harn's canonical tool definitions and their adapters emit Google `functionDeclarations`. |
| `defer_loading` | bool | Whether `defer_loading: true` on tool definitions is honored server-side. |
| `tool_search` | list of strings | Native `tool_search` variants, preferred first. Anthropic: `["bm25", "regex"]`. OpenAI: `["hosted", "client"]`. Empty = no native support (client fallback only). |
| `responses_api` | bool | Whether Harn exposes this route through the native OpenAI Responses path. Generic OpenAI-compatible providers do not claim this even when they inherit other OpenAI-family capabilities. |
| `hosted_tools` | list of strings | Provider-hosted tool kinds Harn can pass through without local execution, such as `web_search`, `file_search`, `code_interpreter`, or `mcp` / `remote_mcp`. |
| `remote_mcp` | bool | Provider-hosted remote MCP connectors are available. |
| `conversation_state` | bool | Provider-managed previous-response chaining is available. |
| `compaction` | bool | Provider-side truncation/compaction controls are available. |
| `background_mode` | bool | Provider-side background jobs are available. |
| `tool_approval_policy` | string | Approval policy story for provider-executed tools, for example `provider_or_harn`. |
| `max_tools` | int | Cap on tool count. `harn lint` will warn if a registry exceeds the smallest cap any active provider advertises. |
| `prompt_caching` | bool | Provider-side prompt caching is available. |
| `cache_breakpoint_style` | string | Request marker strategy when caching is explicit: `none`, `top_level`, or `last_block`. |
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

### Field-wise catalog patches with `[patch.models]`

An overlay's `[models.<id>]` table replaces the whole model row, which is
the right tool for adding a route but the wrong one for tweaking a single
field: copying a large baseline row verbatim freezes every other field
against future catalog updates. `[patch.models.<id>]` instead merges just
the named fields into the existing row:

```toml
# Only these two fields change; the rest of the baseline row
# (name, pricing.input_per_mtok, capabilities, ...) stays live.
[patch.models."deepinfra/openai/gpt-oss-120b"]
stream_timeout = 1200.0

[patch.models."deepinfra/openai/gpt-oss-120b".pricing]
output_per_mtok = 2.5
```

Patch rules:

- Nested tables merge recursively; scalars **and arrays** replace the base
  value wholesale (no element-wise array merge).
- Within one overlay, `[models.<id>]` whole-row replacement applies before
  `[patch.models.<id>]`, so patch fields win.
- Patches are **sticky** across layers: they re-apply after every later
  layer's merge, including a later layer's whole-row replacement of the
  same id. A patch means "always tweak this field", not "tweak it once".
- A patch whose target row does not exist yet is held silently and applies
  as soon as a later layer contributes the row.
- A patch that produces a type-invalid row warns once and keeps the
  unpatched row.

The same schema works at every overlay layer, including `harn.toml`
`[llm]` sections (`[llm.patch.models.<id>]`). Pair it with `[suppress]`
(route suppression) and whole-row `[models.<id>]` replacement — the three
tools cover field tweaks, route removal, and route addition/renames
without forking the baseline catalog.

### ACP agent providers

External ACP agents can be registered as LLM providers by declaring
`protocol = "acp"`. Harn launches the configured command over stdio, performs
`initialize`, creates a session, sends the `llm_call` prompt as
`session/prompt`, and collects `agent_message_chunk` updates into the normal
`LlmResult`.

```toml
[llm.providers.codex_acp]
protocol = "acp"
command = "codex-acp"
args = []
auth_style = "none"
cwd = "."
mcp_servers = []
```

Provider-specific call overrides use the provider name as the option key:

```harn
let answer = llm_call("Summarize the current workspace", nil, {
  provider: "codex_acp",
  model: "default",
  codex_acp: {
    cwd: cwd(),
    args: ["--profile", "default"],
    mcpServers: [],
  },
})
```

The adapter treats host-mediated ACP requests conservatively: it cancels
`session/request_permission` and returns method-not-found for other client
methods instead of granting file, shell, or UI authority through an LLM
provider call. Use `harn serve acp` when a real editor or host should own those
permissions.

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
`devstral-small-2:24b` defaults to `262144`) will be loaded at
that maximum if the first request to load it does not pass an explicit
`num_ctx`. Subsequent Harn calls with `HARN_OLLAMA_NUM_CTX=32768` then
appear to be ignored — they are not, but Ollama is reusing the larger
runner.

Inspect what is actually loaded vs. what Harn would request:

```bash
harn models info devstral-small-2 --verify --warm
```

The JSON output includes:

- `expected.num_ctx` / `expected.keep_alive` — what Harn injects into
  request bodies for this model.
- `loaded_runner.context_length` — what `/api/ps` reports for the
  matched runner, when present.
- `context_drift` — a remediation message when the two diverge.

If `context_drift` is set, force a reload with:

```bash
ollama stop devstral-small-2:24b
harn models info devstral-small-2 --verify --warm
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
- Default alias: `mlx-qwen3.6`
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
llm_call("...", nil, {model: "claude-sonnet-5"})

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

Harn supports catalog-driven per-provider and per-model rate limiting. The
runtime enforces `rate_limits` metadata for requests per minute (`rpm`), total
tokens per minute (`tpm`), split input/output token buckets, and published route
concurrency before each provider call.

```bash
# Legacy provider RPM override.
export HARN_RATE_LIMIT_ANTHROPIC=60
export HARN_RATE_LIMIT_OPENAI=120

# Rich quota override for a paid or custom plan.
export HARN_RATE_LIMIT_MYPROVIDER_RPM=1000
export HARN_RATE_LIMIT_MYPROVIDER_TPM=1000000
```

Or in code:

```harn
llm_rate_limit("anthropic", {rpm: 60, tpm: 250000})
let active = llm_rate_limit("anthropic", {details: true})
```

The limiter uses a sliding-window budget and pauses before sending requests
that would exceed the configured request or token quota. Request and token
buckets are durable across Harn processes by default, using a SQLite state file
under Harn's runtime state root. Fleet runners that need every child process to
share one explicit file can set `HARN_LLM_RATE_LIMIT_STATE_PATH`; constrained
tests or embeddings can disable the durable layer with
`HARN_LLM_RATE_LIMIT_DURABLE=0`.

## Troubleshooting

- **"No API key found"** — Check that the correct environment variable is
  set for your provider. Run `echo $ANTHROPIC_API_KEY` to verify.
- **Wrong provider selected** — Set `HARN_LLM_PROVIDER` explicitly to
  override automatic detection.
- **Rate limit errors** — Prefer fixing the provider/model catalog
  `rate_limits` entry for shared defaults. Use `HARN_RATE_LIMIT_<PROVIDER>_RPM`
  and `HARN_RATE_LIMIT_<PROVIDER>_TPM` only when your local key has a different
  paid/custom quota. `HARN_RATE_LIMIT_<PROVIDER>` remains a legacy RPM
  shorthand.
- **Debug message shapes** — Set `HARN_DEBUG_MESSAGE_SHAPES=1` to log
  the structure of messages sent to the LLM provider.
