# Testing

Harn provides several layers of testing support: a conformance test runner,
a standard library testing module, and host-mock helpers for isolating
agent behavior from real host capabilities.

## Conformance tests

Conformance tests are the primary executable specification for the Harn
language and runtime. They live in `conformance/tests/` as paired files:

- `test_name.harn` — Harn source code
- `test_name.expected` — exact expected stdout output

Shared helpers live in `conformance/tests/lib/`.

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
```

### Writing a conformance test

Create a `.harn` file with a `pipeline default(task)` entry point and use
`log()` or `println()` to produce output:

```harn
// conformance/tests/my_feature.harn
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

For testing agent loops without real LLM calls, use `mock_llm_response`:

```harn
mock_llm_response("The answer is 42")

let result = ask { user: "What is the answer?" }
log(result)
```

This queues a canned response that the next LLM call consumes.

## Built-in assertions

Harn provides `assert` and `assert_eq` builtins:

```harn
assert(x > 0, "x must be positive")
assert_eq(actual, expected)
assert_eq(len(items), 3)
```

Failed assertions throw an error with a descriptive message including
the expected and actual values.
