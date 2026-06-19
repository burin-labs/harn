## Attributes

Attributes are declarative metadata attached to a top-level declaration
with the `@` prefix. They compile to side-effects (warnings, runtime
registrations) at the attached declaration, and stack so a single decl
can carry multiple. Arguments are restricted to compile-time values:
strings, numbers, booleans, `nil`, bare or dotted identifiers, lists,
dicts, and simple call-shaped sentinels such as `schedule("...")`.
There is no runtime evaluation.

### Syntax

```ebnf
attribute    ::= '@' attr_name ['(' attr_arg (',' attr_arg)* [','] ')']
attr_arg     ::= [attr_name ':'] attr_value
attr_name    ::= IDENTIFIER | 'retry'
attr_value   ::= literal | dotted_ident | attr_list | attr_dict | attr_call
dotted_ident ::= IDENTIFIER ('.' IDENTIFIER)*
attr_list    ::= '[' [attr_value (',' attr_value)* [',']] ']'
attr_dict    ::= '{' [attr_key ':' attr_value (',' attr_key ':' attr_value)* [',']] '}'
attr_key     ::= IDENTIFIER | STRING
attr_call    ::= dotted_ident '(' [attr_value (',' attr_value)* [',']] ')'
```

```harn
@deprecated(since: "0.8", use: "compute_v2")
@test
pub fn compute(x: int) -> int { return x + 1 }
```

Attributes attach to the **immediately following** declaration —
either `pipeline`, `fn`, `tool`, `struct`, `enum`, `type`, `interface`,
or `impl`. Attaching to anything else (a `let`, a statement) is a parse
error.

### Standard attributes

#### `@deprecated`

```harn,ignore
@deprecated(since: "0.8", use: "new_fn")
pub fn old_fn() -> int { ... }
```

Emits a type-checker warning at every call site of the attributed
function. Both arguments are optional; when present they are folded
into the warning message.

| Argument | Type | Meaning |
|---|---|---|
| `since` | string | Version that introduced the deprecation |
| `use` | string | Replacement function name (rendered as a help line) |

#### `@test`

```harn,ignore
@test
pipeline test_smoke(task) { ... }
```

Marks a pipeline as a test entry point. The conformance / `harn test`
runner discovers attributed pipelines in addition to the legacy
`test_*` naming convention. Both forms continue to work.

#### `@job`, `@schedule`, `@queue`, and `@retry`

```harn,ignore
import "std/triggers"

@job("scan")
@retry(max: 3, backoff: "exponential")
@schedule("0 * * * *", "UTC")
@queue("scan-jobs")
pub fn scan(event: TriggerEvent) -> dict {
  return {status: "ok", request: event.provider_payload.raw}
}
```

`@job` marks an exported function as a trigger-dispatched background
entrypoint. `harn run --as-job file.harn --job scan --request req.json`
runs that job once, delivering the request JSON as
`event.provider_payload.raw` and printing the returned value as JSON.
`harn serve worker file.harn` activates every `@schedule`d job in the
file and consumes any declared `@queue` worker queues until shutdown.

`@retry(max: N, backoff: "...")` applies the dispatcher's retry policy
to the job. `max`/`max_attempts` must be a non-negative integer;
`backoff`/`policy` accepts `svix`, `linear`, or `exponential`. A compact
`@job("scan", retry: { max: 3, backoff: "linear" })` form is also
accepted for generated metadata that already mirrors trigger configs.
`@schedule`, `@queue`, and `@retry` are inert without `@job` and produce
a serve diagnostic.

#### Durable persona annotations

Durable persona metadata can be declared directly on a function, tool, or
pipeline with `@persona`, `@step`, `@trigger`, `@handoff`, and `@budget`:

