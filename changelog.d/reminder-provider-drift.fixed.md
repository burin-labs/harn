- **The default-enabled reminder-provider set is now derived from a single
  source of truth instead of a parallel hand-maintained list.** Previously
  `canonical_providers()` (the provider objects) and `canonical_provider_ids()`
  (the default-enabled ids) were two separate arrays kept in sync only by
  convention. Adding a default-on provider but forgetting its id in the second
  list left it registered yet never fired — and that silent miss was
  indistinguishable from the intentional opt-in case (the burin compass, which
  is deliberately registered-but-off). `ReminderProvider` gained a
  `default_enabled()` method (defaulting to `true`; the compass overrides it to
  `false`), and the default id set is derived from `canonical_providers()`
  filtered by that flag, so a new provider fires by default automatically and
  the drift class is gone.
