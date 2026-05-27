# Edit stdlib

`import "std/edit"` exposes safe, structured helpers for mutating
source files. Three flavors live side by side:

- `edit_apply_node` — AST-precise replace via a Tree-Sitter query.
  The default reach when agents need to rewrite a function body,
  swap a call expression, or update a typed declaration.
- `edit_apply_old_new_patch` — collision-aware old/new text patch
  with exact / line / structural matching modes. The default reach
  when the language has no tree-sitter grammar or when the model
  reasoned in terms of literal lines.
- Validators and helpers — `edit_changed_regions`,
  `edit_validate_changed_regions`, `edit_check_lazy_truncation`,
  `edit_explain_whitespace_difference`, `edit_strip_line_number_prefixes`.

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
  let result = edit_apply_node(
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
  let all = edit_apply_node(
    {
      path: "src/lib.rs",
      query: "(function_item body: (block) @target)",
      replacement: "{ unimplemented!() }",
      select: "all",
    },
  )

  // Rewrite the second function only.
  let second = edit_apply_node(
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
  let result = edit_apply_node(
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
  let session = harness.session_id()
  let _ = hostlib_fs_set_mode({session_id: session, mode: "staged"})
  let result = edit_apply_node(
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
  let _ = hostlib_fs_commit_staged({session_id: session})
}
```

### When `edit_apply_node` is not the right tool

- The language has no tree-sitter grammar (returns
  `result == "unsupported_language"`). Fall back to
  `edit_apply_old_new_patch`.
- The change crosses files (rename-style refactors). The
  [umbrella epic](https://github.com/burin-labs/harn/issues/2497) ships
  `edit_rename_symbol` for that case, backed by the
  [#2434](https://github.com/burin-labs/harn/issues/2434) symbol graph.
- The change is sub-token (rewrite a single identifier inside a larger
  expression). The minimum granularity for `apply_node` is one
  tree-sitter node.

## See also

- [`std/edit` cookbook recipe](../cookbook.md#how-to-rewrite-a-function-body-via-a-tree-sitter-query)
- [`std/code_librarian`](./code-librarian.md) — symbol graph + Cypher
  surface used by the cross-file rename API.
