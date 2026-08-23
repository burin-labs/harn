# Platform support

Which operating systems and processors Harn runs on, and what "supported"
means for each.

## Prebuilt binaries

The installer picks one of these automatically. Every release publishes all
five.

| Target | Operating system | Processor |
|---|---|---|
| `aarch64-apple-darwin` | macOS | Apple silicon |
| `x86_64-apple-darwin` | macOS | Intel |
| `aarch64-unknown-linux-gnu` | Linux (glibc) | ARM64 |
| `x86_64-unknown-linux-gnu` | Linux (glibc) | x86-64 |
| `x86_64-pc-windows-msvc` | Windows | x86-64 |

Two gaps worth naming rather than leaving you to discover them:

- **Windows on ARM has no prebuilt binary.** Build from source, or run the
  x86-64 build under emulation.
- **Linux builds link against glibc**, so musl-only distributions such as
  Alpine need a source build.

See [Getting started](./getting-started.md) for the install commands.

## Building from source

Any platform with a working Rust toolchain can build Harn, including ones with
no published binary:

```bash
git clone https://github.com/burin-labs/harn.git
cd harn
make setup
```

A source build is the supported path for the gaps above. It is not a lesser
tier: the published binaries are built from the same workspace.

## WebAssembly

Harn builds a `wasm32-unknown-unknown` module, but be precise about what that
gives you. The WebAssembly target runs **portable reducers**: self-contained
Harn modules with no host capabilities, compiled into a package and executed in
a browser worker.

It does not run arbitrary Harn programs. Anything that reaches for the
filesystem, the network, a subprocess, or a model provider needs a host, and
the browser is not one today. Use it for deterministic, pure computation you
want to run next to a user interface.

See [Run a portable reducer in a browser](./portable-kernel-browser.md) and
[Portable execution](./concepts/portable-execution.md).

## Embedding in another program

Harn is a Rust workspace, so a Rust application can embed the runtime directly
rather than shelling out to the CLI. See [Embedding in
Rust](./embedding-rust.md).

## Editors

Harn ships LSP and DAP servers, so editor support does not depend on a
per-editor plugin being written first. See [Editor
setup](./editor-setup.md) for VS Code, Neovim, and Zed.

## Models

Platform support and model support are different questions. Harn talks to
hosted providers, OpenAI-compatible endpoints, and local runtimes, and which
local models are practical depends on your hardware rather than your operating
system. Run `harn models recommend` to get a specific answer for the machine
you are on, and see [Configure a provider](./provider-setup.md).
