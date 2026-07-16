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
`agents/`, `scenarios/` (cross-feature compositions), `reminders/`,
`runtime/`). High-volume categories may have a second level — for example,
`stdlib/oauth/`, `stdlib/json/`, `stdlib/hitl/`, `stdlib/preset_hooks/`,
`stdlib/tool_hooks/`, and `stdlib/project/` group the larger stdlib API
surfaces. The runner discovers `.harn` files recursively, so new tests just
need to be dropped into the appropriate subdirectory.

Shared helpers live alongside the tests that use them:
`conformance/tests/modules/lib/` holds import targets for the `modules/`
tests, and `conformance/tests/templates/fixtures/` holds prompt-template
fixtures for the `templates/` tests. The cross-cutting helper
`conformance/tests/_common.harn` is imported as `"../_common"` from any
direct subdirectory and `"../../_common"` from a second-level subdirectory.

Error tests live in two complementary homes:

- `conformance/errors/`, subdivided by error **class** into `syntax/`,
  `types/`, `semantic/`, and `runtime/` — for tests organized by where the
  error fires in the compilation pipeline.
- `conformance/tests/errors_by_feature/` — for error tests grouped by the
  feature that produces them (for example, `agent_loop_*`, `defer_*`,
  `catch_*`, `finally_*`).

Both homes share the `.harn` + `.error` (or `.expected`) sibling-file
convention and are walked by the same runner.

### Running tests

```bash
# Run the full conformance suite
harn test conformance

# Filter by name (substring match)
harn test conformance --filter workflow_runtime

# Filter by name or path
harn test conformance --filter agent

# Verbose output
harn test conformance --filter my_test -v

# Timing summary without verbose failure details
harn test conformance --timing --filter my_test
```

For user tests, `--timeout` bounds only the pipeline execution phase. VM,
stdlib, skill, and manifest setup is measured separately and cannot consume the
test body's correctness budget. Use `--max-test-ms` when total wall time is a
performance requirement, or `--max-execute-ms` to ratchet execution cost.
Conformance and other non-user targets continue to apply `--timeout` to their
whole test case or subprocess.

### Writing a conformance test

Create a `.harn` file with a `pipeline default(task)` entry point and use
`log()` to produce output:

```harn,ignore
// conformance/tests/<group>/my_feature.harn  (e.g. stdlib/, types/)
pipeline default(task) {
  const result = my_feature(42)
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
| `mock_host_result(cap, op, result, params?, unregistered_ok?)` | Mock a host capability to return a value |
| `mock_host_error(cap, op, message, params?, unregistered_ok?)` | Mock a host capability to return an error |
| `mock_host_response(cap, op, config)` | Mock with full response configuration |

### Host call assertions

| Function | Description |
|----------|-------------|
| `host_calls()` | Return all recorded host calls |
| `host_calls_for(cap, op)` | Return calls for a specific capability/operation |
| `host_call_count()` / `host_call_count_for(cap, op)` | Return recorded host call counts |
| `assert_host_called(cap, op, params?)` | Assert a host call was made |
| `assert_host_call_count(cap, op, expected_count)` | Assert exact call count |
| `assert_no_host_calls()` | Assert no host calls were made |

### Persona step assertions

Persona steel-thread tests can assert Harn orchestration boundaries without
depending on Rust internals. `step_assertions_begin(pattern?)` installs
`PreStep` / `PostStep` hooks for matching personas and records the hook
payloads until `step_assertions_end()`.

| Helper | Description |
|----------|-------------|
| `step_assertions_begin(persona_pattern?)` | Clear persona hooks and start recording matching step payloads |
| `step_events()` / `step_events_clear()` | Inspect or reset captured step payloads |
| `assert_steps_ran(names)` | Assert the exact ordered list of `@step` names |
| `assert_step_received(step, predicate?)` | Assert a `PreStep` payload matched a closure, dict subset, or value |
| `assert_step_emitted(step, predicate?)` | Assert a `PostStep` payload matched a closure, dict subset, or value |
| `assert_handoff_emitted(source, kind, target?)` | Assert a run record or handoff list contains a typed handoff |
| `assert_receipt_field(receipt, pointer, expected)` | Assert an RFC 6901 JSON Pointer field in a receipt, with a diff on failure |
| `assert_golden_transcript(actual, golden)` | Structured subset matcher with `<ms>`, `<uuid>`, and `<any>` sentinels |

### Example

```harn
import { mock_host_result, assert_host_called, clear_host_mocks } from "std/testing"

