# Configure a model provider

This guide helps you connect Harn to a model provider. It covers the checks
you can run before you put a provider in a program.

Before entering a credential, run `harn --version`. Quickstart's Git and Unix
file-permission safeguards require Harn 0.10.192 or later.

## 1. Create starter configuration

Run setup in your project:

```bash
harn quickstart
set -a; . ./.env; set +a
```

In Bash, Zsh, or a POSIX shell, this loads `.env` and exports its assignments
to commands such as `harn doctor` and `harn run`.

The command inspects credential paths and local Ollama models, lets you choose
a provider and model, and writes starter defaults to `harn.toml`, `.env`, and
your user `providers.toml`. Existing credentials and configured defaults stay
intact. Provider discovery and configuration writes happen in this explicit
setup command.

For a terminal without prompts, name the provider:

```bash
harn quickstart --non-interactive --provider openai
```

When a call specifies no provider or model, Harn selects the first route in the
catalog's `automatic_defaults` whose provider has a credential. Explicit call
options, session selections, and configured defaults take precedence. If no
automatic credential is available, the call reports setup guidance; configure
a local server with quickstart to use it as your default.

## 2. Check the local installation

Run the doctor command first:

```bash
harn doctor
harn doctor --check-providers
```

The first command reports the local Harn setup. The second also checks the
configured provider paths. It does not print secret values.

## 3. Find a provider and model

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

## 4. Set the provider credential

For guided setup, run `harn quickstart` in your project directory. Quickstart
adds `/.env` to the project's `.gitignore`, preserves existing ignore rules,
and writes `.env` with owner-only permissions on Unix. It refuses an environment
file already tracked by Git. On Windows, the file inherits the directory's
access permissions.

Each provider reads its key from an environment variable. Set the one for the
provider you picked:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

Harn names these providers first, because most people already have an account
with one of them:

| Provider | Environment variable |
|---|---|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Google Gemini | `GEMINI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Groq | `GROQ_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| Ollama | none — runs locally without a key |

Harn supports dozens more, and some accept more than one variable. For every
provider and the variables it reads, see
[credential variables](./provider-support.md#credential-variables); for the
endpoint and header details behind them, see the
[provider reference](./llm/providers.md#provider-api-details). To see which
variables are already set on this machine, run `harn doctor`.

### Keep the key out of your shell

A variable can hold a secret reference instead of the key itself:

```bash
export ANTHROPIC_API_KEY="harn-secret://work/anthropic"
```

Harn resolves the reference when it makes the call, so the key never lands in
your shell history or a config file. See
[secrets](./orchestrator/secrets.md) for how to store one.

After you set the variable, confirm the provider resolves:

```bash
harn doctor --check-providers
```

That command reports which providers have a working credential path. It never
prints a secret value.

## 5. Test the connection

Use the model test command for a small smoke test:

```bash
harn models test claude-sonnet-5 --provider anthropic
```

The command sends a test request. It can use provider credits. Use a model
that is available to your account.

## 6. Use the provider in a program

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
