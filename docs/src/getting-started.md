# Getting started

Install Harn, create a project, and run a program without an API key.

## Install Harn

### macOS and Linux

```bash
curl -fsSL https://harnlang.com/install.sh | sh
```

The installer downloads the release for your operating system and CPU. To
install a particular release, set `HARN_VERSION` to its release tag.

### Windows

Run this command in PowerShell:

```powershell
irm https://harnlang.com/install.ps1 | iex
```

### From source

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
make setup
```

Check the installation:

```bash
harn --version
```

## Create a project

The project generator creates a `harn.toml`, a program, a library directory,
and a test directory.

```bash
harn init hello-harn
cd hello-harn
```

For a small first program, replace `main.harn` with:

```harn,check
fn main(harness: Harness) {
  const response = harness.llm.call(
    "Say hello in one short sentence.",
    nil,
    { provider: "mock" }
  )
  harness.stdio.println(response.text)
}
```

Run it:

```bash
harn run main.harn
```

The mock provider is deterministic and needs no network access or API key. Use
it while you learn the language and write tests.

## Check your program

Run these commands before you commit:

```bash
harn fmt main.harn
harn check main.harn
harn lint main.harn
```

`fmt` applies the formatter. `check` validates syntax and types. `lint` finds
common problems and style issues.

## Pick a model for your machine

Before you choose a provider, let Harn choose one for you. `harn models
recommend` measures free memory, GPU, and disk, checks which provider
credentials it can find, and names one model to start with:

```bash
harn models recommend
```

```text
vertex/claude-sonnet-4-6
17 GB free, MPS available, cloud creds available -> vertex/claude-sonnet-4-6 (local installable route available: devstral-small-2)
```

The first line is the model. The second is the reasoning: free memory, GPU,
whether a cloud credential was found, and — in parentheses — the other route
you could take. Your output will differ, because the answer depends on your
hardware and on which credentials are already in your environment.

With no cloud credentials at all, every recommendation is a local model, so
this works as a first command even before you have signed up for anything.

## Run a local model

If the recommendation is a local model, or you want the local route it offered
as an alternative, install it:

```bash
harn models install devstral-small-2
```

For an Ollama model that pulls the weights. For llama.cpp, MLX, or vLLM it
prints the exact download and launch steps for your platform instead of
downloading anything. `harn local list` then shows every local runtime Harn
knows about and which models each is serving, and `harn local switch <alias>`
makes one of them the active local model.

## Call a real provider

To use a cloud provider, set its API key in your shell and test the route:

```bash
export ANTHROPIC_API_KEY=your-key
harn models test claude-sonnet-5 --provider anthropic
```

`harn models test` sends one small prompt and reports timing, tokens, and cost.
It works for a local model too — pass the alias you installed above. Either way
it checks the provider path without requiring a Harn program.

To use the same provider in code, change the options in the example to:

```harn
{ provider: "anthropic", model: "claude-sonnet-5" }
```

Do not copy API keys into Harn source or commit them. See [Configure a
provider](./provider-setup.md) for discovery, readiness checks, local models,
and provider-specific details.

## See bundled examples

Harn includes offline demos that do not need an API key:

```bash
harn demo --list
harn demo <scenario>
```

When you know the kind of program you want to build, use [Common tasks](./common-tasks.md).
When you need syntax details, use [Language basics](./language-basics.md).