pipeline default(task) {
  clear_host_mocks()

  // Mock the workspace.read_text capability
  mock_host_result("workspace", "read_text", "file contents")

  // Code under test calls host_call("workspace.read_text", ...)
  const content = host_call("workspace.read_text", {path: "test.txt"})
  log(content)

  // Verify the call was made
  assert_host_called("workspace", "read_text")
}
```

### Scoped fixtures (`with_host_mocks` / `with_llm_mocks` / `with_mocks`)

Pipeline tests with many capabilities accumulate manual `host_mock_clear()`
pairs around each test. A failing assertion can skip the clear step and leak
mocks into the next test. Scoped fixtures handle that lifecycle for you and
clean up reliably even when the body throws.

| Helper | Description |
|----------|-------------|
| `with_host_mocks(mocks, body)` | Push a fresh host-mock scope, register `mocks`, run `body()`, restore on exit |
| `with_llm_mocks(mocks, body)` | Same shape for LLM mocks (FIFO + `match` patterns) |
| `with_mocks({host_mocks, llm_mocks}, body)` | Combined scope for tests that exercise both surfaces |
| `llm_calls()` / `llm_call_count()` | Inspect the LLM call log captured inside the current scope |

Each entry in the host-mock list is a dict shaped like the existing
`host_mock(...)` config:

```harn
{capability: "runtime", operation: "pipeline_input", result: {}, params: {}}
{capability: "project", operation: "metadata_set", error: "denied"}
{
  capability: "tools",
  operation: "run_command",
  result: {status: "completed", exit_code: 0, stdout: "ok", stderr: ""},
  params: {argv: ["echo", "ok"]},
}
```

`error` (if non-nil) takes precedence over `result`, mirroring
`mock_host_error` / `mock_host_result`.

Host mocks are strict by default: the capability/operation must be registered
by Harn or the active hostlib embedder. Use `unregistered_ok: true` only for a
pure test-local operation that does not correspond to a real host boundary.

Hostlib builtins that take a request dict use the same registry under their
module/method pair, so `hostlib_tools_run_command(...)` can be mocked with
`{capability: "tools", operation: "run_command"}`. For the `process.exec` to
hostlib migration, `hostlib_tools_run_command(...)` also honors existing
`{capability: "process", operation: "exec"}` mocks when the declared `params`
subset matches the command request.

```harn,ignore
import { with_host_mocks, assert_host_called } from "std/testing"

