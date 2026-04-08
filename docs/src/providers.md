# Configuring LLM Providers

Harn supports multiple LLM providers out of the box. This page explains how
provider and API key resolution works, and how to configure each one.

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

## Local LLM support

For local models (Ollama, llama.cpp, vLLM, etc.):

```bash
export LOCAL_LLM_BASE_URL=http://localhost:11434
export LOCAL_LLM_MODEL=llama3
```

Harn will automatically fall back to a local provider if no cloud API key
is configured. This makes it easy to develop and test without incurring
API costs.

## Troubleshooting

- **"No API key found"** — Check that the correct environment variable is
  set for your provider. Run `echo $ANTHROPIC_API_KEY` to verify.
- **Wrong provider selected** — Set `HARN_LLM_PROVIDER` explicitly to
  override automatic detection.
- **Rate limit errors** — Use `HARN_RATE_LIMIT_<PROVIDER>` to throttle
  requests below your plan's limit.
- **Debug message shapes** — Set `HARN_DEBUG_MESSAGE_SHAPES=1` to log
  the structure of messages sent to the LLM provider.
