# Tool hooks cookbook

Copy-paste recipes for the
[`preset_run_command`](../tool-hooks.md) wrapper. Each recipe is
self-contained — drop it into a `pipeline default(task) { ... }` body,
wire the returned closure into your `agent_loop`'s `run_command`
handler, and you have working catalogue-driven command guards.

For the full surface reference see [Preset tool hooks](../tool-hooks.md).
To extend the shipped catalogues see [Contributing preset
hooks](../contributing/preset-hooks.md).

The recipes use `harn,ignore` fences because they show full
agent-loop topologies (handler + dispatch + transcript wiring) that
need an active session to type-check end-to-end. Each fragment is the
same shape as the conformance fixtures under
`conformance/tests/stdlib/tool_hooks_*.harn`, so you can copy and
adapt them with confidence.

## 1. Rust agent: safe `cargo`

A Rust coding agent should never thrash the workspace lockfile, should
keep `println!` output visible from `cargo test`, and should never
force-push `main`. The shipped catalogues cover all three; opting in
to `stacks: ["rust"]` is enough.

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  let run_command = preset_run_command({
    stacks: ["rust"],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {
    tools: {tools: [{name: "run_command", handler: run_command}]},
    system: "You are a Rust contributor. Run cargo commands through run_command.",
  })
}
```

What fires here:

- `rust.cargo.target_dir_conflict` rewrites bare `cargo build|test|check|run`
  to add `--target-dir target-shared`.
- `rust.cargo.test_no_capture_default` appends `-- --nocapture` to
  `cargo test`.
- `rust.cargo.clippy_full_workspace` appends `--workspace` to
  `cargo clippy`.
- `rust.cargo.fmt_check_vs_apply` warns (no rewrite) on bare
  `cargo fmt`.
- The universal catalogue's `git push --force main` and `rm -rf /`
  denies fire regardless of `stacks`.

## 2. Python agent: virtualenv-friendly defaults

Same shape, different opt-in. The Python catalogue rewrites `find` to
`rg --files`, warns on system-wide `pip install`, and keeps `pytest`
output visible.

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  let run_command = preset_run_command({
    stacks: ["python"],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

## 3. TypeScript agent: refuse `--legacy-peer-deps`

The TypeScript catalogue's `ts.npm_install_force_resolution` rule is
informational by default (warning severity, no rewrite). For a
strict-CI agent you can promote it to a deny by switching the mode:

```harn,ignore
import { preset_run_command, tool_hooks_mode_deny_with_explanation } from "std/tool_hooks"

