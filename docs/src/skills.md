# Skills

Harn discovers **skills** — bundled instructions, tool lists, and
activation rules — from the filesystem and from the host process. Every
skill is a directory containing a `SKILL.md` file with YAML
frontmatter plus a Markdown body; the format matches Anthropic's
[Agent Skills](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)
and [Claude Code](https://code.claude.com/docs/en/skills) specs, so
skills you author once work across both environments.

This page describes:

- the **layered discovery** hierarchy (CLI > env > project > manifest >
  user > package > system > host),
- the **SKILL.md frontmatter** Harn recognizes,
- the **body substitution** (`$ARGUMENTS`, `$N`, `${HARN_SKILL_DIR}`,
  `${HARN_SESSION_ID}`) that runs over SKILL.md before the model sees
  it,
- the `harn.toml` `[skills]` / `[[skill.source]]` tables, and
- the `harn doctor` output for diagnosing collisions / missing
  entries.

The companion language form — `skill NAME { ... }` — is documented in
[Language basics](./language-basics.md) and the skill builtins
(`skill_registry`, `skill_define`, `skill_find`, `skill_list`,
`skill_render`, …) in [Builtin functions](./builtins.md).

## Layered discovery

When `harn run` / `harn test` / `harn check` starts, every discovered
skill is merged into a single registry and exposed as the pre-populated
VM global `skills`. The layers — in order of highest to lowest
priority — are:

| # | Layer | Source | When |
|---|---|---|---|
| 1 | CLI | `--skill-dir <path>` (repeatable) | Ephemeral overrides, CI pinning |
| 2 | Env | `$HARN_SKILLS_PATH` (colon-separated on Unix, `;` on Windows) | Deployment config, Docker, cloud agents |
| 3 | Project | `.harn/skills/<name>/SKILL.md` walking up from the script | Default for repo-scoped skills |
| 4 | Manifest | `[skills] paths` + `[[skill.source]]` in harn.toml | Multi-root, shared across siblings |
| 5 | User | `~/.harn/skills/<name>/SKILL.md` | Personal skills across projects |
| 6 | Package | `.harn/packages/**/skills/<name>/SKILL.md` | Skills shipped via `[dependencies]` |
| 7 | System | `/etc/harn/skills/` + `$XDG_CONFIG_HOME/harn/skills/` | Managed / enterprise |
| 8 | Host | Registered via the bridge at runtime | Cloud / embedded hosts |

**Name collisions:** when two layers both expose a skill named `deploy`,
the higher layer wins. The shadowed entry is recorded so `harn doctor`
can surface it. Scripts that need both at once can register a
fully-qualified `<namespace>/<skill>` id via `[[skill.source]]` in the
manifest (see below).

## SKILL.md frontmatter

The frontmatter is YAML, delimited by `---` on its own line above and
below. Unknown fields are **not** hard errors — `harn doctor` reports
them as warnings so newer spec fields roll out cleanly.

```markdown
---
name: deploy
description: Deploy the application to production
when-to-use: User says deploy / ship / release
disable-model-invocation: false
user-invocable: true
allowed-tools: [bash, git]
paths:
  - infra/**
  - Dockerfile
context: fork
agent: ops-lead
model: claude-opus-4-7
effort: high
shell: bash
argument-hint: "<target-env>"
hooks:
  on-activate: echo "starting deploy"
  on-deactivate: echo "deploy ended"
---
# Deploy runbook
Ship it: `$ARGUMENTS`. Skill directory: `${HARN_SKILL_DIR}`.
```

**Recognized fields** (Harn normalizes hyphens to underscores, so
`when-to-use` and `when_to_use` are the same key):

| Field | Type | Purpose |
|---|---|---|
| `name` | string | Required. Id the script looks up via `skill_find`. |
| `description` | string | One-liner the model sees for auto-activation. |
| `when-to-use` | string | Longer activation trigger. |
| `disable-model-invocation` | bool | If `true`, never auto-activate — explicit use only. |
| `allowed-tools` | list of string | Restrict tool surface while the skill is active. |
| `user-invocable` | bool | Expose the skill to end users via a slash menu. |
| `paths` | list of glob | Files the skill expects to touch. |
| `context` | string | `"fork"` runs in an isolated subcontext. |
| `agent` | string | Sub-agent that owns the skill. |
| `hooks` | map or list | Shell commands for lifecycle events. |
| `model` | string | Preferred model alias. |
| `effort` | string | `low` / `medium` / `high`. |
| `shell` | string | Shell to run the body under when `context` is shell-ish. |
| `argument-hint` | string | UI hint for `$ARGUMENTS`. |

## Body substitution

When a skill is rendered (via the `skill_render` builtin, or by a host
before handing the body to the model), the following substitutions run
over the Markdown body:

- `$ARGUMENTS` → all positional args joined with spaces
- `$N` → the N-th positional arg (1-based). `$0` is reserved.
- `${HARN_SKILL_DIR}` → absolute path to the skill directory
- `${HARN_SESSION_ID}` → opaque session id threaded through the run
- `${OTHER_NAME}` → looks up `OTHER_NAME` in the process environment
- `$$` → literal `$`

Missing positional args (`$3` when only `$1` was supplied) **pass
through unchanged** so authors see what wasn't supplied rather than a
silent empty substitution.

```harn
let deploy = skill_find(skills, "deploy")
let rendered = skill_render(deploy, ["prod", "us-east-1"])
// rendered now has $1 and $2 replaced with "prod" and "us-east-1".
```

## harn.toml `[skills]` + `[[skill.source]]`

Projects that share skills across siblings or pull them from a remote
tag use the manifest instead of a per-script flag:

```toml
[skills]
paths = ["packages/*/skills", "../shared-skills"]
lookup_order = ["cli", "project", "manifest", "user", "package", "system", "host"]
disable = ["system"]

[skills.defaults]
tool_search = "bm25"
always_loaded = ["look", "edit", "bash"]

[[skill.source]]
type = "fs"
path = "../shared"

[[skill.source]]
type = "git"
url = "https://github.com/acme/harn-skills"
tag = "v1.2.0"

[[skill.source]]
type = "registry"   # reserved, inert until a marketplace exists
url = "https://skills.harn.burincode.com"
name = "acme/ops"
```

- `paths` is joined against the directory holding harn.toml and
  supports a single trailing `*` component (`packages/*/skills`).
- `lookup_order` lets you invert a layer's priority — for example, to
  prefer `user` over `project` on a personal checkout without touching
  the repo.
- `disable` kicks entire layers out of discovery. Disabled layers are
  reported by `harn doctor`.
- `[[skill.source]]` entries of type `git` expect their materialized
  checkout to live under `.harn/packages/<name>/skills/` — run
  `harn install` to populate it.
- `registry` entries are accepted but inert until a Harn Skills
  marketplace exists (tracked by [#73](https://github.com/burin-labs/harn/issues/73)).

## harn doctor

`harn doctor` reports the resolved skill catalog:

```text
  OK   skills                 3 loaded (1 cli, 1 project, 1 user)
  WARN skill:deploy           shadowed by cli layer; user version at /home/me/.harn/skills/deploy is hidden
  WARN skill:review           unknown frontmatter field(s) forwarded as metadata: future_field
  SKIP skills-layer:system    layer disabled by harn.toml [skills.disable]
```

## CLI flags

- `harn run --skill-dir <path>` (repeatable) — highest-priority layer.
- `harn test --skill-dir <path>` — same semantics for user tests and
  conformance fixtures.
- `$HARN_SKILLS_PATH` — colon-separated list of directories, applied
  to every invocation.

## Bridge protocol

Hosts expose their own managed skill store through three RPCs:

- `skills/list` (request) — response is an array of
  `{ id, name, description, source }` entries.
- `skills/fetch` (request) — payload `{ id: "<skill id>" }`; response
  is the full manifest + body shape so the CLI can hydrate a
  `SkillManifestRef` into a `Skill`.
- `skills/update` (notification, host → VM) — invalidates the VM's
  cached catalog. The CLI re-runs discovery on the next boundary.

See [Bridge protocol](./bridge-protocol.md) for wire-format details.
