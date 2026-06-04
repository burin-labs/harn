# Process sandboxing

Harn ships a default `harn run` worktree sandbox plus an OS-level
process sandbox that engages whenever a subprocess is spawned under an
active capability policy. The OS sandbox runs in addition to the
workspace-root path enforcement and the approval-policy DSL — defense
in depth, not a replacement.

Direct `harn run` invocations install a `worktree` capability policy
before the VM starts. The policy roots filesystem and process-cwd
access at the nearest `harn.toml` project root, or at the invocation
working directory when no manifest is present, and sets the
side-effect ceiling below `network`. Pass `--no-sandbox` to opt out
for a single run; the CLI prints a warning when that escape hatch is
used.

## Sandbox profiles

The active [`CapabilityPolicy`](./host-boundary.md) carries a
`sandbox_profile` field that selects how strictly the runtime confines
spawned processes. Pipelines and the `process.exec` host call set it
explicitly when they want stronger or weaker isolation than the
default.

| Profile | Path enforcement | OS confinement | When the spawn fails |
|---|---|---|---|
| `unrestricted` | skipped | skipped | only on direct OS errors |
| `worktree` *(default)* | required (`workspace_roots`) | best-effort | OS sandbox unavailability is logged once and ignored unless `HARN_HANDLER_SANDBOX=enforce` |
| `os_hardened` | required | **required** | spawn returns `tool_rejected` if the platform mechanism is missing or rejects the call, regardless of `HARN_HANDLER_SANDBOX` |
| `wasi` | enforced by the WASI runtime | enforced by the WASI runtime | testbench-only; the host spawn path is never reached |

The strictness ladder is `unrestricted < worktree < wasi <
os_hardened`. `CapabilityPolicy::intersect` always picks the strictest
of the two profiles when a parent ceiling is composed with a child
request — so a lenient parent cannot weaken a child's `os_hardened`
ask.

`HARN_HANDLER_SANDBOX={off,warn,enforce}` controls fallback behavior
for the `worktree` profile. `os_hardened` ignores the env var on
purpose: a profile that means "the OS sandbox is required" cannot be
silently downgraded by an environment variable.

### Process sandbox policy

`CapabilityPolicy` also carries a `process_sandbox` section. This policy is
process-only: it widens the generated OS child-process profile without widening
Harn file builtins. A compiler may need to load `/Applications/Xcode.app` or
update a per-user `xcrun` cache, but the agent still cannot call `read_text` on
those paths unless they are also in `workspace_roots` or `read_only_roots`.

```json
{
  "process_sandbox": {
    "presets": [
      "system_runtime",
      "developer_toolchains",
      "package_manager_config",
      "user_temp"
    ],
    "read_roots": ["/opt/vendor-sdk"],
    "write_roots": ["/opt/vendor-cache"]
  }
}
```

`presets: null` or an omitted `presets` field selects the runtime defaults:

- `system_runtime`: host runtime directories needed to launch common binaries.
- `developer_toolchains`: standard compiler/toolchain locations such as Xcode,
  Command Line Tools, Homebrew, and language runtime roots.
- `package_manager_config`: read-only per-user npm, pip, cargo, git, and CA
  config/cache roots under `$HOME`, such as `.npmrc`, `.gitconfig`, `.netrc`,
  `.config`, `.cache`, and cargo config/registry paths.
- `user_temp`: scratch/cache roots used by developer tools. These roots are
  writable only when the active capability policy already allows workspace
  writes.

An explicit empty `presets: []` disables every named preset. `read_roots` and
`write_roots` are for subprocesses only; `write_roots` are also gated by the
workspace-write capability. `CapabilityPolicy::intersect` narrows presets and
roots to their common set, so managed or parent ceilings can prevent a child
policy from adding host filesystem reach.

### Running real toolchains in the sandbox

The local process sandbox is meant to run normal developer tools, including
toolchains that depend on enterprise package-manager state. It constrains what
the child process can open; it does not rewrite npm, pip, cargo, git, proxy, or
CA configuration.

