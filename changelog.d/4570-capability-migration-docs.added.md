- **The 0.10 migration guide now covers the capability cutover.** The largest
  change in the release — effects moving from globals onto typed `Harness`
  handles — had no migration section, so the only guidance was per-diagnostic
  linter output. The guide now explains why a signature carries its authority,
  gives the exact `harn fix` invocation that performs the rewrite, shows
  before/after for the single- and multi-capability cases, tabulates the
  globals that became a field on a snapshot, and documents
  `HARN_LEGACY_AMBIENT_CAPABILITIES` for staging the upgrade across many
  packages.
