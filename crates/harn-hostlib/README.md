# harn-hostlib

Opt-in host builtins for the Harn VM that provide:

1. **Code intelligence** — tree-sitter–backed parsing, deterministic
   trigram/word indexing, and project-wide repo scanning. Ports the Swift
   `Sources/ASTEngine/`, `Sources/BurinCodeIndex/`, and
   `Sources/BurinCore/Scanner/` surface from
   [`burin-labs/burin-code`](https://github.com/burin-labs/burin-code).
2. **Deterministic tools** — content search (`grep-searcher` + `ignore`),
   file I/O, directory listing, file outline, git inspection (`gix`), file
   watching (`notify`), and process lifecycle (`run_command`, `run_test`,
   `run_build_command`, `inspect_test_results`, `manage_packages`). Ports
   the Swift `CoreToolExecutor` surface so calls no longer have to bounce
   Harn → Swift → Harn.

## Status

The crate was first scaffolded by
[#563](https://github.com/burin-labs/harn/issues/563). Implementations land
incrementally — this column tracks today's state.

| Issue | Module | What lands | State |
|-------|--------|-----------|-------|
| B2    | `ast/`         | `parse_file`, `symbols`, `outline` | scaffold |
| B3    | `code_index/`  | `query`, `rebuild`, `stats`, `imports_for`, `importers_of` | scaffold |
| B4    | `scanner/`     | `scan_project`, `scan_incremental` | scaffold |
| C1    | `fs_watch/`    | `subscribe`, `unsubscribe` | scaffold |
| C2    | `tools/` (read & search) | `search`, `read_file`, `list_directory`, `get_file_outline`, `git`, `write_file`, `delete_file` | scaffold |
| [#568](https://github.com/burin-labs/harn/issues/568) | `tools/` (process) | `run_command`, `run_test`, `run_build_command`, `inspect_test_results`, `manage_packages` | **implemented** |

### Process tools (issue #568)

The five process-lifecycle tools spawn real subprocesses and so route
through `harn_vm::process_sandbox`. That ensures every spawn is wrapped in
the active orchestration capability policy: Linux seccomp/landlock filters
via `pre_exec`, macOS `sandbox-exec` policy wrapping, and `cwd` enforcement
against the workspace roots the embedder configured.

Per-tool contracts:

- `tools/run_command` — accepts `argv: [String]` (no shell parsing), captures
  stdout/stderr in full, enforces `timeout_ms` by killing the child and
  reporting `timed_out: true`. Forwards `cwd`, `env` (full replacement, not
  patch), and `stdin`.
- `tools/run_test` — when `argv` is supplied, runs verbatim. Otherwise
  detects the workspace ecosystem from manifests in `cwd` (Cargo.toml,
  package.json + lockfile, pyproject.toml, go.mod, Package.swift, …) and
  picks a sensible default. Where the runner supports it (`pytest`,
  `vitest`), we ask for JUnit XML and stash the path with the result handle
  so `inspect_test_results` can drill in.
- `tools/run_build_command` — same detection ladder, plus
  `--message-format=json-diagnostic-rendered-ansi` for cargo so we can emit
  per-error `Diagnostic` records out of the JSON stream. Falls back to a
  generic regex sweep over stdout/stderr for runners we don't know.
- `tools/inspect_test_results` — keyed by the opaque `result_handle` from
  the matching `run_test`. Parses JUnit XML, cargo libtest plain text, or
  go test text into per-test records with status / message / stdout /
  stderr / path / line.
- `tools/manage_packages` — install / add / remove / update / refresh
  across cargo, npm, pnpm, yarn, pip, uv, poetry, go, swift, gradle,
  maven, bundler, composer, dotnet. Approval UX is the embedder's job —
  by the time the builtin runs, the host has already obtained consent.

## Why a separate crate?

`harn-vm` powers Harn pipelines that have nothing to do with editing host
code. Pulling tree-sitter grammars, ripgrep, and `notify` into the VM
crate would balloon its compile time and binary size for every embedder
that doesn't index host source. `harn-hostlib` is **opt-in**: nothing
inside `harn-vm` knows the crate exists. Embedders that want the surface
ask for it.

Conversely, the work that *does* belong in `harn-vm` — orchestration,
transcript lifecycle, replay/eval, mutation session audit metadata —
stays there. See
[`AGENTS.md`](../../CLAUDE.md#trust-boundary) for the canonical trust
boundary.

## How embedders consume it

The `harn-cli` ACP server wires hostlib in by default:

```rust
let mut vm = harn_vm::Vm::new();
let _registry = harn_hostlib::install_default(&mut vm);
```

`install_default` registers every shipped capability and returns a
`HostlibRegistry` that can be introspected (e.g. for
`burin-code`'s schema-drift tests) without mutating the VM further.

Pick-and-choose embedders that only want a subset of modules can build a
custom registry:

```rust
let mut registry = harn_hostlib::HostlibRegistry::new()
    .with(harn_hostlib::tools::ToolsCapability::default())
    .with(harn_hostlib::ast::AstCapability::default());
registry.register_into_vm(&mut vm);
```

The cargo feature `hostlib` on `harn-cli` is **default-on**. Embedders
can disable it with `--no-default-features` for a slimmer build that
omits the tree-sitter/notify/gix dependency tree entirely.

## How `burin-code` consumes it

`burin-code` pulls hostlib in transitively via the harn release pinned in
its `.harn-version` manifest. After this scaffold lands, the parent epic
ships:

1. A harn release bumping the version in this repo (per
   [`scripts/release_ship.sh`](../../scripts/release_ship.sh)).
2. A burin-code PR bumping `.harn-version` to that release.
3. burin-code progressively retires its Swift-side `BurinCore`
   counterparts as each implementation issue lands here.

The schemas under `schemas/<module>/<method>.{request,response}.json` are
the **source of truth** for burin-code's schema-drift tests. They ship
with the published crate (see the `include` field in `Cargo.toml`) and
are also mirrored at compile time via `include_str!` into
[`schemas.rs`](src/schemas.rs) so embedders can fetch them
programmatically without locating the on-disk schema directory.

## Directory layout

```text
crates/harn-hostlib/
├── Cargo.toml
├── README.md                  # this file
├── schemas/                   # JSON Schema 2020-12 contracts
│   ├── ast/
│   ├── code_index/
│   ├── scanner/
│   ├── fs_watch/
│   └── tools/
├── src/
│   ├── lib.rs                 # public surface + install_default
│   ├── error.rs               # HostlibError → VmError translation
│   ├── registry.rs            # HostlibCapability + HostlibRegistry
│   ├── schemas.rs             # const SCHEMAS catalog (include_str!)
│   ├── ast/
│   ├── code_index/
│   ├── scanner/
│   ├── fs_watch/
│   └── tools/
└── tests/
    └── registration.rs        # registration + schema parity tests
```

## Adding a new method

1. Add a `register_unimplemented(...)` entry in the relevant module's
   `register_builtins`.
2. Drop `<method>.request.json` and `<method>.response.json` into
   `schemas/<module>/`.
3. Append two `include_str!` entries to `SCHEMAS` in `src/schemas.rs`.
4. Add the method name to the `assert_eq!` list in `tests/registration.rs`.

The integration tests catch any drift between the four locations.
