# Process sandboxing

Harn ships an OS-level process sandbox that engages whenever a
subprocess is spawned under an active capability policy. The sandbox
runs in addition to the workspace-root path enforcement and the
approval-policy DSL — defense in depth, not a replacement.

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
| always | `(allow process*)` + `(allow sysctl-read)` + `(allow mach-lookup)` + `(allow file-read-data (literal "/"))` + `(allow file-write* (subpath "/dev"))` | minimum surface required to exec a binary and reach `/dev` |
| always | `(allow file-read* (subpath "/bin" \| "/etc" \| "/Library" \| "/opt/homebrew" \| "/private/etc" \| "/System" \| "/usr"))` | read access to the directories the dynamic linker and most CLI tools need |
| `workspace_roots: [...]` | `(allow file-read* (subpath "<root>"))` | workspace roots are readable |
| `workspace.write_text` / `workspace.delete` (or empty `capabilities`) | `(allow file-write* (subpath "/tmp" \| "/private/tmp" \| "/var/tmp"))` + `(allow file-write* (subpath "<root>"))` | scratch dirs and workspace roots are writable |
| `side_effect_level >= network` | `(allow network*)` | otherwise outbound network is denied |

`sandbox-exec` is officially deprecated but remains the platform
mechanism Apple ships for non-App-Store binaries. We track that
status in the file-level docstring and will switch to a supported
successor when one exists.

### Windows (`crates/harn-vm/src/stdlib/sandbox/windows.rs`)

| Capability / policy | Win32 mechanism | Effect |
|---|---|---|
| always | `CreateAppContainerProfile` + `STARTUPINFOEX` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` | the process runs inside a per-spawn AppContainer with no capability SIDs |
| `workspace.write_text` / `workspace.delete` | `icacls /grant *<sid>:(OI)(CI)M /T /C` on each `workspace_roots` entry | the AppContainer SID gets Modify access on the roots; revoked on `Drop` |
| read-only | `icacls /grant *<sid>:(OI)(CI)RX /T /C` | the AppContainer SID gets ReadAndExecute |
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

## Diagnostics from a script

Three Harn builtins surface backend identity for `harn doctor`-style
scripts and conformance fixtures:

| Builtin | Returns | Use |
|---|---|---|
| `sandbox_active_backend()` | `string` | name of the compiled-in backend (`linux`, `macos`, `windows`, `openbsd`, `noop`) |
| `sandbox_backend_available()` | `bool` | whether the platform mechanism behind the backend is reachable on the running host |
| `sandbox_active_profile()` | `string` | profile carried by the current execution policy (`unrestricted` if no policy is active) |

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

- gVisor / Firecracker / Kata containers — those belong to
  `harn-cloud`'s `SandboxBackend` impl, not the local runtime.
- Network egress allow/deny by domain — leave to `with_consent` /
  `approval_policy.rules`.
- Sandboxing for in-process work (LLM calls, deterministic Harn
  evaluation). Capability ceilings and the approval policy are the
  enforcement layers there; the OS sandbox only kicks in when Harn
  spawns a subprocess.
