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
`std/ansi`, `std/table`, `std/diff`, `std/path`, `std/fs`, `std/os`,
`std/edit`, `std/artifact/web`, `std/ui_resource`, `std/cache`,
`std/llm/handlers`, `std/llm/budget`, `std/llm/prompts`, `std/vision`,
`std/context`, `std/agent_state`, `std/agents`, `std/agent/user`,
`std/runtime`, `std/command`, `std/gha`, `std/tui`, `std/git`,
`std/review`, `std/experiments`,
`std/project`, `std/memory`, `std/prompt_library`, `std/monitors`,
`std/triage`, `std/worktree`, `std/checkpoint`, `std/personas/prelude`,
`std/personas/bulletins`,
`std/connectors/shared`, and provider-specific `std/connectors/...` modules).
These add layered
utilities on top of the core builtins; the core builtins themselves are
always available.

## Standard library modules

Harn includes built-in modules that are compiled into the interpreter.
Import them with the `std/` prefix:

```harn
import "std/agent_state"
import "std/agents"
import "std/agent/user"
import { retry_predicate_with_backoff } from "std/async"
import "std/cache"
import "std/collections"
import "std/connectors/shared"
import "std/context"
import "std/edit"
import "std/artifact/web"
import "std/ui_resource"
import "std/experiments"
import "std/git"
import "std/json"
import "std/llm/budget"
import "std/llm/prompts"
import "std/math"
import "std/monitors"
import "std/path"
import "std/personas/bulletins"
import "std/personas/prelude"
import "std/prompt_library"
import "std/review"
import "std/text"
import "std/triage"
import "std/tui"
import "std/vision"
```

### std/async

Polling and retry helpers for closure-shaped conditions:

| Function | Description |
|---|---|
| `wait_for(timeout_ms, interval_ms, predicate)` | Poll `predicate()` until it returns a truthy value or the timeout expires |
| `retry_until(max_attempts, predicate)` | Retry `predicate()` without delay until it returns a truthy value or attempts are exhausted |
| `retry_predicate_with_backoff(max_attempts, base_ms, predicate)` | Retry `predicate()` with exponential backoff between attempts |
| `circuit_call(name, closure)` | Run `closure()` only while the named circuit breaker allows calls, recording success or failure |

### std/connectors/shared

Connector package helpers for common provider plumbing:

| Function | Description |
|---|---|
| `verify_hmac_signature(body, signature, secret, algorithm?)` | Constant-time check for bare or `sha256=`/`sha1=` HMAC signatures |
| `verify_jwt(token, jwks_url, options?)` | Verify a compact JWT against a JWKS URL, or `options.inline_jwks`, returning `{ok, claims, error}` |
| `oauth2_token_refresh(client_id, client_secret, refresh_token, token_url, options?)` | Refresh an OAuth2 access token with form-encoded `grant_type=refresh_token` |
| `rate_limit_token_bucket(state?, config?, now_ms?)` | Pure token-bucket transition for package-local quota decisions |
| `paginate_cursor(initial_url, fetch_fn, cursor_path, options?)` | Collect cursor-paginated pages from a package-supplied fetch closure |

### std/triage

Normalize connector-derived inbox items into host-renderable dashboard cards:

| Function | Description |
|---|---|
| `triage_normalize(input, options?)` | Convert a TriggerEvent or provider payload into `harn.triage_event.v1`, preserving provider raw payload separately |
| `triage_dedupe_key(provider, source_kind, source_url, source_id?)` | Build a stable dedupe key from provider-neutral source provenance |
| `triage_dedupe_events(events)` | Drop duplicate triage events by stable dedupe key |
| `triage_emit(input, options?)` | Validate and append a triage event to the EventLog, returning an emit receipt |
| `triage_start_my_day(inputs, options?)` | Build a deduped Start My Day feed and optionally emit each event |

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
| `truncate_text(text, max_chars?, marker?)` | Keep the first `max_chars` characters and append a deterministic truncation marker |
| `truncate_middle(text, max_chars?, marker?)` | Keep both ends of a long string with an omission marker in the middle |
| `single_line_or(value, fallback?)` | Collapse whitespace into one line, returning `fallback` for blank input |
| `prefix_lines(text, prefix?)` | Prefix every line in a text block |
| `indent(text, spaces?)` | Prefix every line with a fixed number of spaces |
| `detect_compile_error(output)` | Check for compile error patterns (SyntaxError, etc.) |
| `has_got_want(output)` | Check for got/want test failure patterns |
| `format_test_errors(output)` | Extract error-relevant lines (max 20) |

These helpers are intentionally small because they sit under higher-level
reporting modules. For example, harnesses can normalize untrusted command
output before placing it into prompt context:

```harn
import { single_line_or, truncate_middle } from "std/text"

let long_output = "..."
let label = single_line_or(" cargo\n test\t-p harn-vm ", "command")
let summary = truncate_middle(long_output, 2000)
```

### std/edit

Pure helpers for agent-authored text patches:

| Function | Description |
|---|---|
| `edit_apply_old_new_patch(text, old_text, new_text, options?)` | Apply one anchored old/new patch with exact, line-normalized, and structural matching; returns hashes, match kind, line span, changed regions, errors, warnings, and provenance |
| `edit_splice_lines(text, start_line, end_line_exclusive, new_text, options?)` | Replace a half-open 0-based line range and return the same patch metadata shape |
| `edit_changed_regions(before, after)` | Return deterministic line-level changed-region metadata for one contiguous diff |
| `edit_validate_changed_regions(before, after, expected_regions, options?)` | Verify that all changes fall inside expected 0-based line regions |
| `edit_strip_line_number_prefixes(text)` | Remove leading `<spaces>N<space><pipe><space>` line-number prefixes when at least 60% of non-empty lines carry them; useful as preprocessing for `old_text` pasted from a numbered file read |
| `edit_explain_whitespace_difference(needle, matched)` | Diagnose the dominant whitespace cause (tabs vs spaces, base indent, blank lines) when a fuzzy match was needed |
| `edit_check_lazy_truncation(old_content, new_content, options?)` | Detect whole-file rewrites that shrank a file below `min_keep_pct` (35%) of its original line count while still containing lazy placeholders |

