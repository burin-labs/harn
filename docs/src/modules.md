# Modules and imports

Harn supports splitting code across files using `import` and top-level `fn` declarations.

## Importing files

```harn
import "lib/helpers.harn"
```

The extension is optional — these are equivalent:

```harn
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

```harn
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

```harn
import "lib/analysis"

pipeline default(task) {
  // the "analyze" pipeline is now registered and available
}
```

## Standard library modules

Harn includes built-in modules that are compiled into the interpreter.
Import them with the `std/` prefix:

```harn
import "std/text"
import "std/collections"
import "std/math"
import "std/path"
import "std/json"
import "std/context"
import "std/agents"
```

### std/text

Text processing utilities for LLM output and code analysis:

| Function | Description |
|---|---|
| `extract_paths(text)` | Extract file paths from text, filtering comments and validating extensions |
| `parse_cells(response)` | Parse fenced code blocks from LLM output. Returns `[{type, lang, code}]` |
| `filter_test_cells(cells, target_file?)` | Filter cells to keep code blocks and write_file calls |
| `truncate_head_tail(text, n)` | Keep first/last n lines with omission marker |
| `detect_compile_error(output)` | Check for compile error patterns (SyntaxError, etc.) |
| `has_got_want(output)` | Check for got/want test failure patterns |
| `format_test_errors(output)` | Extract error-relevant lines (max 20) |

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

```harn
import "std/math"

println(clamp(150, 0, 100))         // 100
println(lerp(0, 10, 0.5))           // 5
println(map_range(50, 0, 100, 0, 1)) // 0.5
println(sum([1, 2, 3, 4]))          // 10
println(avg([10, 20, 30]))          // 20
```

### std/path

Path manipulation utilities:

| Function | Description |
|---|---|
| `ext(path)` | Get the file extension without the dot |
| `stem(path)` | Get the filename without extension |
| `normalize(path)` | Normalize path separators (backslash to forward slash) |
| `is_absolute(path)` | Check if a path is absolute |
| `list_files(dir)` | List files in a directory (one level) |
| `list_dirs(dir)` | List subdirectories in a directory |

```harn
import "std/path"

println(ext("main.harn"))          // "harn"
println(stem("/src/main.harn"))    // "main"
println(is_absolute("/usr/bin"))   // true

let files = list_files("src")
let dirs = list_dirs(".")
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

### std/project

Project metadata and scanner helpers built on `metadata_*`, `project_root()`, and `scan_directory(...)`:

| Function | Description |
|---|---|
| `metadata_namespace(dir, namespace)` | Read resolved metadata for a namespace, defaulting to `{}` |
| `metadata_local_namespace(dir, namespace)` | Read only the namespace data stored directly on a directory |
| `project_inventory(namespace?)` | Return `{entries, status}` for metadata-backed project state |
| `project_root_package()` | Infer the repository's root package/module name from common manifests |
| `project_scan(path, options?)` | Scan files/directories with the runtime scanner options |
| `project_scan_paths(path, options?)` | Return only the scanned paths |
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
| `task_run(task, flow, overrides?)` | Run an act/verify/repair workflow |
| `workflow_result_text(result)` | Extract a visible text result from an LLM call, workflow wrapper, or ad hoc payload |
| `workflow_result_run(task, workflow_name, result, artifacts?, options?)` | Normalize an ad hoc result into a reusable run record |
| `workflow_result_persist(task, workflow_name, result, artifacts?, options?)` | Persist an ad hoc result as a run record without going through `workflow_execute` |
| `workflow_session(prev)` | Normalize a task result or transcript into a reusable session object |
| `workflow_session_new(metadata?)` | Create a new empty workflow session |
| `workflow_session_restore(run_or_path)` | Restore a session from a run record or persisted run path |
| `workflow_session_fork(prev)` | Fork a session transcript and mark it `forked` |
| `workflow_session_archive(prev)` | Archive a session transcript |
| `workflow_session_resume(prev)` | Resume an archived session transcript |
| `workflow_session_compact(prev, options?)` | Summarize/compact a session transcript in place |
| `workflow_session_reset(prev, carry_summary)` | Reset a session transcript, optionally carrying summary |
| `workflow_session_persist(prev, path?)` | Persist the session run record and attach the saved path |
| `workflow_continue(prev, task, flow, overrides?)` | Continue from an existing transcript |
| `workflow_compact(prev, options?)` | Summarize and compact a transcript |
| `workflow_reset(prev, carry_summary)` | Reset or summarize-then-reset a workflow transcript |

`workflow_session(...)` returns a normalized session dict that includes the
current transcript, message count, summary, persisted run metadata, and a
`usage` object when the source run captured LLM totals:
`{input_tokens, output_tokens, total_duration_ms, call_count}`.

For background or delegated execution, use the worker lifecycle builtins
(`spawn_agent`, `send_input`, `resume_agent`, `wait_agent`, `close_agent`, `list_agents`)
directly from the runtime.

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

## Import behavior

1. The imported file is parsed and executed
2. Pipelines in the imported file are registered by name
3. Non-pipeline top-level statements (fn declarations, let bindings) are executed, making their values available
4. Circular imports are detected and skipped (each file is imported at most once)
5. The working directory is temporarily changed to the imported file's directory, so nested imports resolve correctly
6. Source-relative builtins like `render(...)` inside imported functions resolve
   paths relative to the imported module's directory, not the entry pipeline

## Import collision detection

If two wildcard imports export a function with the same name, Harn will
report an error at both runtime and during `harn check` preflight:

```text
Import collision: 'helper' is already defined when importing lib/b.harn.
Use selective imports to disambiguate: import { helper } from "..."
```

To resolve collisions, use selective imports to import only the names
you need from each module:

```harn
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
  override fn setup() {
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

```harn
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
