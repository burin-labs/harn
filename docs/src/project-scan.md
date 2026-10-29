# Project scanning

The `std/project` module now includes a deterministic L0/L1 project scanner for
lightweight "what kind of project is this?" evidence without any LLM calls.

Import it with:

```harn
import "std/project"
```

## Fast fingerprint

`harness.project.fingerprint(path?)` returns the fast normalized repo profile that
skill-card and persona bootstraps can consume without paying for enrichment:

```harn
const fp = harness.project.fingerprint(".")
```

Typical fields:

- `primary_language`: `"rust"`, `"typescript"`, `"python"`, `"go"`,
  `"swift"`, `"ruby"`, `"mixed"`, or `"unknown"`
- `frameworks`: normalized coarse framework tags such as `"axum"`, `"next"`,
  `"react"`, `"django"`, `"fastapi"`, or `"rails"`
- `package_manager`: dominant normalized package-manager tag such as
  `"cargo"`, `"spm"`, `"pnpm"`, `"npm"`, `"uv"`, `"poetry"`, `"pip"`,
  `"go-mod"`, or `"bundler"`
- `test_runner`: dominant normalized test-runner tag such as `"nextest"`,
  `"cargo-test"`, `"vitest"`, `"pytest"`, `"go-test"`, or `"xctest"`
- `build_tool`: dominant normalized build-tool tag such as `"cargo"`,
  `"spm"`, `"next"`, `"vite"`, `"uv"`, `"poetry"`, or `"go"`
- `vcs`: `"git"`, `"hg"`, or `nil`
- `ci`: normalized CI-provider tags such as `"github-actions"`,
  `"gitlab-ci"`, `"circleci"`, `"buildkite"`, `"azure-pipelines"`, or
  `"bitrise"`

Compatibility fields remain available for callers that need the full shallow
signal set:

- `languages`
- `package_managers`
- `has_tests`
- `has_ci`
- `lockfile_paths`

Normalization rules:

- Tags are lowercase and versionless.
- Singular fields choose the dominant value in a stable precedence order, while
  the plural fields preserve every detected tag.
- The catalog is local to Harn so `harness.project.fingerprint(...)` remains fast and
  self-contained; downstream consumers consume the stable output tags rather
  than acting as a runtime dependency for detection.

## Makefile declarations

`harness.scanner.scan_project(...)` and the config tier of
`harness.project.scan(...)` select the first default file in
`makefile_filenames()`: `GNUmakefile`, `makefile`, then `Makefile`. They read
literal target declarations without evaluating Make or expanding variables.
Multi-target rules contribute each declared target; assignments and `define`
bodies do not create targets. Test-shaped targets contribute their generated
commands to the scanner's `test_command_discovery.commands` map. The config tier exposes all
literal names in `makefile_targets` and build/test invocations in `build_commands`.

`makefile_parse(text)` exposes the same parser to source-facing adapters. Each
record has `name`, a shell-quoted `command`, `recipe_preview`, and
`recipe_line_count`. The preview retains at most three lines of 512 characters
each; the count includes all recipe lines. These are declared source facts,
not evidence that a command ran or passed. Variable-generated targets and
pattern rules are not resolved by this static parser.

The scanner's `test_command_discovery` is a tagged result. `complete` carries
the discovered `commands` and nullable `preferred` command; an empty map means
the search completed without candidates. `unavailable` carries a short `detail`
instead of a command map when the search could not inspect a relevant path.
Candidates describe possible commands, not proof that a command ran or passed.

```harn
const targets = makefile_parse("unit test:\n\t@echo checked\n")
assert_eq(targets[1]?.command, "make test")
assert_eq(targets[1]?.recipe_preview, ["echo checked"])
```

## Context profiles

`harness.project.context_profile(path?, options?)` turns project signals into the prompt,
skill, and tool preset profile that an agent should activate for the current
workspace:

```harn
import "std/project"

const profile = harness.project.context_profile(
  ".", {include_env_credentials: false},
)
```

