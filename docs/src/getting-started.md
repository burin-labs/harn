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

```harn
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
harn run
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

## Call a real provider

Choose a provider and model, set its API key in your shell, then run a small
program. For example:

```bash
export ANTHROPIC_API_KEY=your-key
harn models test claude-sonnet-5 --provider anthropic
```

The test command checks the provider path without requiring a Harn program. To
use the same provider in code, change the options in the example to:

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
