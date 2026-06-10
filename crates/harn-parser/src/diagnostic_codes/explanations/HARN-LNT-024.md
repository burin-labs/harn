# HARN-LNT-024 — missing harndoc lint

## What it means

A public function has no `/** */` HarnDoc block above its declaration.

This rule is **opt-in**: it only runs when the nearest `harn.toml` sets

```toml
[lint]
require_docstrings = true
```

Out of the box, public functions in user scripts and pipelines need no
doc comments. Embedded stdlib sources always enforce docstrings (the
HARN-STD-101 metadata lint relies on the doc block existing).

## How to fix

- Add a `/** ... */` block with a one-line prose summary above the
  declaration. No structured tags are required; LSP hover derives a usage
  example from the type signature.
- Apply the lint's auto-fix where one is offered (`harn lint --fix`).
- Or leave `require_docstrings` unset if the project doesn't want the
  requirement.
