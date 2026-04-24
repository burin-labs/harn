# Package authoring

Harn packages are ordinary Harn projects with package metadata, stable exports,
tests, and optional connector contracts in `harn.toml`. They use the same
`[dependencies]`, `.harn/packages/`, and `harn.lock` workflow as applications.

## Create a package

```bash
harn new package acme-tools
cd acme-tools
harn test tests/
harn package check
harn package docs
harn package pack
```

The package template creates:

- `harn.toml` with `[package]` metadata, `[exports]`, and `[dependencies]`
- `lib/main.harn` with a documented public function
- `tests/` smoke tests
- `README.md`, `LICENSE`, `docs/api.md`
- CI that installs `harn-cli`, runs tests, checks docs drift, and runs
  `harn package pack --dry-run`

Consumers can add a local package while developing:

```bash
harn add ../acme-tools
harn install
```

Before publishing, replace local path dependencies with registry or git
dependencies pinned to a version or rev.

## Create a connector package

```bash
harn new connector echo-connector
cd echo-connector
harn connector check .
harn test tests/
harn package check
harn package docs
harn publish --dry-run
```

Connector packages use the same package checks plus `harn connector check .` for
the pure-Harn connector contract. First-party and community connectors should
keep this CI shape so package consumers get the same authoring workflow.

## Manifest metadata

Publishable packages should include:

```toml
[package]
name = "acme-tools"
version = "0.1.0"
description = "Reusable Harn helpers."
license = "MIT OR Apache-2.0"
repository = "https://github.com/acme/acme-tools"
harn = ">=0.7,<0.8"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[dependencies]
json-helpers = { git = "https://github.com/acme/json-helpers", rev = "v0.1.0" }
```

`harn package check` validates required metadata, dependency declarations,
stable exports, README/license presence, docs links, and Harn compatibility.
Publish readiness rejects path-only dependencies and unsupported Harn version
ranges because they cannot be reproduced from a registry index.

## API docs

Document exported symbols with doc comments:

```harn
/// Return a greeting for `name`.
pub fn greet(name: string) -> string {
  return "Hello, " + name + "!"
}
```

Generate docs with:

```bash
harn package docs
```

CI should use:

```bash
harn package docs --check
```

## Pack and publish dry run

`harn package pack` validates the package and writes an inspectable artifact
directory at `.harn/dist/<name>-<version>`. It excludes local build state such
as `.git/`, `.harn/`, `target/`, and `node_modules/`.

```bash
harn package pack --dry-run
harn package pack
```

`harn publish --dry-run` runs the same publish-readiness checks and reports the
registry target that would receive the submission. Real registry submission is
reserved for the registry/index workflow.