pipeline test_skill_registry() {
  with_host_mocks(
    [
      {capability: "runtime", operation: "pipeline_input", result: {}},
      {capability: "project", operation: "skills", result: [], unregistered_ok: true},
    ],
    { ->
      const registry = skill_registry_from_host()
      assert_eq(len(registry.skills), 0, "no skills registered")
      assert_host_called("project", "skills", nil, nil)
    },
  )
}
```

Key properties:

- The body runs inside a fresh host-mock and host-call log; nothing inside
  leaks out, and nothing outside is visible inside.
- The prior state is restored before the helper returns, including when the
  body throws — the thrown error is re-raised after cleanup.
- Scopes nest: an inner `with_host_mocks` sees only its own mocks while
  active, then pops back to the outer scope on exit.
- `with_llm_mocks` follows the same shape; entries are passed straight to
  `llm_mock(...)`, so any field accepted by that builtin (including
  `match` / `consume_match` / `error`) is supported.

`with_mocks(config, body)` is the unified form for tests that need both:

```harn,ignore
with_mocks(
  {
    host_mocks: [{capability: "ws", operation: "read", result: "ok", unregistered_ok: true}],
    llm_mocks: [{text: "agreed"}],
  },
  { ->
    run_pipeline_under_test()
  },
)
```

### Golden-file snapshots (`assert_snapshot`)

`assert_snapshot(name, actual, options?)` pins a string against a committed
golden file, the file-backed counterpart to `assert_golden_transcript`. It
follows the Jest `toMatchSnapshot` / insta model: the golden lives at
`__snapshots__/<name>.harn.snap` next to the test file, running with
`HARN_UPDATE_SNAPSHOTS=1` writes it, and every other run asserts equality and
fails with a unified diff on drift.

| Behavior | Result |
|----------|--------|
| `HARN_UPDATE_SNAPSHOTS=1`, not in CI | (Re)writes the golden and passes |
| `HARN_UPDATE_SNAPSHOTS=1`, in CI | **Ignored** — compares only; missing/drift is a hard failure |
| `options.update = true` (any environment) | Writes the golden and passes |
| `actual` equals the golden | Passes, returns `actual` |
| `actual` differs from the golden | Fails with a unified diff (golden vs actual) |
| Golden missing, no write signal | Fails with guidance to update locally |

**CI safety.** In CI (the `CI` or `HARN_CI` env var set) the
`HARN_UPDATE_SNAPSHOTS` trigger is **ignored** — the primitive compares only and
never creates or rewrites a golden. So a broken output cannot silently
rubber-stamp itself green on the CI machine even if the update flag leaks into a
CI job (the classic snapshot footgun). Goldens are (re)written only through the
explicit local flow: run with `HARN_UPDATE_SNAPSHOTS=1` locally, review the diff,
and commit. The `options.update = true` seam is deliberate in-source code (not
the accidental env-leak vector) and stays honored even under CI — it exists so a
test can drive the write path against a temp/fixture golden.

`options.dir` overrides the `__snapshots__/` directory (handy for driving a
golden into a temp workspace). `options.redact` is a list of
`{pattern, replacement?}` regex scrubs applied to `actual` before write/compare,
for masking residual volatile tokens. Keep snapshots small and deterministic —
no wall-clock or random inputs belong in `actual`. The primitive only ever reads
or writes its own `<name>.harn.snap` file and never deletes anything.

```harn,ignore
import { assert_snapshot } from "std/testing"

pipeline test_render() {
  // Run once locally with HARN_UPDATE_SNAPSHOTS=1 to write
  // __snapshots__/home_page.harn.snap, then commit it; later runs assert.
  assert_snapshot("home_page", render_home_page())
}
```

## LLM mocking

For testing agent loops without real LLM calls, use `llm_mock()`:

```harn
llm_mock({text: "The answer is 42"})

const result = llm_call([
  {role: "user", content: "What is the answer?"},
].join("\n"))
log(result)
```

This queues a canned response that the next LLM call consumes.

For end-to-end CLI runs, `harn run` and `harn playground` can preload the same mock
infrastructure from a JSONL fixture file:

```jsonl
{"text":"PLAN: find the middleware module first","model":"fixture-model"}
{"match":"*hello*","text":"matched","model":"fixture-model"}
{"match":"*","error":{"category":"rate_limit","message":"fake rate limit"}}
{"match":"*retry*","error":{"status":503,"kind":"transient","reason":"upstream_unavailable"}}
```

```bash
harn run script.harn --llm-mock fixtures.jsonl
harn playground --script pipeline.harn --llm-mock fixtures.jsonl
```

- A line without `match` is FIFO and is consumed on use.
- A line with `match` is checked in file order as a glob against the request transcript text.
- Add `"consume_match": true` when repeated matching prompts should advance
  through a scripted sequence instead of reusing the same line forever.
- When no fixture matches, `harn run --llm-mock ...` and
  `harn playground --llm-mock ...` fail with the
  first prompt snippet so you can add the missing case directly.

To capture a replayable fixture from a run, record once and then replay
the saved JSONL:

```bash
harn run script.harn --llm-mock-record fixtures.jsonl
harn run script.harn --llm-mock fixtures.jsonl

harn playground --script pipeline.harn --llm-mock-record fixtures.jsonl
harn playground --script pipeline.harn --llm-mock fixtures.jsonl
```

To import an external eval trace into the same fixture format:

```bash
harn trace import \
  --trace-file traces/generic.jsonl \
  --trace-id trace_123 \
  --output fixtures/imported.jsonl
