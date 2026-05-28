# Rename a symbol across the workspace

`edit_rename_symbol` is the safe alternative to grep + per-file text replace
for cross-file renames. It uses the typed symbol graph from
[`std/code_librarian`](../stdlib/code-librarian.md) to resolve the seed symbol,
walks every file in scope with tree-sitter to find identifier-context
occurrences (skipping comments and string literals), and refuses to write if
the new name already exists as an identifier in any rewritten file.

When the host runs with a staged-fs session (`hostlib_fs_set_mode`), every
touched file lands in the overlay; one `hostlib_fs_commit_staged` flips them
atomically. Without a session, the host still buffers the full plan in memory
and only writes after pre-flight passes, so a clean run is all-or-nothing.

Supported languages (first batch): Rust, TypeScript/TSX, JavaScript/JSX,
Python, Swift, Go.

## Recipe — rename a Rust struct across the workspace

The workspace defines `Widget` in one file and uses it from another. We
rename it to `Gadget` with a single call, then commit the staged writes.

```harn,ignore
import { edit_rename_symbol } from "std/edit"

pipeline rename_widget_to_gadget(harness: Harness, session_id: string) {
  // Workspace must already be indexed; usually the host does this on
  // startup. From a script you can run `hostlib_code_index_rebuild` first.
  hostlib_fs_set_mode({ session_id: session_id, mode: "staged" })

  let plan = edit_rename_symbol({
    symbol_ref: { name: "Widget", path: "src/lib.rs", kind: "Type" },
    new_name: "Gadget",
    scope: "workspace",
    session_id: session_id,
    dry_run: true,
  })

  if !plan.ok {
    println("rename refused: " + plan.result + " — " + (plan.details ?? ""))
    return plan
  }

  // Review the staged plan before committing. `touched_files[*].edits[*]`
  // exposes byte and (row, col) spans on both sides of the edit.
  for file in plan.touched_files {
    println("would rewrite " + file.path + " (" + str(len(file.edits)) + " edits)")
  }

  // Drop dry_run to actually stage the writes, then commit.
  let applied = edit_rename_symbol({
    symbol_ref: { name: "Widget", path: "src/lib.rs", kind: "Type" },
    new_name: "Gadget",
    scope: "workspace",
    session_id: session_id,
  })
  if !applied.ok { return applied }

  return hostlib_fs_commit_staged({ session_id: session_id })
}
```

## Conflict simulation

If `Gadget` already exists as an identifier in any file the rename would
touch, the host short-circuits with `result: "conflict"` and never writes:

```harn,ignore
import { edit_rename_symbol } from "std/edit"

pipeline rename_would_shadow(harness: Harness) {
  // `src/main.rs` defines both `Widget` and `Gadget`. Renaming Widget
  // to Gadget would create two `Gadget` definitions in the same file —
  // the host rejects before touching disk and surfaces the shadow site.
  let result = edit_rename_symbol({
    symbol_ref: { name: "Widget", path: "src/main.rs", kind: "Type" },
    new_name: "Gadget",
    scope: "workspace",
  })

  assert(result.result == "conflict")
  for site in result.conflicts {
    println("shadow at " + site.path + ":" + str(site.row + 1) + ":" + str(site.col + 1))
  }
  return result
}
```

## Result shape

`result` is one of:

| `result`                | meaning                                                       |
|-------------------------|---------------------------------------------------------------|
| `"applied"`             | rename succeeded (or, with `dry_run`, would have).            |
| `"conflict"`            | `new_name` shadows an existing identifier in a rewritten file.|
| `"no_match"`            | `symbol_ref` did not resolve in the typed graph.              |
| `"ambiguous_symbol"`    | multiple symbols share `symbol_ref.name`; pass `line`/`kind`. |
| `"unsupported_language"`| an in-scope file uses a grammar outside the first batch.      |
| `"invalid_identifier"`  | `new_name` is not a valid identifier token.                   |
| `"syntax_error"`        | a rewritten file failed re-parse with `validate=true`.        |

Each entry in `touched_files` carries the workspace-relative `path`, the
detected `language`, `before_sha256` / `after_sha256` (over the full file
body, so the caller can detect concurrent edits), and an `edits` list of
`{start_byte, end_byte, start_row, start_col, end_row, end_col, before,
after}` per occurrence.

For the surface reference see [`std/edit`](../stdlib/edit.md); for the
underlying graph see
[`std/code_librarian`](../stdlib/code-librarian.md).