Default guardrails reject empty anchors, no-op edits, whitespace-only edits,
lazy omission placeholders (including `// TODO: implement`, `// ... rest`,
`# ...`, `pass # ...`, `/* ... */`, and "unchanged" / "omitted for brevity"
phrases), ambiguous matches, and excessive patch growth.

Structural matching is conservative by default: the needle must contain at
least three non-blank lines and both the first and last anchor lines must
carry a distinctive 4+ character alphanumeric token. Callers can relax this
with `structural_require_anchored_lines: "either" | "none"`,
`structural_min_nonblank_lines: N`, or `structural_anchor_chars: N`. Pass
`strip_line_numbers: true` to apply `edit_strip_line_number_prefixes` to
`old_text` before matching. Successful line/structural matches surface a
`whitespace_explanation` field describing the dominant difference between
the needle and the matched span.

### std/artifact/web

Safe helpers for small generated HTML/CSS/JS artifacts:

| Function | Description |
|---|---|
| `web_artifact_extract(html)` | Extract `<script>`, `<style>`, and body fragments with tag-balance errors and provenance |
| `web_artifact_text_fallback(html, options?)` | Strip script/style/markup into a compact text fallback for hosts without embedded UI support |
| `web_artifact_validate(html, options?)` | Return a machine-readable validation report with fragments, warnings, errors, error codes, text fallback, hashes, and provenance |
| `web_artifact_apply_patch(html, old_text, new_text, options?)` | Compose `std/edit` patching with web artifact validation and changed-region checks |

Validation rejects unclosed core tags, obvious network calls or external
resources, forbidden host bridge calls, dangerous navigation, and inline
secret-like values via `secret_scan`. It does not parse or execute HTML.
Pass `{allow_host_bridge: true}` when the artifact is intentionally an
MCP Apps UI resource that uses `parent.postMessage` to talk to the host;
`std/ui_resource` sets this by default.

### std/ui_resource

MCP Apps-compatible UI resource envelopes with text and structured fallbacks:

| Function | Description |
|---|---|
| `ui_resource(uri, name, html, options?)` | Build a `harn.ui_resource.v1` envelope, validate HTML through `std/artifact/web`, and capture a content hash, size, requested permissions, and a CSP/sandbox policy |
| `ui_tool_meta(resource, options?)` | Build a `_meta.ui` tool-declaration block (`harn.ui_tool_meta.v1`) with visibility, initial view, and permission/capability lists |
| `ui_tool_meta_to_mcp(meta)` | Serialize a tool-meta into the MCP Apps `_meta.ui` dict (`resourceUri`, `visibility`, etc.) for direct inclusion in `tools/list` payloads |
| `ui_text_fallback(content)` / `ui_structured_fallback(data, options?)` | Build text and structured fallback envelopes for hosts without UI support |
| `ui_tool_result(resource, options?)` | Wrap a resource with mandatory text and optional structured fallbacks, defaulting the text to a `web_artifact_text_fallback` projection |
| `ui_select_for_host(result, capabilities?)` | Choose between `ui_resource`, `structured_fallback`, and `text_fallback` for a host based on advertised MCP Apps capability |
| `ui_host_supports_apps(capabilities?)` / `ui_host_capabilities(input?)` | Detect whether an MCP, MCP Apps, or OpenAI Apps SDK host advertises support for the `mcp-app` profile |
| `ui_tool_call_envelope(name, params?, options?)` | Build the host→guest JSON-RPC `tools/call` envelope a sandboxed iframe receives over `postMessage` |
| `ui_context_update_envelope(key, value, options?)` | Build the guest→host JSON-RPC `context/update` envelope used to update model-visible context |
| `ui_resource_csp_header(csp)` / `ui_resource_sandbox_attr(csp)` | Project the resource CSP into header value and `<iframe sandbox>` attribute strings |
| `ui_tool_result_validate(result)` | Reject tool results that are missing required fields or ship a UI resource whose HTML failed validation |

