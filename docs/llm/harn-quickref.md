# Harn Quick Reference (LLM-friendly)

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

## Files and execution

- File extension: `.harn`.
- Entry points: a file can either declare `pipeline default() { ... }`
  (pipeline mode — `compile_top_level_declarations` runs first, then
  the pipeline body) or be a bare script with top-level statements.
- Run: `harn run script.harn`.
- Inline: `harn run -e 'println("hi")'`.
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

## stdin / stdout / stderr / TTY

- `print(s)` / `println(s)` → stdout. `eprint(s)` / `eprintln(s)` →
  stderr.
- `read_stdin()` slurps the rest of stdin to a `string`. `read_line()`
  reads one line (without trailing newline). Both return `nil` at EOF.
- `is_stdin_tty()`, `is_stdout_tty()`, `is_stderr_tty()` — `bool`,
  uses `std::io::IsTerminal`. Use these to decide between rich
  interactive UI and pipe-friendly output.
- `set_color_mode("auto"|"always"|"never")` controls whether
  `color`/`bold`/`dim` emit ANSI. Auto honors `NO_COLOR` and
  `FORCE_COLOR` env vars and only emits when stdout is a TTY.

In tests: `mock_stdin(text)` / `unmock_stdin()`,
`mock_tty(stream, bool)` / `unmock_tty()`,
`capture_stderr_start()` / `capture_stderr_take()`.

## Time, sleep, monotonic clock

- `now_ms()` — wall-clock millis since UNIX_EPOCH (`int`).
- `monotonic_ms()` — monotonic millis since process start (`int`).
- `sleep_ms(n)` — async sleep. **Mock-aware**: under `mock_time`, this
  advances mocked time instantly instead of blocking — so tests of
  retry/backoff/timeout logic stay deterministic and fast.
- `mock_time(ms)` / `advance_time(ms)` / `unmock_time()` —
  `timestamp` and `elapsed` also route through this clock, so
  every time-sensitive builtin is mockable.

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
println(s[0:5])        // "hello"
println(s[6:11])       // "world"

let xs = [1, 2, 3, 4, 5]
println(xs[1:4])       // [2, 3, 4]
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
println(outcome.succeeded)  // count
println(outcome.failed)
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
  println("${k}: ${v}")
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
```

`regex_replace` and `regex_replace_all` both replace every match and
both support `$1`, `$2`, `${name}` backrefs.

## Encoding, Bytes, and Compression

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
println(response.prose)          // unwrapped prose (text minus tags)
println(response.text)           // raw provider text (may include tags)
println(response.canonical_text) // canonical tagged reconstruction
println(response.input_tokens)
println(response.output_tokens)
```

### `llm_call` options

| Option | Type | Default | Notes |
|---|---|---|---|
| `provider` | string | `"auto"` | `"auto"` infers from `model` (`local:*` → local, `/` → openrouter, `claude-*` → anthropic, `gpt-*` → openai, `llm:` → ollama). Explicit wins. |
| `model` | string | (inferred) | `local:gemma-4-e4b-it` routes through local. |
| `max_tokens` | int | 4096 | |
| `temperature` | float | provider default | |
| `tools` | list | nil | Registered tool schemas. |
| `tool_search` | bool \| string \| dict | nil | Engage progressive tool disclosure. Shorthand `"bm25"` / `"regex"` (variant, mode auto). Dict: `{variant: "bm25" \| "regex", mode: "auto" \| "native" \| "client", strategy: "bm25" \| "regex" \| "semantic" \| "host", always_loaded: [string], budget_tokens: int, name: string, include_stub_listing: bool}`. See "Tool loading & search" below. |
| `response_format` | string | nil | `"json"` asks the provider for JSON mode. |
| `output_schema` | `Schema<T>` (dict \| type-alias) | nil | JSON-schema-shaped dict, or a top-level `type T = ...` alias (compiler lowers to the schema dict). The generic parameter `T` flows into the narrowed `r.data: T`. Validated after parse. |
| `output_validation` | string | `"off"` | `"error"` throws on mismatch; `"warn"` logs. |
| `schema_retries` | int | 1 | When validation fails, re-prompt up to N times with a corrective user turn. Each retry is a single-turn correction — the invalid response is NOT persisted; the original messages are replayed with one appended user-role correction citing the validation errors + schema. Works alongside `output_validation: "error"`. |
| `schema_retry_nudge` | string \| bool | auto | String = verbatim corrective message (+ validation errors appended). `true` = auto nudge from schema required/properties keys. `false` = bare retry — replays the original messages unchanged, no correction appended. |
| `llm_retries` | int | 2 | Retries on transient HTTP / provider errors. Set to 0 for fail-fast. |
| `llm_backoff_ms` | int | 2000 | Base exponential backoff. |
| `stream` | bool | true | SSE streaming transport. |

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
  tool_search: "bm25",                 // or "regex"
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
  whose handler runs BM25/regex/semantic/host in-VM or through the
  bridge, then promotes the matching deferred tools into subsequent
  turns' schema list.