```harn,ignore
@persona(
  triggers: [github.pr_opened, schedule("*/30 * * * *")],
  tools: [github, ci, linear],
  autonomy: act_with_approval,
  budget: {daily_usd: 20, frontier_escalations: 3},
  handoffs: [review_captain, human_maintainer],
  receipts: required,
)
@trigger(github.check_failed)
@handoff(target: review_captain, reason: "risky diff")
@budget(daily_usd: 20, max_tokens: 100000)
fn merge_captain(ctx: HandlerCtx) { ... }

@step(
  name: "plan",
  model: "gpt-5.4-mini",
  approval: optional,
  receipt: audit,
  error_boundary: fail,
  retry: {max_attempts: 2},
)
fn plan_step(ctx: HandlerCtx) { ... }
```

The annotations are first-class parser and type-checker metadata. The
attached declaration remains an ordinary callable declaration at runtime
unless a host or packaging layer consumes the metadata. Existing manifest
and dict-style trigger/persona metadata remains valid Harn data and is
not rewritten by this syntax.

| Attribute | Arguments |
|---|---|
| `@persona` | Named metadata: `triggers`, `schedules`, `tools`, `autonomy`, `budget`, `handoffs`, `context_packs`, `evals`, `receipts`, `model`, `owner`, `name`, `description` |
| `@step` | Named metadata: `name`, `model`, `approval` (`required`/`optional`), `receipt` (`audit`/`none`), `error_boundary` (`fail`/`continue`/`escalate`), `retry` (`{max_attempts: N}`) |
| `@trigger` | Positional trigger specs (`github.pr_opened`, `"github.pr_opened"`, `schedule("cron")`) or named `id`, `provider`, `kind`, `event`, `when`, `schedule`, `budget` |
| `@handoff` | Named `target`/`to`, `reason`, `schema`, `artifact` |
| `@budget` | Named numeric fields: `daily_usd`, `hourly_usd`, `run_usd`, `max_tokens`, `frontier_escalations`, `max_autonomous_decisions_per_hour`, `max_autonomous_decisions_per_day`; string/symbol exhaustion policy via `on_exhausted` or `on_budget_exhausted` |

For `@persona` functions, `harn lint` reports
`persona-body-must-call-steps` when the body directly calls a non-stdlib
function that is not declared with `@step`. Projects may allow specific
legacy helpers with `[lint].persona_step_allowlist`.

#### `@command(name?, description?, hint?)`

```harn,ignore
@command(name: "review", description: "Review the diff", hint: "(optional focus area)")
pipeline review_branch(task) { ... }
```

Marks a top-level pipeline as an ACP slash-command. The Harn ACP adapter
(`harn serve --pipeline ...`) discovers `@command`-tagged pipelines from
the loaded source and advertises them to clients via the
`available_commands_update` session notification. When a client invokes
`/<name> args`, the named pipeline is compiled as the entry point and
the post-name text is passed to the pipeline as the `prompt` global.

Arguments are all optional and string/symbol-typed:

- `name` — slash-command name advertised to the client. Defaults to the
  pipeline's declared name.
- `description` — human-readable summary shown next to the command.
- `hint` — placeholder text the client displays before the user types
  arguments. Maps to ACP's `UnstructuredCommandInput.hint`.

The attribute is compile-time metadata only; outside of an ACP session
the attached pipeline runs unchanged. Slash-command discovery refreshes
on every prompt, so editor changes to the pipeline file propagate
without restarting the agent.

#### `@complexity(allow)`

```harn,ignore
@complexity(allow)
pub fn classify(x: int) -> string {
  if x == 1 { return "one" }
  ...
}
```

Suppresses the `cyclomatic-complexity` lint warning on the attached
function. The bare `allow` identifier is the only currently accepted
form. Use it for functions whose branching is *intrinsic* (parsers,
tier dispatchers, tree-sitter adapters) rather than accidental.

The rule fires when a function's cyclomatic score exceeds the default
threshold of **25**. Projects can override the threshold in
`harn.toml`:

```toml
[lint]
complexity_threshold = 15   # stricter for this project
```

