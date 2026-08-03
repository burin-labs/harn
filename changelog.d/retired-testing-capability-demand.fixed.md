`harn fix --capability-migrations-only` now threads a capability handle into
callables that call retired `std/testing` wrappers. The whole-program pass
previously seeded demand only from ambient builtins, so `with_host_mocks` and
`with_mocks` never requested the `HarnessTesting`/`HarnessLlm` carrier their
typed replacements require. The rewrite found no handle to name, declined the
file, and the plan reported convergence while retired calls survived.
