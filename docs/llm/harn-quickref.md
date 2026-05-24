# Harn quick reference (LLM-friendly)

**Canonical URL:** <https://harnlang.com/docs/llm/harn-quickref.html>

This file is a one-pass reference optimized for LLM consumption and
grep. It covers the syntax, stdlib highlights, concurrency, and the
LLM / agent_loop surface an agent typically needs to write scripts.
You can fetch the hosted quick reference in any agent context that supports
HTTP fetches (Claude with `WebFetch`, Cursor's `@web`, Aider, etc.)
using the canonical URL above.

The human-facing companion lives at `docs/src/scripting-cheatsheet.md`.
Keep the two in lockstep when syntax changes.

For trigger manifests, connector contract v1, and the provider catalog, also
load `docs/llm/harn-triggers-quickref.md`.

## `--json` cheatsheet (agent-driven Harn)

Every machine-readable mode returns a versioned envelope:
`{ "schemaVersion": N, "ok": bool, "data": ..., "error": ..., "warnings": [] }`.
Stdout is one parseable JSON document (or one NDJSON event per line);
logs and progress always go to stderr.

- Discover supported commands and their current schema versions:
  `harn --json-schemas` (filter with `--command <name>`).
- Per-command shape reference: `docs/src/cli-json-contract.md`.
- Common pairs an agent will use:
  - `harn version --json` — build metadata (`name`, `version`, `description`).
  - `harn upgrade --check --json` — resolve target release without downloading.
  - `harn lint --json <path>` — structured lint diagnostics + summary; pair with
    `harn lint --fix <path>` (no `--json`) to apply the recommended edits.
  - `harn replay --json <run.json>` — per-stage replay summary + fixture verdict.
  - `harn check --json <path>` / `harn fmt --json <path>` — type-check and format
    reports with the same `CheckDiagnostic` shape.
  - `harn run --json script.harn` — NDJSON event stream (one envelope per line).
  - `harn doctor --json` — capability matrix for host / targets / providers.

## Files and execution

- File extension: `.harn`.
- Entry points:
  - Preferred capability-aware script entrypoint:
    `fn main(harness: Harness) { ... }`.
  - Workflow entrypoint: `pipeline default() { ... }` (pipeline mode —
    `compile_top_level_declarations` runs first, then the pipeline
    body).
  - Bare script with top-level statements for tiny one-off files.
- Run: `harn run script.harn`.
- Inline: `harn run -e 'log("hi")'`. The snippet is wrapped in
  `pipeline main(task) { ... }`; leading `import "..."` /
  `import { x } from "..."` / `pub import { x } from "..."` lines are
  hoisted out of the wrapper. The temp file lives in the current
  directory so relative imports (`import "./lib"`) and `harn.toml`
  discovery resolve against your project, e.g.
  `harn run -e $'import "./lib"\nlog(answer())'`. Imports must come
  first — interleaved imports are not lifted.
- Shebang: a `#!/usr/bin/env harn` line at byte offset 0 of a `.harn`
  file is skipped by the lexer, so executables on PATH can `chmod +x`
  scripts and run them directly.
- CLI arguments: `harn run script.harn -- a b c` exposes
  `argv: list<string>` as a global (`argv == ["a", "b", "c"]`).
- Exit code: any of three paths sets the process exit code.
  - `exit(code)` terminates immediately with that code.
  - `pipeline main()` (or any pipeline used as the entry) — the value
    flowing out of the body sets the exit code:
    - `return n: int` → exits `n` (clamped 0..=255).
    - `return Err(msg)` → writes `msg` to stderr, exits 1.
    - `return Ok(_)` / no explicit return → exits 0.
  - Uncaught errors exit with 1 and a rendered diagnostic.

## Merge captain eval loop

Use `harn merge-captain run` when iterating on the Merge Captain persona from a
single command. It resolves a backend, streams canonical agent JSONL, persists a
receipt, runs the Merge Captain oracle, and exits non-zero on unsafe action
attempts or any oracle error.

```bash
# Mock playground smoke path. Streams JSONL to stdout and writes a receipt under
# .harn-runs/merge-captain/<run-id>/receipt.json.
harn merge-captain run --backend mock examples/merge_captain/playground_3repos --once

# Keep stdout for the machine-readable summary and put the transcript/receipt in
# explicit files.
harn merge-captain run --backend mock examples/merge_captain/playground_3repos \
  --once \
  --model-route value/gemma \
  --timeout-tier smoke \
  --transcript-out .harn-runs/mc/event_log.jsonl \
  --receipt-out .harn-runs/mc/receipt.json

# Replay a deterministic transcript fixture through the same receipt + oracle
# path.
harn merge-captain run --backend replay \
  examples/personas/merge_captain/transcripts/green_pr.jsonl \
  --once --no-stdout

# Run the in-process fake GitHub/fake git golden-transition suite.
cargo test -p harn-cli --test merge_captain_cli issue_1012
```

Backends:

| Backend | Argument | Use |
|---|---|---|
| `mock` | playground directory or scenario manifest | Local fake-backend scenario loop. |
| `replay` | transcript JSONL file or event-log directory | Deterministic replay/audit without backend I/O. |
| `live` | none | Production connector runtime selector; fails closed when the connector runtime is unavailable. |

Flags:

| Flag | Use |
|---|---|
| `--once` / `--watch` | One sweep or finite watch mode (`--max-sweeps`, `--watch-backoff-ms`). |
| `--model-route ROUTE` | Pin the model/profile route in the receipt. |
| `--timeout-tier TIER` | Pin the timeout/budget tier in the receipt. |
| `--transcript-out PATH` | Write JSONL transcript to a file instead of stdout. |
| `--receipt-out PATH` | Write receipt JSON to an explicit path. |
| `--summary-out PATH` | Write run summary JSON to a file. |

Use `harn merge-captain ladder <manifest>` to run the same backend fixture
across a matrix of model routes and timeout tiers. The report records the first
route/tier that completed correctly, every degraded or looping tier, and paths
to each tier's JSONL transcript, receipt, and summary.

```bash
harn merge-captain ladder personas/merge_captain/harn.eval.toml \
  --report-out .harn-runs/merge-captain-ladder/report.json \
  --format json
```

The same ladder manifests can live inside eval packs, so `harn eval
personas/merge_captain/harn.eval.toml` and `harn test package --evals` use the
same runner and JSON artifact contract as host TUI/CLI surfaces.

Use `harn merge-captain iterate <manifest>` when an agent needs the brute-force
outer loop: scenarios × variants, where variants include model route, timeout
tier, Harn package revision, and prompt-asset revision metadata. The command
copies replay fixtures or materializes mock playgrounds into one iteration
directory, writes every run's JSONL transcript, receipt, and summary, then emits
`summary.json` plus a Markdown ranking table sorted by transcript-drift score
and cost.

```bash
harn merge-captain iterate examples/personas/merge_captain/iterations/smoke.toml \
  --report-out .harn-runs/merge-captain-iterations/latest.json \
  --markdown-out .harn-runs/merge-captain-iterations/latest.md

harn merge-captain iterate --diff \
  examples/personas/merge_captain/iterations/diff/baseline-summary.json \
  examples/personas/merge_captain/iterations/diff/candidate-summary.json
```

Iteration manifests are intentionally small:

```toml
version = 1
id = "merge-captain-local-loop"
base_dir = "."
artifact-root = ".harn-runs/merge-captain-iterations/local-loop"

[budget]
max-runs = 12
max-wallclock-ms = 30000
max-cost-usd = 0.01

[[scenarios]]
id = "single-green"
[scenarios.backend]
kind = "mock"
path = "examples/merge_captain/scenarios/single_green.json"

[[variants]]
id = "value-route-balanced"
model-route = "local/qwen-value"
timeout-tier = "balanced"
package-revision = "harn-package@workspace"
prompt-asset-revision = "merge-captain/prompts@v2"
max-tool-calls = 8
max-model-calls = 1
```

### Mock-repos playground (#1020)

`harn merge-captain mock` materializes a real on-disk sandbox — temp
git repos plus a fake GitHub HTTP server — so you can iterate on the
captain against real `git` codepaths without touching live
infrastructure. This is the recommended local iteration loop.

```bash
# 1. Create a playground from a built-in scenario. Default scenario is
#    `three_repo_basic`. List built-ins with `mock scenarios`.
harn merge-captain mock init ./pg --scenario three_repo_basic

# 2. Sweep the captain against it. The driver detects the on-disk
#    playground and synthesizes a canonical JSONL transcript reflecting
#    the live state.
harn merge-captain run --backend mock ./pg --once

# 3. Advance the scenario between sweeps — flip a check, advance base,
#    force-push as the author, merge a PR, etc. Steps come from the
#    scenario manifest; `--action <json>` is the one-off escape hatch.
harn merge-captain mock step ./pg --name gamma_force_push_fix
harn merge-captain mock step ./pg --action \
  '{"kind":"set_check","repo":"alpha","pr_number":101,"name":"ci","status":"completed","conclusion":"success"}'

# 4. Boot the fake GitHub HTTP server pointing at the playground state.
#    Real HTTP clients (e.g. harn-github-connector) talk to this; the
#    captain still uses real `git` against bare remotes under
#    ./pg/remotes/<repo>.git.
harn merge-captain mock serve ./pg --bind 127.0.0.1:0 --print-addr

# 5. Snapshot or tear down.
harn merge-captain mock status ./pg --json
harn merge-captain mock cleanup ./pg
```

Subcommands:

| Subcommand | Purpose |
|---|---|
| `mock init <dir>` | Materialize bare+working git repos + `state.json` from a scenario. `--scenario` (built-in) or `--manifest <path>` (custom JSON/YAML). `--force` cleans up first. |
| `mock step <dir>` | Apply a manifest-defined `--name <step>` or one-off `--action <json>`. Mutates `state.json` (and the bare remote when the action is `merge_pull_request`, `force_push_author`, or `advance_base`). |
| `mock status <dir>` | Print the current PR/check/history state. `--json` for machine output. |
| `mock serve <dir>` | Boot the fake GitHub HTTP server. Endpoints: `pulls`, `pulls/.../merge`, `pulls/.../files`, `commits/.../check-runs`, `actions/runs/.../logs`, `merge_queue/queues/...`, `issues`, `issues/.../comments`, `issues/.../labels`. |
| `mock cleanup <dir>` | Remove the playground. Idempotent and refuses to delete arbitrary directories without the playground marker. |
| `mock scenarios` | List built-in scenarios. |

Scenario manifests live at `examples/merge_captain/scenarios/*.json`
and follow the `merge_captain_playground_scenario` schema documented
in `crates/harn-vm/src/orchestration/playground/manifest.rs`.

## stdin / stdout / stderr / TTY

- Stdio capability calls route through `Harness`: use
  `harness.stdio.print(s)` / `harness.stdio.println(s)` for stdout,
  `harness.stdio.eprint(s)` / `harness.stdio.eprintln(s)` for stderr,
  and `harness.stdio.read_line()` / `harness.stdio.prompt(msg?)` for
  interactive input.
- `read_stdin()` slurps the rest of stdin to a `string` and returns `nil`
  at EOF.
- `is_stdin_tty()`, `is_stdout_tty()`, `is_stderr_tty()` — `bool`,
  uses `std::io::IsTerminal`. Use these to decide between rich
  interactive UI and pipe-friendly output.
- `std/io` exposes structured interactive helpers: `is_tty(fd?)`,
  `read_line({prompt?, timeout_ms?, trim?, echo?, raw?})`,
  `read_password(prompt?, timeout_ms?)`, and `write_stderr(text)`.
  Structured reads return `{ok, value?, status?, error?}` with statuses
  `ok`, `eof`, `timeout`, `interrupt`, or `error`.
- `set_color_mode("auto"|"always"|"never")` controls whether
  `color`/`bold`/`dim` emit ANSI. Auto honors `NO_COLOR` and
  `FORCE_COLOR` env vars and only emits when stdout is a TTY.

In tests: `mock_stdin(text)` / `unmock_stdin()`,
`mock_tty(stream, bool)` / `unmock_tty()`,
`capture_stderr_start()` / `capture_stderr_take()`.

For long terminal artifacts, import `std/tui`:

```harn
import { page, rule, terminal_width, clear } from "std/tui"

let result = page({title: "Audit", body: markdown, format: "markdown"})
```

`page(...)` uses `$PAGER` when stdout is a TTY, adds `-R -F -X` for `less`,
falls back to full print output when stdout is not interactive or the pager is
missing, and returns `{ok, paged, error?}`.

For interactive pickers, the same module exports
`select_from(items, opts?)` so harness scripts stop hand-rolling
`fzf` / `gum choose` detection. It returns `{ok, value, status}`,
auto-detects fzf then gum then falls back to a numbered `read_line`
menu, and honors `mock_stdin` under `prefer_external: "none"`.

## Command helpers (`std/command`)

Use `std/command` for script-side harness commands. It runs through the same
host command substrate as model-facing tools, but returns deterministic Harn
records for retries, artifacts, tails, classification, and recovery hints.

```harn,ignore
import { command_json, command_json_step, command_try } from "std/command"

let repo = command_json(["gh", "api", "repos/burin-labs/harn"], {
  capture: {max_inline_bytes: 65536},
})

let step = command_json_step("repo metadata", ["gh", "api", "repos/burin-labs/harn"], {
  retry: {max_attempts: 2, delay_ms: 0},
})

let fallback = command_try(
  [
    {source: "connector", run: fn() { return repos_get("burin-labs", "harn") }},
    {source: "cli", run: fn() { return command_json(["gh", "api", "repos/burin-labs/harn"]) }},
  ],
  {normalize: { value, source -> return {source: source, name: value.name} }},
)
```

- `command_json(spec, opts?)` parses stdout as JSON, returns `nil` for empty
  output only with `allow_empty: true`, and supports `result: "record"` for
  `{ok:false,error,step}` instead of throwing.
- `command_json_step(name, spec, opts?)` preserves `command_step` retry,
  classify, recovery, artifact, and attempt fields, then adds `json` or
  `parse_error`.
- `command_try(attempts, opts?)` is only for ordered equivalent probes. It adds
  `fallback_index`, `fallback_total`, and per-attempt summaries; it is not a
  retry system or provider framework.

## Time, sleep, monotonic clock

- `now_ms()` — wall-clock millis since UNIX_EPOCH (`int`).
- `monotonic_ms()` — monotonic millis since process start (`int`).
- `sleep(d)` / `sleep_ms(n)` — async sleep. **Mock-aware**: under
  `mock_time`, both advance the mocked clock instantly instead of
  blocking — so tests of retry/backoff/timeout logic stay
  deterministic and fast. The same mock is observed by `now_ms`,
  `monotonic_ms`, `timestamp`, `elapsed`, the trigger dispatcher, and
  the cron scheduler.
- `yield_now()` — cooperative scheduling primitive. Lets sibling
  `parallel each` / spawned tasks make progress without advancing time.
  Useful inside `mock_time(...)` blocks where you want one more poll
  cycle but no clock movement.
- `mock_time(ms)` / `advance_time(ms)` / `unmock_time()` — install,
  advance, and tear down the mock. The clock stack nests, so a Rust
  test harness can install an outer mock and a Harn pipeline can layer
  its own on top.

## Strings

```harn
let plain = "hello\n"
let interp = "Hello, ${name}!"
let multi = """
This is a triple-quoted multiline string.
It keeps line breaks verbatim and is the preferred way to declare
long system prompts in source code.
"""
let raw = r"C:\path\does\no\escapes"
```

Heredoc-style `<<TAG ... TAG` is **only** valid inside LLM tool-call
argument JSON. In source code, use `"""..."""`.

## Slicing

End-exclusive slicing works on strings and lists:

```harn
let s = "hello world"
log(s[0:5])        // "hello"
log(s[6:11])       // "world"

let xs = [1, 2, 3, 4, 5]
log(xs[1:4])       // [2, 3, 4]
```

`substring(s, start, length)` also exists — note the third argument
is a **length**, not an end index. Prefer the slice syntax to avoid
that footgun.

## Control flow: `if` is an expression

`if` / `else` produces a value. Bind it directly into `let`, pass it
to functions, or `return` it:

```harn
let body = if len(content) > 2400 {
  head_slice + "..." + tail_slice
} else {
  content
}

let grade = if score >= 90 { "A" } else if score >= 80 { "B" } else { "C" }
```

## Iteration

Harn loops are `for x in <iterable>`. Reach for destructuring and
stdlib helpers instead of integer-indexed loops — they read better and
avoid off-by-one bugs.

```harn
for x in items { ... }

// enumerate: yields a list of {index, value} dicts.
for {index, value} in items.enumerate() {
  log("${index}: ${value}")
}

// zip: yields [a, b] pairs — destructure with list pattern.
for [a, b] in xs.zip(ys) { ... }

// dict iteration: entries() yields [{key, value}, ...].
for {key, value} in my_dict.entries() { ... }

// Ranges:
let first_5 = range(5)         // [0, 1, 2, 3, 4] — half-open, Python-style
let middle  = range(3, 7)      // [3, 4, 5, 6]
let inc     = 1 to 5            // [1, 2, 3, 4, 5] — inclusive default
let exc     = 1 to 5 exclusive  // [1, 2, 3, 4]    — half-open
```

Note: `for (a, b) in ...` with parentheses is NOT supported — only list
patterns `[a, b]` and dict patterns `{name1, name2}`. Use `enumerate()`
/ `entries()` (dict-shaped) or `zip()` (list-shaped) accordingly.

## Streams

- Declare stream producers with `gen fn name(...) -> Stream<T> { ... }`.
- Emit one value with `emit expr`; `emit` is valid only inside `gen fn`.
- Consume with `for item in stream`, `.next()` (`{value, done}`), or `.iter()`.
- `Stream<T>` is distinct from `Generator<T>`; existing `yield` behavior is unchanged.
- Throws inside a stream propagate when the consumer pulls the next item.

```harn
gen fn numbers() -> Stream<int> {
  emit 1
  emit 2
}

for n in numbers() { log(n) }
```

`stream.*` works with any iterable source: lists, ranges, channels,
generators, and lazy `iter(...)` values. Operators are single-pass and lazy
unless the name is a sink such as `collect`, `fold`, or `first`.

```harn
// LLM token feed -> tap to log, then keep a bounded transcript.
let chunks = stream.collect(
  stream.tap(llm_stream_call("Summarize logs", nil, {provider: "mock"}), { chunk -> log(chunk.visible_delta) }),
  {max: 200}
)

// Parallel or channel results -> take the first three.
let first_three = stream.collect(stream.take(results_channel, 3), {max: 3})

// Agent events -> filter by topic.
let tool_events = stream.collect(
  stream.filter(agent_events, { ev -> ev?.topic == "tool_call" }),
  {max: 100}
)

// Two streams -> race; the first source to emit wins.
let winner = stream.first(stream.race(primary_stream, fallback_stream))

// Combine streams and fold to a result.
let total = stream.fold(
  stream.merge(worker_a, worker_b, worker_c),
  0,
  { acc, item -> acc + item.cost }
)
```

Common operators:

| Operator | Use |
|---|---|
| `stream.map(s, f)` / `stream.filter(s, pred)` / `stream.tap(s, f)` | Per-item transform, selection, side effects. |
| `stream.scan(s, seed, f)` / `stream.fold(s, seed, f)` | Running accumulator vs final accumulator. |
| `stream.collect(s, {max: N})` | Materialize with an explicit cap; exceeding it throws loudly. |
| `stream.take(s, n)` / `stream.take_until(s, pred)` / `stream.first(s)` | Bounded consumption and head lookup. |
| `stream.merge(...)` / `stream.interleave(...)` / `stream.zip(a, b)` / `stream.race(...)` / `stream.broadcast(s, n)` | Combine or fan out streams. |
| `stream.throttle(s, per_sec)` / `stream.debounce(s, window_ms)` | Basic emission pacing and burst coalescing. |

`llm_stream_call(prompt, system?, options?)` returns
`Stream<{delta, visible_delta, partial, role, finish_reason}>`. It accepts the
same options as `llm_call`; the `stream` option is still only the provider
transport toggle. Use `visible_delta` for UI rendering because it hides open
internal `<think>` blocks. Breaking out of consumption drops the stream and
cancels the background request.

## Module scope

Top-level `let` / `var` and `fn` declarations are visible inside
functions defined in the same file:

```harn
let GRADER_SYSTEM = """
You are a strict grader...
"""

pub fn grade_file(path) {
  // GRADER_SYSTEM is in scope here.
  return llm_call("...", GRADER_SYSTEM, { ... })
}
```

Top-level mutable `var` cross-fn mutation is not fully supported yet
(each function closure captures its own value copy). If you need
shared mutable state across functions, use atomics (`atomic(0)`,
`atomic_add`, `atomic_get`) or a channel.

## Attributes (`@name(...)`)

Declarative metadata on a top-level decl. Stack any number; each line
attaches to the **next** declaration. Args are literals only (no expr
evaluation).

```harn
@deprecated(since: "0.8", use: "compute_v2")
@test
pub fn compute(x: int) -> int { return x + 1 }
```

| Attr | Effect |
|---|---|
| `@deprecated(since: "X", use: "Y")` | Type-check warning at every call site (both args optional). |
| `@test` | Marks a `pipeline` as a test. `harn test` discovers it alongside the legacy `test_*` naming convention. |
| `@serial(group: "name")` | Test-scheduler hint: tests sharing the group are run serially under `--parallel`. Bare `@serial` shares a default group. |
| `@heavy(threads: N)` | Test-scheduler hint: the test reserves `N` worker permits under `--parallel` so it never oversubscribes the pool. |
| `@complexity(allow)` | Suppresses the `cyclomatic-complexity` lint warning on this fn. |
| `@invariant("fs.writes", "src/**")` | Checked only by `harn check --invariants`. Current built-ins: `fs.writes`, `budget.remaining`, `approval.reachability`. `harn explain --invariant <name> <handler> <file>` prints the violating CFG path. |
| `@acp_tool(name: "X", kind: "edit", side_effect_level: "mutation", ...)` | Compiles to `tool_define(...)` with the fn as the handler and the named args (minus `name`) lifted into `annotations`. `name` defaults to the fn name. |
| `@acp_skill(name: "X", when_to_use: "...", invocation: "explicit", ...)` | Compiles to `skill_define(...)` with the fn bound as the skill's `on_activate` hook. Named args (minus `name`) become skill-metadata fields. `name` defaults to the fn name. |

Unknown attribute names produce a type-checker warning (typo guard)
but don't break compilation. Attached to any non-decl statement is a
parse error.

## Typing: `any` vs `unknown` vs no annotation

Harn is gradually typed. Three levels of "I don't know the type yet":

| Annotation | Accepts any value in | Flows out to concrete types | Use when |
|---|---|---|---|
| *(omitted)* | yes | yes | Internal, unstable code you haven't typed yet. |
| `unknown` | yes | **no** — must narrow first | Untrusted boundaries: LLM responses, parsed JSON, dynamic dicts. |
| `any` | yes | yes (escape hatch) | Last resort. Prefer `unknown` unless you have a specific reason to defeat checking. |

Narrow `unknown` with `type_of(x) == "T"` or `schema_is(x, Shape)`:

```harn
fn handle(v: unknown) -> string {
  if type_of(v) == "string" { return "str:${v.upper()}" }  // v: string here
  if schema_is(v, MyShape) { return "shape:${v.name}" }    // v: MyShape here
  return "other"
}
```

`never` is the bottom type — expressions like `throw`, `return`,
`unreachable()`, and blocks that always exit infer to `never`. It's a
subtype of every type.

### Discriminated unions & distribution

Three discriminated-union surface forms, all check identically once
you've written them — pick whichever reads best at the call site.

**Pure literal unions.** No discriminant, no shape: just enumerate
the literal values. `match` covers them like an enum.

```harn,ignore
type Verdict = "pass" | "fail" | "unclear"

fn classify(v: Verdict) -> string {
  match v {
    "pass" -> { return "ok" }
    "fail" -> { return "no" }
    "unclear" -> { return "?" }
  }
}
```

**Tagged shape unions.** Two or more dict shapes joined by `|`. The
checker auto-detects the discriminant: a field that is non-optional
in every variant, has a literal type, and takes a distinct literal
value per variant. The field can be named anything — `kind`, `type`,
`op`, whatever fits the domain — there is no privileged spelling.

```harn,ignore
type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int}

fn handle(m: Msg) -> string {
  match m.kind {                             // narrows m per arm
    "ping" -> { return "ttl=" + to_string(m.ttl) }
    "pong" -> { return to_string(m.latency_ms) + "ms" }
  }
}

// Same narrowing works on `if`:
if m.kind == "ping" { /* m: {kind: "ping", ttl: int} */ }
else                { /* m: {kind: "pong", latency_ms: int} */ }
```

**Legacy `enum`.** Nominal variants with optional payload fields,
matched on `.variant`.

```harn,ignore
enum Action { Create, Edit, Delete }
match a.variant { "Create" -> { … } "Edit" -> { … } "Delete" -> { … } }
```

**`match` must be exhaustive.** Missing a variant is a hard error.
Add the missing arm or end with `_ -> { … }`. `if/elif/else` chains
stay intentionally partial; opt into exhaustiveness by ending the
chain with `unreachable("…")`.

**Or-patterns (`pat1 | pat2 -> body`)** let a single arm body cover
two or more alternatives, and each alternative counts toward
exhaustiveness. Inside the arm, the matched variable is narrowed to
the *union* of the alternatives' matches — on a tagged shape union
this is a sub-union, not a single variant:

```harn,ignore
match m.kind {
  "ping" | "pong" -> { /* m is {kind:"ping",…} | {kind:"pong",…} */ }
  "close"         -> { /* m is the close variant */ }
}
```

Or-pattern alternatives are restricted to literals (string, int,
float, bool, nil) and the wildcard `_`. Guards (`… if cond ->`) work
on or-pattern arms too.

**Generic aliases distribute over closed unions.** When you write
`Container<A | B>`, the checker expands it to
`Container<A> | Container<B>` so each instantiation fixes the type
parameter independently. This is what makes the TypeScript pain
around `(t: "create" | "edit") => void` not bite in Harn:

```harn,ignore
type Action = "create" | "edit"
type ActionContainer<T> = {action: T, process_action: fn(T) -> nil}

fn process_create(a: "create") { … }
fn process_edit(a: "edit")     { … }

let containers: list<ActionContainer<Action>> = [
  {action: "create", process_action: process_create},
  {action: "edit",   process_action: process_edit},
]
```

`ActionContainer<Action>` is `ActionContainer<"create"> |
ActionContainer<"edit">`, so the literal-tagged elements fit one
specific branch each — no contravariance grief.

### Intersection types (`A & B`)

`A & B` requires the value to satisfy *every* component, not just
one. The intersection of two shape types behaves like a dict that
has every field from each component, so both fields are accessible:

```harn,ignore
type BaseCtx = {request_id: string}
type AuthCtx = {user_id: string}

fn use_ctx(ctx: BaseCtx & AuthCtx) -> string {
  return ctx.request_id + "/" + ctx.user_id
}
```

`&` binds tighter than `|`, so `A & B | C` parses as `(A & B) | C`.
Inline shapes work too: `fn f(env: {region: string} & {tier: string})`.
Lowering: at runtime an intersection annotation becomes a JSON-Schema
`allOf` guard, so missing a field from any component triggers the
parameter-runtime check just like a single-shape mismatch.

### Variance (`in T` / `out T`)

User-declared generics default to **invariant**. Mark a type
parameter `out T` for covariance (T appears only in output position)
or `in T` for contravariance (T appears only in input position):

```harn,ignore
type Reader<out T> = fn() -> T
interface Sink<in T> { fn accept(v: T) -> int }
fn map<in A, out B>(value: A) -> B { ... }
```

Built-ins: `iter<T>` covariant; `list<T>` and `dict<K, V>` invariant
(mutable); `Result<T, E>` covariant in both. Function types are
**contravariant in parameters**, covariant in return — `fn(float)`
stands in for `fn(int)`, never the reverse. The numeric widening
`int <: float` is suppressed in invariant positions, so `list<int>`
does not flow into `list<float>`.

## Results and errors

`try { ... }` returns a `Result.Ok(value)` on success or
`Result.Err(value)` on thrown error. Unwrap with:

- `unwrap(r) -> T` — returns `T`, panics if `Err`.
- `unwrap_err(r) -> string` — returns the error message, panics if
  `Ok`.
- `r?.field` — optional chaining that returns `nil` on `Err`.

```harn
let r = try { llm_call("hi", nil, opts) }
let text = r?.text ?? "no response"
```

`try { body } catch (e) { handler }` is also an expression: its value is
the body tail on success or the handler tail on a caught throw. A typed
catch that doesn't match the thrown type rethrows past the expression. A
trailing `finally { ... }` runs once for effect only.

```harn
let parsed = try { json_parse(raw) } catch (e) { default_config() }
```

`try* EXPR` (prefix) evaluates `EXPR` and rethrows any throw so an
enclosing `try { ... } catch (e) { ... }` sees it. Use it instead of
the verbose `try { foo() } / guard is_ok else / unwrap` boilerplate:

```harn
fn fetch(prompt) {
  // Without try*: try { llm_call(prompt) } / guard is_ok / unwrap
  let response = try* llm_call(prompt)
  return parse(response)
}

let outcome = try {
  fetch(user_prompt)
} catch (e: ApiError) {
  fallback(e)
}
```

`try*` requires an enclosing function (`fn`, `tool`, or `pipeline`) so
the rethrow has somewhere to live; it's a compile error at the module
top level. It's distinct from postfix `?`: `?` early-returns
`Result.Err(...)` from a `Result`-returning function, while `try*`
rethrows a thrown value into an enclosing catch.

## JSON querying

Use `json_pointer(value, ptr)` for RFC 6901 paths such as
`/users/0/email`; escaping is `~0` for `~` and `~1` for `/`. Missing
paths return `nil`. `json_pointer_set(value, ptr, new)` and
`json_pointer_delete(value, ptr)` return modified copies.

Use `jq(value, expr)` for a jq-like stream query; it always returns a
list. Use `jq_first(value, expr)` when you expect one value or `nil`.
Supported v1 forms include `.`, `.foo.bar`, `.[2]`, `.[2:5]`,
`.[]`, `.["quoted key"]`, pipes, commas, `length`, `keys`,
`values`, `type`, `map(...)`, `select(...)`, boolean comparisons,
object construction, and recursive descent `..`.

```harn
let api = json_parse(response.body)
let first_email = json_pointer(api, "/users/0/email")
let active = jq(api, ".users[] | select(.active == true) | .email")
let summary = jq_first(api, "{ count: .users | length, next: .meta.next }")
```

## Concurrency

```harn
// Spawn a background task.
let h = spawn { long_work() }
let value = await(h)

// parallel each: concurrent map.
let results = parallel each paths { p -> process(p) }

// parallel settle: like `each` but collects per-item Ok/Err.
let outcome = parallel settle paths { p -> grade(p) }
log(outcome.succeeded)  // count
log(outcome.failed)
for r in outcome.results {
  // r is Result.Ok(...) or Result.Err(...)
}

// parallel N: fan-out with an index.
let indices = parallel 8 { i -> fetch(i) }

// Cap in-flight work to avoid overwhelming downstream services.
let results = parallel settle paths with { max_concurrent: 4 } { p ->
  llm_call(p, nil, opts)
}
```

`max_concurrent: 0` (or no `with` clause) means unlimited. See also
`retry { } catch err { }`, channels, `select`, and `deadline` in
`docs/src/concurrency.md`.

## Iteration & lazy iterators

Eager collection methods (`list.map`, `list.filter`, `list.flat_map`,
`dict.map_values`, `dict.filter`, set/string equivalents, `.reduce`,
`.find`, `.any`, `.all`, etc.) still return eager collections. Nothing
about those has changed — use them when you just want a list/dict back.

Lazy iteration is opt-in via `.iter()`:

```harn
let xs = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
let first_three_doubled_evens = xs
  .iter()
  .filter({ x -> x % 2 == 0 })
  .map({ x -> x * 2 })
  .take(3)
  .to_list()
// [4, 8, 12]
```

`.iter()` lifts a list/dict/set/string/generator/channel into
`Iter<T>` — a lazy, single-pass, fused iterator. Combinators chain by
returning a new `Iter`. Sinks drain the iter and return an eager value.

### Lazy combinators (`Iter<T> -> Iter<...>`)

`.map(f)`, `.filter(p)`, `.flat_map(f)`, `.take(n)`, `.skip(n)`,
`.take_while(p)`, `.skip_while(p)`, `.zip(other)`, `.enumerate()`,
`.chain(other)`, `.chunks(n)`, `.windows(n)`, `.iter()` (no-op on an
iter). `iter(x)` is also available as a free builtin.

### Sinks (drain, return eager value)

`.to_list()`, `.to_set()`, `.to_dict()` (requires `Pair` items),
`.count()`, `.sum()`, `.min()`, `.max()`, `.reduce(init, f)`,
`.first()`, `.last()`, `.any(p)`, `.all(p)`, `.find(p)`,
`.for_each(f)`.

### Dict iteration and `Pair`

`.iter()` on a dict yields `Pair(key, value)` values — **not**
`{key, value}` dicts. Access with `.first` / `.second`, or destructure
in a for-loop:

```harn
for (k, v) in {a: 1, b: 2}.iter() {
  log("${k}: ${v}")
}
```

A direct `for entry in some_dict` still yields `{key, value}` dicts
(back-compat). A `pair(a, b)` builtin exists for constructing pairs
explicitly; `.zip` and `.enumerate` also emit pairs.

### Semantics

- **Lazy**: nothing runs until a sink (or for-loop) pulls values.
- **Single-pass, fused**: once exhausted, stays exhausted. Call
  `.iter()` again on the source to restart.
- **Snapshot**: the iter `Rc`-clones the backing collection, so
  mutating the source after `.iter()` doesn't affect the iter.
- **String iteration**: yields chars (Unicode scalar values), not
  graphemes.
- **Printing**: `log(it)` renders `<iter>` or `<iter (exhausted)>`
  without draining.

### Ranges and iters

`Range` (from `a to b` / `range(n)`) is its own value type with O(1)
`.len() / .first() / .last() / .contains(x)` and `r[k]` subscript —
no materialization. Calling any lazy combinator on a Range
(`.map / .filter / .flat_map / .take / .skip / .take_while /
.skip_while / .zip / .enumerate / .chain / .chunks / .windows`)
returns a lazy `iter`. Sinks (`.to_list / .sum / .reduce / ...`)
drain through the iter. In short: Range handles integer ranges
with O(1) ops; Iter handles arbitrary lazy sequences. Chaining
`(1 to 10_000_000).map(...).take(5).to_list()` finishes instantly
because only 5 elements flow through the pipeline.

## Regex

```harn
let matches  = regex_match("[0-9]+", "abc 42 def 7")   // ["42", "7"] or nil
let swapped  = regex_replace("(\\w+)\\s(\\w+)", "$2 $1", "hello world")
//           -> "world hello"
let same     = regex_replace_all("(\\w+)\\s(\\w+)", "$2 $1", "hello world")
//           -> alias of regex_replace; every match replaced.
let captures = regex_captures("(?P<day>[A-Z][a-z]+)", "Mon Tue")
let words    = regex_split("a, b, c", ",\\s*")
let ci       = regex_match("hello", "HeLLo", "i")
let fixed_ci = regex_replace("hello", "hi", "HeLLo", "i")
```

`regex_replace` and `regex_replace_all` both replace every match and
both support `$1`, `$2`, `${name}` backrefs plus the same optional
`i`/`m`/`s`/`x` flags as `regex_match`.

## Encoding, bytes, and compression

Use byte helpers when content may not be UTF-8:

```harn
let bytes = bytes_from_string("hello")
let text = bytes_to_string(bytes)
let hex = bytes_to_hex(bytes)
let same = bytes_from_hex(hex)
```

Compression is in-memory and returns `bytes`. Encoders accept `bytes`
or `string`; decoders always return `bytes`.

```harn
let gz = gzip_encode("hello", 6)       // level 0..9, default 6
let zst = zstd_encode(bytes, 3)        // zstd level, default 3
let br = brotli_encode("hello", 11)    // quality 0..11, default 11

let hello = bytes_to_string(gzip_decode(gz))

let tar = tar_create([
  {path: "README.md", content: "# Hi\n", mode: 420},
])
let tar_entries = tar_extract(tar)     // [{path, content: bytes, mode}]

let zip = zip_create([{path: "a.txt", content: "alpha"}])
let zip_entries = zip_extract(zip)     // [{path, content: bytes}]
```

## Scripting helpers

```harn
let rng = rng_seed(42)
let roll = random_int(rng, 1, 6)
let shuffled = random_shuffle(rng, [1, 2, 3, 4])
let grouped = group_by(["a", "bb", "c"], { s -> len(s) })
let parts = partition([1, 2, 3, 4], { x -> x % 2 == 0 })
let padded = str_pad("é", 3, ".", "both")
let graphemes = unicode_graphemes("éx")
let parsed = uuid_parse(uuid_v7())
```

## LLM surface

```harn
let response = llm_call(prompt, system, options)
log(response.prose)          // unwrapped prose (text minus tags)
log(response.text)           // raw provider text (may include tags)
log(response.canonical_text) // canonical tagged reconstruction
log(response.input_tokens)
log(response.output_tokens)
log(response.logprobs)       // present when requested and returned
```

### `llm_call` options

| Option | Type | Default | Notes |
|---|---|---|---|
| `provider` | string | `"auto"` | Explicit provider wins. `"auto"` infers from `model`; see the resolution table below. |
| `model` | string | (inferred) | `local:gemma-4-e4b-it` strips the `local:` transport prefix and routes through Ollama. |
| `max_tokens` | int | 16384 | |
| `temperature` | float | provider default | |
| `logprobs` | bool | false | Request token log probabilities when the selected provider route supports them. |
| `top_logprobs` | int | nil | Request top alternative token log probabilities where supported. |
| `tools` | list | nil | Registered tool schemas. |
| `reasoning_policy` / `thinking_policy` | string \| bool | nil | Provider-aware reasoning policy for direct calls. Values: `auto`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`; `none`, `disabled`, `no_think`, and `nothink` alias to `off`. Explicit `thinking` or `reasoning_effort` wins. |
| `reasoning_scale` / `problem_scale` | string | `"medium"` | Scale hint for `reasoning_policy: "auto"`: `small`, `medium`, or `large`. |
| `reasoning_task` | string | inferred | Task hint for `reasoning_policy: "auto"`: `chat`, `agent`, `code`, `verify`, or `summarize`. |
| `thinking` | bool \| dict | nil | Typed provider reasoning. `true` / `{mode: "enabled"}` automatically sends Anthropic's `interleaved-thinking-2025-05-14` beta header on supported Claude Opus models. `thinking: false` on Qwen3 routes auto-prepends `/no_think` to the system message (capability-driven; no per-template knowledge needed in scripts). |
| `interleaved_thinking` | bool | false | Force the Anthropic interleaved-thinking beta header for the call/loop. |
| `anthropic_beta_features` | string \| list | nil | Extra Anthropic beta feature names for the comma-separated `anthropic-beta` header. |
| `tool_search` | bool \| string \| dict | nil | Engage progressive tool disclosure. Shorthand `"bm25"` / `"regex"` / `"hybrid"` (variant, mode auto). Dict: `{variant: "bm25" \| "regex" \| "hybrid", mode: "auto" \| "native" \| "client", strategy: "bm25" \| "regex" \| "hybrid" \| closure \| {handler}, always_loaded: [string], budget_tokens: int, name: string, include_stub_listing: bool}`. See "Tool loading & search" below. |
| `output_format` | dict \| string | `{kind: "text"}` | Provider-agnostic output shape. Dicts: `{kind: "json_schema", schema: {...}, strict: true}`, `{kind: "json_object"}`, `{kind: "text"}`. Strings: `"json_schema"`, `"json_object"`/`"json"`, `"text"`. |
| `response_format` | string | nil | Legacy alias. `"json"` maps to `output_format: {kind: "json_object"}` unless `json_schema`/`schema` is also supplied, in which case it maps to `kind: "json_schema"`. |
| `json_schema` | dict | nil | Legacy alias for `output_format.schema` and `output_schema`. Prefer `output_format`. |
| `output_schema` | `Schema<T>` (dict \| type-alias) | nil | JSON-schema-shaped dict, or a top-level `type T = ...` alias (compiler lowers to the schema dict). The generic parameter `T` flows into the narrowed `r.data: T`. Validated after parse. |
| `output_validation` | string | `"off"` | `"error"` throws on mismatch; `"warn"` logs. |
| `schema_retries` | int | 1 | When validation fails, re-prompt up to N times with a corrective user turn. Each retry is a single-turn correction — the invalid response is NOT persisted; the original messages are replayed with one appended user-role correction citing the validation errors + schema. Works alongside `output_validation: "error"`. |
| `schema_retry_nudge` | string \| bool | auto | String = verbatim corrective message (+ validation errors appended). `true` = auto nudge from schema required/properties keys. `false` = bare retry — replays the original messages unchanged, no correction appended. |
| `schema_stream_abort` | bool | `true` when `output_schema` is set | While streaming, feed each visible text delta through an incremental JSON validator. The moment the partial response can no longer satisfy `output_schema`, the provider stream is aborted, a `schema_stream_aborted` transcript event fires, and the abort consumes one `schema_retries` slot — the retry replays the prompt with a corrective nudge that cites the failing JSON path and reason. Opt out with `false` to wait for the whole malformed response (post-hoc validator still runs). |
| `llm_retries` | int | 0 | (deprecated; see `with_retry`) Retries on transient HTTP / provider errors. Raw `llm_call` is fail-fast by default; set to N to allow N retries after the first attempt. Note off-by-one: `llm_retries: 3` ≈ `with_retry(..., {max_attempts: 4})`. |
| `llm_backoff_ms` | int | 250 | (deprecated; see `with_retry`) Base exponential backoff in milliseconds. |
| `llm_caller` | closure | nil | (`agent_loop` only) Custom caller wrapping the per-turn `llm_call`. See "Composable LLM callers" below. |
| `tool_caller` | closure | nil | (`agent_loop` only) Custom caller wrapping every tool dispatch. Signature `fn(call, next) -> result_dict`. See "Composable tool middleware" below. |
| `max_concurrent_tools` | int | 1 | (`agent_loop` only) When a planner emits N tool calls in one turn, dispatch siblings concurrently capped at this count. Results inject in source order regardless of completion order. Middleware-backed dispatch uses a fresh caller chain per sibling, so `audit.layers` histories never cross-talk. With `with_audit_log`, each receipt carries an `emit_order` field so consumers can sort completion-ordered events back to source order. |
| `prefetch_next_turn` | bool | false | (`agent_loop` only) Start the next planner turn as soon as tool results have been recorded, before non-result audit sinks such as local receipt writes or custom receipt callbacks finish. Background audit flushes are drained before the loop returns. |
| `progress_tool` | bool \| dict | false | (`agent_loop` only) Expose an opt-in progress-reporting tool. `true` installs `agent_progress`; dict form may set `name`, `description`, and `system_prompt_nudge`. ACP clients receive entries as canonical `plan` updates; A2A clients receive non-terminal `working` status updates; message-only reports surface as Harn progress narration. |
| `stream` | bool | true | SSE streaming transport. |

Provider auto-resolution precedence:

1. Explicit `provider` option other than `"auto"` wins.
2. `provider: "auto"` with a `model` infers from the model selector.
3. If `provider` is omitted, `HARN_LLM_PROVIDER` wins when set; otherwise a `model` infers the provider.
4. Unknown model IDs fall back to `HARN_DEFAULT_PROVIDER`, then the
   configured default provider (`anthropic` in the built-in catalog),
   and emit a warning.

| Model selector | Provider | Model sent to provider |
|---|---|---|
| `local:<model>` | `ollama` | `<model>` |
| `ollama:<model>` | `ollama` | `<model>` |
| `<org>/<model>` (one slash) | `openrouter` | unchanged |
| `claude-*` | `anthropic` | unchanged |
| `gpt-*`, `o1*`, `o3*`, `o4*` | `openai` | unchanged |
| `gemini-*` | `gemini` | unchanged |
| `<model>:<tag>` | `ollama` | unchanged |
| anything else | `HARN_DEFAULT_PROVIDER` / configured default | unchanged |

Native Gemini routes use Google's `generateContent` wire format directly:
tool schemas become `functionDeclarations`, model tool requests are
`functionCall` parts, tool observations are `functionResponse` parts, and
JSON schemas lower to Gemini's JSON response controls. Vertex AI also serves
Gemini models through `generateContent`, but keeps Google Cloud project /
location and OAuth/service-account authentication. OpenAI-compatible Gemini
routes such as OpenRouter remain OpenAI-wire routes and use OpenAI-style
`tools`, `tool_calls`, and structured-output parameters.

### Reranking and self-certainty

```harn
import { pairwise_rerank, self_certainty } from "std/llm/rerank"

let ranked = pairwise_rerank(candidates, {
  task: "Pick the most relevant search result.",
  criteria: "Prefer primary sources with direct evidence.",
  provider: "mock",
})

let confidence = self_certainty(
  "ignored",
  {logprobs: [{token: "answer", logprob: -0.1}]},
)
```

`pairwise_rerank` returns `{ranked, scores, comparisons}` using `O(n log n)`
pairwise judge calls, or a deterministic `compare(left, right, ctx)` callback
when supplied. `self_certainty` scores supplied/result `logprobs`, or makes an
extra repeat-exactly model call with `logprobs: true`; live support depends on
the provider returning OpenAI-compatible or legacy completion logprob records.

### Tool executor declarations

Every `tool_define(...)` registration declares **how the tool is
dispatched**. The runtime uses this to decide where the call runs and
to tag ACP `tool_call_update.executor` events so clients can render
"via host bridge" / "via mcp:linear" badges.

| `executor` value | Required companion field | Where it dispatches |
|---|---|---|
| `"harn"` *(or `"harn_builtin"` alias)* | `handler` (a closure) | In-VM via the registered handler. The VM stdlib short-circuits `read_file` / `list_directory` even without a handler. |
| `"host_bridge"` | `host_capability: "cap.op"` | Through the host shell's `builtin_call` bridge (Swift IDE bridge, BurinApp, BurinCLI). `harn check` validates the binding against the host capability manifest when one is configured. |
| `"mcp_server"` | `mcp_server: "<server_name>"` | Through the configured MCP server. Tools sourced from `mcp_list_tools` carry the `_mcp_server` annotation and don't need the explicit declaration. |
| `"provider_native"` | *(none)* | Provider-side (e.g. OpenAI Responses API server tools). The runtime never dispatches these locally — the model returns the already-executed result inline. |

```harn
// Harn handler (default when `handler` is present and `executor` is
// omitted — back-compat path).
registry = tool_define(registry, "look", "Read files", {
  parameters: {path: "string"},
  handler: { args -> read_file(args.path) },
})

// Host-bridge tool — handler-less by design.
registry = tool_define(registry, "ask_user", "Ask the user", {
  parameters: {prompt: "string"},
  executor: "host_bridge",
  host_capability: "interaction.ask",
})

// MCP-served tool with explicit server binding.
registry = tool_define(registry, "github_search", "Search issues", {
  parameters: {query: "string"},
  executor: "mcp_server",
  mcp_server: "github",
})

// Provider-native — runtime never dispatches.
registry = tool_define(registry, "tool_search", "...", {
  parameters: {query: "string"},
  executor: "provider_native",
})
```

`tool_define` rejects invalid combinations at definition time, and
`agent_loop` refuses to start if the registry contains a tool with no
executable backend. The historical `[builtin_call] unhandled: <name>`
runtime failure is replaced by a clear error pointing at the offending
tool.

### Experimental MCP file inputs

Harn implements draft MCP SEP-2356 file inputs behind an explicit opt-in. The
wire shape is `x-mcp-file` on a `{"type": "string", "format": "uri"}` schema
property, with selected file bytes sent inline as an RFC 2397 `data:` URI.

```harn
harn.mcp.configure({
  experimental: {file_upload: {spec_revision: "modelcontextprotocol/modelcontextprotocol#2356"}},
})

let image = harn.mcp.upload_file(mcp.image_server, "photo.png", {
  accept: ["image/png", "image/jpeg"],
  max_size: 5242880,
})
mcp_call(mcp.image_server, "describe_image", {image: image})

registry = tool_define(registry, "inspect_upload", "Inspect text", {
  parameters: {upload: harn.mcp.file_input({accept: ["text/*"], max_size: 64})},
  handler: { args -> "received" },
})
```

Keep this in the experimental namespace until upstream ratifies file inputs;
large files should still use URL-mode elicitation or an app-provided upload UI.

### Tool loading & search

Mark tools that the model rarely needs with `defer_loading: true` and
opt the call into progressive disclosure with `tool_search: "bm25"`:

```harn
var registry = tool_registry()
registry = tool_define(registry, "look", "Read files", {
  parameters: {path: {type: "string"}},
  handler: { args -> read_file(args.path) },
})
registry = tool_define(registry, "deploy", "Deploy to production", {
  parameters: {env: {type: "string"}},
  defer_loading: true,                 // schema held back until searched
  handler: { args -> shell("deploy " + args.env) },
})

let r = llm_call(prompt, sys, {
  provider: "anthropic",
  model: "claude-opus-4-7",
  tools: registry,
  tool_search: "bm25",                 // or "regex" / "hybrid"
})
```

Provider support matrix for `tool_search`:

| Provider | Native | Client fallback |
|---|---|---|
| Anthropic — Opus/Sonnet 4.0+, Haiku 4.5+ | ✓ (`bm25`, `regex`) | ✓ |
| Anthropic — pre-4.0 / other Claude | ✗ | ✓ |
| OpenAI — GPT 5.4+ (Responses API, hosted) | ✓ (`tool_search`) | ✓ |
| OpenAI — pre-5.4 (`gpt-4o`, `gpt-4.1`, older) | ✗ | ✓ |
| OpenRouter, Together, Groq, DeepSeek, Fireworks, HuggingFace, local vLLM | ✓ when model matches `gpt-5.4+` upstream | ✓ |
| Gemini, Ollama, others | ✗ | ✓ |

Semantics:

- `defer_loading: true` on an individual tool keeps its schema out of
  the model's context until a tool-search call surfaces it. On
  capable Anthropic models the schema goes into the API prefix but not
  the model's context, so prompt caching stays warm. On OpenAI GPT
  5.4+ the wrapper-level flag rides alongside the `{"type":
  "tool_search"}` meta-tool in the tools array.
- `tool_search: "bm25"` prepends the server-side
  `tool_search_tool_bm25_20251119` meta-tool on capable Anthropic
  models, or `{"type": "tool_search", "mode": "hosted"}` on GPT 5.4+
  via the Responses API. On any other provider, Harn falls back to a
  client-executed equivalent: a synthetic `__harn_tool_search` tool
  whose handler runs BM25/regex/hybrid or a custom Harn scorer, then
  promotes the matching deferred tools into subsequent turns' schema
  list.
- `tool_search: "regex"` uses the Python-regex variant
  (`tool_search_tool_regex_20251119`) on Anthropic, or an
  in-VM case-insensitive Rust-regex search on everything else.
- `tool_search: {mode: "native"}` refuses to silently downgrade —
  errors if the provider isn't natively capable.
- `tool_search: {mode: "client"}` forces the client-executed path
  even on providers with native support (useful for debuggability on
  GPT 5.4+, where the hosted path hides search deltas in the usage
  accounting).
- `tool_search: {strategy: "bm25" | "regex" | "hybrid" | scorer}`
  (client mode only) picks the implementation. A scorer can be a Harn
  closure or `{handler: closure, name?: string}` and may call embeddings,
  host-backed tools, MCP tools, or project-specific indexes.
- `tool_search: {budget_tokens: N}` caps the total token footprint
  of client-mode promoted tool schemas; oldest-first eviction when
  exceeded.
- `tool_search: {name: "find_tool"}` renames the synthetic search
  tool (default `__harn_tool_search`).
- `tool_search: {include_stub_listing: true}` appends a short list
  of deferred tool names to the contract prompt.
- `namespace: "ops"` on a `tool_define(...)` call groups deferred
  tools for OpenAI's `tool_search` meta-tool. The distinct set of
  namespaces is collected into the meta-tool's `namespaces` field;
  Anthropic ignores the label (harmless passthrough).
- Escape hatch for proxied OpenAI-compat endpoints whose model ID
  Harn cannot parse: pass `{<provider_name>:
  {force_native_tool_search: true}}` on the call options. Asserts
  the endpoint forwards `tool_search` + `defer_loading` unchanged and
  opts into the hosted path regardless of model detection.
- Pre-flight: at least one user tool must be non-deferred, matching
  Anthropic's 400 on all-deferred tool lists.
- Transcript events: `tool_search_query` and `tool_search_result`
  blocks appear in the run record so replay / eval can see which tools
  got promoted and when. Client-mode events carry a
  `metadata.mode: "client"` tag so replayers can distinguish the two
  paths; otherwise the shapes are identical. OpenAI hosted mode emits
  the same block shapes from the wire `tool_search_call` and
  `tool_search_output` entries in the response.

### Provider capabilities (data-driven matrix)

The per-provider / per-model capability surface lives in a shipped
TOML table (`crates/harn-vm/src/llm/capabilities.toml`), overridable
per-project via `[[capabilities.provider.<name>]]` in `harn.toml`:

```toml
# harn.toml
[[capabilities.provider.my-proxy]]
model_match = "*"
native_tools = true
preferred_tool_format = "native"
tool_mode_parity = "unknown"
tool_search = ["hosted"]
thinking_modes = ["effort"]
```

Query the effective matrix at runtime:

```harn
let caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   provider: "anthropic", model: "claude-opus-4-7",
//   native_tools: true, text_tool_wire_format_supported: true,
//   preferred_tool_format: "native", tool_mode_parity: "unknown",
//   tools: true, defer_loading: true,
//   tool_search: ["bm25", "regex"], max_tools: 10000,
//   prompt_caching: true, thinking: true,
//   thinking_modes: ["adaptive"],
//   requires_completion_tokens: false,
//   reasoning_effort_supported: false,
//   interleaved_thinking_supported: true,
//   message_wire_format: "anthropic",
//   native_tool_wire_format: "anthropic",
//   prefers_xml_scaffolding: true,
//   structured_output_mode: "xml_tagged",
//   supports_assistant_prefill: false,
//   prefers_xml_tools: true,
//   thinking_block_style: "thinking_blocks",
// }

// `caps.tools` matches Harn's own tool gate: true when the route can call
// tools via either the native API wire shape or Harn's text wire format.
// Inspect `native_tools` or `text_tool_wire_format_supported` directly when
// you need to distinguish. Presets use `preferred_tool_format` when it is
// present, so known native/text divergences stay data-driven.

if "bm25" in caps.tool_search {
  // opt into progressive disclosure
}
```

Additional helpers:

- `provider_capabilities_install(toml_src)` — install overrides from
  a TOML string (same layout as the shipped table). Useful for
  scripts that detect a proxied endpoint at runtime without editing
  `harn.toml`.
- `provider_capabilities_clear()` — revert to the shipped defaults.

Rule schema (per `[[provider.<name>]]` entry). Shared defaults can also be
set under `[provider_defaults.<name>]`:

| Field | Type | Purpose |
|---|---|---|
| `model_match` | glob string | Required. Matched against lowercased model ID. |
| `version_min` | `[major, minor]` | Optional lower bound; parsed via Claude / GPT version extractors. |
| `native_tools` | bool | Native tool-call wire shape supported. |
| `text_tool_wire_format_supported` | bool | Harn text-tool contract supported. |
| `preferred_tool_format` | string | Default preset tool mode: `native` or `text`. |
| `tool_mode_parity` | string | Native/text interchangeability status: `interchangeable`, `unknown`, `native_unreliable`, `text_unreliable`, `native_only`, `text_only`, or `unsupported`. |
| `tool_mode_parity_notes` | string | Optional explanation for known non-interchangeable routes. |
| `message_wire_format` | string | Shared request/response message format: `openai`, `anthropic`, `gemini`, or `ollama`. |
| `native_tool_wire_format` | string | Native tool definition shape for shared helpers: `openai` or `anthropic`. Gemini/Vertex adapters emit Google `functionDeclarations` from canonical tool definitions. |
| `defer_loading` | bool | Provider honors `defer_loading: true` on tool defs. |
| `tool_search` | `[string]` | Native variants (`["bm25", "regex"]` or `["hosted", "client"]`). Empty = no native support. |
| `max_tools` | int | Cap on tool count (used by `harn lint`). |
| `prompt_caching` | bool | `cache_control` blocks honored. |
| `prefers_xml_scaffolding` | bool | Prompt sections prefer XML tags such as `<task>` / `<examples>`. |
| `prefers_markdown_scaffolding` | bool | Prompt sections prefer Markdown headings such as `## Task`. |
| `structured_output_mode` | string | Preferred logical output shape: `native_json`, `delimited`, `xml_tagged`, or `none`. |
| `supports_assistant_prefill` | bool | Assistant-role prefill turns are accepted. |
| `prefers_role_developer` | bool | Durable instructions should use OpenAI's `developer` role. |
| `prefers_xml_tools` | bool | Text-rendered tool specs should use XML wrappers. |
| `thinking_block_style` | string | Preferred thinking representation: `none`, `thinking_blocks`, `reasoning_summary`, or `inline`. |
| `thinking_modes` | `[string]` | Supported script-facing modes: `enabled`, `adaptive`, `effort`. |
| `reasoning_wire_format` | string | Non-standard OpenAI-compatible reasoning shape: `openrouter` or `enabled`. |
| `requires_completion_tokens` | bool | Use OpenAI `max_completion_tokens` instead of `max_tokens`. |
| `reasoning_effort_supported` | bool | Provider/model accepts OpenAI `reasoning_effort`. |
| `interleaved_thinking_supported` | bool | `thinking: true` can request Anthropic's interleaved-thinking beta header. |
| `anthropic_beta_features` | `[string]` | Anthropic beta feature names always requested for this route. |
| `image_url_input_supported` | bool | Image content may use remote URLs. Set false for base64-only routes. |
| `file_upload_wire_format` | string | Upload API family used by `files.upload`: `anthropic` or `gemini`. |
| `seed_supported`, `top_k_supported`, `frequency_penalty_supported`, `presence_penalty_supported` | bool | Generation option support flags. |
| `thinking_disable_directive` | string | In-prompt directive (e.g. `"/no_think"` for Qwen3) auto-prepended to system when `thinking: false`. Idempotent. |

First match wins within a provider's rule list. `[provider_family]`
declares siblings that inherit a canonical family's rules
(OpenRouter → `openai`, etc.).

### Skills (bundled tool + prompt + MCP metadata)

Use `skill NAME { ... }` to declare a named skill: metadata, a tool
registry reference, MCP server names, a system-prompt fragment, and
optional lifecycle hooks that run on activate/deactivate. Each body
entry is `<field_name> <expression>` — unreserved identifiers, regular
expressions as values. The decl lowers to `skill_define(skill_registry(), NAME, { ... })`
and binds the result to `NAME`.

```harn
pub skill deploy {
  description "Deploy the application to production"
  when_to_use "User says deploy/ship/release"
  invocation "explicit"           // "auto" | "explicit" | "both"
  paths ["infra/**", "Dockerfile"]
  allowed_tools ["bash", "git"]
  model "claude-opus-4-7"
  effort "high"
  prompt "Follow the deployment runbook."

  on_activate fn() { log("deploy activated") }
  on_deactivate fn() { log("deploy deactivated") }
}
```

Registry ops: `skill_registry()`, `skill_define(reg, name, config)`,
`skill_list(reg)`, `skill_find(reg, name)`, `skill_count(reg)`,
`skill_select(reg, names)`, `skill_remove(reg, name)`,
`skill_describe(reg)`. `skill_list` strips closure hooks for
serialization; `skill_find` returns the full entry.

Known-key validation in `skill_define`: `description`, `when_to_use`,
`prompt`, `invocation`, `model`, `effort` must be strings; `paths`,
`allowed_tools`, `mcp` must be lists. Unknown keys pass through.

### Common patterns

Structured output with automatic retry — prefer
`llm_call_structured(prompt, schema, options?)`, which returns the
validated data directly (no `.data` unwrap) and forces the schema
defaults (`output_format: {kind: "json_schema", schema, strict: true}`,
`output_validation: "error"`,
`schema_retries: 3`). Throws on exhausted retries or transport
failure:

```harn
let schema = {
  type: "object",
  required: ["verdict"],
  properties: {
    verdict: {type: "string"},
    improvement: {type: "string"},
  },
}
let verdict = llm_call_structured(prompt, schema, {
  provider: "auto",
  model: "local:gemma-4-e4b-it",
  system: "You are a strict grader.",
})
log(verdict.verdict)
```

Non-throwing variant `llm_call_structured_safe(prompt, schema,
options?)` returns `{ok, data, error}` (same envelope as
`llm_call_safe`, but with the validated `.data` pre-unwrapped):

```harn
let r = llm_call_structured_safe(prompt, schema, {provider: "auto"})
if !r.ok {
  log("structured call failed:", r.error.category, r.error.message)
  return nil
}
log(r.data.verdict)
```

Diagnostic envelope `llm_call_structured_result(prompt, schema,
options?)` returns the full failure-mode breakdown
production agent pipelines need — `{ok, data, raw_text, error,
error_category, attempts, repaired, extracted_json, usage, model,
provider}`. Never throws; dispatch on `ok` / `error_category`:

```harn
let r = llm_call_structured_result(prompt, schema, {
  provider: "auto",
  schema_retries: 2,
  // Optional repair pass — runs only when the main call's JSON is
  // malformed or schema-invalid. Skipped on transport failures.
  repair: {
    enabled: true,
    model: "cheapest_over_quality(low)",
    max_tokens: 600,
  },
})
if r.ok {
  log(r.data.verdict)
} else {
  // error_category ∈ "transport" | "missing_json" | "schema_validation"
  // | "repair_failed" — plus retryable transport categories
  // ("rate_limit", "timeout", ...) when the underlying call failed.
  log("grade failed:", r.error_category, "raw:", r.raw_text)
}
```

`r.attempts` counts model calls (1 = no retries used; ≥2 = one or
more schema retries were spent). `r.repaired: true` means the repair
pass succeeded. `r.extracted_json: true` flags responses where
JSON had to be lifted from prose / markdown fences.

Options: everything `llm_call` accepts flows through, plus
`retries` as an alias for `schema_retries`. Provider options,
`system`, `provider`, `model`, `max_tokens`, etc. are all passed
through unchanged. The `repair` block is recognized only by
`llm_call_structured_result`.

After-the-fact recovery — `schema_recover(text, schema, opts?)`
turns malformed output that's already in your hand into a validated
payload. Three deterministic stages followed by an optional one-shot
LLM repair, returning the same `{ok, data, raw_text, error,
error_category, attempts, stage, repaired}` envelope shape:

| Stage | When | Notes |
|---|---|---|
| `parsed` | Raw text is valid JSON that schema-validates. | Cheapest path; always tried first. |
| `extracted` | JSON is wrapped in markdown fences or surrounded by prose. | Uses the same balanced-brace lifter as `json_extract`. |
| `regex` | Model produced YAML-ish / unquoted `key: value` lines. | Only top-level scalar fields (string/int/number/boolean) are recovered — nested objects fall through. |
| `llm_repair` | Earlier stages failed and `llm_repair` is enabled (default). | Single shot, `schema_retries: 0`. Set `{llm_repair: false}` for fully deterministic recovery. |

```harn
let raw = llm_call(prompt, sys, {provider: "auto"}).text
let r = schema_recover(raw, schema)
if r.ok {
  process(r.data)                  // narrowed-shape dict
} else {
  log("recovery failed:", r.stage, r.error_category, r.error)
}
```

Use it as a drop-in replacement for hand-rolled `normalize_*()`
chains downstream of `llm_call(...)` / Ollama prose responses, or
when you want a deterministic local recovery pass before paying for
a structured re-call. The `llm_repair` block accepts the same
overrides as `llm_call_structured_result`'s `repair`:

```harn
let r = schema_recover(raw, schema, {
  apply_defaults: true,            // schema defaults during validation
  llm_repair: {
    enabled: true,
    model: "cheapest_over_quality(low)",
    max_tokens: 600,
  },
})
```

Stages report via `r.stage` ∈ `"parsed" | "extracted" | "regex" |
"llm_repair" | "failed"`; `r.attempts` counts how many stages ran
(1 = clean parse, 4 = ran every stage including the LLM repair).
On failure, `r.error_category` is `"schema_validation"` (no stage
recovered) or `"repair_failed"` / `"transport"` (LLM repair was
attempted and failed).

If you need the raw response (token counts, transcript, thinking
trace) alongside the parsed data, call `llm_call` directly:

```harn
let r = llm_call(prompt, sys, {
  provider: "auto",
  model: "local:gemma-4-e4b-it",
  output_schema: schema,
  output_validation: "error",
  schema_retries: 2,
  output_format: {kind: "json_schema", schema: schema, strict: true},
})
log(r.data.verdict)
log(r.input_tokens)
```

Schema-as-type (a `type` alias drives both the schema and the
narrowing guard — lowered to the canonical JSON-Schema dict at compile
time; literal-string/int unions emit as `{type, enum}`). With
`llm_call_structured` the return narrows to `T` directly:

```harn
type GraderOut = {
  verdict: "pass" | "fail" | "unclear",
  summary: string,
}

let out: GraderOut = llm_call_structured(prompt, GraderOut, {
  provider: "auto",
  system: sys,
})
log(out.verdict)     // narrowed to GraderOut
```

Reusable generic wrapper (narrows via the `Schema<T>` generic
param):

```harn
fn grade<T>(prompt: string, schema: Schema<T>) -> T {
  return llm_call_structured(prompt, schema, {provider: "auto"})
}

let out: GraderOut = grade("Grade this", schema_of(GraderOut))
log(out.verdict)
```

Batch grading at bounded concurrency:

```harn
let outcome = parallel settle paths with { max_concurrent: 4 } { path ->
  llm_call(read_file(path), GRADER_SYSTEM, {
    provider: "auto",
    model: "local:gemma-4-e4b-it",
    output_schema: grader_schema,
    output_validation: "error",
    schema_retries: 2,
    output_format: {kind: "json_schema", schema: grader_schema, strict: true},
  })
}
```

### `assemble_context`

`assemble_context(options)` packs a list of artifacts into a
token-budgeted slice of chunks for the next prompt. Complements
`transcript_auto_compact` (which shrinks the ongoing conversation).

```harn
let packed = assemble_context({
  artifacts: [skill_a, skill_b, fetched_docs],
  budget_tokens: 8000,
  dedup: "chunked",                 // none | chunked | semantic
  strategy: "relevance",            // recency | relevance | round_robin
  query: user_prompt,               // scored by default keyword-overlap ranker
  microcompact_threshold: 2000,     // artifacts over this get chunked
})
// packed = {chunks, included, dropped, reasons, total_tokens, budget_tokens, …}
```

Chunk ids are content-addressed (`{artifact_id}#{sha256(text)[..16]}`)
so the same input produces the same ids across runs — safe to diff in
replay. `reasons` names the strategy and inclusion verdict per chunk;
`dropped` surfaces exclusions (`"duplicate"`, `"budget_exceeded"`,
`"no_text"`). For a custom relevance ranker, pass
`ranker_callback: { query, chunks -> chunks.map({ c -> score }) }`;
the default ranker uses keyword overlap against `query`. Workflow
nodes may set `context_assembler: {...}` to route the stage's selected
artifacts through this builtin before the prompt is rendered.

### Compaction policies

Compaction entrypoints accept a typed host/user instruction lane through
`policy`, `compaction_policy`, `compaction_request`, or the direct fields
`instructions`, `mode`, `scope`, `preserve`, `drop`,
`extend_default_instructions`, and `author`.

```harn
import {compact_preserving_test_failures} from "std/agent/autocompact"

let compacted = transcript_auto_compact(messages, {
  keep_last: 1,
  token_threshold: 1,
  policy: compact_preserving_test_failures({author: "host"})
})
```

Omitting `extend_default_instructions` or setting it to `true` appends the
instructions to Harn's default summary guidance; `false` replaces it.
Host-only instructions are kept in
`compaction` event metadata (`instruction_mode`, `instruction_source`,
`compaction_policy`) and are not copied into the next model-visible summary
unless `scope` is `"model_visible"`, `"summary"`, or `"transcript"`.

Helper policies in `std/agent/autocompact`:
`compaction_policy(...)`, `compact_for_bug_fix_resumption(...)`,
`compact_preserving_test_failures(...)`, and
`compact_retaining_current_plan(...)`.

## Reminders

System reminders are typed, ephemeral `system_reminder` transcript events
for nudging a running agent without pretending the nudge is user input
and without adding it to durable messages. They support `ttl_turns`,
`dedupe_key`, `preserve_on_compact`, `propagate`, and provider-aware
`role_hint` rendering. Full reference:
`docs/src/system-reminders.md`.

`transcript.inject_reminder(transcript, options)` appends a pending
reminder and returns `{transcript, reminder_id, deduped_count}`. The
input transcript is unchanged.

```harn
let injected = transcript.inject_reminder(transcript(), {
  body: "Approaching context window cap.",
  tags: ["token_pressure"],
  dedupe_key: "token_pressure",
  ttl_turns: 3,
  preserve_on_compact: true,
  propagate: "session",
  role_hint: "developer",
})
let t = injected.transcript
```

`body` is required and must be non-empty. Optional `tags`,
`dedupe_key`, `ttl_turns`, `preserve_on_compact`, `propagate`, and
`role_hint` fields are validated; unknown option keys fail fast. A new
reminder with the same `dedupe_key` replaces pending reminders with
that key and emits `transcript.reminder.deduped` on
`transcript.reminder.lifecycle` when an EventLog is active.

`transcript.clear_reminders(transcript, selector)` removes pending
reminders and returns `{transcript, removed_count}`. Select by `id`,
`tag`, or `dedupe_key`; when multiple selectors are present, all must
match.

```harn
let cleared = transcript.clear_reminders(t, {tag: "token_pressure"})
log(cleared.removed_count)
```

`agent_loop(...)` enables canonical reminder providers by default; bare
`llm_call(...)` does not. Providers are:

- `token_pressure` on `on_budget_threshold` at about 70/85/95% context
  use (`ttl_turns: 2`, critical threshold preserves across compaction).
- `idle_nudge` on `session_idle` after `idle_seconds` (default 60).
- `tool_output_truncated` on `post_tool_use` when tool output was
  compacted/truncated before the model saw it.
- `post_compact_recap` on `post_compact` with the latest recap.
- `resume_continuity` on `worker_resumed`, visible only to the first
  resumed turn. It names the suspend turn, reason, resume cause, and
  optional resume input; when `continue_transcript: false`, it also
  carries the pre-suspend digest.
- `idle_nudge`, `tool_output_truncated`, and `resume_continuity` use
  `propagate: "none"`; `post_compact_recap` uses `propagate: "session"`.

Opt out per loop:

```harn
agent_loop(task, system, {
  reminders: {providers: ["-token_pressure", "-idle_nudge"]},
})
```

Configure providers under `reminders.config`, e.g.
`{reminders: {config: {token_pressure: {context_window: 128000},
idle_nudge: {idle_seconds: 120}}}}`. Register Harn-defined providers
with `register_reminder_provider({id, subscribes_to, evaluate})`; the
closure receives `{event, session, session_id, payload, options,
config}` and returns a reminder effect, bare spec, effect list, or `nil`.

Hooks can return `{reminder: {...}, then?: ...}`, a bare reminder spec,
or a session-hook effect list. Hosts inject ambient context with the
bridge `session/remind` notification; `session/inject` remains user-role
input.

Rendering is capability-aware: routes that prefer developer-role
instructions get separate developer messages; Anthropic routes can use
`role_hint: "user_block"` or `"ephemeral_cache"` as
`<system-reminder>` user content blocks; XML providers get
`<system-reminder>` system text; fallback providers get plain
`System reminder:` system text.

Sub-agent handoffs carry a filtered `reminder_propagation` list.
`propagate: "all"` reaches descendants, `"session"` reaches direct
children only, and `"none"` stays local. Compaction decrements finite
TTLs, drops expired reminders, dedupes by `dedupe_key`, preserves only
`preserve_on_compact: true`, and passes surviving reminders to custom
compactors. Gotcha: `preserve_on_compact: false` with no finite
`ttl_turns` can live forever during normal turns but vanish on
compaction; `HARN-RMD-004` flags that shape.

## Agent runtime

### `agent_turn`

`agent_turn(prompt, options?)` is the high-level wrapper for the common
"complete this request" shape. It builds on `agent_loop`, moves
`options.system` into the system prompt, adds generic progress guidance,
defaults to loop-until-done completion, and requires the completion judge.
Native-tool turns complete naturally when the model returns final text
with no tool calls; text/no-tool turns use the normal sentinel path.
Pass `judge: {...}` or `done_judge: {...}` to customize the judge; omit
both to use the default judge.

The result is the normal `agent_loop` dict plus:

- `iterations` — compact per-turn summaries from live loop events.
- `judge_decisions` — structured completion judge decisions with
  `iteration`, `verdict`, `reasoning`, `next_step`, and
  `judge_duration_ms`, plus optional `trigger`.

```harn
let result = agent_turn("Review this patch and fix obvious issues.", {
  system: "Be direct and keep changes narrowly scoped.",
  provider: "openai",
  model: "gpt-5-mini",
})
log(result.visible_text)
log(result.judge_decisions[0].verdict)
```

### `agent_loop`

`agent_loop(prompt, system?, options?)` runs a multi-turn loop with
tool dispatch. Native-tool loops complete naturally when the model
returns final assistant text with no tool calls. Tagged text-tool stages
use `<done>##DONE##</done>`, and no-tool sentinel loops use bare
`##DONE##`. Set `done_sentinel` to a non-empty string to require a
sentinel, or `nil` for no sentinel. Native-tool loop-until-done loops default
to `nil`; text/no-tool loop-until-done loops default to `"##DONE##"`.

Returns a namespaced dict: top-level `status`, `text`, `visible_text`
(last iteration's prose with tool calls stripped), `task_ledger`,
`transcript`, `daemon_state`, `daemon_snapshot_path`, `trace`, and
`deferred_user_messages`; LLM execution metrics nested under `llm`
(`iterations`, `duration_ms`, `input_tokens`, `output_tokens`); tool
invocation data nested under `tools` (`calls`, `successful`, `rejected`,
`mode`). Failed tool dispatches are fed back to the next model turn as
error observations and appear under `tools.rejected`. The preferred
resilience surface is the `llm_caller:` seam (see "Composable LLM
callers"); the legacy `llm_retries` / `llm_backoff_ms` options are
still accepted for back-compat but emit a deprecation lint. Plus its
own `profile`, `tool_retries`, `max_iterations`, `max_nudges`, and
`native_tool_fallback`
(`"allow"`, `"allow_once"`, or `"reject"` for native-tool stages that
receive text-mode `<tool_call>` fallback output). `thinking`,
`interleaved_thinking`, and `anthropic_beta_features` apply to every
model turn; `reminders` controls canonical reminder providers (`false`
disables all, `providers: ["-id"]` opts out by id, `config` carries
provider-specific knobs). For Claude Opus 4.6/4.7, `thinking: true` is
enough to enable the interleaved-thinking beta header for the whole loop.

Profiles preload common loop budgets and retry counts. Explicit keys
override the profile:

| Profile | `max_iterations` | `max_nudges` | `tool_retries` | `llm_retries` | `schema_retries` |
|---|---:|---:|---:|---:|---:|
| `tool_using` (default) | 50 | 8 | 0 | 2 | 0 |
| `researcher` | 30 | 4 | 0 | 2 | 0 |
| `verifier` | 5 | 0 | 0 | 2 | 3 |
| `completer` | 1 | 0 | 0 | 2 | 0 |

Use `iteration_budget: {mode, initial, max, extend_by}` when a loop should
start with a small cap and extend only while making progress. `max_iterations`
is equivalent to a fixed budget; if both are present, `iteration_budget.max`
wins. Explicit `max_iterations`, `initial`, `max`, and adaptive `extend_by`
values must be positive integers, and `initial <= max`. Workflow stage
`model_policy` accepts the same `iteration_budget` shape and passes it through
to the per-stage `agent_loop`.

Pass `stop_after_successful_tools: ["name", ...]` to terminate the loop
the moment any of those tools is dispatched successfully. Same shape as
Vercel AI SDK's `stopWhen: hasToolCall(name)` and OpenAI Agents SDK's
`StopAtTools([name])`. Use this for "terminal" tools (e.g.
`exit_plan_mode`, `submit_answer`, `ask_user`) that mark the end of an
agent step:

```harn
agent_loop(task, sys, {
  tools: registry,
  stop_after_successful_tools: ["ask_question", "exit_plan_mode"],
})
```

The check fires after each iteration's tool dispatch, so any other
tool calls in the same iteration still run; only subsequent
iterations are skipped. The loop exits with `status = "done"` and
the tool name appears in `tools.successful`.

### Progress narration

Use `agent_progress({message?, entries?, replace?, metadata?})` from inside an
agent session when a meaningful sub-step completes or the visible plan changes.
The payload must include a non-empty `message` or `entries`; `replace` defaults
to `true`.

```harn
agent_progress({
  message: "Finished API inventory; checking auth paths next.",
  entries: [
    {content: "Inventory public API routes", status: "completed", priority: "high"},
    {content: "Trace auth middleware", status: "in_progress"},
  ],
})
```

`entries` are task-list items with `content`, `status`, and optional
`priority`. ACP clients receive entries as canonical `session/update`
`plan` payloads. A2A clients receive non-terminal `TaskStatusUpdateEvent`
updates with `status.state = "working"`. Message-only reports surface as Harn
progress narration for clients that do not render plans.

For model-facing loops, set `progress_tool: true` or pass a dict to customize
the tool name, description, or system-prompt nudge. Call it after observable
progress, not on a timer.

Pass `done_judge: true` or `done_judge: {...}` to run a structured
completion judge after a native-tool loop naturally completes or after
the model emits `##DONE##` in a sentinel loop.
The judge returns `verdict: "done" | "continue"` plus optional
`reasoning` and `next_step`. A veto injects feedback and the loop
continues until the judge accepts or `max_verify_attempts` is exhausted.
Each judge call emits a `JudgeDecision` agent event with optional `trigger`.
Use
`verify_completion_judge` instead when every natural stop should be
judged.

Use `done_judge.cadence` when completion checks should be signal-gated
instead of firing on every completion candidate:

```harn
agent_loop(task, system, {
  loop_until_done: true,
  done_judge: {
    cadence: {
      every: 5,                         // judge turns 5, 10, 15, ...
      when: "always",                   // or "stalled" / { state -> bool }
      max_invocations: 3,
      min_iterations_before_first: 2,
    },
  },
})
```

With `when: "stalled"`, stall diagnostics run the judge when
`agent_loop_stall_warning` fires. `done` stops the loop with
`stalled_done_judge`; `continue` keeps the normal stall feedback fallback. The
judge event includes `trigger: "stalled"`.

Omitting `cadence` preserves the default behavior: every completion
candidate is judged. `when: "stalled"` is quiet during healthy turns and
is reserved for stall diagnostics; pair it with stall-aware loop policy
instead of fixed "are you done?" prompting.

Fixed-cadence completion prompts are not recommended: Huang et al.'s
[AutoGPT/agent benchmark study](https://arxiv.org/abs/2310.01798) found that
periodic "are you done?" checks can distort behavior. Prefer explicit progress
signals and `done_judge.cadence.when: "stalled"` when the loop is actually
showing stall symptoms.

Pass `permissions` to scope one agent below the ambient `policy` ceiling:

```harn
agent_loop(task, system, {
  permissions: {
    allow: {read_note: { args -> args.path.starts_with("/workspace/") }},
    deny: ["write_note"],
    on_escalation: { request -> {grant: "once", approver: "operator"} },
  },
})
```

`allow` and `deny` accept tool-name globs, argument pattern lists, or VM
predicates. Deny rules win. Escalation callbacks receive a `PermissionRequest`
dict and return `false`, `true`, `{grant: "once"}`, or `{grant: "session"}`.
Child agents still intersect with the parent capability policy; escalation
cannot widen a parent ceiling.

### Agent lifecycle: pause, resume, self-park

`spawn_agent`, `wait_agent`, `resume_agent`, `suspend_agent`, and
`list_agents` from `std/agent/workers` are the script-level lifecycle
surface for delegated work. Layered on top, `agent_loop(...)` exposes a
model-facing **lifecycle tool** so the agent can park itself between turns
and so a parent loop can pause/resume children. Full reference:
`docs/src/agent-lifecycle.md`.

Three model-facing tools:

| Tool | Use |
|---|---|
| `agent_await_resumption(reason, conditions?)` | The current worker self-parks. Registered automatically by `agent_loop(...)`. |
| `subagent_pause(handle, reason)` | Parent loop pauses a running child after its current turn settles. Opt-in via `subagents: true`. |
| `subagent_resume(handle, input?, continue_transcript? = true)` | Parent loop resumes a suspended child. Opt-in via `subagents: true`. |

When the model calls `agent_await_resumption(...)` inside an `agent_loop`
running as a worker, the call is intercepted *before* normal tool dispatch:
the loop validates `conditions` with `parse_resume_conditions(...)`,
persists a snapshot, emits `WorkerSuspended`, and returns
`{status: "suspended", handle, reason, initiator: "self", conditions,
iterations_completed}` to the parent. Lifecycle calls emit
`tool_call_audit` telemetry with `initiator` (one of `"self"`, `"parent"`,
`"operator"`, `"triggered"`) and the supplied `reason`.

Top-level loops use the same shape: a root `agent_loop(...)` that parks
returns `status: "suspended"` with `handle.snapshot_path`, and the CLI
cold-restores it with `harn run --resume <snapshot_path>`.

```harn,ignore
// Self-park mid-loop until a review approval lands or 30 minutes pass.
import { agent_await_resumption } from "std/agent/workers"

let result = agent_loop("Wait for the maintainer's review.", nil, {
  provider: "openai",
  model: "gpt-5",
  tool_format: "native",
})

if result.status == "suspended" {
  log(result.reason)               // model-supplied
  log(result.handle.snapshot_path) // resumable snapshot on disk
}
```

```harn,ignore
// Parent-driven pause/resume of a background child.
let handle = sub_agent_run("Draft the changelog.", {
  background: true,
  provider: "openai",
})
suspend_agent(handle, "operator pulled context")
// ... other work ...
resume_agent(handle, "Pick up where you left off.")
let final = wait_agent(handle)
```

```harn,ignore
// Conditioned self-park: trigger + timeout.
agent_await_resumption("waiting on review", {
  trigger: {
    kind: "review.approved",
    provider: "github",
    match: {events: ["review.approved"]},
  },
  timeout: {duration_minutes: 30, on_timeout: "resume_with_summary"},
})
```

Resume responsibility is named by an optional `resume_by` callback (third
arg to `agent_await_resumption`). The four presets in `std/agent/resume_by`
are `ResumeBy.parent_llm`, `ResumeBy.local_runtime`, `ResumeBy.cloud_harness`,
and `ResumeBy.pipeline_drain`. They compose with
`std/lifecycle/combinators::first_available`. `default_resume_by(...)`
picks one based on whether `conditions` were supplied and whether a cloud
session is bound:

- `conditions == nil` → `ResumeBy.parent_llm`
- `conditions != nil`, no cloud session → `ResumeBy.local_runtime`
- `conditions != nil`, cloud session → `first_handled([cloud_harness, local_runtime])`

Transcript continuity: `resume_agent(...)` defaults to
`continue_transcript: true` — the resumed worker keeps its full transcript
and the runtime injects a single-shot `system_reminder` with
`dedupe_key: "resume_continuity"` summarizing the gap. Pass
`continue_transcript: false` to restart from the prior summary plus new
input only.

Daemon idle is a degenerate case: `agent_loop(..., {daemon: true})` and
the `daemon_*` stdlib wrappers (see `docs/src/llm/agent_loop.md#daemon-stdlib-wrappers`)
internally call `agent_await_resumption(...)` when no wake source is
queued. The snapshot carries daemon-specific fields
(`pending_event_count`, `wake_interval_ms`, `watch_paths`) alongside the
standard suspend metadata, so `daemon_resume(path)` cold-restores the
loop identically.

Common gotchas:

- **Suspend is cooperative**, not preemptive. The flag is honored at the
  next turn boundary — not mid-tool-call, not mid-LLM-request. Cap
  long-running tools with `tool_call_timeout`.
- **Conditions are optional.** A bare `agent_await_resumption("waiting")`
  parks the worker open; only the parent agent, an operator, or
  `resume_agent(...)` can wake it.
- **Snapshots survive process restart.** Both the snapshot file and any
  registered trigger conditions are durable. `harn run --resume <path>`
  rehydrates the worker in a fresh process.
- **Double-resume is detected** (`HARN-SUS-006`); the second caller can
  retry against the now-running handle.
- **Closing a suspended worker is terminal** — a later `resume_agent`
  raises `HARN-SUS-010`.

Diagnostic codes for the suspend/resume namespace are `HARN-SUS-001..010`
— see `docs/src/agent-lifecycle.md#diagnostic-codes` for the full table.

### Durable agent channels

Use `emit_channel(name, payload, options?)` for cross-run facts that should land
in the active event log. Bare names default to tenant scope:
`emit_channel("pr.merged", payload)` resolves to
`tenant:<current-or-default-tenant>:pr.merged`. Prefixes select a scope:
`session:foo`, `pipeline:foo`, `tenant:foo`, `tenant:<tenant_id>:foo`, or
`org:<org_id>:foo`; org scope currently fails with `HARN-CHN-002` until org
grants exist. Distinct `tenant_id`, `session_id`, or `pipeline_id` values
resolve to distinct topics, so cross-scope readers see an empty view. The
resolver also returns `HARN-CHN-001` for `pipeline:` outside a pipeline,
`HARN-CHN-003` for malformed names, and `HARN-CHN-004` when an explicit
`options.session_id` or `options.pipeline_id` conflicts with the active
context.

```harn
let receipt = emit_channel("session:worker.ready", {worker: "lint"}, {
  id: "worker-ready-lint",
  ttl: 10m,
})
log(receipt.event_id)
log(receipt.emitted_at.signature.starts_with("sha256:"))
```

Each stored event includes `id`, fully resolved `name`, `payload`,
`emitted_at` (signed), `emitted_by`, available `pipeline_id`, `session_id`, or
`tenant_id`, and `ttl_ms` when `options.ttl` is provided. Reusing the same
`options.id` on the same resolved channel is idempotent and returns the
original `event_id`. Use `channel_events(name, options?)` for tests and local
inspection.

Subscribe to channel emits with a `channel.emit` trigger (provider
`channel`). `match.events` accepts `"channel:<name>"` selectors:

```harn,ignore
import { trigger_register } from "std/triggers"

trigger_register({
  id: "release-on-pr-merge",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:pr.merged"]},
  handler: { event -> kick_release(event.provider_payload.payload) },
})
```

Add `batch: {count, window, key?, expire_action?}` to fire after N
matching emits (Inngest-shape fire-after-N — no other major durable-
execution platform owns this primitive). `key` is a dotted JSON path
that partitions counters; `expire_action` is `"fire_partial"` (default)
or `"discard"`. On dispatch `event.batch` holds the constituent events;
the buffer is per-process thread-local, capped at 1024 events per
partition, and replay reconstructs the batch from the recorded
`constituent_event_ids`.

```harn,ignore
trigger_register({
  id: "release-on-3-merges",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:pr.merged"]},
  batch: {count: 3, window: "1h", key: "repo"},
  handler: { event -> cut_release(event.batch) },
})
```

Pair `batch` with `ReminderInject({target, body, tags?, ttl_turns?,
dedupe_key?})` to land a periodic reminder on a running session
without spawning or resuming it. `target` is `"current"`, `"parent"`,
a literal session id, or a closure; `body` is a `.harn.prompt`
template against `{{ event }}`, `{{ match }}`, and `{{ batch }}`.
Missing targets drop gracefully with a `triggers.reminder_inject.audit`
audit entry. See `docs/src/agent-channels.md` for the full surface
and `docs/src/cookbooks/channels.md` for runnable recipes.

Pick the right primitive:

| Goal | Use |
|---|---|
| Hand off to one specific agent | Handoffs (`handoff(...)`, `@handoff`) |
| Wait for an external event (GitHub, Slack, cron) | Provider trigger |
| Park one agent until a specific event with a declared resume condition | Suspend/resume (`agent_await_resumption(reason, conditions)`) |
| Emit a typed event to many subscribers | **Channels** (`emit_channel(...)`) |
| Periodic reminder into a running loop | **Channels + `batch` + `ReminderInject`** |

Diagnostic codes: `HARN-CHN-001` (`pipeline:` outside a pipeline),
`HARN-CHN-002` (cross-tenant emit / disabled `org:`), `HARN-CHN-003`
(malformed name), `HARN-CHN-004` (scope ambiguous —
`options.session_id`/`pipeline_id` conflicts with active context),
`HARN-CHN-005` (malformed `batch` config). Replay-oracle codes are
`HARN-REP-CHN-001..003` — see `docs/src/observability/replay-benchmarks.md`.

Channel guardrails (`channel_guardrail_register(config)` and
`std/channel_guardrails` presets) run before the durable journal
append. Each guardrail returns `allow` / `warn` / `block`; worst
verdict wins; blocked emits never persist but the block decision does
on `lifecycle.channel.audit`. Built-ins ship `prompt_injection_scanner`
and `llm_risk_classifier`; `register_guardrail` accepts any custom
closure.

Pass `autonomy_budget` to cap how many autonomous decisions an agent can
make per UTC hour / UTC day. The check fires at loop entry, before any
LLM/MCP work — scripts can't bypass it. When the cap is exhausted,
`agent_loop` returns `status: "approval_required"` with a HITL approval
request id, emits an `autonomy.budget_exceeded` lifecycle event, and
appends an `autonomy.tier_transition` trust-graph record from `act_auto`
to `act_with_approval`:

```harn
agent_loop(task, system, {
  autonomy_budget: {per_hour: 10, per_day: 100, key: "captain.persona", reviewer: "oncall"},
})
```

`key` defaults to the loop's `session_id`; pick a stable identity (e.g.
persona name) when each call mints a fresh session. `reviewer` defaults
to `"operator"`. Setting both `per_hour` and `per_day` to `nil` disables
the budget. See `docs/src/triggers/budgets.md` for the matching
trigger-side cap and audit trail shape.

### `post_turn_callback` (judge / reflection pattern)

Every `agent_loop` turn fires the optional `post_turn_callback` closure
*after* tool dispatch and before the next LLM call. It is the canonical
hook for judges, reflection passes, and graders — no second
`agent_loop`-flavored builtin required.

The closure receives one dict argument with these keys (stable wire
shape; new keys are additive):

```harn
{
  session_id: string,                // live agent_session id (use this with agent_session_*)
  iteration: int,                    // 0-based turn index
  has_tool_calls: bool,
  dispatch: list | dict | nil,
  tool_count: int,                   // calls dispatched this turn
  tool_results: list<dict>,          // structured per-call results
  successful_tool_names: list<string>,
  rejected_tool_names: list<string>,
  session_successful_tools: list<string>,
  session_rejected_tools: list<string>,
  text: string,
  visible_text: string,
}
```

The return value drives the loop. Accepted shapes:

- `nil` / `""` — no-op, loop continues
- `string s` — inject as runtime feedback for the next turn
- `bool b` — set the stop flag
- dict with any combination of:
  - `message: string` — same as the bare-string shape
  - `stop: bool` — terminate the loop after this turn
  - `next_options: dict` — merge into the next loop iteration's options
  - `llm_options: dict` — merge into the next LLM call's `llm_options`

Because `session_id` is exposed, the closure can call any
`agent_session_*` builtin against the live transcript. The minimal
"every-N-turns judge" pattern:

```harn
let judge = { info ->
  if info.iteration % 3 != 0 { return nil }       // skip 2/3 turns
  let snapshot = agent_session_snapshot(info.session_id)
  let verdict = llm_call("...grade this transcript...", {
    provider: "openai", model: "gpt-5-mini",      // cheaper reflection model
    messages: [{role: "user", content: json_encode(snapshot)}],
    schema: {approved: "bool", feedback: "string"},
  })
  if !verdict.approved {
    return {message: "judge: " + verdict.feedback}
  }
  if verdict.approved && info.iteration > 5 { return {stop: true} }
  nil
}

agent_loop(task, system, {tools: registry, post_turn_callback: judge})
```

Hooks can also shape the next model turn. For example, once the required tool
evidence exists, ask the provider to stop calling tools and synthesize:

```harn
let finalize_after_evidence = { info ->
  if info?.session_successful_tools?.contains("read_file") {
    return {
      message: "Use the gathered evidence and produce the final answer now.",
      llm_options: {tool_choice: "none"},
    }
  }
  nil
}
```

Other strategies compose from existing primitives — no new runtime
mechanics required:

- **Terminal-only review** — gate the body on `info.iteration ==
  expected_max - 1`, or check `info.session_successful_tools` for a
  terminal tool name. Skip the early turns and judge once at the end.
- **Branch-and-replay** — call `agent_session_fork_at(info.session_id,
  k)` to checkpoint at a known-good turn, then return `{stop: true}`
  to halt the live loop. The enclosing pipeline rebuilds with the
  branch (see snippet below). The runtime intentionally does *not*
  swap the live loop's session mid-run — that would race with
  in-flight tool dispatches.

  ```harn
  let s = agent_session_open()
  let main = agent_loop(task, sys, {session_id: s, tools: registry,
    post_turn_callback: { info ->
      if judge_says_redo_from(info) {
        let branch = agent_session_fork_at(info.session_id, judged_k)
        agent_session_inject(branch, {role: "system",
          content: "Redo from turn ${judged_k} with: ${redirection}"})
        // Stash the branch id so the caller can pick it up.
        save_branch_id(branch)
        return {stop: true}
      }
      nil
    },
  })
  if main.status == "stopped" {
    agent_loop(task, sys, {session_id: load_branch_id(), tools: registry})
  }
  ```

- **Fork-and-race** — fork at the start (or any turn) and race two
  variants. Reuse the existing concurrency primitives — no race
  scaffolding lives in `agent_loop`:

  ```harn
  let base = agent_session_open()
  let branch = agent_session_fork(base)
  agent_session_inject(branch, {role: "system",
    content: "Try the brute-force approach."})

  let outcomes = parallel settle [base, branch]
    with { max_concurrent: 2 } { sess ->
      agent_loop(task, sys, {
        session_id: sess, tools: registry, max_iterations: 10,
      })
    }
  let winner = pick_first_done(outcomes.results)
  ```

  Use `parallel settle` (vs. `parallel each`) so a failure on one
  branch doesn't cancel the other. `max_concurrent: 2` keeps both
  branches running concurrently without unbounded fan-out if you
  generalize the list.

The closure runs in a child VM (separate `output` buffer) and its
return is parsed by `interpret_post_turn_callback_verdict`. Any
captured `log()` output flows back to the parent VM unchanged. The
callback is awaited synchronously per turn, so it can be a heavy LLM call
without races. Keep broad review strategies in
`post_turn_callback` when the policy needs custom timing, branching, or
multiple competing judges; use `done_judge` for the built-in
sentinel-only completion gate.

### Resume conditions

Self-parking agents use a shared `ResumeConditions` shape for
`agent_await_resumption(reason, conditions?)` and `spawn_agent({options: {resume_when:
...}})`. Call `parse_resume_conditions(conditions?)` or
`agent_await_resumption(reason, conditions?)` from
`std/agent/workers` when you need to validate or normalize the shape
without spawning a worker.

```harn
import { parse_resume_conditions, spawn_agent } from "std/agent/workers"

let resume_when = parse_resume_conditions({
  trigger: {
    kind: "review.approved",
    provider: "github",
    match: {events: ["review.approved"]},
  },
  timeout: {duration_minutes: 30, on_timeout: "resume_with_summary"},
  on_event: "operator.resume",
})

let worker_node = {
  kind: "subagent",
  mode: "llm",
  model_policy: {provider: "mock"},
  output_contract: {output_kinds: ["summary"]},
}

spawn_agent({
  task: "wait for review",
  node: worker_node,
  options: {resume_when: resume_when},
})
```

`trigger` reuses the trigger spec parser from `std/triggers` rather
than defining a second trigger DSL. `timeout.duration_minutes` must be a
positive integer, `timeout.on_timeout` defaults to
`"resume_with_summary"` and may be `"fail"` or `"resume_with_input"`,
and `on_event` must be a non-empty EventLog topic. Invalid fields raise
`HARN-SUS-002` with the failing field path.

### Sessions (persistent conversations)

Pass `session_id` to `agent_loop` to resume a multi-turn conversation:
prior messages are loaded as a prefix before the call runs, and the
final transcript is persisted back under the same id on exit. Calls
without a `session_id` (or with an empty string) mint an anonymous id
and never touch the store — the one-shot call shape is preserved.

```harn
let s = agent_session_open()                       // mint UUIDv7
agent_session_inject(s, {role: "user", content: "hi"})
let a = agent_loop("continue", nil, {session_id: s, provider: "mock"})
let b = agent_loop("remember me?", nil, {session_id: s, provider: "mock"})
let branch = agent_session_fork(s)                 // counterfactual
let replay = agent_session_fork_at(s, 1)           // branch from a rebuilt prefix
agent_session_close(branch)
agent_session_close(replay)
```

Lifecycle builtins (all hard-error on unknown ids except `exists`,
`open`, `snapshot`, `ancestry`):

- `agent_session_open(id?)` / `_close(id)` / `_exists(id)`
- `agent_session_current_id()` returns the innermost active session id or `nil`.
- `agent_session_reset(id)` / `_fork(src, dst?)` / `_fork_at(src, keep_first, dst?)` / `_trim(id, keep_last)`
- `agent_session_inject(id, {role, content, …})` — missing `role` errors.
- `agent_session_seed_from_jsonl(path, opts?)` creates a new session from a
  replayable `llm_transcript.jsonl` sidecar. Useful opts:
  `truncate_to_last`, `drop_tool_calls`, `rename_session`, `validate`,
  `provider`, `model`.
- `agent_session_compact(id, opts)` — supports LLM/truncate/observation-mask/custom
  compaction, accepts the same compaction policy fields as
  `transcript_auto_compact`, and errors on unknown option keys.
- `agent_session_length(id)` / `_snapshot(id)` / `_ancestry(id)` for read-only inspection.
- `cancel_in_flight_tool_call(session_id, call_id, opts?)` — abort one
  in-flight tool call without closing the session. `opts.reason` is
  surfaced to the model, `opts.inject_reminder` (default `true`) queues
  a system reminder so the model knows it was stopped, and
  `opts.timeout_ms` (default `5000`) bounds how long to wait for the
  dispatch to unwind. Returns
  `{status, call_id, tool, reason}` where status is `"cancelled"`,
  `"already_cancelled"`, `"not_found"`, or `"timeout"`. The cancelled
  call returns to the loop as `status: "cancelled"` so the model can
  distinguish "the host stopped me" from "the tool errored". The same
  surface is exposed over ACP as `session/cancel_tool_call`.

Session snapshots include `metadata.transcript_budget` after hard retention
budget pressure. `last_action` records whether Harn rejected, trimmed, or
compacted the transcript, along with before/after message and event counts.

### Daemon wrappers

Use the daemon stdlib wrappers when you want a first-class handle around
`agent_loop(..., {daemon: true})`:

- `daemon_spawn(config)` starts a persistent daemon and returns `{id, status, persist_path, ...}`.
- `daemon_trigger(handle, event)` appends a durable FIFO trigger event.
- `daemon_snapshot(handle)` returns the persisted daemon snapshot plus queue
  fields such as `pending_event_count`, `queued_event_count`,
  `inflight_event`, and `event_queue_capacity`.
- `daemon_stop(handle)` preserves state and re-queues any in-flight trigger.
- `daemon_resume(path)` resumes from the daemon state directory.

`daemon_spawn` accepts daemon-loop options like `wake_interval_ms`,
`watch_paths`, and `idle_watchdog_attempts`, plus
`event_queue_capacity` (default `1024`).

### Bridge-only builtins (IDE host integration)

These builtins are only meaningful when a Harn script runs inside a host
with a `HostCallBridge` attached (e.g. burin-code). Outside a bridge
session they raise an error — don't call them from `harn run` in a
plain terminal.

- `host_tool_list()` returns `list<{name, description, schema}>` —
  every tool the attached host has registered. Call once per script;
  cache the result.
- `host_tool_call(name, args)` invokes a host tool with a dict of
  arguments. Returns an opaque value — narrow it yourself before
  field access (strict types mode treats this as an untyped boundary).

### Filesystem extras

- `glob(pattern, base?)` → list of matching paths. Pattern is matched
  against forward-slash paths relative to `base` (defaults to script
  source dir); `**` glob is supported.
- `walk_dir(root, opts?)` → list of `{path, is_dir, is_file, depth}`.
  `opts.max_depth: int` and `opts.follow_symlinks: bool` are honored.
- `move_file(src, dst)` — `rename` with cross-filesystem copy+delete
  fallback.
- `read_lines(path)` → list of lines (no trailing newline). Handles
  CRLF correctly.

### CSV

```harn
csv_parse("name,age\nalice,30\n", {headers: true})
// → [{name: "alice", age: "30"}]

csv_stringify([{name: "alice", age: 30}], {headers: true})
// → "age,name\n30,alice\n"
```

Options: `headers: bool` (default false), `delimiter: ","` as one
ASCII character. Without headers, `csv_parse` returns list-of-lists;
with headers, list of dicts (keys are sorted on stringify for
determinism).

### URL parsing

```harn
url_parse("https://api.example.com:8080/v1/items?q=hi#frag")
// → {scheme: "https", host: "api.example.com", port: 8080,
//     path: "/v1/items", query: "q=hi", fragment: "frag", ...}

url_build({scheme: "https", host: "example.com", path: "/api",
           query: "x=1&y=2"})
// → "https://example.com/api?x=1&y=2"

query_parse("?key=alpha&key=beta")
// → [{key: "key", value: "alpha"}, {key: "key", value: "beta"}]

query_stringify([{key: "name", value: "ali ce"}])
// → "name=ali+ce"
```

### Modern crypto

- Hashes: `sha3_256`, `sha3_512`, `blake3` (in addition to existing
  SHA-2 family + MD5).
- Ed25519 signatures: `ed25519_keypair() -> {private, public}` (hex),
  `ed25519_sign(priv, msg) -> string` (hex sig),
  `ed25519_verify(pub, msg, sig) -> bool`.
- X25519 key agreement: `x25519_keypair() -> {private, public}`,
  `x25519_agree(priv, peer_pub) -> string` (hex shared secret).
- JWT verification: `jwt_verify(alg, token, key)` (HS256 / RS256 /
  ES256). Pairs with the existing `jwt_sign`.

### Date/time builtins

- `date_now() -> {year, month, day, hour, minute, second, weekday, timestamp, iso8601}`.
- `date_now_iso() -> string` returns current UTC as RFC 3339.
- `date_parse(str) -> int | float` parses RFC 3339 / ISO 8601 first, then falls back to
  legacy digit extraction for malformed date-ish strings.
- `date_format(ts, fmt?, tz?) -> string` supports chrono/strftime codes including `%A`,
  `%B`, `%Z`, `%z`, `%:z`, `%f`, `%3f`, and `%s`; negative pre-epoch timestamps work.
- `date_in_zone(ts, "America/Los_Angeles") -> dict` and `date_to_zone(ts, tz) -> string`
  convert through IANA timezone names.
- `date_from_components({year, month, day, hour?, minute?, second?}, tz?) -> int | float`.
- Durations: `duration_ms/seconds/minutes/hours/days(n) -> duration`,
  `date_add(ts, d)`, `date_diff(a, b) -> duration`,
  `duration_to_seconds(d)`, `duration_to_human(d)`.
- `weekday_name(ts, tz?)` and `month_name(ts, tz?)` return localized English names.

### HTTP builtins

- `http_get/post/put/patch/delete/request` return
  `{status, headers, body, ok}` for outbound HTTP calls.
- `http_download(url, dst_path, options?)` streams a response body to disk and
  returns `{bytes_written, status, headers, ok}`.
- `http_stream_open/read/info/close` expose pull-based response streaming;
  `http_stream_read` returns `bytes` chunks and then `nil` at EOF.
- Common options: `timeout_ms` (alias `timeout`), `total_timeout_ms`,
  `connect_timeout_ms`, `read_timeout_ms`, `retry: {max, backoff_ms}`,
  legacy `retries` / `backoff`, `retry_on`, `retry_methods`, `headers`,
  `auth`, `follow_redirects`, `max_redirects`, `proxy`,
  `proxy_auth: {user, pass}`, `decompress`, and
  `tls: {ca_bundle_path?, client_cert_path?, client_key_path?, client_identity_path?, pinned_sha256?}`.
- `http_post/put/patch` accept either `(url, body, options?)` or `(url, options)`
  when the request is driven entirely by options such as `multipart`.
- `multipart` accepts a list of part dicts with `name` plus one of `value`,
  `value_base64`, or `path`, along with optional `filename` and `content_type`.
- Default retries cover `408`, `429`, `500`, `502`, `503`, and `504` for
  idempotent methods only. `Retry-After` is honored on `429` / `503`.
- `http_mock(method, url_pattern, response)` can script multiple responses
  with `{responses: [...]}` and `http_mock_calls()` records each attempt.

### Connector HTTP policy

Package authors should prefer `std/connectors/shared` for provider API calls:

```harn
import { connector_http_json } from "std/connectors/shared"

let response = connector_http_json("POST", url, {
  headers: {Authorization: "Bearer " + token, Accept: "application/json"},
  body: json_stringify(payload),
  idempotency_key: "create:" + payload.id,
  retry: {max_attempts: 3, base_ms: 250, cap_ms: 30000},
  provider: "example",
  operation: "create_item",
})
```

`connector_http_request` returns a non-throwing envelope. Success:
`{ok: true, status, headers, body, retry_after_ms?}`. Failure:
`{ok: false, status?, retryable, retry_after_ms?, error}` where
`error.category` is stable for branching. `connector_http_json` adds `json` on
valid JSON and returns `error.category == "invalid_json"` on parse failure.
`POST`/`PATCH` retries require an existing or supplied `Idempotency-Key`, unless
`retry_unsafe: true` is explicit. `connector_http_header` and
`connector_http_rate_limit` cover case-insensitive header lookup plus
`Retry-After`, `RateLimit-*`, and `X-RateLimit-*` extraction.

For narrow AWS connector calls, use `aws_sigv4_headers(spec)` to sign one
request with explicit credentials, then pass `signed.headers` into
`harness.net.request(...)`. This is not an AWS SDK: there is no credential
chain, paginator, service client, or live AWS test requirement. `timestamp` is
required for deterministic signing, and temporary credentials use
`session_token` / `X-Amz-Security-Token`.

```harn
let body = "{\"TableName\":\"Items\"}"
let url = "https://dynamodb.us-east-1.amazonaws.com/"
http_mock("POST", url, {status: 200, body: "{\"ok\":true}", headers: {}})
let signed = aws_sigv4_headers({
  method: "POST",
  url: url,
  service: "dynamodb",
  region: "us-east-1",
  body: body,
  access_key_id: access_key_id,
  secret_access_key: secret_access_key,
  session_token: session_token,
  headers: {"Content-Type": "application/x-amz-json-1.0"},
  timestamp: "20260429T120000Z",
})
let response = harness.net.request("POST", url, {body: body, headers: signed.headers})
```

### Human-in-the-loop primitives

`ask_user`, `request_approval`, `dual_control`, and `escalate_to` are
**reserved keywords** — first-class typed expression syntax. The names
cannot be shadowed; envelopes are signed by the VM; quorum requires
distinct principals; replay is deterministic. Shared type aliases live
in `std/hitl`.

Each primitive accepts named arguments (preferred) or the legacy
positional form. Both lower to the same VM-enforced runtime.

```harn,ignore
let answer  = ask_user(prompt: "choose A or B", schema: schema_of(Choice))
let record  = request_approval(action: "merge_pr", args: {pr: 123}, quorum: 2,
                               reviewers: ["alice", "bob", "carol"])
let result  = dual_control(n: 2, m: 3, action: destructive_step,
                           approvers: ["alice", "bob", "carol"])
let handle  = escalate_to(role: "oncall", reason: "deploy failed")
```

- `ask_user<T>(prompt, schema?, timeout?, default?) -> T`
- `request_approval(action, args?, detail?, quorum?, reviewers?, deadline?,
  principal?, evidence_refs?, undo_metadata?, capabilities_requested?)
  -> {approved, reviewers, approved_at, reason, signatures}`
- `dual_control<T>(n, m, action: fn() -> T, approvers?) -> T`
- `escalate_to(role, reason)
  -> {request_id, role, reason, trace_id, status, accepted_at, reviewer}`
- `hitl_pending({since?, until?, kinds?, agent?, limit?} | nil)
  -> list<{request_id, request_kind, agent, prompt, trace_id, timestamp, approvers, metadata}>`

Operational semantics:

- Approval deadlines default to 24 hours.
- Timeouts append `hitl.timeout` and either return the supplied default or
  throw `HumanTimeoutError`.
- Denials throw `ApprovalDeniedError`.
- Replay reads recorded HITL responses from the event log instead of asking
  a live host again.

Host contract:

- Notification: `harn.hitl.requested`
- Resolution method: `harn.hitl.respond`

### Trigger stdlib

Use the trigger stdlib wrappers when a script needs to inspect or manually
exercise the live trigger registry:

- `trigger_list()` returns `list<TriggerBinding>`.
- `trigger_register(config)` hot-installs a dynamic trigger and returns a
  `TriggerHandle`. `config.retry` accepts `{max, backoff}` with
  `backoff: "svix" | "immediate"`. `config.when_budget` accepts
  `{max_cost_usd, tokens_max, timeout}` when `config.when` calls `llm_call(...)`.
- `trigger_fire(handle, event)` injects a synthetic `TriggerEvent` and returns a
  `DispatchHandle`.
- `trigger_replay(event_id)` fetches an event from `triggers.events` and
  re-dispatches it through the trigger dispatcher, preserving
  `replay_of_event_id`.
- `trigger_inspect_dlq()` returns `list<DlqEntry>` with retry history.
- `trigger_inspect_lifecycle(kind?)` returns lifecycle records including
  `predicate.evaluated`, `predicate.budget_exceeded`, and
  `predicate.daily_budget_exceeded`.

Shared types live in `std/triggers`: `TriggerConfig`, `TriggerBinding`,
`TriggerHandle`, `DispatchHandle`, `DlqEntry`, and `TriggerEvent`.

Trust-graph helpers also live in `std/triggers`:

- `handler_context()` returns the active trigger dispatch context or `nil`.
- `trust_record(agent, action, approver, outcome, tier)` appends a manual
  trust record.
- `trust_query(filters)` queries historical trust records, including
  `limit` and `grouped_by_trace`.
- `TriggerConfig.autonomy_tier` and manifest `[[triggers]].autonomy_tier`
  accept `shadow | suggest | act_with_approval | act_auto`.
- `harn trust query`, `harn trust promote`, and `harn trust demote` expose the
  same substrate from the CLI.

Current caveats:

- LLM-gated predicates are fail-closed. Single-evaluation budget overruns,
  daily budget exhaustion, provider failures, and circuit-breaker-open states
  all short-circuit the handler to `false`.
- Example:

```harn
import "std/triggers"

fn about_outages(event: TriggerEvent) -> bool {
  let result = llm_call(
    "Is this message about outages? " + event.kind,
    nil,
    {provider: "mock", model: "gpt-4o-mini", llm_retries: 0},
  )
  return contains(result.text.lower(), "yes")
}

let handle = trigger_register({
  id: "slack-outage-gate",
  kind: "slack.message",
  provider: "slack",
  handler: fn(event) { return event.kind },
  when: about_outages,
  when_budget: {max_cost_usd: 0.001, tokens_max: 500, timeout: "5s"},
  retry: nil,
  match: {events: ["slack.message"]},
  events: nil,
  dedupe_key: nil,
  filter: nil,
  budget: {daily_cost_usd: 1.0, max_concurrent: nil},
  manifest_path: nil,
  package_name: nil,
})
```

- `trigger_fire` / `trigger_replay` now reuse the dispatcher for local
  handlers, retries, and DLQ transitions. `a2a://...` returns either
  an inline remote result or a pending task handle, while `worker://...`
  returns an enqueue receipt for the durable worker queue job.
- `trigger_replay` is not the full deterministic T-14 replay engine yet:
  it replays the recorded trigger event through today’s dispatcher/runtime
  state rather than a sandboxed drift-detecting environment.

### Triage inbox stdlib

Use `std/triage` to turn Slack, Notion, GitHub, or generic connector payloads
into host-renderable inbox cards while retaining raw provider payloads for
audit:

```harn
import { triage_start_my_day } from "std/triage"

let connector_events = []
let feed = triage_start_my_day(connector_events, {emit: true})
for event in feed.events {
  log(event.summary)
}
```

- `triage_normalize(input, options?)` returns `harn.triage_event.v1` with
  `source_url`, normalized actors, card copy, action intents, privacy flags,
  a stable `dedupe_key`, and separate `raw_payload`.
- `triage_dedupe_key(provider, source_kind, source_url, source_id?)` hashes
  source provenance, not transport delivery ids.
- `triage_dedupe_events(events)` keeps first-seen order while dropping
  duplicate triage keys.
- `triage_emit(input, options?)` validates the envelope and appends
  `kind = "triage_event"` to `triage.inbox.events` by default.
- Non-navigation action intents must set `requires_approval: true`; hosts own
  write execution for dismiss, snooze, and convert-to-task actions.

### MCP apps UI resource stdlib

Use `std/ui_resource` to package interactive widgets as `ui://` resources for
MCP Apps hosts while keeping text/structured fallbacks first-class:

```harn
import { ui_resource, ui_select_for_host, ui_structured_fallback, ui_tool_result } from "std/ui_resource"

let resource = ui_resource(
  "ui://harn-dashboard/kpis@v1",
  "Weekly KPIs",
  weekly_kpi_html,
  {permissions: ["tools/call"], capabilities: ["tools/call", "context/read"]},
)
let result = ui_tool_result(resource, {structured_fallback: ui_structured_fallback({signups: 42, churn: 3})})
let rendered = ui_select_for_host(result, host_capabilities)
```

- `ui_resource(uri, name, html, options?: UiResourceOptions)` produces
  `UiResource` (`harn.ui_resource.v1`)
  with `mime_type: "text/html;profile=mcp-app"`, a content hash, CSP/sandbox
  policy, and an embedded `std/artifact/web` validation summary.
  `allow_host_bridge: true` is the default so `parent.postMessage` to the
  host counts as an expected MCP Apps bridge call rather than a finding.
- `ui_tool_meta(resource, options?: UiToolMetaOptions)` returns a `_meta.ui`
  block;
  `ui_tool_meta_to_mcp(meta)` serializes it into the MCP `resourceUri` /
  `visibility` / `initialView` shape MCP Apps hosts read from `tools/list`.
- `ui_tool_result(resource, options?: UiToolResultOptions)` wraps the resource
  with a mandatory text fallback (default: `web_artifact_text_fallback` of the
  HTML) and an optional `UiStructuredFallback`. Wrap raw fallback data with
  `ui_structured_fallback(data, options?: UiStructuredFallbackOptions)`.
  Invalid resources are stripped automatically unless the caller passes
  `allow_invalid_resource: true`.
- `ui_select_for_host(result, capabilities?)` picks `ui_resource`,
  `structured_fallback`, or `text_fallback` from the same envelope based on
  host capability advertisements. `ui_host_capabilities` accepts the MCP
  `client_capabilities.apps`, OpenAI Apps SDK `ui.apps`, or bare `{apps:
  true}` shapes through `UiHostCapabilityInput`.
- `ui_tool_call_envelope(name, params?, options?)` and
  `ui_context_update_envelope(key, value, options?)` build the JSON-RPC
  envelopes a sandboxed iframe sends through `window.parent.postMessage`.
- `ui_resource_csp_header(csp)` and `ui_resource_sandbox_attr(csp)` project
  the resource's CSP into header and sandbox attribute strings hosts can
  apply directly.
- `ui_tool_result_validate(result)` enforces schema versions, the text
  fallback contract, and refuses to ship a resource whose HTML failed
  validation.

Examples: `examples/ui_resource/dashboard-widget.harn`,
`examples/ui_resource/review-form.harn`.

### Profile bulletins stdlib

Use `std/personas/bulletins` when an agent learns a durable fact about a
person, project, team, or task. Bulletins are proposals — they never silently
enter durable context, and hosts emit separate decision events so the review
trail is replayable:

```harn
import { bulletin_propose, bulletin_emit, bulletin_accept, bulletin_render_for_prompt } from "std/personas/bulletins"

let bulletin = bulletin_propose(
  {
    scope: "user",
    scope_key: "kenneth@example.com",
    subject: "kenneth",
    persona: "burin_home",
    assertion: "prefers concise responses without trailing summaries",
    confidence: 0.92,
    source: {agent: "burin_home_curator"},
    evidence: [{kind: "user_msg", ref: "msg-42"}],
    privacy: {sync: "local_only"},
  },
)
let _proposal = bulletin_emit(bulletin)
let _accepted = bulletin_accept(bulletin, {decided_by: "user"})
```

- `bulletin_propose(input, options?)` returns `harn.profile_bulletin.v1` with
  `id`, `scope`, `scope_key`, `subject`, `assertion`, `status` (always
  `proposed` by default), `confidence` in `[0, 1]`, structured `evidence`,
  `source`, `privacy`, `proposed_at`, optional `expires_at` and `review_after`,
  and optional `supersedes` list.
- `bulletin_emit(input, options?)` always writes status `proposed` to
  `personas.bulletins.proposed`, even when the input has a different status.
- `bulletin_accept` / `bulletin_reject` / `bulletin_expire` /
  `bulletin_supersede` build and emit a typed
  `harn.profile_bulletin_decision.v1` envelope on
  `personas.bulletins.decisions`. `bulletin_supersede` requires at least one
  prior bulletin id.
- `bulletin_active(bulletins, now?)` returns only `accepted` bulletins still
  within their TTL; `bulletin_render_for_prompt(bulletins, options?)` renders
  prompt-ready text that visibly separates accepted facts from proposals
  pending review. Pass `{include_proposed: false}` to drop proposals.
- `bulletin_accept(b, {embed: true, memory_root?, embed_model_hint?})` also
  writes the accepted bulletin into the scope-partitioned memory namespace
  (`bulletin_memory_namespace(b)` — `personas/bulletins/<scope>/<scope_key>`)
  with eager embedding, so persona prompts can `memory_recall` past
  decisions semantically.

### Durable memory (`std/memory`)

```harn
import { memory_open, memory_store, memory_recall, memory_summarize, memory_forget } from "std/memory"

// Optional: configure the namespace once. Defaults to deterministic BM25.
memory_open("workspace/acme", {backend: "hybrid", embed_dim: 1024, embed_model_hint: "voyage-2"})

memory_store("workspace/acme", "alice-profile", {text: "prefers Rust"}, ["profile"])
let hits = memory_recall("workspace/acme", "rust", 5, {mode: "semantic"})
let summary = memory_summarize("workspace/acme", {limit: 10})
memory_forget("workspace/acme", {tag: "stale"})
```

- Append-only event log at `.harn/memory/<namespace>/events.jsonl`. Pass
  `{root: "path"}` in options to override.
- `memory_open` writes a config event (latest wins) — backends: `"bm25"`
  (default), `"vector"`, `"hybrid"`. Hybrid weights default to `0.5 / 0.5`
  and are tunable via `bm25_weight` / `cosine_weight`.
- `memory_recall` accepts `options.mode` (`lexical` / `semantic` / `hybrid`)
  to override the namespace default for one query. Returned records carry a
  `score` field.
- Vector and hybrid recall call the typed host capability
  `memory.embed({text, model_hint})` and cache the result on disk at
  `.harn/memory/<namespace>/vectors/<sanitized_model_hint>/<sha256(text)>.json`.
  Replays with the same event log and cache are deterministic without the
  host being attached.
- In tests, register the embedder via `host_mock("memory", "embed", {result:
  {vector: [...], dim: N, model: "..."}})`. Mocks can match on `params: {text,
  model_hint}` for per-record vectors.

Workflow stages pick up a session id from `model_policy.session_id`;
two stages sharing an id share their conversation automatically. The
pre-0.7 `transcript_policy` dict (with `mode: "reset" | "fork"`) was
removed — call the lifecycle verbs explicitly.

### Lifecycle hooks

Three concentric surfaces:

- `register_tool_hook({pattern, deny?, max_output?, pre?, post?})` — tool-level
  `PreToolUse` / `PostToolUse`. `pre` and `post` are closures that
  receive `{event, tool, result?}` payloads; `pre` can return `{deny}`
  or `{args}`, and `post` can return a string or `{result}`.
- `register_persona_hook(persona_pattern, event, handler)` — persona
  `PreStep` / `PostStep` / `OnApprovalRequested` / `OnHandoffEmitted` /
  `OnPersonaPaused` / `OnPersonaResumed` / `OnBudgetThreshold(pct)`.
- `register_session_hook(event, handler)` — whole-session lifecycle:
  `session_start`, `session_end`, `user_prompt_submit`, `pre_compact`,
  `post_compact`, `post_turn`, `permission_asked`, `permission_replied`,
  `file_edited`, `session_error`, `session_idle`, `pre_finish`,
  `post_finish`, `on_unsettled_detected`, plus the agent-lifecycle
  events `pre_suspend`, `post_suspend`, `pre_resume`, `post_resume`,
  `pre_drain`, `post_drain`, `on_drain_decision` (harn#1859). Veto
  with `{block: true, reason}`; short-circuit a permission with
  `{decision: "allow"|"deny"|"ask", reason}`. Lifecycle-gate events
  also accept `{modify: payload}` to rewrite the dispatched event
  (`pre_suspend` rewrites the reason, `pre_resume` amends the resume
  input, `pre_drain` amends the drain spec, `on_drain_decision`
  rewrites the tool call, `on_unsettled_detected` amends the unsettled
  snapshot). `pre_finish` rejects `{block: true}` and surfaces a
  runtime error pointing at `OnFinish.block_until_settled`; use that
  preset to delay finish until unsettled work clears. The full
  per-event return semantics:

  | Event                  | Allow | Deny / Block                            | Modify                  | Reminder    |
  |------------------------|-------|-----------------------------------------|-------------------------|-------------|
  | `pre_suspend`          | yes   | cancel suspend, worker keeps running    | rewrite reason          | inject only |
  | `post_suspend`         | yes   | n/a                                     | n/a                     | inject only |
  | `pre_resume`           | yes   | stay suspended                          | amend resume input      | inject only |
  | `post_resume`          | yes   | n/a                                     | n/a                     | inject only |
  | `pre_drain`            | yes   | skip drain                              | amend drain spec        | inject only |
  | `post_drain`           | yes   | n/a                                     | n/a                     | inject only |
  | `on_drain_decision`    | yes   | block tool call                         | rewrite tool call       | inject only |
  | `on_unsettled_detected`| yes   | block finish until settled              | amend unsettled payload | inject only |
  | `pre_finish`           | yes   | INVALID — use `OnFinish.block_until_settled` | n/a                | inject only |
  | `post_finish`          | yes   | n/a (advisory)                          | n/a                     | inject only |

  Tape captures every invocation under `hook_call` / `hook_returned` /
  `hook_vetoed`.
- Any tool, persona, step, or session hook can also emit a typed reminder
  for the active session transcript. Return `{reminder: {body, tags?,
  dedupe_key?, ttl_turns?, preserve_on_compact?, propagate?, role_hint?},
  then?}` to combine the reminder with an existing action, return a bare
  reminder spec such as `{body: "Refresh context", tags: ["context"]}`,
  or return a session-hook effect list like `[{reminder: {...}}]`.
- `register_reminder_provider({id, subscribes_to, evaluate})` registers a
  Harn-defined provider for `post_tool_use`, `on_budget_threshold`,
  `post_compact`, or `session_idle`; `clear_reminder_providers()` clears
  user-defined providers.
- `pipeline_on_finish(callback)` — register a `fn(harness, return_value)`
  callback that runs between `pre_finish` and `post_finish` on the main
  VM (its stdout reaches the host capture buffer). The callback's
  return value replaces the pipeline's return value, so a custom
  `on_finish` can wrap, redact, or audit the result. Four canonical
  presets ship in `std/lifecycle`:
  - `on_finish_abandon(harness, return_value)` — reproduces today's
    no-callback behavior; emits `pipeline_abandoned_unsettled` when
    work is left behind.
  - `on_finish_drain(harness, return_value)` — recommended default;
    emits `pipeline_finalized` when nothing is deferred, otherwise
    delegates to `harness.spawn_settlement_agent` (settlement-agent
    loop tracked under harn#1856).
  - `on_finish_block_until_settled(timeout, fallback?)` — factory that
    waits until everything settles or the timeout elapses, then
    delegates to `fallback` (default `on_finish_drain`).
  - `on_finish_handoff_to(target_pipeline, options?)` — factory that
    packages unsettled state into an envelope and hands it to
    `target_pipeline` via `harness.handoff_to`.

  Presets are pure functions / pure factories; they compose freely. See
  `docs/src/stdlib/lifecycle.md` for an example chain.
- `std/lifecycle/combinators` exports six pure factories that wrap any
  `(harness, return_value) -> return_value`-shaped callback (hook
  handler, `resume_by`, `on_finish`, ...):
  - `compose(callbacks)` — invoke each callback sequentially, threading
    each return value into the next callback's `return_value`; returns
    the last entry's value.
  - `first_available(callbacks)` — invoke in order; return the first
    non-nil result. Skips remaining callbacks after the first non-nil.
  - `with_telemetry(callback, span_name?)` — wrap with a `SpanKind::FnCall`
    OTel span and paired `{span_name}_started` / `_completed` /
    `_errored` audit entries.
  - `with_timeout(callback, ms)` — soft, clock-aware deadline; on
    overrun returns `{__timed_out: true, timeout_ms, elapsed_ms,
    return_value}` and emits a `lifecycle_callback_timed_out` audit.
  - `if_unsettled(callback)` — only invoke when
    `harness.unsettled_state()` is non-empty; one snapshot per call.
  - `when(predicate, callback)` — only invoke when
    `predicate(harness, return_value)` is truthy; otherwise pass the
    inbound value through unchanged.
- `std/observability` exports `obs()`, a unified facade for user-space
  spans, logs, metrics, and structured events. Configure once with
  `import { obs } from "observability"` then
  `obs().configure({backend: obs().Backend.auto})`, use
  `obs().span("name", attrs, { -> ... })` for scoped auto-close, or
  `start_span` / `log_in_span` / `end_span` for imperative flows.
  Backends include OTel, Splunk HEC, Honeycomb, pretty stderr,
  `compose([...])`, and env-driven `auto`.
- `std/timing` is the scoped-duration primitive that replaces hand-rolled
  `let started = harness.clock.now_ms(); work(); let dur =
  harness.clock.now_ms() - started`. Use `timed("op", attrs, { -> work()
  })` for callback-scoped auto-close (returns `{result, timing}` with
  `timing.duration_ms` from the monotonic clock and
  `timing.started_at_ms` / `timing.ended_at_ms` from the wall clock for
  external correlation). Use `start_timing` / `timing_event` /
  `end_timing` for flows that cross callbacks, branches, or async-ish
  lifecycle boundaries. Duplicate `end_timing` is idempotent. Timing
  spans are emitted under `SpanKind::UserTiming` (`kind: "user_timing"`),
  so `trace_spans()` and `harn run --profile-json` surface them as their
  own bucket without colliding with LLM/tool spans.
- `std/lifecycle/on_budget` exports three named callback strategies for
  the `OnBudgetThreshold` event. Each takes `(harness, budget_state)`
  and composes with the combinators above:
  - `terminate(harness, budget_state)` — emits a `budget_exceeded`
    audit, then throws `{category: "budget_exceeded", kind: "terminal",
    reason: "on_budget_terminate", strategy: "terminate", budget_state,
    message}` so the surrounding agent loop / pipeline unwinds.
  - `graceful_exit(harness, budget_state)` — emits a
    `budget_graceful_exit` audit; returns a deterministic envelope
    `{status: "budget_exhausted", strategy: "graceful_exit", reason:
    "on_budget_graceful_exit", budget_state, message}` instead of
    throwing.
  - `warn_and_continue(harness, budget_state)` — emits a
    `budget_warn_and_continue` audit, injects a 1-turn
    `budget_warning` system_reminder via `tool_hooks_inject_reminder`,
    and returns the original `budget_state` unchanged (passthrough for
    combinator chains).
  - `OnBudget()` returns the namespace dict so callers can use
    dotted access (`OnBudget.terminate`, etc.) after a single import.
- `harness.unsettled_state()` returns a stable dict with
  `suspended_subagents`, `queued_triggers`, `partial_handoffs`,
  `in_flight_llm_calls`, and `pool_pending_tasks` lists.
  `harness.is_empty(state?)`, `harness.counts(state?)`, and
  `harness.summary(state?)` summarize that shape; `std/lifecycle` exports
  equivalent `unsettled_state(harness)`, `is_empty(state)`,
  `counts(state)`, and `summary(state)` helpers. Suspended subagents,
  partial handoffs, in-flight LLM calls, and pool pending tasks are
  populated from live VM registries, while queued triggers are
  reconstructed from the active trigger inbox and worker-queue event-log
  records.
- Lifecycle action methods exist on the root harness for drain callbacks:
  `resume_subagent`, `cancel_subagent`, `handoff_to`, `acknowledge_trigger`,
  `defer_trigger`, `acknowledge_handoff`, `wait_for_any_settlement`,
  `emit_audit`, `finalize`, `spawn_settlement_agent`, and
  `current_pipeline_id`. `resume_subagent` and `cancel_subagent` delegate
  to host worker primitives; trigger acknowledgements use existing
  dispatcher cancel requests or worker-queue ack records; handoff
  acknowledgement removes the partial envelope; `emit_audit`,
  `handoff_to`, and `finalize` record into the per-pipeline-run lifecycle
  registries. `spawn_settlement_agent` remains the P-03 handoff point and
  returns a typed `{status: "unsupported", method, reason}` receipt until
  harn#1856 lands.
- `pipeline_lifecycle_audit_log_take()` and
  `pipeline_lifecycle_audit_log_snapshot()` drain or peek at the
  per-pipeline-run audit log that `harness.emit_audit` writes. Each
  entry is `{seq, kind, payload, pipeline_id}`. `std/lifecycle`
  re-exports them as `lifecycle_audit_log_take` /
  `lifecycle_audit_log_snapshot`.

### Pipeline lifecycle: drain, on_finish, composable handlers

Every lifecycle boundary in a Harn pipeline is a callback. Presets in
`std/lifecycle` cover the common dispositions; combinators in
`std/lifecycle/combinators` compose them; the harness exposes a single
read-side surface (`unsettled_state`) and a dozen write-side actions
for custom drain logic. Full prose: `docs/src/pipeline-lifecycle.md`.
Cookbook recipes: `docs/src/cookbooks/lifecycle.md`. Per-preset stdlib
reference: `docs/src/stdlib/lifecycle.md`.

`pipeline_on_finish(callback)` registers a `fn(harness, return_value)`
that runs between `pre_finish` and `post_finish` on the main VM. The
return value replaces the pipeline's return value. Registration is
last-write-wins and one-shot per run — a stale registration cannot
leak.

`OnFinish.*` presets (`std/lifecycle`):

| Preset | Behavior |
|---|---|
| `on_finish_abandon` | Today's default. Emits `pipeline_abandoned_unsettled` when work survives. |
| `on_finish_drain` | Recommended. Walks unsettled buckets via `harness.spawn_settlement_agent` in canonical order with per-item `drain_decision` audits. |
| `on_finish_block_until_settled(timeout, fallback?)` | Polls `harness.wait_for_any_settlement` until drained or timeout, then delegates to `fallback` (default `on_finish_drain`). |
| `on_finish_handoff_to(target_pipeline, options?)` | Packages unsettled state into a typed envelope and hands it to `target_pipeline` via `harness.handoff_to`. |

```harn,ignore
import { on_finish_drain, on_finish_handoff_to, on_finish_block_until_settled } from "std/lifecycle"

pipeline_on_finish(on_finish_drain)
pipeline_on_finish(on_finish_handoff_to("nightly-drain"))
pipeline_on_finish(on_finish_block_until_settled(30s, on_finish_drain))
```

`Combinator.*` factories (`std/lifecycle/combinators`) wrap any
`(harness, return_value) -> return_value` callback (presets, hook
handlers, `resume_by`, custom drain). All six are pure factories:

| Combinator | Behavior |
|---|---|
| `compose([cb, ...])` | Sequential; threads each return value into the next. |
| `first_available([cb, ...])` | Returns the first non-nil result. |
| `with_telemetry(cb, span_name?)` | OTel `SpanKind::FnCall` + paired `{span_name}_started` / `_completed` / `_errored` audits. |
| `with_timeout(cb, ms)` | Soft deadline; on overrun returns `{__timed_out, timeout_ms, elapsed_ms, return_value}` and emits `lifecycle_callback_timed_out`. |
| `if_unsettled(cb)` | Only when `harness.unsettled_state()` is non-empty (one snapshot per call). |
| `when(predicate, cb)` | Only when `predicate(harness, return_value)` is truthy. |

```harn,ignore
import { on_finish_drain } from "std/lifecycle"
import { compose, if_unsettled, with_telemetry, with_timeout } from "std/lifecycle/combinators"

pipeline_on_finish(
  if_unsettled(with_telemetry(with_timeout(on_finish_drain, 30000), "drain")),
)
```

The drain step is the per-item disposition loop behind
`on_finish_drain`. The settlement-agent walks buckets in the
documented order — suspended subagents → queued triggers → partial
handoffs → in-flight LLM calls → pool pending — applying a default
disposition (cancel / acknowledge / defer) per item and firing
`OnDrainDecision` for each. The constrained drain tool surface is
exposed when `__host_settlement_agent_active()` returns true. The
loop is bounded by a per-call budget (default 5, hard-cap 20); on
exhaustion a `drain_unsettled_remaining` audit captures the
remainder. `harness.acknowledge_trigger` and `acknowledge_handoff`
reject out-of-order calls with `HARN-DRN-001`.

`OnBudget.*` strategies (`std/lifecycle/on_budget`) for the
`OnBudgetThreshold` event, all `(harness, budget_state) -> result`:

| Strategy | Behavior |
|---|---|
| `OnBudget.terminate` | Emits `budget_exceeded`; throws structured terminal error. |
| `OnBudget.graceful_exit` | Emits `budget_graceful_exit`; returns deterministic exit envelope (no throw). |
| `OnBudget.warn_and_continue` | Emits `budget_warn_and_continue`; injects a 1-turn `budget_warning` reminder; passes `budget_state` through. |

Hook-event table for lifecycle gates (`register_session_hook`):

| Event | Allow | Deny / Block | Modify | Reminder |
|---|---|---|---|---|
| `pre_finish` | yes | INVALID — use `OnFinish.block_until_settled` | n/a | inject only |
| `post_finish` | yes | n/a (advisory) | n/a | inject only |
| `on_unsettled_detected` | yes | block finish until settled | amend unsettled payload | inject only |
| `pre_suspend` | yes | cancel suspend | rewrite reason | inject only |
| `post_suspend` | yes | n/a | n/a | inject only |
| `pre_resume` | yes | stay suspended | amend resume input | inject only |
| `post_resume` | yes | n/a | n/a | inject only |
| `pre_drain` | yes | skip drain | amend drain spec | inject only |
| `post_drain` | yes | n/a | n/a | inject only |
| `on_drain_decision` | yes | block tool call | rewrite tool call | inject only |

Common patterns:

```harn,ignore
// Hand unsettled to a nightly settlement pipeline.
import { on_finish_handoff_to } from "std/lifecycle"
pipeline_on_finish(on_finish_handoff_to("nightly-settle"))

// Drain with custom audit per disposition.
import { on_finish_drain } from "std/lifecycle"
register_session_hook("on_drain_decision", { event ->
  external_audit_push(event)
  return nil
})
pipeline_on_finish(on_finish_drain)

// Abort cleanly on unsettled state (no silent loss).
import { on_finish_block_until_settled } from "std/lifecycle"
pipeline_on_finish(
  on_finish_block_until_settled(60s, { harness, rv ->
    harness.emit_audit("aborted_with_unsettled", {state: harness.unsettled_state()})
    throw {category: "unsettled_at_finish", reason: "timeout"}
  }),
)
```

Cross-ref: the suspend/resume primitive that drives
`suspended_subagents` is the agent-lifecycle entry above (harn#1836).

### Agent pools

`std/lifecycle/pool` provides named, concurrency-bounded thread pools.
One named pool, one shared concurrency budget across every submitter.
Use a pool when many independent call sites need to share a cap; use
`parallel each ... with { max_concurrent: N }` when one call site
needs a local cap.

```harn
import { Backpressure, fair_round_robin, pool_create, pool_wait } from "std/lifecycle/pool"

let backpressure = Backpressure()
let pool = pool_create({
  name: "reviews",
  max_concurrent: 2,
  queue: fair_round_robin("tenant_id"),
  backpressure: backpressure.queue(100, "fail_submitter"),
})

let handle = pool.submit({ -> agent_loop("review", "You are a reviewer.") }, {
  tenant_id: "acme",
  priority: 10,
  idempotency_key: "review-pr-1984",
})
let result = pool_wait(handle)
```

Pick-the-right-primitive:

| Need | Use |
|---|---|
| Bound concurrency at one call site | `parallel each ... with { max_concurrent }` |
| Bound concurrency across many call sites in one process | Pool, `scope: "session"` (default) |
| Bound across pipeline runs that survive restart | Pool, `scope: "pipeline"` (state in `.harn/pools/`) |
| Bound across tenants/orgs (cloud) | Pool, `scope: "tenant"` / `"org"` (host-routed; harn-cloud#306) |
| Route trigger events through a shared budget | `SpawnToPool` handler (see below) |

Queue strategies (factories from `std/lifecycle/pool`):

| Factory | Behavior |
|---|---|
| `fifo()` | Oldest queued first. |
| `priority()` | Highest submit `priority` first, FIFO tiebreak. Default. |
| `lifo()` | Newest queued first. |
| `fair_round_robin(key = "key")` | Partition by `options.<key>` on submit; round-robin across distinct partitions. Missing field shares a default partition. |

Backpressure descriptors are `backpressure.queue(max_depth, on_full)`,
`backpressure.fail_fast`, and `backpressure.ring_buffer(capacity)`.
`on_full` accepts `block_submitter`, `drop_oldest`, `drop_newest`, or
`fail_submitter`. Drop policies return rejected task handles
(`status: "rejected"`, `rejection_reason`, `rejection_policy`) and
emit `pool_drop` audits on `lifecycle.pool.audit`; fail paths raise
`HARN-POL-001` (`fail_submitter`) or `HARN-POL-002` (`fail_fast`).

Submit options:

| Option | Notes |
|---|---|
| `priority` | int; higher dequeues sooner under `priority()`. |
| `key` | string; generic fairness key for `fair_round_robin("key")`. |
| custom key (e.g. `tenant_id`) | When using `fair_round_robin("tenant_id")`, pass the partition under that name. |
| `idempotency_key` | Two submits with the same `(pool_id, key)` return the *same* task handle. Pipeline-scope pools persist the index so resubmit after restart short-circuits. |

`pool.submit` returns a task handle (`_type: "pool_task"`) with `id`,
`pool`, `pool_id`, `status`, `submitted_at`, `key`, `priority`, and
(when terminal) `result` / `error` / `rejection_reason`.
`pool_wait(handle)` (or a list of handles) blocks until terminal and
returns the final snapshot. `wait_agent(handle)` from
`std/agent/workers` recognises pool task handles transparently.

Inspection: `pool.size()`, `pool.snapshot()` (full dict with
`active`, `queued`, `completed`, `failed`, `rejected`,
`blocked_submitters`, `total`, selected `queue` / `backpressure`,
per-task list, original `config`), `pool_get(name_or_id)`,
`pool_list()`. Pipeline-scope pools also reload in-flight tasks past
`stale_after_ms` as re-enqueued attempts; `pool_simulate_restart()`
drops the in-process registry for conformance tests.

Route trigger events through a pool with the `SpawnToPool` handler
variant from `std/triggers` (one trigger, one drain rate):

```harn,ignore
import { trigger_register, SpawnToPool } from "std/triggers"

trigger_register({
  id: "webhook-router",
  kind: "channel.emit",
  provider: "channel",
  match: {events: ["channel:webhook.received"]},
  handler: SpawnToPool({
    pool: "webhook-work",
    key_from: "provider_payload.payload.source",
    priority_from: "provider_payload.payload.urgency",
    task_factory: { event -> { -> handle_webhook(event) } },
  }),
})
```

`key_from` / `priority_from` are dotted JSON paths into the trigger
event. Missing paths fall back to the default partition and `0`
priority. The dispatcher records the resulting pool task id on the
match receipt so replay verifies the same event mapped to the same
task across runs.

Full prose: `docs/src/agent-pools.md`. Cookbook recipes (webhook
rate-limit, GPU pool, cross-customer fairness, burst absorber):
`docs/src/cookbooks/pools.md`. Stdlib API reference:
`docs/src/stdlib/lifecycle-pool.md`.

```harn
register_session_hook("user_prompt_submit", { event ->
  if to_string(event?.prompt ?? "").contains("secret") {
    return {block: true, reason: "policy violation"}
  }
  return nil
})
register_session_hook("file_edited", { event ->
  log("edit: " + to_string(event?.path ?? ""))
  return nil
})
```

`write_file` / `append_file` / `write_file_bytes` queue automatically;
hooks fire at the next agent-loop turn boundary. Call
`notify_file_edited(path, metadata?)` to explicitly emit one.
For background context refresh/librarian jobs, import
`std/context/maintenance` and return `context_maintenance_queue_receipt(...)`
from the hook instead of doing slow work inline.

## Stdlib LLM helpers (`std/llm/*`)

Nine opinionated modules wrap common LLM patterns:

- `std/llm/handlers` — composable middleware: `default_llm_caller`,
  `with_retry`, `with_fallback`, `with_shadow`, `with_prompt_rewrite`,
  `with_logging`, `with_budget`, `with_cache`, `with_circuit_breaker`,
  `with_repair`, `with_coerce`, `with_timeout`, `with_routing`,
  `compose([...])`.
- `std/llm/tool_middleware` — composable middleware around tool execution
  (parallel to handlers, but for tools): `default_tool_caller`,
  `compose_tool_callers([...])`, `tools_use_middleware` (schema
  decorator), `tool_inject_param`, plus the bundled library
  (`with_required_reason`, `with_audit_log`, `with_consent`,
  `with_dry_run`, `with_redaction`, `with_idempotency`,
  `with_rate_limit`, `with_telemetry`, `with_summary`,
  `with_handoff_artifact`, `with_timeout`).
- `std/llm/tool_binder` — experimental natural-language tool binder
  middleware (`with_natural_language_executor`). OFF by default; opt in
  via `compose_tool_callers`. Hands the planner-emitted intent + tool
  JSON Schema to a latency-budgeted binder LLM (Cerebras GPT-OSS-120B is
  the primary accuracy substrate) and replaces `tool_args` with the
  binder's structured output. Default `timeout_ms` is `500`; overruns
  drop the hop and pass through unchanged with
  `audit.binder.status = "timeout"`. Default `max_tokens` is `1024` so
  reasoning binders have room to emit structured JSON after their
  reasoning preamble. See the parent epic
  [#1696](https://github.com/burin-labs/harn/issues/1696) for the
  experimental contract.
- `std/llm/ensemble` — multi-call quality strategies: `best_of_n`,
  `self_consistency`, `parallel_judge`, `debate`. Cites Wang 2022
  (arxiv:2203.11171) and Du 2023 (arxiv:2305.14325).
- `std/llm/refine` — `refine_prompt`, `refine_caller`. One-shot
  meta-prompt rewrite with a `DIFF:` summary trailer.
- `std/llm/budget` — `estimate_text_tokens`, `context_window_for`,
  `recommend_max_output_tokens`, `budget_summary`, `fits_in_context`.
- `std/llm/economics` — `pricing_for(provider?, model)`,
  `estimate_call_cost`, `estimate_session_cost`, `compare_model_costs`,
  `cache_break_even`, `volume_cost`, `format_usd`. Unknown pricing
  surfaces as `pricing_known: false` / `cost_usd: nil` rather than $0;
  only providers explicitly configured to $0 (ollama, local, llamacpp,
  mlx, vllm, tgi) report cost=$0 with pricing_known=true.
- `std/llm/defaults` — `pack_for(opts)` and convenience wrappers
  (`pack_chat`, `pack_agent`, `pack_refine`, `pack_judge`,
  `pack_summarize`, `pack_code`, `pack_json`). Calibrated for
  Anthropic Sonnet/Opus/Haiku 4.x, OpenAI GPT-5/5.5/4o/4.1, Gemini
  2.5 Pro/Flash, Ollama Qwen3/Llama 3.x.
- `std/llm/safe` — `safe_call`, `safe_field`, `dict_get_ci`,
  `with_case_insensitive_keys`, `structured_envelope_or_default`,
  `judge_payload`, `verdict_normalize`, `schema_retry_nudge_for`.
- `std/llm/prompts` — `system_prelude`, `tool_use_prelude`,
  `structured_output_preface`.
- `std/llm/catalog` — `model_info(selector)`, `resolved_options(opts)`,
  `has_capability(model, cap)`, `family_of(model_id)`. Note:
  Harn-side names are `model_info` / `resolved_options` to avoid
  shadowing the same-named builtins.

Full reference: [`docs/src/stdlib/llm-handlers.md`](https://harnlang.com/stdlib/llm-handlers.html).

## Resilient LLM patterns

`llm_call` throws on transport / schema / budget failures. The thrown
value is a dict with the same fields `llm_call_safe` exposes under
`r.error`, so scripts can dispatch on a canonical LLM error taxonomy
without string-sniffing:

```harn
try {
  let r = llm_call(user_prompt, nil, opts)
} catch (e) {
  // e is {kind, reason, category, message, retry_after_ms?, provider, model}
  if e.kind == "transient" && e.reason == "rate_limit" {
    sleep(e.retry_after_ms ?? 1000)
    continue
  }
  throw e
}
```

Three helpers flatten the common recovery boilerplate:

```harn
// Non-throwing envelope: the ok/response/error shape eliminates the
// try/guard/unwrap/?.data boilerplate at every callsite.
let r = llm_call_safe(user_prompt, nil, opts)
if !r.ok {
  log("llm_call failed:", r.error.category, r.error.message)
  return nil
}
let data = r.response.data

// When the call is a JSON-against-schema extraction, prefer
// `llm_call_structured` / `*_safe` instead: `.data` is
// pre-unwrapped and the schema-validated-JSON options are forced
// by default (no boilerplate `output_validation` / `schema_retries`
// / `output_format` keys at each callsite).
let verdict = llm_call_structured(user_prompt, schema, {provider: "auto"})
// ...or non-throwing:
let r = llm_call_structured_safe(user_prompt, schema, {provider: "auto"})
if !r.ok { log("structured call failed:", r.error.category); return nil }
let data = r.data

// Scoped permit acquisition + backoff for flaky providers. Retries on
// rate_limit / overloaded / transient_network / timeout categories with
// exponential backoff (capped at 30s). Composes with
// HARN_RATE_LIMIT_<PROVIDER> and the providers.toml `rpm` field.
let r = with_rate_limit("openai", fn() {
  llm_call(user_prompt, nil, {provider: "openai", llm_retries: 0})
}, {max_retries: 5, backoff_ms: 500})
```

`error.category` (both on the thrown dict and on
`r.error.category`) remains for compatibility and is one of the
canonical `ErrorCategory` strings:
`"rate_limit"`, `"timeout"`, `"overloaded"`, `"server_error"`,
`"transient_network"`, `"schema_validation"`, `"auth"`, `"not_found"`,
`"circuit_open"`, `"tool_error"`, `"tool_rejected"`, `"cancelled"`,
`"generic"`. `retry_after_ms` is set when the provider surfaced a
`Retry-After` hint (or `llm_mock` was told to); otherwise omitted.

LLM provider failures also include `error.kind` and `error.reason`.
`kind` is `"transient"` or `"terminal"`. Transient reasons are
`"rate_limit"`, `"server_error"`, `"network_error"`, and `"timeout"`;
terminal reasons are `"auth_failure"`, `"context_overflow"`,
`"content_policy"`, `"invalid_request"`, `"model_unavailable"`, and
`"unknown"`. `llm_call` and `agent_loop` spend their retry budget only
when `kind == "transient"`.

Pair with `llm_mock({error: {category, message, retry_after_ms?}})` to
write deterministic tests for either helper's error path:

```harn
llm_mock({error: {category: "rate_limit", message: "429", retry_after_ms: 2500}})
try {
  llm_call("hi", nil, {provider: "mock", llm_retries: 0})
} catch (e) {
  assert(e.kind == "transient")
  assert(e.reason == "rate_limit")
  assert(e.category == "rate_limit")
  assert(e.retry_after_ms == 2500)
}

llm_mock({error: {category: "rate_limit", message: "429"}})
let r = llm_call_safe("hi", nil, {provider: "mock", llm_retries: 0})
assert(!r.ok)
assert(r.error.category == "rate_limit")
```

## Composable LLM callers

`agent_loop` accepts `llm_caller:` — a closure that owns the per-turn
`llm_call(...)` invocation. Wrap with middleware from
`std/llm/handlers` to compose retry / fallback / shadow / logging /
budget behavior without forking the loop:

```harn,ignore
import {default_llm_caller, with_retry, with_fallback, compose} from "std/llm/handlers"

let caller = compose([
  with_retry({max_attempts: 4, backoff: "exponential"}),
  with_fallback,    // pseudo: with_fallback expects a list of callers
])(default_llm_caller())

agent_loop(task, system, {loop_until_done: true, llm_caller: caller})
```

The caller signature is `fn(call) -> {ok, value | status, error?}`
where `call = {prompt, system, opts, turn: {iteration, session_id, attempt}}`.

**Off-by-one in retry semantics:** `llm_retries: 3` historically meant
4 total attempts; `with_retry`'s `max_attempts: N` means N total
attempts. To migrate `llm_retries: K`, pass `max_attempts: K + 1`.

**Persona-shaped chain (cost moat substrate):** the canonical compose
for a durable persona is cheap-by-default with frontier escalation,
deterministic budget enforcement, and receipt-grade structured logs.
`with_routing` is a **base** caller (it picks cheap vs. frontier);
budget and logging compose over it.

```harn,ignore
let router = with_routing({
  default: cheap,                                // fast inexpensive model
  routes: [{name: "frontier",
            when: { call -> call?.opts?.escalate ?? false },
            caller: strong}],                    // longer retries + fallback
})
let persona_caller = compose([
  with_logging({sink: receipts_sink}),
  with_budget({max_total_tokens: 250000, max_calls: 200}),
])(router)
```

Full reference: [`docs/src/stdlib/llm-handlers.md`](https://harnlang.com/stdlib/llm-handlers.html).

## First-class routing policy

`routing_policy({...})` builds a reusable handle that drives a chain of
providers with failover, latency-aware racing, and per-call / session
budget caps. Pipe it through `llm_call(... routing: policy ...)` to
replace ad-hoc `with_routing` + `with_retry` + `with_fallback`
compositions with a single typed primitive.

```harn,ignore
let policy = routing_policy({
  chain: [
    {provider: "anthropic", model: "claude-opus-4-20250514"},
    {provider: "openai",    model: "gpt-4o"},
    {provider: "ollama",    model: "llama4:70b"},      // local fallback
  ],
  failover: {
    on_status: [429, 500, 502, 503, 504],
    on_timeout_ms: 30_000,
    on_error_kinds: ["rate_limit", "schema_validation"],
    max_attempts: 3,
  },
  latency: {
    target_p95_ms: 8000,
    race_after_ms: 5000,                              // race backup after 5s
  },
  budget: {
    per_call_usd: 0.50,                               // hard ceiling per call
    session_usd: 5.00,                                // session-wide cap
    on_exceed: "abort",                               // or "skip" | "warn"
  },
  observe: {emit_event: "billing.routing_decision"},  // optional dispatch label
})

let result = llm_call("Summarize this PR.", nil, {routing: policy})
// result.routing = {policy, attempts: [{provider, model, status, duration_ms, cost_usd, error?}], selected, session_cost_usd}
```

Semantics:

- **Failover**: each link is tried in order; an attempt advances when
  the error matches `on_status` (HTTP code), `on_error_kinds`
  (category short-name — `rate_limit`, `timeout`, `transient_network`,
  `server_error`, `schema_validation`, `auth`, `overloaded`,
  `tool_error`, `tool_rejected`, `egress_blocked`, `cancelled`,
  `not_found`, `circuit_open`, `budget_exceeded`, `generic`), or the
  built-in transient defaults (429 / 5xx, rate-limit, overloaded,
  timeout, transient_network, server_error). Non-failover errors stop
  the chain immediately.
- **Racing**: when `race_after_ms` is set and a second link is
  available, the executor kicks off the next link in parallel after
  that delay; the loser is cancelled and recorded with
  `status: "race_lost"`.
- **Budgets**: `per_call_usd` and `session_usd` reuse the catalog
  pricing in `std/llm/economics`. `on_exceed: "abort"` throws the
  standard budget-exceeded error, `"skip"` advances to the next chain
  link, `"warn"` emits an event and proceeds.
- **Tape events**: `<dispatch>.decision`, `<dispatch>.attempt`,
  `<dispatch>.race_started`, `<dispatch>.race_won`,
  `<dispatch>.race_lost`, `<dispatch>.budget_exceeded`,
  `<dispatch>.exhausted` (default `dispatch = llm.routing`; override via
  `observe.emit_event`).
- **Replay**: the routing decision rides on the result envelope's
  `routing_decision` block, so transcripts and replay re-attribute each
  attempt to the same chain link without re-resolving.

The policy is a reusable handle: build it once, pass it to many
`llm_call` invocations.

## Composable tool middleware

`agent_loop` also accepts `tool_caller:` — the parallel seam for tool
**execution**. While `llm_caller` wraps the model call, `tool_caller`
wraps every tool dispatch. Combined with the `tools_use_middleware`
**schema-time** decorator, you get two composable seams that let you:

- force every tool call to provide a `reason` (or any other extra arg)
  that the harness reasons about, not the tool — and surface that reason
  as a user-facing chip ("Searched codebase to find rate limiter")
- add audit logs / consent prompts / dry-run preview / redaction /
  rate-limit / telemetry to all tool calls without touching individual
  tool definitions

```harn,ignore
import {
  with_required_reason, with_audit_log, with_consent,
  compose_tool_callers, tools_use_middleware,
} from "std/llm/tool_middleware"

let mw = with_required_reason({schema_required: false})
let registry = tools_use_middleware(my_registry, mw.schema_transform)

let caller = compose_tool_callers([
  with_audit_log({sink: "both", redact: ["token", "content"]}),
  with_consent({ call -> ask_human(call) }),
  mw.caller,
])

agent_loop(task, system, {tools: registry, tool_caller: caller})
```

`with_audit_log` emits typed `ToolCallReceipt` records with rationale,
status, timing, model/provider, and hashes instead of raw args/results.
Use `sink: "local"` for `.harn/receipts/<session_id>.jsonl`,
`sink: "cloud"` for host bridge mirroring, or `sink: "both"` for both
paths.

The caller signature is `fn(call, next) -> result_dict` where
`call = {tool_name, tool_args, call_id, declared_executor, schema, description, turn}`
and `next(call)` runs the default dispatch (with any envelope mutations
the layer applied — typically `tool_args` rewrites). Short-circuit by
returning a result dict without calling `next`. `call.turn.tool_call_index`
is the call's position in the turn's emitted batch — useful when middleware
fans out (`max_concurrent_tools > 1`) and needs to reorder completions
back to source order.

For multi-tool turns, set `max_concurrent_tools: N` on `agent_loop` to
fan out dispatch across siblings (capped at N). Middleware-backed
dispatch uses `parallel settle`; the host-batch path uses the same cap.
Each middleware sibling invokes its own caller chain in a fresh scope,
so `audit.layers` histories don't cross-talk. Results inject in source
order regardless of completion order so text tool-call parsers keep
working. `with_audit_log` receipts carry an `emit_order` field equal
to `turn.tool_call_index` so consumers can re-sort to source order
if they store events in completion order. Set `prefetch_next_turn:
true` to let the next planner call begin after tool results are in the
transcript while local/custom audit receipt sinks finish in the
background; the loop drains those flushes before returning.

Middleware-attached metadata rides on `result.audit` (free-form dict
aligned with A2A `metadata` / ACP `kind` / OpenAI `summary_text` / OTel
`gen_ai.tool.description` conventions). Each call also emits a
`tool_call_audit` AgentEvent so live ACP/A2A consumers can render
chips alongside the standard `tool_call_update` stream.

Full reference: [`docs/src/stdlib/tool-middleware.md`](https://harnlang.com/stdlib/tool-middleware.html).

### Catalogue-driven `run_command` hooks

`tool_rule`, `catalogue`, and `tool_hooks_registry` (TH-01) declare a
versionable corpus of "command faux-pas" rules — rewrite-able shell
mistakes (`find . -name`, `cargo build` without `--target-dir`,
`git push --force`, etc.) that any agent's `run_command` handler can
filter through. `preset_run_command(...)` (TH-02) is the shipped
wrapper that turns a registry into a tool handler.

```harn
import { preset_run_command, tool_hooks_mode_rewrite_with_audit } from "std/tool_hooks"

let rust_cat = catalogue({
  id: "harn-canon/rust",
  stack: "rust",
  rules: [
    tool_rule({
      id: "rust.cargo.target_dir",
      pattern: "^cargo (build|test)\\b",
      applies_to: ["rust"],
      severity: "warning",
      explanation: "use --target-dir to avoid lockfile thrash",
      rewrite: { command, _context -> command + " --target-dir target-shared" },
    }),
  ],
})
let registry = tool_hooks_register(tool_hooks_registry(), rust_cat)

let run_command = preset_run_command({
  stacks: ["rust"],
  registry: registry,
  custom_rules: [],                                 // matched before the registry
  mode: tool_hooks_mode_rewrite_with_audit,         // default
  inner: { args -> shell(args.command) },           // underlying executor
})
agent_loop(message, tools: {tools: [{name: "run_command", handler: run_command}]})
```

- `stacks` opts catalogues in via `tool_hooks_filter` (catalogues with
  no `stack` field are universal; per-rule `applies_to` filters further).
- `custom_rules` are matched before the registry so harness authors can
  unconditionally override registered behavior.
- Three shipped modes cover the epic's v1 contract:
  `tool_hooks_mode_rewrite_with_audit` (rewrite + run inner),
  `tool_hooks_mode_deny_with_explanation` (refuse to dispatch), and
  `tool_hooks_mode_passthrough_only_audit` (run inner unchanged, tag
  the result). All three return the same envelope shape so audit
  consumers can render them uniformly: `{action, command,
  original_command, rule_id, catalogue_id, severity, explanation,
  references, result?}`.
- Side effects (TH-03 #1896): each shipped mode records a
  `tool_rewrite` / `tool_denied` / `tool_rule_warning` lifecycle audit
  entry observable via `lifecycle_audit_log_take()`. The rewrite mode
  also queues a one-turn `tool_rewritten` system reminder via
  `tool_hooks_inject_reminder(...)` so the next agent turn sees the
  corrected command shape. When no agent session is active (headless
  pipelines, unit tests) the reminder still produces a
  `tool_hooks.reminder_injected` audit entry so conformance can verify
  the side effect either way. The underlying primitives
  `tool_hooks_emit_audit(kind, payload)` and
  `tool_hooks_inject_reminder({tags, body, ttl_turns, ...})` are
  exported for custom mode callbacks that want the same
  audit/reminder plumbing.
- Omit `inner` to get decision envelopes without execution — useful
  for previewing rewrites or testing rule coverage. The audit +
  reminder side effects still fire in preview mode.
- Catalogue auto-seed (TH-04 #1897): omit `registry` and the wrapper
  builds one from `stacks` via `tool_hooks_seed_registry(stacks)`. The
  universal catalogue (`git push --force main`, `rm -rf` against `/`,
  `~`, `..`, `$HOME`, `*`) is always included; per-stack catalogues
  ship for `rust`, `python`, `typescript` (aliased `ts`), `swift`,
  `sql`, and `harn`. Unknown stacks are silently skipped so callers
  opting into a future name don't break.
- Optional LLM classifier (TH-05 #1898): pass
  `llm_classifier: {model, threshold?, meta_prompt?, provider?, cache?, llm_options?}`
  to consult a small model on any command that didn't hit a
  deterministic rule. Verdicts at or above `threshold` (default 0.8)
  dispatch via the mode the verdict implies (`rewrite` →
  `tool_hooks_mode_rewrite_with_audit`, `deny` →
  `tool_hooks_mode_deny_with_explanation`); lower confidence or `allow`
  falls through to `inner` so the loop stays usable when the model is
  unsure. Every call emits a `tool_hook_classifier_verdict` audit
  (kind, confidence, scope, cache hit/miss, action) regardless of
  outcome. Cache TTL accepts `cache.ttl_ms` (preferred for tests) or
  `cache.ttl_seconds`. The classifier sends the raw command + meta
  prompt to the model, so redact secrets the same way `run_command`
  already requires; transport errors degrade gracefully to passthrough.

Full reference: [`docs/src/tool-hooks.md`](https://harnlang.com/tool-hooks.html).
Recipes per stack: [`docs/src/cookbooks/tool-hooks.md`](https://harnlang.com/cookbooks/tool-hooks.html).
Contributing rules: [`docs/src/contributing/preset-hooks.md`](https://harnlang.com/contributing/preset-hooks.html).

## Cancellation

`llm_call` and `agent_loop` cooperate with the VM's cancellation token,
which the host raises on Ctrl-C, `cancel(task)` inside a Harn program,
or an ACP `session/cancel` request:

- **Mid-`llm_call`**: the in-flight HTTP request is dropped
  (best-effort) and the call returns a thrown
  `VmError::Thrown(cancelled)` that bubbles out of the enclosing
  pipeline. Non-throwing callers can use `llm_call_safe` to catch it
  as `{ok: false, error.category: "cancelled"}`.
- **Mid-tool-call inside `agent_loop`**: the tool's async handler sees
  the same cancellation token; async builtins that opted in
  (`llm_call`, `http_*`, `sleep`, …) short-circuit immediately. The
  loop finalizes the transcript with the partial turn and exits with
  `status: "cancelled"`.
- **Between turns in `agent_loop`**: the next iteration never starts;
  the loop returns with its current iteration count, the accumulated
  transcript, and `status: "cancelled"`. Persistent sessions remain
  usable — re-invoke `agent_loop` with the same `session_id` to
  resume.

`done_sentinel`, `max_iterations`, and `token_budget` each produce
their own non-cancellation statuses; the cancellation path is
specifically for external interruption.

## Rate limiting

Per-provider RPM limiting is built in:

- Set `rpm: 600` in the provider entry in `providers.toml` /
  `harn.toml`.
- Or `HARN_RATE_LIMIT_<PROVIDER>=600` env var (e.g.
  `HARN_RATE_LIMIT_TOGETHER=600`, `HARN_RATE_LIMIT_LOCAL=60`). Env
  overrides config.
- Or `llm_rate_limit("provider", 600)` at runtime.
- Wrap individual call sites in `with_rate_limit(provider, fn, opts?)`
  to acquire a permit and auto-retry retryable failures.

RPM shapes sustained throughput; `max_concurrent` caps simultaneous
in-flight work. Use both when batching LLM calls at scale.

## Cache (`std/cache`)

Content-addressed cache with three backends and a composable wrapper:

```harn
import { mem_cache, fs_cache, sqlite_cache, with_cache } from "std/cache"

let store = sqlite_cache(state_path("evals.sqlite"), {ttl: "1h"})
let answer = with_cache("key", { -> heavy_work() }, {store: store})
```

- `mem_cache(opts?)` — thread-local LRU. Does not survive `harn run`.
- `fs_cache(path, opts?)` — one JSON file per key under `<path>/<namespace>/`.
- `sqlite_cache(path, opts?)` — single sqlite file; many namespaces share it.

Common options: `namespace`, `ttl` (string like `"10m"`) or `ttl_seconds`,
`max_entries` (LRU bound). TTL honors the unified clock.

`with_cache` is also a composable middleware in `std/llm/handlers` — drop
it into `compose([...])` to deduplicate identical `(prompt, system, opts)`
LLM calls. Tool-bearing calls bypass the cache by default.

On a cache hit with `options.session_id` set, both the caller-wrapper
and direct-call forms emit `cache_hit` + receipts (`model_calls_avoided`,
`tokens_saved`, `latency_saved_ms`) on the agent event tape. The persona
value ledger and crystallization receipts read these back.

Full reference: [`docs/src/stdlib/cache.md`](https://harnlang.com/stdlib/cache.html).

## Per-harness net policy (`std/net_policy`)

Attach an allowlist/denylist to one harness so its `harness.net.*`
calls (added by E4.4 / #1769) get gated against your rules. Returns a
new `Harness` value bound to the policy — the source handle stays
unrestricted, so policies are scoped by where you rebind, not by
mutating shared state. Tracked through harn#1913 / epic #1765.

```harn,ignore
import { create, domain, domain_wildcard, cidr, host } from "std/net_policy"

let policy = create({
  allow: [
    domain("github.com"),
    domain_wildcard("*.github.com"),
    cidr("10.0.0.0/8"),
    host("api.anthropic.com", [443]),
  ],
  deny: [domain_wildcard("*.competitor.com")],
  default: "deny",                                    // or "allow"
  on_violation: "error",                              // or "audit_only", "quarantine",
                                                      // or a fn(req) returning one of those
})
let restricted = harness.with_net_policy(policy)
restricted.net.get("https://github.com/foo")          // allowed
restricted.net.get("https://example.test/blocked")    // throws NetPolicyViolation
restricted.is_quarantined()                           // sticky after a quarantine deny
```

- Rule precedence: `deny` rules fire first, then `allow`, then the
  `default`. A typed `NetPolicyViolation` (`{type, category, host,
  port, reason, outcome, matched_rule}`) is thrown for `error` /
  `quarantine` outcomes; `audit_only` still records the audit and
  lets the request through.
- `on_violation` callbacks receive a `{method, url, host, port,
  reason, matched_rule}` envelope and must return one of `"error"`,
  `"audit_only"`, `"quarantine"` (returning a closure is rejected).
- Every evaluation — including the `HARN_NET_POLICY_BYPASS=1`
  short-circuit — emits a `harness.net.policy.audit` event so the
  trust graph keeps an evidence trail.
- The matcher is mock-aware: in mock mode the policy runs ahead of
  the canned-response lookup, so conformance fixtures exercise the
  same matcher path as production without touching the network.

## Authentication (OAuth)

Harn ships a full OAuth stack: provider catalogue, five interchangeable
storage backends, an authorization-code client with PKCE + transparent
refresh, RFC 8628 device flow, RFC 7591 dynamic registration, and a
token-redaction catalog. The five modules under `std/oauth/*` compose
freely — pick a provider, pick a storage, then pick a grant.

```harn,ignore
import { providers } from "std/oauth/providers"             // github, slack, linear, notion,
                                                            // google, microsoft, atlassian,
                                                            // discord, gitlab, bitbucket,
                                                            // github_enterprise, custom
import { memory } from "std/oauth/storage"                  // memory, file, harn_cloud_*,
                                                            // custom
import { client, request, token } from "std/oauth/client"   // RFC 6749 + 7636 + 9700
import { device_flow } from "std/oauth/device_flow"         // RFC 8628 (CI / headless)
import { register_pattern } from "std/oauth/redaction"      // HARN-OAU-001 catalog
```

Full reference + per-provider cookbook: [`docs/src/oauth.md`](https://harnlang.com/oauth.html).

## OAuth client (`std/oauth/client`)

RFC 6749 authorization-code + RFC 7636 PKCE S256 + RFC 9700
transparent refresh. Build on top of `std/oauth/providers` and
`std/oauth/storage`; the client knows nothing about which storage
backend it's holding.

```harn
import { providers } from "std/oauth/providers"
import { memory } from "std/oauth/storage"
import { client, exchange_code, start_authorization, token, request } from "std/oauth/client"

let cli = client(
  providers().github,
  {
    client_id: env("GH_CLIENT_ID"),
    client_secret: env("GH_CLIENT_SECRET"),
    scopes: ["read:user", "user:email"],
    redirect_uri: "http://127.0.0.1:8765/callback",
    storage: memory(),
  },
)

// One-shot authorization-code dance (host drives the browser):
let pkce = start_authorization(cli)            // pkce.url, pkce.state, pkce.code_verifier
let token_set = exchange_code(cli, pkce, code, state)

// Subsequent calls auto-refresh past 75% TTL:
let access = token(cli)                        // -> string, valid access token

// Or let the client own HTTP, with 1x retry on 401:
let response = request(cli, "GET", "https://api.github.com/user")
```

- **PKCE always enforced.** `start_authorization` generates a fresh
  64-byte CSPRNG verifier (base64url-no-pad → ~86 chars) and a SHA-256
  S256 challenge. `code_challenge_method=S256` is hardcoded.
- **State always enforced.** `exchange_code` raises on `state`
  mismatch before issuing the token request.
- **Refresh transparency.** `token(cli)` re-reads storage every call
  and refreshes if the stored TokenSet is past 75% TTL or already
  expired. `request(cli, ...)` additionally retries once on 401
  (forces a refresh between attempts).
- **Audit log.** Every successful refresh / exchange emits
  `oauth.client.audit` with `token_refreshed` / `token_exchanged`. The
  payload carries presence flags + expiry timestamps; it never
  includes the new access or refresh token.
- **Concurrency.** Storage is the source of truth. Two concurrent
  `token(cli)` calls may both observe staleness and both refresh; the
  later `set` wins and both callers see the same token. Pre-refresh
  at 75% TTL keeps that window narrow.
- **Storage key.** Defaults to `provider.id`; pass `storage_key` to
  fan out multiple installations of the same provider.

Full reference: conformance fixtures at
`conformance/tests/stdlib/oauth_client_*.harn`.

## OAuth storage (`std/oauth/storage`)

Token store for the OAuth client with five interchangeable backends.
Every handle is a dict with three closures (`get`, `set`, `delete`) so
the client doesn't know the difference between in-process memory, an
encrypted file, harn-cloud, or a vault.

```harn
import { memory, file, harn_cloud_session, harn_cloud_org, custom } from "std/oauth/storage"

let mem = memory()                                       // ephemeral
let disk = file("/var/lib/harn/oauth.bin", env("KEY"))   // AES-256-GCM
let cloud = harn_cloud_session()                          // per-session
let shared = harn_cloud_org()                             // org-scoped
let vault = custom({get: my_get, set: my_set, delete: my_delete})

mem.set("github", {access_token: "abc"}, 3600)
let token = mem.get("github")                            // -> TokenSet | nil
mem.delete("github")
```

- `memory()` lives in a thread-local map and never escapes the VM.
- `file(path, key)` writes a single AES-256-GCM envelope; the 32-byte
  AEAD key is derived via HKDF-SHA256 from `key`. Pass high-entropy
  bytes, not a user passphrase.
- `harn_cloud_*()` route through the `oauth_storage` host capability
  (`cloud_get / cloud_set / cloud_delete`); harn-cloud enforces RLS.
- `custom({get, set, delete, id?})` validates that the three handlers
  are callables and then dispatches to them. Closure capture is
  by-value, so back the closures with a real store (HTTP, MCP,
  `harn-cloud`) rather than a captured local.

Full reference: [`docs/src/stdlib/oauth-storage.md`](https://harnlang.com/stdlib/oauth-storage.html).

## OAuth device flow (`std/oauth/device_flow`)

RFC 8628 device authorization grant for headless contexts (CI runners,
daemons, IDE side panes). Persists the TokenSet into the same storage
the authorization-code client reads from, so subsequent
`OAuth.client(...)` calls see the same token without re-running the
dance.

```harn,ignore
import { device_flow } from "std/oauth/device_flow"
import { providers } from "std/oauth/providers"
import { file } from "std/oauth/storage"

let token_set = device_flow(providers().github, {
  client_id: env("GH_CLIENT_ID"),
  scopes: ["read:user", "repo"],
  storage: file("/var/lib/harn/ci.bin", env("HARN_OAUTH_KEY")),
  on_user_code: { user_code, verification_uri ->
    log("Open " + verification_uri + " and enter " + user_code)
  },
})
```

- **Polling honors the server's `interval`.** `authorization_pending`
  is treated as a soft retry; `slow_down` bumps the interval by 5s;
  `expired_token` and `access_denied` raise.
- **Cancellable.** Each inter-poll sleep is a cancellable point.
- **Time-mock-friendly.** Polling routes through `sleep(ms)`, which
  honors `mock_time(...)` / `advance_time(...)` for tests.
- **Audit.** `oauth.device_flow.audit` `token_obtained` with presence
  flags only — never the `device_code` / `user_code` / access tokens.
- **Provider support.** GitHub, Google, Microsoft, GitLab — the rest
  of the catalog has `device_code_url: nil` and will raise.

## OAuth dynamic registration (`std/oauth/dynamic_registration`)

The server side of OAuth. Build RFC 7591 client metadata + RFC 8414
authorization-server metadata, validate incoming registrations, and
issue `client_id` / `client_secret` pairs from an in-process store.
Embedders (`harn serve`, harn-cloud, custom hosts) mount the well-known
endpoints + the registration handler; this module does not host HTTP
itself.

```harn,ignore
import { providers } from "std/oauth/providers"
import {
  authorization_server_metadata, client_metadata, dynamic_registration_store,
  register_client, validate_metadata, well_known_paths, well_known_response,
} from "std/oauth/dynamic_registration"

let paths = well_known_paths()  // {client_metadata, authorization_server_metadata, registration}
let oas = authorization_server_metadata(providers().github, {registration_endpoint: paths.registration})
let envelope = well_known_response(oas)  // {status, content_type, headers, body}

let store = dynamic_registration_store()
let result = register_client(store, {redirect_uris: ["https://app.example/cb"]})
// result.client_id, result.client_secret (returned ONCE), result.client_id_issued_at
```

- **Strict validation.** `redirect_uris` must be absolute `https://` or
  loopback `http://` per RFC 8252 §7.3; grant / response types and
  `token_endpoint_auth_method` are restricted to spec-blessed enums.
  Each validation error is prefixed `HARN-OAU-005:` for pattern
  matching.
- **Secret returned once.** `register_client` includes `client_secret`;
  `get_client(store, id)` does not. Audit events carry counts only —
  never the secret.
- **Validation surface.** `validate_metadata(metadata)` returns
  `{ok: bool, errors: list<string>}` without registering.

## OAuth redaction (`std/oauth/redaction`)

Runtime ships a default catalog of high-confidence token patterns (JWT,
GitHub PAT classic + fine-grained, Slack `xox*`, AWS `AKIA`, OpenAI
`sk-`, Stripe `sk_live_`/`sk_test_`, GitLab `glpat-`, npm `npm_`,
`Authorization: Bearer ...`). Persisted transcripts / receipts /
OTel attrs / system reminders replace matches with
`<redacted:<pattern>:<len>>`. The original token still flows to the
underlying tool — redaction is display-only.

```harn,ignore
import { default_patterns, drain_audit, redact, register_pattern } from "std/oauth/redaction"

register_pattern("acme_api_key", "\\bACME-[A-Z0-9]{12}\\b")
let display = redact("Bearer ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
for entry in drain_audit() {
  // entry.code == "HARN-OAU-001"
  // entry.pattern, entry.match_count, entry.bytes_redacted
}
```

- **Per-thread custom patterns** via `register_pattern(name, regex)`.
  Anchor with `\b` to avoid chewing unrelated identifiers.
- **`drain_audit()` is the authoritative compliance contract** — works
  on every execution backend. Audit entries also fan out to the live
  event sink and (when multi-threaded Tokio is available) to the
  `audit.token_redaction` event-log topic.

## Gotchas (friction-log distilled)

- Heredoc `<<TAG ... TAG` is **not** a source-level string. Use
  `"""..."""`. The parser emits a targeted error pointing here.
- `substring(s, start, length)` takes a **length**, not an end index.
  Prefer `s[start:end]` slicing.
- Do NOT add `trailing_var_arg = true` to `RunArgs.argv` in clap — it
  conflicts with `last = true` at runtime. `last = true` alone is
  sufficient for `harn run script.harn -- a b c`.
- Don't set `minLength` on optional-feeling schema fields like
  `improvement`. Small models often leave them blank, and validation
  will fail every time. Use the system prompt to demand non-empty
  strings instead.
- On `llm_call`, `provider: "auto"` with `model: "local:foo"` strips
  the `local:` prefix and routes to Ollama. Without `"auto"`, an
  explicit provider such as `"local"` still wins.
- `schema_retries` retries schema-validation failures with a
  corrective nudge. `llm_retries` (deprecated; prefer `with_retry`)
  retries transient provider errors. They compose orthogonally —
  each schema retry starts a fresh transient budget.
- A schema retry is a **single-turn correction**, not a multi-turn
  conversation. The invalid response is not persisted; the retry
  replays the original messages with one appended user-role correction
  that cites the validation errors and the schema. For cost / cache
  purposes, treat the retry as one extra prompt+response on the same
  prefix as the original call (not a growing conversation). The
  correction text is surfaced on the `SchemaRetry` trace event as
  `correction_prompt`.
- Module-level `var` cross-fn mutation is not shared yet. Prefer
  atomics (`atomic(0)` / `atomic_add`) for shared counters.
- Small / local models benefit heavily from:
  1. Wrapping judge input in `<transcript_to_grade>...</transcript_to_grade>`.
  2. Forcing canonical start tokens (`Start with VERDICT:`).
  3. `output_validation: "error"` + `schema_retries: 2`.
  4. Generous `maxLength` / `maxItems` bounds in the schema.

## Prompt templates (`.harn.prompt` / `.prompt`)

Load file-backed templates via `render("path.prompt", bindings)` or
`render_prompt(...)`. Use `render_string(template, bindings)` when the
template source lives inline in a string literal. File paths resolve relative
to the calling module's directory.

**Package-root paths** — prefer `@/...` and `@<alias>/...` over
`../../partials/foo.harn.prompt`. They anchor at the calling file's
project root (nearest `harn.toml`) so refactors that move callers don't
break asset references:

```harn,ignore
render_prompt("@/prompts/tool-examples.harn.prompt", bindings)  // project-root
render_prompt("@partials/tool-examples.harn.prompt", bindings)  // [asset_roots] alias
```

Define aliases in `harn.toml`:

```toml
[asset_roots]
partials = "Sources/BurinCore/Resources/pipelines/partials"
```

Both `render_prompt(...)` and `{{ include "@/..." }}` honor the same
syntax. `harn check` validates the resolved files exist; bundle manifests
and LSP go-to-definition follow `@`-paths to the target file.
When an execution policy is active, file-backed templates and includes obey
the same `workspace_roots` read boundary as `read_file(...)`.

- `{{ name }}` — interpolation; nested with `{{ a.b[0] }}`.
- `{{ if expr }}..{{ elif expr }}..{{ else }}..{{ end }}` — expression
  operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `and`/`&&`, `or`/`||`,
  `not`/`!`.
- `{{ for x in xs }}..{{ else }}..{{ end }}` — `else` renders when empty.
  Inside: `{{ loop.index }}`, `.index0`, `.first`, `.last`, `.length`.
  Dict iteration: `{{ for k, v in dict }}..{{ end }}`.
- `{{ include "partial.prompt" }}` or `{{ include "..." with { x: y } }}`
  — resolves relative to the including file; `{{ include "@/..." }}`
  resolves from the project root; cycle detection is built in.
- Filters: `{{ name | upper | default: "anon" }}`. Built-ins: `upper`,
  `lower`, `title`, `trim`, `capitalize`, `length`, `first`, `last`,
  `reverse`, `join:sep`, `default:fallback`, `json`, `indent:n`, `lines`,
  `escape_md`, `replace:from,to`.
- `{{# comments stripped at parse #}}`,
  `{{ raw }}..literal {{braces}}..{{ endraw }}`,
  `{{- trim whitespace + one newline -}}`.
- Missing *bare* `{{ident}}` passes through the literal source (back-compat).
  New constructs raise `template at L:C: ...` errors.
- **`llm` scope**: inside an LLM-aware frame (`llm_call`, the default
  handler stack, `agent_loop`) the engine auto-injects
  `llm = {provider, model, family, capabilities: {...}}` so a single
  logical prompt can adapt by capability. Branch on `{{ if llm }}` for
  the bare-render fallback; branch on `{{ if llm.capabilities.native_tools }}`
  to pick wire envelope. `family` is one of `claude` / `gpt` / `gemini` /
  `qwen` / `llama` / `mistral` / `deepseek` / `phi` / `grok` / `command`.
  User bindings that already provide an `llm` key win for back-compat
  and trigger a one-shot warning under `template.llm_scope`.
- **Variant resolution transcripts**: a `template.render` event lands in
  `llm_transcript.jsonl` for every render under an LLM frame, carrying
  the resolved `llm` snapshot and a per-branch / per-section trace.
  Surface in the portal under "Variant resolution".
- **Drift-prevention lints**: `harn lint` walks `.harn.prompt` files and
  warns when a template branches on `llm.provider` / `llm.model` /
  `llm.family` directly (`template-provider-identity-branch`) or when
  more than three capability-aware conditionals appear in the same file
  (`template-variant-explosion`). Configure the threshold via
  `[lint] template_variant_branch_threshold = N`.
- Full reference: `docs/src/prompt-templating.md`.

## Discovery

- Human cheatsheet: `docs/src/scripting-cheatsheet.md`.
- Language spec: `spec/HARN_SPEC.md` (mirrored to
  `docs/src/language-spec.md`).
- Concurrency: `docs/src/concurrency.md` (`max_concurrent`, RPM
  limits, channels, `select`, `deadline`).
- LLM / agent surface: `docs/src/llm-and-agents.md`.
- Conformance examples: `conformance/tests/*.harn`.
