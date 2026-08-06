## Test framework

Harn includes a built-in test runner invoked via `harn test`.

### Running tests

```bash
harn test path/to/tests/         # run all test files in a directory
harn test path/to/test_file.harn # run tests in a single file
```

Tests are ordinary unprivileged modules by default. A Rust-hosted service may
use `harn test --trusted-host-dispatch path/to/test_file.harn` to compile the
test and its private import graph with the same explicit privileged-wire
authority as its host-selected production route graph. The flag does not
change the authority of modules loaded by ordinary Harn imports.

### Test discovery

The test runner scans `.harn` files for pipelines whose names start with
`test_`. Each such pipeline is executed independently. A test passes if
it completes without error; it fails if it throws or an assertion fails.

```harn
pipeline test_addition(harness: Harness) {
  assert_eq(1 + 1, 2)
}

pipeline test_string_concat(harness: Harness) {
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
pipeline add_case(harness: Harness, left, right, expected) {
  assert_eq(left + right, expected)
}
```

### Reusable fixtures

`@test_fixture(scope: file|case)` marks a zero-argument function with an
explicit return type as reusable test setup. Select it by name with
`@test(fixture: fixture_name)`; the fixture value is injected as the test
pipeline's first parameter and table-row `args` supply the remaining
parameters.

```harn
@test_fixture(scope: file)
fn fixture() -> dict {
  return {prefix: "user", rows: []}
}

@test(
  cases: [
    {name: "alice", args: ["alice", "user:alice"]},
    {name: "bob", args: ["bob", "user:bob"]},
  ],
  fixture: fixture,
)
pipeline test_query(harness: Harness, fx: dict, input: string, expected: string) {
  fx.rows.push(input)
  assert_eq("${fx.prefix}:${input}", expected)
}
```

A `file` fixture runs once for the selected cases that reference it. Its
return value must be isolate-safe data: scalars, bytes, ranges, and nested
lists, dicts, sets, structs, enums, or pairs. Each case receives an isolated
copy-on-write clone, so mutation in one case cannot leak to a sibling.
Execution-bound values such as closures, channels, atomics, task/resource
handles, generators, streams, iterators, or harness capabilities are rejected
as one file-level setup failure.

A `case` fixture runs inside each case's fresh VM immediately before the test
pipeline. Use it for resources or other execution-bound values. The fixture
and test share one timeout and one pipeline lifecycle. VM/resource drop is the
teardown contract for both scopes; there is no separate teardown hook.

If file fixture setup fails, the runner emits one named file-level failure,
suppresses only the cases that reference that fixture, and continues other
files and unrelated tests. `--fail-fast` stops before case scheduling instead.
Fixture declarations, references, scopes, arity, row shape, and row names are
validated during discovery with source locations.

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
| `--parallel` | Run user tests in a bounded pool, or conformance tests in isolated worker processes |
| `--fail-fast` | Stop scheduling new tests after the first failure; already-running parallel tests finish |
| `--junit <path>` | Write JUnit XML report to `<path>` |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay LLM responses from `.harn-fixtures/` |