Cyclomatic complexity counts each branching construct (`if`/`else`,
`guard`, `match` arm, `for`, `while`, `try`/`catch`, `ternary`,
`select` case, `retry`) and each short-circuit boolean operator
(`&&`, `||`). Nesting, guard-vs-if, and De Morgan rewrites are all
**score-preserving** — the only way to reduce the count is to
extract helpers or mark the function `@complexity(allow)`.

#### `@acp_tool`

```harn,ignore
@acp_tool(name: "edit", kind: "edit", side_effect_level: "mutation")
pub fn apply_edit(path: string, content: string) -> EditResult { ... }
```

Compiles to the same runtime registration as an imperative
`tool_define(tool_registry(), name, "", { handler, annotations })`
call, with the function bound as the tool's `handler` and every named
attribute argument (other than `name`) lifted into the
`annotations` dict. `name` defaults to the function name when
omitted.

| Argument | Type | Meaning |
|---|---|---|
| `name` | string | Tool name (defaults to fn name) |
| `kind` | string | One of `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `other` |
| `side_effect_level` | string | `none`, `read`, `mutation`, `destructive` |

Other named arguments pass through to the annotations dict unchanged,
so additional `ToolAnnotations` fields can be added without a parser
change.

#### `@invariant`

```harn,ignore
@invariant("fs.writes", "src/**")
tool write_patch() {
  write_file("src/out.txt", "ok")
}
```

Attaches one or more compile-time capability invariants to a `fn`,
`tool`, or `pipeline`. Invariants are only evaluated when the user opts
into them with `harn check --invariants`; plain `harn check` keeps the
baseline type-check + preflight behavior. Each attributed handler is
lowered into a small control-flow graph plus simple data-flow summaries,
then the selected invariant checks run against that IR.

| Invariant | Configuration | Meaning |
|---|---|---|
| `@invariant("fs.writes", "src/**")` | One or more allowed globs, passed positionally or as `path_glob:` / `glob:` / `allow:` | Every reachable file-system write must target a literal path proven to stay within one of the declared globs. |
| `@invariant("budget.remaining", target: "remaining")` | Optional `target:` variable name, default `budget.remaining` | Assignments to the tracked budget value may only preserve it, decrement it, or refresh it from `llm_budget_remaining()`. |
| `@invariant("approval.reachability")` | No extra args | Every reachable side-effecting call must be gated by a prior `request_approval(...)` or enclosed inside a `dual_control(...)` approval scope. |
| `@invariant("capability.policy", allow: "fs.write,llm.model", ...)` | `allow:` capability list; optional `workspace:` globs; optional `require_approval:`, `require_budget:`, `require_autonomy:`, `require_execution_policy:`, `require_command_policy:`, `require_egress_policy:`, and `require_approval_policy:` capability lists | Every reachable capability effect must be declared, and selected capabilities must be guarded by the requested policy/gate on every path. |

The capability-policy lattice recognizes these canonical capabilities:
`fs.write`, `process.exec`, `network.access`, `mcp.connector`,
`llm.model`, `worker.dispatch`, `human.approval`, and
`autonomy.policy`. Common aliases such as `workspace.write`, `command`,
`connector`, `llm`, and `worker` normalize to those names. The checker
classifies direct builtins plus bridge calls such as `mcp_call(...)`,
`host_tool_call(...)`, and `host_call("capability.operation", ...)`.
Calls like `with_execution_policy(...)`, `with_command_policy(...)`,
`with_approval_policy(...)`, `with_autonomy_policy(...)`,
`with_dynamic_permissions(...)`, `egress_policy(...)`,
`request_approval(...)`, `dual_control(...)`, and budget-bearing
`llm_call(..., {budget: ...})` satisfy the corresponding gates.

Invariant violations surface through `harn check --invariants`,
`harn explain --invariant <name> <handler> <file>`, and the LSP. Each
diagnostic carries a concrete CFG path so editors and the CLI can show
how the violating call or assignment is reached.

### Unknown attributes

Unknown attribute names produce a type-checker warning so that
misspellings surface at check time. The attribute itself is otherwise
ignored — code still compiles.