Tool results always carry a non-empty text fallback so plain-text hosts
still see useful output. UI resources fail closed: `ui_tool_result` omits
`ui_resource` whenever validation finds errors (network calls, dangerous
navigation, embedded secrets, etc.) unless the caller opts in with
`allow_invalid_resource: true` for preview-only use. See
[examples/ui_resource](https://github.com/burin-labs/harn/tree/main/examples/ui_resource)
for a dashboard widget and a multi-step review form.

### std/llm/budget

Model-aware token budget helpers:

| Function | Description |
|---|---|
| `estimate_text_tokens(text, model?)` | Count text tokens with tiktoken for known OpenAI models, labeled tiktoken approximations for Claude/Gemini families, or a heuristic fallback |
| `estimate_text_tokens_detail(text, model?)` | Return `{tokens, encoder, source, exact, model_family, known_model_family}` for budget/debug UI |
| `token_count_encoder(model)` | Return encoder metadata for a model without counting text |

### std/llm/prompts

Prompt helpers for deterministic system prompt composition:

| Function | Description |
|---|---|
| `system_prompt_part(content, options?)` | Build a labeled, enableable system prompt fragment for `system_prompt_parts` |
| `system_preamble(content, options?)` | Build a fragment positioned before the call/session system prompt |
| `system_appendix(content, options?)` | Build a fragment positioned after the call/session system prompt |
| `with_system_prompt_parts(options, parts)` | Return `options` with one or more normalized `system_prompt_parts` appended |
| `system_prelude(opts)` | Build a structured system prompt from persona, constraints, tools, output contract, examples, and tone |
| `tool_use_prelude(tools, format)` | Render a deterministic tool-use prelude for non-`agent_loop` callers |

`llm_call` and `agent_loop` also accept raw option keys
`system_preamble`, `system_context`, `system_prompt_parts`,
`system_appendix`, `system_prefix`, and `system_suffix`. For persistent
agent sessions, Harn records the composed session-level system prompt once in
transcript metadata, emits one leading fingerprint event, and sends the
synthesized provider system field on each model request without adding repeated
`system` messages to the replayable message list. A later continuation that
omits all system prompt fields reuses the stored session prompt for the provider
request without writing another transcript event.

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
| `filter_nil<V>(dict<string, V>)` | Remove entries where value is nil, empty string, or "null"; preserves the value type |
| `pick_keys<V>(dict<string, V>, keys, options: PickKeysOptions = {})` | Project a dict onto a key list; pass `{drop_nil: true}` to omit nil values |
| `store_stale(key, max_age_seconds)` | Check if a store key's timestamp is stale |
| `store_refresh(key)` | Update a store key's timestamp to now |

Typed shapes:

| Type | Description |
|---|---|
| `PickKeysOptions = {drop_nil?: bool}` | Options shape consumed by `pick_keys` |

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
| `stream_validator(schema)` | Incrementally validate JSON string or bytes chunks against a schema; `feed(chunk)` returns `Pending`, `Valid`, or `Invalid({reason, path})`, and `value()` returns the parsed JSON once valid |
| `merge<V>(a: dict<string, V>, b: dict<string, V>)` | Shallow-merge two dicts (keys in b override); preserves the value type |
| `pick<V>(data: dict<string, V>, keys)` | Pick specific keys from a dict, dropping nil values; preserves the value type |
| `omit<V>(data: dict<string, V>, keys)` | Omit specific keys from a dict; preserves the value type |

```harn
import "std/json"

let data = safe_parse("{\"x\": 1}")   // {x: 1}, or nil on bad input
let validator = stream_validator({type: "dict", required: ["x"], properties: {x: {type: "int"}}})
let status = validator.feed("{\"x\": 1}")  // JsonStreamStatus.Valid
let parsed = validator.value()             // {x: 1}
let merged = merge({a: 1}, {b: 2})    // {a: 1, b: 2}
let subset = pick({a: 1, b: 2, c: 3}, ["a", "c"])  // {a: 1, c: 3}
let rest = omit({a: 1, b: 2, c: 3}, ["b"])          // {a: 1, c: 3}
```

### std/ansi

Terminal styling helpers that follow Harn's color policy (`NO_COLOR`,
`FORCE_COLOR`, configured color mode, and TTY detection) by default:

| Function | Description |
|---|---|
| `ansi_enabled(options?)` | Return whether ANSI should be emitted for `stdout`, `stderr`, or `stdin`; accepts `{mode: "auto"/"always"/"never", enabled?, stream?}` |
| `ansi_escape(code)` | Build a Select Graphic Rendition escape sequence |
| `ansi_reset()` | Return the reset escape sequence |
| `ansi_strip(text)` | Remove CSI and OSC ANSI escape sequences |
| `ansi_visible_len(text)` | Count visible characters after stripping ANSI escapes |
| `ansi_style(text, style?, options?)` | Apply foreground/background color and common styles when enabled |
| `ansi_color(text, name, options?)` / `ansi_bg(text, name, options?)` | Apply a named foreground or background color |
| `ansi_bold(text, options?)`, `ansi_dim(text, options?)`, `ansi_underline(text, options?)` | Common text styles |
| `ansi_success(text, options?)`, `ansi_warn(text, options?)`, `ansi_error(text, options?)`, `ansi_info(text, options?)`, `ansi_muted(text, options?)` | Semantic styles for CLI status output |
| `ansi_link(label, url, options?)` | Render an OSC-8 terminal hyperlink when ANSI is enabled |
| `ansi_truncate(text, max_chars, marker?)` | Truncate by visible length after stripping ANSI escapes |

Pass `{mode: "always"}` in deterministic tests when you need to assert exact
escape output; otherwise let the auto policy decide.

```harn
import { ansi_success, ansi_strip } from "std/ansi"

let line = ansi_success("passed", {mode: "always"})
println(ansi_strip(line)) // passed
```

### std/table

Deterministic plain-text and Markdown table rendering for logs, summaries, and
GitHub step output:

| Function | Description |
|---|---|
| `render_table(rows, options?)` | Render dict/list/scalar rows as a stable plain-text or Markdown table |
| `render_markdown_table(rows, options?)` | Alias for `render_table(..., {format: "markdown"})` |
| `render_kv_table(data, options?)` | Render a dict as a sorted two-column key/value table |

Columns can be inferred from the first row or supplied with
`{key, header?, align?, width?, max_width?}` entries. Cell text is single-line
normalized, ANSI-aware for visible width, and optionally capped with
`max_cell_width`.

```harn
import { render_table } from "std/table"

println(render_table(
  [{name: "harn", status: "ok"}, {name: "burin", status: "queued"}],
  {columns: ["name", "status"]},
))
```

### std/diff

Pure-Harn line diff helpers for short texts, generated reports, and release
scripts that need a stable unified diff without shelling out:

| Function | Description |
|---|---|
| `diff_lines(before, after)` | Return `{changed, insertions, deletions, old_lines, new_lines, ops}` |
| `unified_diff(before, after, options?)` | Render a unified diff with optional `{path, from_label, to_label, context, color, color_mode}` |
| `colorize_diff(diff_text, options?)` | Apply ANSI coloring to an existing unified diff |
| `diff_summary(before, after)` | Return compact changed/insertions/deletions counts |
| `render_diff_stat(entries, options?)` | Render per-file diff stats from `{path, before, after}` or stat dicts |

`std/diff` favors predictable, dependency-free rendering over competing with
`git diff` for large repository diffs. For large file sets, call `git diff`
through `std/git` or `std/command` and use `colorize_diff` or
`render_diff_stat` for presentation.