With the default `package_manager_config` preset, the Linux and macOS backends
grant child processes read-only access to these paths under the first absolute
`$HOME` or `$USERPROFILE`: `.npmrc`, `.gitconfig`, `.netrc`, `.yarnrc.yml`,
`.config`, `.npm`, `.cache`, `.pip`, `.pypirc`, `.cargo/config`,
`.cargo/config.toml`, `.cargo/credentials`, `.cargo/credentials.toml`,
`.cargo/registry`, and `.cargo/git`. These grants are process-only: Harn file
builtins still need `workspace_roots` or `read_only_roots`, and the package
manager paths are explicitly kept unwritable by the OS profile.

Child processes spawned without env overrides inherit the parent environment.
That includes corporate proxy and CA variables such as `HTTP_PROXY`,
`HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, `NODE_EXTRA_CA_CERTS`,
`SSL_CERT_FILE`, `SSL_CERT_DIR`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`,
`GIT_SSL_CAINFO`, and `CARGO_HTTP_CAINFO`. The sandbox does not special-case
those variables; the paths they reference must already be readable through the
workspace, the package-manager preset, a system read root, or an explicit
`process_sandbox.read_roots` entry. Calls that pass an `env` map can use
`env_mode` to replace or patch the inherited environment, and can remove
individual variables with `env_remove`.

For private registries, vendored SDKs, self-signed CA bundles outside the
default roots, or offline caches, add process-only roots at the policy layer:

```harn,ignore
let policy = {
  capabilities: {workspace: ["read_text"], process: ["exec"]},
  workspace_roots: [project_root()],
  process_sandbox: {
    read_roots: [
      "/opt/acme/npm-cache",
      "/opt/acme/pip-wheelhouse",
      "/opt/acme/certs",
    ],
  },
}
```

`process_sandbox.read_roots` lets subprocesses read those paths without
granting Harn file builtins access to them. In air-gapped environments, seed
the registry config and cache before the run, point the inherited package
manager/proxy/CA variables at readable files, and keep network side effects
disabled; if a tool must reach a corporate proxy, the active policy still needs
to allow network side effects.

### Writable vs. read-only roots

A policy declares two root lists. `workspace_roots` are read-write: a
path resolving under one passes every scope check, and the OS profile
grants it write when the `workspace.write_text`/`workspace.delete`
capability is present. `read_only_roots` are additive read-only scope:
a path under one passes `read_text`/`list`/`exists` checks but
`write_text`/`delete` are rejected with a `tool_rejected` "read-only
workspace root" violation, and the generated OS profile grants the root
read but never write — even when the policy otherwise allows workspace
writes. The two lists are intended to be disjoint. macOS additionally
re-denies write to each read-only root *after* the broad workspace
write allow (`sandbox-exec` is last-match-wins), so a read-only root
nested under a writable root stays hermetically unwritable; Linux
Landlock rules are purely additive (no deny), so a nested read-only
root under a writable parent inherits the parent's write grant — keep
the lists disjoint when targeting the Linux backend. This lets a caller
mount a reference tree (a shared memory dir, a persona bundle) that the
workload can read but cannot mutate. `CapabilityPolicy::intersect`
narrows each list to the roots common to both sides, with an empty list
on either side deferring to the other.

## Selecting a profile

### From a pipeline

A workflow author sets the profile on the `CapabilityPolicy` that
gates the agent loop or workflow (typically through the agent-session
or workflow-runtime constructors that accept a policy). The default
is `Worktree`, so pipelines that want the strongest confinement
include `sandbox_profile: "os_hardened"` in the policy literal:

```harn,ignore
let policy = {
  capabilities: {workspace: ["read_text"], process: ["exec"]},
  workspace_roots: [project_root()],
  sandbox_profile: "os_hardened",
}
```

### From a single host call

A single subprocess can be promoted (or demoted) without rewriting the
surrounding policy by passing `sandbox_profile` on the
`process.exec` host call. The override is scoped to that call:

```harn,ignore
host_call("process.exec", {
  mode: "argv",
  argv: ["./untrusted-tool", "input.json"],
  cwd: project_root(),
  sandbox_profile: "os_hardened",
})
```

### From an embedder

Embedders that drive the runtime through `harn-vm` directly construct
a `CapabilityPolicy` with the desired profile:

```rust
let policy = CapabilityPolicy {
    workspace_roots: vec![workspace.display().to_string()],
    sandbox_profile: SandboxProfile::OsHardened,
    ..Default::default()
};
push_execution_policy(policy);
```

