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
harn package list
harn package doctor
```

Before publishing, replace local path dependencies with registry or git
dependencies pinned to a version or rev.

## Create a tool package

```bash
harn tool new acme-echo
cd acme-echo
harn test tests/
harn package check
harn package docs --check
harn package pack --dry-run
```

The tool template creates a package with a Harn-native registry builder,
`[[package.tools]]` metadata, a local smoke test that dispatches the tool,
and CI that exercises the same publish-readiness checks. The generated package
is intentionally ordinary Harn source; consumers install it with `harn add`,
import the stable export, and merge it into their own tool registry:

```harn,ignore
import { tools } from "acme-echo/tools"
```

## Create a skill package

```bash
harn skill new review-helper
```

`harn skill new` is the singular alias for `harn skills new`. Package authors
can publish skills by adding `[[package.skills]]` entries that point at
package-root-relative skill directories containing `SKILL.md`.

## Create a connector package

```bash
harn new connector echo-connector
cd echo-connector
harn connector test .
harn package docs
harn publish --dry-run
```

Connector packages use `harn connector test .` as the CI gate. It runs package
metadata validation, `harn check`, `harn lint`, `harn fmt --check`, connector
contract fixtures, package-local fixture tests, install/import smoke tests, and
standalone Harn doc examples. Use `harn connector check .` when you only need
the lower-level pure-Harn connector contract check.

Connector packages should also declare package-facing capability coverage on
their `[[providers]]` entry so `harn check --connector-matrix` can compare
provider support:

```toml
[[providers]]
id = "acme"
connector = { harn = "src/lib.harn" }
capabilities = ["webhook", "oauth", "rate_limit", "pagination", "graphql", "streaming"]
```

## Manifest metadata

Publishable packages should include:

```toml
[package]
name = "acme-tools"
version = "0.1.0"
description = "Reusable Harn helpers."
license = "MIT OR Apache-2.0"
repository = "https://github.com/acme/acme-tools"
provenance = "https://github.com/acme/acme-tools/releases/tag/v0.1.0"
harn = ">=0.8,<0.9"
docs_url = "docs/api.md"
permissions = ["tool:read_only"]
host_requirements = ["workspace.read_text"]

[exports]
lib = "lib/main.harn"

[[package.tools]]
name = "read-note"
module = "lib/main.harn"
symbol = "tools"
description = "Read a note through the package tool registry."
permissions = ["tool:read_only"]

[package.tools.input_schema]
type = "object"
required = ["path"]

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"

[[package.skills]]
name = "review"
path = "skills/review"

[dependencies]
json-helpers = { git = "https://github.com/acme/json-helpers", rev = "v0.1.0" }
```

`harn package check` validates required metadata, dependency declarations,
stable exports, tool and skill declarations, README/license presence, docs
links, and Harn compatibility. Tool declarations must point at a parseable
Harn module, provide valid JSON-schema-shaped `input_schema` and
`output_schema` tables when present, and use policy annotations that match the
runtime tool annotation schema. Skill declarations must stay inside the package
root and point at a directory with valid `SKILL.md` front matter. Publish
readiness rejects path-only dependencies and unsupported Harn version ranges
because they cannot be reproduced from a registry index.

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

## Lockfile provenance

`harn.lock` is the executable record of what was resolved and why. Every
write captures:

- `version`, `generator_version`, and `protocol_artifact_version` at the
  top level so downstream automation can detect when the lock was produced
  by an older Harn line.
- Per-entry `source`, `commit`, and `content_hash` for git/registry deps,
  plus `package_version`, `harn_compat`, package `provenance`, and a separate
  `manifest_digest` taken from the resolved package's `harn.toml`.
- Exported modules, custom tools, skills, package permissions, and host
  requirements declared by the resolved package. Hosts and CI can inspect this
  metadata without reading arbitrary package source.
- A `[package.registry]` table for entries originally added via
  `harn add @scope/name@version`, preserving the registry source, package
  name, and requested version so `harn package outdated` can compare
  against the registry's latest release without re-reading the manifest.

Path dependencies remain live-linked; their lockfile entry records the
resolved `path+file://` URI plus the same `package_version` and
`manifest_digest` so `harn package audit` flags drift without rebuilding.

## Maturing the package surface

Once a manifest and lockfile are in place, these commands cover the
day-to-day automation surface:

- `harn package list` — summarize locked packages, materialization status,
  exported modules/tools/skills, and declared permission or host requirements.
  Use `--json` when the output feeds host UI or CI policy.
- `harn package doctor` — diagnose missing or stale lockfiles, missing
  materialized packages, content-hash mismatches, declared host capability
  gaps, and invalid installed package tool/skill metadata. It is safe for
  applications: publish-only checks remain under `harn package check`.
- `harn package outdated` — surface registry version bumps and (with
  `--remote`) git branch HEAD drift. JSON output matches the report
  struct documented in
  [`cli-reference.md`](cli-reference.md#harn-package-outdated).
- `harn package audit` — verify provenance, compatibility, and content
  integrity in one pass. `--json` returns a stable `code` per finding so
  CI can fail on specific issues (yanked registry pins, content-hash
  tampering, broken `harn` ranges) without parsing prose.
- `harn package artifacts check` — compare a vendored copy of the
  protocol artifact manifest (e.g. `spec/protocol-artifacts/manifest.json`
  in your repo) against the running Harn. The exit code is non-zero on
  drift, so a downstream that vendors generated TypeScript or Swift
  bindings can fail CI when those bindings would change after a Harn bump.

## Cross-repo bump workflow

When Harn cuts a new release, downstream package authors and host
integrators (Burin Code, Harn Cloud, third-party connectors) need a
consistent way to land the bump. The recommended sequence:

1. **Refresh the running Harn binary** in the downstream repo
   (`fetch-harn`, `make setup`, or whatever the local script is).
2. **Re-resolve dependencies** with `harn install` so `harn.lock`
   rewrites with the current `generator_version`,
   `protocol_artifact_version`, exported package surface, and host
   requirements.
3. **Inspect** with `harn package list --json` and `harn package doctor --json`
   so host requirements and materialized package state are visible before
   tests start.
4. **Audit** with `harn package audit --json`. If any package declares a
   `harn` range that excludes the new line, contact the package owner or
   pin to a previous tag.
5. **Check vendored protocol bindings** with
   `harn package artifacts check path/to/vendored/manifest.json`. If it
   reports drift, regenerate the bindings (`make gen-protocol-artifacts`
   inside the downstream repo, or `harn package artifacts manifest
   --output …` to refresh the vendored copy).
6. **Surface registry updates** with `harn package outdated --json` and
   bump dependency versions intentionally; `harn update <alias>` keeps
   each upgrade in its own commit.

Steps 2–5 are a single CI job for hosts that vendor Harn artifacts:
`harn install --frozen --json`, then `harn package doctor --json`, then
`harn package audit --json`, then `harn package artifacts check`. The `--json`
output is stable across Harn lines, so the same automation script runs against
multiple Harn versions during a staged rollout.
