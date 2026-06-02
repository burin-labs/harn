# HARN-LNT-059 — project rule-engine or native lint rule

## What it means

A project-supplied rule matched here. This is not a built-in lint: it is either
a structural rule (a `*.toml` pattern from the project's `[rules] ruleDirs`) run
through the linter via the rule engine (`harn-rules`), or a trusted native rule
library loaded from `[rules] nativeRuleDirs`.

The message, severity, and any suggested fix come from the matched rule. The
diagnostic's reported rule id is the project rule's id, so you filter it with
`disable_rules` (or `[lint]` config) by that id, exactly like a built-in.

## Why it fires

The project declared one or more rule directories:

```toml
[rules]
ruleDirs = ["rules"]
nativeRuleDirs = ["native-rules"]
```

and a rule in one of them matched this code. Declarative rules pair a structural
`pattern` with a `message`; native rules emit diagnostics through the
`harn_lint::native` ABI. Rules that carry a fix surface it as a
machine-applicable lint fix.

## How to fix

- Address the issue the rule describes (see the rule's `message`).
- If the rule carries a fix, `harn lint --fix` applies it. Declarative codemod
  fixes can also be applied by `harn codemod`.
- To silence it, disable the rule by its id, or remove/adjust the rule in your
  `ruleDirs` / `nativeRuleDirs`.

## See also

- The rule engine: `harn scan`, `harn codemod`, and the `harn-rules` skill.
- Project rule discovery: `[rules] ruleDirs` and `nativeRuleDirs` in
  `harn.toml`.
