- `harn quickstart` refuses an unknown provider before discovery and suggests
  the nearest catalog spelling, refuses a model alias the catalog assigns to a
  different provider, and validates an existing `harn.toml` `[llm]` section
  before writing anything. Its credential table reports deferred discovery as
  `deferred` instead of `set`, and a rerun that keeps existing configuration
  says the requested route was not saved.
