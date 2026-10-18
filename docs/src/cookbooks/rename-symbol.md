# Rename a symbol across the workspace

`edit_rename_symbol` is the safe alternative to grep + per-file text replace
for cross-file renames. It uses the typed symbol graph from
[`std/code_librarian`](../stdlib/code-librarian.md) to resolve the seed symbol,
then asks the language's own server for the edit. For Rust that is
rust-analyzer, which follows derives, macro calls, generic parameters that
share the name, and paths into other workspace crates. Harn completes what
rust-analyzer's rename leaves behind: absolute `$crate::`/`crate::` paths
inside `macro_rules!` bodies and intra-doc links such as ``[`Span::new`]``
that resolve to the renamed item. A macro-body mention it cannot resolve
returns `declined` without writing.

TypeScript and JavaScript use typescript-language-server for TypeScript 6 and
older, or the native TypeScript 7 server (`tsc --lsp`), preferring copies in
the project's `node_modules/.bin`. Files outside the `tsconfig.json` project,
such as tests excluded by `include`, are opened so their references are renamed
too, and re-exports are renamed rather than aliased. Python uses basedpyright
or pyright. Go uses gopls with the outermost `go.work`, else the nearest
`go.mod`, as its root.

When no server is installed, or it cannot answer within its startup budget,
the rename falls back to the syntax-tree engine. That engine walks every file
in scope with tree-sitter to find identifier-context occurrences (skipping
comments and string literals) and proves that each occurrence belongs to the
selected declaration before writing. Both engines refuse a new name that
already exists as an identifier in a rewritten file.

The result's `backend` field says which engine ran:

```json
{"name": "language_server", "server": "rust-analyzer", "server_start": "warm", "server_elapsed_ms": 41}
{"name": "syntax_tree", "fallback_reason": "rust-analyzer is not installed (looked for rust-analyzer)"}
```

Pass `backend: "language_server"` to refuse instead of falling back, or
`backend: "syntax_tree"` to never start a server. `HARN_REFACTOR_BACKEND` sets
the process default. Servers start on first use, one per workspace, and stay
running for the life of the Harn process. `HARN_LSP_STARTUP_TIMEOUT_MS`
(default 30000) bounds a cold start and `HARN_LSP_REQUEST_TIMEOUT_MS` (default
15000) bounds each request; a cold start that runs out of time falls back but
keeps the server warming for the next call. `HARN_LSP_SERVER_<ID>` names a
server's executable, where `<ID>` is `RUST_ANALYZER`,
`TYPESCRIPT_LANGUAGE_SERVER`, `TSC_LSP`, `PYRIGHT` or `GOPLS`; a path that does not
exist forces the fallback.

When the host runs with a staged-fs session (`harness.fs.set_mode`), every
touched file lands in the overlay; one `harness.fs.commit_staged` flips them
atomically. Without a session, the host still buffers the full plan in memory
and only writes after pre-flight passes, so a clean run is all-or-nothing.

Supported languages (first batch): Harn, Rust, TypeScript/TSX, JavaScript/JSX,
Python, Swift, Go.

In the syntax-tree engine, cross-file import resolution covers Rust,
TypeScript/JavaScript and Python. References in another Harn, Swift or Go file
are refused when their binding cannot be proved. Same-spelled locals,
unresolved imports, receiver members, dynamic class namespaces, macros and
escaped or normalized identifiers can also produce `ambiguous_symbol`.
Refusal leaves every file unchanged.

## Recipe — rename a Rust struct across the workspace

The workspace defines `Widget` in one file and uses it from another. We
rename it to `Gadget` with a single call, then commit the staged writes.

```harn,ignore
import { edit_rename_symbol } from "std/edit"

pipeline rename_widget_to_gadget(harness: Harness, session_id: string) {
  // Workspace must already be indexed; usually the host does this on
  // startup. From a script you can run
  // `harness.code_index.rebuild` first.
  harness.fs.set_mode({ session_id: session_id, mode: "staged" })

  const plan = edit_rename_symbol(harness.code_index,
    harness.code_index,
    {
      symbol_ref: { name: "Widget", path: "src/lib.rs", kind: "Type" },
      new_name: "Gadget",
      scope: "workspace",
      session_id: session_id,
      dry_run: true,
    },
  )

  if !plan.ok {
    harness.stdio.println(
      "rename refused: " + plan.result + " — " + (plan.details ?? ""),
    )
    return plan
  }

  // Review the staged plan before committing. `touched_files[*].edits[*]`
  // exposes byte and (row, col) spans on both sides of the edit.
  for file in plan.touched_files {
    harness.stdio.println(
      "would rewrite " + file.path
        + " (" + str(len(file.edits)) + " edits)",
    )
  }

  // Drop dry_run to actually stage the writes, then commit.
  const applied = edit_rename_symbol(harness.code_index,
    harness.code_index,
    {
      symbol_ref: { name: "Widget", path: "src/lib.rs", kind: "Type" },
      new_name: "Gadget",
      scope: "workspace",
      session_id: session_id,
    },
  )
  if !applied.ok { return applied }

  return harness.fs.commit_staged({ session_id: session_id })
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
  const result = edit_rename_symbol(harness.code_index,
    harness.code_index,
    {
      symbol_ref: { name: "Widget", path: "src/main.rs", kind: "Type" },
      new_name: "Gadget",
      scope: "workspace",
    },
  )

  assert(result.result == "conflict")
  for site in result.conflicts {
    harness.stdio.println(
      "shadow at " + site.path
        + ":" + str(site.row + 1) + ":" + str(site.col + 1),
    )
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
| `"ambiguous_symbol"`    | ambiguous seed or unproven binding; inspect `details` and use a binding-aware LSP when needed. |
| `"unsupported_language"`| an in-scope file uses a grammar outside the first batch.      |
| `"invalid_identifier"`  | `new_name` is not a valid identifier token.                   |
| `"syntax_error"`        | a rewritten file failed re-parse with `validate=true`.        |
| `"declined"`            | the language server refused the rename; see `details`.        |
| `"scope_exceeded"`      | references exist outside `scope`, listed in `warnings`.       |
| `"backend_unavailable"` | `backend: "language_server"` and no server could answer.      |

Each entry in `touched_files` carries the workspace-relative `path`, the
detected `language`, `before_sha256` / `after_sha256` (over the full file
body, so the caller can detect concurrent edits), and an `edits` list of
`{start_byte, end_byte, start_row, start_col, end_row, end_col, before,
after}` per occurrence.

For the surface reference see [`std/edit`](../stdlib/edit.md); for the
underlying graph see
[`std/code_librarian`](../stdlib/code-librarian.md).
