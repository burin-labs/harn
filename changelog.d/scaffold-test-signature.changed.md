- **`harn init` no longer scaffolds a test with an unused input.**
  The generated connector test was `pipeline test_provider_id(_task: unknown)`,
  a slot nothing reads. The test runner derives one argument per declared
  non-`Harness` parameter, so the generated test runs the same without it, and
  every new package started by teaching the pattern to whoever read it next.
