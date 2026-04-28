# Modules and imports

Harn supports splitting code across files using `import` and top-level `fn` declarations.

## Importing files

```harn,ignore
import "lib/helpers.harn"
```

The extension is optional — these are equivalent:

```harn,ignore
import "lib/helpers.harn"
import "lib/helpers"
```

Import paths are resolved relative to the current file's directory.
If `main.harn` imports `"lib/helpers"`, it looks for `lib/helpers.harn`
next to `main.harn`.

## Writing a library file

Library files contain top-level `fn` declarations:

```harn
// lib/math.harn

fn double(x) {
  return x * 2
}

fn clamp(value, low, high) {
  if value < low { return low }
  if value > high { return high }
  return value
}
```

When imported, these functions become available in the importing file's scope.

## Using imported functions

```harn,ignore
import "lib/math"

pipeline default(task) {
  println(double(21))        // 42
  println(clamp(150, 0, 100)) // 100
}
```

## Importing pipelines

Imported files can also contain pipelines, which are registered globally by name:

```harn
// lib/analysis.harn
pipeline analyze(task) {
  println("Analyzing: ${task}")
}
```

```harn,ignore
import "lib/analysis"

pipeline default(task) {
  // the "analyze" pipeline is now registered and available
}
```

## What needs an import

Most Harn builtins — `println`, `log`, `read_file`, `write_file`, `llm_call`,
`agent_loop`, `http_get`, `parallel`, `workflow_*`, `transcript_*`,
`mcp_*`, and the rest of the runtime surface — are registered globally and
require **no import statement**. You can call them directly from top-level
code or inside any pipeline.

`import "std/..."` is only needed for the Harn-written helper modules
described below (`std/text`, `std/json`, `std/math`, `std/collections`,
`std/path`, `std/vision`, `std/context`, `std/agent_state`, `std/agents`,
`std/runtime`, `std/review`, `std/experiments`, `std/project`,
`std/prompt_library`, `std/monitors`, `std/worktree`,
`std/checkpoint`). These add layered
utilities on top of the core builtins; the core builtins themselves are
always available.

## Standard library modules

Harn includes built-in modules that are compiled into the interpreter.
Import them with the `std/` prefix:

```harn
import "std/text"
import "std/collections"
import "std/math"
import "std/path"
import "std/vision"
import "std/json"
import "std/context"
import "std/agent_state"
import "std/agents"
import "std/prompt_library"
import "std/review"
import "std/experiments"
import "std/monitors"
```

### std/monitors

Monitor waits for external state with deterministic replay records:

| Function | Description |
|---|---|
| `wait_for(options)` | Poll a source until `condition(state)` is truthy or timeout expires; push-capable sources can wake early from trigger inbox events |

See [Monitor stdlib](./stdlib/monitors.md) for the source shape and result
record.

### std/text

Text processing utilities for LLM output and code analysis:

| Function | Description |
|---|---|
| `int_to_string(value)` | Convert an integer-compatible value to a decimal string |
| `float_to_string(value)` | Convert a float-compatible value to a string |
| `parse_int_or(value, fallback)` | Parse an integer, returning `fallback` on failure |
| `parse_float_or(value, fallback)` | Parse a float, returning `fallback` on failure |
| `extract_paths(text)` | Extract file paths from text, filtering comments and validating extensions |
| `parse_cells(response)` | Parse fenced code blocks from LLM output. Returns `[{type, lang, code}]` |
| `filter_test_cells(cells, target_file?)` | Filter cells to keep code blocks and write_file calls |
| `truncate_head_tail(text, n)` | Keep first/last n lines with omission marker |
| `detect_compile_error(output)` | Check for compile error patterns (SyntaxError, etc.) |
| `has_got_want(output)` | Check for got/want test failure patterns |
| `format_test_errors(output)` | Extract error-relevant lines (max 20) |

### std/experiments

Helpers for structural prompt experiments:

| Function | Description |
|---|---|
| `prompt_order_permutation({seed?})` | Built-in experiment spec that permutes blank-line-separated sections of the latest user prompt |
| `doubled_prompt()` | Built-in experiment spec that duplicates the latest user prompt at the front and back of the message list |
| `chain_of_draft()` | Built-in experiment spec that injects a lightweight `<draft>` / final-answer scaffold |
| `inverted_system()` | Built-in experiment spec that swaps the latest user prompt with the system prompt |
| `custom(label, transform, args?)` | Build a closure-backed experiment spec from Harn |
| `latest_string_user_message(messages)` | Return `{index, message}` for the latest plain-string user message |
| `replace_message(messages, index, message)` | Return a copy of `messages` with one entry replaced |
| `prepend_message(messages, msg)` / `append_message(messages, msg)` | Convenience helpers for custom transforms |

