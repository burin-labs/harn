## Environment variables

The following environment variables configure runtime behavior:

| Variable | Description |
|---|---|
| `HARN_LLM_PROVIDER` | Override the default LLM provider. Any configured provider is accepted. Built-in names include `anthropic` (default), `openai`, `openrouter`, `huggingface`, `ollama`, `local`, and `mock`. |
| `HARN_LLM_TIMEOUT` | LLM request timeout in seconds. Default `120`. |
| `HARN_STATE_DIR` | Override the runtime state root used for store, checkpoint, metadata, and default worktree state. Relative values resolve from the active project/runtime root. |
| `HARN_RUN_DIR` | Override the default persisted run directory. Relative values resolve from the active project/runtime root. |
| `HARN_WORKTREE_DIR` | Override the default worker worktree root. Relative values resolve from the active project/runtime root. |
| `ANTHROPIC_API_KEY` | API key for the Anthropic provider. |
| `OPENAI_API_KEY` | API key for the OpenAI provider. |
| `OPENROUTER_API_KEY` | API key for the OpenRouter provider. |
| `HF_TOKEN` | API key for the HuggingFace provider. |
| `HUGGINGFACE_API_KEY` | Alternate API key name for the HuggingFace provider. |
| `OLLAMA_HOST` | Override the Ollama host. Default `http://localhost:11434`. |
| `HARN_OLLAMA_NUM_CTX` | Preferred Ollama context window for Harn-owned Ollama chat, completion, context-window fallback, and warmup requests. Must be a positive integer. Takes precedence over `OLLAMA_CONTEXT_LENGTH` and `OLLAMA_NUM_CTX`; default `32768`. Hosts that persist IDE preferences should pass the raw stored value here and let Harn validate/default it. |
| `HARN_OLLAMA_KEEP_ALIVE` | Preferred Ollama keep-alive for Harn-owned Ollama chat, completion, and warmup requests. Takes precedence over `OLLAMA_KEEP_ALIVE`; default `30m`. `forever`, `infinite`, and `-1` normalize to Ollama's numeric `-1`; `default` normalizes to `30m`. |
| `HARN_OLLAMA_UNLOAD_GRACE_MS` | Preferred Ollama unload/warmup grace in milliseconds before Harn emits a one-time progress notification for an Ollama stream that has produced no chunks yet. Takes precedence over `OLLAMA_UNLOAD_GRACE_MS`; default `10000`. Set `0` to disable the notification. |
| `LOCAL_LLM_BASE_URL` | Base URL for a local OpenAI-compatible server. Default `http://localhost:8000`. |
| `LOCAL_LLM_MODEL` | Default model ID for the local OpenAI-compatible provider. |
| `MLX_BASE_URL` | Base URL for the MLX OpenAI-compatible provider. Default `http://127.0.0.1:8002`. |
| `MLX_MODEL_ID` | Default model ID for the MLX OpenAI-compatible provider readiness probe. |

### Sandbox and egress

These variables affect the confinement `harn run` installs by default. See
[Sandbox mode](30-sandbox-mode.md) for the run-level sandbox and `egress_policy(config)`
in the built-in method table for the full rule syntax.

| Variable | Description |
|---|---|
| `HARN_HANDLER_SANDBOX` | How the `worktree` sandbox profile reacts when the platform's OS confinement mechanism (Linux Landlock + seccomp, macOS `sandbox-exec`, Windows AppContainer) is unavailable: `enforce`/`required`/`1`/`true` fail the spawn, `warn` (default) logs once and continues with workspace-root path enforcement but **without** OS confinement, and `off`/`none`/`0`/`false` disables the OS portion silently. Workspace-root path enforcement for file builtins is unaffected either way. The `os_hardened` profile always enforces and ignores this variable. |
| `HARN_EGRESS_ALLOW` | Comma-separated egress allow rules seeding the egress policy. Rules accept exact hosts, `*.suffix` wildcards, IP literals/CIDR, and an optional `:port`. |
| `HARN_EGRESS_DENY` | Comma-separated egress deny rules, same syntax as `HARN_EGRESS_ALLOW`. Deny wins over allow. |
| `HARN_EGRESS_DEFAULT` | Action for destinations matching no rule: `allow` (default) or `deny` (allowlist mode). |
| `HARN_EGRESS_BLOCK_PRIVATE` | Private-address (SSRF) guard. `private`/`on`/`block`/`true` block DNS-resolved loopback, private, link-local, cloud-metadata, multicast, documentation, CGNAT, benchmark, and equivalent IPv6 ranges; `off`/`false`/`none` opts out. Under default `harn run` this guard is already on; configured LLM provider endpoints on loopback or a private LAN remain reachable, while link-local and cloud-metadata ranges stay blocked. |
| `HARN_EGRESS_ALLOW_LOOPBACK` | Set to `1`/`true` to permit loopback destinations while the private-address guard otherwise stays engaged. Default `false`. |

Blocked calls throw `{type: "EgressBlocked", category: "egress_blocked", host, port, reason, url}`.
Any of these variables being set seeds the egress policy before
`egress_policy(config)` runs, so a script that also declares a policy fails with
`policy already configured from environment`. `harn run` keeps that policy
process-wide. `harn test` seeds an isolated copy for each test pipeline, including
under `--parallel`, so one pipeline cannot alter another's policy.

