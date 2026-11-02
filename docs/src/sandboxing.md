# Process sandboxing

**What this page covers:** which directories a script can read and write, and
what a command the script spawns is allowed to do. Three related boundaries
are documented elsewhere, and mixing them up is the usual source of
confusion:

| If you are asking… | Go to |
|---|---|
| Which environment variables and secrets can the script see? | [Environment policies and grants](./cli-reference.md#environment-policies-and-grants) |
| Which hosts may the Harn runtime itself call over the network? | [Egress policy](./builtins.md#http) (`HARN_EGRESS_ALLOW`, `harness.net.egress_policy(...)`) |
| What is a capability policy, and who sets one? | [Host boundary](./host-boundary.md) |

`harn run` confines a script to its own project directory before the VM
starts. You do not turn the sandbox on. It is already on, and this page is
mostly about how to give a script the one extra path or socket it needs
without switching it off.

Three terms show up throughout, so it helps to pin them down first:

- A **capability policy** is the record of what one run is allowed to do:
  which directories are readable, which are writable, and how far its side
  effects may go. `harn run` builds one for you; see
  [Host boundary](./host-boundary.md) for the full reference.
- A **root** is one directory that a policy opens up, along with everything
  beneath it.
- The **side-effect ceiling** is the highest class of effect a run may reach,
  ordered from harmless to far-reaching. The default stops just below
  `network`, so a script can touch files in its project but cannot open a
  socket.

Confinement then comes in two layers. Harn checks every path itself, against
the roots in the active policy. Separately, when a script spawns a
subprocess, the operating system confines that child using whatever mechanism
the platform provides: Landlock or bubblewrap on Linux, `sandbox-exec` on macOS. Windows has
no OS sandbox, so a child there runs unconfined; see
[Windows](#windows-and-other-platforms-without-a-backend).

Both layers stay in force alongside approval policy, the separate rule set
that decides which risky operations have to ask a human first. None of the
three replaces the others, so a path can be refused by any one of them.

On Unix, scoped content writes walk parent directories without following
symlinks and carry the resulting parent file descriptor into the final file
open. This defends against user-creatable symlink redirection inside a
workspace; it does not defend against bind mounts installed by a privileged
host administrator. The workspace sandbox therefore treats host root access
as outside its threat model.

## What a default run can do

A direct `harn run` installs the `worktree` capability policy before the VM
starts. That policy:

- roots filesystem and process-cwd access at the nearest `harn.toml`
  project root, falling back to the working directory you launched from
  when there is no manifest;
- holds the side-effect ceiling below `network`.

So a script can read and write inside its own project and nowhere else, and
neither it nor anything it spawns can open a socket.

Pass `--allow-http-read` for direct HTTP reads under the run's destination
policy. This sets the existing side-effect ceiling to `network_read`, below
full `network` access. Harn still denies remote mutations and child-process
sockets, and keeps filesystem confinement active. Set `HARN_EGRESS_ALLOW` or
`harness.net.egress_policy(...)` to name the destinations the script may read.
This flag conflicts with `--allow-process-network` and `--no-sandbox`.

Pass `--allow-process-loopback` when a confined child needs a local TCP server,
as test runners commonly do. On macOS this admits TCP bind, accept, and connect
only on IPv4 or IPv6 loopback; direct remote sockets remain denied. Linux needs
the host's network-namespace launcher and
[withholds the grant when it is unavailable](#loopback-on-a-host-that-cannot-grant-it).
Other unsupported backends refuse the grant.

Pass `--sandbox-unix-socket-root <path>` when a confined child needs a local
socket file rather than a TCP port. Build servers are the common case: sbt,
Gradle's Kotlin daemon, and MSBuild worker nodes talk to themselves over a
Unix-domain socket under the project or the temp dir, and without this grant
the build dies with a bare `Operation not permitted`. On macOS, bind and
connect are scoped to socket files under the given roots. Linux grants the
serving operations under these roots; connections require a separate SSH
agent or general network grant. Other local backends reject a non-empty
socket-root grant. Nothing in the socket-root grant opens IP networking.

Pass `--allow-ssh-agent` to let a confined command sign Git commits through the
launcher's `SSH_AUTH_SOCK`. The process policy records that one socket as
`process_sandbox.ssh_agent_socket` and passes its path to children even with
`--environment-policy isolated`. Nested tool policies inherit the host's exact
endpoint and cannot invent or redirect it. It grants connection to that endpoint without
granting private-key reads, directory reads under `~/.ssh`, socket creation, or
IP networking. Other connections remain governed by their existing grants.
macOS enforces the exact socket path. Linux requires an enforcing Landlock
backend with ABI 9 for this path restriction. Older kernels and the bubblewrap
fallback refuse the grant unless the policy already permits general networking.
Other backends refuse it. The socket must
exist and have an absolute path; an unset `SSH_AUTH_SOCK` is a CLI error.

When a confined child fails, the refusal record names the boundary that
refused it: `mechanism` is one of `egress`, `local_socket`, `home_read`,
`write`, or `unknown`, and `reason` spells out what was and was not granted.
This is the same record the agent-visible error carries, so a build that
needed a socket root is not misread as a network denial or a toolchain defect.

Pass `--allow-process-network` to allow network access for the Harn run and its
child processes under the run's egress policy. Filesystem and process
confinement remain active. Supported local sandboxes route child HTTP, HTTPS,
and SOCKS5 traffic through Harn's managed forwarding proxy and restrict the
child itself to that proxy. Children reach public hosts by default; private and
loopback addresses stay denied. `HARN_EGRESS_*` or
`harness.net.egress_policy(...)` narrows that default.

`harness.net.egress_policy(...)` does not grant network access. It restricts
the destinations available to a run that already has network access, so a
script can install its policy before the network grant. See the
[egress-policy reference](./builtins.md#http) for the policy fields.

Command tool receipts retain the effective network grant in `sandbox.network`.
See [command network grants](./long-running-tools.md#command-network-grants)
for the fields and their limits.

A matching [`harness.testing.http_mock`](./builtins.md#mock-http) is an
in-process fixture, so it does not require a network grant. An unmatched HTTP
call still requires network access and passes through the active egress policy.

## Widening the sandbox for one path

Scripts often need a single path outside the project. Writing a status file
to a host-level state directory, say, or reading a sibling checkout. Grant
that one path rather than reaching for `--no-sandbox`:

| Flag | Effect |
|---|---|
| `--write-root <path>` | Adds a writable root for Harn and its subprocesses. |
| `--read-only-root <path>` | Adds a readable root for Harn and its subprocesses. |
| `--sandbox-write-root <path>` | Adds a writable root for subprocesses only. |
| `--sandbox-read-root <path>` | Adds a readable root for subprocesses only. |

The first two widen the active profile and leave the rest of the sandbox
intact. The last two populate `process_sandbox.write_roots` and
`.read_roots`, so a compiler or package manager gets the path while Harn's
own filesystem builtins still cannot touch it. Reach for those when only
the child needs access.

All four are rejected alongside `--no-sandbox`, which would make them
meaningless.

A run that grants a root prints one line to stderr naming exactly what it
widened:

```text
sandbox active; extra write root: /path
```

That line replaces the blanket `--no-sandbox` warning. A run that grants
nothing prints nothing.

## Opting out for a single run

`--no-sandbox` removes the confinement entirely for one invocation, and the
CLI warns when you use it. Prefer a scoped root whenever you can name the
path you actually need.

## Managed child-process egress

`--allow-process-network` grants socket access to the Harn runtime and its
child processes. `HARN_EGRESS_ALLOW` and `harness.net.egress_policy(...)`
restrict calls made by Harn, including HTTP, provider, and connector calls.
The same live policy state also configures a host-side HTTP/SOCKS5 proxy for
child traffic, whether it came from `HARN_EGRESS_*` at startup or
`harness.net.egress_policy(...)` during the run. Until either source configures
a policy, the grant itself is the child's policy: every public host is allowed
and private, link-local, and loopback addresses are denied. The OS sandbox grants the child
only the proxy's ephemeral loopback ports, so clearing `HTTP_PROXY` or opening a
raw socket cannot bypass the host decision. DNS is resolved by the proxy and
each connection is pinned to the addresses checked by the existing CIDR, deny,
and private-address rules.

The native managed boundary is currently available on macOS. On another local
platform, a CLI `--allow-process-network` grant still raises the capability
ceiling to network, but Harn does not *start* a proxy it cannot enforce. Child
processes then have unrestricted sockets; `harn run` discloses that on stderr,
and run attestation reports `unrestricted` rather than
`unsupported_fail_closed`. An execution policy that already carries
`process_network_proxy` (inherited from a host or outer sandbox) is unchanged:
Linux and Windows still fail-close `process.exec` behind that proxy. Remote
sandbox backends continue to advertise their own `network_policy` capability.
An empty allowlist is deny-all everywhere.

The managed proxy records only protocol, normalized host, port, decision, and
reason. It never records request paths, query strings, headers, bodies, proxy
credentials, or environment values.

## Environment variables are a separate boundary

Sandbox roots control *where* a script reaches. What it can *read out of the
environment* is governed independently, by the session's environment policy:
`inherited` snapshots the launcher environment, `isolated` admits runtime
essentials only, and `granted` adds the `--grant` mappings you declare. The
resolved values govern `harness.env`, providers, and subprocesses alike.

The two boundaries never widen each other, and environment policy stays
active under `--no-sandbox`. See
[Environment policies and grants](./cli-reference.md#environment-policies-and-grants).

### Cargo compiler wrappers

Confined process calls measure a configured Rust compiler wrapper for a
compiling Cargo command, such as `cargo build` or `cargo test`. Complete,
literal shell invocations also qualify when their startup environment can't
change command lookup and they don't set environment variables in the command.
Harn keeps the shell invocation unchanged.
Version checks and informational commands don't trigger the probe.
Harn builds a temporary crate with Cargo under the
active OS sandbox. A wrapper stays enabled only if that build succeeds and
leaves no confined background process running. Harn starts known compiler-cache
servers outside confinement before measuring them.

Other programs and uncertain shell invocations receive empty `RUSTC_WRAPPER`,
`RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, and
`CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER` settings. This clears inherited and
Cargo-configured wrappers without triggering a compiler build during unrelated
commands such as Git inventory. A shell or script can set these variables again
before launching Cargo; Harn does not intercept that nested launch or measure
its wrapper. The nested process remains subject to the OS sandbox.

Wrapper decisions are reused within the process for the same working directory,
child environment, and OS confinement. Changing an agent's tool list or
recursion budget doesn't repeat the probe. Changes to process permissions,
Cargo configuration, or wrapper executable files repeat it. Configurations with
includes are measured on every compiling launch because Harn cannot verify
their full file dependencies.
Decisions aren't persisted across processes: compiler-cache servers can stop
between launches, changing whether a wrapper would start a confined daemon.

JSON runs emit `compiler_wrapper_applied` for each applied decision, including
cached decisions. The record names the wrapper, reason, working directory,
program and sandbox profile. It records preparation; the command result proves
whether execution succeeded. See the [JSON event contract](cli-json-contract.md#harn-run---json).

Git configuration discovery also caches results within the process. Changes to
the child environment, config files, included files, or repository branch
invalidate those results. A failed query isn't cached as an empty configuration.
On macOS, discovery uses an installed Git binary from Command Line Tools or
Xcode when available, avoiding the `/usr/bin/git` toolchain lookup shim.

## Sandbox profiles

The active [`CapabilityPolicy`](./host-boundary.md) carries a
`sandbox_profile` field. Pipelines and the `process.exec` host call set
it explicitly when they want stronger or weaker isolation than the
default.

A profile decides two independent questions:

- **Path enforcement** — do Harn's own in-process checks apply? This
  covers the `harness.fs.*` builtins scoping against `workspace_roots`
  and `read_only_roots`, plus the launch-cwd check for subprocesses. It
  is portable and deterministic, because Harn performs it itself.
- **OS confinement** — is a platform mechanism (Linux Landlock or bubblewrap with seccomp,
  macOS sandbox-exec) applied to spawned subprocesses? Windows has
  none. This depends on a mechanism that may be unavailable, and it is
  the only axis that can deny a child something Harn never asked about.

| Profile | Path enforcement | OS confinement | When the spawn fails |
|---|---|---|---|
| `unrestricted` | skipped | skipped | only on direct OS errors |
| `workspace_paths` | required (`workspace_roots`) | skipped | only on direct OS errors |
| `worktree` *(default)* | required (`workspace_roots`) | platform-dependent | Linux refuses unusable filesystem backends; platforms without an OS backend warn unless `HARN_HANDLER_SANDBOX=enforce` |
| `wasi` | required | not applied on the host path | testbench-only; the host spawn path is never reached |
| `os_hardened` | required | **required** | spawn returns `tool_rejected` if the platform mechanism is missing or rejects the call, regardless of `HARN_HANDLER_SANDBOX` |

`workspace_paths` is for callers that own the code they are running and
want its writes confined, but must let it shell out freely — a test
runner isolating cases from each other, a build driver invoking a
toolchain. OS confinement would buy nothing there, since the subprocess
is as trusted as its parent, while costing portability. **Do not use it
for foreign or untrusted code:** a subprocess it spawns is unconfined,
so path scoping alone is not containment.

Top-level `agent_loop` sessions install an empty-ceiling `os_hardened`
carrier by default, so agent subprocess tools require the OS sandbox
even when the caller did not pass an explicit capability policy. On
Windows, which has none, those tools are refused. Direct scripts and
process calls keep the `worktree` default unless their caller selects a
stricter profile.

The strictness ladder is `unrestricted < workspace_paths < wasi <
worktree < os_hardened`. `CapabilityPolicy::intersect` always picks the
strictest of the two profiles when a parent ceiling is composed with a
child request — so a lenient parent cannot weaken a child's
`os_hardened` ask, and a child asking for `workspace_paths` cannot shed
an OS sandbox its parent imposed.

The `wasi` profile describes testbench execution, not host-process isolation.
A child requesting it under `worktree` or `os_hardened` keeps the parent's
host-confinement profile; the testbench's process interceptor selects the
WebAssembly runner separately.

`HARN_HANDLER_SANDBOX=off` skips OS confinement for `worktree`. On Linux,
`warn` and `enforce` both require usable Landlock or bubblewrap confinement.
Platforms without an OS backend warn under `warn` and refuse under `enforce`.
`os_hardened` ignores this setting because its contract requires OS confinement.

### What each backend confines

OS confinement is not one thing. Each backend's mechanism holds a confined child
to some dimensions of the policy and not others, and this table is the runtime's
statement of which. `harn doctor` reports the active backend's row, and an
`os_hardened` spawn is refused when its policy requires a dimension the row marks
`not enforced`. Other profiles run, and the row is their receipt.

A cell is filled only from what the sandbox conformance suite measures on that
platform, and the suite judges every case against it, so a cell that claims more
or less than the backend does fails a named case. `unmeasured` means no
measurement backs a claim either way. Network confinement is only required below
the `network` ceiling. No policy term requires process confinement yet.

`harn doctor sandbox` measures the contract through real child processes and exits
nonzero if an applicable case fails, goes unmeasured, or isn't enforced. It writes
each case's start and verdict to stderr as execution proceeds. A start names the
case and counts completed and pending cases, including the current case. If the
run stops, the last unmatched start identifies the unfinished case. With `--json`,
stdout contains only the completed JSON report. An unsuccessful measurement sets
`ok: false` and `error.code: "sandbox_conformance_failed"`, retaining all case
verdicts in `data`. An empty or incomplete report cannot pass.

<!-- sandbox-enforcement-table:begin -->
| Backend | writes | reads | credential reads | network | process |
|---|---|---|---|---|---|
| Linux Landlock | enforced | enforced | enforced | enforced | unmeasured |
| Linux bubblewrap | enforced | enforced | enforced | enforced | unmeasured |
| macOS sandbox-exec | enforced | enforced | enforced | enforced | unmeasured |
| OpenBSD unveil | unmeasured | unmeasured | unmeasured | unmeasured | unmeasured |
| No OS sandbox | not enforced | not enforced | not enforced | not enforced | unmeasured |
<!-- sandbox-enforcement-table:end -->

A row states what a backend confines on a fully capable kernel. An older kernel
can hold the same dimensions with less precision, and each difference a child
can observe is reported as a mechanism limit: `harn doctor` lists them as
`limits=[...]`, `harn doctor sandbox` prints one `limit:` line each, and both
`--json` reports carry them as `mechanism_limits`. When a limit can make an
ordinary command fail, the first confined spawn also logs one
`handler_sandbox` warning that names it.

| Limit | Linux Landlock ABI | What a confined child sees |
|---|---|---|
| `cross_directory_rename_refused` | 1 | `rename` or `link` into another directory fails with `EXDEV`, inside the workspace too |
| `truncation_filtered_by_syscall` | 1 and 2 | `truncate(2)`, `openat2(2)`, and a read-only `open` with `O_TRUNC` fail with `EPERM`; `ftruncate` on a file opened for writing works |
| `device_ioctl_unscoped` | 1 to 4 | `ioctl` on a device opened under a grant isn't scoped; the default grants open only `/dev/null`, `/dev/zero`, `/dev/random`, and `/dev/urandom` |
| `unix_socket_grant_unavailable` | 1 to 5 | a serve-only Unix socket grant is refused |

### Reading a mechanism refusal

A spawn refused because the platform mechanism could not be attached carries
the cause as typed fields rather than as advice prose. The caught value is a
`tool_rejected` dict whose `source` is `sandbox_mechanism` and whose
`sandbox_mechanism` member names the `mechanism` (`linux_landlock`,
`macos_sandbox_exec`, `openbsd_unveil`, or `none` on a platform with no OS
sandbox), the `availability` (`absent_on_host`, `does_not_confine`, or
`host_protection_unavailable`), the requested `profile`, the unsatisfied `requirement`
(`profile` or `fallback`), and `selector_honored` —
false when the requested profile requires the mechanism outright, so no
`HARN_HANDLER_SANDBOX` value can weaken it. A `does_not_confine` refusal also
lists the table dimensions the profile required and the backend does not hold,
as `unconfined` (for example `["reads", "network"]`).

A `host_protection_unavailable` refusal means the mechanism is present but the
host fails a prerequisite that keeps the spawning process's own environment
from confined children. On macOS, `host_protection_gap` names it:
`unrestricted_dtrace` when System Integrity Protection allows unrestricted
DTrace (always the case with SIP disabled, as on GitHub-hosted macOS runners),
`development_kernel`, or `kernel_configuration_unreadable`.

Harn's own message states the mechanism fact and nothing else. Which control an
operator actually has depends on the embedding product: an embedder that hardens
its default has made the fallback selector inert, and may use a Harn ladder name
for a profile of its own. Read the fields and write the remedy sentence you own.

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
    "write_roots": ["/opt/vendor-cache"],
    "allow_tcp_loopback": false,
    "unix_socket_roots": [],
    "allow_child_workspace_write": false
  }
}
```

`presets: null` or an omitted `presets` field selects the runtime defaults:

- `system_runtime`: host runtime directories needed to launch common binaries.
  On macOS, this includes the resolved targets of the public resolver
  configuration and timezone data under `/etc` and `/usr`. Their containing
  private directories remain outside the grant.
- `developer_toolchains`: standard compiler/toolchain locations such as Xcode,
  Command Line Tools, Homebrew, Linux vendor installs under `/opt`, plus common
  home-dir toolchain managers and runtimes such as `~/.local/share/uv`,
  `~/.rustup`, `~/.cargo`, `~/.pyenv`, `~/.nvm`, `~/.volta`, and `~/go`.
- `package_manager_config`: read-only per-user npm, pip, cargo, git, and CA
  config/cache roots under `$HOME`, such as `.npmrc`, `.gitconfig`, `.netrc`,
  `.config/git`, `.config/pip`, `.config/rustfmt`, `.cache/pip`, and cargo
  config paths. It doesn't grant all of `.config` or `.cache`. See the
  [measured toolchain paths](./sandbox-config-census.md) and
  [credential denials](./sandbox-read-deny-reference.md).
- `user_temp`: the session's own temp dir, `.harn-tmp` in the first
  workspace root, which the child's `TMPDIR`, `TMP`, and `TEMP` name. On macOS
  Harn supplies a Darwin user-directory suffix that maps ordinary native tools'
  Foundation temporary-directory lookup to the same storage through an exclusive
  symlink in the native temp root. Apple platform executables such as
  `/usr/bin/osascript` ignore that startup environment setting. A cooperating
  platform client must initialize the supplied suffix with
  `_set_user_dir_suffix` before using Foundation's native temporary storage;
  an unmodified platform client can still have an atomic write refused. The
  sandbox conformance probe initializes this binding explicitly and checks that
  the native path resolves to `.harn-tmp`. The separate SwiftPM product test
  exercises ordinary compiler and XCTest descendants without that probe setup.
  The host's shared temp dirs (`/tmp`, `/var/folders`) are never
  granted: they hold every other process's files. Caches that default there,
  such as clang's module cache and xcrun's lookup cache, are pointed into the
  workspace by the child's environment instead. Writable only when the active
  policy lets child processes write (see `allow_child_workspace_write` below).

An explicit empty `presets: []` disables every named preset. `read_roots` and
`write_roots` are for subprocesses only; `write_roots` are also gated by
whether child processes may write at all. `CapabilityPolicy::intersect` narrows presets and
roots to their common set, so managed or parent ceilings can prevent a child
policy from adding host filesystem reach. `read_deny_roots` is the one subtractive term: it beats every
grant, and it unions rather than intersects as policies nest, because narrowing
a denial would widen authority. See the
[credential denylist reference](./sandbox-read-deny-reference.md).
`allow_tcp_loopback` defaults to
`false` and is host-owned authority: a nested policy can neither introduce it
nor erase an outer host grant. `unix_socket_roots` follows the same rule: a
nested policy keeps only the roots the outer grant already covers. Stage-policy validation also rejects a flattened
child that tries to introduce it beyond its ceiling.

Flattened stages retain every host `read_deny_roots` entry. They also preserve
`netns_launcher_path`, the host-selected network-namespace launcher, exactly:
a stage cannot replace or remove it, or introduce one when the host set none.

#### Loopback on a host that cannot grant it

On Linux a confined child gets loopback only inside a private network
namespace, which needs the namespace launcher named by
`process_sandbox.netns_launcher_path`. When a policy grants
`allow_tcp_loopback` and the host has no usable launcher, Harn drops that one
grant, keeps every other confinement, and emits a `sandbox.loopback_unavailable`
warning event whose metadata carries `grant`, `reason`, and `launcher`.
Commands that need loopback then fail inside the sandbox. It never widens the
syscall filter without the namespace that bounds it.

Embedders ask the same question without spawning:
`harn_vm::process_sandbox::loopback_support(&policy.process_sandbox)` returns
`LoopbackSupport::Supported` or `Unavailable` with a typed reason, and
`withhold_unsupported_loopback(&mut policy.process_sandbox)` applies the rule and
returns what it dropped.

#### Child writes for read-only roles

A child process may write its writable roots (the workspace roots,
`write_roots`, the session temp dir, and the toolchain caches) when the policy's
`workspace` capability grants `write_text` or `delete`, when `capabilities` is
empty, or when `process_sandbox.allow_child_workspace_write` is `true`.
Otherwise it can write nothing, not even its `TMPDIR`, and every build or test
it runs fails on its first write.

`allow_child_workspace_write` exists for roles whose tools run commands but edit
nothing, such as a reviewer or a verifier:

```json
{
  "capabilities": {"process": ["exec"], "workspace": ["read_text", "list"]},
  "sandbox_profile": "os_hardened",
  "process_sandbox": {"allow_child_workspace_write": true}
}
```

It changes only the OS profile rendered for child processes. Harn's own file
builtins still read the `workspace` capability, so the role above cannot call
`write_text`. `read_only_roots` disjoint from the writable roots stay
unwritable to the child on every backend, and nothing outside the writable
roots becomes writable. A read-only root nested inside a writable root differs
by backend: macOS re-denies it, Linux Landlock cannot and grants the parent's
write. `child_write_disposition` reads that answer from the installed rules. A nested policy keeps
the grant only when its ceiling's children could already write, through the
same grant or through the ceiling's `workspace` capability; stage-policy
validation rejects a flattened child that adds it beyond that. The
`harn run` provenance receipt reports `process_child_writes` (whether children
may write) and `process_child_workspace_write_granted` (whether the grant is
what allowed it), and a refused write under a policy whose children may write
nowhere is explained as the missing grant.

### Running real toolchains in the sandbox

The local process sandbox is meant to run normal developer tools, including
toolchains that need mutable build and package-manager state. Restricted
worktree profiles create `.harn-toolchain-cache/` under the writable workspace
and point XDG/cache variables, Go caches, pip/uv caches, and npm/Yarn caches
into it. Explicit per-command environment values
win. The directory self-ignores its contents and can be deleted safely.
`HOME`, `PYTHONUSERBASE`, `PNPM_HOME`, `CARGO_HOME`, `RUSTUP_HOME`, and existing package-manager config files keep
pointing at their narrowly scoped toolchain/config preset roots so installed
toolchains and private-registry configuration remain usable. `PNPM_HOME`
contains installed pnpm versions, so moving it would make an offline
`pnpm run` try to download the project's already-installed pinned version.
Preserving it grants no additional filesystem or network access. On macOS, a
loopback grant also appends `-Djava.net.preferIPv6Addresses=true` to
`JAVA_TOOL_OPTIONS`: a dual-stack JVM binds `127.0.0.1` as the IPv4-mapped
IPv6 address `::ffff:127.0.0.1`, which the seatbelt's loopback filter does not
recognise, so Gradle and other JVM tools would otherwise fail their loopback
bind with `Operation not permitted` despite the grant. Preferring IPv6 makes
`localhost` resolve to `::1`, which also reaches child JVMs that Gradle starts
without the variable, such as its Checkstyle and PMD workers. A JVM that names
the literal `127.0.0.1` is still refused.

On Linux, a loopback grant runs the child in a private network namespace. The
namespace keeps the caller's own user id, so the JVM finds the real home
directory, and mounts the workspace's `.harn-tmp` over `/tmp`, so tools that
write a compiled-in `/tmp` path (the JVM attach socket that Mockito's inline
mock maker uses) write there instead of the host's `/tmp`. Concurrent runs in
one workspace share that directory. The mount would hide every host path under
`/tmp`, so it is skipped when the child is granted anything there, its
workspace or its program included. Such a child keeps the host's `/tmp` with
only its granted paths writable, and those tools fail; keep a JVM workspace
outside `/tmp`. The helper now performs a bind mount, so a host policy that
confines the helper must allow mounts as well as namespace creation; without
them every loopback spawn fails closed.

With the default `developer_toolchains` and `package_manager_config` presets,
the Unix desktop backends grant child processes read-only access to common
home-scoped toolchain and package-manager roots under the first absolute
`$HOME`. That includes user-managed runtimes such as `.local/share/uv`,
`.cargo`, `.rustup`, `.pyenv`, `.nvm`, `.volta`, and `go`, plus
package-manager config paths such as `.gitconfig`, `.yarnrc.yml`, `.pip`,
`.composer`, and `.cargo/config.toml`. XDG grants cover only the
[measured tool paths](./sandbox-config-census.md), and
[credential denials](./sandbox-read-deny-reference.md) still win inside them.
These grants are process-only: Harn file builtins still need `workspace_roots`
or `read_only_roots`. Config roots stay read-only; developer-toolchain caches
are writable when the policy permits child writes.

For Git, the `package_manager_config` preset also asks the host's Git to
resolve global and system config includes for each workspace. Included config
files and paths those configs name for hooks, excludes, attributes, and direct
credential-helper executables are readable by confined children. External
paths gain no write grant, and repository-local config cannot add process read
roots. Credential-helper data files are not inferred from helper arguments;
on macOS and Linux, the credential read denylist still takes precedence.

Other admitted child variables still come from the parent environment. That
includes corporate proxy and CA variables such as `HTTP_PROXY`,
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
const policy = {
  capabilities: {workspace: ["read_text"], process: ["exec"]},
  workspace_roots: [harness.fs.project_root()],
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

The `harn run` boundary also grants subprocesses read/execute access to the
exact Harn runtime executable for that invocation. This file-scoped grant lets
scripts delegate back to the verified runtime through wrappers such as `sh` or
`/usr/bin/time`, including when the binary came from a temporary CI artifact.
It does not expose the runtime's containing directory and does not grant Harn
filesystem builtins any additional authority.

Direct CLI runs expose the same process-only policy without requiring a
manifest edit:

```bash
harn run \
  --sandbox-read-root /opt/acme/sdk \
  --sandbox-write-root /opt/acme/cache \
  main.harn
```

Both flags are repeatable and incompatible with `--no-sandbox`.

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

### Roots that sit outside the working tree

Two kinds of directory are writable without appearing in the policy,
because a workload cannot function without them and neither is under
the workload's control:

- **Git's real dir and shared common dir for a linked worktree.**
  Git keeps both outside the working tree, and every git subprocess
  fails without read-write scope on them.
- **Harn's own runtime directories, once relocated.** `HARN_STATE_DIR`,
  `HARN_RUN_DIR`, and `HARN_WORKTREE_DIR` let an operator move Harn's
  state, run records, and managed worktrees anywhere. The runtime writes
  there from Rust either way; scoping `harness.fs.*` out of them would
  leave the state root readable and unwritable from Harn code, which is
  what pushes stdlib features into rebuilding a cwd-relative `.harn`
  and silently ignoring the override. Scripts should ask
  [`harness.fs.runtime_paths()`](builtins.md) for these paths rather than joining
  `.harn` onto a directory themselves.

Only directories genuinely outside the declared roots are added, and
only once they exist — in the default layout Harn's directories already
sit inside the workspace, so nothing widens. Runtime directories become
available to Harn code after the runtime materializes them. The overrides
are read from the process environment, which a sandboxed script cannot
write, so the grant follows the same principal that configured the
sandbox. Granting a relocated state root does not grant its parent or
its siblings.

## Swift package commands on macOS

SwiftPM normally starts its own sandbox for package manifests and plugins.
macOS refuses that nested sandbox inside a confined Harn process. Harn supplies
`--disable-sandbox` to SwiftPM while retaining the outer process sandbox, and
defaults manifest caches, configuration, and security state to the project.

Direct calls and workspace-local `swift` and `xcrun` launchers use the same
option definitions. Harn prepends the launchers to the child process's `PATH`
so ordinary project scripts receive those defaults too. SDK and toolchain
selection, explicit SwiftPM options, and arguments after `--` are preserved.
The launchers grant no additional file or network access.

On Xcode 27, Swift Build clears the environment of compiler and linker jobs.
For the default toolchain, Harn supplies a SwiftPM `--toolset` pointing to a
workspace-local launcher for that same installed compiler. The launcher restores
the private temporary-directory settings before executing the compiler; adjacent
frontend and library paths remain read-only links to the installed toolchain.
This keeps the default build backend. An explicit `--toolset` or `--toolchain`
takes precedence over this compiler launcher.

A script that replaces `PATH` or invokes an absolute Swift path bypasses the
launcher. This includes a path previously returned by `xcrun --find swift`;
lookup-only commands retain their original output. Such a script must supply
its own `--disable-sandbox` and workspace-local cache/configuration options.
An absolute Swift command issued directly through Harn's process interface
still receives the defaults. Unrestricted processes receive no launchers.

## Selecting a profile

### From a pipeline

A workflow author sets the profile on the `CapabilityPolicy` that
gates the agent loop or workflow (typically through the agent-session
or workflow-runtime constructors that accept a policy). The default
is `Worktree`, so pipelines that want the strongest confinement
include `sandbox_profile: "os_hardened"` in the policy literal:

```harn,ignore
const policy = {
  capabilities: {workspace: ["read_text"], process: ["exec"]},
  workspace_roots: [harness.fs.project_root()],
  sandbox_profile: "os_hardened",
}
```

### From one process capability call

A single subprocess can be promoted (or demoted) without rewriting the
surrounding policy by passing `sandbox_profile` on the
process request. The override is scoped to that call:

```harn,ignore
harness.process.run({
  program: "./untrusted-tool",
  args: ["input.json"],
  cwd: harness.fs.project_root(),
  sandbox_profile: "os_hardened",
})
```

`std/command.command_run` accepts the same per-call override in either the
command spec or options. Use `workspace_paths` for a trusted compiler, package
manager, or nested Harn invocation that must retain Harn's workspace-root path
checks without recursively installing an OS process sandbox:

```harn,ignore
import { command_run } from "std/command"

const checked = command_run(
  harness.tools,
  [harn_bin, "check", script],
  {
    cwd: harness.fs.project_root(),
    sandbox_profile: "workspace_paths",
  },
)
```

`workspace_paths` is not containment for untrusted child code. Use `worktree`
or `os_hardened` when the child itself needs OS confinement. The override
changes only the profile for that spawn; surrounding workspace roots and
capability ceilings remain in force, and the parent profile is restored when
the command returns.

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

### Brokering one trusted spawn

An embedder may need one specific trusted invocation to run outside the
confinement, for example a read-only CLI call that needs host credentials the
confined child cannot see. `harn_vm::process_sandbox::install_spawn_broker`
installs a process-wide `SpawnBroker`. It can be installed once; a second call
returns `SpawnBrokerAlreadyInstalled` and keeps the first broker.

```rust,ignore
use harn_vm::process_sandbox::{
    install_spawn_broker, SpawnBroker, SpawnBrokerGrant, SpawnBrokerRequest,
};

struct ReadOnlyStatus;

impl SpawnBroker for ReadOnlyStatus {
    fn admit(&self, request: &SpawnBrokerRequest<'_>) -> Option<SpawnBrokerGrant> {
        (request.program == "/usr/local/bin/status-cli"
            && request.args == ["status", "--json"])
            .then(|| SpawnBrokerGrant {
                broker_id: "status-cli".into(),
                cwd: request.cwd?.to_path_buf(),
                env: host_credentials_env(),
            })
    }
}

install_spawn_broker(std::sync::Arc::new(ReadOnlyStatus))?;
```

Every `harn-hostlib` process spawn goes through `spawn_process`. With a broker
installed it runs these steps in order:

1. The universal catastrophic-command floor runs. A refused argv never reaches
   the broker.
2. A shell interpreter (`sh`, `bash`, `zsh`, `cmd`, `pwsh`, and the other
   common shells) is never offered to the broker.
3. `admit` sees the requested program, literal arguments, cwd, and the caller's
   environment overrides. `None` leaves the spawn on its normal confined path.
4. On `Some(grant)` the exact argv runs with no confinement, in `grant.cwd`,
   with `grant.env` as the whole environment. Nothing is inherited, and the
   caller's `env` and `env_remove` are not applied. Harn adds only its private
   process-cleanup token so it can still reap the child's tree. Output capture,
   process groups, timeouts, cancellation, and owner-death cleanup behave as
   they do for any other spawn.

The command receipt reports `sandbox.enforced: false` and
`sandbox.broker: "<broker_id>"`. If the host process itself was confined with
`confine_current_process`, the brokered child still runs under that confinement
and the receipt says `enforced: true`. With no broker installed, spawning is
unchanged. On Windows no spawn is OS-confined to begin with, so a grant changes
only the child's cwd and environment.

## Confining a server process

Everything above confines a child when Harn spawns it. An agent server that
runs as its own process can also confine itself, so that a flaw in the VM, a
pipeline, or a tool is held by the kernel to the workspace instead of running
with the user's full authority.

- From the CLI, pass `--confine-workspace <path>` to `harn serve acp` (stdio
  only).
- From Rust, build the `AcpServerConfig`, then call
  `harn_serve::confine_acp_server_process(&config, &AcpServerConfinement {
  workspace_roots, state_roots })` before `run_acp_server`. The profile comes
  from `config.sandbox`, the same config the server's sessions use.
  `state_roots` names the embedder's own directories outside the workspace
  that the server writes. Harn adds read access to the served pipeline's
  package and the installed package cache. It returns `Ok(None)` without
  confining when the requested profile confines no process, such as
  `unrestricted`.
- The primitive underneath is
  `harn_vm::process_sandbox::confine_current_process(&policy)`, and
  `current_process_confinement()` reads the result back.

The profile is the one the backend renders for a child under the same
policy, so the server gets exactly what a confined command gets: its
workspace roots writable, system and toolchain roots readable, credential
directories and `read_deny_roots` denied. The ceiling uses the `act_auto`
autonomy tier, because a session can switch modes but the process profile
cannot change. Network stays allowed because the server makes model calls.
The host temp dir is shared with every other process you run, so it isn't
granted: the process gets a private `harn-confined-<pid>` directory under it,
and `TMPDIR` names that directory from then on.

Confinement is permanent and fails closed. An error means the process isn't
confined and must not serve. The ACP `initialize` response reports the state
as `agentCapabilities._meta.harn.processConfinement`: `{"state": "confined", "backend",
"mechanism", "workspaceRoots"}` or `{"state": "unconfined"}`. The
`process_sandbox_self` log event records the same facts. A command that the
confined process runs reports `"enforced": true` in its sandbox receipt even
when no per-turn policy confines it, and a permission error from it is
recorded as a `harn.process.sandbox_refusal.v1` denial.

What children get differs by platform:

- **macOS** can't stack Seatbelt profiles, so commands the confined server
  runs inherit its profile instead of getting a narrower one of their own.
  When a command's policy denies something the server's profile allows (for
  example, network), Harn refuses the command rather than run it with less
  confinement than it asked for. The `process_sandbox_nested` event records
  each case. Narrowing beyond those probed axes is enforced only by Harn's
  in-process checks; recovering per-command profiles needs a spawn broker
  outside the confined process (harn#9215).
- **Linux** confines with Landlock, whose domains stack, so a command's own
  ruleset still narrows it further. Landlock confines only the calling thread
  and threads it starts later, so Harn refuses to confine a process that
  already runs more than one thread. `harn serve acp --confine-workspace`
  confines on its main thread before it starts its runtime; an embedder must
  likewise confine before it starts any thread. Seccomp isn't applied to the
  server, because the child syscall allowlist isn't sized for a server
  runtime. Without that filter, nothing else closes the truncation gap below
  Landlock ABI 3, so self-confinement requires ABI 3 or newer and refuses on
  an older kernel.
- **Other platforms** refuse.

The profile allows every Mach service lookup, as the child profile does.
A process confined this way can still reach system services that an App
Sandbox profile would deny, so it isn't a drop-in equivalent of App Sandbox
on that axis.

Only a process dedicated to serving may confine itself. An in-process
(channel) ACP server shares its host's process, and confining it would
confine the host.

## Workspace-local Gradle cache (`GRADLE_USER_HOME`)

The child environment admits `GRADLE_USER_HOME` as a cache path. A caller's
populated Gradle cache is retained only when its resolved path is inside a
writable workspace root. Otherwise, confined commands use
`.harn-toolchain-cache/gradle`; an external cache path grants no new writes.
Secret-shaped names such as `GRADLE_TOKEN` still require an explicit grant.

## Workspace-local temp dir (`TMPDIR`/`TMP`/`TEMP`)

Every sandboxed child spawned under a restricted profile gets `TMPDIR`,
`TMP`, and `TEMP` pointed at a `.harn-tmp/` directory inside the first
writable `workspace_root` (created lazily, with a self-`.gitignore` so its
churn never leaks into a diff, PR, or eval grading). Compiler linkers
(`rustc`/`cc`/`ld`, Go, Swift, …) and many other toolchains write
intermediate object/temp files to `$TMPDIR`, defaulting to the system
`/tmp` when it is unset — which is outside the writable workspace roots, so
those writes are denied and a build that would otherwise succeed
FALSE-FAILS for an infrastructure reason (`could not write output to
/tmp/rustcXXXX/…: Permission denied`). Anchoring temp at an
already-writable workspace location fixes this for any TMPDIR-honoring
toolchain without widening the sandbox. A `TMPDIR`/`TMP`/`TEMP` the caller
sets explicitly (via the process call's `env`) is respected; only the
otherwise-inherited, non-writable value is overridden.

On macOS, Foundation asks Darwin for its temporary directory before consulting
`TMPDIR`. Harn supplies `DIRHELPER_USER_DIR_SUFFIX` with an alias to `.harn-tmp`,
so native temporary files use the same workspace storage. The alias is named
from the canonical workspace temp path, created exclusively, and reused only
when its stored target matches. Existing host files or different symlink targets
are refused. No alias subtree or shared `TemporaryItems` directory receives a
filesystem grant; the kernel still authorizes the resolved workspace path.

## Capability → kernel-knob mapping

The runtime translates the active capability ceiling into per-platform
knobs. The mapping is intentionally narrow — each capability maps to a
small, named kernel feature, never an open-ended escape hatch.

### Linux (`crates/harn-vm/src/stdlib/sandbox/linux.rs`)

Harn selects Landlock when the kernel offers any Landlock ABI and a child
restriction actually denies access to the host root. Otherwise it probes the
installed `/usr/bin/bwrap` or `/bin/bwrap` by creating private namespaces and
checking that a known host file is hidden.

Every Landlock ABI confines reads, writes, and execution beneath the granted
roots. Harn negotiates the newest ABI the kernel offers through the `landlock`
crate, handles every right that ABI defines, and covers or refuses each gap an
older ABI leaves:

- ABI 1 can't permit a rename or link into another directory, so it refuses
  all of them. That is narrower than the policy.
- Below ABI 3, Landlock can't scope truncation by path. The seccomp filter
  refuses `truncate(2)`, `openat2(2)`, and read-only opens with `O_TRUNC`, so
  an ungranted file can't be emptied.
- Below ABI 5, `ioctl` on a device opened under a grant isn't scoped.
- Below ABI 6, Landlock can't contain abstract Unix sockets, so only the
  serve-only Unix socket grant is refused. Without that grant the seccomp
  filter withholds `socket` below the `network` ceiling, so no abstract socket
  can be created or reached; at the ceiling the policy already grants
  `connect`.

The mechanism limit table above names each gap. These stock images, measured
as a non-root user with no extra packages and default kernel settings, select
Landlock:

| Image | Kernel | Landlock ABI | Mechanism limits |
|---|---|---|---|
| Ubuntu 24.04 LTS | 6.8 | 4 | `device_ioctl_unscoped`, `unix_socket_grant_unavailable` |
| Ubuntu 22.04 LTS | 5.15 | 1 | all four |
| Debian 12 | 6.1 | 2 | `truncation_filtered_by_syscall`, `device_ioctl_unscoped`, `unix_socket_grant_unavailable` |
| Rocky Linux 9 (RHEL 9) | 5.14 | 6 or newer | none |
| Any distribution on Linux 6.12 or newer | 6.12+ | 6 or newer | none |

Harn reports ABI 6 for any newer kernel, because later ABIs add only features
Harn doesn't use.

Both backends use the same opened filesystem grants and credential denylist.
Bubblewrap mounts the admitted file descriptors, so replacing a grant's path
after preparation doesn't change its source. The process guardian carries those
descriptors to the wrapper. Bubblewrap installs the existing seccomp filter
after constructing the mounts and namespaces. Harn's existing pre-runtime
helper checks each device mount against its pinned source, closes the setup
descriptors, and executes the payload. The CLI supplies its already-admitted
runtime path; embedders with device grants must supply
`process_sandbox.netns_launcher_path`. Grant sets without devices execute the
payload directly after bubblewrap installs confinement and need no helper.

Linux launches that need a trusted namespace helper or bubblewrap refuse
nonempty loader controls (`LD_*`, `GLIBC_TUNABLES`, and legacy `MALLOC_*`
aliases) after composing the session environment,
caller overrides, removals, and `env_clear`. Those controls can execute before
the helper installs confinement. Ordinary payload environment grants are
preserved; a direct Landlock-confined payload may still configure its loader.

The glibc allocator sizing knobs (`MALLOC_ARENA_MAX`, `MALLOC_ARENA_TEST`,
`MALLOC_MMAP_MAX_`, `MALLOC_MMAP_THRESHOLD_`, `MALLOC_TOP_PAD_`, and
`MALLOC_TRIM_THRESHOLD_`) are the exception under bubblewrap. Harn keeps them
out of bubblewrap's environment and re-applies them to the confined payload, so
a host that exports `MALLOC_ARENA_MAX`, as Heroku and many container images do,
still runs sandboxed commands. A `process_sandbox_environment` log event names
what was re-applied. `MALLOC_CHECK_` and `MALLOC_PERTURB_` change allocator
behavior rather than its sizing and stay refused, as does every control under
the namespace helper, which has no way to re-apply them.

Bubblewrap supports complete read-only and writable directory grants. It
refuses selective filesystem rights and managed proxy-only egress. A mount
can't grant procfs file reads without directory listing, so the default
files-only `/proc` grant mounts no procfs at all; a child that reads
`/proc/self` fails there. Granting process self-introspection with
`--sandbox-allow-process-self-introspection` mounts a fresh read-only procfs
from the sandbox's private PID namespace, which lists only the sandbox's own
processes. This applies only to hosts without usable Landlock, since every
image in the table above selects Landlock.
An unavailable namespace facility or a failed mount stops the launch before
the payload runs.

| Capability / policy | Kernel knob | Effect |
|---|---|---|
| `workspace.read_text` / `workspace.list` / `workspace.exists` | Landlock LSM `LANDLOCK_ACCESS_FS_READ_FILE` + `_READ_DIR` + `_EXECUTE` | reads under `workspace_roots` and the `system_read_roots()` allowlist (`/bin`, `/lib`, `/lib64`, `/usr`, `/etc`, `/nix/store`, `/System`); at the network ceiling, exact name-service files such as `/etc/resolv.conf` are also opened before confinement so symlink targets under `/run` remain readable without granting the mutable `/run` tree |
| `workspace.write_text` | Landlock `_WRITE_FILE` + `_REMOVE_*` + `_MAKE_*` + (ABI ≥ 2) `_REFER` + (ABI ≥ 3) `_TRUNCATE` | writes scoped to `workspace_roots`; below ABI 3 the seccomp filter refuses truncation by path instead |
| `workspace.delete` | Landlock `_REMOVE_DIR` + `_REMOVE_FILE` | removes scoped to `workspace_roots` |
| `read_only_roots: [...]` | Landlock `_READ_FILE` + `_READ_DIR` + `_EXECUTE` only | each read-only root is readable but never writable, regardless of the `workspace.*` capabilities |
| `process_sandbox.presets` includes `package_manager_config` | Landlock read-only rules for existing npm, pip, cargo, git, and CA config/cache roots under `$HOME` | package managers can resolve real per-user config without granting Harn file builtin access or write rights |
| `process_sandbox.allow_child_workspace_write` | the full write set above on `workspace_roots`, plus writable `process_sandbox.write_roots` and toolchain caches | a read-only role's children can build and test; `read_only_roots` keep read-only rules |
| `process_sandbox.read_roots` / `.write_roots` | Landlock read-only rules, plus writable rules only when child writes are allowed | process-only roots for SDKs/caches without widening Harn file builtins |
| standard process devices | Landlock grants read/write on `/dev/null` and read-only access on `/dev/zero`, `/dev/random`, and `/dev/urandom`; ABI ≥ 5 also handles `_IOCTL_DEV` but does not grant it to these device rules | language runtimes and test harnesses can open the devices they normally need without broad `/dev` access or device ioctl rights |
| `side_effect_level < network` | seccomp-bpf allowlist excludes addressable socket openers: `socket`, `connect`, `accept`, `accept4`, `bind`, `listen`. `socketpair`, `sendto`, `sendmsg`, `recvfrom`, and `recvmsg` stay allowlisted for inherited anonymous local IPC | addressable-socket / egress syscalls fail with `EPERM`, while local IPC keeps working |
| always | seccomp-bpf default-deny allowlist omits tier-1 dangerous syscalls including `bpf`, mount/module/kexec/sysctl families, `ptrace`, `process_vm_readv`/`process_vm_writev`, `io_uring_*`, `perf_event_open`, `userfaultfd`, `fanotify_init`, and `open_by_handle_at` | unknown and dangerous syscalls fail with `EPERM` |
| always | `prctl(PR_SET_NO_NEW_PRIVS, 1)` | no setuid escalation across `exec` |

The ruleset handles every right the negotiated ABI defines, so a right outside
a grant is denied. A host where Landlock can't confine a child selects
bubblewrap; if bubblewrap can't preserve the requested grants, the launch is
refused.

Bubblewrap availability uses a functional confinement probe with its own
one-second setup budget and the runtime's normal cancellation and process
cleanup. An inherited sandbox that prevents namespace setup, or a setup that
outlives the budget, produces a typed mechanism refusal; a descendant retaining
the probe's output cannot indefinitely delay command preparation. The budget is
not the caller's deadline: its expiry is never reported as `Deadline exceeded`,
while caller cancellation, the interrupt-handler window, and the scope deadline
keep their own errors. This does not grant additional syscalls or extend the
calling command's deadline.
Caller interrupts and an expired setup budget are not cached as host
unavailability; a later command can retry the functional probe.
Since a confined command can stack its own Landlock domain, the nested path
reaches Bubblewrap only when Landlock is not functional for that child.

### macOS (`crates/harn-vm/src/stdlib/sandbox/macos.rs`)

Before starting a confined command, Harn marks its host process with the sticky
`CS_RESTRICT` flag and reads the flag back. XNU then omits that host's launch
environment from ordinary readers' `KERN_PROCARGS` and `KERN_PROCARGS2` reads.
Seatbelt's `sysctl-read` and `process-info` rules do not block the numeric
argument-read path. Commands retain their own environment and ordinary process
execution. Harn refuses the spawn when this protection cannot be established,
including development kernels and systems that permit unrestricted DTrace.

Apple's privileged environment-read entitlement bypasses that redaction.
This protects the Harn host's environment from ordinary commands. It does not isolate commands from
every other same-user process or prove complete IPC isolation. Use a separately
isolated execution host for untrusted repair code that requires that guarantee.

| Capability / policy | `sandbox-exec` rule | Effect |
|---|---|---|
| always | `(deny default)` | every operation requires an explicit allow |
| always | `(allow process*)` + `(allow sysctl-read)` + `(allow mach-lookup)` + `(allow file-read-data (literal "/"))` | minimum surface required to exec a binary |
| standard process devices | `(allow file-read* ...)` for `/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, and `/dev/fd`; `(allow file-write* ...)` only for `/dev/null`, `/dev/stdout`, `/dev/stderr`, and `/dev/fd` | common stdio, entropy, and zero devices work without granting broad `/dev` writes |
| `process_sandbox.presets` | named read/write rules for `system_runtime`, `developer_toolchains`, `package_manager_config`, and `user_temp` | default process reach for system binaries, Xcode/Homebrew/toolchains, read-only package-manager home config, and per-user developer-tool caches without granting Harn file builtin access |
| `process_sandbox.allow_tcp_loopback` | bind/inbound on local `localhost:*`; outbound to remote `localhost:*` | IPv4 and IPv6 loopback servers and clients work without opening remote egress |
| `process_sandbox.unix_socket_roots` | `(allow network-bind (subpath "<root>"))`, `(allow network-inbound (subpath "<root>"))`, `(allow network-outbound (subpath "<root>"))` for each granted root, and for the session temp dir when the `user_temp` preset is on | build servers bind and connect Unix-domain sockets whose socket file lives under a granted root or the session temp dir; a daemon that binds under the shared temp dirs (sbt under `/tmp/bsbt`) needs that root named; `subpath` never matches an IP endpoint, so no egress opens |
| `workspace_roots: [...]` / `read_only_roots: [...]` | `(allow file-read* (subpath "<root>"))` | workspace and read-only roots are readable |
| `workspace.write_text` / `workspace.delete` (or empty `capabilities`, or `process_sandbox.allow_child_workspace_write`) | writable `user_temp`, `process_sandbox.write_roots`, and `workspace_roots`, followed by `(deny file-write* (subpath "<read_only_root>"))` | scratch dirs, explicit process-write roots, and writable `workspace_roots` are writable; each `read_only_roots` entry is then re-denied write. `sandbox-exec` is last-match-wins, so the trailing deny keeps a read-only root nested under a writable root unwritable even though the two lists are nominally disjoint |
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

### OpenBSD (`crates/harn-vm/src/stdlib/sandbox/openbsd.rs`)

| Capability / policy | OpenBSD mechanism | Effect |
|---|---|---|
| always | `unveil("/bin", "rx")`, `("/usr", "rx")`, `("/lib", "rx")`, `("/etc", "r")`, `("/dev", "rw")` | minimum surface required to exec |
| `workspace_roots: [...]` | `unveil("<root>", "rwcx" \| "rx")` | rwcx when `workspace.write_text` / `workspace.delete` present or `process_sandbox.allow_child_workspace_write` is set, otherwise rx |
| `read_only_roots: [...]` | `unveil("<root>", "rx")` | each read-only root is read+execute only, never write/create |
| `process_sandbox.read_roots` / `.write_roots` | `unveil("<root>", "rx" \| "rwcx")` | process-only roots, with writes gated by whether child processes may write |
| always | `pledge("stdio rpath proc exec", NULL)` | minimum process-exec promise set |
| `workspace.write_text` / `workspace.delete`, or `process_sandbox.allow_child_workspace_write` | adds `wpath cpath dpath` to pledge | filesystem mutation promises |
| `side_effect_level >= network` | adds `inet dns` to pledge | network promises |

### Windows and other platforms without a backend

Windows has no OS sandbox. A child process runs with the parent's full
filesystem and network access, and Harn says so instead of passing silently:

| Profile | On Windows |
|---|---|
| `os_hardened` | the spawn is refused before it starts, as a typed `does_not_confine` refusal naming mechanism `none` and every dimension the policy required |
| `worktree` | the child runs unconfined after a one-time `handler_sandbox` warning, `this platform has no OS process sandbox; child processes run unconfined`; `HARN_HANDLER_SANDBOX=enforce` refuses instead |
| `unrestricted`, `workspace_paths` | the child runs unconfined, as on every platform |

`harn doctor` reports `backend=unconfined filesystem_mechanism=none
active=false` and the `No OS sandbox` row of the table above. Harn's own path
checks, the process environment it builds, and the launch-cwd check still
apply. Any other platform without a backend behaves the same way.

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
        ├── linux::Backend       (Landlock + seccomp, or pinned bubblewrap mounts + seccomp)
        ├── macos::Backend       (wrap with sandbox-exec)
        ├── openbsd::Backend     (pre_exec → unveil + pledge)
        └── UnconfinedBackend    (Windows and others: no OS sandbox; warn, or refuse)
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

Harn builtins surface backend identity and confinement for `harn doctor`-style
scripts and conformance fixtures:

| Builtin | Returns | Use |
|---|---|---|
| `harness.system.sandbox_active_backend()` | `string` | name of the compiled-in backend (`linux`, `macos`, `openbsd`, `unconfined`) |
| `harness.system.sandbox_backend_available()` | `bool` | whether the backend is available; Linux reports `true` even when Landlock is unavailable |
| `harness.system.sandbox_active_profile()` | `string` | profile carried by the current execution policy (`worktree` under default `harn run`, `unrestricted` if no policy is active) |
| `harness.system.sandbox_confinement()` | closed record | whether this host can confine child processes, before any spawn |

The confinement record has schema `harn.process.sandbox_confinement.v1`.
Its `backend` names the compiled backend; `mechanism` names its filesystem
confinement mechanism. `confines_processes` reports whether that mechanism is
available on the running host. On Linux, the backend can be available while
both filesystem mechanisms are unavailable, so check `confines_processes`
before promising confinement.

When the mechanism is unavailable, `os_hardened_refusal` contains the exact
structured value an `os_hardened` spawn throws under the current execution
policy. On an unconfined platform, its `sandbox_mechanism.unconfined` list
names the dimensions that policy requires. Otherwise it is `nil`.
Both `harness.process.run` and `harness.tools.run_command` preserve this value
in a `catch` binding. This host status doesn't guarantee that every requested
policy dimension is enforceable; each spawn checks its own policy.

ACP clients read the same engine-host record from
`agentCapabilities._meta.harn.sandboxConfinement` in the initialize response.
This lets a client report a remote engine's confinement status before its
first command.

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
- Destination-level network egress allow/deny — use `harness.net.egress_policy(...)`
  or `HARN_EGRESS_*` once a host policy allows network side effects.
- Sandboxing for in-process work (LLM calls, deterministic Harn
  evaluation) in a process that hasn't confined itself. Capability ceilings
  and the approval policy are the enforcement layers there; the OS sandbox
  applies when Harn spawns a subprocess, or to the whole process after
  [Confining a server process](#confining-a-server-process).
