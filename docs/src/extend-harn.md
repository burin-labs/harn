# Extend Harn

Five things in Harn are meant to be extended by you. They are documented
separately because they are genuinely different mechanisms; this page is the
map.

| You want to | Reach for | Ships as |
| --- | --- | --- |
| Give an agent a new action | [a custom tool](#custom-tools) | a package that exports one tool |
| Share Harn code across projects | [a package](#packages) | a versioned `harn.toml` package |
| Reach an outside service | [a connector](#connectors) | a package with connector metadata |
| Teach an agent a procedure | [a skill](#skills) | a signed `SKILL.md` |
| Add a `harn` subcommand | [a CLI extension](#cli-subcommands) | a `.harn` script the CLI dispatches |

## Everything you ship can be signed

Before the five, the thing that makes shipping any of them safe.

`harn pack` walks an entrypoint's transitive imports, links one closed program,
snapshots the provider catalog and stdlib pin, generates an SBOM, and writes a
deterministic `.harnpack` container. With `--sign` it embeds an Ed25519
signature over the bundle hash:

```bash
harn skill key generate --out signing-key.pem
harn pack agent.harn --sign --key signing-key.pem --exclude-secrets
harn pack verify agent.harnpack
```

```text
wrote agent.harnpack (3879 bytes, bundle_hash blake3:e045e81a…)
ok agent.harnpack (bundle_hash blake3:e045e81a…, signature_verified=true)
```

`verify` recomputes the canonical bundle hash, checks the signature, and
cross-checks every archive entry's BLAKE3 against the manifest. Change one byte
anywhere in the bundle and it exits non-zero with a hash mismatch rather than
quietly accepting it.

`--exclude-secrets` refuses to bundle paths that look like credentials
(`.env`, `*.pem`, `*.key`, `credentials*`, anything under `secrets/`) and
reports each skipped asset in the manifest. It is off by default, so pass it
from any pipeline that shares bundles outside your own machine.

Skills carry their own signatures rather than relying on the bundle's:
`harn skill sign` writes a detached `SKILL.md.sig`, and a skill declaring
`require_signature = true` is omitted from the startup registry unless its
signature chain verifies. See [Skill provenance](./skill-provenance.md).

## Custom tools

A tool is one action an agent can call. `harn tool new` scaffolds a package
that exports exactly one:

```bash
harn tool new summarize-diff --description "Summarize a git diff"
```

Use this when the capability is a single verb with typed parameters. For a tool
defined inline in the program that uses it, the `tool` declaration in
[Builtin functions](./builtins.md) is simpler and needs no package.

- [Tool surface validation](./tool-surface-validation.md) — the checks Harn
  runs over a tool registry before a loop spends tokens on it.
- [Preset tool hooks](./tool-hooks.md) and
  [Hooks](./extensibility/hooks.md) — intercepting calls rather than adding one.
- [Long-running tools](./long-running-tools.md) — starting slow work and
  collecting the result on a later turn.

## Packages

A package is versioned Harn code with a `harn.toml` manifest: exported
functions, custom tools, `[llm]` adapters, and dependencies. It is the unit
that `harn add`, `harn install`, and the package index move around.

- [Package authoring](./package-authoring.md) — manifest shape, exports, and
  the publishing path.

## Connectors

A connector is a package that also declares how to reach an outside service:
its auth type, required scopes, required secrets, and the operations it
exposes. That declaration is what lets a host show a "Connect" button and
report a connector as installed but not yet usable, instead of failing at call
time.

- [Connector authoring](./connectors/authoring.md) — package layout and setup
  metadata.
- [Connector testkit](./connectors/testkit.md) — exercising one without live
  credentials.
- [Connector OAuth](./orchestrator/oauth.md) — provider-specific flows.
- [Connector parity matrix](./connectors/parity-matrix.md) — what is wired
  today.

## Skills

A skill is procedural knowledge: a `SKILL.md` with frontmatter describing when
it applies, which tools it may use, and what paths it concerns. Harn discovers
skills across layers (project, user, package, system) and selects them per
task.

- [Skills](./skills.md) — discovery, layers, `harn.toml` configuration, and
  selection.
- [Skill provenance](./skill-provenance.md) — Ed25519 signing, endorsement,
  and the trust map.
- [Skill activation evidence](./skill-activation-evidence.md) — proving a
  skill earned its place.

## CLI subcommands

Harn subcommands can themselves be written in Harn. The CLI dispatches to a
`.harn` script, and `std/cli/argparse`, `std/cli/envelope`, and
`std/cli/render` give it the same argument parsing, `--json` envelope, and
output rendering the built-in commands use.

- [Extending the CLI in `.harn`](./cli-extending-in-harn.md) — the dispatch
  shim and how to add or port a subcommand without writing Rust.
- [`std/cli/argparse`](./cli-argparse-reference.md),
  [`std/cli/envelope`](./cli-envelope-reference.md),
  [`std/cli/render`](./cli-render-reference.md),
  [`std/cli/paths`](./cli-paths-reference.md) — the supporting modules.

## Which one do I want?

If you are adding a **verb**, write a tool. If you are adding a **service**,
write a connector — it is a package with the extra metadata a host needs to get
you credentials. If you are adding **know-how** rather than a capability, write
a skill. If you are adding a **command you run yourself**, extend the CLI. A
package is the container the first three ship in.
