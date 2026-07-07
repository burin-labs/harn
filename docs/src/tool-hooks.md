# Preset tool hooks

`preset_run_command(...)` is the shipped wrapper for the
catalogue-driven "command faux-pas" library (epic
[#1884](https://github.com/burin-labs/harn/issues/1884)). It turns a
versionable corpus of shell-mistake rules — `find . -name` instead of
`rg --files`, `cargo build` without `--target-dir`, `git push --force`
to `main`, `rm -rf /`, etc. — into a single `(args) -> result` handler
you can drop straight into `agent_loop`'s tool registry. Rewrites are
applied with audit metadata, denies short-circuit the dispatch, and the
optional LLM classifier handles ad-hoc commands no deterministic rule
covered.

This page is the surface reference. For copy-paste recipes per language
stack see [Cookbook: tool hooks](./cookbooks/tool-hooks.md). To add a
rule to the shipped catalogues see [Contributing preset
hooks](./contributing/preset-hooks.md). The
[`std/tool_hooks`](./stdlib/) module pairs with the lifecycle-hook
surface documented under
[Hooks (tool, persona, session lifecycle)](./extensibility/hooks.md);
they coexist — `register_tool_hook` runs at the dispatcher around every
tool, while `preset_run_command` is the in-tool wrapper specifically
for `run_command`-shaped tools that take a shell command string.

The examples on this page intentionally wrap a shell-command tool: the point
is to intercept, rewrite, deny, and audit command strings before a trusted
host executes them. For ordinary tools that do not need shell syntax, prefer
argv-style calls such as `exec("rg", "--", pattern)` or a narrow host
capability instead of building a shell string.

The quickref companion (LLM-friendly) lives in the "Catalogue-driven
`run_command` hooks" section of
[`docs/llm/harn-quickref.md`](https://harnlang.com/docs/llm/harn-quickref.html).
Cross-references: [system reminders](./system-reminders.md) (the
one-turn reminder injected after each rewrite), [audit
receipts](./audit-receipts.md) (the `tool_rewrite` / `tool_denied` /
`tool_rule_warning` lifecycle entries), and [composable tool
middleware](./stdlib/tool-middleware.md) (the broader middleware story
the preset wrapper composes under).

## Quick start

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  const run_command = preset_run_command({
    stacks: ["rust", "python"],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

That's the whole footprint. Omit `registry` and the wrapper auto-seeds
from `tool_hooks_seed_registry(stacks)`, which always includes the
universal deny catalogue plus one per opted-in stack. The default mode
(`tool_hooks_mode_rewrite_with_audit`) silently fixes the command and
records what it did; agents see the rewritten command in their next
turn via a one-shot `tool_rewritten` system reminder.

## Configuration reference

`preset_run_command(config)` accepts a single optional dict. All keys
have defaults so `preset_run_command()` is valid and returns a wrapper
that runs the universal catalogue against any command before forwarding.

| Key | Default | Purpose |
|---|---|---|
| `stacks` | `[]` | Opt-in stack list (`["rust", "python", "typescript"\|"ts", "swift", "sql", "harn"]`). Drives both registry auto-seed and `tool_hooks_filter`. Catalogues with no `stack` field are always active; per-rule `applies_to` filters further. |
| `registry` | `tool_hooks_seed_registry(stacks)` | Explicit `tool_hooks_registry()` value. Pass your own for tests, vendor patches, or to drop a shipped catalogue. |
| `custom_rules` | `[]` | List of `tool_rule(...)` values matched **before** the registry, regardless of stack scoping. Use for harness-level overrides that must fire unconditionally. |
| `mode` | `tool_hooks_mode_rewrite_with_audit` | Match-dispatch callable shaped `(rule, args, inner) -> result`. Three shipped modes (see below) cover the v1 contract; custom modes can call `tool_hooks_emit_audit` and `tool_hooks_inject_reminder` directly. |
| `inner` | `nil` | Underlying executor shaped `(args) -> result`. Optional — when omitted, the wrapper returns decision envelopes so the caller drives the executor itself (useful for previews and tests). |
| `llm_classifier` | `nil` | Opt-in TH-05 classifier config. See [LLM classifier](#llm-classifier) below. |

### Recognized `args` shapes

The wrapper accepts whichever shape your `run_command` tool already
hands to its handler:

- **String** — `"cargo build --release"`.
- **Dict with `command`/`cmd`/`run`** — `{command: "...", cwd: "..."}`.
  Unknown keys are preserved on the way through to `inner`; rewrites
  swap `command` in place.

The same uniform string is what the classifier and every rule
predicate sees.

## Modes

Each mode is a `(rule, args, inner) -> result` callable. Mix and match
freely; the wrapper picks one for the whole session, but classifier
verdicts dispatch via the verdict's own mode regardless. All three
shipped modes return the same envelope shape so audit consumers can
render them uniformly.

### `tool_hooks_mode_rewrite_with_audit` (default)

Invokes the rule's `rewrite` callable, forwards the rewritten command
to `inner`, and tags the result with audit metadata. Falls back to the
original command when the rule has no rewriter (treat that case as a
"warning that fired but had nothing to fix").

Side effects:

- `tool_hooks_emit_audit("tool_rewrite", payload)` — lifecycle audit
  entry observable via `lifecycle_audit_log_take()`.
- `tool_hooks_inject_reminder({tags: ["tool_rewritten"], body, ttl_turns: 1})`
  — queues a one-turn `system_reminder` so the next agent turn sees
  the corrected shape. When no agent session is active (headless
  pipelines, unit tests) the reminder still produces a
  `tool_hooks.reminder_injected` audit entry so conformance can
  observe the side effect.

### `tool_hooks_mode_deny_with_explanation`

Refuses to dispatch the command. Never invokes `inner`. Records a
`tool_denied` lifecycle audit entry and returns
`{action: "deny", ...envelope}` so the caller can surface a
`tool_error` / `system_reminder` to the agent. Use for irreversible or
policy-violating commands (`git push --force` to `main`, root-adjacent
`rm -rf`).

### `tool_hooks_mode_passthrough_only_audit`

Runs `inner` with the original command unchanged and tags the result
with the matched rule so audit consumers see which rule **would** have
fired. Records a `tool_rule_warning` lifecycle audit entry. Use for
informational rules during a rollout, or for migrating from a more
permissive mode to a stricter one without breaking existing flows.

### Custom modes

A mode is just a Harn closure. The wrapper passes the matched rule
record (whichever `tool_hooks_match` produced), the original `args`,
and the configured `inner`. Custom modes typically combine
`tool_hooks_emit_audit(kind, payload)` and
`tool_hooks_inject_reminder({tags, body, ttl_turns, ...})` for
observability, then return an envelope shaped the same as the shipped
modes:

```harn,ignore
import { tool_hooks_emit_audit, tool_hooks_inject_reminder } from "std/tool_hooks"

const warn_then_run = { rule, args, inner ->
  const cmd = type_of(args) == "string" ? args : to_string(args?.command ?? "")
  tool_hooks_emit_audit("custom.tool_warn", {rule_id: rule.rule_id, command: cmd})
  tool_hooks_inject_reminder({
    tags: ["custom_warning"],
    body: "rule " + rule.rule_id + " advisory: " + rule.explanation,
    ttl_turns: 2,
  })
  if inner == nil { return {action: "warn", rule_id: rule.rule_id, command: cmd} }
  const result = inner(args)
  return {action: "warn", rule_id: rule.rule_id, command: cmd, result: result}
}

const wrapper = preset_run_command({stacks: ["rust"], mode: warn_then_run})
```

## Decision envelope

Every shipped mode returns the same shape. Audit dashboards and
replay tooling should treat unknown `action` strings as advisory
extensions:

```text
{
  action: "rewrite" | "deny" | "passthrough",
  command: <string>,            // what would run (rewritten or original)
  original_command: <string>,   // what the agent submitted
  rule_id: <string>,
  catalogue_id: <string>,
  severity: "error" | "warning" | "info",
  explanation: <string>,
  references: [<string>, ...],  // doc / RFC / post-mortem links
  result?: <inner's return>,    // omitted when inner == nil or action == "deny"
}
```

The no-match passthrough case (no rule fired, no classifier, `inner`
present) is the bare return value of `inner(args)`. When `inner == nil`
the wrapper returns
`{action: "passthrough", command, original_command}` so call-sites can
still tell "we considered it, nothing matched" apart from "we
rewrote".

## Custom rules and registries

`custom_rules` and `registry` are independent surfaces. Custom rules
are matched first regardless of stack scoping, so they're the right
place for harness-level overrides:

```harn,ignore
import { preset_run_command, tool_hooks_mode_deny_with_explanation } from "std/tool_hooks"

const no_curl_pipe_sh = tool_rule({
  id: "harness.curl_pipe_sh",
  pattern: "curl[^|]+\\|\\s*(?:sudo\\s+)?(?:ba)?sh\\b",
  applies_to: [],
  severity: "error",
  explanation: "Pipe-to-shell installs run unreviewed remote code. Download, inspect, then run.",
  references: ["https://www.idontplaydarts.com/2016/04/detecting-curl-pipe-bash-server-side/"],
})

const wrapper = preset_run_command({
  stacks: ["rust"],
  custom_rules: [no_curl_pipe_sh],
  mode: tool_hooks_mode_deny_with_explanation,
  inner: { args -> shell(args.command) },
})
```

To compose a registry by hand (e.g. drop a shipped catalogue and
substitute your own), build one from scratch:

```harn,ignore
import { preset_run_command, tool_hooks_catalogue_universal } from "std/tool_hooks_catalogues"
import { tool_hooks_register, tool_hooks_registry } from "std/tool_hooks"

const vendor_rust = catalogue({
  id: "vendor/rust",
  stack: "rust",
  rules: [/* ... vendor-pinned rules ... */],
})
const registry = tool_hooks_register(
  tool_hooks_register(tool_hooks_registry(), tool_hooks_catalogue_universal()),
  vendor_rust,
)
const wrapper = preset_run_command({stacks: ["rust"], registry: registry})
```

The registered catalogues' `stack` field cooperates with
`tool_hooks_filter`: stackless catalogues survive every filter (used
for the universal deny set), and stack-tagged catalogues only fire
when the caller opts in via `stacks`.

## LLM classifier

The optional classifier (TH-05, [#1898](https://github.com/burin-labs/harn/issues/1898))
catches ad-hoc commands no deterministic rule matched. It's strictly
opt-in — leaving `llm_classifier: nil` preserves TH-02 passthrough
semantics byte-for-byte.

```harn,ignore
const wrapper = preset_run_command({
  stacks: ["rust", "python"],
  llm_classifier: {
    model: "haiku",
    provider: "anthropic",
    threshold: 0.8,
    cache: {ttl_seconds: 3600},
  },
  inner: { args -> shell(args.command) },
})
```

The classifier calls a small model with a meta-prompt that asks for a
single-line JSON verdict shaped
`{kind: "rewrite"|"deny"|"allow", confidence, rewritten?, explanation?, references?}`,
then:

- **`confidence >= threshold` and `kind == "rewrite"`** — dispatches
  via `tool_hooks_mode_rewrite_with_audit` with the model's
  `rewritten` substitution.
- **`confidence >= threshold` and `kind == "deny"`** — dispatches via
  `tool_hooks_mode_deny_with_explanation`.
- **`kind == "allow"` or sub-threshold** — passes through to `inner`
  audited so observers can verify "we asked, the model wasn't sure".
- **Transport error or non-JSON response** — degrades to passthrough,
  still audited.

Every invocation emits a `tool_hook_classifier_verdict` audit entry
(`kind`, `confidence`, `scope`, `cache_hit`, `action`, plus
`normalized_command` and `error` when present) so observers can trace
which decisions were model-driven without re-running the model.

### Classifier config keys

| Key | Default | Notes |
|---|---|---|
| `model` | (required) | Non-empty string. Pass a small/cheap model — every classifier miss is one extra `llm_call_safe`. |
| `threshold` | `0.8` | Float in `[0, 1]`. Verdicts below this fall through to `inner`. |
| `meta_prompt` | shipped default | Override only if you need to extend the verdict schema; the default is the safety reviewer prompt validated by conformance. |
| `provider` | active default | Pin a provider explicitly to avoid silent provider-switch surprises. |
| `cache.ttl_ms` / `cache.ttl_seconds` | `0` (no cache) | Cache scope is per-config (or per `id` if supplied) so independent wrappers don't share verdicts. |
| `llm_options` | `{}` | Forwarded verbatim. Use to pin `temperature`, `seed`, retries, etc. |

### Trust contract

The classifier sends the raw command text + the meta prompt to the
configured model. Callers must redact secrets in command arguments
the same way `run_command` already requires for the underlying shell.
Classifier calls count against the session's normal LLM cost telemetry
(`peek_total_cost`, autonomy budget, etc.) — keep the model small.

## `ToolRule` schema

Every rule passes through `tool_rule(config)`, which validates the
shape at construction time so misconfigurations fail loudly rather
than at first dispatch.

| Field | Type | Required | Purpose |
|---|---|---|---|
| `id` | `string` | yes | `<stack>.<tool>.<short>` convention so audit consumers can group by stack and tool. Must be unique within a catalogue. |
| `pattern` | `string` regex or `(command, context) -> bool` callable | yes | Match predicate. Regex strings are compiled lazily; callables let you express lookaround-free predicates (the Rust `regex` crate has no lookaround support). |
| `applies_to` | `list<string>` | no | Per-rule stack scoping. Empty list = matches every `stacks` opt-in (use for universal rules). |
| `severity` | `"error"` \| `"warning"` \| `"info"` | no, default `"warning"` | Drives audit-side rendering and the shipped modes' kind selection. |
| `explanation` | `string` | no | Single-sentence rationale the agent can paraphrase back to the user. |
| `references` | `list<string>` | no | Links to upstream docs, RFCs, or post-mortems. Surface in the audit envelope. |
| `priority` | `int` | no, default `0` | Sort key during the linear sweep. Higher fires first. Universal deny rules use `1000`; stack-specific rules use `0`–`100`. |
| `rewrite` | `(command, context) -> string \| dict \| nil` | no | Optional rewriter. Return `nil` or the same string to skip (rule still fires); return a new string to substitute. Dict form is reserved for future structured arg overrides. |

### `Catalogue` schema

Every catalogue passes through `catalogue(config)`:

| Field | Type | Required | Purpose |
|---|---|---|---|
| `id` | `string` | yes | `<source>/<stack>` convention (e.g. `harn-canon/rust`). |
| `rules` | `list<tool_rule>` | yes | Rule corpus. Validated for unique `id`s within the catalogue. |
| `stack` | `string` | no | Opt-in scoping key. Omit for stackless universal catalogues. |
| `version` | `string` | no | Semver tag for the catalogue, e.g. `"0.1.0"`. |
| `source` | `string` | no | Provenance tag. The shipped catalogues use `"harn-canon"`. |
| `priority` | `int` | no, default `0` | Sort key applied before per-rule priority. Universal catalogues use `100` so deny rules sweep first. |

### `Registry`

`tool_hooks_registry()` returns an empty registry value;
`tool_hooks_register(registry, catalogue)` appends; `tool_hooks_filter(registry, stacks)`
narrows by stack list (stackless catalogues always survive);
`tool_hooks_unregister(registry, catalogue_id)` removes a catalogue by
id; `tool_hooks_list(registry)` enumerates ids; and
`tool_hooks_match(registry, command, {stacks?})` is the underlying
linear-sweep matcher the wrapper uses.

## Shipped catalogues

`tool_hooks_seed_registry(stacks)` composes the universal catalogue
plus one per opted-in stack. Unknown stacks are silently skipped so
older scripts opting into a future stack don't break.

| Catalogue | Stack | Rules |
|---|---|---|
| `harn-canon/universal` | (stackless) | `universal.git_push_force_main` (deny `git push --force` to `main` / `master` unless `HARN_TOOL_HOOKS_ALLOW_FORCE_PUSH=1`); `universal.rm_rf_root_adjacent` (deny `rm -rf` against `/`, `~`, `..`, `$HOME`, `${HOME}`, `*`). |
| `harn-canon/rust` | `rust` | `target_dir_conflict` (rewrite `cargo build\|test\|check\|run` to add `--target-dir target-shared`); `test_no_capture_default` (append `-- --nocapture` to `cargo test`); `clippy_full_workspace` (append `--workspace` to `cargo clippy`); `fmt_check_vs_apply` (warn on bare `cargo fmt`). |
| `harn-canon/python` | `python` | `find_versus_rg` (rewrite `find . -name '*.py'` to `rg --files -t py`); `pip_install_no_user` (warn on system-wide `pip install`); `pytest_no_capture` (append `-s` to bare `pytest`). |
| `harn-canon/typescript` | `typescript` (alias `ts`) | `find_versus_rg` (rewrite `find . -name '*.ts\|tsx\|js\|jsx\|mjs\|cjs'` to `rg --files -t ts`); `npm_install_force_resolution` (warn on `npm install --force` / `--legacy-peer-deps`). |
| `harn-canon/swift` | `swift` | `swift_build_clean_cache_misuse` (warn on `swift build --clean` / `swift package clean`). |
| `harn-canon/sql` | `sql` | `select_star_warning` (warn on unbounded `SELECT * FROM ...`). |
| `harn-canon/harn` | `harn` | `cargo_run_quiet` (rewrite `cargo run --bin harn` to `cargo --quiet run --bin harn`). |

The full source is in
[`crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`](https://github.com/burin-labs/harn/blob/main/crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn).
Each catalogue's HarnDoc lists its rules, severity, and rationale.

## Observability

Every shipped mode and the classifier emit lifecycle audit entries
that flow through the standard transcript machinery:

| Kind | Emitter | Payload (subset) |
|---|---|---|
| `tool_rewrite` | `tool_hooks_mode_rewrite_with_audit` | `rule_id`, `catalogue_id`, `severity`, `original_command`, `command`, `explanation` |
| `tool_denied` | `tool_hooks_mode_deny_with_explanation` | `rule_id`, `catalogue_id`, `severity`, `command`, `explanation` |
| `tool_rule_warning` | `tool_hooks_mode_passthrough_only_audit` | `rule_id`, `catalogue_id`, `severity`, `command`, `explanation` |
| `tool_hook_classifier_verdict` | LLM classifier | `scope`, `model`, `kind`, `confidence`, `threshold`, `cache_hit`, `action`, `normalized_command`, `error?` |
| `tool_hooks.reminder_injected` | Any reminder-injecting mode | The reminder spec passed to `tool_hooks_inject_reminder` |

`lifecycle_audit_log_take()` drains the in-memory buffer so unit tests
can assert on the exact sequence; in a live session, the events are
also written to the transcript next to `hook_call` / `hook_returned`
events from the broader [hooks
surface](./extensibility/hooks.md). The `tool_rewritten` system
reminder threads back through the [system reminders](./system-reminders.md)
machinery (`ttl_turns: 1` by default, `tags: ["tool_rewritten"]`).

## Composing with the broader hooks surface

`preset_run_command` is an *in-tool* wrapper specifically for
`run_command`-shaped tools. The general
[`register_tool_hook`](./extensibility/hooks.md#tool-lifecycle-hooks-register_tool_hook)
surface fires `PreToolUse` / `PostToolUse` around **every** tool
dispatch in the agent loop. The two compose freely:

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  register_tool_hook({pattern: "*", max_output: 4000})
  register_tool_hook({pattern: "exec_*", deny: "exec_* is gated"})

  const run_command = preset_run_command({
    stacks: ["rust"],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

Execution order for a `run_command` call here:

1. `register_tool_hook` `PreToolUse` fires (can deny or rewrite `args`).
2. The handler runs — that's where `preset_run_command` matches the
   command, picks a mode, optionally rewrites, and forwards to `inner`.
3. `register_tool_hook` `PostToolUse` fires (can rewrite or truncate
   the result via `max_output`).

Both layers write to the transcript, so replay tools see every
decision in order.

## Tracking issues

- Epic: [#1884 — Preset run-command hook library](https://github.com/burin-labs/harn/issues/1884).
- TH-01..TH-07 status: all shipped (see epic for individual issue
  numbers). TH-07 ([#2022](https://github.com/burin-labs/harn/pull/2022))
  closed out the conformance coverage; this page is TH-08
  ([#1901](https://github.com/burin-labs/harn/issues/1901)).
- Cross-references: combinators
  ([#1860](https://github.com/burin-labs/harn/issues/1860)), system
  reminders ([#1815](https://github.com/burin-labs/harn/issues/1815)).
