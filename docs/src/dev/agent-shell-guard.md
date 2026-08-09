# Agent shell guard

Harn gives Codex and Claude the same repository command rules. The guard keeps
Rust builds and tests on the Make targets that configure a private build
directory for each worktree. It also stops long commands from losing most of
their output in a filter.

The guard applies only to agent shell calls. It does not change commands run by
people, continuous integration, Make recipes, or repository scripts.

## Use the supported commands

Run the Make target when one owns the operation:

| Instead of | Run |
|---|---|
| `cargo build` | `make build` |
| `cargo check` | `make check` |
| `cargo test` or `cargo nextest` | `make test` |
| `cargo clippy` | `make lint` |
| `cargo fmt` | `make fmt` |
| `cargo bench` | `make bench` |

Inspection and package-management commands such as `cargo tree` and
`cargo add` remain available because no Make target owns them.

For a real one-off, add `HARN_ALLOW_RAW_CARGO=1` to that command. The escape is
visible in the shell call and does not weaken later calls.

## Keep complete command output

Do not send a build or test directly into `grep`, `head`, or another filter.
Save the complete output first:

```bash
make test > .harn-runs/test.log 2>&1; result=$?
grep FAILED .harn-runs/test.log
exit "$result"
```

`tee` is also accepted when it names a log file:

```bash
make test 2>&1 | tee .harn-runs/test.log
```

This preserves the evidence needed to investigate a failure without repeating
the expensive command.

## Keep generated output out of Trash

Trash is reserved for user-authored or uncertain data that may need recovery.
Do not move temporary files, benchmark output, compiler caches, or other
reproducible build artifacts there: that turns cleanup into visible Trash
clutter. The guard rejects `trash` and `trash-put` when an argument resolves to
an OS temporary root or a repository-generated root such as `.build/`,
`target/`, `.harn-runs/`, or the Burin eval roots. It still allows Trash for
ordinary user files.

Prefer leaving a rebuildable cache in place. Put one-off output beneath an OS
temporary directory, and permanently remove it only through an exact,
validated path and a mechanism accepted by the host. If the host refuses that
cleanup, leave the data in its temporary/build root and report it instead of
using Trash as a fallback.

## Enable the hooks

The repository already carries both host adapters:

- Codex reads `.codex/hooks.json` after the project is trusted. Open `/hooks`
  to review the active hook. Codex asks for review again when the hook file
  changes.
- Claude Code reads `.claude/settings.json`. Open `/hooks` to inspect or disable
  the active hook.

Both adapters call `scripts/agent-shell-guard.sh`. That small adapter locates an
existing Harn executable and passes the host payload to
`scripts/agent_shell_guard.harn`, which owns every decision. The adapter never
starts a build. Its empty builtin allow list denies registered non-core
builtins and the typed Harness methods backed by them.
`HARN_LLM_CALLS_DISABLED=1` independently prevents a real model request.

The adapter loads project trigger and hook handler code only if a handler runs.
A top-level handler initialization failure therefore cannot disable command
checks. Harn still parses `harn.toml` and validates handler declarations. If
that validation fails, or no Harn executable is available, the adapter allows
the command so a broken project or fresh checkout cannot lock the agent out of
setup.

See the current [Codex hooks reference](https://developers.openai.com/codex/config-advanced#hooks)
and [Claude Code hooks reference](https://code.claude.com/docs/en/hooks) for the
host-level trust and configuration rules.

## Diagnose a decision

Send a host-shaped payload to the same adapter used by the agents:

```bash
printf '%s' '{"tool_name":"Bash","tool_input":{"command":"cargo test"}}' \
  | scripts/agent-shell-guard.sh
```

A blocked command prints one JSON decision with the reason and replacement. An
allowed command prints nothing. To check the policy and its in-process tests:

```bash
harn check scripts/agent_shell_guard.harn
harn test scripts/tests/agent_shell_guard_test.harn
```

If the adapter stays silent unexpectedly, confirm that `harn` is installed or
set `HARN_BIN` to an existing executable. Then repeat the probe with diagnostics
enabled:

```bash
printf '%s' '{"tool_name":"Bash","tool_input":{"command":"cargo test"}}' \
  | AGENT_SHELL_GUARD_DEBUG=1 scripts/agent-shell-guard.sh
```

The adapter deliberately ignores an invalid executable path and remains
fail-open. Debug mode preserves Harn startup and policy errors on stderr.

## Reuse the policy in another repository

First give that repository Make targets with the same public names. Each target
must own its build directory and environment before the matching Cargo command
is blocked. Then project these files from Harn without editing their copies:

- `scripts/agent_shell_guard.harn`
- `scripts/agent-shell-guard.sh`

Add host settings that call the projected adapter. Keep repository-specific
startup hooks in the host settings rather than putting them in the shared
policy. The deferred handler startup matters most in these downstream projects,
where the command policy sits beside application triggers and hooks. The Harn
fleet manifest checks projected files byte for byte, so a policy change has one
owner and downstream drift becomes a failed check.
