<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Sandbox mode

The `harn run` command installs a default worktree sandbox before the
VM starts. The default policy uses `sandbox_profile: "worktree"`,
roots filesystem/process access at the nearest `harn.toml` project
root (or the invocation working directory when no project manifest is
present), allows local process execution, and denies network side
effects. This makes direct runs fail closed for filesystem escapes,
subprocess working directories, and outbound HTTP/connectors unless
the operator explicitly opts out.

`harn run` also supports builtin allow/deny flags that restrict which
builtins a program may call.

### --no-sandbox

```bash
harn run --no-sandbox script.harn
```

Disables the default worktree filesystem/process sandbox and the
network side-effect ceiling for this invocation. The CLI emits a
warning when this escape hatch is used. `--deny` / `--allow` still
apply when present.

### --read-only-root

```bash
harn run --read-only-root /path/to/other-repo main.harn
```

Permit reads from additional filesystem roots while keeping the default
`harn run` sandbox policy intact. Paths under each `--read-only-root`
are allowed for read-style operations but rejected for writes. Use this
when a script needs to consume files from a sibling checkout or shared
assets without disabling sandboxing. `--read-only-root` cannot be
combined with `--no-sandbox`.

### --write-root

```bash
harn run --write-root /path/to/output main.harn
```

Permit reads and writes under additional filesystem roots while keeping
the default `harn run` sandbox policy intact. Each `--write-root` entry
is added to the run's `workspace_roots`, so the same filesystem builtins,
process cwd checks, and OS sandbox backends enforce it as a first-class
writable mount. Use this for external output folders such as iCloud Drive
exports without falling back to `--no-sandbox`. `--write-root` cannot be
combined with `--no-sandbox`.

### --sandbox-read-root / --sandbox-write-root

```bash
harn run --sandbox-read-root /path/to/sdk \
  --sandbox-write-root /path/to/cache main.harn
```

Permit a spawned subprocess to read or write an additional root without
granting Harn filesystem builtins access to that root. These repeatable flags
populate `process_sandbox.read_roots` and `process_sandbox.write_roots`
respectively while leaving the rest of the default sandbox active. Neither
flag can be combined with `--no-sandbox`.

Restricted worktree profiles also create a self-ignored
`.harn-toolchain-cache/` directory under the first writable workspace root.
The spawned-process environment points mutable home, compiler-cache,
package-manager-cache, and Python user-site state into this directory unless
the caller explicitly supplies the corresponding environment variable.

### --deny

```bash
harn run --deny read_file,write_file,exec script.harn
```

Denies the listed builtins. Any call to a denied builtin produces a
runtime error:

```text
Permission denied: builtin 'read_file' is not allowed in sandbox mode
  (use --allow read_file to permit)
```

### --allow

```bash
harn run --allow llm,llm_stream,llm_stream_call script.harn
```

Allows only the listed builtins plus the core builtins (see below). All
other builtins are denied.

`--deny` and `--allow` cannot be used together; specifying both is an error.

### Core builtins

The following builtins are always allowed, even when using `--allow`:

`println`, `print`, `log`, `type_of`, `to_string`, `to_int`, `to_float`,
`len`, `assert`, `assert_eq`, `assert_ne`, `json_parse`, `json_stringify`,
`runtime_context`, `task_current`, `runtime_context_values`,
`runtime_context_get`, `runtime_context_set`, `runtime_context_clear`

### Propagation

Sandbox restrictions propagate to child VMs created by `spawn`,
`parallel`, and `parallel each`. A child VM inherits the same set of
denied builtins as its parent.

### Handler capability sandbox

When a workflow or handler runs under an active `CapabilityPolicy`,
Harn also enforces `workspace_roots` at runtime for filesystem builtins.
Attempts to read, write, create, copy, stat, list, or delete paths outside
the declared roots fail as typed `tool_rejected` sandbox violations.
File-backed prompt-template rendering (`render`, `render_prompt`,
`render_with_provenance`, `template.render`, and `include`) follows the
same read boundary. Embedded `std/...` prompt assets are not filesystem
reads.
Path-backed `vision_ocr(...)` image inputs follow the same read boundary.
Process cwd escapes through `exec_at` / `shell_at` are rejected the same way.

Pure-compute handlers can run through the WASM sandbox entrypoint exposed
by `harn-wasm` as `executePureComponent` and described by
`crates/harn-wasm/wit/harn-pure.wit`. That component surface has no host
imports for filesystem, process, network, clock, random, LLM, or async
effects, so attempted side effects fail inside the component boundary.

Process execution is wrapped in an OS sandbox selected by the active
`CapabilityPolicy.sandbox_profile`. The default profile is `worktree`:
workspace-root path enforcement plus best-effort OS confinement, with
`HARN_HANDLER_SANDBOX={off,warn,enforce}` controlling what happens when
the platform mechanism is unavailable. Pipelines opt into
`sandbox_profile: "os_hardened"` to make the OS confinement required —
spawns return `tool_rejected` if the platform mechanism is missing,
regardless of `HARN_HANDLER_SANDBOX`. The Tesseract subprocess used by
`vision_ocr(...)` is launched through the same sandbox entrypoint. The
per-platform mechanisms are:

- **Linux**: a Landlock LSM ruleset derived from `workspace_roots` and
  the workspace capability set, plus a seccomp-bpf blocklist of
  Tier-1 dangerous syscalls (and the network family when the
  side-effect level is below `network`); `PR_SET_NO_NEW_PRIVS` is
  always enabled.
- **macOS**: a `sandbox-exec` profile rendered from the policy.
  Writes are limited to scratch dirs plus declared `workspace_roots`
  only when the policy allows workspace writes; network is allowed
  only when the side-effect ceiling permits `network`.
- **Windows**: a per-spawn AppContainer with no capability SIDs plus
  a Job Object capping memory, process count, and UI surface;
  `icacls` grants the AppContainer SID Modify (or ReadAndExecute)
  on each `workspace_roots` entry for the lifetime of the spawn.
- **OpenBSD**: `pledge` promises and `unveil` path permissions
  derived from the same policy.

`SandboxProfile::Unrestricted` skips both path enforcement and OS
confinement; `harn run --no-sandbox` is the CLI escape hatch that
leaves direct runs in that state. `SandboxProfile::Wasi` is
testbench-only — subprocesses are intercepted by the process tape and
resolved against recorded WASI modules. See the
[sandboxing guide](https://harnlang.com/sandboxing.html) for the full
per-platform capability → kernel-knob mapping table.
