# Credential denylist reference

Reference for `read_deny_roots`, the one subtractive term in
`ProcessSandboxPolicy`, and for the data file that seeds its defaults.

For how the process sandbox works overall, see
[Process sandboxing](./sandboxing.md).

## `read_deny_roots`

| | |
| --- | --- |
| Type | `Vec<String>` |
| JSON field | `process_sandbox.read_deny_roots` |
| Default | the home-relative paths below, resolved against `$HOME` |
| Scope | child processes, and Harn's own file builtins via `check_fs_path_scope` |
| Nesting | **unions**; every other field of the policy intersects |

Each entry is an absolute path or a `$HOME`-relative path. A path is denied when
it equals an entry or sits underneath one; there is no globbing and no pattern
syntax.

Two properties distinguish it from every other field:

- **It beats every grant.** The denial is checked before any preset, any
  `read_roots` entry, and any `workspace_roots` entry. `PackageManagerConfig`
  grants tool roots such as `~/.config/composer`, whose `auth.json` remains
  denied. Explicit host grants may also cover credential directories.
- **It unions as policies nest.** `CapabilityPolicy::intersect` narrows presets
  and roots to their common set. Narrowing a *denial* would widen authority, so
  a nested policy may add a denial and can never drop one.

## Default data file

The defaults are the `[credential]` class of the path registry,
`crates/harn-vm/src/known_paths/known_paths.toml`, embedded with `include_str!`
and parsed once. The same entries drive the taint gate's secret-read check, the
command risk scan's `credential_file_read` label, and the default approval
`sensitive_path_patterns`, so a path added once is recognized by all four; a
parity test fails if any consumer misses an entry. Parse failure, an empty
list, a credential without a reason, or a preset grant that names a credential
panics at first use rather than silently producing an empty denylist.

```toml
[credential]
home = [".ssh", ".aws", "..."]
basename_globs = [".env", "*.pem", "..."]

[credential.reason]
".ssh" = "private keys and known_hosts"
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `credential.home` | array of strings | yes, non-empty | paths denied under `$HOME` |
| `credential.basename_globs` | array of strings | yes, non-empty | credential file names matched anywhere by the path matchers; not sandbox-denied |
| `credential.reason.<path>` | string | yes, one per `home` entry | why the entry is on the list |

The registry file lists the current entries, each with its reason.

The list names the default credential locations of widely used CLIs. It is not
exhaustive: credentials elsewhere inside an admitted tool root need another
denial. Unknown `~/.config` and `~/.cache` siblings aren't admitted by the
preset. See [Toolchain config and cache reference](./sandbox-config-census.md).

A denied config file is not left for the tool to trip over. npm and pnpm read
`~/.npmrc` at startup and exit on the `EPERM`, so a confined child instead gets
`NPM_CONFIG_USERCONFIG` pointing at a stand-in under the workspace toolchain
cache: a copy of the user's `.npmrc` with every credential line (`_authToken`,
`_auth`, `_password`, and any other `//registry/:_key` entry) removed, so a
private registry URL still resolves and no token is ever copied into the
workspace.

A host may add to this list through `read_deny_roots`. It cannot remove from it.

## Per-backend enforcement

| Backend | Mechanism | Applied |
| --- | --- | --- |
| macOS | `(deny file-read* (subpath …))` and unlink denials on the path to it, emitted after every allow | yes |
| Linux | Landlock grants the siblings that do not lead to the denial | yes |
| Windows | none: Windows runs with no OS sandbox confinement | no |
| OpenBSD | `unveil` | not yet |

Only an `os_hardened` spawn refuses on Windows, as it does for every
dimension the enforcement table marks not enforced there. Other profiles on
Windows and OpenBSD do not refuse the spawn. The default denylist is
never empty, so refusing on an unsupporting backend would refuse every spawn on
that platform. The term is simply unenforced there, and saying so is worth more
than a claim the code does not honor.

`sandbox-exec` is last-match-wins, so on macOS the deny rules' **position** in
the generated profile is the enforcement.

Linux has no deny rule. Landlock is allow-only, so `expand_around_denied`
expresses a denial by not granting: it walks from a read-only granted root
toward each denied path and substitutes, at every level, the sibling entries
that do not lead to the denial.

