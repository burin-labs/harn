# CLI dispatch snapshot fixtures

Golden-file fixtures consumed by the dispatch snapshot harness
(`crates/harn-cli/tests/dispatch_snapshot.rs`).

Each fixture is a directory under `<command>/<scenario>/` with:

| File | Purpose | Required |
| --- | --- | --- |
| `argv.txt` | One arg per line. Empty file = no args. | yes |
| `stdin.txt` | Stdin to pipe to the dispatched script. | no |
| `env.txt` | One `KEY=VALUE` per line. Layered on top of the host env. | no |
| `stdout.txt` | Expected stdout, byte-for-byte. | yes |
| `stderr.txt` | Expected stderr, byte-for-byte. Empty file = no stderr. | yes |
| `exit_code.txt` | Single integer on its own line. | yes |

## Adding a fixture

When adding fixture coverage for an embedded CLI script:

1. Add fixture directories under
   `crates/harn-cli/tests/dispatch_fixtures/<cmd>/<scenario>/`.
2. Record the expected stdout, stderr, and exit code.
3. Register the fixture under `crates/harn-cli/tests/dispatch_snapshot.rs`
   so `cargo test -p harn-cli --test dispatch_snapshot` picks it up.

## Recording new snapshots

The first time you add a fixture, leave `stdout.txt` / `stderr.txt`
empty and run with `HARN_CLI_DISPATCH_RECORD=1`. The harness writes the
captured outputs back into the fixture directory. Commit them.