### std/prompt_library

Reusable prompt fragments and deterministic prompt-hotspot proposals:

| Function | Description |
|---|---|
| `prompt_library(fragments?)` | Create an in-memory prompt fragment library |
| `prompt_library_load(path_or_paths)` | Load TOML `[[prompt_fragments]]` catalogs or front-matter `.harn.prompt` files |
| `prompt_library_inject(library, id, bindings?)` | Render one fragment to text |
| `prompt_library_payload(library, id, bindings?)` | Render one fragment plus cache metadata |
| `prompt_library_inject_cluster(library, filters?, bindings?)` | Render matching fragments until `max_tokens` is reached |
| `prompt_library_suggest(library, ctx?)` | Rank fragments by tags and query terms |
| `prompt_library_hotspots(conversations, options?)` | Produce tenant-scoped k-means fragment proposals |
| `prompt_library_review_queue(library, filters?)` | Return pending k-means proposals for review UIs |

See [Prompt library stdlib](./stdlib/prompt-library.md) for the fragment file
format and hotspot proposal shape.

### std/collections

Collection utilities and store helpers:

| Function | Description |
|---|---|
| `filter_nil(dict)` | Remove entries where value is nil, empty string, or "null" |
| `store_stale(key, max_age_seconds)` | Check if a store key's timestamp is stale |
| `store_refresh(key)` | Update a store key's timestamp to now |

### std/math

Extended math utilities:

| Function | Description |
|---|---|
| `clamp(value, lo, hi)` | Clamp a value between min and max |
| `lerp(a, b, t)` | Linear interpolation between a and b by t (0..1) |
| `map_range(value, in_lo, in_hi, out_lo, out_hi)` | Map a value from one range to another |
| `deg_to_rad(degrees)` | Convert degrees to radians |
| `rad_to_deg(radians)` | Convert radians to degrees |
| `sum(items)` | Sum a list of numbers |
| `avg(items)` | Average of a list of numbers (returns 0 for empty lists) |
| `mean(items)` | Arithmetic mean of a list of numbers |
| `median(items)` | Median of a non-empty numeric list |
| `percentile(items, p)` | R-7 percentile interpolation for `p` in `[0, 100]` |
| `argsort(items, score_fn?)` | Indices that would sort a list ascending, optionally by score |
| `top_k(items, k, score_fn?)` | Highest-scoring `k` items, descending |
| `variance(items, sample?)` | Population variance, or sample variance when `sample = true` |
| `stddev(items, sample?)` | Population standard deviation, or sample mode when `sample = true` |
| `minmax_scale(items)` | Scale a numeric list into `[0, 1]`, or all zeros for a constant list |
| `zscore(items, sample?)` | Standardize a numeric list, or all zeros for a constant list |
| `weighted_mean(items, weights)` | Weighted arithmetic mean |
| `weighted_choice(items, weights?)` | Randomly choose one item by non-negative weights |
| `softmax(items, temperature?)` | Convert numeric scores into probabilities |
| `normal_pdf(x, mean?, stddev?)` | Normal density with defaults `mean = 0`, `stddev = 1` |
| `normal_cdf(x, mean?, stddev?)` | Normal cumulative distribution with defaults `mean = 0`, `stddev = 1` |
| `normal_quantile(prob, mean?, stddev?)` | Inverse normal CDF for `0 < prob < 1` |
| `dot(a, b)` | Dot product of two equal-length numeric vectors |
| `vector_norm(v)` | Euclidean norm of a numeric vector |
| `vector_normalize(v)` | Unit-length version of a non-zero numeric vector |
| `cosine_similarity(a, b)` | Cosine similarity of two non-zero equal-length vectors |
| `euclidean_distance(a, b)` | Euclidean distance between two equal-length vectors |
| `manhattan_distance(a, b)` | Manhattan distance between two equal-length vectors |
| `chebyshev_distance(a, b)` | Chebyshev distance between two equal-length vectors |
| `covariance(xs, ys, sample?)` | Population or sample covariance between two numeric lists |
| `correlation(xs, ys, sample?)` | Pearson correlation between two numeric lists |
| `moving_avg(items, window)` | Sliding-window moving average |
| `ema(items, alpha)` | Exponential moving average over a numeric list |
| `kmeans(points, k, options?)` | Deterministic k-means over `list<list<number>>`, returns `{centroids, assignments, counts, iterations, converged, inertia}` |