## Capability → kernel-knob mapping

The runtime translates the active capability ceiling into per-platform
knobs. The mapping is intentionally narrow — each capability maps to a
small, named kernel feature, never an open-ended escape hatch.

### Linux (`crates/harn-vm/src/stdlib/sandbox/linux.rs`)

| Capability / policy | Kernel knob | Effect |
|---|---|---|
| `workspace.read_text` / `workspace.list` / `workspace.exists` | Landlock LSM `LANDLOCK_ACCESS_FS_READ_FILE` + `_READ_DIR` + `_EXECUTE` | reads under `workspace_roots` and the `system_read_roots()` allowlist (`/bin`, `/lib`, `/lib64`, `/usr`, `/etc`, `/nix/store`, `/System`) |
| `workspace.write_text` | Landlock `_WRITE_FILE` + `_REMOVE_*` + `_MAKE_*` + (ABI ≥ 2) `_REFER` + (ABI ≥ 3) `_TRUNCATE` | writes scoped to `workspace_roots` |
| `workspace.delete` | Landlock `_REMOVE_DIR` + `_REMOVE_FILE` | removes scoped to `workspace_roots` |
| `read_only_roots: [...]` | Landlock `_READ_FILE` + `_READ_DIR` + `_EXECUTE` only | each read-only root is readable but never writable, regardless of the `workspace.*` capabilities |
| `process_sandbox.presets` includes `package_manager_config` | Landlock read-only rules for existing npm, pip, cargo, git, and CA config/cache roots under `$HOME` | package managers can resolve real per-user config without granting Harn file builtin access or write rights |
| `process_sandbox.read_roots` / `.write_roots` | Landlock read-only rules, plus writable rules only when workspace writes are allowed | process-only roots for SDKs/caches without widening Harn file builtins |
| standard process devices | Landlock grants read/write on `/dev/null` and read-only access on `/dev/zero`, `/dev/random`, and `/dev/urandom`; ABI ≥ 5 also handles `_IOCTL_DEV` but does not grant it to these device rules | language runtimes and test harnesses can open the devices they normally need without broad `/dev` access or device ioctl rights |
| `side_effect_level < network` | seccomp-bpf blocklist on `socket`, `socketpair`, `connect`, `accept`, `accept4`, `bind`, `listen`, `sendto`, `sendmsg`, `recvfrom`, `recvmsg` (return `EPERM`) | network syscalls fail without taking down the process |
| always | seccomp-bpf blocklist on `bpf`, `mount`, `umount2`, `init_module`, `delete_module`, `finit_module`, `kexec_*`, `ptrace`, `process_vm_readv`/`process_vm_writev`, `perf_event_open`, `swapon`/`swapoff`, `reboot`, `userfaultfd`, `fanotify_init`, `open_by_handle_at` (return `EPERM`) | tier-1 dangerous syscalls are denied unconditionally |
| always | `prctl(PR_SET_NO_NEW_PRIVS, 1)` | no setuid escalation across `exec` |

The Landlock ruleset is built lazily from `landlock_abi_version()`:
unknown access bits are masked off so a recent userspace stays
forward-compatible with older kernels. ABI 0 (no Landlock at all)
falls back to the warn/enforce decision documented above.

### macOS (`crates/harn-vm/src/stdlib/sandbox/macos.rs`)

