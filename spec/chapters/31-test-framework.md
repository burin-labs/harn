## Test framework

Harn includes a built-in test runner invoked via `harn test`.

### Running tests

```bash
harn test path/to/tests/         # run all test files in a directory
harn test path/to/test_file.harn # run tests in a single file
```

### Test discovery

The test runner scans `.harn` files for pipelines whose names start with
`test_`. Each such pipeline is executed independently. A test passes if
it completes without error; it fails if it throws or an assertion fails.

```harn
pipeline test_addition() {
  assert_eq(1 + 1, 2)
}

pipeline test_string_concat() {
  let result = "hello" + " " + "world"
  assert_eq(result, "hello world")
}
```

### Assertions

Three assertion builtins are available. They can be called anywhere, but
they are intended for test pipelines and the linter warns on non-test use:

| Function | Description |
|---|---|
| `assert(condition)` | Throws if `condition` is falsy |
| `assert_eq(a, b)` | Throws if `a != b`, showing both values |
| `assert_ne(a, b)` | Throws if `a == b`, showing both values |

### Mock LLM provider

During `harn test`, the `HARN_LLM_PROVIDER` environment variable is
automatically set to `"mock"` unless explicitly overridden. The mock
provider returns deterministic placeholder responses, allowing tests
that call `llm`, `llm_stream`, or `llm_stream_call` to run without API keys.

### CLI options

| Flag | Description |
|---|---|
| `--filter <pattern>` | Only run tests whose names contain `<pattern>` |
| `--verbose` / `-v` | Show per-test timing and detailed failures |
| `--timing` | Show per-test timing and summary statistics |
| `--timeout <ms>` | Per-test timeout in milliseconds (default 30000) |
| `--parallel` | Run test files concurrently |
| `--junit <path>` | Write JUnit XML report to `<path>` |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay LLM responses from `.harn-fixtures/` |