The resolver composes existing signals instead of indexing code itself. It uses
the shallow `harness.project.fingerprint(...)` result when a caller has not supplied one,
reads Git remote configuration directly, and checks credential availability only
by normalized alias such as `"github"`; secret values are never returned.

Typical fields:

- `profile_ids`: stable active profile IDs such as `"git"`, `"github"`,
  `"rust"`, `"node"`, `"python"`, or `"swift"`
- `prompt_fragments`: reducer-ready fragments with `id`, `source`, `body`, and
  `requires_caps`
- `skills`: skill IDs that should be active when present in the skill registry
- `tool_groups`: host/tooling groups such as `"git"` or `"cargo"` that a client
  can use for preset tool surfaces
- `mcp_presets`: MCP preset IDs whose prerequisites are already satisfied
- `mcp_preset_candidates`: candidate preset IDs plus `status` and missing
  credential keys when prerequisites are not satisfied
- `caps`: capability flags that gate the prompt fragments
- `signals`: the normalized fingerprint, redacted remote, signal source, and
  credential aliases used for the decision
- `token_delta`: estimated prompt-token/byte cost for activated fragments versus
  the full always-on profile catalog

`agent_loop_options(...)` resolves a context profile automatically from
`context_profile_root`, `project_root`, `root`, `cwd`, or `"."` unless the caller
already supplied `context_profile`/`project_context_profile` or set
`auto_context_profile: false`. Pass `context_profile_options` through
`agent_loop_options(...)` to forward resolver options.

Callers with a pre-existing code librarian or host index signal should pass it
instead of forcing another project scan:

```harn
import { agent_loop_options } from "std/agent/options"

const _opts = agent_loop_options({
  root: ".",
  code_librarian_signals: {
    source: "code_librarian",
    fingerprint: {primary_language: "rust", languages: ["rust"]},
    remote: "https://github.com/burin-labs/harn.git",
  },
})
```

The same profile can be handed to `harness.agent.prompt_explain(...)`; the
resulting
provenance shows each `profile:*` fragment and the capability that caused it to
be included.

## What it returns

`harness.project.scan(path, options?)` resolves `path` to a directory and returns a
dictionary describing exactly that directory:

```harn
import "std/project"

const ev = harness.project.scan(".", {tiers: ["ambient", "config"]})
```

Typical fields:

- `path`: absolute path to the scanned directory
- `languages`: stable, confidence-filtered language IDs such as `["rust"]`
- `frameworks`: coarse framework IDs when an anchor is obvious
- `build_systems`: coarse build systems such as `["cargo"]` or `["npm"]`
- `vcs`: currently `"git"` when the directory is inside a Git checkout
- `anchors`: anchor files or directories found at the project root
- `lockfiles`: lockfiles found at the project root
- `confidence`: coarse per-language/per-framework scores
- `package_name`: root package/module name when it can be parsed deterministically

When `tiers` includes `"config"`, the scan also fills in:

- `build_commands`: default or discovered build/test commands
- `declared_scripts`: parsed `package.json` scripts
- `makefile_targets`: parsed Makefile targets
- `dockerfile_commands`: parsed `RUN`, `CMD`, and `ENTRYPOINT` commands
- `readme_code_fences`: fenced-language labels found in the README

### Package-script commands

The project scanner and hostlib scanner resolve commands from the same
string-valued `package.json` scripts. Each manifest uses the nearest supported
`packageManager` declaration or lockfile within the requested scan root. A
nested manifest's own evidence takes precedence over inherited evidence. A
manifest with no manager evidence uses npm.

Explicit declarations require an exact semantic version, including valid
prerelease or build metadata. Ranges, tags and malformed versions are refused.
Manifest files and inherited manifest evidence must physically remain within
the scan root; legitimate file aliases within that root are accepted. Manager
lockfiles and inherited evidence are resolved from the physical manifest's
parent, so an alias cannot detach scripts from their package manager. Cyclic
inherited aliases are refused as invalid ancestor evidence.

