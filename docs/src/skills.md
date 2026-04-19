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
- the **SKILL.md frontmatter** Harn recognizes, including the required
  compact `short:` card,
- the **body substitution** (`$ARGUMENTS`, `$N`, `${HARN_SKILL_DIR}`,
  `${HARN_SESSION_ID}`) that runs over SKILL.md before the model sees
  it,
- the `harn.toml` `[skills]` / `[[skill.source]]` tables, and
- the `harn doctor` output for diagnosing collisions / missing
  entries.

The companion language form — `skill NAME { ... }` — is documented in
[Language basics](./language-basics.md) and the skill builtins
(`skill_registry`, `skill_define`, `skill_find`, `skill_list`,
`skill_render`, `load_skill`, `skills_catalog_entries`,
`render_always_on_catalog`, …) in [Builtin functions](./builtins.md).

## Layered discovery

When `harn run` / `harn test` / `harn check` starts, every discovered
skill is merged into a single registry and exposed as the pre-populated
VM global `skills`. That startup registry is intentionally compact: it
keeps the frontmatter card and activation metadata, but leaves the full
Markdown body behind the lazy `load_skill(...)` path. The layers — in
order of highest to lowest priority — are:

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
short: Deploy the application when the user asks for a release
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
| `name` | string | Optional in frontmatter when the directory name already provides it; Harn falls back to the enclosing skill directory basename. |
| `short` | string | Required. One-sentence compact card describing what the skill does and when to use it. Always loaded into the startup registry and catalogs. |
| `description` | string | Optional longer summary. Useful for richer CLI output or custom matchers, but not required for lazy discovery. |
| `when-to-use` | string | Longer activation trigger. |
| `disable-model-invocation` | bool | If `true`, never auto-activate — explicit use only. |
| `allowed-tools` | list of string | Restrict tool surface while the skill is active. Entries accept three shapes: an exact tool name (`"deploy_service"`), a namespace tag (`"namespace:read"` — matches every tool declared with `namespace: "read"`), or `"*"` (escape hatch that keeps the full surface, useful for skills that only carry prompt context). |
| `user-invocable` | bool | Expose the skill to end users via a slash menu. |
| `paths` | list of glob | Files the skill expects to touch. |
| `context` | string | `"fork"` runs in an isolated subcontext. |
| `agent` | string | Sub-agent that owns the skill. |
| `hooks` | map or list | Shell commands for lifecycle events. |
| `model` | string | Preferred model alias. |
| `effort` | string | `low` / `medium` / `high`. |
| `shell` | string | Shell to run the body under when `context` is shell-ish. |
| `argument-hint` | string | UI hint for `$ARGUMENTS`. |

## Tool scoping with `namespace:<tag>`

Tool declarations that carry a `namespace:` field can be grouped into
one `allowed-tools` entry instead of enumerating names. Given

```harn,ignore
tool_define(reg, "read_file", "...", {namespace: "read", ...})
tool_define(reg, "list_files", "...", {namespace: "read", ...})
tool_define(reg, "write_file", "...", {namespace: "write", ...})
```

a skill with `allowed-tools: ["namespace:read"]` scopes the turn to
`read_file` + `list_files` and hides `write_file`. Exact tool names
and the wildcard `"*"` remain valid and can mix freely:

```yaml
allowed-tools: ["namespace:read", "grep", "*"]
```

Malformed entries fail loudly at `skill_define` time — a bare `":"`
without a tag or a colon-prefixed entry that isn't `namespace:` raises
so authors don't silently scope to an empty set.

## Body substitution

When a skill body is rendered (via `skill_render`, `load_skill`, or by a
host before handing the body to the model), the following substitutions
run over the Markdown body:

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

## Progressive disclosure with `load_skill`

Harn supports lazy skill loading in two places:

- `load_skill("deploy")` is a stdlib builtin for Harn code. It resolves
  the requested skill against the startup registry, lazily hydrates the
  full `SKILL.md`, applies substitution, and returns the rendered body
  as a string.
- When an agent loop receives a skill registry through `skills:`, Harn
  also exposes a runtime-owned `load_skill({ name })` tool for the
  model. That tool calls the same lazy loader and returns the rendered
  body in the next turn.

Both paths resolve the requested skill id against the active registry
and apply the same substitution rules described above.

Builtin example:

```harn
let runbook = load_skill("deploy")
assert(contains(runbook, "Deploy runbook"), "full body is fetched lazily")
```

If the target skill has `disable-model-invocation: true`, the runtime
tool returns a typed error instead of leaking the body. Direct Harn-code
`load_skill("name")` calls are explicit and are not blocked by that
flag.

### Always-on catalog helper

The recommended harness convention is:

1. Keep a compact catalog of available skills in the system prompt.
2. Let the model call the runtime `load_skill({ name })` tool only when
   one of those entries looks relevant.

Harn ships two pure helpers for that pattern:

```harn
let entries = skills_catalog_entries(skills)
let catalog = render_always_on_catalog(entries, 2000)
```