```harn
import { unified_diff } from "std/diff"

println(unified_diff("one\ntwo", "one\nthree", {path: "example.txt"}))
```

### std/cache

Persistent cache helpers backed by sqlite or filesystem storage:

| Function | Description |
|---|---|
| `cache_get(key, options?)` | Return `{hit: bool, value?}` for a persistent cache key |
| `cache_put(key, value, options?)` | Store a value with TTL and LRU eviction |
| `cache_clear(options?)` | Remove all entries in the configured namespace |

Options accept `store: "namespace"` or
`store: {backend: "sqlite"|"fs", namespace?, path?}`, plus `ttl`,
`ttl_seconds`, `max_age_seconds`, and `max_entries`.

### std/llm/handlers

LLM call wrappers and middleware helpers:

| Function | Description |
|---|---|
| `llm_cache_key(prompt, system?, options?)` | Derive the canonical `sha256:` key for a cached LLM call |
| `with_cache(prompt, system?, options?)` | Return a cached `llm_call` envelope when available, otherwise call and store the response |
| `with_circuit_breaker(handler, options?)` | Wrap a call handler with per-`(provider, model)` circuit-breaker pooling, or pass `name` to share one circuit |

`with_cache` keys `{prompt, system, provider, model, temperature, top_p,
max_tokens}` after defaults resolve. Its default store is sqlite namespace
`llm.with_cache` with TTL `10m` and LRU size 256. Calls with `tools` bypass the
cache by default; set `skip_when` to a bool or predicate closure to override
that policy.

### std/context

Structured prompt/context assembly helpers:

| Function | Description |
|---|---|
| `section(name, content, options?)` | Create a named context section |
| `context_attach(name, path, content, options?)` | Attach file/path-oriented context |
| `context(sections, options?)` | Build a context object |
| `context_artifact(input, options?)` | Normalize a host-neutral `harn.context_artifact.v1` envelope |
| `context_artifact_from_burin_digest(path, body, options?)` | Wrap a legacy `.burin/context-digests` markdown body without changing its storage path |
| `context_artifact_rank(artifact, options?)` | Score an artifact using freshness, confidence, authority, task/role/path applicability, priority, and token cost |
| `context_artifact_dedupe(artifacts, options?)` | Deduplicate artifacts by explicit source-hash overlap or canonical body key, merging provenance and metadata |
| `context_artifact_merge(left, right, options?)` | Merge two duplicate artifact envelopes |
| `context_artifact_budget(artifacts, options?)` | Rank, deduplicate, filter, and fit artifacts into token/count budgets |
| `context_artifact_select(artifacts, options?)` | Return only the selected artifacts from `context_artifact_budget` |
| `context_render_artifacts(artifacts, options?)` | Render artifacts as `markdown`, `xml`, `plain`, `compact`, or provider-capability-driven `auto` |
| `context_render_logical_section(name, title, body, options?)` | Render a logical section using the same provider capability flags as prompt templates |
| `context_render(ctx, options?)` | Render a context into prompt text |
| `context_add_artifact(ctx, artifact)` | Append a normalized context artifact to an existing context |
| `prompt_compose(task, ctx, options?)` | Compose `{prompt, system, rendered_context}` |

`context_artifact(...)` is the portable repository-context envelope for host
pipelines. It preserves the artifact body in both `body` and `text` and
normalizes metadata used by Harn and Burin-style context pipelines:

- `kind`, `scope.path` / `path`, `language`, `role`, `task`, and
  `applicability.{roles,tasks,languages,paths,tags}`
- `freshness`, `confidence`, `provenance.{source,authority,explanation,...}`,
  and `source_hashes`
- `token_estimate`, `redaction`, `sensitivity`, and arbitrary `metadata`

Rendering with `variant: "auto"` follows the active provider capability flags:
`prefers_xml_scaffolding` selects XML, `prefers_markdown_scaffolding` selects
Markdown, and otherwise the renderer falls back to plain text. Explicit
`variant: "markdown" | "xml" | "plain" | "compact"` overrides capability
selection. The Burin adapter keeps generated digest compatibility during
migration by wrapping existing `.burin/context-digests/*.md` content instead of
requiring hosts to rewrite those files.

### std/context/maintenance

Portable receipts for host-owned background context jobs:

| Function | Description |
|---|---|
| `context_maintenance_dedupe_key(job_id, lifecycle_event, affected_paths?, source_id?)` | Build a stable dedupe key from job identity, lifecycle event, sorted paths, and source/session id |
| `context_maintenance_receipt(job_id, status, input?)` | Normalize a `harn.context_maintenance.job_receipt.v1` receipt |
| `context_maintenance_queue_receipt(job_id, event, options?)` | Return the standard non-blocking queued receipt for a lifecycle hook event |
| `context_maintenance_transition(receipt, status, patch?)` | Move a receipt to `running`, `succeeded`, `failed`, or `skipped` while preserving stable fields |
| `context_maintenance_replay_decision(receipt, options?)` | Return a deterministic replay include/skip decision and matching receipt update |

Use this module from lifecycle hook packages that queue refresh or librarian
jobs. See [Context maintenance hooks](./context-maintenance-hooks.md) for the
canonical file-edited, session-idle, pre-compact, post-turn, and session-end
recipes.

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

### std/memory

Append-only durable memory helpers for observations that should be recalled by
later runs:

| Function | Description |
|---|---|
| `memory_store(namespace, key, value, tags?, options?)` | Append a memory observation and return a `memory_record` dict |
| `memory_recall(namespace, query, k?, options?)` | Return active records ranked by deterministic BM25-style lexical recall |
| `memory_summarize(namespace, window?, options?)` | Return an extractive summary dict for a recent or query-filtered slice |
| `memory_forget(namespace, predicate, options?)` | Append a soft-delete tombstone for matching records |