- An ancestor that cannot be enumerated **ends the walk**, granting nothing
  beneath it and never the ancestor itself. That covers both a missing
  directory (nothing below it to subtract) and an unreadable one (its children
  cannot be granted, so granting none of them is exactly right). Ending early
  is strictly narrower than continuing, so it is the safe direction on an
  allow-only backend; refusing the spawn instead bought no authority and took
  down every run whose `$HOME` the runtime could not list.
- Any **other** enumeration error still refuses the spawn, so the relaxation
  cannot widen into "any error means nothing to exclude".
- Expansion is capped at `MAX_DENY_EXPANSION_RULES` (4096). Measured cost for
  the twelve original defaults on a real home directory was 187 to 190 rules in 10 to 45
  ms; `report_default_denylist_expansion_cost` prints the number for the host
  you are on.

### A denial inside a writable root

A read denial is path-based, so inside a root the child may write it holds only
while nothing on the path to it can be renamed.

On macOS, the profile also denies `file-write-unlink`, which covers rename, on
the denied subtree and on each directory above it. The denied file, its
directory, and the directories above cannot be renamed or deleted; every other
entry in those directories stays writable. A child that tries to carry the file
out of the denial gets `Operation not permitted`. When Harn runs inside an outer
sandbox it would inherit, that sandbox must deny the same unlinks, or the spawn
is refused. A denied path under a writable root that does not exist yet also
refuses inheritance: macOS reports every absent path as denied, so the outer
sandbox cannot be asked whether it would hold a file created there later.

On Linux, a writable root that contains a denial is granted whole rather than
subtracted. Landlock could hold the denial only by withholding every directory
on the path to it, and the child would then be unable to create a file
anywhere on that path. A writable root stays writable, so the denial inside it
is not enforced, and Harn says so.

| Profile | Result |
| --- | --- |
| `os_hardened` | the spawn is refused; the error names each denied path and the writable root that contains it |
| any other | the child runs with the writable root intact, and a `handler_sandbox` warning names each denied path and its writable root |

A writable root that sits inside a denial is still not granted. To keep a
credential denial enforced, put it under a read-only root, or outside every
writable root.

## Refusal event

A refused child emits `harn.process.sandbox_refusal.v1` through
`log_warn_meta("process_sandbox_refusal", …)`.

| Field | Type | Meaning |
| --- | --- | --- |
| `schema` | string | `harn.process.sandbox_refusal.v1` |
| `command` | array of strings | argv of the refused child |
| `cwd` | string | working directory of the spawn |
| `backend` | string | active process-sandbox backend |
| `operation` | `read`, `write`, or `unknown` | refused operation class when reported by the backend |
| `mechanism` | `egress`, `local_socket`, `home_read`, `write`, or `unknown` | which sandbox boundary refused the child, inferred from its output |
| `reason` | string | the mechanism spelled out with the grants in force (loopback, socket roots, a denylisted path) |
| `resource` | string or null | refused resource when reported by the backend |
| `refused_paths` | array of strings | paths implicated, when they can be determined |
| `observability` | string | how the refusal was detected; currently only `inferred` |
| `stderr_excerpt` | string | bounded child-output evidence; stderr when it carried the refusal, otherwise stdout |
| `count` | integer | refusals coalesced into this record |

`observability: "inferred"` is the honest label for what the OS gives us today:
Landlock and seatbelt refuse in-kernel without notifying the supervisor, so the
refusal is reconstructed from the child's exit and stderr rather than observed
directly. The enum is `#[non_exhaustive]`; a backend that can report a refusal
directly will add a variant rather than change the meaning of this one.

`tools.run_command` returns the same record in `denial` using the common
agent-handler denial shape (`gate: "process_sandbox"`, `retryable: false`). Its
`sandbox.denial_reporting` field is always present. `inferred_only` means a
null `denial` is not evidence that every child operation was allowed;
`backend_unavailable` and `not_enforced` name the other two coverage states.
Current Landlock and seatbelt integrations cannot report a
resource or operation directly, so inferred records use `resource: null` and
`operation: "unknown"` rather than parsing tool- or locale-specific prose.