```

The importer expects JSONL records shaped like
`{prompt, response, tool_calls}` and passes through common metadata
such as `model`, `provider`, and token counts when present.

## Eval kinds

`harn eval` supports the default replay fixture flow plus an explicit
clarifying-question kind for ambiguous tasks.

`harn eval context <manifest>` supports deterministic context-engineering
fixtures for pack, projection, compaction, and tool-disclosure experiments. A
manifest declares task fixtures and one or more context modes; the runner scores
each task/mode pair without model calls and writes stable local artifacts:
`summary.json`, `per_run.jsonl`, and `summary.md`. Use the builders in
`std/context/eval` when authoring manifests from Harn code, and use
`spec/schemas/context-eval-report.v1.schema.json` when ingesting
`harn.context_eval.report.v1` reports from hosted systems or downstream UIs.

```bash
harn eval context examples/evals/context-engineering-smoke.json \
  --output target/context-eval --json
```

`harn eval scope_triage` runs the opt-in pre-turn scope-classifier measurement
harness. The default mode uses a deterministic reference classifier over the
100-case synthetic dataset; pass `--live --model ollama:qwen3:1.7b` to exercise
the local small-model classifier. The report includes turn-cost reduction,
coverage, false-positive rate, false-negative rate, and a keep-default-off /
graduate decision.

```bash
harn eval scope_triage --output .harn-runs/scope-triage/latest
```

## Eval packs

Portable eval packs live in `harn.eval.toml` or another TOML file listed in
`[package].evals` in `harn.toml`. The same pack can be run locally and imported
by hosted tooling because it contains only portable fixture references, rubrics,
judge metadata, thresholds, and package metadata.

```toml
version = 1
id = "slack-connector"
name = "Slack connector evals"

[package]
name = "slack-connector"
version = "0.1.0"

[[fixtures]]
id = "url-verification-run"
kind = "run-record"
path = "fixtures/url-verification.run.json"

[[fixtures]]
id = "url-verification-replay"
kind = "replay-fixture"
path = "fixtures/url-verification.replay.json"

[[rubrics]]
id = "webhook-normalization"
kind = "deterministic"
description = "Webhook normalization keeps status and response shape stable."

[[rubrics.assertions]]
kind = "run-status"
expected = "completed"

[[cases]]
id = "url-verification"
name = "URL verification handshake"
run = "url-verification-run"
fixture = "url-verification-replay"
rubrics = ["webhook-normalization"]
severity = "blocking"

[cases.thresholds]
max-latency-ms = 500
max-cost-usd = 0.001
```

Run a single pack directly:

```bash
harn eval harn.eval.toml
```

Run the eval packs shipped by a package:

```bash
harn test package --evals
```

After `harn install`, this also includes eval packs declared by installed
dependency packages in the leased current generation. Dependency eval packs are
passive until this command or a root `eval_pack://...` trigger references them.

`[package].evals` is optional when the package root contains
`harn.eval.toml`; otherwise declare one or more package-relative pack paths:

```toml
[package]
name = "slack-connector"
version = "0.1.0"
evals = ["evals/webhooks.toml", "evals/replay.toml"]
```

Fixture refs support these portable `kind` values:

| Kind | Local behavior |
|---|---|
| `run-record` or `recorded-run` | Loads a persisted Harn run record JSON file |
| `replay-fixture` | Loads a replay fixture JSON file |
| `friction-events` | Loads repeated-friction event fixtures and evaluates generated context-pack suggestions |
| `jsonl-trace` | Reserved for imported trace fixture metadata |
| `provider-events` | Reserved for synthetic provider event streams |
| `connector-payload` | Reserved for connector payload samples |

Local `harn eval` executes replay fixtures, baseline comparisons,
deterministic assertions, HITL question assertions, repeated-friction
context-pack suggestion assertions, and cost/latency/token/stage thresholds.
`llm-judge` rubrics carry judge model, calibration, tie-break, and
prompt-version metadata for hosted or explicit judge runners; a blocking
`llm-judge` rubric fails locally rather than being silently skipped.

