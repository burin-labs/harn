- **An unused private pipeline input is now removable on a `test_*`
  declaration.**
  `HARN-LNT-074` exempted any declaration named `test_*`, on the belief that
  the test runner requires a trailing input slot. It does not: the runner
  derives one argument per declared non-`Harness` parameter, so a test pipeline
  runs identically at arity two, one, or zero, and the slot was never part of
  its contract. Consumers accumulated thousands of vestigial `_task` parameters
  that the linter would not offer to remove. An attributed declaration is
  unchanged: `@test(cases: ...)` rows and `@test(fixture: ...)` count the slot,
  so only a bare `@test` allows removal, and a slot any caller passes is still
  kept.
