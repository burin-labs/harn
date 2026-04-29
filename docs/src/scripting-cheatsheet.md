# Scripting Cheatsheet

A compact, prose-friendly tour of everything you need to write real
Harn scripts. The companion one-page LLM reference is at
[`docs/llm/harn-quickref.md`](https://harnlang.com/docs/llm/harn-quickref.html)
(published in the mdBook) — they cover the same
ground with different shapes, and should stay in lockstep. Agents that
can fetch URLs should prefer the quickref.

## Strings

Use standard double-quoted strings with `\n` escapes for short
literals, and triple-quoted `"""..."""` for multiline prose like
system prompts:

```harn
let greeting = "Hello, ${name}!"
let prompt = """
You are a strict grader.
Emit exactly one verdict.
"""
```

Heredoc-style `<<TAG ... TAG` is **only** valid inside LLM tool-call
argument JSON — in source code, the parser points you at triple
quotes.

## Prompt templates

Use `render("file.prompt", bindings)` / `render_prompt(...)` for
source-relative prompt assets, and `render_string(template, bindings)`
when the template should live inline in the module:

```harn
let template = """
pub fn {{ fn_name }}({{ for p in params }}{{ p }}{{ if !loop.last }}, {{ end }}{{ end }}) {
  return "{{ fn_name }}"
}
"""

let src = render_string(template, {
  fn_name: "hello",
  params: ["name", "title = nil"],
})
```

The template language is the same either way: `{{ if }}`, `{{ for }}`,
filters like `| upper` / `| default: ...`, `{{ include "..." }}` for
file-backed partials, comments, raw blocks, and whitespace trimming.
See `prompt-templating.md` for the full reference.

## Slicing

End-exclusive slicing works on strings and lists:

```harn
let head = content[0:400]
let tail = content[len(content) - 400:len(content)]
let sub = xs[1:4]
```

`substring(s, start, length)` exists too, but the third argument is a
**length**, not an end index. Prefer the slice syntax to avoid that
footgun.

## `if` is an expression

`if` / `else` produces a value. Drop it straight into `let`, an
argument, or a `return`:

```harn
let body = if len(content) > 2400 {
  content[0:400] + "..." + content[len(content) - 400:len(content)]
} else {
  content
}
```

## Streams

`stream.*` accepts lists, ranges, channels, generators, and lazy
`iter(...)` values. Operators stay lazy until a sink such as
`stream.collect`, `stream.fold`, or `stream.first` pulls from them.

```harn
let first_three = stream.collect(stream.take(results_channel, 3), {max: 3})

let tool_events = stream.collect(
  stream.filter(agent_events, { ev -> ev?.topic == "tool_call" }),
  {max: 100}
)

let winner = stream.first(stream.race(primary_stream, fallback_stream))

let total = stream.fold(
  stream.merge(worker_a, worker_b, worker_c),
  0,
  { acc, item -> acc + item.cost }
)
```

Always pass a realistic `{max: N}` to `stream.collect` when the upstream
can be unbounded.

## Module scope

Top-level `let` / `var` and `fn` declarations are visible inside
functions defined in the same file — no wrapping in a getter fn
needed:

```harn
let GRADER_SYSTEM = """
You are a strict grader...
"""

pub fn grade(path) {
  return llm_call(read_file(path), GRADER_SYSTEM, {
    provider: "auto",
    model: "local:gemma-4-e4b-it",
  })
}
```

(Module-level mutable `var` cross-function mutation is not fully
supported yet. If you need shared mutable state across functions, use
atomics: `atomic(0)`, `atomic_add(a, 1)`, `atomic_get(a)`.)

## Results and error handling

```harn
let r = try { llm_call(prompt, nil, opts) }
// Optional chaining short-circuits on Result.Err.
let text = r?.prose ?? "no response"
// Explicit error inspection.
if unwrap_err(r) != "" {
  log("failed")
}

// `try/catch` also works as an expression — the whole form evaluates to
// the try body's tail value on success or the catch handler's tail value
// on a caught throw, so simple fallbacks don't need Result gymnastics.
let prose = try { llm_call(prompt, nil, opts).prose } catch (e) { "fallback" }
```

## Concurrency

```harn,ignore
// Spawn a task, collect its result.
let h = spawn { long_work() }
let value = await(h)

// parallel each: concurrent map over a list.
let doubled = parallel each xs { x -> x * 2 }

// parallel settle: concurrent map that collects per-item Ok/Err.
let outcome = parallel settle paths { p -> grade(p) }
println(outcome.succeeded)

// Cap in-flight workers so you don't overwhelm the backend.
let results = parallel settle paths with { max_concurrent: 4 } { p ->
  llm_call(p, nil, opts)
}
```

`max_concurrent: 0` (or a missing `with` clause) means unlimited. See
`concurrency.md` for the RPM rate limiter, channels, `select`,
`deadline`, and `defer`.

## Streams

Use `gen fn` plus `emit` for lazy script-level streams:

```harn
gen fn numbers() -> Stream<int> {
  emit 1
  emit 2
}

for n in numbers() {
  println(n)
}
```

`Stream<T>` is distinct from the older `Generator<T>` type. Existing
`yield` behavior is unchanged; use `emit` inside `gen fn`. Streams are
single-pass, support `.next()` returning `{value, done}`, and propagate
throws to the consumer when the next item is pulled.

## CLI: `argv`

```bash
harn run my_script.harn -- file1.md file2.md
```

Inside the script:

```harn
fn grade_file(path) {
  println(path)
}

for path in argv {
  grade_file(path)
}
```

`argv` is always defined as `list<string>`; empty when no positional
args were given.

## Regex

```harn
let matches  = regex_match("[0-9]+", "abc 42 def 7")
let swapped  = regex_replace("(\\w+)\\s(\\w+)", "$2 $1", "hello world")
let same     = regex_replace_all("(\\w+)\\s(\\w+)", "$2 $1", "hello world")
let captures = regex_captures("(?P<day>[A-Z][a-z]+)", "Mon Tue")
```

Both `regex_replace` and `regex_replace_all` replace every match;
both support `$1`, `$2`, `${name}` backrefs from the `regex` crate.

## LLM calls

```harn
let r = llm_call(prompt, system, {
  provider: "auto",        // infers from model prefix
  model: "local:gemma-4-e4b-it",
  output_schema: schema,
  output_validation: "error",
  schema_retries: 2,       // retry with corrective nudge on schema mismatch
  response_format: "json",
})
println(r.prose)           // unwrapped prose (preferred for "the answer")
println(r.data.verdict)    // parsed structured output
```

Key options:

| Option | Default | Notes |
|---|---|---|
| `provider` | `"auto"` | `"auto"` infers from model prefix (`local:` / `/` / `claude-*` / `gpt-*` / `:`). |
| `llm_retries` | `0` | Transient error retries (HTTP 5xx, timeout, rate-limit). Set to N to allow N retries after the first attempt. |
| `llm_backoff_ms` | `250` | Base exponential backoff in milliseconds. |
| `schema_retries` | `1` | Re-prompt on `output_schema` validation failure. Requires `output_validation: "error"` to kick in. |
| `schema_retry_nudge` | auto | String (verbatim), `true` (auto), or `false` (bare retry). |
| `output_validation` | `"off"` | `"error"` throws on mismatch; `"warn"` logs. |

See `docs/src/llm-and-agents.md` for the overview, or
`docs/src/llm/agent_loop.md` for `agent_loop`, tool dispatch, and the full
option surface.

## Rate limiting

`max_concurrent` bounds simultaneous in-flight tasks on the caller
side. Providers can also be rate-limited at the throughput layer via
`rpm:` in `providers.toml` / `harn.toml` or
`HARN_RATE_LIMIT_<PROVIDER>=N` env vars. The two compose: use
`max_concurrent` to prevent bursts, and `rpm` to shape sustained
throughput.

## More

- LLM-friendly one-pager: `docs/llm/harn-quickref.md` (hosted at
  <https://harnlang.com/docs/llm/harn-quickref.html> and loaded
  automatically by the `harn-scripting` Claude skill when present).
- Full mdBook: `docs/src/` (`introduction.md`, `language-basics.md`,
  `concurrency.md`, `error-handling.md`, `llm-and-agents.md`).
- Language spec: `spec/HARN_SPEC.md`.
- Conformance examples: `conformance/tests/*.harn`.
