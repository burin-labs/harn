<!-- Generated from spec/chapters/*.md by scripts/sync_language_spec.harn -->

## Type annotations

Harn has an optional, gradual type system. Type annotations are checked at compile time
but do not affect runtime behavior. Omitting annotations is always valid.

### Basic types

```harn
const name: string = "Alice"
const age: int = 30
const rate: float = 3.14
const ok: bool = true
const nothing: nil = nil
```

### The `never` type

`never` is the bottom type — the type of expressions that never produce a
value. It is a subtype of all other types.

Expressions that infer to `never`:

- `throw expr`
- `return expr`
- `break` and `continue`
- A block where every control path exits
- An `if`/`else` where both branches infer to `never`
- `while true { ... }` whose body contains no `break` binding to that loop
  (a `break` inside a nested loop binds the inner loop and does not count),
  so a function whose tail is such a loop needs no trailing `return`
- Calls to `unreachable()`

`never` is removed from union types: `never | string` simplifies to
`string`. An empty union (all members removed by narrowing) becomes
`never`.

```harn
fn always_throws() -> never {
  throw "this function never returns normally"
}
```

### The `any` type

`any` is the top type and the explicit escape hatch. Every concrete
type is assignable to `any`, and `any` is assignable back to every
concrete type without narrowing. `any` disables type checking in both
directions for the values it flows through.

```harn
fn passthrough(x: any) -> any {
  return x
}

const s: string = passthrough("hello")  // any → string, no narrowing required
const n: int    = passthrough(42)
```

Use `any` deliberately, when you want to opt out of checking — for
example, a generic dispatcher that forwards values through a runtime
protocol you don't want to describe statically. Prefer `unknown` (see
below) for values from untrusted boundaries where callers should be
forced to narrow.

### The `unknown` type

`unknown` is the safe top type. Every concrete type is assignable to
`unknown`, but an `unknown` value is **not** assignable to any
concrete type without narrowing. This is the correct annotation for
values arriving from untrusted boundaries (parsed JSON, LLM responses,
dynamic dicts) where callers should be forced to validate the shape
before use.

```harn
fn describe(v: unknown) -> string {
  // Direct use of `v` as a concrete type is a compile-time error.
  // Narrow via type_of/schema_is first.
  if type_of(v) == "string" {
    return "string: ${v.upper()}"
  }
  if type_of(v) == "int" {
    return "int: ${v + 1}"
  }
  return "other"
}
```

Narrowing rules for `unknown`:

- `type_of(x) == "T"` narrows `x` to `T` on the truthy branch (where
  `T` is one of the type-of protocol names: `string`, `int`, `float`,
  `bool`, `nil`, `list`, `dict`, `closure`, `bytes`).
- `schema_is(x, Shape)` narrows `x` to `Shape` on the truthy branch.
- `guard type_of(x) == "T" else { ... }` narrows `x` to `T` in the
  surrounding scope after the guard.
- The falsy branch keeps `unknown` — subtracting one concrete type
  from an open top still leaves an open top. The checker still tracks
  which concrete `type_of` variants have been ruled out on the current
  flow path, so an exhaustive chain ending in `unreachable()` / `throw`
  can be validated; see the "Exhaustive narrowing on `unknown`"
  subsection of "Flow-sensitive type refinement".
- These rules apply equally when `x` is a *reference path* whose type is
  `unknown` (e.g. an `unknown`-typed field reached via `o.data`), not
  only a bare `unknown` variable — including the ruled-out tracking for
  the exhaustiveness check.

Interop between `any` and `unknown`:

- `unknown` is assignable to `any` (upward to the full escape hatch).
- `any` is assignable to `unknown` (downward — the `any` escape hatch
  lets it flow into anything, including `unknown`).

**When to pick which:**

- **No annotation** — "I haven't annotated this." Callers get no
  checking. Use for internal, unstable code.
- **`unknown`** — "this value could be anything; narrow before use."
  Use at untrusted boundaries and in APIs that hand back open-ended
  data. This is the preferred annotation for LLM / JSON / dynamic
  dict values.
- **`any`** — "stop checking." A last-resort escape hatch. Prefer
  `unknown` unless you have a specific reason to defeat checking
  bidirectionally.

### Member access and nil safety

Three syntactic forms dereference a receiver value at runtime — property
read (`obj.field`), subscript (`obj[key]`), and method call
(`obj.method(..)`). All three fail identically when the receiver is `nil`
or is not the kind of value the access expects, so the checker applies one
consistent set of diagnostics to all three rather than treating property
access as special:

| Receiver type | `obj.field` / `obj[key]` / `obj.m()` | `obj?.field` / `obj?.[key]` / `obj?.m()` |
|---|---|---|
| statically `nil` | **error** — known nil here | allowed (the `?` short-circuits) |
| `T \| nil` (nilable) | **error** — may be nil at runtime | allowed |
| `unknown` | **warning** — narrow or validate first | **warning** — `?` only guards nil, not a non-shape value |
| `any` | no diagnostic (checking opted out) | no diagnostic |
| concrete (`struct`, shape, `list`, …) | field/index/method checked against the type | unnecessary-`?` lint if the receiver can't be nil |

The fix for a `nil` / nilable receiver is always one of: the matching
optional operator (`?.`, `?.[…]`, or `?.m()`), a `!= nil` guard that
narrows the value, or a `??` default. For an `unknown` receiver, narrow
with `is_a` / `type_of`, validate with `assert_shape` / `schema_is`, or
add a shape annotation. `any` is the deliberate escape hatch and is never
diagnosed — see the `any` vs `unknown` guidance above.

Two narrowings keep this rule ergonomic. A guard on an optional-access
chain narrows the **base** identifier: inside `if o?.field != nil { … }`
the value `o` is non-nil (if `o` were nil, `o?.field` would be nil), so a
plain `o.field` read in that branch is allowed. And `value ?? default`
drops the `nil` arm of `value` even when `value`'s type is a **named
alias** that expands to a nilable union (`type Opts = {…} | nil`), so the
common `let opts = options ?? {}` option-defaulting idiom yields a non-nil
value.

These diagnostics fire only when the receiver's type comes from a real
contract — a written annotation, a named struct / alias / enum, a
call-return, or any non-identifier expression. The ambient dict-literal
idiom (`let d = {a: 1}; d.missing`) stays loose and returns `nil` at
runtime, matching the gradual-typing affordance for one-off glue.

### Open records and row polymorphism

A shape type may end with one or more **row tails** — `...` followed by a type.
A row tail stands for "any other fields," so the shape is **open**:

```harn,ignore
// `x` is any record that has at least a string `id`; `rest` captures the
// other fields, whatever they are.
fn needs_id(x: {id: string, ...rest}) -> string {
  return x.id
}

needs_id({id: "u1", name: "Ann", age: 3})   // ok — extra fields allowed
needs_id({name: "Ann"})                      // error — required `id` missing
```

`rest` is a **row variable**: an ordinary generic type parameter that happens
to appear in tail position. A function generic over rows types record **merge**
precisely — the result carries every field of both inputs, with the right-hand
fields overriding the left on overlap (matching the runtime of `merge` /
`{...a, ...b}` / `a + b`):

```harn,ignore
fn merge<R1, R2>(a: {...R1}, b: {...R2}) -> {...R1, ...R2} {
  return a + b
}

const m = merge({a: 1, b: 2}, {b: "x", c: true})
// m : {a: int, b: string, c: bool}   (b overridden by the right side)
```

Rules:

- **Subtyping** checks only the *explicit* fields of the expected side against
  the actual's known fields (Harn shapes are width-subtyped, so extra actual
  fields are always allowed and the row tail absorbs them). A required field
  absent from the actual's known fields is accepted only when the actual has a
  **gradual** tail (`dict`, `any`); an abstract row variable is not assumed to
  supply it.
- **Binding** is one-sided: a row variable binds to the actual record's
  *leftover* fields (the fields not matched by the explicit ones). With no
  explicit fields, `{...R}` binds `R` to the whole record.
- **Override** on overlap is right-biased; the result field is required if
  either side is required, and its type is the right field's type (or the union
  of both when the right field is optional).
- **Gradual interop:** `dict` is the dynamic row — spreading or merging a
  `dict`-typed value yields a `dict` rather than a falsely-precise closed shape.
  Precision is monotone: you never get a more precise result than the inputs
  justify.

### Union types

```harn
const value: string | nil = nil
const id: int | string = "abc"
```

Union members may also be **literal types** — specific string or int
values used to encode enum-like discriminated sets:

```harn
type Verdict = "pass" | "fail" | "unclear"
type RetryCount = 0 | 1 | 2 | 3

const v: Verdict = "pass"
```

Literal types are assignable to their base type (`"pass"` flows into
`string`), and a base-typed value flows into a literal union (`string`
into `Verdict`). Runtime `schema_is` / `schema_expect` guards and the
parameter-annotation runtime check reject values that violate the
literal set.

A `match` on a literal union must cover every literal or include a
wildcard `_` arm — non-exhaustive `match` is a hard error.

#### Tagged shape unions (discriminated unions)

A union of two or more dict shapes is a *tagged shape union* when the
shapes share a discriminant field. The discriminant is auto-detected:
the first field of the first variant that (a) is non-optional in every
member, (b) has a literal type (`LitString` or `LitInt`), and (c) takes
a distinct literal value per variant qualifies. The field can be named
anything — `kind`, `type`, `op`, `t`, etc. — there is no privileged
spelling.

```harn
type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int}
```

Matching on the discriminant narrows the value to the matching variant
inside each arm; the same narrowing fires under
`if obj.<tag> == "value"` / `else`:

```harn
type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int}

fn handle(m: Msg) -> string {
  match m.kind {
    "ping" -> { return "ttl=" + to_string(m.ttl) }
    "pong" -> { return to_string(m.latency_ms) + "ms" }
  }
}
```

Such a `match` must cover every variant or include a wildcard `_` arm
— non-exhaustive `match` is a hard error.

#### Distributive generic instantiation

Generic type aliases distribute over closed-union arguments. Writing
`Container<A | B>` is equivalent to `Container<A> | Container<B>` so
each instantiation independently fixes the type parameter. This is what
keeps `processCreate: fn("create") -> nil` flowing into a `list<
ActionContainer<Action>>` element instead of getting rejected by the
contravariance of the function-parameter slot:

```harn
type Action = "create" | "edit"
type ActionContainer<T> = {action: T, process_action: fn(T) -> nil}
```

`ActionContainer<Action>` resolves to `ActionContainer<"create"> |
ActionContainer<"edit">`, and a literal-tagged shape on the right flows
into the matching branch.

### Intersection types

```harn
type BaseCtx = {request_id: string}
type AuthCtx = {user_id: string}

fn use_ctx(ctx: BaseCtx & AuthCtx) -> string {
  return ctx.request_id + "/" + ctx.user_id
}
```

`A & B` requires the value to satisfy *every* component. The intersection
of two shape types behaves like a dict that has every field from each
component, so `ctx.request_id` and `ctx.user_id` are both accessible
above. Shape components may be inline or named aliases; the operator
nests freely (`A & B & C`).

`&` binds tighter than `|`, so `A & B | C` parses as `(A & B) | C`. Use
parentheses to write the union-of-intersections form.

At runtime, an intersection annotation lowers to a JSON-Schema `allOf`
guard. A value that is missing a field required by *any* component is
rejected by the parameter-annotation runtime check just like a single
shape mismatch is.

### Parameterized types

```harn
const numbers: list<int> = [1, 2, 3]
const also_numbers: [int] = [1, 2, 3]
const headers: dict<string, string> = {content_type: "json"}
```

`[T]` is shorthand for `list<T>` in type positions. User-defined functions,
structs, enums, interfaces, and type aliases may declare type parameters with
`<T, U>`. Function calls normally infer those parameters from arguments, but
callers may pass them explicitly when inference needs help or when the desired
instantiation should be documented at the call site:

```harn,ignore
fn map<T, U>(xs: [T], f: fn(T) -> U) -> [U] { ... }
const labels: [string] = map<int, string>([1, 2, 3], label)
```

Explicit type arguments are erased at runtime. They are checked statically:
the number of supplied type arguments must match the function declaration, and
each explicit binding must remain consistent with the concrete argument types.

### Structural types (shapes)

Dict shape types describe the expected fields of a dict value. The type checker
verifies that dict literals have the required fields with compatible types.

```harn
const user: {name: string, age: int} = {name: "Alice", age: 30}
```

Optional fields use `?` and need not be present:

```harn
const config: {host: string, port?: int} = {host: "localhost"}
```

Width subtyping: a dict with extra fields satisfies a shape that requires fewer fields.

```harn
fn greet(u: {name: string}) -> string {
  return "hi ${u["name"]}"
}
greet({name: "Bob", age: 25})  // OK — extra field allowed
```

Option-bag parameters are stricter for literal calls. When a parameter is
named like `opts`, `options`, or `config`, or its annotation is a type alias
ending in `Options` or `Config`, a direct dict literal argument may only use
fields declared by the option shape:

```harn
type PickOptions = {drop_nil?: bool}

fn pick(_options: PickOptions = {}) {}

pick({drop_nil: true})  // OK
```

A typo in a direct option literal is a type error:

```harn,ignore
pick({dropnil: true})   // type error — unknown option `dropnil`
```

Nested shapes:

```harn
const data: {user: {name: string}, tags: list} = {user: {name: "X"}, tags: []}
```

Shapes are compatible with `dict` and `dict<string, V>` when all field values match `V`.

### Type aliases

```harn
type Config = {model: string, max_tokens: int}
const cfg: Config = {model: "gpt-4", max_tokens: 100}
```

A type alias can also drive schema validation for structured LLM output
and runtime guards. `schema_of(T)` lowers an alias to a JSON-Schema
dict at compile time:

```harn
type GraderOut = {
  verdict: "pass" | "fail" | "unclear",
  summary: string,
  findings: list<string>,
}

// Use the alias directly wherever a schema dict is expected.
const s = schema_of(GraderOut)
const ok = schema_is({verdict: "pass", summary: "x", findings: []}, GraderOut)

const r = llm_call(prompt, nil, {
  provider: "openai",
  output_schema: GraderOut,     // alias in value position — compiled to schema_of(T)
  schema_retries: 2,
})
```

`llm_call` can also express routing intent without pinning a single
provider/model pair. The `route_policy` option accepts:

- `"manual"` (default): use the normal `provider` / `model` / env resolution.
- `"always(id)"`: pin to a model alias, model id, or `provider:model` selector.
- `"cheapest_over_quality(t)"`: select the lowest-cost available catalog
  candidate whose model tier is at least `t`.
- `"fastest_over_quality(t)"`: select the lowest-latency available catalog
  candidate whose model tier is at least `t`.

The optional `fallback_chain` is an ordered list of provider ids to try when
the selected provider fails availability or transport. Routing decisions are
recorded in LLM transcript events with the selected route plus all considered
alternatives so costs can be re-scored later:

```harn
const r = llm_call(prompt, nil, {
  route_policy: "cheapest_over_quality(mid)",
  fallback_chain: ["local", "ollama", "openai"],
})
```

System prompt fragments can be supplied without hand-concatenating the
positional `system` string. `system_preamble`, `system_context`,
`system_prompt_parts`, `system_appendix`, `system_prefix`, and
`system_suffix` are normalized into the provider's system/developer
instruction channel for `llm_call` and `agent_loop`. In persistent
`agent_loop` sessions, the composed session-level system prompt is recorded
once in transcript metadata and as one leading internal `system_prompt`
fingerprint event; it is not injected into the replayable message list. A later
continuation that omits all system prompt fields reuses the stored session
prompt for the provider request without writing another transcript event.
Internal `_system_fragments` entries may set `bucket: "before"` (default) or
`bucket: "after"`; the latter is used for live prompt-tail recitations such as
the agent scratchpad.

```harn
import { system_preamble, with_system_prompt_parts } from "std/llm/prompts"

const opts = with_system_prompt_parts(
  {provider: "anthropic", session_id: "review-42"},
  [system_preamble("Follow the repository's validation gate before final output.")]
)
const r = agent_loop("Review this change", "You are a code review agent.", opts)
```

For call sites that want routing policy to be visibly scoped around the work,
`cost_route` installs an inherited LLM routing context for the dynamic extent
of its block. Nested `llm_call` invocations inherit the block's routing and
budget options; an explicit option on the call wins for the same key.

```harn
const r = cost_route {
  budget_usd: 0.05
  prefer: ["anthropic:claude-haiku-4-5", "openai:gpt-5.4-mini"]
  fallback_strategy: cheapest_first

  llm_call(prompt, nil, {max_tokens: 800})
}
```

`budget_usd` is a shorthand for `budget.max_cost_usd`. `prefer` is an ordered
list of model aliases, model ids, or `provider:model` selectors. The
`fallback_strategy` value may be `prefer_order`, `cheapest_first`, or
`fastest_first`; failures on the selected route are retried against the
remaining preferred routes before provider-level fallbacks are considered.
Without `prefer`, `fallback_strategy: cheapest_first` and
`fallback_strategy: fastest_first` lower to the corresponding
`cheapest_over_quality(quality)` / `fastest_over_quality(quality)` policy,
using `quality` or `min_quality` when present and `mid` otherwise.

For call sites that want Harn-managed response reuse, `std/llm/handlers`
exports `with_cache(prompt, system?, options?)`. It returns the same envelope as
`llm_call`, but first checks a persistent content-addressed cache. The key is
`sha256:` plus canonical JSON over `{prompt, system, provider, model,
temperature, top_p, max_tokens}` after provider/model defaults resolve. Cache
storage defaults to a sqlite store under Harn state with namespace
`llm.with_cache`, a 10-minute TTL, and LRU eviction at 256 entries. Calls with
`options.tools != nil` bypass the cache by default because tool results can
carry side effects; callers may set `skip_when` to a bool or predicate closure
to override that policy.

```harn
import { with_cache } from "std/llm/handlers"

const r = with_cache("Summarize this file", nil, {
  provider: "anthropic",
  model: "claude-haiku-4-5",
  store: {backend: "fs", namespace: "summaries"},
  ttl: "10m",
  max_entries: 256,
})
```

`std/cache` exposes the underlying `{hit, value?}` primitive with
`cache_get(key, options?)`, `cache_put(key, value, options?)`, and
`cache_clear(options?)`. Cache options accept either `store: "namespace"` or
`store: {backend: "sqlite"|"fs", namespace?, path?}` plus `ttl`,
`ttl_seconds`, `max_age_seconds`, and `max_entries`.

The emitted schema follows canonical JSON-Schema conventions (objects
with `properties`/`required`, arrays with `items`, literal unions as
`{type, enum}`) so it is compatible with structured-output validators
and with ACP `ToolAnnotations.args` schemas. The compile-time lowering
applies when the alias identifier appears as:

- The argument of `schema_of(T)`.
- The schema argument of `schema_is`, `schema_expect`, `schema_parse`,
  `schema_check`, `schema_report`, `is_type`, `json_validate`.
- The value of an `output_schema:` entry in an `llm_call` options dict.

For aliases not known at compile time (e.g. `let T = schema_of(Foo)`
or dynamic construction), passthrough through the runtime `schema_of`
builtin keeps existing schema dicts working.

#### Generic inference via `Schema<T>`

Schema-driven builtins are typed with proper generics so user-defined
wrappers pick up the same narrowing.

- `llm_call<T>(prompt, system, options: {output_schema: Schema<T>, ...})
  -> {data: T, text: string, ...}`
- `llm_completion<T>` has the same signature.
- `llm_call_structured<T>(prompt, schema: Schema<T>, options?) -> T`
- `llm_call_structured_safe<T>(prompt, schema: Schema<T>, options?) ->
  {ok: bool, data: T | nil, error: dict | nil}`
- `llm_call_structured_result<T>(prompt, schema: Schema<T>, options?) ->
  {ok: bool, data: T | nil, raw_text: string, error: string,
  error_category: string | nil, attempts: int, repaired: bool,
  extracted_json: bool, usage: {input_tokens: int, output_tokens: int,
  cache_read_tokens: int, cache_write_tokens: int,
  cache_creation_input_tokens: int, cache_hit_ratio: float,
  cache_savings_usd: float}, model: string, provider: string}`.
  Never throws on transport / schema failures —
  callers dispatch on `ok` / `error_category`. Recognized
  `error_category` values: `transport`-class categories pass through
  the underlying enum (`rate_limit`, `timeout`, `auth`,
  `transient_network`, ...); JSON / schema failures surface as
  `missing_json`, `schema_validation`, or `repair_failed` when an
  optional repair pass was attempted and also failed. Options accept a
  `repair: {enabled: bool, ...llm_call_overrides}` block — the repair
  pass runs a single shot on malformed JSON only and is skipped on
  transport-layer failures.
- `schema_parse<T>(value: unknown, schema: Schema<T>) -> Result<T, string>`
- `schema_check<T>(value: unknown, schema: Schema<T>) -> Result<T, string>`
- `schema_expect<T>(value: unknown, schema: Schema<T>) -> T`
- `schema_recover<T>(text: string, schema: Schema<T>, options?:
  {llm_repair?: bool | dict, apply_defaults?: bool,
  ...llm_call_overrides}) -> {ok: bool, data: T | nil, raw_text:
  string, error: string, error_category: string | nil, attempts: int,
  stage: string, repaired: bool}`. Best-effort recovery of malformed
  LLM output against a target schema. Three deterministic stages
  followed by an optional one-shot LLM repair: `parsed` (direct
  `serde_json` parse) → `extracted` (lift JSON from prose / code
  fences) → `regex` (scrape top-level `key: value` lines for scalar
  fields) → `llm_repair` (single-shot `llm_call` with `schema_retries:
  0`). `stage` reports which stage produced the result; `failed` means
  every stage exhausted. Set `{llm_repair: false}` for a fully
  deterministic recovery pass with no LLM calls. The LLM repair stage
  accepts the same overrides as `llm_call_structured_result`'s
  `repair`.

`Schema<T>` denotes a runtime schema value whose static shape is `T`.
In a parameter position, matching a `Schema<T>` against an argument
whose value resolves to a type alias (directly, via `schema_of(T)`,
or via an inline JSON-Schema dict literal) binds the type parameter.
A user-defined wrapper such as

```harn,ignore
fn grade<T>(prompt: string, schema: Schema<T>) -> T {
  const r = llm_call(prompt, nil,
    {provider: "mock", output_schema: schema, output_validation: "error",
     response_format: "json"})
  return r.data
}

const out: GraderOut = grade("Grade this", schema_of(GraderOut))
log(out.verdict)
```

narrows `out` to `GraderOut` at the call site without any
`schema_is` / `schema_expect` guard, and without per-wrapper
typechecker support.

`Schema<T>` is a type-level construct. In value positions, the
runtime `schema_of(T)` builtin returns an idiomatic schema dict
whose static type is `Schema<T>`.

### Human-in-the-loop primitives

Human-in-the-loop is modeled as **first-class typed expression syntax**.
`ask_user`, `request_approval`, `dual_control`, and `escalate_to` are
reserved keywords with VM-enforced semantics: their names cannot be
shadowed or rebound, the result envelopes are produced (and signed) by
the runtime, and quorum approval requires distinct principals. Each
primitive accepts either named arguments or the legacy positional form;
both lower to the same runtime.

```harn,ignore
const answer = ask_user(prompt: "deploy now?", schema: schema_of(Choice))
const record = request_approval(action: "merge_pr", quorum: 2,
                              reviewers: ["alice", "bob", "carol"])
const merged = dual_control(n: 2, m: 3, action: destructive_step,
                          approvers: ["alice", "bob", "carol"])
const handle = escalate_to(role: "oncall", reason: "deploy failed")
```

The runtime owns blocking semantics, timeout behavior, event-log
records, and replay.

- `ask_user<T>(prompt: string, options?: {schema?: Schema<T>, timeout?: duration, default?: T}) -> T`
- `request_approval(action: string, options?: ApprovalRequestOptions)`
  returns `{approved: bool, reviewers: list<string>, approved_at: string, reason: string | nil,
  signatures: list<{reviewer: string, signed_at: string, signature: string}>}`.
  `ApprovalRequestOptions` is `{detail?: any, args?: any, quorum?: int,
  reviewers?: list<string>, deadline?: duration, principal?: string,
  evidence_refs?: list<dict>, undo_metadata?: dict,
  capabilities_requested?: list<string>}`.
- `dual_control<T>(n: int, m: int, action: fn() -> T, approvers?: list<string>) -> T`
- `escalate_to(role: string, reason: string)`
  returns `{request_id: string, role: string, reason: string, trace_id: string,
  status: string, accepted_at: string | nil, reviewer: string | nil}`.
- `hitl_pending(filters?: {since?: string, until?: string, kinds?: list<string>,
  agent?: string, limit?: int})`
  returns `list<{request_id: string, request_kind: string, agent: string,
  prompt: string, trace_id: string, timestamp: string, approvers: list<string>,
  metadata: dict}>`.

Normative behavior:

- `ask_user` appends `hitl.question_asked`, then blocks until the host appends
  a matching response. The default timeout is 24 hours unless `timeout` is
  supplied. If `schema` is present, the answer must satisfy it. If
  the wait times out, Harn appends `hitl.timeout` and either returns
  `options.default` or throws `HumanTimeoutError`.
- `request_approval` appends `hitl.approval_requested` and waits for the
  configured quorum. `deadline` defaults to 24 hours. Denial raises
  `ApprovalDeniedError`. Successful completion returns the approval record,
  including one signed reviewer timestamp receipt per counted approver.
- `dual_control` is an approval-gated wrapper around a closure. The closure is
  not executed until quorum is satisfied. The runtime appends
  `hitl.dual_control_requested`, `hitl.dual_control_approved` /
  `hitl.dual_control_denied`, and `hitl.dual_control_executed`.
- `escalate_to` appends `hitl.escalation_issued` and blocks until the host
  appends `hitl.escalation_accepted`. The request payload includes the active
  capability policy when one is installed so hosts can resolve the requested
  role against the same capability ceiling enforced by the VM. If the host does
  not respond, the dispatch remains paused until manual resume.
- `hitl_pending` reads the durable HITL topics via the active event log,
  returns `[]` when no event log is attached, filters by `since` / `until` /
  `kinds` / `agent` / `limit`, and omits requests that have already reached a
  terminal HITL event.

HITL records live in durable event-log topics:

- `hitl.questions`
- `hitl.approvals`
- `hitl.dual_control`
- `hitl.escalations`

Replay is event-log-driven. During replay, HITL primitives resolve from the
previously recorded HITL response events instead of consulting a live host,
so approval reviewer identities, signed timestamps, and signatures remain
stable across deterministic replay.

Replay-for-teaching corrections live in `corrections.records`. `std/corrections`
accepts `CorrectionInput`, whose `from_decision` and `to_decision` fields use
the reusable `CorrectionDecision` shape and whose optional evidence uses
`list<CorrectionEvidenceRef>`. The stored `CorrectionRecord` captures those
decisions plus `{ reason, applied_by, scope }`, with optional
actor/action/trace/step metadata. `this_persona` and `all` scopes feed
`CapabilityPolicy` derivation by tightening the affected actor to a read-only
side-effect ceiling while matching correction records remain applicable.

### Function type annotations

Parameters and return types can be annotated:

```harn
fn add(a: int, b: int) -> int {
  return a + b
}
```

### Type checking behavior

- Annotations are optional (gradual typing). Untyped values are `None` and skip checks.
- `int` is assignable to `float`.
- Dict literals with string keys infer a structural shape type.
- Dict literals with computed keys infer as generic `dict`.
- Shape-to-shape: all required fields in the expected type must exist with compatible types.
- Option-bag literal calls reject keys that are not declared by the expected option shape.
- Shape-to-`dict<K, V>`: all field values must be compatible with `V`.
- Type errors are reported at compile time and halt execution.

### Flow-sensitive type refinement

The type checker performs flow-sensitive type refinement (narrowing) on
union types based on control flow conditions.  Refinements are
bidirectional — both the truthy and falsy paths of a condition are
narrowed.

#### Nil checks

`x != nil` narrows to non-nil in the then-branch and to `nil` in the
else-branch.  `x == nil` applies the inverse.

```harn
fn greet(name: string | nil) -> string {
  if name != nil {
    // name is `string` here
    return "hello ${name}"
  }
  // name is `nil` here
  return "hello stranger"
}
```

#### `type_of()` checks

`type_of(x) == "typename"` narrows to that type in the then-branch and
removes it from the union in the else-branch.

```harn
fn describe(x: string | int) {
  if type_of(x) == "string" {
    log(x)  // x is `string`
  } else {
    log(x)  // x is `int`
  }
}
```

#### Reference paths

Narrowing applies to *reference paths* — an identifier followed by a
chain of constant property accesses and constant subscripts
(`entry.arguments`, `cfg.opts.mode`, `xs[0]`, `m["k"]`) — not just bare
variables. Every refinement form that narrows a variable also narrows a
path: `type_of(path) == "T"`, `path != nil`, a bare `if path`
(truthiness, removes `nil`), `schema_is(path, S)` / `path.has("k")`, and
a tagged-shape-union discriminant (`o.msg.kind == "ping"` narrows
`o.msg`). A guard on a path narrows later reads of that same path:

```harn
fn flags(entry: {arguments: list?}) -> list {
  if type_of(entry.arguments) == "list" {
    return entry.arguments  // entry.arguments is `list` here
  }
  return []
}
```

Optional (`?.`) and plain (`.`) links address the same value, so they
share a narrowing — `type_of(entry?.arguments) == "list"` narrows reads
of both `entry?.arguments` and `entry.arguments`. A path whose type is
the top type (`unknown`/`any`, common for `json_parse` / `llm_call`
boundary fields) narrows to the tested kind, exactly as an
`unknown`-typed variable does.

The narrowing is dropped when the base variable or the path is
reassigned, since the reference may then point at a different value. A
*dynamic* subscript (`xs[i]` with a non-literal index) is deliberately
never narrowed: it is not a stable reference, so a later `xs[i]` may
read a different element.

#### Index reads are optional

An index read into a `list<T>`, a `dict<K, V>`, or a `string` yields
`T | nil` (respectively `V | nil` and `string | nil`): an out-of-bounds
index or an absent key is `nil` at runtime, so typing the read as a bare
`T` would be unsound. This mirrors TypeScript's `noUncheckedIndexedAccess`.

```harn,ignore
const xs: list<int> = []
const n: int = xs[0]   // error: expected int, found int? (xs[0] may be nil)
```

Recover the non-nil element in one of three ways:

- `?? default` — coalesce the absent case: `const n: int = xs[0] ?? 0`.
- A `for x in xs` loop — the loop variable binds the element type `T`
  directly, never `T?`, because iteration never visits an absent slot.
- `.first()` / `.last()` — these accessors already return `T?`.

An honest element accessor therefore returns the optional element type:
`fn first<T>(xs: list<T>) -> T? { return xs[0] }`. Index *writes*
(`xs[i] = v`) are unaffected: the assigned slot keeps its bare element
type `T`, since a write stores a present value.

#### Truthiness

A bare identifier in condition position narrows by removing `nil`:

```harn
fn check(x: string | nil) {
  if x {
    log(x)  // x is `string`
  }
}
```

#### Logical operators

- `a && b`: combines both refinements on the truthy path.
- `a || b`: combines both refinements on the falsy path.
- `!cond`: inverts truthy and falsy refinements.

```harn
fn check(x: string | int | nil) {
  if x != nil && type_of(x) == "string" {
    log(x)  // x is `string`
  }
}
```

#### Guard statements

After a `guard` statement, the truthy refinements apply to the outer
scope (since the else-body must exit):

```harn
fn process(x: string | nil) {
  guard x != nil else { return }
  log(x)  // x is `string` here
}
```

#### Early-exit narrowing

When one branch of an `if`/`else` definitely exits (via `return`,
`throw`, `break`, or `continue`), the opposite refinements apply after
the `if`:

```harn
fn process(x: string | nil) {
  if x == nil { return }
  log(x)  // x is `string` — the nil path returned
}
```

#### While loops

The condition's truthy refinements apply inside the loop body.

#### Ternary and `if` expressions

The condition's refinements apply to the true and false branches
respectively — for both the `cond ? a : b` ternary and an `if`/`else`
used as an expression for its value:

```harn
fn pick(x: string | int) -> string {
  // then-branch sees `x: string`, so the result type is `string`.
  return if type_of(x) == "string" { x } else { "fallback" }
}
```

#### Match expressions

When matching a union-typed variable against literal patterns, the
variable's type is narrowed in each arm:

```harn
fn check(x: string | int) {
  match x {
    "hello" -> { log(x) }  // x is `string`
    42 -> { log(x) }       // x is `int`
    _ -> {}
  }
}
```

Matching on `type_of(subject)` narrows the subject (variable or
reference path) in each arm to the tested kind — the `match`
counterpart of an `if type_of(subject) == "T"` chain:

```harn
fn describe(o: {val: string | int}) -> string {
  return match type_of(o.val) {
    "string" -> { o.val }  // o.val is `string`
    "int" -> { to_string(o.val) }  // o.val is `int`
    _ -> { "?" }
  }
}
```

#### Or-patterns (`pat1 | pat2 -> body`)

A match arm may list two or more alternative patterns separated by `|`;
the shared body runs when any alternative matches. Each alternative
contributes to exhaustiveness coverage independently, so an or-pattern
and a single-literal arm compose naturally:

```harn
fn verdict(v: "pass" | "fail" | "unclear") -> string {
  return match v {
    "pass" -> { "ok" }
    "fail" | "unclear" -> { "not ok" }
  }
}
```

Narrowing inside the or-arm refines the matched variable to the *union
of the alternatives' single-literal narrowings*. On a literal union
this is a sub-union; on a tagged shape union it is a union of the
matching shape variants:

```harn,ignore
type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int} |
  {kind: "close", reason: string}

fn summarise(m: Msg) -> string {
  return match m.kind {
    "ping" | "pong" -> {
      // m is narrowed to {kind:"ping",…} | {kind:"pong",…};
      // the shared `kind` discriminant stays accessible.
      "live:" + m.kind
    }
    "close" -> { "closed:" + m.reason }
  }
}
```

Guards apply to the arm as a whole: `1 | 2 | 3 if n > 2 -> …` runs the
body only when some alternative matched *and* the guard held. A guard
failure falls through to the next arm, exactly like a literal-pattern
arm.

Or-patterns are restricted to literal alternatives (string, int,
float, bool, nil) in this release. Alternatives that introduce
identifier bindings or destructuring patterns are a forward-compatible
extension and currently rejected.

#### `.has()` on shapes

`dict.has("key")` narrows optional shape fields to required:

```harn
fn check(x: {name?: string, age: int}) {
  if x.has("name") {
    log(x)  // x.name is now required (non-optional)
  }
}
```

#### Exhaustiveness checking with `unreachable()`

The `unreachable()` builtin acts as a static exhaustiveness assertion.
When called with a variable argument, the type checker verifies that the
variable has been narrowed to `never` — meaning all possible types have
been handled. If not, a compile-time error reports the remaining types.

```harn
fn process(x: string | int | nil) -> string {
  if type_of(x) == "string" { return "string: ${x}" }
  if type_of(x) == "int" { return "int: ${x}" }
  if x == nil { return "nil" }
  unreachable(x)  // compile-time verified: x is `never` here
}
```

At runtime, `unreachable()` throws `"unreachable code was reached"` as a
safety net. When called without arguments or with a non-variable argument,
no compile-time check is performed.

#### Exhaustive narrowing on `unknown`

The checker tracks the set of concrete `type_of` variants that have been
ruled out on the current flow path for every `unknown`-typed variable.
The falsy branch of `type_of(v) == "T"` still leaves `v` typed `unknown`
(subtracting one concrete type from an open top still leaves an open
top), but the **coverage set** for `v` gains `"T"`.

When control flow reaches a never-returning site — `unreachable()`, a
`throw` statement, or a call to a user-defined function whose return
type is `never` — the checker verifies that the coverage set for every
still-`unknown` variable is either empty or complete. An incomplete
coverage set is treated as a failed exhaustiveness claim and triggers a
warning that names the uncovered concrete variants:

```harn
fn handle(v: unknown) -> string {
  if type_of(v) == "string" { return "s:${v}" }
  if type_of(v) == "int"    { return "i:${v}" }
  unreachable("unknown type_of variant")
  // warning: `unreachable()` reached but `v: unknown` was not fully
  // narrowed — uncovered concrete type(s): float, bool, nil, list,
  // dict, closure, bytes
}
```

Covering all nine `type_of` variants (`int`, `string`, `float`, `bool`,
`nil`, `list`, `dict`, `closure`, `bytes`) silences the warning. Suppression via
an explicit fallthrough `return` is intentional: a plain `return`
doesn't claim exhaustiveness, so partial narrowing followed by a normal
return stays silent. Reaching `throw` or `unreachable()` with no prior
`type_of` narrowing also stays silent — the coverage set must be
non-empty for the warning to fire, which avoids false positives on
unrelated error paths.

Reassigning the variable clears its coverage set, matching the way
narrowing is already invalidated on reassignment.

#### Unreachable code warnings

The type checker warns about code after statements that definitely exit
(via `return`, `throw`, `break`, or `continue`), including composite
exits where both branches of an `if`/`else` exit:

```harn
fn foo(x: bool) {
  if x { return 1 } else { throw "err" }
  log("never reached")  // warning: unreachable code
}
```

#### Reassignment invalidation

When a narrowed variable is reassigned, the narrowing is invalidated and
the original declared type is restored.

#### Mutability

Variables declared with `let` are immutable.  Assigning to a `let`
variable produces a compile-time warning (and a runtime error).

### Runtime parameter type enforcement

In addition to compile-time checking, function parameters with type annotations
are enforced at runtime. When a function is called, the VM verifies that each
annotated parameter matches its declared type before executing the function body.
If the types do not match, a `TypeError` is thrown:

```text
TypeError: parameter 'name' expected string, got int (42)
```

The following types are enforced at runtime: `int`, `float`, `string`, `bytes`,
`bool`, `list`, `dict`, `set`, `nil`, and `closure`. `int` and `float` are mutually
compatible (passing an `int` to a `float` parameter is allowed, and vice versa).
Union types, `list<T>`, `dict<string, V>`, and nested shapes are also checked at
runtime when the parameter annotation can be lowered into a runtime schema.

### Runtime shape validation

Shape-annotated function parameters are validated at runtime. When a function
parameter has a structural type annotation (e.g., `{name: string, age: int}`),
the VM checks that the argument is a dict (or struct instance) with all
required fields and that each field has the expected type.

```harn,ignore
fn process(user: {name: string, age: int}) {
  log("${user.name} is ${user.age}")
}

process({name: "Alice", age: 30})     // OK
process({name: "Alice"})              // Error: parameter 'user': missing field 'age' (int)
process({name: "Alice", age: "old"})  // Error: parameter 'user': field 'age' expected int, got string
```

Shape validation works with both plain dicts and struct instances. Extra
fields are allowed (width subtyping). Optional fields (declared with `?`)
are not required to be present.
