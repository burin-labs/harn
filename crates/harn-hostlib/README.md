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

This is the **scaffold** that issue
[#563](https://github.com/burin-labs/harn/issues/563) introduced. Every host
method registered here today returns
`HostlibError::Unimplemented { builtin }`. Implementations land in the
follow-up issues called out in the parent epic
[`burin-labs/burin-code#289`](https://github.com/burin-labs/burin-code/issues/289):

| Issue | Module | What lands |
|-------|--------|-----------|
| B2    | `ast/`         | `parse_file`, `symbols`, `outline` |
| B3    | `code_index/`  | `query`, `rebuild`, `stats`, `imports_for`, `importers_of` |
| B4    | `scanner/`     | `scan_project`, `scan_incremental` |
| C1    | `fs_watch/`    | `subscribe`, `unsubscribe` |
| C2    | `tools/` (read & search) | `search`, `read_file`, `list_directory`, `get_file_outline`, `git` |
| C3    | `tools/` (mutating) | `write_file`, `delete_file`, `run_command`, `run_test`, `run_build_command`, `inspect_test_results`, `manage_packages` |

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