```harn
import "std/math"

println(clamp(150, 0, 100))         // 100
println(lerp(0, 10, 0.5))           // 5
println(map_range(50, 0, 100, 0, 1)) // 0.5
println(sum([1, 2, 3, 4]))          // 10
println(avg([10, 20, 30]))          // 20
println(percentile([1, 2, 3, 4], 75)) // 3.25
println(top_k(["a", "bbbb", "cc"], 2, { x -> len(x) })) // ["bbbb", "cc"]
println(softmax([1, 2, 3]))         // probabilities summing to 1
println(cosine_similarity([1, 0], [1, 1])) // ~0.707
println(moving_avg([1, 2, 3, 4, 5], 3)) // [2.0, 3.0, 4.0]

let grouped = kmeans([[0, 0], [0, 1], [10, 10], [10, 11]], 2)
println(grouped.centroids)          // [[0.0, 0.5], [10.0, 10.5]]
```

### std/path

Path manipulation utilities:

| Function | Description |
|---|---|
| `ext(path)` | Get the file extension without the dot |
| `stem(path)` | Get the filename without extension |
| `normalize(path)` | Normalize path separators (backslash to forward slash) |
| `is_absolute(path)` | Check if a path is absolute |
| `workspace_info(path, workspace_root?)` | Classify a path at the workspace boundary |
| `workspace_normalize(path, workspace_root?)` | Normalize a path into workspace-relative form when safe |
| `list_files(dir)` | List files in a directory (one level) |
| `list_dirs(dir)` | List subdirectories in a directory |

```harn
import "std/path"

println(ext("main.harn"))          // "harn"
println(stem("/src/main.harn"))    // "main"
println(is_absolute("/usr/bin"))   // true
println(workspace_normalize("/packages/app/SKILL.md", cwd())) // "packages/app/SKILL.md"

let files = list_files("src")
let dirs = list_dirs(".")
```

### std/vision

Deterministic OCR helpers layered on top of the runtime's `vision_ocr(...)`
builtin:

| Function | Description |
|---|---|
| `ocr(image, options?)` | Run OCR over an image path or image payload and return `StructuredText` with text, blocks, lines, tokens, source metadata, backend info, and counts |

```harn
import "std/vision"

let structured = ocr("fixtures/ui.png")
println(structured.text)
println(structured.lines[0]?.text)
println(structured.tokens[0]?.bbox.left)
```

### std/json

JSON utility patterns:

| Function | Description |
|---|---|
| `pretty(value)` | Pretty-print a value as indented JSON |
| `safe_parse(text)` | Safely parse JSON, returning nil on failure instead of throwing |
| `merge(a, b)` | Shallow-merge two dicts (keys in b override keys in a) |
| `pick(data, keys)` | Pick specific keys from a dict |
| `omit(data, keys)` | Omit specific keys from a dict |

```harn
import "std/json"

let data = safe_parse("{\"x\": 1}")   // {x: 1}, or nil on bad input
let merged = merge({a: 1}, {b: 2})    // {a: 1, b: 2}
let subset = pick({a: 1, b: 2, c: 3}, ["a", "c"])  // {a: 1, c: 3}
let rest = omit({a: 1, b: 2, c: 3}, ["b"])          // {a: 1, c: 3}
```

### std/context

Structured prompt/context assembly helpers:

| Function | Description |
|---|---|
| `section(name, content, options?)` | Create a named context section |
| `context_attach(name, path, content, options?)` | Attach file/path-oriented context |
| `context(sections, options?)` | Build a context object |
| `context_render(ctx, options?)` | Render a context into prompt text |
| `prompt_compose(task, ctx, options?)` | Compose `{prompt, system, rendered_context}` |

### std/agent_state

Durable session-scoped state helpers built on the VM-side durable-state
backend:

