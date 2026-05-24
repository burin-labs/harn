# CLI parity-snapshot fixtures

Golden-file fixtures consumed by the parity-snapshot harness
(`crates/harn-cli/tests/parity_dispatch.rs`) — see harn#2299 (G6) and
harn#2293 (epic) for the why.

Each fixture is a directory under `<command>/<scenario>/` with:

| File | Purpose | Required |
| --- | --- | --- |
| `argv.txt` | One arg per line. Empty file = no args. | yes |
| `stdin.txt` | Stdin to pipe to the dispatched script. | no |
| `env.txt` | One `KEY=VALUE` per line. Layered on top of the host env. | no |
| `stdout.txt` | Expected stdout, byte-for-byte. | yes |
| `stderr.txt` | Expected stderr, byte-for-byte. Empty file = no stderr. | yes |
| `exit_code.txt` | Single integer on its own line. | yes |

## Adding a fixture for a W ticket port

When a port wave (W1-W13) lands a `.harn` implementation of a
subcommand, it should:

1. Add fixture directories under
   `crates/harn-cli/tests/parity_fixtures/<cmd>/<scenario>/`. The W
   tickets list a minimum count (5 for simple ports, more for
   higher-stakes commands).
2. Run each fixture twice — once with `HARN_CLI_IMPL=rust` (the legacy
   Rust handler) and once with `HARN_CLI_IMPL=harn` (the new
   `.harn`-backed dispatch). Both should produce byte-identical stdout
   / stderr / exit code.
3. Register the fixture under `crates/harn-cli/tests/parity_dispatch.rs`
   so `cargo test -p harn-cli --test parity_dispatch` picks it up.

Once a port is the default (every release sets `HARN_CLI_IMPL=harn`
by default), the legacy Rust handler can be deleted — the parity
harness, plus the existing snapshot, guards against regression.

## Recording new snapshots

The first time you add a fixture, leave `stdout.txt` / `stderr.txt`
empty and run with `HARN_CLI_PARITY_RECORD=1`. The harness will
write the captured outputs back into the fixture directory. Commit
them.
