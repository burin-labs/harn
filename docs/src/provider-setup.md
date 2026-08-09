# Configure a model provider

This guide helps you connect Harn to a model provider. It covers the checks
you can run before you put a provider in a program.

## 1. Check the local installation

Run the doctor command first:

```bash
harn doctor
harn doctor --check-providers
```

The first command reports the local Harn setup. The second also checks the
configured provider paths. It does not print secret values.

## 2. Find a provider and model

List the models that Harn knows about:

```bash
harn models list
harn models list --provider anthropic
```

Inspect one model before you use it:

```bash
harn models info claude-sonnet-5
harn provider dispatch-explain anthropic claude-sonnet-5
```

The catalog is the source of truth for current aliases and capabilities. Do
not copy an old model name from an example when the catalog gives you a newer
one.

## 3. Set the provider credential

Set the credential in your shell or secret manager. Common cloud providers use
these variables:

| Provider | Environment variable |
|---|---|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Google Gemini | `GEMINI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Together AI | `TOGETHER_AI_API_KEY` |
| DeepInfra | `DEEPINFRA_API_KEY` |
| NVIDIA NIM | `NVIDIA_API_KEY` |
| Hugging Face | `HUGGINGFACE_API_KEY` |

The exact provider configuration can change. Use the provider reference for
[API details](./llm/providers.md#provider-api-details), or run
`harn doctor --check-providers` after setting the variable.

## 4. Test the connection

Use the model test command for a small smoke test:

```bash
harn models test claude-sonnet-5 --provider anthropic
```

The command sends a test request. It can use provider credits. Use a model
that is available to your account.

## 5. Use the provider in a program

Keep the provider and model in the call options:

```harn
fn main(harness: Harness) {
  const response = harness.llm.call(
    "Reply with one short greeting.",
    nil,
    { provider: "anthropic", model: "claude-sonnet-5", max_tokens: 64 }
  )
  harness.stdio.println(response.text)
}
```

For a single project, put stable defaults in `harn.toml` when the provider
reference says the setting is supported. Keep credentials out of that file.

## Local providers

Start your local server first, then use the provider's name and model from the
catalog:

```bash
harn models list --provider ollama
harn provider ready ollama --model <model>
```

For an OpenAI-compatible server, check the endpoint and model settings in the
[provider reference](./llm/providers.md#local-openai-compatible-server).

## Use the mock provider in tests

The `mock` provider needs no credentials and returns deterministic responses.
Use it for syntax checks, unit tests, and examples. A mock run proves that the
program reaches Harn's model-call boundary; it does not prove that a cloud
provider is configured or that a model produces useful answers.

See [mock LLM responses](./llm/llm_call.md#testing-with-mock-llm-responses) for
queued responses and error cases.
