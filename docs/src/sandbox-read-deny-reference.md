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
| Default | the twelve home-relative paths below, resolved against `$HOME` |
| Scope | child processes, and Harn's own file builtins via `check_fs_path_scope` |
| Nesting | **unions**; every other field of the policy intersects |

Each entry is an absolute path or a `$HOME`-relative path. A path is denied when
it equals an entry or sits underneath one; there is no globbing and no pattern
syntax.

Two properties distinguish it from every other field:

- **It beats every grant.** The denial is checked before any preset, any
  `read_roots` entry, and any `workspace_roots` entry. `PackageManagerConfig`
  grants `~/.config`, `~/.cache`, and `~/.netrc` wholesale, so a term that
  merely competed with presets would never fire on the paths it exists for.
- **It unions as policies nest.** `CapabilityPolicy::intersect` narrows presets
  and roots to their common set. Narrowing a *denial* would widen authority, so
  a nested policy may add a denial and can never drop one.

## Default data file

`crates/harn-vm/src/orchestration/policy/read_deny_defaults.toml`, embedded with
`include_str!` and parsed once into `default_read_deny_home_paths()`. Parse
failure or an empty list panics at first use rather than silently producing an
empty denylist.

```toml
[defaults]
home_relative = [".ssh", ".aws", "..."]

[reason]
".ssh" = "private keys and known_hosts"
```

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `defaults.home_relative` | array of strings | yes, non-empty | paths denied under `$HOME` |
| `reason.<path>` | string | no | why the entry is on the list, for review |

Keys in `[reason]` that do not appear in `home_relative` are ignored, and an
entry with no reason is not an error. The table is documentation kept next to
the data instead of in a comment that drifts from it.

### Current defaults

`.ssh`, `.aws`, `.gnupg`, `.netrc`, `.docker/config.json`,
`.config/gh/hosts.yml`, `.config/gcloud`, `.kube/config`, `.npmrc`, `.pypirc`,
`.cargo/credentials`, `.cargo/credentials.toml`.

A host may add to this list through `read_deny_roots`. It cannot remove from it.

## Per-backend enforcement

| Backend | Mechanism | Applied |
| --- | --- | --- |
| macOS | `(deny file-read* (subpath …))` emitted after every allow | yes |
| Linux | Landlock grants the siblings that do not lead to the denial | yes |
| Windows | AppContainer | not yet |
| OpenBSD | `unveil` | not yet |

`sandbox-exec` is last-match-wins, so on macOS the deny rules' **position** in
the generated profile is the enforcement.

Linux has no deny rule. Landlock is allow-only, so `expand_around_denied`
expresses a denial by not granting: it walks from the granted root toward each
denied path and substitutes, at every level, the sibling entries that do not
lead to the denial.

- An unlistable ancestor **refuses the spawn** rather than granting the root.
- A denial under a directory that does not exist costs nothing and ends the
  walk.
- Expansion is capped at `MAX_DENY_EXPANSION_RULES` (4096). Measured cost for
  the twelve defaults on a real home directory is 187 to 190 rules in 10 to 45
  ms; `report_default_denylist_expansion_cost` prints the number for the host
  you are on.

## Refusal event

A refused child emits `harn.process.sandbox_refusal.v1` through
`log_warn_meta("process_sandbox_refusal", …)`.

| Field | Type | Meaning |
| --- | --- | --- |
| `schema` | string | `harn.process.sandbox_refusal.v1` |
| `command` | array of strings | argv of the refused child |
| `cwd` | string | working directory of the spawn |
| `refused_paths` | array of strings | paths implicated, when they can be determined |
| `observability` | string | how the refusal was detected; currently only `inferred` |
| `stderr_excerpt` | string | first 512 bytes of the child's stderr |
| `count` | integer | refusals coalesced into this record |

`observability: "inferred"` is the honest label for what the OS gives us today:
Landlock and seatbelt refuse in-kernel without notifying the supervisor, so the
refusal is reconstructed from the child's exit and stderr rather than observed
directly. The enum is `#[non_exhaustive]`; a backend that can report a refusal
directly will add a variant rather than change the meaning of this one.