Case `metadata` is preserved on the report's `stats_rows`, so packs can define
their own scalar taxonomy without extending the manifest schema. Use
`axis_breakdown` from `std/eval/stats` to measure each value while making
unclassified cases explicit:

```harn
import "std/eval/stats"

const by_language = axis_breakdown(report.stats_rows, "language")
```

The breakdown composes macro pass@1, reliability, skip rate, timeout rate, and
cost per solved. It does not impose a product-specific threshold; the pack or
gate consuming the report owns that policy.

Eval packs can also include persona timeout ladders. A `[[ladders]]`
entry runs the same persona fixture across every configured
`model-routes` / `timeout-tiers` combination, writes per-tier JSONL
transcripts, receipts, and summaries, and reports the first route/tier
that completed correctly. Degraded and looping tiers remain in the
machine-readable report so host CLIs and TUIs can render the same
result without reimplementing the matrix runner.

```toml
[[ladders]]
id = "merge-captain-green-pr"
persona = "merge_captain"
artifact-root = ".harn-runs/merge-captain-timeout-ladder"

[ladders.backend]
kind = "replay"
path = "../../examples/personas/merge_captain/transcripts/green_pr.jsonl"

[[ladders.model-routes]]
id = "gemma-value"
route = "local/gemma-value"
provider = "llama.cpp"
model = "gemma"
profile = "value"

[[ladders.timeout-tiers]]
id = "balanced"
timeout-ms = 500
max-tool-calls = 4
max-model-calls = 1
```

Repeated-friction cases use `friction_events = "<fixture-id-or-path>"` and a
rubric assertion such as:

```toml
[[rubrics.assertions]]
kind = "context-pack-suggestion"
contains = "incident"
expected = { min_suggestions = 1, recommended_artifact = "context_pack", required_capability = "splunk.search" }
```

Threshold `severity` controls gate behavior:

| Severity | Local gate behavior |
|---|---|
| `blocking` | Failing case exits non-zero |
| `warning` | Failure is reported but does not fail the command |
| `informational` | Failure is reported as info only |

### Replay evals

Replay evals are the default. They compare a run's persisted status and
stage outcomes against an embedded or explicit replay fixture.

### Clarifying-question evals

Clarifying-question evals assert that the agent called `ask_user(...)`
and asked the minimal question required to proceed. The run record
persists `ask_user` prompts, and the fixture can require a single
question plus term-level constraints:

```json
{
  "_type": "replay_fixture",
  "eval_kind": "clarifying_question",
  "expected_status": "completed",
  "clarifying_question": {
    "required_terms": ["repository"],
    "forbidden_terms": ["branch"],
    "min_questions": 1,
    "max_questions": 1
  }
}
```

Use this when defaults would be unsafe and the right behavior is to ask
the user before continuing.

## Determinism harness

Use `harn test --determinism` to assert that a pipeline replays the same
way on a second pass:

```bash
harn test --determinism tests/agent_loop.harn
```

The harness records once and replays once when no sibling
`<name>.llm-mock.jsonl` exists. If a sibling fixture is already
present, it replays both passes from that fixture. It compares stdout,
provider response payloads from `llm_transcript.jsonl`, and persisted
run-record structure to catch branching drift.

## Built-in assertions

These are available with no import at all, and `std/testing` re-exports them so
`import { assert_eq } from "std/testing"` works too:

| Function | Description |
|----------|-------------|
| `assert(condition, message?)` | Assert a condition is truthy |
| `assert_eq(actual, expected, message?)` | Assert two values are equal, with a structural diff on failure |
| `assert_ne(actual, expected, message?)` | Assert two values are not equal |
| `assert_approx(actual, expected, tolerance?, message?)` | Compare numbers within a tolerance (default `1e-9`) |
| `assert_matches(actual, pattern, message?)` | Assert text matches a regex; returns the text |
| `value_diff(actual, expected)` | The diff itself, as a string — or `nil` when the values are equal |

```harn
assert(x > 0, "x must be positive")
assert_eq(len(items), 3)
assert_approx(total, 0.3)
assert_matches(receipt.id, "^rcpt-\\d+$")
```