| Function | Description |
|---|---|
| `agent_state_init(root, options?)` | Create or reopen a session-scoped durable state handle |
| `agent_state_resume(root, session_id, options?)` | Reopen an existing durable state session |
| `agent_state_write(handle, key, content)` | Atomically persist text content under a relative key |
| `agent_state_read(handle, key)` | Read a key, returning `nil` when it is absent |
| `agent_state_list(handle)` | Recursively list keys in deterministic order |
| `agent_state_delete(handle, key)` | Delete a key |
| `agent_state_handoff(handle, summary)` | Write a structured JSON handoff envelope to the reserved handoff key |
| `agent_state_handoff_key()` | Return the reserved handoff key name |

See [Agent state](./agent-state.md) for the handle format, conflict
policies, and backend details.

### std/postgres

Postgres persistence helpers for durable tenant state, event logs, receipts,
claims, and audit records:

| Function | Description |
|---|---|
| `pg_pool(source, options?)` | Open a pooled Postgres connection from a URL, `env:NAME`, `secret:namespace/name`, or source dict |
| `pg_connect(source, options?)` | Open a single-connection pool |
| `pg_query(handle, sql, params?)` | Run a parameterized query and return rows as dictionaries |
| `pg_query_one(handle, sql, params?)` | Return the first row, or `nil` when no rows match |
| `pg_execute(handle, sql, params?)` | Run a statement and return `{rows_affected}` |
| `pg_transaction(pool, callback, options?)` | Run a closure with a scoped transaction handle, committing on success and rolling back on error |
| `pg_close(pool)` | Close a pool handle |
| `pg_mock_pool(fixtures)` | Create fixture-backed Postgres test handle |
| `pg_mock_calls(mock)` | Inspect mock SQL calls |

See [Postgres](./postgres.md) for parameter binding, transaction settings,
RLS examples, pool options, and migration boundaries.

### std/runtime

Generic host/runtime helpers that are useful across many hosts:

| Function | Description |
|---|---|
| `runtime_task()` | Return the current runtime task string |
| `runtime_pipeline_input()` | Return structured pipeline input from the host |
| `runtime_dry_run()` | Return whether the current run is dry-run only |
| `runtime_approved_plan()` | Return the host-approved plan text when available |
| `process_exec(command)` | Execute a process through the typed host contract |
| `process_exec_with_timeout(command, timeout_ms)` | Execute a process with an explicit timeout |
| `interaction_ask(question)` | Ask the host/user a question through the typed interaction contract |
| `interaction_ask_with_kind(question, kind)` | Ask the host/user a question with an explicit interaction kind |
| `record_run_metadata(run, workflow_name)` | Persist normalized workflow run metadata through the runtime contract |

### std/review

Typed review helpers that pair with the global `self_review(...)` builtin:

| Function | Description |
|---|---|
| `review_rubrics()` | Return the built-in rubric library as a dict keyed by preset name |
| `review_rubric(name)` | Return one rubric preset body, or `nil` when the preset is unknown |

Type aliases:

- `ReviewFinding`
- `ReviewRound`
- `ReviewResult`

### std/project

Project metadata helpers plus deterministic project evidence scanning:

| Function | Description |
|---|---|
| `metadata_namespace(dir, namespace)` | Read resolved metadata for a namespace, defaulting to `{}` |
| `metadata_local_namespace(dir, namespace)` | Read only the namespace data stored directly on a directory |
| `project_inventory(namespace?)` | Return `{entries, status}` for metadata-backed project state |
| `project_root_package()` | Infer the repository's root package/module name from common manifests |
| `project_fingerprint(path?)` | Return the normalized shallow repo profile used by higher-level personas |
| `project_scan(path, options?)` | Scan a directory for deterministic L0/L1 evidence |
| `project_enrich(path, options)` | Run caller-owned L2 enrichment over bounded project context with schema validation and caching |
| `project_scan_tree(path, options?)` | Walk subdirectories and return a `{rel_path: evidence}` map |
| `project_enrich(path, options?)` | Run a structured per-directory L2 enrichment with caller-owned prompt/schema |
| `project_deep_scan(path, options?)` | Build or refresh a cached per-directory evidence tree backed by metadata namespaces |
| `project_deep_scan_status(namespace, path?)` | Return the last deep-scan status for a namespace/scope |
| `project_catalog()` | Return the built-in anchor/lockfile catalog used by `project_scan(...)` |
| `project_scan_paths(path, options?)` | Return only the keys from `project_scan_tree(...)` |
| `project_stale(namespace?)` | Return the stale summary from `metadata_status(...)` |
| `project_stale_dirs(namespace?)` | Return the tier1+tier2 stale directory list |
| `project_requires_refresh(namespace?)` | Return `true` when stale or missing hashes require refresh |

