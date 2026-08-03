- **`harn fix --capability-migrations-only` now migrates the retired
  `with_mocks` wrapper instead of leaving it behind.** The wrapper bundled a
  fixture scope and an LLM scope behind one untyped config, so its replacement
  is a split rather than a rename. The recipe reads the config literal and keeps
  exactly the scopes the site declared, nesting the LLM scope inside the fixture
  scope so teardown order and the body's context argument are unchanged. Capability
  demand is now read per call site, so a host-only wrapper no longer acquires an
  LLM handle it never uses. Configs the recipe cannot read — a forwarded
  parameter, a helper call, or a key outside the two-scope contract — keep their
  call site inert for a human rather than being guessed at.