### Argument order

Every assertion that weighs a subject against an expectation takes **the
subject first**:

```harn
assert_eq(actual, expected)
assert_matches(actual, pattern)
assert_contains(haystack, needle)
```

This matters more than it looks. Swap the two values of an equality assertion
and it still passes and fails in exactly the same cases — it just labels the
two halves of every failure backwards, sending you to look for a bug in the
value that was right all along.

That hazard is the whole reason for the rule, and it is why
`assert_snapshot(name, actual)` is not an exception to it: a snapshot's
expectation lives in a file, so its leading argument is an identifier naming
*which* snapshot, not an expectation. A name and a value cannot be transposed,
so there is no backwards failure to print. Where the hazard cannot occur, the
rule has nothing to say.

### What a failure looks like

`assert_eq` does not print two values and leave you to compare them. It reports
each place they differ, addressed by path, and shows only those leaves:

```text
assert_eq failed: the two values differ in 2 places.

  at .user.name
    expected  "Grace"
    actual    "Ada"
    The strings first differ at character 0.

  at .user.roles[1]
    expected  "ops"
    actual    "dev"
    The strings first differ at character 0.
```

A path like `.user.roles[1]` is ordinary Harn access syntax, so you can paste
it straight back into your program to inspect the value.

Because values carry their type at runtime, the diff can tell apart things that
would otherwise render identically:

```text
assert_eq failed.
    expected  "1" (string)
    actual    1 (int)
    One side is a number and the other is text. If this came from parsed
    input, the conversion may be missing.
```

Types are named only when the two sides disagree, so the common case stays
quiet. A float mismatch points you at `assert_approx` rather than making you
rediscover that `0.1 + 0.2 != 0.3`.

Passing a `message` replaces the diff outright — it is a deliberate choice to
say something the diff cannot, so reach for it when you have real context to
add, not to restate what the values already show.

Large values are abbreviated in the middle (keeping both ends, which is where
strings usually differ), and a mismatch with more than ten differing leaves
reports the first ten and counts the rest.

Use `require` for runtime invariants in normal pipelines. The linter warns if
you use `assert*` outside test pipelines, and it suggests `assert*` instead of
`require` inside test pipelines.

## Line coverage

`harn test --coverage` reports per-file line coverage for the Harn source your
user test suite executes:

```bash
# Print a per-file coverage summary after the run
harn test tests/ --coverage

# Also write an LCOV tracefile (implies --coverage)
harn test tests/ --coverage --coverage-out coverage/lcov.info
```

The summary lists each executed source file with its instrumentable line count,
the number of lines covered, and the percentage, followed by a `TOTAL` row:

```text
Line coverage: 41/47 (87.2%)
File              Lines  Covered       %
tests/math.harn      18       18   100.0
src/util.harn        29       23    79.3
TOTAL                47       41    87.2
```

The `--coverage-out` tracefile is standard
[LCOV](https://github.com/linux-test-project/lcov), so it drops straight into
Codecov, `genhtml`, and the VS Code Coverage Gutters extension.

Notes:

- Coverage reuses the per-instruction source-line table the VM already carries,
  so it adds no separate instrumentation pass. Recording is opt-in; runs without
  `--coverage` pay nothing.
- The denominator is the set of distinct source lines that emit bytecode,
  including the bodies of functions that are loaded but never called (which
  therefore show as uncovered).
- Reporting is filtered to source files that exist on disk, so the embedded
  standard library and in-memory `eval` chunks are excluded.
- `--coverage` is for user test suites; it cannot be combined with `--watch`,
  `--determinism`, `--evals`, or the conformance / protocols targets.

## Cross-platform test coverage

Most workspace tests run on both Unix and Windows. A small set of test
modules opts out of Windows via `#![cfg(unix)]` because they exercise
POSIX-only semantics (`bash`-fixture process spawning, SIGTERM-driven
graceful shutdown). The full inventory and disposition lives at
[Windows test coverage](./dev/windows-test-coverage.md), and the nightly
`Windows nightly` GitHub Actions workflow runs the portable surface on
`windows-latest` so cross-platform regressions surface within 24 hours.