Host-specific editor, git, diagnostics, learning, and filesystem/edit helpers
should live in host-side `.harn` libraries built on capability-aware
`host_call(...)`, not in Harn's shared stdlib.

### std/agents

Workflow helpers built on transcripts and `agent_loop`:

| Function | Description |
|---|---|
| `workflow(config)` | Create a workflow config |
| `action_graph(raw, options?)` | Normalize planner output into a canonical action-graph envelope |
| `action_graph_batches(graph, completed?)` | Compute dependency-ready action batches grouped by phase and tool class |
| `action_graph_render(graph)` | Render a human-readable markdown summary of an action graph |
| `action_graph_flow(graph, config?)` | Convert an action graph into a typed workflow graph |
| `action_graph_run(task, graph, config?, overrides?)` | Execute an action graph through the shared workflow runtime |
| `task_run(task, flow, overrides?)` | Run an act/verify/repair workflow |
| `workflow_result_text(result)` | Extract a visible text result from an LLM call, workflow wrapper, or ad hoc payload |
| `workflow_result_run(task, workflow_name, result, artifacts?, options?)` | Normalize an ad hoc result into a reusable run record |
| `workflow_result_persist(task, workflow_name, result, artifacts?, options?)` | Persist an ad hoc result as a run record without going through `workflow_execute` |
| `handoff_artifact(value)` | Wrap a typed handoff payload as a normal workflow artifact without transferring raw transcript history |
| `workflow_session(prev)` | Normalize a task result or transcript into a reusable session object |
| `workflow_session_new(metadata?)` | Create a new empty workflow session |
| `workflow_session_restore(run_or_path)` | Restore a session from a run record or persisted run path |
| `workflow_session_fork(prev)` | Fork a session transcript and mark it `forked` |
| `workflow_session_archive(prev)` | Archive a session transcript |
| `workflow_session_resume(prev)` | Resume an archived session transcript |
| `workflow_session_compact(prev, options?)` | Summarize/compact a session transcript in place |
| `workflow_session_reset(prev, carry_summary)` | Reset a session transcript, optionally carrying summary, while preserving `workflow_id` |
| `continue_as_new(prev, options?)` | Advance workflow generation and return a reset session that keeps the same `workflow_id` |
| `workflow_session_persist(prev, path?)` | Persist the session run record and attach the saved path |
| `workflow_continue(prev, task, flow, overrides?)` | Continue from an existing transcript |
| `workflow_compact(prev, options?)` | Summarize and compact a transcript |
| `workflow_reset(prev, carry_summary)` | Reset or summarize-then-reset a workflow transcript |
| `worker_request(worker)` | Return a worker handle's immutable original request payload |
| `worker_result(worker)` | Return a worker handle/result payload or worker-result artifact payload |
| `worker_provenance(worker)` | Return normalized worker provenance fields |
| `worker_research_questions(worker)` | Return the worker's canonical `research_questions` list |
| `worker_action_items(worker)` | Return the worker's canonical `action_items` list |
| `worker_workflow_stages(worker)` | Return the worker's canonical `workflow_stages` list |
| `worker_verification_steps(worker)` | Return the worker's canonical `verification_steps` list |

`workflow_session(...)` returns a normalized session dict that includes the
current transcript, message count, summary, persisted run metadata,
`workflow_id` when one is available, and a `usage` object when the source run
captured LLM totals:
`{input_tokens, output_tokens, total_duration_ms, call_count}`.

For background or delegated execution, use the worker lifecycle builtins
(`spawn_agent`, `send_input`, `resume_agent`, `wait_agent`, `close_agent`, `list_agents`)
directly from the runtime, or the `worker_*` helpers above when you need the
normalized request/provenance views.

### std/worktree

Helpers for isolated git worktree execution built on `exec_at(...)` and
`shell_at(...)`:

