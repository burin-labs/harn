# Edit stdlib

`import "std/edit"` exposes safe, structured helpers for mutating
source files. Three flavors live side by side:

- `edit_apply_node` — AST-precise replace via a Tree-Sitter query.
  The default reach when agents need to rewrite a function body,
  swap a call expression, or update a typed declaration.
- `edit_insert_at_anchor` — AST-precise insert before/after/inside an
  anchor node. The default reach for adding a new function, import,
  test case, or match arm.
- `edit_rename_symbol` — safe cross-file rename driven by the typed
  symbol graph (#2434). The default reach when one identifier needs
  to flip across the workspace without colliding on partial-name
  matches.
- `edit_fast_apply` / `fast_apply` — merge-model-assisted full-file
  application for broad edit intent. It reads the current file,
  asks the configured `merge` model role for complete updated bytes,
  validates and previews the result, then commits through
  `edit_safe_text_patch`.
- `edit_apply_old_new_patch` — collision-aware old/new text patch
  with exact / line / structural matching modes. The default reach
  when the language has no tree-sitter grammar or when the model
  reasoned in terms of literal lines.
- `edit_dry_run` — render a multi-op plan to a per-file unified diff
  without touching disk. The default reach when an agent wants to
  "measure twice, cut once" before committing a multi-step edit.
- `edit_capabilities` — report which AST-precise primitives are
  available per language. The default reach when the loop needs to
  decide between an AST edit and a text fallback before acting (see
  [Language coverage](#language-coverage--capability-matrix)).
- Validators and helpers — `edit_changed_regions`,
  `edit_validate_changed_regions`, `edit_check_lazy_truncation`,
  `edit_explain_whitespace_difference`, `edit_strip_line_number_prefixes`.

> **Feature gate.** Every helper that reads or writes a file on disk —
> `edit_apply_node`, `edit_insert_at_anchor`, `edit_fast_apply`, and
> `edit_safe_text_patch` —
> is gated on the deterministic-tools feature, the same gate the
> `hostlib_tools_*` file I/O builtins use. Call
> `hostlib_enable("tools:deterministic")` once at the start of the
> pipeline before invoking them; otherwise the underlying builtin returns
> a structured error pointing you back here. Pure in-memory helpers
> (`edit_apply_old_new_patch`, `edit_dry_run`, the validators) are not
> gated.

## `edit_apply_node` — Tree-Sitter query → format-preserving replace

`edit_apply_node({path, query, replacement, ...})` locates AST node(s)
via a Tree-Sitter S-expression query and replaces each match's bytes
with `replacement`. Because the splice operates on the matched node's
start/end bytes, leading indentation, surrounding whitespace, and
trailing trivia outside the matched span are preserved verbatim.

Backed by the `hostlib_ast_apply_node` builtin (issue
[#2506](https://github.com/burin-labs/harn/issues/2506)) under the
`std/edit` umbrella epic
[#2497](https://github.com/burin-labs/harn/issues/2497).

Requires `hostlib_enable("tools:deterministic")` first — it writes the
edited source to disk (see [Feature gate](#edit-stdlib)).

### Parameters

| Field | Required | Notes |
|---|---|---|
| `path` | yes | File to mutate. |
| `query` | yes | Tree-Sitter query with at least one capture. |
| `replacement` | yes | Replacement text for each selected node. |
| `language` | no | Inferred from the file extension when missing. |
| `target_capture` | no | Capture name to treat as the replaced span. Defaults to `target`. Single-capture queries accept any name. |
| `select` | no | `"unique"` (default) \| `"first"` \| `"all"` \| `"nth"`. |
| `nth` | when `select == "nth"` | 1-based index. |
| `dry_run` | no | When `true`, the file is left untouched and `preview` carries the would-be content. |
| `validate` | no, default `true` | Re-parse the post-edit source; reject on ERROR / MISSING nodes. |
| `session_id` | no | Routes the read + write through the staged filesystem (#1722). |
| `max_bytes` | no | Read cap; `0` (default) means unlimited. |

### Result

The response is a tagged union over `result`. Successful edits return
`result == "applied"`. Failure modes:

| `result` | When |
|---|---|
| `no_match` | The query produced zero captures at `target_capture`. |
| `ambiguous` | `select == "unique"` but the query matched more than once. |
| `invalid_query` | Tree-sitter rejected the query string; `error_row`/`error_column` carry the position. |
| `unsupported_language` | The file extension did not resolve to a tree-sitter grammar. |
| `syntax_error` | `validate == true` and the post-edit source has tree-sitter errors. The file on disk is left untouched. |

Every result carries `applied: bool` (mirrors `result == "applied"`),
`match_count`, and a `provenance` envelope. Successful results
additionally carry `edits` (per-match span + replacement metadata),
`preview` (post-splice source), and SHA-256 hashes of the before and
after text.

### Worked example: rename a function body

```harn,ignore
import "std/edit"

pipeline default() {
  // src/lib.rs contains:
  //
  //   fn greet(name: &str) -> String {
  //       format!("hi {name}")
  //   }
  //
  const result = edit_apply_node(
    {
      path: "src/lib.rs",
      query: "(function_item name: (identifier) @name (#eq? @name \"greet\") body: (block) @target)",
      replacement: "{ format!(\"hi {name}!\") }",
    },
  )
  __io_println(result.result)               // "applied"
  __io_println(result.match_count == 1)     // true
  __io_println(contains(result.preview, "hi {name}!"))
}
```

The body of `greet` is replaced; the surrounding signature
(`fn greet(name: &str) -> String`) keeps its leading indentation, the
closing brace stays anchored, and any trailing whitespace below is
untouched.

### Multi-match selectors

```harn,ignore
import "std/edit"

pipeline default() {
  // Rewrite every function body in the file.
  const all = edit_apply_node(
    {
      path: "src/lib.rs",
      query: "(function_item body: (block) @target)",
      replacement: "{ unimplemented!() }",
      select: "all",
    },
  )

  // Rewrite the second function only.
  const second = edit_apply_node(
    {
      path: "src/lib.rs",
      query: "(function_item body: (block) @target)",
      replacement: "{ todo!() }",
      select: "nth",
      nth: 2,
    },
  )

  __io_println(all.match_count)
  __io_println(second.match_count == 1)
}
```

### Validation rejects bad edits

```harn,ignore
import "std/edit"

pipeline default() {
  // Intentional syntax error.
  const result = edit_apply_node(
    {
      path: "src/lib.rs",
      query: "(function_item body: (block) @target)",
      replacement: "{ (",
    },
  )
  __io_println(result.applied)              // false
  __io_println(result.result)               // "syntax_error"
  __io_println(result.details)              // human-readable diagnostic
  // src/lib.rs is unchanged on disk.
}
```

### Staged-filesystem atomicity

When the hostlib session is in staged mode (see
[`hostlib_fs_set_mode`](https://github.com/burin-labs/harn/blob/main/crates/harn-hostlib/src/fs.rs)),
passing the session id routes both the read and the write through the
overlay. The edit becomes part of the same transaction as any sibling
staged writes, and the working tree is only touched on
`hostlib_fs_commit_staged`.

```harn,ignore
import "std/edit"

pipeline default() {
  const session = harness.session_id()
  const _ = hostlib_fs_set_mode({session_id: session, mode: "staged"})
  const result = edit_apply_node(
    {
      path: "src/lib.rs",
      query: "(function_item body: (block) @target)",
      replacement: "{ 42 }",
      select: "first",
      session_id: session,
    },
  )
  __io_println(result.applied)
  // The working tree only changes on commit.
  const _ = hostlib_fs_commit_staged({session_id: session})
}
```

### When `edit_apply_node` is not the right tool

- The language has no tree-sitter grammar (returns
  `result == "unsupported_language"`). Fall back to
  `edit_apply_old_new_patch`.
- The change crosses files (rename-style refactors). Reach for
  [`edit_rename_symbol`](#edit_rename_symbol--safe-cross-file-rename)
  below — it's backed by the
  [#2434](https://github.com/burin-labs/harn/issues/2434) symbol graph.
- The change is sub-token (rewrite a single identifier inside a larger
  expression). The minimum granularity for `apply_node` is one
  tree-sitter node.

## `edit_insert_at_anchor` — splice a sibling or child relative to an AST anchor

`edit_insert_at_anchor({path, query, position, content, ...})` is the
companion to `edit_apply_node` for the *other* canonical mutation: not
"replace this node" but "add a sibling next to it" or "append a child
inside it". The query locates a single anchor; `position` picks the
slot; `content` is re-indented to the right depth and spliced in.

Backed by the `hostlib_ast_insert_at_anchor` builtin (issue
[#2507](https://github.com/burin-labs/harn/issues/2507)) under the same
[#2497](https://github.com/burin-labs/harn/issues/2497) umbrella epic
as `edit_apply_node`.

Requires `hostlib_enable("tools:deterministic")` first — it writes the
edited source to disk (see [Feature gate](#edit-stdlib)).

### Parameters

| Field | Required | Notes |
|---|---|---|
| `path` | yes | File to mutate. |
| `query` | yes | Tree-Sitter query naming the anchor. Must match exactly one node — multi-match returns `ambiguous`. |
| `position` | yes | `"before"` \| `"after"` \| `"first_child"` \| `"last_child"`. |
| `content` | yes | Text to insert. Re-indented to the target depth on each line unless `reindent: false`. |
| `language` | no | Inferred from the file extension when missing. |
| `target_capture` | no | Capture name to treat as the anchor. Defaults to `anchor`. Single-capture queries accept any name. |
| `indent` | no | Indent unit override for `first_child` / `last_child` (e.g. `"  "` or `"\t"`). Otherwise detected from the file. |
| `reindent` | no, default `true` | When `false`, splice `content` verbatim instead of prefixing each line with the inferred indent. |
| `dry_run` | no | When `true`, the file is left untouched and `preview` carries the would-be content. |
| `validate` | no, default `true` | Re-parse the post-edit source; reject on ERROR / MISSING nodes. |
| `session_id` | no | Routes the read + write through the staged filesystem (#1722). |
| `max_bytes` | no | Read cap; `0` (default) means unlimited. |

### Position semantics

- `before` / `after` — insert at the anchor's indent depth. The
  anchor's leading whitespace on the line stays put; the new content
  lands on a fresh line above or below.
- `first_child` — insert just past the anchor's opening delimiter
  (e.g. the `{` of a block). Indent is taken from the existing first
  named child if any, else `anchor_indent + indent_unit`.
- `last_child` — insert just before the anchor's closing delimiter
  (e.g. the `}` of a block). Indent is computed the same way.

### Result

| `result` | When |
|---|---|
| `applied` | The splice landed; the file is updated unless `dry_run`. |
| `no_match` | The query produced zero anchors. |
| `ambiguous` | The query matched more than one node; tighten with a `(#eq? @name "…")` predicate. |
| `invalid_query` | Tree-sitter rejected the query string. |
| `invalid_anchor` | The anchor cannot host the requested position (e.g. `first_child` on a leaf node). |
| `unsupported_language` | The file extension did not resolve to a tree-sitter grammar. |
| `syntax_error` | `validate == true` and the post-edit source has tree-sitter errors. The file on disk is left untouched. |

On `applied`, the response carries the anchor span, the byte offset the
splice landed at, the actual `inserted_text`, the inferred `indent`,
and SHA-256 hashes of the before / after text.

### Worked example: append a test to a Rust mod

```harn,ignore
import "std/edit"

pipeline default() {
  // src/lib.rs contains:
  //
  //   #[cfg(test)]
  //   mod tests {
  //       #[test]
  //       fn one() {}
  //   }
  //
  const result = edit_insert_at_anchor({
    path: "src/lib.rs",
    query: "(mod_item name: (identifier) @name (#eq? @name \"tests\") body: (declaration_list) @anchor)",
    position: "last_child",
    content: "#[test]\nfn two() {}",
  })
  __io_println(result.result)            // "applied"
  __io_println(result.position)          // "last_child"
  __io_println(contains(result.preview, "fn two()"))
}
```

The new `#[test] fn two() {}` lands at the right depth inside the
`tests` mod, right after `fn one()`.

### Worked example: add an import after the last one

```harn,ignore
import "std/edit"

pipeline default() {
  const result = edit_insert_at_anchor({
    path: "src/index.ts",
    // Anchor on the last existing import. `select` is not exposed —
    // tighten the query if you need a specific one.
    query: "(import_statement source: (string (string_fragment) @src) (#eq? @src \"./util\")) @anchor",
    position: "after",
    content: "import { extra } from \"./extra\";",
  })
  __io_println(result.applied)
}
```

### Ambiguity is the default failure mode for under-specified queries

```harn,ignore
import "std/edit"

pipeline default() {
  const result = edit_insert_at_anchor({
    path: "src/lib.rs",
    query: "(function_item) @anchor",       // matches every top-level fn
    position: "after",
    content: "fn baz() {}",
  })
  // result.applied == false, result.result == "ambiguous",
  // result.match_count carries the number of competing anchors.
}
```

Add a `(#eq? @name "alpha")` predicate (or use `name: (identifier) @name`
plus the predicate) to pin a single anchor.

### When `edit_insert_at_anchor` is not the right tool

- The change replaces an existing span. Reach for
  [`edit_apply_node`](#edit_apply_node--tree-sitter-query--format-preserving-replace)
  instead.
- The language has no tree-sitter grammar (returns
  `result == "unsupported_language"`). Fall back to
  `edit_apply_old_new_patch`.
- The anchor cannot host children (e.g. an identifier) and you asked
  for `first_child` / `last_child`. The call returns
  `result == "invalid_anchor"` with the anchor span attached.

## `edit_safe_text_patch` — multi-hunk text edits with staged-fs collision rejection

`edit_safe_text_patch({path, expected_hash, hunks, ...})` reads the
file through the staged-fs overlay, runs each `{old_text, new_text}`
hunk through the same matcher as `edit_apply_old_new_patch`, and
writes the composed post-image back atomically. When the observed
pre-image hash diverges from `expected_hash` the call returns
`result == "stale_base"` without writing — callers should re-read and
retry, never blindly clobber.

Backed by the `hostlib_fs_safe_text_patch` builtin (issue
[#2509](https://github.com/burin-labs/harn/issues/2509)).

Requires `hostlib_enable("tools:deterministic")` first — it reads
(`hostlib_fs_read_text`) and writes the target file on disk (see
[Feature gate](#edit-stdlib)).

### Parameters

| Field | Required | Notes |
|---|---|---|
| `path` | yes | File to mutate. |
| `hunks` | yes | List of `{old_text, new_text, options?}`. Each hunk's `options` override `match_options` for that hunk; matcher options accept the same keys as `edit_apply_old_new_patch`. |
| `expected_hash` | no | `sha256:HEX` of the pre-image the caller observed. When omitted the stale-base check is skipped (still atomic w.r.t. other staged-fs writers in the same process). |
| `session_id` | no | Hostlib session whose staged-fs overlay should intercept the read and the write. |
| `match_options` | no | Default `edit_apply_old_new_patch` options merged into every hunk. |
| `dry_run` | no | When `true` the post-image is rendered into `preview` but no bytes are written. |
| `create_parents` | no, default `true` | Create missing parent directories on write. When `false`, a missing parent is a hard error (the call does **not** silently fall back to creating it). |
| `overwrite` | no, default `true` | Allow replacing existing files. |

### Result

| `result` | When |
|---|---|
| `applied` | All hunks matched and the bytes changed. `bytes_written` / `created` describe the write. |
| `no_op` | All hunks matched but the post-image equals the pre-image (skipped the write). |
| `stale_base` | `expected_hash` did not match the observed pre-image, or another writer committed between snapshot and write. No bytes were written. |
| `hunk_conflict` | A hunk's `old_text` failed to match against the running post-image. `failed_hunk_index` and `failed_hunk_error_code` describe which hunk and why. None of the hunks committed. |

Every result carries `before_sha256` / `after_sha256` / `current_hash`,
the per-hunk `hunk_results`, a `telemetry` envelope (`applied`,
`stale_base`, `hunk_conflict`, `no_op` counters plus `hunks`), and a
`provenance` envelope so hosts can roll up stale-base / hunk-conflict
rates and average hunks-per-patch without re-parsing logs. The same
counters fire through the `SafeTextPatchResult` agent event so hosts
that subscribe to the event stream see every terminal outcome without
polling.

`applied` is `true` whenever the hunk matcher succeeded — including
when `dry_run: true` skipped the on-disk write. Distinguish the two
via the `dry_run` field on the result, mirroring `edit_apply_node`.

### Worked example: two hunks under stale-base guarding

```harn,ignore
import { edit_safe_text_patch } from "std/edit"

pipeline default() {
  const path = "src/lib.rs"
  // 1) Snapshot the pre-image hash through the same overlay.
  const snapshot = hostlib_fs_read_text({path: path})
  // 2) Compose a patch off the snapshot.
  const result = edit_safe_text_patch(
    {
      path: path,
      expected_hash: snapshot.sha256,
      hunks: [
        {old_text: "return 1", new_text: "return 11"},
        {old_text: "return 3", new_text: "return 33"},
      ],
    },
  )
  __io_println(result.result)                  // "applied"
  __io_println(result.telemetry.applied)       // 1
  __io_println(result.hunks_count)             // 2
  // On a stale_base result, re-read snapshot.sha256 and retry.
  if result.result == "stale_base" {
    __io_println(result.current_hash)          // overlay's actual hash
  }
}
```

### Multi-agent collision rejection

When two agents race against the same file, the staged-fs overlay
turns the race into a deterministic `stale_base` outcome:

```harn,ignore
import { edit_safe_text_patch } from "std/edit"

pipeline default() {
  const session = "demo"
  hostlib_enable("tools:deterministic")
  hostlib_fs_set_mode({session_id: session, mode: "staged"})
  const pre = hostlib_fs_read_text({path: "src/main.rs", session_id: session})

  // Sibling agent stages a competing write — overlay diverges.
  hostlib_tools_write_file(
    {session_id: session, path: "src/main.rs", content: "// sibling won\n"},
  )

  const losing = edit_safe_text_patch(
    {
      path: "src/main.rs",
      expected_hash: pre.sha256,
      hunks: [{old_text: "TODO", new_text: "DONE"}],
      session_id: session,
    },
  )
  __io_println(losing.result)                 // "stale_base"

  // Retry against the now-current overlay hash.
  const refreshed = hostlib_fs_read_text({path: "src/main.rs", session_id: session})
  const winner = edit_safe_text_patch(
    {
      path: "src/main.rs",
      expected_hash: refreshed.sha256,
      hunks: [{old_text: "sibling won", new_text: "we negotiated"}],
      session_id: session,
    },
  )
  __io_println(winner.result)                 // "applied"
}
```

### Bounded retry loop on `stale_base`

The natural pattern for hot paths: re-snapshot and re-apply against the
overlay's actual hash, up to a small cap. Past the cap, surface the
conflict to the caller rather than spinning forever.

```harn,ignore
import { edit_safe_text_patch } from "std/edit"

fn rewrite(path, hunks, session_id) {
  let attempt = 0
  const max_attempts = 3
  while attempt < max_attempts {
    const snapshot = hostlib_fs_read_text({path: path, session_id: session_id})
    const result = edit_safe_text_patch(
      {
        path: path,
        expected_hash: snapshot.sha256,
        hunks: hunks,
        session_id: session_id,
      },
    )
    if result.result != "stale_base" {
      return result
    }
    attempt = attempt + 1
  }
  return {result: "stale_base_exhausted", attempts: max_attempts}
}
```

### Preview an edit before writing

`dry_run: true` runs the matcher and returns the post-image in
`preview` without touching the file. `applied: true` plus
`dry_run: true` together mean "the matcher succeeded but we did not
write" — same convention as `edit_apply_node`.

```harn,ignore
import { edit_safe_text_patch } from "std/edit"

pipeline default() {
  const path = "src/lib.rs"
  const snapshot = hostlib_fs_read_text({path: path})
  const preview = edit_safe_text_patch(
    {
      path: path,
      expected_hash: snapshot.sha256,
      hunks: [{old_text: "return 1", new_text: "return 11"}],
      dry_run: true,
    },
  )
  __io_println(preview.applied)               // true (matcher succeeded)
  __io_println(preview.dry_run)               // true (no write happened)
  __io_println(preview.bytes_written)         // 0
  // The file on disk is unchanged. `preview.preview` carries the
  // post-image the real apply would produce — show it in a diff UI,
  // gate on user approval, then re-run with `dry_run: false`.
  if user_approves(preview.preview) {
    edit_safe_text_patch(
      {
        path: path,
        expected_hash: snapshot.sha256,
        hunks: [{old_text: "return 1", new_text: "return 11"}],
      },
    )
  }
}
```

### Migration from `edit_apply_old_new_patch`

Callers of the pure-text helpers from
[#1499](https://github.com/burin-labs/harn/issues/1499) can adopt the
new entry point incrementally:

| Before | After |
|---|---|
| Read file, call `edit_apply_old_new_patch(text, old, new)`, write result. | `edit_safe_text_patch({path, hunks: [{old_text: old, new_text: new}]})` — handles the read + write + staged-fs routing for you. |
| Race-aware bespoke retry loop. | Pass `expected_hash` from a `hostlib_fs_read_text` snapshot; the helper returns `result == "stale_base"` and `current_hash` on collision so the caller can retry. |
| Apply multiple hunks via N sequential `edit_apply_old_new_patch` calls + N writes. | Pass them as one `hunks: [...]` list — all-or-nothing commit, no half-applied intermediate state. |
| Manual logging of hunk-conflict / stale-base counters. | `result.telemetry` carries per-call counters so hosts aggregate without log scraping. |

The pure helpers (`edit_apply_old_new_patch`, `edit_splice_lines`,
`edit_check_lazy_truncation`, …) remain available for callers that
operate on in-memory strings without a path. `edit_safe_text_patch`
is the recommended entry point any time the call ends with a write
back to disk.

## `edit_fast_apply` — merge-model-assisted full-file apply

`edit_fast_apply({path, intent, ...})` is the safe fallback when an
agent has broad edit intent but not a precise AST query yet. It
separates "what should change" from "how to rewrite the bytes":

1. Read the current file through `hostlib_fs_read_text`.
2. Call `llm_call` with `model_role: "merge"` (or `params.model_role`)
   and ask for complete updated file content.
3. Reject lazy truncation and syntax errors for supported Tree-Sitter
   languages.
4. Route the proposed post-image through `edit_dry_run` for a unified
   diff preview.
5. Commit only through `edit_safe_text_patch` with the observed
   `before_sha256` as `expected_hash`.

The convenience wrapper
`fast_apply(path, edit_intent, options = nil)` lowers to the same call.

### Merge model role

The merge model is configured through the normal LLM routing layer, not
special host glue:

```toml
[model_roles.merge]
provider = "ollama"
model = "devstral-small-2"
temperature = 0.0
max_tokens = 12000
```

Per-call `llm_options` still win:

```harn,ignore
const result = edit_fast_apply({
  path: "src/lib.rs",
  intent: "Rename the local variable to make the intent clearer.",
  llm_options: {provider: "mock", model: "mock"},
})
```

Operational overrides are available without editing config:
`HARN_LLM_MERGE_PROVIDER`, `HARN_LLM_MERGE_MODEL`,
`HARN_LLM_MERGE_ROUTE_POLICY`, and the corresponding
`HARN_LLM_FAST_APPLY_*` aliases. Generic `HARN_LLM_ROLE_<ROLE>_*`
variables work for other roles.

### Parameters

| Field | Required | Notes |
|---|---|---|
| `path` | yes | File to mutate. |
| `intent` / `edit_intent` / `instruction` | yes | Natural-language edit request. |
| `model_role` | no, default `"merge"` | Role name resolved before normal provider/model routing. |
| `llm_options` | no | Extra `llm_call` options. Explicit options win over role defaults. |
| `dry_run` | no | Return `preview` and `per_file_unified_diff` without writing. |
| `validate_syntax` | no, default `true` | Parse supported languages and reject `syntax_error`; unsupported paths skip validation. |
| `session_id` | no | Routes reads and writes through the staged-fs overlay. |

### Result

Successful calls return `result == "applied"` or `"no_op"`. Rejections
include `"invalid_params"`, `"llm_invalid_output"`, `"lazy_truncation"`,
`"syntax_error"`, `"dry_run_rejected"`, and any terminal result returned
by `edit_safe_text_patch` such as `"stale_base"` or `"hunk_conflict"`.

Every result carries `telemetry` counters:

```text
{
  apply_path: "fast_apply",
  requested: 1,
  llm_calls: 1,
  success: 0|1,
  applied: 0|1,
  no_op: 0|1,
  validation_rejected: 0|1,
  rejected: 0|1,
  dry_run: 0|1,
}
```

`dry_run_bundle` contains the `edit_dry_run` response, and
`safe_text_patch` is attached on committed calls so hosts can inspect
the lower-level stale-base / hunk-conflict counters.

### Example

```harn,ignore
import { edit_fast_apply } from "std/edit"

pipeline default() {
  hostlib_enable("tools:deterministic")
  const preview = edit_fast_apply({
    path: "src/lib.rs",
    intent: "Change answer() to return 42 and keep the rest of the file untouched.",
    dry_run: true,
  })
  __io_println(preview.result)
  __io_println(preview.per_file_unified_diff[0].diff)

  if user_approves(preview.per_file_unified_diff[0].diff) {
    const applied = edit_fast_apply({
      path: "src/lib.rs",
      intent: "Change answer() to return 42 and keep the rest of the file untouched.",
    })
    __io_println(applied.telemetry.applied)
  }
}
```

## `edit_rename_symbol` — safe cross-file rename

`edit_rename_symbol({symbol_ref, new_name, scope, ...})` is the cross-file
counterpart of `edit_apply_node`. It resolves `symbol_ref` against the
typed symbol graph (#2434), walks every file in scope with tree-sitter
to collect identifier-context byte spans for `symbol_ref.name`, and
refuses to write if `new_name` already exists as an identifier in any
rewritten file (shadow check).

Backed by the `hostlib_code_index_rename_symbol` builtin (issue
[#2508](https://github.com/burin-labs/harn/issues/2508)) under the
`std/edit` umbrella epic
[#2497](https://github.com/burin-labs/harn/issues/2497).

### Parameters

| Field | Required | Notes |
|---|---|---|
| `symbol_ref` | yes | `{name, path, line?, kind?}`. `line` (1-based) and `kind` (`"Function" \| "Type" \| "Module"`) disambiguate when several symbols in the workspace share a name. |
| `new_name` | yes | Replacement identifier. Must be a valid identifier token and differ from `symbol_ref.name`. |
| `scope` | yes | `"file"` \| `"module"` \| `"workspace"`. `file` and `module` are aliases today (one Module node per file); `workspace` follows REFS edges and a textual sweep across the index. |
| `session_id` | no | Routes reads + writes through staged-fs (#1722). |
| `dry_run` | no | When `true`, the host validates end-to-end (parse, conflict, syntax) and returns the planned edits without writing. |
| `validate` | no, default `true` | Re-parse every rewritten file; reject on ERROR / MISSING nodes. |

Supported languages (first batch): Harn, Rust, TypeScript/TSX, JavaScript/JSX,
Python, Swift, Go. Other languages return `result ==
"unsupported_language"` instead of silently misrewriting.

### Result tags

| `result` | meaning |
|---|---|
| `"applied"` | rename succeeded (or, with `dry_run`, would have). `touched_files[*].edits[*]` carries byte and `(row, col)` spans for every occurrence. |
| `"conflict"` | `new_name` is already an identifier in at least one file the rename would touch. `conflicts[*]` names the shadow sites. |
| `"no_match"` | `symbol_ref` did not resolve in the typed graph. |
| `"ambiguous_symbol"` | multiple symbols share `symbol_ref.name`; pass `line` / `kind` to disambiguate. Candidate list surfaces in the response's `warnings` field. |
| `"unsupported_language"` | an in-scope file uses a grammar outside the first batch. |
| `"invalid_identifier"` | `new_name` is empty or shaped wrong for any in-scope language. |
| `"syntax_error"` | a rewritten file failed re-parse with `validate=true`. |

### Atomicity

When `session_id` is supplied AND the session is in `staged` mode,
every touched file lands in the overlay; one `hostlib_fs_commit_staged`
call flips them atomically. Without a session, the host still buffers
the full plan in memory and only writes after pre-flight validation
passes, so a clean run is all-or-nothing modulo mid-call disk failures.

```harn,ignore
import { edit_rename_symbol } from "std/edit"

const result = edit_rename_symbol({
  symbol_ref: {name: "Widget", path: "src/lib.rs", kind: "Type"},
  new_name: "Gadget",
  scope: "workspace",
})
if !result.ok && result.result == "conflict" {
  for site in result.conflicts {
    println("would shadow " + site.shadow + " at " + site.path)
  }
}
```

See the cookbook recipe [Rename a symbol across the
workspace](../cookbooks/rename-symbol.md) for the end-to-end staged
flow.

## `edit_dry_run` — preview a multi-op plan

`edit_dry_run({plan: [op, op, ...]})` runs the plan through a transient
staged-fs (#1722) overlay, renders one unified diff per touched file,
then discards the overlay — so the working tree is byte-identical
before and after the call. Plan ops share that transient session, so
the second op sees the first op's pending write and the response
collapses to one diff per file even when several ops touch it.

Backed by the `hostlib_ast_dry_run` builtin (issue
[#2510](https://github.com/burin-labs/harn/issues/2510)).

### Plan shape

Each op carries an `op` tag:

| `op` | Required fields | Notes |
|---|---|---|
| `apply_node` | `path`, `query`, `replacement` | Same shape as `edit_apply_node`. Optional: `select`, `nth`, `target_capture`, `language`, `validate`. |
| `insert_at_anchor` | `path`, `query`, `position`, `content` | `position` ∈ `before \| after \| first_child \| last_child`. Anchor must match exactly once. |
| `safe_text_patch` | `path`, `old_text`, `new_text` | Exact unique-match text replacement. |
| `rename_symbol` | `symbol_ref`, `new_name` | Workspace-level cross-file rename through the shared code-index graph. Optional: `scope` (`workspace` by default), `validate`. Hosts that register AST without code-index reject with `reason: "code_index_unavailable"`; call [`edit_rename_symbol({..., dry_run: true})`](#edit_rename_symbol--safe-cross-file-rename) for the standalone metadata-rich preview. |

### Result

```text
{
  result: "ok" | "partial" | "no_ops_applied",
  per_file_unified_diff: [
    { path, diff, lines_added, lines_removed },
    ...
  ],
  summary: {
    files_touched,
    lines_added,
    lines_removed,
    ops_applied,
    ops_rejected,
  },
  ops: [
    { op, applied, result: "applied"|"rejected"|"error", reason?, details, path?, paths?, match_count? },
    ...
  ],
}
```

The `diff` field is standard unified diff (compatible with
`git apply --check`): `---`/`+++` headers, `@@ -a,b +c,d @@` hunk
markers, three lines of leading and trailing context, and the
conventional `\ No newline at end of file` annotations when either
side lacks a trailing newline. New files use `--- /dev/null`; deleted
files use `+++ /dev/null`.

### Worked example: preview before approving

```harn,ignore
import "std/edit"

pipeline default() {
  const bundle = edit_dry_run(
    {
      plan: [
        {
          op: "apply_node",
          path: "src/lib.rs",
          query: "(function_item body: (block) @target)",
          replacement: "{ format!(\"hi {name}!\") }",
          select: "first",
        },
        {op: "safe_text_patch", path: "src/lib.rs", old_text: "fn greet", new_text: "fn greeter"},
      ],
    },
  )
  __io_println(bundle.result)                     // "ok"
  __io_println(bundle.summary.ops_applied == 2)   // true
  __io_println(bundle.summary.files_touched == 1) // true
  // `bundle.per_file_unified_diff[0].diff` is the patch you'd show
  // a reviewer or feed to `git apply` to commit the plan.
}
```

### Rejected ops keep the plan moving

A rejected op never aborts the plan. The dispatcher records the
failure on `ops[i]` and continues. `result: "partial"` flags a plan
that mixed successes and failures; `"no_ops_applied"` covers the
fully-rejected case.

```harn,ignore
import "std/edit"

pipeline default() {
  const bundle = edit_dry_run(
    {
      plan: [
        // Applied.
        {
          op: "apply_node",
          path: "src/lib.rs",
          query: "(function_item body: (block) @target)",
          replacement: "{ 42 }",
          select: "first",
        },
        // Rejected — no_match.
        {op: "safe_text_patch", path: "src/lib.rs", old_text: "missing", new_text: "x"},
      ],
    },
  )
  __io_println(bundle.result)                       // "partial"
  __io_println(bundle.ops[0].applied)               // true
  __io_println(bundle.ops[1].applied)               // false
  __io_println(bundle.ops[1].reason)                // "no_match"
}
```

## Language coverage & capability matrix

The edit primitives are **query-driven and grammar-agnostic**:
`edit_apply_node` and `edit_insert_at_anchor` work against every
registered tree-sitter grammar with no per-language code, because span
replacement and indentation inference operate on bytes and tree depth,
not language semantics. `edit_rename_symbol` is the exception — it needs
a per-language identifier-kind projection — and symbol/outline
extraction needs a per-language extractor.

| Capability | Languages |
|---|---|
| `apply_node`, `insert_at_anchor` | **all** registered grammars |
| `rename_symbol` | Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, Swift |
| `symbols` / `outline` | every general-purpose language (not the data/markup grammars) |

Registered grammars fall into two groups:

- **General-purpose** — TypeScript/TSX, JavaScript/JSX, Python, Go,
  Rust, Java, C, C++, C#, Ruby, Kotlin, PHP, Scala, Bash, Swift, Zig,
  Elixir, Lua, Haskell, R. Symbol extraction works; rename works for the
  subset above.
- **Data / markup / config** (B.7) — JSON, YAML, TOML, CSS, HTML, SQL,
  Markdown. The query-driven edit primitives work; there is no nameable
  symbol projection, so `rename_symbol` and `symbols`/`outline` return
  empty / `unsupported_language`.

> **Dockerfile** has no tree-sitter grammar compatible with the current
> tree-sitter ABI, so it is intentionally absent. Files the loop can't
> address with a grammar degrade gracefully (see below).

Query the matrix at runtime with `edit_capabilities`:

```harn
import "std/edit"

pipeline default() {
  // Whole matrix, or pass {language: "yaml"} to filter to one row.
  const caps = edit_capabilities()
  for row in caps.languages {
    __io_println("${row.language}: rename=${row.rename_symbol}")
  }
}
```

Each row is `{language, extension, apply_node, insert_at_anchor,
rename_symbol, symbols}`. A `language` filter that names no grammar
returns `result == "unsupported_language"` plus a `fallback_suggestion`.

### Graceful degradation

When a file's language has no grammar (or the requested operation isn't
available for it), the edit builtins return
`result == "unsupported_language"` with a `fallback_suggestion` field
naming the text-level path:

```text
fall back to a text-level edit (std/edit `edit_safe_text_patch`)
```

The agent loop branches on `result` rather than maintaining its own
language list: reach for an AST primitive first, and on
`unsupported_language` fall back to `edit_safe_text_patch` /
`edit_apply_old_new_patch`.

### Adding a new language (onboarding contract)

The per-language contract lives on the `Language` enum in
`harn-hostlib` (`crates/harn-hostlib/src/ast/language.rs`) — there is no
separate adapter object to wire up. Adding a language is a bounded
ticket:

1. **Grammar dep** — add the `tree-sitter-<lang>` crate (must be
   compatible with the workspace's tree-sitter version).
2. **Enum + mappings** — add a `Language` variant and its arms in
   `name`, `ts_language`, `from_name`, `from_extension`, and
   `primary_extension`.
3. **Symbol extraction** — add a match arm in `ast::symbols::extract`
   (empty for data/markup formats).
4. **Rename projection** *(optional)* — add an arm to
   `Language::rename_identifier_kinds` to enable `rename_symbol`.
5. **Fixture** — drop `tests/fixtures/ast/<name>/source.<ext>` and run
   `HARN_AST_UPDATE_GOLDEN=1 cargo test -p harn-hostlib --test harn_hostlib ast_fixtures`
   to generate the goldens.
6. **Conformance** — add an `EditCase` row to
   `tests/harn_hostlib/ast_language_coverage.rs` proving a real edit
   round-trips.

The capability matrix (`Language::edit_capabilities`) and the
`every_language_has_a_fixture` test then keep the new language honest.

## Structured refactorings

Built on the primitives above, these compound, language-aware refactorings
(issue [#2520](https://github.com/burin-labs/harn/issues/2520)) resolve
structure with tree-sitter, preview via `dry_run`, and commit atomically
through the staged-fs overlay. They share one result shape — `{ok, applied,
result, operation, language, dry_run, touched_files, unified_diff, summary,
conflicts, errors, warnings, provenance}` — where `result` is one of `applied
| no_op | conflict | unsupported | invalid_params`. Pass `dry_run: true` for a
diff-only preview and `session_id` to stage into a caller-owned session;
otherwise each call opens its own transient session and commits atomically.
All require the `tools:deterministic` capability.

| Function | Key params | Languages |
|---|---|---|
| `edit_extract_variable` | `path`, `range{start_line,start_col,end_line,end_col}`, `new_name` | rust, python, ts/tsx, js/jsx, go, swift, ruby |
| `edit_extract_function` | `path`, `range{start_line,end_line}`, `new_name`, `target_scope?`, `params_order?` | python, js/jsx, ts/tsx, ruby |
| `edit_change_signature` | `path`, `symbol`, `new_params`, `callsite_strategy?` (`strict \| default_fill \| manual`), `fill?` | rust, python, ts/tsx, js/jsx, go |
| `edit_add_parameter` | `path`, `symbol`, `param`, `index?`, `default?`, `callsite_strategy?` | rust, python, ts/tsx, js/jsx, go |
| `edit_reorder_parameters` | `path`, `symbol`, `order` (permutation of param indices) | rust, python, ts/tsx, js/jsx, go |
| `edit_change_return_type` | `path`, `symbol`, `new_type` | rust, python, ts/tsx, go |
| `edit_inline` | `path`, `symbol` (zero-param, single-`return` body) | rust, python, ts/tsx, js/jsx, go |
| `edit_move_decl` | `path`, `symbol`, `target_file`, `target_position?` (`end \| start`) | follows `hostlib_ast_symbol_extract` |

`symbol` accepts `{name}` or a bare name string. A language outside a
refactoring's matrix returns `result: "unsupported"` with a reason rather than
producing an unsafe edit. See the
[structured refactorings cookbook](../cookbooks/structured-refactorings.md) for
worked recipes.

## See also

- [`std/edit` cookbook recipe](../cookbook.md#how-to-rewrite-a-function-body-via-a-tree-sitter-query)
- [Structured refactorings cookbook](../cookbooks/structured-refactorings.md)
- [`std/code_librarian`](./code-librarian.md) — symbol graph + Cypher
  surface used by the cross-file rename API.
