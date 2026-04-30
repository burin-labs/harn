# Windows test coverage

This page tracks the disposition of every workspace test module that opts
out of Windows via `#![cfg(unix)]` (or per-test `#[cfg(unix)]`). It exists
so that a new contributor can tell at a glance whether a gate is
load-bearing (POSIX semantics under test) or simply lazy (the test does
not exercise anything OS-specific and could run on Windows with minor
care).

The Windows nightly nextest matrix in
`.github/workflows/windows-nightly.yml` runs the workspace test surface on
`windows-latest`, so any test that is *not* gated below is expected to
pass on Windows.

## Inventory

| Test module | Disposition | Rationale |
|-------------|-------------|-----------|
| [`crates/harn-hostlib/tests/process_tools.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-hostlib/tests/process_tools.rs) | **Keep `#![cfg(unix)]`** — POSIX-bound | Every fixture spawns `bash -c "<script>"` to exercise argv / cwd / env / stdin / timeout plumbing. Bash and the shell-script vocabulary used here (`echo`, `pwd`, `1>&2`, `for i in $(seq 1 N)`, `printenv`, `$$`, etc.) are POSIX-only. Porting to Windows would require rewriting every fixture against `cmd.exe` / PowerShell, with subtle quoting and exit-code semantics that would no longer test the same plumbing on both sides. |
| [`crates/harn-cli/tests/orchestrator_cli.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-cli/tests/orchestrator_cli.rs) | **Keep `#![cfg(unix)]`** — POSIX-bound | Every test sends `SIGTERM` to the orchestrator child and asserts on the resulting drain (`graceful shutdown complete` log line, successful exit code, lifecycle event ordering). Windows has no portable signal-delivery mechanism for an arbitrary console child without taking over the parent's console group, so the orchestrator cannot be drained the same way under test. |
| [`crates/harn-cli/tests/orchestrator_http.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-cli/tests/orchestrator_http.rs) | **Keep `#![cfg(unix)]`** — POSIX-bound | Same as `orchestrator_cli.rs`: every test asserts on graceful shutdown after `SIGTERM`. Some tests additionally assert on in-flight HTTP request drain, which is also a POSIX-signal-driven flow today. |
| [`crates/harn-cli/tests/orchestrator_inbox_dedupe.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-cli/tests/orchestrator_inbox_dedupe.rs) | **Keep `#![cfg(unix)]`** — POSIX-bound | Same as above: `SIGTERM` + clean drain are part of the assertion. |
| [`crates/harn-cli/tests/acp_server_cli.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-cli/tests/acp_server_cli.rs) | **Ungate** — fully portable | Drives `harn serve acp` over piped stdio; child shutdown is via stdin EOF, not signals. Runs on Unix and Windows. |
| [`crates/harn-cli/tests/mcp_server_cli.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-cli/tests/mcp_server_cli.rs) | **Ungate** — fully portable | Drives `harn mcp serve` over piped stdio; child shutdown is via `std::process::Child::kill` (TerminateProcess on Windows, SIGKILL on Unix). |
| [`crates/harn-cli/tests/harn_serve_mcp_cli.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-cli/tests/harn_serve_mcp_cli.rs) | **Ungate** — fully portable | Drives `harn serve mcp`; child shutdown is via stdin EOF or `Child::kill`. No POSIX signals, no shellouts to `kill`. |

The rest of the workspace either has no module-level `#![cfg(unix)]`
(individual `#[cfg(unix)]` items remain — typically for skill paths,
symlink helpers, or tokio signal streams that already have a
`#[cfg(not(unix))]` Windows fallback nearby) or is a Unix-only
implementation file (`crates/harn-vm/src/stdlib/sandbox.rs`,
`crates/harn-cli/src/package/registry.rs::symlink_path_dependency`, etc.)
that genuinely cannot be implemented on Windows the same way.

## Adding new POSIX-bound tests

If a new test module needs POSIX semantics (signals, fork, real shell),
gate it `#![cfg(unix)]` and add a row to the table above so the gate
remains discoverable. The header comment in the test file should explain
*why* the gate exists; this page is for cross-cutting visibility.

## Adding new portable tests

Default to *not* gating. The Windows nightly matrix will catch hidden
Unix assumptions (`/`-only paths, `\n`-only line endings, missing `.exe`
suffix, etc.) and you can either fix them in place or add a per-test
`#[cfg(unix)]` annotation if a single case turns out to be POSIX-bound.

## Cross-platform child shutdown helpers

`crates/harn-cli/tests/test_util/timing.rs::ChildExitWatcher` exposes
`terminate()` and `kill()` that work on both platforms:

- **Unix**: shells out to `kill -TERM` / `kill -KILL`.
- **Windows**: shells out to `taskkill /F` (which calls `TerminateProcess`
  underneath). Both `terminate()` and `kill()` collapse to the same
  forceful path; there is no graceful-shutdown drain on Windows.

Tests that depend on the orchestrator running its drain logic must
therefore stay `#![cfg(unix)]` regardless of which helper they call. This
is documented in the doc-comment on `ChildExitWatcher::terminate`.