| Function | Description |
|---|---|
| `worktree_default_path(repo, name)` | Return the default `.harn/worktrees/<name>` path |
| `worktree_create(repo, name, base_ref, path?)` | Create or reset a worktree branch at a target path |
| `worktree_remove(repo, path, force)` | Remove a worktree from the parent repo |
| `worktree_status(path)` | Run `git status --short --branch` in the worktree |
| `worktree_diff(path, base_ref?)` | Render diff output for the worktree |
| `worktree_shell(path, script)` | Run an arbitrary shell command inside the worktree |

### Selective imports

Import specific functions from any module:

```harn
import { extract_paths, parse_cells } from "std/text"
```

### Public re-exports

A facade module can re-publish symbols from other modules as part of its
own public surface by prefixing any import with `pub`:

```harn,ignore
// Facade that exposes a curated public API while the implementation
// lives in shard files.
pub import { enrich_source_batch, enrich_source_dir } from "enrich-source"
pub import { enrich_test_batch, enrich_test_dir } from "enrich-test"
pub import "shared"
```

- `pub import "module"` re-exports every public name from the target
  module — the wildcard form.
- `pub import { name } from "module"` re-exports only the listed names.

Re-exports compose: a facade can re-export from another facade and the
chain is followed transitively. `harn check` flags re-export conflicts
when two `pub import`s contribute the same name from different sources,
or when a re-exported name collides with a local `pub` declaration.
Editor go-to-definition follows re-export chains to the originating
declaration.

Plain `import` (without `pub`) remains private — the imported names are
visible only inside the importing file.

## Package-root prompt assets

`render(...)`, `render_prompt(...)`, the `template.render` host
capability, and `{{ include "..." }}` directives accept two
package-root forms in addition to plain source-relative paths. They
exist to keep prompt-asset references stable across pipeline file
moves — a refactor that relocates the caller no longer breaks the
asset path.

```harn,ignore
render_prompt("@/prompts/tool-examples.harn.prompt", bindings)
render_prompt("@partials/tool-examples.harn.prompt", bindings)
```

Resolution rules:

- **`@/<rel>`** — resolves from the calling file's project root (the
  nearest `harn.toml` ancestor). The resulting absolute path is the
  same regardless of how deep the caller sits in the workspace.
- **`@<alias>/<rel>`** — resolves from a `[asset_roots]` entry in the
  project's `harn.toml`:

  ```toml
  [asset_roots]
  partials = "Sources/BurinCore/Resources/pipelines/partials"
  prompts  = "Sources/BurinCore/Resources/pipelines"
  ```

Both forms reject `..` segments and absolute targets so a
package-rooted asset can never escape the project root. Plain (non-`@`)
paths keep the legacy source-relative behavior unchanged — back-compat
is exact.

`{{ include "@/..." }}` is honored inside `.harn.prompt` files too,
so a deeply-included partial can pull in its sibling fragments by the
same stable name regardless of which entry pipeline rendered it.

`harn check` resolves `@`-paths during preflight and fails the run
when:

- the calling file has no `harn.toml` ancestor;
- an `@<alias>/...` reference targets an alias that isn't defined in
  `[asset_roots]`;
- the resolved file does not exist.

`harn contracts bundle` records every resolved `@`-path under
`prompt_assets`, so packagers don't need to maintain a separate file
list. The Harn LSP's go-to-definition jumps straight from a literal
`render_prompt("@/...")` argument to the target prompt file.

## Import behavior

Import paths resolve in this order:

1. `std/<module>` from the embedded stdlib
2. Relative to the importing file, with implicit `.harn`
3. Installed packages under the nearest ancestor `.harn/packages/`
4. Package manifest `[exports]` aliases
5. Package directories with `lib.harn`

Packages can publish stable module entry points in `harn.toml`:

```toml
[exports]
capabilities = "runtime/capabilities.harn"
providers = "runtime/providers.harn"
```

With that manifest, `import "acme/capabilities"` resolves to the
declared file inside `.harn/packages/acme/`, and nested package modules
can import sibling packages through the workspace-level `.harn/packages`
root instead of relying on brittle relative paths.

`harn add`, `harn install`, and `harn lock` populate
`.harn/packages/` from `harn.lock`. Git dependencies must specify `rev`
or `branch`; Harn resolves them to commits, records content hashes, caches
them under the user cache directory, and copies them back into the
workspace as needed. Package dependencies are flattened into the same
workspace package root, so a connector package can import an SDK package
declared in its own `harn.toml` without requiring a sibling checkout.
Directory path dependencies are live-linked when possible and are meant
for local development; git-installed packages cannot publish transitive
path dependencies.