The default backend writes JSONL events under `.harn/memory/<namespace>/`.
Pass `{root: "path"}` in options to choose a different memory root. Forgetting
is a tombstone operation, so the event log remains auditable.

See [Memory](./memory.md) for record shape, predicate options, and replay
notes.

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

### std/io

Terminal-oriented helpers for scripts that need direct operator input without
shelling out to `bash`:

| Function | Description |
|---|---|
| `is_tty(fd?)` | Return whether fd `0`, `1`, or `2` is attached to a terminal; defaults to stdin |
| `read_line(opts?)` | Read one line from stdin and return `{ok, value?, status?, error?}`; supports `prompt`, `timeout_ms`, `trim`, `echo`, and `raw` options |
| `read_password(prompt?, timeout_ms?)` | Convenience wrapper around `read_line` with terminal echo disabled |
| `write_stderr(text)` | Write text to stderr without appending a newline |

`read_line` statuses are `ok`, `eof`, `timeout`, `interrupt`, and `error`.
Prompts are written to stderr with ANSI sequences preserved.

### std/runtime

Generic host/runtime helpers that are useful across many hosts:

| Function | Description |
|---|---|
| `runtime_task()` | Return the current runtime task string |
| `runtime_pipeline_input()` | Return structured pipeline input from the host |
| `runtime_dry_run()` | Return whether the current run is dry-run only |
| `runtime_approved_plan()` | Return the host-approved plan text when available |
| `process_run(argv, options?)` | Execute a process through argv-mode `process.exec`; prefer this for programmatic commands |
| `process_shell(command, options?)` | Execute an explicit shell command through `process.exec` using the host default shell unless options provide `shell` or `shell_id` |
| `process_result_text(result)` | Return stdout, stderr, combined output, or inline output from a command-runner result |
| `process_result_success(result)` | Return explicit command success when present, otherwise derive success from `status` and `exit_code` |
| `shell_quote(value)` | Quote a value as one POSIX shell argument for unavoidable shell-mode command composition |
| `interaction_ask(question)` | Ask the host/user a question through the typed interaction contract |
| `interaction_ask_with_kind(question, kind)` | Ask the host/user a question with an explicit interaction kind |
| `record_run_metadata(run, workflow_name)` | Persist normalized workflow run metadata through the runtime contract |

### std/fs

File-system convenience helpers built on the globally available host file
primitives. These remove the repeated parent-directory, parse/fallback, and
relative-path boilerplate that release scripts and harnesses tend to carry:

| Function | Description |
|---|---|
| `ensure_parent_dir(path)` | Create the parent directory for a file path when needed |
| `read_json(path, fallback?)` | Read and parse JSON, returning `fallback` for missing or invalid files |
| `read_json_result(path)` | Read and parse JSON as `{ok, value?, error?}` |
| `write_json(path, value, options?)` | Write JSON with optional `{pretty, trailing_newline, ensure_parent}` |
| `read_yaml(path, fallback?)` / `write_yaml(path, value, options?)` | YAML file helpers |
| `read_toml(path, fallback?)` / `write_toml(path, value, options?)` | TOML file helpers |
| `write_lines(path, lines, options?)` | Write a list of lines as one text file |
| `append_line(path, line)` | Append exactly one line with a newline terminator |
| `touch(path)` | Create an empty file if it does not exist |
| `find_files(root, pattern, options?)` | Glob below `root`; pass `{relative: true}` for root-relative paths |
| `relative_path(root, path)` | Return a slash-normalized path relative to `root` when possible |
| `is_file(path)` / `is_dir(path)` | Return type-aware existence checks |
| `file_size(path)` | Return file size in bytes, or `nil` when unavailable |

```harn
import { read_json, relative_path, write_json } from "std/fs"

let path = path_join(temp_dir(), "report/data.json")
write_json(path, {status: "ok"}, {pretty: true})
println(read_json(path).status)
println(relative_path(temp_dir(), path))
```

### std/os

Environment and host diagnostic helpers:

| Function | Description |
|---|---|
| `os_info()` | Return platform, arch, cwd, home/temp dirs, user/host, pid, runtime paths, and TTY status |
| `env_bool(name, fallback?)` | Read a boolean environment variable using common CLI spellings |
| `env_int(name, fallback?)` | Read an integer environment variable |
| `env_list(name, separator?)` | Split a path/list environment variable and drop blanks |
| `require_env(name)` | Return a required env var or throw a clear error |
| `which(binary)` | Resolve an executable on `PATH`, returning `nil` when absent |
| `command_exists(binary)` | Return whether an executable is visible on `PATH` |

Use `std/os` for script-local concerns. Use `std/runtime` when the information
comes from the Harn host contract rather than the ambient operating system.

### std/command

Deterministic command-runner helpers for Harn scripts and harnesses. These use
the same hostlib command runner and artifact reader substrate as model-facing
host tools, but return script-friendly step records with retry bookkeeping and
compact recovery context:

| Function | Description |
|---|---|
| `command_run(spec, options?)` | Run an argv-first command through `hostlib_tools_run_command` and return normalized success, status, output, artifact, and timing fields |
| `command_output_range(locator, options?)` | Range-read a command output artifact by command result, `command_id`, `handle_id`, or artifact path |
| `command_output_tail(locator, options?)` | Read the last bytes of a command output artifact without calculating offsets |
| `command_step(name, spec, options?)` | Run one named command and return a normalized step record with artifacts, tail text, optional classification, optional recovery hint, and attempts |
| `command_step_with_retry(name, spec, options?)` | Explicit alias for `command_step` with a retry policy in options |
| `command_steps_append(steps, name, spec, options?)` | Run a step, append it to a step list, and return `{steps, step, success, status, exit_code}` |
| `command_steps_failed(steps, options?)` | Return whether any step failed and was not caller-marked recovered |
| `command_last_failed_step(steps, options?)` | Return the last unrecovered failed step, or `nil` |
| `command_step_ref(step, options?)` | Return compact agent/recovery context with command identity, status, artifacts, classification, recovery hint, and capped tail |
| `argv_label(argv)` | Render argv parts as a stable space-separated label for logs |
| `command_output_text(result, stream?)` | Extract stdout, stderr, combined output, tail, or failure tail from a command result |
| `command_failure_text(result, options?)` | Render a compact failure block with status, exit code, and capped stdout/stderr |
| `command_result_ok(stdout?, extra?)` | Build a normalized success result for tests and harness adapters |
| `command_result_fail(exit_code?, stderr?, extra?)` | Build a normalized failure result for tests and harness adapters |