| Capability / policy | `sandbox-exec` rule | Effect |
|---|---|---|
| always | `(deny default)` | every operation requires an explicit allow |
| always | `(allow process*)` + `(allow sysctl-read)` + `(allow mach-lookup)` + `(allow file-read-data (literal "/"))` | minimum surface required to exec a binary |
| standard process devices | `(allow file-read* ...)` for `/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, and `/dev/fd`; `(allow file-write* ...)` only for `/dev/null`, `/dev/stdout`, `/dev/stderr`, and `/dev/fd` | common stdio, entropy, and zero devices work without granting broad `/dev` writes |
| `process_sandbox.presets` | named read/write rules for `system_runtime`, `developer_toolchains`, `package_manager_config`, and `user_temp` | default process reach for system binaries, Xcode/Homebrew/toolchains, read-only package-manager home config, and per-user developer-tool caches without granting Harn file builtin access |
| `workspace_roots: [...]` / `read_only_roots: [...]` | `(allow file-read* (subpath "<root>"))` | workspace and read-only roots are readable |
| `workspace.write_text` / `workspace.delete` (or empty `capabilities`) | writable `user_temp`, `process_sandbox.write_roots`, and `workspace_roots`, followed by `(deny file-write* (subpath "<read_only_root>"))` | scratch dirs, explicit process-write roots, and writable `workspace_roots` are writable; each `read_only_roots` entry is then re-denied write. `sandbox-exec` is last-match-wins, so the trailing deny keeps a read-only root nested under a writable root unwritable even though the two lists are nominally disjoint |
| `side_effect_level >= network` | `(allow network*)` | otherwise outbound network is denied |

SwiftPM commands (`swift build`, `swift test`, `swift run`, and
`swift package`) run with Harn's outer sandbox as the enforcement layer.
Harn passes `--disable-sandbox` to avoid SwiftPM's nested
`sandbox-exec` call, which macOS rejects from inside an existing sandbox,
and points SwiftPM cache/config/security paths at `.build/harn/swiftpm/`
inside the workspace rather than granting default access to user-level
SwiftPM state.

`sandbox-exec` is officially deprecated but remains the platform
mechanism Apple ships for non-App-Store binaries. We track that
status in the file-level docstring and will switch to a supported
successor when one exists.

### Windows (`crates/harn-vm/src/stdlib/sandbox/windows.rs`)

| Capability / policy | Win32 mechanism | Effect |
|---|---|---|
| always | `CreateAppContainerProfile` + `STARTUPINFOEX` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` | the process runs inside a per-spawn AppContainer with no capability SIDs |
| `workspace.write_text` / `workspace.delete` | `icacls /grant *<sid>:(OI)(CI)M /T /C` on each `workspace_roots` entry | the AppContainer SID gets Modify access on the roots; revoked on `Drop` |
| read-only (denied workspace write, or any `read_only_roots` entry) | `icacls /grant *<sid>:(OI)(CI)RX /T /C` | the AppContainer SID gets ReadAndExecute; `read_only_roots` always use this grant even when workspace writes are allowed |
| `process_sandbox.read_roots` / `.write_roots` | `icacls /grant *<sid>:(OI)(CI)RX` or Modify | process-only roots, with writes gated by workspace-write capability |
| always | `CreateJobObjectW` with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, `_DIE_ON_UNHANDLED_EXCEPTION`, `_ACTIVE_PROCESS` (cap 32), `_PROCESS_MEMORY` (cap 512 MiB) | resource caps and lifecycle binding |
| always | `JOBOBJECT_BASIC_UI_RESTRICTIONS` blocking `HANDLES`, `READCLIPBOARD`, `WRITECLIPBOARD`, `SYSTEMPARAMETERS`, `DISPLAYSETTINGS`, `GLOBALATOMS`, `DESKTOP`, `EXITWINDOWS` | UI surface is blocked |
| always | direct `CreateProcessW` with explicit handle list and `STARTF_USESTDHANDLES` | stdin/stdout/stderr inheritance is restricted to the three pipes the runtime created |

`std::process::Command` cannot carry an AppContainer
`SECURITY_CAPABILITIES` block, so Windows callers must use
`process_sandbox::command_output(...)` (which goes through
`SandboxBackend::run_to_output`). The `std_command_for` /
`tokio_command_for` helpers warn-or-error per the active fallback
policy.

### OpenBSD (`crates/harn-vm/src/stdlib/sandbox/openbsd.rs`)

| Capability / policy | OpenBSD mechanism | Effect |
|---|---|---|
| always | `unveil("/bin", "rx")`, `("/usr", "rx")`, `("/lib", "rx")`, `("/etc", "r")`, `("/dev", "rw")` | minimum surface required to exec |
| `workspace_roots: [...]` | `unveil("<root>", "rwcx" \| "rx")` | rwcx when `workspace.write_text` / `workspace.delete` present, otherwise rx |
| `read_only_roots: [...]` | `unveil("<root>", "rx")` | each read-only root is read+execute only, never write/create |
| `process_sandbox.read_roots` / `.write_roots` | `unveil("<root>", "rx" \| "rwcx")` | process-only roots, with writes gated by workspace-write capability |
| always | `pledge("stdio rpath proc exec", NULL)` | minimum process-exec promise set |
| `workspace.write_text` / `workspace.delete` | adds `wpath cpath dpath` to pledge | filesystem mutation promises |
| `side_effect_level >= network` | adds `inet dns` to pledge | network promises |

