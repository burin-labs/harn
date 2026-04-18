# Project scanning

The `std/project` module now includes a deterministic L0/L1 project scanner for
lightweight "what kind of project is this?" evidence without any LLM calls.

Import it with:

```harn
import "std/project"
```

## What it returns

`project_scan(path, options?)` resolves `path` to a directory and returns a
dictionary describing exactly that directory:

```harn
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