Use registry names for discoverable first-party and community packages:

```bash
harn package search notion
harn package info @burin/notion-sdk
harn add @burin/notion-sdk@1.2.3
```

Registry-name installs resolve through the package index and then write
the same git dependency table as a direct GitHub install. Direct GitHub
refs remain the right choice for private repos, unreleased commits,
temporary pins, and local dogfood before a package is added to the
shared index.

Canonical bootstrap for first-party packages:

```bash
cargo install harn-cli
harn init connector-app
cd connector-app
harn add github.com/burin-labs/harn-openapi@v1.2.3
harn add github.com/burin-labs/notion-sdk-harn@v1.2.3
harn add github.com/burin-labs/notion-connector-harn@v1.2.3
harn install --frozen
harn check main.harn
```

Generated Agents API SDKs should keep the discovery types distinct:
`/v1/agent-card` returns the Harn resource type `HarnAgentCard`, while its
`a2a_agent_card` field is the A2A-compatible `A2aAgentCard` to pass to A2A
SDKs or validators.

Equivalent manifest entries:

```toml
[dependencies]
harn-openapi = { git = "https://github.com/burin-labs/harn-openapi", rev = "v1.2.3" }
notion-sdk-harn = { git = "https://github.com/burin-labs/notion-sdk-harn", rev = "v1.2.3" }
notion-connector-harn = { git = "https://github.com/burin-labs/notion-connector-harn", rev = "v1.2.3" }
```

Installed package code is importable, but package manifests do not
automatically inject host runtime configuration. Runtime tables such as
`[llm]`, `[capabilities]`, `[[hooks]]`, and `[[triggers]]` only come
from the root project's `harn.toml` by default.

1. The imported file is parsed and executed
2. Pipelines in the imported file are registered by name
3. Non-pipeline top-level statements (fn declarations, let bindings) are executed, making their values available
4. Circular imports are detected and skipped (each file is imported at most once)
5. The working directory is temporarily changed to the imported file's directory, so nested imports resolve correctly
6. Source-relative builtins like `render(...)` inside imported functions resolve
   paths relative to the imported module's directory, not the entry pipeline

## Static cross-module checking

`harn check`, `harn run`, `harn bench`, and the Harn LSP all build a
**module graph** from the entry file that follows `import` statements
transitively, so they share one consistent view of what names are
visible in each module.

When every import in a file resolves, the typechecker treats a call to
an unknown name as an **error** (not a lint warning):

```text
error: call target `helpr` is not defined or imported
```

Resolution is conservative: if any import in the file fails to resolve
(missing file, parse error, nonexistent package), the stricter
cross-module check is turned off for that file and only the normal
builtin/local-declaration check applies. That way one broken import
does not produce a flood of follow-on undefined-name errors.

Go-to-definition in the LSP uses the same graph, so navigation works
across any chain of imports — not just direct ones.

## Import collision detection

If two wildcard imports export a function with the same name, Harn will
report an error at both runtime and during `harn check` preflight:

```text
Import collision: 'helper' is already defined when importing lib/b.harn.
Use selective imports to disambiguate: import { helper } from "..."
```

To resolve collisions, use selective imports to import only the names
you need from each module:

```harn,ignore
import { parse_output } from "lib/a"
import { format_result } from "lib/b"
```

## Pipeline inheritance

Pipelines can extend other pipelines:

```harn
pipeline base(task) {
  println("Step 1: setup")
  println("Step 2: execute")
  println("Step 3: cleanup")
}

pipeline custom(task) extends base {
  override setup() {
    println("Custom setup")
  }
}
```

If the child pipeline has `override` declarations, the parent's body runs
with the overrides applied. If the child has no overrides, the child's body
replaces the parent's entirely.

## Organizing a project

A typical project structure:

```text
my-project/
  main.harn
  lib/
    context.harn      # shared context-gathering functions
    agent.harn        # shared agent utility functions
    helpers.harn      # general-purpose utilities
```

```harn,ignore
// main.harn
import "lib/context"
import "lib/agent"
import "lib/helpers"

pipeline default(task, project) {
  let ctx = gather_context(task, project)
  let result = run_agent(ctx)
  finalize(result)
}
```
