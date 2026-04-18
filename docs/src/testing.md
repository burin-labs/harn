# Testing

Harn provides several layers of testing support: a conformance test runner,
a standard library testing module, and host-mock helpers for isolating
agent behavior from real host capabilities.

## Conformance tests

Conformance tests are the primary executable specification for the Harn
language and runtime. They live under `conformance/tests/` as paired files:

- `test_name.harn` — Harn source code
- `test_name.expected` — exact expected stdout output

Tests are grouped by area into subdirectories. `ls conformance/tests/` gives
the current top-level map (examples: `language/`, `control_flow/`, `types/`,
`collections/`, `concurrency/`, `stdlib/`, `templates/`, `modules/`,
`agents/`, `integration/`, `runtime/`). The runner discovers `.harn` files
recursively, so new tests just need to be dropped into the appropriate
subdirectory.

Shared helpers live alongside the tests that use them:
`conformance/tests/modules/lib/` holds import targets for the `modules/`
tests, and `conformance/tests/templates/fixtures/` holds prompt-template
fixtures for the `templates/` tests.

Error tests (Harn programs that are expected to fail) live under
`conformance/errors/`, similarly subdivided into `syntax/`, `types/`,
`semantic/`, and `runtime/`.

### Running tests

```bash
# Run the full conformance suite
harn test conformance

# Filter by name (substring match)
harn test conformance --filter workflow_runtime

# Filter by tag (if test uses tags)
harn test conformance --tag agent

# Verbose output
harn test conformance --filter my_test -v

# Timing summary without verbose failure details
harn test conformance --timing --filter my_test
```

### Writing a conformance test

Create a `.harn` file with a `pipeline default(task)` entry point and use
`log()` or `println()` to produce output:

```harn
// conformance/tests/<group>/my_feature.harn  (e.g. stdlib/, types/)
pipeline default(task) {
  let result = my_feature(42)
  log(result)
}
```

Then create a `.expected` file with the exact output:

```text
[harn] 84
```

## The `std/testing` module

Import `std/testing` in your Harn tests for higher-level test helpers:

```harn
import { mock_host_result, assert_host_called, clear_host_mocks } from "std/testing"
```

### Host mock helpers

| Function | Description |
|----------|-------------|
| `clear_host_mocks()` | Remove all registered host mocks |
| `mock_host_result(cap, op, result, params?)` | Mock a host capability to return a value |
| `mock_host_error(cap, op, message, params?)` | Mock a host capability to return an error |
| `mock_host_response(cap, op, config)` | Mock with full response configuration |

### Host call assertions

| Function | Description |
|----------|-------------|
| `host_calls()` | Return all recorded host calls |
| `host_calls_for(cap, op)` | Return calls for a specific capability/operation |
| `assert_host_called(cap, op, params?)` | Assert a host call was made |
| `assert_host_call_count(cap, op, expected_count)` | Assert exact call count |
| `assert_no_host_calls()` | Assert no host calls were made |

### Example

```harn
import { mock_host_result, assert_host_called, clear_host_mocks } from "std/testing"

pipeline default(task) {
  clear_host_mocks()

  // Mock the workspace.read_text capability
  mock_host_result("workspace", "read_text", "file contents")

  // Code under test calls host_call("workspace.read_text", ...)
  let content = host_call("workspace.read_text", {path: "test.txt"})
  log(content)

  // Verify the call was made
  assert_host_called("workspace", "read_text")
}
```

## LLM mocking

For testing agent loops without real LLM calls, use `llm_mock()`:

```harn
llm_mock({text: "The answer is 42"})

let result = llm_call([
  {role: "user", content: "What is the answer?"}
])
log(result)
```

This queues a canned response that the next LLM call consumes.

For end-to-end CLI runs, `harn run` can preload the same mock
infrastructure from a JSONL fixture file:

```jsonl
{"text":"PLAN: find the middleware module first","model":"fixture-model"}
{"match":"*hello*","text":"matched","model":"fixture-model"}
{"match":"*","error":{"category":"rate_limit","message":"fake rate limit"}}
```

```bash
harn run script.harn --llm-mock fixtures.jsonl
```

- A line without `match` is FIFO and is consumed on use.
- A line with `match` is a reusable glob against the prompt text.
- When no fixture matches, `harn run --llm-mock ...` fails with the
  first prompt snippet so you can add the missing case directly.

To capture a replayable fixture from a run, record once and then replay
the saved JSONL:

```bash
harn run script.harn --llm-mock-record fixtures.jsonl
harn run script.harn --llm-mock fixtures.jsonl
```

## Built-in assertions

Harn provides `assert`, `assert_eq`, and `assert_ne` builtins for test pipelines:

```harn
assert(x > 0, "x must be positive")
assert_eq(actual, expected)
assert_ne(actual, unexpected)
assert_eq(len(items), 3)
```

Failed assertions throw an error with a descriptive message including
the expected and actual values.

Use `require` for runtime invariants in normal pipelines. The linter warns if
you use `assert*` outside test pipelines, and it suggests `assert*` instead of
`require` inside test pipelines.