pipeline default(task) {
  let run_command = preset_run_command({
    stacks: ["typescript"],
    mode: tool_hooks_mode_deny_with_explanation,
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

With `tool_hooks_mode_deny_with_explanation` as the wrapper-wide mode,
**every** match denies — not just the npm one. For per-rule
gradations, use the default rewrite-with-audit mode and write
deny-only `custom_rules` (recipe 5).

## 4. Swift agent: protect `.build/` from accidental cleans

The Swift catalogue ships one rule by default. Compose it with custom
rules for project-specific extensions:

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  let xcodebuild_no_destination = tool_rule({
    id: "swift.xcodebuild_no_destination",
    pattern: { command, _context ->
      if !regex_match("^xcodebuild\\b", command) { return false }
      return !contains(command, "-destination")
    },
    applies_to: ["swift"],
    severity: "warning",
    explanation: "Run `xcodebuild` with `-destination` so the simulator/device target is explicit.",
    references: ["https://developer.apple.com/library/archive/technotes/tn2339/_index.html"],
  })

  let run_command = preset_run_command({
    stacks: ["swift"],
    custom_rules: [xcodebuild_no_destination],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

## 5. SQL agent: deny `SELECT *` without a `LIMIT`

The shipped `sql.select_star_warning` rule is `severity: "warning"`
without a rewrite — it surfaces the pattern but doesn't block. To
make it block for an autonomous reporting agent, promote it with a
priority override in a custom rule that matches first:

```harn,ignore
import { preset_run_command, tool_hooks_mode_deny_with_explanation } from "std/tool_hooks"

pipeline default(task) {
  let deny_unbounded_select_star = tool_rule({
    id: "harness.sql.unbounded_select_star",
    pattern: { command, _context ->
      let lc = lowercase(command)
      if !regex_match("^\\s*select\\s+\\*\\s+from\\s+\\S+", lc) { return false }
      return !(contains(lc, " where ") || contains(lc, " limit "))
    },
    applies_to: [],
    severity: "error",
    explanation: "Unbounded `SELECT *` scans entire tables. Add a WHERE / LIMIT clause.",
  })

  let run_command = preset_run_command({
    stacks: ["sql"],
    custom_rules: [deny_unbounded_select_star],
    mode: tool_hooks_mode_deny_with_explanation,
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

Custom rules are matched before the registry, so the deny fires
ahead of the catalogue's softer warning. The default mode would
otherwise let unbounded `SELECT *` through with a warning audit.

## 6. Harn dogfood agent: silence `cargo run --bin harn`

The Harn catalogue patches CLAUDE.md's "always pass `--quiet`"
guidance into the dispatcher itself. Opt-in is one line:

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  let run_command = preset_run_command({
    stacks: ["harn"],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

A bare `cargo run --bin harn -- run examples/hello.harn` rewrites to
`cargo --quiet run --bin harn -- run examples/hello.harn`, and the
audit envelope explains why.

## 7. Multi-stack agent with classifier fallback

A polyglot agent needs every shipped catalogue plus a model-driven
fallback for ad-hoc commands. The classifier is opt-in — leaving
`llm_classifier: nil` preserves passthrough semantics exactly.

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  let run_command = preset_run_command({
    stacks: ["rust", "python", "typescript", "swift", "sql", "harn"],
    llm_classifier: {
      model: "haiku",
      provider: "anthropic",
      threshold: 0.85,
      cache: {ttl_seconds: 3600},
    },
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

What the classifier adds:

- Every command that misses every deterministic rule goes to the
  small model with a JSON-shaped safety verdict.
- Verdicts at or above `threshold` (default `0.8`) dispatch via the
  verdict's mode — `rewrite` → `tool_hooks_mode_rewrite_with_audit`,
  `deny` → `tool_hooks_mode_deny_with_explanation`.
- `allow` and sub-threshold verdicts pass through to `inner`, audited
  as `action: "passthrough"`.
- Cache TTL deduplicates verdicts within the configured window so a
  loop hitting the same command repeatedly only pays the LLM cost
  once.
- Every call (cache hit or miss, verdict or transport error) emits a
  `tool_hook_classifier_verdict` audit entry so you can see exactly
  which decisions were model-driven.

## 8. Audit-only rollout (preview, then enforce)

Roll a new catalogue out by running it in `passthrough_only_audit`
mode first; collect the `tool_rule_warning` audit entries; once the
false-positive rate is acceptable, switch to the default mode.

```harn,ignore
import { preset_run_command, tool_hooks_mode_passthrough_only_audit } from "std/tool_hooks"

pipeline default(task) {
  let run_command = preset_run_command({
    stacks: ["python"],
    mode: tool_hooks_mode_passthrough_only_audit,
    inner: { args -> shell(args.command) },
  })
  let result = agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
  let audits = lifecycle_audit_log_take()
  let warnings = audits |> filter({ a -> a.kind == "tool_rule_warning" })
  println("matched (audit-only): " + to_string(len(warnings)))
}
```

`lifecycle_audit_log_take()` drains the in-memory buffer; combine
with your transcript persistence to retain the rule-firings beyond
the pipeline run.

## 9. Preview without executing

Omit `inner` to get decision envelopes back without running anything.
Useful for unit tests, dry-runs, and replaying a transcript with
different rule shapes:

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default() {
  let preview = preset_run_command({stacks: ["rust"]})
  let envelope = preview("cargo build --release")
  println(envelope.action)        // "rewrite"
  println(envelope.command)       // "cargo build --release --target-dir target-shared"
  println(envelope.rule_id)       // "rust.cargo.target_dir_conflict"
  println(envelope.severity)      // "warning"
}
```

Audit + reminder side effects still fire in preview mode, so a
test asserting on `lifecycle_audit_log_take()` sees the rule
firings even though no command actually executed.

## 10. Composing with `register_tool_hook`

`preset_run_command` is the in-tool wrapper for `run_command`-shaped
tools. The general
[`register_tool_hook`](../extensibility/hooks.md#tool-lifecycle-hooks-register_tool_hook)
surface still applies around every dispatch, including `run_command`.
Both layers compose cleanly — preset rewrites happen inside the
handler, while `register_tool_hook` PreToolUse / PostToolUse fire
around it:

```harn,ignore
import { preset_run_command } from "std/tool_hooks"

pipeline default(task) {
  register_tool_hook({pattern: "*", max_output: 4000})

  let run_command = preset_run_command({
    stacks: ["rust"],
    inner: { args -> shell(args.command) },
  })
  agent_loop(task, {tools: {tools: [{name: "run_command", handler: run_command}]}})
}
```

That gives you "catalogue-driven safety inside the handler, output
caps and pattern bans outside the handler" in eleven lines.
