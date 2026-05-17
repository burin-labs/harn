# harn-hostlib

Opt-in host builtins for the Harn VM that provide:

1. **Code intelligence** — tree-sitter–backed parsing, deterministic
   trigram/word indexing, project-wide repo scanning, file watching, and
   live workspace state.
2. **Deterministic tools** — content search (`grep-searcher` + `ignore`),
   file I/O, directory listing, file outline, git inspection (`gix`), file
   watching (`notify`), and process lifecycle (`run_command`, `run_test`,
   `run_build_command`, `inspect_test_results`, `manage_packages`).
3. **Session filesystem staging** — per-session deferred writes, deletes,
   read-through overlays, status reporting, commit, and discard.

## Status

[#563](https://github.com/burin-labs/harn/issues/563) introduced the
scaffold (every method routed through `HostlibError::Unimplemented`).
[#564](https://github.com/burin-labs/harn/issues/564) lights up the
`ast/` surface — tree-sitter parsing, symbol extraction, and outline
generation for 22 host languages.
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
[#1722](https://github.com/burin-labs/harn/issues/1722) lights up the
`fs` surface: deferred filesystem writes stored under
`.harn/state/staged/<session_id>/` with read-through overlays and ACP
commit controls.

### `ast/` languages

Tree-sitter grammars are pinned in [`Cargo.toml`](Cargo.toml). Adding or
dropping a language requires a coordinated change to the language table,
schemas, fixtures, and any host bridge that relies on the canonical
language names.

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
the lowercase hostlib wire set
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
| #1722 | `fs/`                    | `set_mode`, `staged_status`, `commit_staged`, `discard_staged`                    | ✅ shipped |
| #1720 | `fs/` (snapshots)        | `snapshot`, `restore`, `list_snapshots`, `drop_snapshot`                          | ✅ shipped |

### Process tools

The five process-lifecycle tools spawn real subprocesses and route through
`harn_vm::process_sandbox`. That keeps every spawn under the active
orchestration capability policy: Linux seccomp/landlock filters via
`pre_exec`, macOS `sandbox-exec` wrapping, and cwd enforcement against the
workspace roots the embedder configured.

- `tools/run_command` is the canonical command runner. It accepts
  `mode: "argv"` with `argv: [string]` as the recommended path, or
  `mode: "shell"` with `command`; shell mode uses the shared
  `process.get_default_shell` selection unless callers provide a `shell`
  object or `shell_id` from `process.list_shells`. It captures stdout/stderr,
  enforces `timeout_ms`, forwards optional `cwd`, `env`, `env_mode`, and
  `stdin`, and returns a command
  envelope with `command_id`, `status`, pid/process-group metadata, inline
  output capped by `capture.max_inline_bytes`, full output artifact paths,
  byte/line counts, `output_sha256`, sandbox metadata, and `audit_id`.
  `background: true` returns the same envelope with `status: "running"` and
  a `handle_id`; the old `long_running` field remains accepted as an alias.
- `tools/read_command_output` range-reads the artifact for a `command_id`,
  `handle_id`, or explicit `path`. Use it when `stdout`/`stderr` were capped
  inline or when an agent needs to inspect large command output.
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

`scanner/` emits the Harn `ScanResult` contract: project metadata,
file/folder/symbol records, dependency edges, sub-project boundaries, and
a token-budgeted text repo map. Two builtins:

- `hostlib_scanner_scan_project({ root, include_hidden?, respect_gitignore?,
  max_files?, include_git_history?, repo_map_token_budget? })` — full scan.
  Persists a snapshot to `<root>/.harn/hostlib/scanner-snapshot.json` so
  follow-up incremental scans can diff against it.
- `hostlib_scanner_scan_incremental({ snapshot_token, changed_paths?, … })`
  — refresh the snapshot. Falls back to a full rescan when the snapshot is
  missing or the diff exceeds ~30% of the workspace.

The Rust scanner API routes Git-backed file discovery and churn scoring through
`GitCapabilities`. The default hostlib builtin uses the Git CLI only when the
scan root is inside a worktree; embedders and tests can supply a mock capability
with `scan_project_with_git` when they need deterministic scanner behavior
without depending on ambient checkout state.

Unlike the `tools/` surface, the scanner is **not** gated by
`hostlib_enable("tools:deterministic")`: producing a `ScanResult` is a
read-only operation that doesn't mutate user state and the snapshot file
already lives under `.harn/`, which the hostlib treats as a managed
directory.

## Per-session opt-in for deterministic tools

The deterministic-tool surface (`tools/{search, read_file, write_file,
delete_file, list_directory, get_file_outline, git, run_command,
read_command_output, run_test, run_build_command, inspect_test_results,
manage_packages}`) is
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

## Staged filesystem mode

`fs/` adds a session-scoped deferred-write layer for hosts that want an
agent to build a cumulative diff before touching the working tree.

- `hostlib_fs_set_mode({ session_id, mode: "immediate" | "staged",
  root? })` switches the session and returns `{ previous_mode }`.
- `hostlib_fs_staged_status({ session_id })` returns pending writes,
  deletes, byte counts, and the age of the oldest pending change.
- `hostlib_fs_commit_staged({ session_id, paths? })` applies all pending
  changes, or only the selected paths, and reports per-path failures.
- `hostlib_fs_discard_staged({ session_id, paths? })` drops pending
  changes without mutating the working tree.

While a session is in `staged` mode, `tools/write_file` and
`tools/delete_file` write to `.harn/state/staged/<session_id>/` instead
of the target path. `tools/read_file`, `tools/list_directory`,
`get_file_outline`, the AST parse-file helper, and code-index file reads
consult the same overlay first, so the agent sees its pending changes
until they are committed or discarded. The ACP server also exposes
`session/fs_mode` and `session/fs_commit_staged`, and emits
`session/update` progress notifications with
`_meta.harn.kind = "staged_writes_pending"` whenever the pending count
or staged byte total changes.

## Per-tool-call FS snapshots

`fs/` also ships a Gemini-style rollback primitive paralleling the staged
overlay. Four builtins under the same `fs/` schema bucket:

- `hostlib_fs_snapshot({ session_id, scope_id, paths?, root? })` registers
  a snapshot keyed by `scope_id` (canonically the ACP `toolCallId`).
  Passing `paths` captures their pre-images immediately; omitting them
  leaves the snapshot "open" for lazy capture by the auto-on-write hook
  inside `tools/write_file` and `tools/delete_file`. Auto-capture binds
  to the active snapshot whose id matches
  [`harn_vm::agent_sessions::current_tool_call_id`].
- `hostlib_fs_restore({ session_id, snapshot_id, paths? })` writes
  captured bytes back onto disk and surgically removes paths the
  snapshot saw as absent. The ACP server exposes the same operation as
  `session/restore_tool_call` and broadcasts the result as a
  `session/update` tagged `_meta.harn.kind = "tool_call_restored"`.
- `hostlib_fs_list_snapshots({ session_id })` returns one entry per
  registered snapshot — `snapshot_id`, `scope_id`, `taken_at_ms`,
  `captured_paths`, `byte_count` — sorted by capture time.
- `hostlib_fs_drop_snapshot({ session_id, snapshot_id })` removes a
  snapshot from both the in-memory store and
  `.harn/state/snapshots/<session>/<scope>/`.

Each snapshot is content-addressed under
`.harn/state/snapshots/<session>/<scope>/bodies/<sha256>` with a
`manifest.json` mapping logical paths to entries. When a session bundle
exceeds the configurable byte cap (default 1 GiB; tune with
[`fs_snapshot::set_session_byte_cap`]), the oldest snapshots are
evicted in insertion order. Snapshots are ephemeral and live only as
long as the in-memory store; consumers that need durable rollback bundle
them into a session via `session/load`.

To advertise `restoreToolCall` over ACP the agent emits
`{ sessionCapabilities: { restoreToolCall: {} } }` in the initialize
response. Clients can then call:

```jsonc
{
  "method": "session/restore_tool_call",
  "params": { "sessionId": "sess_abc", "toolCallId": "tc_42" }
}
```

The Rust dispatch routes through `harn_hostlib::fs_snapshot::restore`
and emits a `tool_call_update` with `status: "restored"` plus
`restoredPaths` on the canonical SessionUpdate channel.

## How embedders consume it

The `harn-cli` ACP server wires hostlib in by default:

```rust
let mut vm = harn_vm::Vm::new();
let _registry = harn_hostlib::install_default(&mut vm);
```

`install_default` registers every shipped capability and returns a
`HostlibRegistry` that can be introspected by schema-compatibility tests
without mutating the VM further.

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

## Schema compatibility

Harn-owned hosts usually consume hostlib through a pinned `harn` release
or through a direct Cargo dependency on this crate. Host integrations
should treat the JSON schemas and registered builtin list as the public
contract, then run their own compatibility tests against those exported
contracts during upgrades.

The schemas under `schemas/<module>/<method>.{request,response}.json` are
the **source of truth** for hostlib request/response compatibility. They
ship with the published crate (see the `include` field in `Cargo.toml`)
and are also mirrored at compile time via `include_str!` into
[`schemas.rs`](src/schemas.rs) so embedders can fetch them
programmatically without locating the on-disk schema directory.

Historical notes about the original bridge migration live in
[`docs/src/migrations/harn-hostlib-host-contracts.md`](../../docs/src/migrations/harn-hostlib-host-contracts.md).

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
│   ├── fs/
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
│   ├── fs.rs                  # deferred per-session filesystem overlay
│   ├── fs_watch/
│   └── tools/
└── tests/
    ├── registration.rs        # registration + schema parity tests
    ├── code_index.rs          # builtin-level integration tests
    └── code_index_scenario.rs # scenario test over a host-shaped fixture
```

## Adding a new method

1. Add a `register_unimplemented(...)` entry in the relevant module's
   `register_builtins`.
2. Drop `<method>.request.json` and `<method>.response.json` into
   `schemas/<module>/`.
3. Append two `include_str!` entries to `SCHEMAS` in `src/schemas.rs`.
4. Add the method name to the `assert_eq!` list in `tests/registration.rs`.

The integration tests catch any drift between the four locations.
