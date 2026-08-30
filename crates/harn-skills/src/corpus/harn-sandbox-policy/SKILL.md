---
name: harn-sandbox-policy
short: What a confined child may read and write, the credential denylist, and how to prove a change to either.
description: Use for the process sandbox — read/write roots, the credential denylist, the toolchain-cache environment, and the per-backend profile emission on macOS seatbelt, Linux Landlock, and Windows AppContainer.
when_to_use: Use when adding a read root or a denylist row, when a toolchain command fails "Permission denied" or "No module named X" under an agent, or when changing anything under stdlib/sandbox.
---

# Harn sandbox policy

The sandbox decides what a child process spawned by an agent tool call may read
and write. It has one owner, `crates/harn-vm/src/stdlib/sandbox/`, and three
backends that project the same `CapabilityPolicy` onto their platform.

Pair it with [[harn-agent]] for autonomy gating and [[harn-orchestration]] for
where the policy comes from.

## The three axes, which are not the same axis

Confusing these is the most common mistake here.

1. **`workspace_roots` / `read_only_roots`** scope *Harn's own file builtins*
   (`read_file`, `harness.fs.*`), through `check_fs_path_scope`.
2. **`process_sandbox.{presets,read_roots,write_roots,read_deny_roots}`** scope
   *child processes only*, through the OS backend. These never widen Harn's file
   builtins.
3. **`sandbox_profile`** decides whether either is enforced at all.
   `enforces_path_scope()` gates axis 1 and the toolchain-cache environment;
   `confines_processes()` gates axis 2.

A grant on one axis is not a grant on another, and a bug report that says "the
agent cannot read X" needs to name which one before it can be diagnosed.

## Adding a read root

Add it to the policy the embedder builds, or to a preset in
`sandbox/mod.rs` if it belongs to every run. Presets are additive and expand
per-platform in `macos.rs` / `linux.rs` / `windows.rs`.

Do not add a root by inferring it from the command being run. A read surface
that is a function of the tool call is not auditable and fails
order-dependently: the same command succeeds or fails depending on what ran
before it.

## Adding a denylist row

`crates/harn-vm/src/orchestration/policy/read_deny_defaults.toml`. One line
under `defaults.home_relative`, plus a line under `[reason]` saying why. It is
data on purpose: nothing in the Rust module decides what belongs on the list.

The denylist **beats every grant**, which is the point — `PackageManagerConfig`
opens `~/.config`, `~/.cache`, and `~/.netrc` wholesale, so a denial that merely
competed with presets would never fire on the paths it exists for. It is
checked before any grant in `check_fs_path_scope`, and it unions rather than
intersects as policies nest, because narrowing a denial would widen authority.

Per backend:

- **macOS** emits `(deny file-read* (subpath …))` *after every allow*.
  `sandbox-exec` is last-match-wins, so position is the enforcement.
- **Linux** has no deny rule. Landlock is allow-only, so a denial is expressed
  by not granting: `expand_around_denied` substitutes the siblings that do not
  lead to the denial. An ancestor it cannot enumerate ends the walk, granting
  nothing beneath it; any other enumeration error still fails closed.

  The rule worth carrying to any allow-only backend: **"cannot enumerate" ends
  the walk, it never refuses the spawn.** The absence of a grant is already the
  denial, so stopping early is strictly narrower than continuing. Refusing
  instead took every run down twice here, once for a missing `~/.kube` and once
  for an unreadable `$HOME`, and gained no authority either time.
- **Windows / OpenBSD** do not apply the denylist yet.

## When a toolchain command fails under an agent

Two failure shapes, and they are not both the sandbox refusing something.

- **"Permission denied" writing a cache** — the tool is writing outside every
  write root. Relocate its cache env var with the pure caches in
  `workspace_env.rs` (that is how `CCACHE_TEMPDIR`, which defaults to
  `/run/user/<uid>`, is handled). Do not widen a write root for it.
- **"No module named X" / "command not found" for something installed** —
  nothing was refused. Check whether an env var moved the place the tool looks.
  `HOME` and `PYTHONUSERBASE` are deliberately *not* relocated for exactly this
  reason; a relocated user site made an installed `pytest` invisible while it
  worked fine in the operator's terminal.

The distinction matters because the second shape leaves no denial to find, and
an agent retrying it gets no diagnostic at all.

## How to prove a change

A rendered profile proves what was written, never what the kernel did with it.
Assert on real behavior:

- `make test ARGS='-p harn-vm --lib sandbox'` on macOS and on Linux. They
  test different mechanisms, so a green run on one says little about the other.
- The live falsifiers are the load-bearing ones:
  `a_live_confined_child_is_refused_a_denied_file_and_allowed_its_sibling`
  (macOS) and `a_live_landlock_child_is_refused_a_denied_file_and_allowed_its_sibling`
  (Linux). Each spawns a real confined child and reads two files inside the same
  granted root: the sibling must be readable, the denied file refused, and
  clearing `read_deny_roots` must make it readable again. Copy that shape.
- On Linux specifically, remember the enforcement is the *absence* of a grant,
  and absence is what a careless test reads as success. A test that only checks
  which paths were selected has not tested a refusal.
- `make test ARGS='-p harn-vm --lib report_default_denylist_expansion_cost'`
  with `--nocapture` prints the live
  Landlock rule count for this host. Run it after changing the denylist; the cap
  is set from measured numbers, not intuition.