`skills_catalog_entries` projects the resolved registry into compact
`{name, description, when_to_use}` cards, with `description` sourced
from the required `short:` frontmatter (sorted deterministically by
skill id, using `<namespace>/<name>` when present).
`render_always_on_catalog` formats those cards into a stable prompt
block and trims the list to the requested character budget.

Copy-pasteable example:

```harn
let catalog = render_always_on_catalog(skills_catalog_entries(skills), 2000)

let result = agent_loop(
  "Help me ship this release",
  catalog,
  {
    provider: "mock",
    model: "gpt-5.4",
    persistent: true,
    skills: skills,
  },
)
```

On a later turn the model can emit:

```text
load_skill({ name: "deploy" })
```

and the next turn will see the substituted SKILL.md body in the tool
result, while any `allowed-tools` declared by that skill narrow the
tool surface for subsequent turns.

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
url = "https://skills.harnlang.com"
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

## Managing skills

The `harn skills` CLI manages and inspects skills without running a
pipeline. Each subcommand resolves the layered catalog the same way
`harn run` does (`--skill-dir`, `HARN_SKILLS_PATH`, project, manifest,
user, packages, system, host), so what you see here is exactly what
pipelines see.

### `harn skills list`

Prints every resolved skill with the layer it came from. Pass
`--all` to include shadowed entries; pass `--json` for machine output.

```text
$ harn skills list
Resolved skills (3):
  deploy         [cli]       Deploy to production when release work is requested
  review         [project]   Review a pull request when asked for code review help
  helpers/utils  [package]   Shared helpers when the task needs the acme/ops package

Shadowed skills (1):
  deploy   winner=[cli] hidden=[user] origin=/home/me/.harn/skills/deploy
```

### `harn skills inspect <name>`

Dumps the resolved SKILL.md — frontmatter, bundled files under the
skill directory, and the full body — for a specific skill. Accepts
bare `<name>` or fully-qualified `<namespace>/<name>`:

```text
$ harn skills inspect deploy
id:          deploy
name:        deploy
layer:       cli
short:       Deploy to production when release work is requested
description: Deploy to production with rollback support
skill_dir:   /repo/.harn/skills/deploy

Bundled files:
  files/runbook.md
  files/rollback.sh

---- SKILL.md body ----
Run the deploy. Confirm replicas and then flip traffic.
```

### `harn skills match "<query>"`

Runs the built-in metadata matcher (same scorer the agent loop uses)
against a prompt and prints the ranked candidates with their scores.
Supports `--working-file` to simulate path-glob matches:

```text
$ harn skills match "deploy the staging service" --top-n 3
Match results for: deploy the staging service
   1. deploy              score=2.400  [cli]       prompt mentions 'deploy'; 1 keyword hit(s)
   2. review              score=0.400  [project]   1 keyword hit(s)
```

Useful when authoring a SKILL.md to confirm its `short:` and
`when_to_use:` frontmatter actually attracts the right prompts.

### `harn skills install <spec>`

Materializes a git ref or local path into `.harn/skills-cache/` so
the filesystem package walker picks it up on the next run. The
`.harn/skills-cache/` layout mirrors `.harn/packages/`:

```text
$ harn skills install acme/harn-skills --tag v1.2.0
installing acme/harn-skills to .harn/skills-cache/harn-skills
installed — layer=package, path=.harn/skills-cache/harn-skills
```

`<spec>` accepts:

- A full git URL: `https://github.com/acme/harn-skills.git`
- `owner/repo` shorthand (expands to GitHub): `acme/harn-skills`
- A local filesystem path: `../shared/skills/deploy`

Pass `--namespace <ns>` to shelf the install under a subdirectory so
it shows up in the resolver as `<ns>/<skill>`. Pass `--tag <ref>` to
pin a git branch or tag. Every install rewrites
`.harn/skills-cache/skills.lock` with the resolved source + commit.

### `harn skills new <name>`

Scaffolds a new SKILL.md and `files/` directory under `.harn/skills/`:

```text
$ harn skills new deploy --description "Deploy to production"
Scaffolded skill 'deploy' at .harn/skills/deploy
  SKILL.md
  files/README.md

Edit the SKILL.md frontmatter and body, then run `harn skills list`
to verify the compact card is picked up.
```

Pass `--dir <path>` to target a different destination (for example
`~/.harn/skills/deploy` to scaffold under the user layer instead of
the project layer), or `--force` to overwrite an existing directory.

## Portal observability

The Harn portal (`harn portal`) surfaces two skill-focused panels on
every run detail page:

- **Skill timeline** — horizontal bars showing which skills activated
  on which agent-loop iteration and when they deactivated. Hover a
  bar for the matcher score and the reason the skill was promoted.
- **Tool-load waterfall** — one row per `tool_search_query` event,
  pairing each query with its `tool_search_result` so you can see
  which deferred tools entered the LLM's context in each turn.
- **Matcher decisions** — per-iteration expansions showing every
  candidate the matcher considered, its score, and the working-file
  snapshot it scored against.

The runs index page takes a `skill=<name>` filter so you can narrow
evals to runs where a specific skill was active. The same
`skill=<name>` query parameter works from a URL, making it easy to
link to "every run that used `deploy`".