`spec` is either an argv list, such as `["git", "status", "--short"]`, or a
dict with `argv`, `cwd`, `env`, `env_mode`, `stdin`, `timeout_ms`, `capture`,
and `max_inline_bytes`. Shell execution is enabled only when the spec
explicitly sets `{mode: "shell", command: "..."}`. Shell mode uses the host
default shell unless the spec or options provide `shell` or `shell_id`.

Retry and classification stay generic. Harnesses provide domain-specific
closures instead of teaching stdlib about a release, repository, package
manager, or host:

```harn,ignore
import { command_step } from "std/command"

let step = command_step("verify package", ["cargo", "test", "-p", "harn-vm"], {
  cwd: repo_root,
  capture: {max_inline_bytes: 12000},
  tail_bytes: 8000,
  retry: {
    max_attempts: 2,
    delay_ms: 0,
    should_retry: { step, _attempt -> return step.exit_code == 101 },
  },
  classify: { step ->
    if contains(step.failure_tail ?? "", "permission denied") {
      return {kind: "permission", retryable: false}
    }
    return nil
  },
  recovery_hint: { step ->
    if step?.classification?.kind == "permission" {
      return "Check credentials or filesystem permissions, then rerun the same step."
    }
    return nil
  },
})
```

### std/gha

GitHub Actions workflow command helpers. The render-only functions are useful
in any terminal; the write helpers append to the file paths GitHub exposes in
`$GITHUB_OUTPUT`, `$GITHUB_ENV`, and `$GITHUB_STEP_SUMMARY`, or to an explicit
path supplied by tests:

| Function | Description |
|---|---|
| `gha_escape_data(value)` | Escape `%`, CR, and LF for workflow command data |
| `gha_escape_property(value)` | Escape workflow command property text |
| `gha_annotation(kind, message, options?)` | Build a `::notice`, `::warning`, or `::error` annotation line |
| `gha_notice(message, options?)` / `gha_warning(message, options?)` / `gha_error(message, options?)` | Print an annotation to stdout |
| `gha_env_block(name, value, delimiter?)` | Build a multiline-safe environment/output block |
| `gha_write_output(name, value, path?)` | Append one value to `$GITHUB_OUTPUT` or `path`; returns `false` outside Actions when no path is available |
| `gha_write_env(name, value, path?)` | Append one value to `$GITHUB_ENV` or `path` |
| `gha_append_summary(markdown, path?)` | Append Markdown to `$GITHUB_STEP_SUMMARY` or `path`, ensuring a trailing newline |

```harn
import { gha_annotation, gha_write_output } from "std/gha"

println(gha_annotation("warning", "line1\nline2", {
  file: "src/main.rs",
  line: 7,
  title: "Heads up",
}))
gha_write_output("release_tag", "v1.2.3")
```

### std/tui

Terminal presentation and picker helpers for interactive scripts:

| Function | Description |
|---|---|
| `page(opts)` | Show a text or markdown artifact through `$PAGER` when stdout is a TTY; otherwise print the full artifact and footer. Returns `{ok, paged, error?}` |
| `select_from(items, opts?)` | Show a picker over `items` and return `{ok, value, status}`. Auto-detects `fzf` then `gum choose` and falls back to a numbered `read_line` menu when neither is available or stdout is not a TTY |
| `terminal_width(default_width?)` | Return the current terminal width, falling back to `default_width` or 80 |
| `rule(char?, width?)` | Return `char` repeated to `width`, or to the terminal width when width is omitted |
| `clear()` | Write the ANSI clear-screen sequence to stdout |

`page` accepts `{title?, body, format?, no_pager?, footer?}`. `format` may be
`"text"` or `"markdown"`; markdown currently passes through raw so callers can
choose their own renderer before paging. In pager mode Harn respects `$PAGER`,
adds `-R -F -X` for `less`, falls back to printing when the pager is missing,
and treats `$PAGER=cat` as print-only.

```harn
import { page } from "std/tui"

let result = page({
  title: "Release audit",
  body: audit_markdown,
  format: "markdown",
})
if !result.ok {
  eprintln(result.error ?? "pager failed")
}
```

`select_from` accepts:

- `prompt` — header shown above the menu / passed to fzf's `--prompt`.
- `display` — `fn(item) -> string` rendering each row. Default reads
  `label` / `title` / `name` / `headline` on dicts and falls through
  to `to_string`.
- `preview` — `fn(item) -> string` rendered into fzf's preview pane.
  The numbered fallback ignores it.
- `multi` — `true` returns a list of items; `false` (default) returns
  one. Numbered fallback accepts comma-separated 1-based indices.
- `default_index` — 0-based index to use when the operator hits Enter
  on an empty line. The numbered menu marks it with a leading `*`.
- `cancel_value` — value returned in `value` when the operator
  cancels (Ctrl+C, Esc, `q`, `/exit`). Defaults to `nil`.
- `prefer_external` — `"auto"` (default), `"fzf"`, `"gum"`, or
  `"none"`. Use `"none"` in scripted tests to force the numbered
  fallback so `mock_stdin` drives the run.
- `header` — extra banner shown above the menu / passed via
  `--header`.

