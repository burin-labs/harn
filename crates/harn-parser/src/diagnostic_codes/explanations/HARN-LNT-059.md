# HARN-LNT-059 — rule-engine structural lint

## What it means

A declarative **rule-engine rule** matched here. This is not a built-in lint:
it is a structural rule (a `*.toml` pattern from the project's
`[rules] ruleDirs`) run through the linter via the rule engine (`harn-rules`),
so it appears in `harn lint` output alongside the built-in rules.

The message, severity, and any suggested fix come from the matched rule's
definition — the rule's own `message`, `severity`, and `fix` template. The
diagnostic's reported rule id is the engine rule's `id`, so you filter it with
`disable_rules` (or `[lint]` config) by that id, exactly like a built-in.

## Why it fires

The project declared one or more rule directories:

```toml
[rules]
ruleDirs = ["rules"]
```

and a rule in one of them matched this code. Each such rule pairs a structural
`pattern` with a `message`; rules that also carry a `fix` are applied by
`harn codemod` and surfaced here as a machine-applicable lint fix.

## How to fix

- Address the issue the rule describes (see the rule's `message`).
- If the rule carries a fix, `harn lint --fix` (or `harn codemod`) applies it.
- To silence it, disable the rule by its id, or remove/adjust the rule in your
  `ruleDirs`.

## See also

- The rule engine: `harn scan`, `harn codemod`, and the `harn-rules` skill.
- Project rule discovery: `[rules] ruleDirs` in `harn.toml`.
