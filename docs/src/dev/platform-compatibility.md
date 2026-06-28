# Platform compatibility

This page is the per-capability compatibility table for the Harn
runtime. It is the authoritative source on what Harn supports, what it
restricts, and what it deliberately defers per host operating system.
Integrators packaging Harn binaries should read this top to bottom
before promising a feature.

The table is exercised on every release-relevant PR by the
[release smoke matrix](https://github.com/burin-labs/harn/blob/main/.github/workflows/release-smoke.yml)
(`scripts/release_smoke.sh`). A regression on any row surfaces as a
`::error::release-smoke (<platform>): <capability> failed` annotation
that points at the specific (platform, capability) pair, not just
"smoke matrix failed".

## Capability matrix

| Capability | macOS | Linux | Windows | Notes |
|------------|-------|-------|---------|-------|
| `harn --help`, `harn --version` | Yes | Yes | Yes | Argument parsing, banner emission. Smoke step 1–2. |
| `harn check` (type-check) | Yes | Yes | Yes | Lexer/parser/type checker is platform-independent. Smoke step 3. |
| `harn fmt --check` | Yes | Yes | Yes | LF-only fixtures keep the formatter byte-stable across platforms. Smoke step 4. |
| `harn package check` | Yes | Yes | Yes | Manifest parsing, exports resolution, path normalization. Smoke step 5. |
| Generated artifacts (`harn provider catalog matrix`) | Yes | Yes | Yes | Deterministic-text emitter; line endings, sort order, and rounding are normalized in the writer. Smoke step 6. |
| `harn run` | Yes | Yes | Yes | VM bootstrapping + stdlib startup. Smoke step 7. |
| Process spawning (`std/command::command_run`) | Yes (sandbox-exec / Seatbelt) | Yes (Landlock + seccomp; falls back to warn under `HARN_HANDLER_SANDBOX=warn`) | Yes (AppContainer + Job Objects; warn fallback applies the same way) | Sandbox backend differences are encapsulated in `crates/harn-vm/src/stdlib/sandbox/` (one file per OS) behind a shared `SandboxBackend` trait. Scripts pick argv via `platform()`. Smoke step 8. |
| No-credentials workflow (`provider: "mock"`) | Yes | Yes | Yes | Drives `llm_call` end-to-end through the in-memory mock provider with no API keys, network, or platform secret store. Smoke step 9. |
| File watching (`harn watch`) | Yes (FSEvents) | Yes (inotify) | Yes (ReadDirectoryChangesW) | All three backends provided by the `notify` crate. Smoke step 10 boots the watcher and verifies the readiness banner; graceful shutdown via SIGTERM is Unix-only (see below). |
| Graceful orchestrator shutdown (`SIGTERM` drain) | Yes | Yes | **Deferred** | Tests that depend on the orchestrator drain are gated `#![cfg(unix)]`. See [Windows test coverage](./windows-test-coverage.md) for the inventory. On Windows the smoke kills `harn watch` with `taskkill /F`. |
| `unveil(2)` / `pledge(2)` host confinement | n/a | n/a | n/a | Implemented for OpenBSD only; not surfaced in the release smoke matrix because OpenBSD is not a published target. |

## Behavior differences by category

### Path handling

- All Harn fixtures and configuration files are stored with **LF line
  endings**. This is enforced by `.gitattributes` for the
  `tests/smoke/` tree, `docs/src/provider-matrix.md`,
  `docs/src/connectors/parity-matrix.md`, and other generated artifacts
  that are byte-compared in CI.
- Path separators in user-facing scripts use `/`. The runtime canonicalizes
  paths through `std::path::Path` before sandbox-policy enforcement,
  so a script written on macOS with `/`-separated paths runs unchanged
  on Windows.
- Windows-only path normalization quirks (UNC, extended-length, drive
  letters) are absorbed by `crates/harn-vm/src/stdlib/sandbox/windows.rs`
  before policy enforcement.

### Line endings

- The release smoke matrix builds and runs with `core.autocrlf=false`
  by virtue of `.gitattributes` `eol=lf` directives.
- `harn fmt` preserves whatever line endings the input file has;
  release smoke fixtures pin LF so the formatter never churns on a
  Windows checkout.
- The provider matrix emitter (`harn provider catalog matrix`) writes
  LF on every platform. CI verifies byte-identical output across the
  matrix as smoke step 6.

### Process spawning and sandboxing

- Per-platform sandbox backends live in `crates/harn-vm/src/stdlib/sandbox/`
  (one file per OS) behind a shared `SandboxBackend` trait. The default
  profile is `worktree` (workspace-root path enforcement plus best-effort
  OS confinement). Pipelines opt into `sandbox_profile: "os_hardened"`
  via the active `CapabilityPolicy` to make OS confinement required —
  see [Process sandboxing](../sandboxing.md). The fallback mode is
  `warn`, configurable via `HARN_HANDLER_SANDBOX` for the `worktree`
  profile only; `os_hardened` always enforces.
- Scripts must pick platform-appropriate argv when shelling out. The
  canonical pattern (used by smoke step 8) is:

  ```harn
  fn echo_argv(text) {
    if platform() == "windows" {
      return ["cmd", "/C", "echo " + text]
    }
    return ["printf", "%s", text]
  }
  ```

- The release smoke does **not** assert that the sandbox enforced a
  specific syscall mask. That belongs in unit tests on the matching
  platform; the smoke matrix only confirms each backend resolves and
  spawns successfully.

### Signals

- `SIGTERM`/`SIGINT`-driven shutdown drains apply to Unix only. The
  orchestrator and `harn watch` rely on `tokio::signal::unix` for the
  drain path; on Windows the equivalent is forceful termination via
  `TerminateProcess` (which the smoke driver invokes through
  `taskkill /F`).
- Any new orchestrator or daemon test that depends on a clean drain
  must add a row to [Windows test coverage](./windows-test-coverage.md).

### File watching

- All three notify backends (FSEvents on macOS, inotify on Linux,
  ReadDirectoryChangesW on Windows) are exercised via `harn watch`
  in smoke step 10. The smoke does not assert that a re-run fires; it
  asserts that the watcher reaches its readiness banner. Re-run logic
  is covered by per-backend integration tests in `crates/harn-cli`.

## Adding a new capability

When you add a new user-visible capability (CLI subcommand, stdlib
builtin, or sandbox-touching surface):

1. Add a row to the matrix above, even if every cell is "Yes". The row
   is the smoke driver's contract.
2. If the capability needs to be exercised at release time, add a step
   to `scripts/release_smoke.sh` keyed off `platform()` (in Harn) or
   the `PLATFORM` shell variable (in the driver).
3. If the Windows path is genuinely deferred (e.g. POSIX-signal
   drain), document why in this page **and** add a row to
   [Windows test coverage](./windows-test-coverage.md). The two pages
   cross-reference; do not let them drift.
4. Add the new fixture under `tests/smoke/` and verify locally via
   `make release-smoke`.
