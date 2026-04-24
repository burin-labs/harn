# Project scanning

The `std/project` module now includes a deterministic L0/L1 project scanner for
lightweight "what kind of project is this?" evidence without any LLM calls.

Import it with:

```harn
import "std/project"
```

## Fast fingerprint

`project_fingerprint(path?)` returns the fast normalized repo profile that
skill-card and persona bootstraps can consume without paying for enrichment:

```harn
let fp = project_fingerprint(".")
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
- The catalog is local to Harn so `project_fingerprint(...)` remains fast and
  self-contained; downstream repos such as burin-code consume the stable output
  tags rather than acting as a runtime dependency for detection.

## What it returns

`project_scan(path, options?)` resolves `path` to a directory and returns a
dictionary describing exactly that directory:

```harn
import "std/project"

let ev = project_scan(".", {tiers: ["ambient", "config"]})
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

## Tiers

- `ambient`: anchor files, lockfiles, coarse build system detection, VCS, and
  confidence scoring. No config parsing.
- `config`: deterministic config reads for files already found by `ambient`.

If `tiers` is omitted, `project_scan(...)` defaults to `["ambient"]`.

## Polyglot repos

Single-directory scans stay leaf-scoped on purpose. For polyglot repos and
monorepos, use `project_scan_tree(...)` and let callers decide how to combine
sub-project evidence:

```harn
import "std/project"

let tree = project_scan_tree(".", {tiers: ["ambient"], depth: 3})
// {".": {...}, "frontend": {...}, "backend": {...}}
```

`project_scan_tree(...)`:

- always includes `"."` for the requested base directory
- walks subdirectories deterministically
- honors `.gitignore` by default
- skips standard vendor/build directories such as `node_modules/` and `target/`
  by default

You can override those defaults with:

- `respect_gitignore: false`
- `include_vendor: true`
- `include_hidden: true`

## Enrichment

`project_enrich(path, options)` layers an L2, caller-owned enrichment pass on
top of deterministic `project_scan(...)` evidence. The caller supplies the
prompt template and the output schema; Harn owns prompt rendering, bounded file
selection, schema-retry plumbing, and content-hash caching.

Typical use:

```harn
import "std/project"

let base = project_scan(".", {tiers: ["ambient", "config"]})
let enriched = project_enrich(".", {
  base_evidence: base,
  prompt: "Project: {{package_name}}\n{{ for file in files }}FILE {{file.path}}\n{{file.content}}\n{{ end }}\nReturn JSON.",
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

`project_enrich(...)` now also augments the evidence with a deterministic `ci`
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

let tree = project_deep_scan(".", {
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

`project_enrich(path, options?)` is the single-directory building block used by
deep scan when the `enriched` tier is requested.

## Catalog

`project_catalog()` returns the authoritative built-in catalog that drives
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

The catalog lives in
`crates/harn-vm/src/stdlib/project_catalog.rs`. Adding a new language should be
a table entry plus a test, not a new custom code path.

## Existing helper

`project_root_package()` now delegates to the scanner's config tier after
checking metadata enrichment, so existing callers keep the same package-name
surface while the manifest parsing logic stays centralized.
