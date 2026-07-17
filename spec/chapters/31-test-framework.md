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
  const result = "hello" + " " + "world"
  assert_eq(result, "hello world")
}
```

Pipelines can also opt in with `@test`, including table-driven cases. A
`@test(cases: [...])` attribute creates one independent test case per row.
Each row must be a dict with a unique string `name` and an `args` list whose
length matches the pipeline parameter count. Reports and filters use the
display name `pipeline[row]`.

```harn
@test(cases: [
  {name: "positive", args: [2, 3, 5]},
  {name: "negative", args: [-2, 1, -1]},
])
pipeline add_case(left, right, expected) {
  assert_eq(left + right, expected)
}
```

### Assertions

Three assertion builtins are available. They can be called anywhere, but
they are intended for test pipelines and the linter warns on non-test use:

| Function | Description |
|---|---|
| `assert(condition, message?)` | Throws if `condition` is falsy |
| `assert_eq(a, b, message?)` | Throws if `a != b`, showing both values |
| `assert_ne(a, b, message?)` | Throws if `a == b`, showing both values |

All three accept an optional custom `message`. If `message` is omitted, nil,
an empty string, or the literal string `"null"` (the common result of
`json_stringify`-ing a value that turned out to be nil), the assertion falls
back to its default message instead of throwing that uninformative value
verbatim.

### Captured output

`log`, `print`, `println`, and related output builtins write into a
per-case buffer rather than directly to the terminal. A passing test stays
quiet by default; a failing test always prints its buffered output
alongside the failure. Pass `--verbose` to see a passing test's captured
output too. `--json-out` and `--junit` reports include it under
`captured_output` (JUnit: `<system-out>`) whenever it is non-empty.

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
| `--fail-fast` | Stop scheduling new tests after the first failure; already-running parallel tests finish |
| `--junit <path>` | Write JUnit XML report to `<path>` |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay LLM responses from `.harn-fixtures/` |
