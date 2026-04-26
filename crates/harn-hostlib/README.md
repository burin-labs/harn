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

[#563](https://github.com/burin-labs/harn/issues/563) introduced the
scaffold (every method routed through `HostlibError::Unimplemented`).
[#564](https://github.com/burin-labs/harn/issues/564) lights up the
`ast/` surface — tree-sitter parsing, symbol extraction, and outline
generation for 22 host languages, mirroring the Swift `ASTEngine`
coverage verbatim.
[#567](https://github.com/burin-labs/harn/issues/567) lights up the
deterministic-tool surface: `search`, `read_file`, `write_file`,
`delete_file`, `list_directory`, `get_file_outline`, and `git`.
[#568](https://github.com/burin-labs/harn/issues/568) lights up the
process-lifecycle surface: `run_command`, `run_test`,
`run_build_command`, `inspect_test_results`, and `manage_packages`.
[#565](https://github.com/burin-labs/harn/issues/565) lights up the
`code_index` surface: trigram + word index, dep graph, and the five
host builtins (`query`, `rebuild`, `stats`, `imports_for`,
`importers_of`).
[#569](https://github.com/burin-labs/harn/issues/569) lights up the
`fs_watch` surface: cross-platform `notify` subscriptions with
debounced AgentEvent batches.

### `ast/` languages

Tree-sitter grammars are pinned in [`Cargo.toml`](Cargo.toml). Adding or
dropping a language requires a coordinated change here, in the Swift
`TreeSitterLanguage` enum, and in burin-code's bridge consumer.

| Language       | Grammar crate                 | Extensions      |
|----------------|-------------------------------|-----------------|
| TypeScript     | `tree-sitter-typescript`      | `.ts`           |
| TSX            | `tree-sitter-typescript`      | `.tsx`          |
| JavaScript     | `tree-sitter-javascript`      | `.js .mjs .cjs` |
| JSX            | `tree-sitter-javascript`      | `.jsx`          |
| Python         | `tree-sitter-python`          | `.py`           |
| Go             | `tree-sitter-go`              | `.go`           |
| Rust           | `tree-sitter-rust`            | `.rs`           |
| Java           | `tree-sitter-java`            | `.java`         |
| C              | `tree-sitter-c`               | `.c .h`         |
| C++            | `tree-sitter-cpp`             | `.cpp .cc .hpp` |
| C#             | `tree-sitter-c-sharp`         | `.cs`           |
| Ruby           | `tree-sitter-ruby`            | `.rb`           |
| Kotlin         | `tree-sitter-kotlin-ng`       | `.kt .kts`      |
| PHP            | `tree-sitter-php`             | `.php`          |
| Scala          | `tree-sitter-scala`           | `.scala .sc`    |
| Bash / shell   | `tree-sitter-bash`            | `.sh .bash .zsh`|
| Swift          | `tree-sitter-swift`           | `.swift`        |
| Zig            | `tree-sitter-zig`             | `.zig`          |
| Elixir         | `tree-sitter-elixir`          | `.ex .exs`      |
| Lua            | `tree-sitter-lua`             | `.lua`          |
| Haskell        | `tree-sitter-haskell`         | `.hs .lhs`      |
| R              | `tree-sitter-r`               | `.r`            |

The `ast::*` builtins emit row/column coordinates as **0-based** values
(matching tree-sitter native `Point`s). Symbol kinds are normalized to
the lowercase string set Swift's `ASTEngine` already produced
(`function`, `method`, `class`, `struct`, `enum`, `interface`,
`protocol`, `type`, `variable`, `module`, `other`).

Per-language fixture goldens live at
`tests/fixtures/ast/<language>/{source.<ext>,symbols.golden.json,outline.golden.json}`.
To regenerate after a deliberate change, run

```text
HARN_AST_UPDATE_GOLDEN=1 cargo test -p harn-hostlib --test ast_fixtures
```

and commit the updated goldens.

| Issue | Module | What lands | Status |
|-------|--------|-----------|--------|
| B1 (#563) | scaffold       | crate + schemas + registration plumbing                                                   | ✅ shipped |
| B2 (#564) | `ast/`         | `parse_file`, `symbols`, `outline` (tree-sitter for 22 host languages)                    | ✅ shipped |
| B3 (#565) | `code_index/`  | `query`, `rebuild`, `stats`, `imports_for`, `importers_of`                                | ✅ shipped |
| B4 (#566) | `scanner/`     | `scan_project`, `scan_incremental`                                                        | ✅ shipped |
| #569  | `fs_watch/`        | `subscribe`, `unsubscribe`                                                                | ✅ shipped |
| #567  | `tools/` (read & search) | `search`, `read_file`, `list_directory`, `get_file_outline`, `git`                 | ✅ shipped |
| #567  | `tools/` (mutating)      | `write_file`, `delete_file`                                                        | ✅ shipped |
| #568  | `tools/` (process)       | `run_command`, `run_test`, `run_build_command`, `inspect_test_results`, `manage_packages` | ✅ shipped |

### Process tools

The five process-lifecycle tools spawn real subprocesses and route through
`harn_vm::process_sandbox`. That keeps every spawn under the active
orchestration capability policy: Linux seccomp/landlock filters via
`pre_exec`, macOS `sandbox-exec` wrapping, and cwd enforcement against the
workspace roots the embedder configured.

- `tools/run_command` accepts `argv: [string]` (no shell parsing), captures
  stdout/stderr, enforces `timeout_ms` by killing the child and reporting
  `timed_out: true`, and forwards optional `cwd`, `env`, and `stdin`.
- `tools/run_test` runs explicit `argv` verbatim or detects a default test
  runner from manifests in `cwd`. Pytest and vitest get a JUnit XML output
  path so `inspect_test_results` can drill into per-test records.
- `tools/run_build_command` runs explicit `argv` or a detected build
  command. Cargo uses `--message-format=json-diagnostic-rendered-ansi`;
  other runners fall back to go/generic diagnostic parsing.
- `tools/inspect_test_results` reads the opaque `result_handle` from
  `run_test` and parses JUnit XML, cargo libtest text, or go test text.
- `tools/manage_packages` assembles install/add/remove/update/refresh
  commands for cargo, npm, pnpm, yarn, pip, uv, poetry, go, swift, gradle,
  maven, bundler, composer, and dotnet, with lockfile mtime change
  detection.

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

## Scanner host capability

`scanner/` ports `Sources/BurinCore/Scanner/CoreRepoScanner.swift` and emits
the `ScanResult` shape that burin-code's intake pipeline consumes today
(project metadata + file/folder/symbol records + dependency edges +
sub-project boundaries + token-budgeted text repo map). Two builtins:

- `hostlib_scanner_scan_project({ root, include_hidden?, respect_gitignore?,
  max_files?, include_git_history?, repo_map_token_budget? })` — full scan.
  Persists a snapshot to `<root>/.harn/hostlib/scanner-snapshot.json` so
  follow-up incremental scans can diff against it.
- `hostlib_scanner_scan_incremental({ snapshot_token, changed_paths?, … })`
  — refresh the snapshot. Falls back to a full rescan when the snapshot is
  missing or the diff exceeds ~30% of the workspace.

Unlike the `tools/` surface, the scanner is **not** gated by
`hostlib_enable("tools:deterministic")`: producing a `ScanResult` is a
read-only operation that doesn't mutate user state and the snapshot file
already lives under `.harn/`, which the hostlib treats as a managed
directory.

## Per-session opt-in for deterministic tools

The deterministic-tool surface (`tools/{search, read_file, write_file,
delete_file, list_directory, get_file_outline, git, run_command,
run_test, run_build_command, inspect_test_results, manage_packages}`) is
**gated**.
`install_default` registers the contract for every method, but the
handlers refuse to run until the pipeline opts in by calling

```text
hostlib_enable("tools:deterministic")
```

(a builtin registered alongside the rest of the `tools/` surface). This
matches the safety story called out in
[#567](https://github.com/burin-labs/harn/issues/567): a Harn script that
hasn't asked for filesystem / git / search access cannot get it even
though the contract is wired in. The same gate applies to process and
package-manager tools. The opt-in is per-thread, so each VM gets an
independent enable set.

Embedders that want to enable the surface from Rust without going through
the builtin can use [`tools::permissions::enable_for_test`] (test-only)
or call `tools::permissions::enable("tools:deterministic")` directly.

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
├── data/                      # data tables consumed via include_str!
│   └── code_index_import_rules.json
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
│   ├── code_index/            # trigram + word index, dep graph (#565)
│   ├── scanner/
│   ├── fs_watch/
│   └── tools/
└── tests/
    ├── registration.rs        # registration + schema parity tests
    ├── code_index.rs          # builtin-level integration tests
    └── code_index_scenario.rs # scenario test over a Swift-shaped fixture
```

## Adding a new method

1. Add a `register_unimplemented(...)` entry in the relevant module's
   `register_builtins`.
2. Drop `<method>.request.json` and `<method>.response.json` into
   `schemas/<module>/`.
3. Append two `include_str!` entries to `SCHEMAS` in `src/schemas.rs`.
4. Add the method name to the `assert_eq!` list in `tests/registration.rs`.

The integration tests catch any drift between the four locations.