Return shape: `{ok: bool, value: any | list<any>, status: string,
error?: string}`. `status` is `"selected"`, `"cancelled"`, `"eof"`,
or `"error"`. On errors the `error` field carries a one-line message
describing what went wrong; the value is `opts.cancel_value` (or
`nil`).

```harn,ignore
import { select_from } from "std/tui"

let runs = [
  {id: "r-001", label: "r-001 — 2 PRs queued"},
  {id: "r-002", label: "r-002 — verification flake"},
]
let pick = select_from(runs, {prompt: "Pick a run", default_index: 0})
if pick.ok {
  println("operator chose " + pick.value.id)
}
```

### std/git

Local filesystem git helpers for scripts and narrow agent tool registries. The
module keeps existing receipt-producing `git.*` builtins available through
function wrappers, and adds Harn-level argv-mode helpers for common checkout
operations that should not require a generic shell or `run_command` tool.

| Function | Description |
|---|---|
| `git_run(args, options?)` | Run one argv-mode local git command; `args` omit the leading `git` and inherit git-safe env removal |
| `git_status(repo?)` | Return structured porcelain status using the receipt-producing `git.status` builtin |
| `git_discover(path?)` | Return repository root/git-dir metadata using the receipt-producing discover builtin |
| `git_diff(repo?, selector?)` | Return diff text using the receipt-producing `git.diff` builtin |
| `git_current_branch(repo?, options?)` | Return `{success, branch, detached, ...}` for the current checkout |
| `git_log(repo?, options?)` | Return recent commit log output with optional `rev`, `rev_range`, `max_count`, `oneline`, and `paths` |
| `git_switch(branch, repo?, options?)` | Switch to a branch/ref, with optional `create`, `force_create`, `detach`, or `discard_changes` |
| `git_pull_ff_only(repo?, remote?, branch?, options?)` | Run `git pull --ff-only` with optional quiet/prune flags |
| `git_fetch(remote?, repo?, refspecs?)` | Fetch from an existing remote using the receipt-producing `git.fetch` builtin |
| `git_tool_catalog(options?)` | Return searchable metadata for available git operations |
| `git_find_tool(query, options?)` | Rank git operations for a natural-language query with a deterministic lexical scorer |
| `git_run_tool(operation, args?, options?)` | Dispatch one catalogued git operation by id; mutating operations require `include_mutations: true` |
| `git_tools(registry?, options?)` | Build an agent tool registry from selected granular git helpers |
| `git_toolbox_tools(registry?, options?)` | Build a compact `find_git_tool` / `run_git_tool` registry for small/local models |

`git_tools(...)` defaults to read-only helpers (`git_status`,
`git_current_branch`, `git_log`, `git_diff`, `git_branch_list`, and
`git_remote_list`). Mutating helpers such as `git_switch` and
`git_pull_ff_only` are only added when requested through `enabled_tools`, so
harnesses can expose just the git operations a model should be allowed to call.
Pass `defer_loading`, `namespace`, or `tool_config` to make the generated tools
participate in the existing Tool Vault / `tool_search` flow.

```harn,ignore
import { git_tools } from "std/git"

let tools = git_tools(nil, {
  repo: repo_root,
  enabled_tools: ["git_status", "git_log", "git_switch"],
  names: {git_log: "release_git_log"},
})
```

For local models that do better with a tiny stable tool surface, use the
two-tool toolbox:

```harn,ignore
let tools = git_toolbox_tools(nil, {
  repo: repo_root,
  include_mutations: true,
})
```

### std/connectors/github

Typed facade for the package-backed GitHub connector. Provider-specific HTTP,
GraphQL, token rotation, and optional `gh` auth fallback live in the
`harn-github-connector` package; this module keeps scripts on stable Harn helper
names and normalized result envelopes.

| Function | Description |
|---|---|
| `github_slug_from_remote(url)` | Parse common GitHub SSH/HTTPS remote URLs into `owner/repo`, or `nil` when the URL is not GitHub |
| `github_repo(repo, name?)` | Normalize an `owner/repo` slug, GitHub remote URL, repo dict, or owner+repo pair |
| `workflow_dispatch(repo, workflow_id, ref?, inputs?, options?)` | Dispatch a `workflow_dispatch` workflow without shelling out |
| `workflow_runs(repo, options?)` | List repository or workflow-scoped Actions runs |
| `workflow_run(repo, run_id, options?)` | Fetch one Actions run |
| `read_file_at_ref(repo, path, ref?, options?)` | Read decoded repository file text at a ref |
| `latest_release(repo, options?)` | Return a stable latest-release envelope with `tag_name` and `asset_names` |
| `release_assets(repo, release_id?, options?)` | Return a stable release-assets envelope |
| `enable_auto_merge(repo, pull_number, options?)` | Enable PR auto-merge and return `{ok, state, strategy, ...}` |
| `close_pr(repo, pull_number, comment?, options?)` | Optionally comment, then close a PR through the issues endpoint |
| `api_call(path, method, body?, options?)` | Raw GitHub REST escape hatch when no typed helper exists |

The pure-Harn connector package also exports matching owner/repo helper names
such as `actions_workflow_dispatch(...)`, `repos_get_text(...)`,
`github_latest_release(...)`, and `github_close_pr(...)`; the stdlib facade
includes those aliases so release scripts can move between stdlib and package
imports without changing call sites.

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

### std/llm/ensemble

LLM ensemble helpers:

| Function | Description |
|---|---|
| `debate(opts)` | Run a multi-debater LLM debate. Pass `adaptive_stop: true` to stop after two consecutive stable rounds when each debater's consecutive-round drift is below `stability_threshold` (default `0.15`) and emit `debate_stability_short_circuit` on `llm.ensemble.debate` |

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

### std/agent/progress

Agent progress events for hosts that render live agent status:

| Function | Description |
|---|---|
| `agent_progress(input)` | Emit a `progress_reported` event for the current agent session; `input` requires `message` or `entries`, with optional `replace` and `metadata` |
| `agent_progress_tool(registry?, options?)` | Add a handler-backed progress tool to a registry; options may set `name`, `description`, and `system_prompt_nudge` |

