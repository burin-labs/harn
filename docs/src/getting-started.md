# Getting started

Install Harn, run an agent, call a tool, and replay a recorded model response.
This tutorial uses synthetic responses, so you don't need provider credentials.

## Install Harn

### macOS and Linux

```bash
curl -fsSL https://harnlang.com/install.sh | sh
```

The installer downloads the release for your operating system and CPU. To
install a particular release, set `HARN_VERSION` to its release tag. Releases
ship as `.tar.xz`; on Linux the installer needs `xz` (package `xz-utils` on
Debian and Ubuntu, `xz` on Fedora and Alpine). Releases through v0.10.178 ship
`.tar.gz` and need the `install.sh` from their own tag.
The installer retains project licenses and third-party notices in
`harn-licenses/` beside the binary, including at custom `HARN_INSTALL_DIR`
destinations. It refuses releases with missing, empty, or invalid legal files
before changing an existing installation.

### Windows

Run this command in PowerShell:

```powershell
irm https://harnlang.com/install.ps1 | iex
```

### From source

See [Contributing](https://github.com/burin-labs/harn/blob/main/CONTRIBUTING.md)
for source builds and development prerequisites. This tutorial uses the
installed release.

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

The generated `.gitignore` keeps local run history out of commits. Source and
test fixtures remain trackable. Existing ignore files are preserved.
Initialization reports conflicting paths or write failures and exits unsuccessfully;
it keeps existing files and any files already created so you can inspect them.

Replace `main.harn` with:

```harn,check
import { agent_loop } from "std/agent/loop"
import { AgentSpec } from "std/agent/options"

fn main(harness: Harness) {
  const options: AgentSpec = {provider: "mock", max_iterations: 2}
  harness.stdio.println("Running: Say hello.")
  const result = agent_loop(harness, "Say hello.", nil, options)
  harness.stdio.println(result.status)
  harness.stdio.println(result.visible_text)
}
```

Run it:

```bash
harn run main.harn
```

```text
Running: Say hello.
done
Mock response to 2-word prompt: Say hello.
```

The loop accepts a task, optional system prompt, and options. `max_iterations`
bounds model requests; `done` means the loop completed. The mock provider
supplies deterministic output without a model request. It doesn't measure
how well a real model follows your instructions.

## Check your program

Run these commands before you commit:

```bash
harn fmt main.harn
harn check main.harn
harn lint main.harn
```

`fmt` applies the formatter. `check` validates syntax and types. `lint` finds
common problems and style issues.

## Call a tool

Get the runnable examples without building Harn from source:

```sh
git clone --depth 1 https://github.com/burin-labs/harn.git harn-examples
cd harn-examples/examples/first-hour
harn run tools.harn --llm-mock tools.jsonl
```

```text
tool: 2 + 3 = 5
done
2 + 3 = 5.
```

Open `tools.harn`. The `add` tool declares integer inputs and an integer
result. The fixture requests the tool, Harn calls the handler, and the next
model response ends the loop. The `tool:` line comes from the handler itself.
This tool only calculates a sum. File and process effects require the
corresponding harness capabilities.

## Record and replay

From the same directory:

```sh
harn run record-replay.harn --llm-mock-record responses.jsonl
harn run record-replay.harn --llm-mock responses.jsonl
```

Each command prints:

```text
Mock response to 2-word prompt: Say hello.
```

The first command records a synthetic response. The second supplies that
recording through the model-call interface. It doesn't recreate the
filesystem or replay tool effects. Keep recordings local until you've checked
them for sensitive data. Delete `responses.jsonl` to discard this recording.

## Use a real model

Follow [Configure a provider](./provider-setup.md) to choose a local or hosted
model and set its credentials. Replace the example's `provider: "mock"`
options with that route, and omit `--llm-mock` when you want a real request.
Keep credentials out of Harn source and version control.

Continue with [Agent loops](./llm/agent_loop.md) for tools and execution limits,
or [Language basics](./language-basics.md) for syntax.
