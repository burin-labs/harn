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