- `tool_search: "regex"` uses the Python-regex variant
  (`tool_search_tool_regex_20251119`) on Anthropic, or an
  in-VM case-insensitive Rust-regex search on everything else.
- `tool_search: {mode: "native"}` refuses to silently downgrade —
  errors if the provider isn't natively capable.
- `tool_search: {mode: "client"}` forces the client-executed path
  even on providers with native support (useful for debuggability on
  GPT 5.4+, where the hosted path hides search deltas in the usage
  accounting).
- `tool_search: {strategy: "bm25" | "regex" | "semantic" | "host"}`
  (client mode only) picks the implementation. `"semantic"` and
  `"host"` delegate to the host via the `tool_search/query` bridge
  RPC so integrators can wire embeddings without Harn pulling in ML
  crates.
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
tool_search = ["hosted"]
thinking_modes = ["effort"]
```

Query the effective matrix at runtime:

```harn
let caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   provider: "anthropic", model: "claude-opus-4-7",
//   native_tools: true, defer_loading: true,
//   tool_search: ["bm25", "regex"], max_tools: 10000,
//   prompt_caching: true, thinking: true,
//   thinking_modes: ["adaptive"],
// }

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

Rule schema (per `[[provider.<name>]]` entry):

| Field | Type | Purpose |
|---|---|---|
| `model_match` | glob string | Required. Matched against lowercased model ID. |
| `version_min` | `[major, minor]` | Optional lower bound; parsed via Claude / GPT version extractors. |
| `native_tools` | bool | Native tool-call wire shape supported. |
| `defer_loading` | bool | Provider honors `defer_loading: true` on tool defs. |
| `tool_search` | `[string]` | Native variants (`["bm25", "regex"]` or `["hosted", "client"]`). Empty = no native support. |
| `max_tools` | int | Cap on tool count (used by `harn lint`). |
| `prompt_caching` | bool | `cache_control` blocks honored. |
| `thinking_modes` | `[string]` | Supported script-facing modes: `enabled`, `adaptive`, `effort`. |

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
defaults (`response_format: "json"`, `output_validation: "error"`,
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
println(verdict.verdict)
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
println(r.data.verdict)
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
  println(r.data.verdict)
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

If you need the raw response (token counts, transcript, thinking
trace) alongside the parsed data, call `llm_call` directly:

```harn
let r = llm_call(prompt, sys, {
  provider: "auto",
  model: "local:gemma-4-e4b-it",
  output_schema: schema,
  output_validation: "error",
  schema_retries: 2,
  response_format: "json",
})
println(r.data.verdict)
println(r.input_tokens)
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
println(out.verdict)     // narrowed to GraderOut
```

Reusable generic wrapper (narrows via the `Schema<T>` generic
param):

```harn
fn grade<T>(prompt: string, schema: Schema<T>) -> T {
  return llm_call_structured(prompt, schema, {provider: "auto"})
}

let out: GraderOut = grade("Grade this", schema_of(GraderOut))
println(out.verdict)
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
    response_format: "json",
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

### `agent_loop`

`agent_loop(prompt, system?, options?)` runs a multi-turn loop with
tool dispatch. Completion uses the `##DONE##` sentinel: tagged
text-tool stages emit `<done>##DONE##</done>`, while no-tool and
native-tool stages emit bare `##DONE##`. The sentinel is configurable
via `done_sentinel`.

Returns a namespaced dict: top-level `status`, `text`, `visible_text`
(last iteration's prose with tool calls stripped), `task_ledger`,
`transcript`, `daemon_state`, `daemon_snapshot_path`, `trace`, and
`deferred_user_messages`; LLM execution metrics nested under `llm`
(`iterations`, `duration_ms`, `input_tokens`, `output_tokens`); tool
invocation data nested under `tools` (`calls`, `successful`, `rejected`,
`mode`). Respects the same `llm_retries` / `llm_backoff_ms` options
as `llm_call`, plus its own `profile`, `tool_retries`,
`max_iterations`, `max_nudges`, and `native_tool_fallback`
(`"allow"`, `"allow_once"`, or `"reject"` for native-tool stages that
receive text-mode `<tool_call>` fallback output).

Profiles preload common loop budgets and retry counts. Explicit keys
override the profile:

| Profile | `max_iterations` | `max_nudges` | `tool_retries` | `llm_retries` | `schema_retries` |
|---|---:|---:|---:|---:|---:|
| `tool_using` (default) | 50 | 8 | 0 | 2 | 0 |
| `researcher` | 30 | 4 | 0 | 2 | 0 |
| `verifier` | 5 | 0 | 0 | 2 | 3 |
| `completer` | 1 | 0 | 0 | 2 | 0 |

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
- `agent_session_compact(id, opts)` — unknown keys in `opts` error.
- `agent_session_length(id)` / `_snapshot(id)` / `_ancestry(id)` for read-only inspection.

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

Options: `headers: bool` (default false), `delimiter: ","`. Without
headers, `csv_parse` returns list-of-lists; with headers, list of
dicts (keys are sorted on stringify for determinism).

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

### Human-in-the-loop builtins

These are typed stdlib primitives, not language syntax. Shared type aliases
live in `std/hitl`; the builtins themselves are always available.

- `ask_user<T>(prompt, {schema?: Schema<T>, timeout?: duration, default?: T}) -> T`
- `request_approval(action, {detail?, quorum?, reviewers?, deadline?}) -> {approved, reviewers, approved_at, reason, signatures}`
- `dual_control<T>(n, m, action: fn() -> T, approvers?) -> T`
- `escalate_to(role, reason) -> {request_id, role, reason, trace_id, status, accepted_at, reviewer}`
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

Workflow stages pick up a session id from `model_policy.session_id`;
two stages sharing an id share their conversation automatically. The
pre-0.7 `transcript_policy` dict (with `mode: "reset" | "fork"`) was
removed — call the lifecycle verbs explicitly.

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
// / `response_format` keys at each callsite).
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
- On `llm_call`, `provider: "auto"` with `model: "local:foo"` routes
  to the local provider. Without `"auto"`, explicit `"local"` wins.
- `schema_retries` retries schema-validation failures with a
  corrective nudge. `llm_retries` retries transient provider errors.
  They compose orthogonally — each schema retry starts a fresh
  transient budget.
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
- Full reference: `docs/src/prompt-templating.md`.

## Discovery

- Human cheatsheet: `docs/src/scripting-cheatsheet.md`.
- Language spec: `spec/HARN_SPEC.md` (mirrored to
  `docs/src/language-spec.md`).
- Concurrency: `docs/src/concurrency.md` (`max_concurrent`, RPM
  limits, channels, `select`, `deadline`).
- LLM / agent surface: `docs/src/llm-and-agents.md`.
- Conformance examples: `conformance/tests/*.harn`.
