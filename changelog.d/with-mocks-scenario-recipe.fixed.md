`harn fix --capability-migrations-only` now projects the retired `with_mocks`
wrapper onto its successor `with_scenario`, threading the root `Harness` and
renaming the `host_mocks` / `llm_mocks` config keys to `capabilities` / `llm`.
Legacy host entries reached through the fixture-source walk also get the
`operation` -> `method` field migration. Previously the repair only dropped the
import when `with_mocks` was uncalled, so every live call site was left behind.
Also corrects a `std/testing` doc comment that still pointed at `with_mocks`.