## How spawns route

```text
caller
  │
  ▼
process_sandbox::{command_output, std_command_for, tokio_command_for}
  │
  ├── if no orchestration policy is active           → direct spawn
  ├── if profile == Unrestricted or Wasi             → direct spawn
  ├── if HARN_HANDLER_SANDBOX=off (Worktree only)    → direct spawn
  └── otherwise                                       → ActiveBackend
        │
        ├── linux::Backend       (pre_exec → seccomp + Landlock)
        ├── macos::Backend       (wrap with sandbox-exec)
        ├── openbsd::Backend     (pre_exec → unveil + pledge)
        └── windows::Backend     (CreateProcessW with AppContainer + Job Object)
```

The backend trait is defined in
`crates/harn-vm/src/stdlib/sandbox/mod.rs`; one impl is selected at
compile time via `cfg`-gated `mod` declarations. Adding a new
backend means writing one file under `sandbox/` and adding the
`mod` plus `type ActiveBackend` lines.

## Sandboxes are the runtime arm of a permission policy

A sandbox is the runtime answer to a declared permission policy. The
authoritative policy model (`policy { read, write, exec, net }`) lives
in `harn-serve`'s `permissions` module; `permissions::enforcement`
lowers it into the two enforcement vocabularies described above:

- `to_capability_policy` derives the in-VM `CapabilityPolicy` ceiling
  — `read`/`write`/`exec` become `workspace`/`process` capabilities and
  the `side_effect_level`, threaded with a chosen `SandboxProfile`.
- `to_sandbox_spec` / `to_network_policy` (behind the `hostlib`
  feature) turn the `net` allowlist into a `SandboxSpec` egress policy
  for a `SandboxBackend` to provision against.

The pluggable backend contract — `SandboxBackend`, `SandboxSpec`,
`ExecRequest`/`ExecResult`, `NetworkPolicy`, mounts, and limits — lives
in `harn-hostlib`'s `sandbox` module, alongside the `LocalSandbox`
backend. `LocalSandbox` does not reimplement OS confinement: it pushes
a `CapabilityPolicy` and runs each command through the same `harn-vm`
process sandbox documented here. Remote backends (Fly Machines, Modal,
E2B, …) implement the same trait from wherever they run.

## Diagnostics from a script

Three Harn builtins surface backend identity for `harn doctor`-style
scripts and conformance fixtures:

| Builtin | Returns | Use |
|---|---|---|
| `sandbox_active_backend()` | `string` | name of the compiled-in backend (`linux`, `macos`, `windows`, `openbsd`, `noop`) |
| `sandbox_backend_available()` | `bool` | whether the platform mechanism behind the backend is reachable on the running host |
| `sandbox_active_profile()` | `string` | profile carried by the current execution policy (`worktree` under default `harn run`, `unrestricted` if no policy is active) |

## Replay fidelity

`CapabilityPolicy` round-trips through serde with `sandbox_profile`
included, so every `RunRecord` records which profile was active when
the run was made. `harn replay` evaluates the recorded fixture
without re-executing subprocess spawns — the process tape supplies
the captured `Output` — so the replay host does not need the same
sandbox mechanism the original run used. Re-execution flows
(`harn test bench`) push the recorded `CapabilityPolicy` back onto
the execution stack, so they re-apply the same profile and fail
loudly under `os_hardened` if the replay host lacks the platform
mechanism.

## Out of scope

- gVisor / Firecracker / Kata containers — those belong to a remote
  `SandboxBackend` impl (Fly Machines, Modal, …), not the local
  runtime; the trait lives in `harn-hostlib`'s `sandbox` module.
- Destination-level network egress allow/deny — use `egress_policy(...)`
  or `HARN_EGRESS_*` once a host policy allows network side effects.
- Sandboxing for in-process work (LLM calls, deterministic Harn
  evaluation). Capability ceilings and the approval policy are the
  enforcement layers there; the OS sandbox only kicks in when Harn
  spawns a subprocess.