### std/agent/chat

Interactive chat-loop helpers built on `agent_loop` sessions:

| Function | Description |
|---|---|
| `agent_chat_loop(opts)` | Run an operator-input / model-turn loop around `agent_loop`, preserving one session across turns and closing it with a typed reason by default |
| `agent_chat_route_input(line, state?, handlers?)` | Apply the shared slash-command convention (`/exit`, `/quit`, `/help`, custom handlers) and return a normalized `{kind, message?, state?}` decision |
| `agent_chat_wait_for_user_tools(registry?)` | Add a `wait_for_user` tool to a registry; the chat loop stops that turn with `stop_reason: "wait_for_user"` and returns to user input |

### std/agent/user

Simulated-user helpers for eval harnesses:

| Function | Description |
|---|---|
| `agentic_user(task_or_config, behavior?, tools?, model?, options?)` | Return an answerer that uses an LLM, optionally with read tools, to stand in for the harness user |
| `scripted_user(script, options?)` | Return a deterministic fixture answerer with string or `{match, reply/action}` script entries |
| `fixture_user(script, options?)` | Alias for `scripted_user(...)` |
| `simulated_user_respond(answerer, payload?)` | Ask an answerer for `{action, message?, reason?}` |
| `user_tools(answerer, registry?, options?)` | Add an `ask_user` tool backed by a simulated answerer |
| `simulated_user_post_turn(answerer, options?)` | Build a `post_turn_callback` that answers plain-text clarification questions |
| `simulated_user_status(answerer)` | Return public state such as reply and LLM-call counts |
| `simulated_user_read_tools(registry?, options?)` | Alias for read-only host research tools appropriate for an agentic simulated user |

### std/handoffs

Harn-owned route policy for typed handoff artifacts:

| Function | Description |
|---|---|
| `handoff_route_select(handoff, routes?, context?)` | Select the first matching handoff route decision from explicit routes or loaded `[[handoff_routes]]` |
| `handoff_routed(payload, routes?, context?)` | Compose and normalize a handoff with the selected target and route decision embedded |
| `handoff_dispatch(handoff, decision?, options?)` | Persist the selected route, enqueue a target-specific dispatch record in the EventLog, and optionally call a local dispatcher hook |

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

### std/personas/bulletins

Transparent profile bulletin envelopes for durable persona facts. See
[Profile bulletins](./personas/profile-bulletins.md) for the full envelope and
review semantics.

| Function | Description |
|---|---|
| `bulletin_propose(input, options?)` | Build a `harn.profile_bulletin.v1` proposal with stable id, evidence, source, and privacy fields |
| `bulletin_id(scope, scope_key, subject, assertion, persona?)` | Compute the stable bulletin id for matching/dedupe |
| `bulletin_validate(bulletin)` | Throw if the envelope is missing required fields or has out-of-range confidence |
| `bulletin_emit(input, options?)` | Append a `proposed` bulletin to `personas.bulletins.proposed` and return an emit receipt |
| `bulletin_decide(bulletin, action, options?)` | Build a typed `harn.profile_bulletin_decision.v1` envelope without emitting |
| `bulletin_emit_decision(decision, options?)` | Append a decision envelope to `personas.bulletins.decisions` |
| `bulletin_accept` / `bulletin_reject` / `bulletin_expire` / `bulletin_supersede` | Decide-and-emit shorthands |
| `bulletin_apply_decisions(bulletins, decisions)` | Project the latest decision per id onto bulletins |
| `bulletin_partition(bulletins)` | Group bulletins by status |
| `bulletin_active(bulletins, now?)` | Return `accepted` bulletins still within their TTL |
| `bulletin_render_for_prompt(bulletins, options?)` | Render an audit-friendly prompt block distinguishing accepted facts from proposals |
| `bulletin_dedupe(bulletins)` | Drop duplicate bulletins by stable id |

### std/personas/prelude

Reusable orchestration primitives for durable persona workflows. See
[Persona Prelude](./personas/prelude.md) for the complete API.

| Function | Description |
|---|---|
| `verify_then_act(verifier, actor, options?)` | Run an actor only after an ok-shaped verifier result |
| `bounded_loop(state_init, step_fn, options?)` | Run a state loop with iteration, duration, and progress bounds |
| `cheap_classify_then_escalate(input, cheap_model, escalate_model, escalation_predicate, options?)` | Use a cheap classifier first and escalate ambiguous or failed cases |
| `parallel_sweep_with_circuit_breaker(items, step_fn, options?)` | Process a bounded parallel sweep and stop scheduling after too many failures |
| `with_audit_receipt(step_fn, options?)` | Wrap a step so success and failure both produce a receipt envelope |
| `with_approval_gate(approval_kind, step_fn, options?)` | Require a replayed or HITL approval before running a step |

### Selective imports

Import specific functions from any module:

```harn
import { extract_paths, parse_cells } from "std/text"
import std::personas::prelude::{verify_then_act}
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
  partials = "pipelines/partials"
  prompts  = "pipelines"
  ```

Both forms reject `..` segments and absolute targets so a
package-rooted asset can never escape the project root. Plain (non-`@`)
paths keep the legacy source-relative behavior unchanged — back-compat
is exact.

`{{ include "@/..." }}` is honored inside `.harn.prompt` files too,
so a deeply-included partial can pull in its sibling fragments by the
same stable name regardless of which entry pipeline rendered it.

Stdlib prompt assets use `std/...harn.prompt` paths:

```harn,ignore
render_prompt("std/agent/prompts/tool_contract_text.harn.prompt", {})
```

These assets are embedded alongside stdlib modules, cache by stable asset id
and content hash, and use `std://...` template URIs in prompt provenance.
They are the default home for reusable model-facing stdlib prompt prose.

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
