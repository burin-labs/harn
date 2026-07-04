- **Removed the never-wired `[security]` config-file section.** Prompt-injection
  posture is a runtime directive, not a persisted config field: the only code
  that builds a `SecurityPolicy` reads the `security_policy(...)` pipeline dict
  (via `std/security`'s `spotlight()`/`strict()`/`local_ml()` helpers), and
  nothing ever read `HarnConfig.security`. A persisted `[security]` section
  therefore silently did nothing — a documented-but-inert surface that read as a
  security fail-open (write `mode = "strict"` in config, still get `spotlight`).
  The field, its JSON-schema block, the published `harn-config.schema.json`
  entry, and the misleading config docs are gone; posture is now configured only
  through `security_policy(...)`. Byte-identical at runtime (the field was never
  consumed). `HarnConfig` uses `deny_unknown_fields`, so a config that still
  carries a `[security]` section now fails to load instead of ignoring it.