Supported managers are npm, pnpm, yarn and bun. npm and pnpm invoke the test
lifecycle as `npm test` and `pnpm test`; their other scripts, and yarn and bun
scripts, use `<manager> run <script>`. Config-tier scans project the actual
declared scripts through this same selection. Ambient scans do not parse the
manifest or propose package-script commands.

Unsupported or malformed declarations, conflicting manager evidence at one
directory, and non-string script definitions produce no package-script command
proposals. The repository fingerprint retains the aggregate manager signals;
individual script commands use their own manifest's resolved manager.

`package_scripts` carries the normalized inventory for command-planning
consumers. Its `state` is `resolved`, `absent` when a requested config scan finds
no package manifest, `refused` when manifest or manager evidence is invalid, or
`not_requested` when the config tier was not requested. `manager` names the
resolved manager, `commands` maps script names to invocation strings, and
`error` gives the refusal code. Absent and refused results have no manager or
commands. A `not_requested` result also has no manager or commands; it does not
claim the manifest is absent. Consumers use this inventory directly and keep
refusal distinct from an empty valid script inventory.

## Tiers

- `ambient`: anchor files, lockfiles, coarse build system detection, VCS, and
  confidence scoring. No config parsing.
- `config`: deterministic config reads for files already found by `ambient`.

If `tiers` is omitted, `harness.project.scan(...)` defaults to `["ambient"]`.

## Polyglot repos

Single-directory scans stay leaf-scoped on purpose. For polyglot repos and
monorepos, use `harness.project.scan_tree(...)` and let callers decide how to combine
sub-project evidence:

```harn
import "std/project"

const tree = harness.project.scan_tree(
  ".", {tiers: ["ambient"], depth: 3},
)
// {".": {...}, "frontend": {...}, "backend": {...}}
```

`harness.project.scan_tree(...)`:

- always includes `"."` for the requested base directory
- walks subdirectories deterministically
- honors `.gitignore` and `.agentignore` by default (not `.ignore`)
- skips standard vendor/build directories such as `node_modules/` and `target/`
  by default

You can override those defaults with:

- `ignore_policy: "builtin"` to keep the built-in directory skips but stop
  reading project ignore files, or `ignore_policy: "none"` for a raw walk
- `include_vendor: true`
- `include_hidden: true`

## Enrichment

`harness.project.enrich(path, options)` layers an L2, caller-owned enrichment pass on
top of deterministic `harness.project.scan(...)` evidence. The caller supplies the
prompt template and the output schema; Harn owns prompt rendering, bounded file
selection, schema-retry plumbing, and content-hash caching.

Typical use:

```harn
import "std/project"

const base = harness.project.scan(".", {tiers: ["ambient", "config"]})
const enriched = harness.project.enrich(".", {
  base_evidence: base,
  prompt: "Project: {{package_name}}\n"
    + "{{ for file in files }}FILE"
      + " {{file.path}}\n{{file.content}}\n{{ end }}\n"
    + "Return JSON.",
  schema: {
    type: "object",
    required: ["framework", "indent_style"],
    properties: {
      framework: {type: "string"},
      indent_style: {type: "string"},
    },
  },
  budget_tokens: 4000,
  model: "auto",
  cache_key: "coding-enrichment-v1",
})
```

Bindings available to the template:

- `path`: absolute project path
- `base_evidence` / `evidence`: the supplied or auto-scanned L0/L1 evidence
- every top-level key from `base_evidence`
- `files`: deterministic bounded file context as `{path, content, truncated}`

`harness.project.enrich(...)` now also augments the evidence with a deterministic `ci`
block unless `include_operator_meta: false` is set in the options. This is
intended to surface the "operator meta-knowledge" a human reviewer picks up
quickly in a new repo:

- `ci.workflows`: `.github/workflows/*.yml` / `.yaml` with per-job
  classifications such as `lint`, `test`, `build`, and `release`
- `ci.hooks`: `.githooks/*`, `.pre-commit-config.yaml`, `lefthook.yml`, and
  `.husky/*` collapsed into stage → command summaries
- `ci.package_manifests`: detected manifests + lockfiles with CI cache/tooling
  hints such as `rust-cache action` or `cargo-nextest installed`
- `ci.merge_policy`: CODEOWNERS, CONTRIBUTING merge-method hints, and GitHub
  branch-protection data when `gh` is installed and authenticated

Typical shape:

```json
{
  "ci": {
    "workflows": [
      {
        "path": ".github/workflows/ci.yml",
        "name": "CI",
        "jobs": [
          {
            "name": "Rust (lint + test + conformance)",
            "classifications": ["lint", "test"],
            "required_check": true
          }
        ]
      }
    ],
    "hooks": {
      "stages": {
        "pre-commit": ["cargo fmt --all", "cargo clippy --workspace -- -D warnings"]
      }
    },
    "package_manifests": [
      {
        "ecosystem": "cargo",
        "manifests": ["Cargo.toml"],
        "lockfiles": ["Cargo.lock"],
        "ci_hints": ["rust-cache action"]
      }
    ],
    "merge_policy": {
      "required_checks": ["Format check", "Rust (lint + test + conformance)"],
      "squash_only": true
    }
  }
}
```

Behavior:

- cache key includes `cache_key`, path, schema, rendered prompt, and the content
  hash of the selected files
- cached hits surface `_provenance.cached == true`
- when the rendered prompt would exceed `budget_tokens`, the call returns the
  base evidence with `budget_exceeded: true` instead of failing
- schema-retry exhaustion returns an envelope with `validation_error` and
  `base_evidence` instead of raising
- workflow/hook/policy files are prioritized in the bounded `files` context so
  operator-facing enrichment prompts see CI + merge-policy inputs before generic
  source snippets

By default, cache entries live under `.harn/cache/enrichment/` inside the
project root. Override that with `cache_dir` when a caller wants a different
location.

## Cached deep scans

`project_deep_scan(path, options?)` layers a cached per-directory tree on top
of the metadata store. It is intended for repeated L2/L3 repo analysis where
callers want stable hierarchical evidence instead of re-running enrichment on
every turn.

Typical shape:

```harn
import "std/project"

const tree = project_deep_scan(".", {
  namespace: "coding-enrichment-v1",
  tiers: ["ambient", "config", "enriched"],
  incremental: true,
  max_staleness_seconds: 86400,
  depth: nil,
  enrichment: {
    prompt: "Return valid JSON only.",
    schema: {purpose: "string", conventions: ["string"]},
    provider: "mock",
    budget_tokens_per_dir: 1024,
  },
})
```

Notes:

- `namespace` is caller-owned, so multiple agents can keep separate trees for
  the same repo without collisions.
- `incremental: true` reuses cached directories whose local directory
  `structure_hash` and `content_hash` still match.
- `depth: nil` means unbounded traversal.
- The filesystem backend persists namespace shards under
  `.harn/metadata/<namespace>/entries.json`.
- `project_deep_scan_status(namespace, path?)` returns the last recorded scan
  summary for that scope: `{total_dirs, enriched_dirs, stale_dirs, cache_hits,
  last_refresh, ...}`.

`harness.project.enrich(path, options?)` is the single-directory building block used by
deep scan when the `enriched` tier is requested.

## Catalog

`harness.project.catalog()` returns the authoritative built-in catalog that drives
ambient detection. Each entry includes:

- `id`
- `languages`
- `frameworks`
- `build_systems`
- `anchors`
- `lockfiles`
- `source_globs`
- `default_build_cmd`
- `default_test_cmd`

Package-script entries have no default commands: a context-free catalog cannot
know whether a manifest declares those scripts. Use config-tier scan commands.

The catalog lives in
`crates/harn-builtins/src/project_catalog.rs`. Adding a new language should be
a table entry plus a test, not a new custom code path.

## Existing helper

`project_root_package()` now delegates to the scanner's config tier after
checking metadata enrichment, so existing callers keep the same package-name
surface while the manifest parsing logic stays centralized.
