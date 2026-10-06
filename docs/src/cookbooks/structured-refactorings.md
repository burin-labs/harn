# Structured refactorings

The `std/edit` module ships compound, language-aware refactorings built on
top of the AST-precise edit primitives ([`edit_apply_node`](../stdlib/edit.md),
`edit_insert_at_anchor`, `edit_safe_text_patch`, `edit_dry_run`). They are the
"burin-like" edits an agent loop should reach for instead of regenerating files
or hand-patching call sites: each one resolves structure with tree-sitter,
previews as a unified diff, and commits atomically through the staged-fs overlay
([#1722](https://github.com/burin-labs/harn/issues/1722)).

| Function | What it does |
|---|---|
| `edit_extract_variable` | Lift a single-line expression into a named local. |
| `edit_extract_function` | Lift an expression, statement run, or closure body into a new function and call it from every copy. |
| `edit_change_signature` | Add, remove, reorder, or rename parameters and rewrite every call site. |
| `edit_add_parameter` | Insert one parameter; fill the argument at every call site. |
| `edit_reorder_parameters` | Permute parameters and every call's arguments together. |
| `edit_change_return_type` | Rewrite a function's declared return type. |
| `edit_inline` | Inline a zero-parameter, single-return function and delete it. |
| `edit_move_decl` | Move a top-level Rust, TS/JS, or Python declaration to another module and rewrite every import and qualified use. |

## Shared contract

Every refactoring returns the same shape:

```text
{
  ok, applied, result,        // result ∈ applied | no_op | conflict
                              //        | unsupported | invalid_params
  operation, language,
  dry_run,
  touched_files,              // files that actually changed
  unified_diff,               // [{path, diff, lines_added, lines_removed}]
  summary: {files_touched, lines_added, lines_removed},
  conflicts,                  // B.3-style [{code, message, path?}]
  errors, warnings, provenance
}
```

Three knobs are common to all of them:

- **`dry_run: true`** — stage the edit into a throw-away overlay and return the
  per-file `unified_diff` without writing a byte. Always preview first.
- **`session_id`** — stage into a caller-owned staged-fs session (the caller
  commits). Omit it and the refactoring opens its own transient session and
  commits atomically — all files flip together, or none do on the first
  conflict.
- **Capability matrix** — when a language lacks the structure a refactoring
  needs, the call returns `result: "unsupported"` with a reason instead of
  guessing. These all require the `tools:deterministic` capability.

## Recipe — move a function to another module

`edit_move_decl` wraps the `code_index.move_symbol` builtin, so it follows the
code index rather than the staged-diff driver above: index the workspace first,
and read the planned edits from `touched_files` instead of `unified_diff`. The
declaration moves with its attributes, decorators, and the comment block
directly above it. Every file that imports it, or names it through a module
(`jobs::priority_label`, `orders.priorityLabel`), is rewritten; the destination
gains imports for the names the declaration uses; and the source imports it
back if it still calls it. Rust, TypeScript/JavaScript, and Python are
supported.

```harn,ignore
import { edit_move_decl } from "std/edit"

pipeline default(harness: Harness) {
  const root = harness.fs.runtime_paths().asset_root
  harness.code_index.rebuild({root: root})
  const preview = edit_move_decl(
    harness.code_index,
    {
      path: "src/jobs.rs",
      symbol: "priority_label",
      to_path: "src/display.rs",
      dry_run: true,
    },
  )
  for file in preview.touched_files {
    const count = to_string(len(file.edits))
    harness.stdio.println(file.path + ": " + count + " edit(s)")
  }
}
```

A refusal (`visibility_required`, `destination_conflict`, `import_cycle`, ...)
writes nothing and lists the blocking locations in `sites`. Fix those, then
repeat the call without `dry_run`.

## Recipe — extract a function

Pull an expression, a run of statements, or a closure body out of a function.
`edit_extract_function` wraps the `code_index.extract_function` builtin: the
region's free names bound in the enclosing function become parameters, names
it assigns that are read afterwards come back as the return value, and every
same-file copy with the same tokens and bindings is replaced by the same call.
Names that resolve to the module level (other functions, imports) stay
referenced rather than parameterized.

```harn,ignore
import { edit_extract_function } from "std/edit"

pipeline default(harness: Harness) {
  // def report(base, qty):
  //     subtotal = base * qty   <- line 2
  //     audit(subtotal)         <- line 3
  //     ...
  const preview = edit_extract_function(
    harness.code_index,
    {
      path: "billing.py",
      start_line: 2,
      end_line: 3,
      new_name: "compute_subtotal",
      dry_run: true,
    },
  )
  harness.stdio.log(preview.helper)
  // def compute_subtotal(base, qty):     <- `base`/`qty` captured,
  //     subtotal = base * qty            <-  `audit` left as a free call
  //     audit(subtotal)
}
```

Rust and strict TypeScript need the typed header in `signature`, for example
`fn parse_line(line: &str) -> Option<Job>`. Without it the result is
`types_required` and carries the computed `inputs` and `outputs` to type. A
`break`, `continue`, `return`, or `?` that would leave the region is refused
with `control_flow_escapes`; extract the closure body, an expression, or a
smaller statement run instead.

## Recipe — change a signature across every caller

This is where structured edits earn their keep: add a parameter to a function
and fill the argument at all of its call sites, in every file, in one
all-or-nothing edit. The parameter refactorings run on the code index, so
rebuild it first.

```harn,ignore
import { edit_add_parameter } from "std/edit"

pipeline default(harness: Harness) {
  // fn scale(value: i64, factor: i64) -> i64 { ... }
  // called as scale(2, 3), scale(4, 5), scale(6, 7)
  harness.code_index.rebuild({root: "."})
  const result = edit_add_parameter(
    harness.code_index,
    {
      symbol_ref: {name: "scale", path: "src/lib.rs"},
      param: {name: "offset", type: "i64", call_value: "0"},
    },
  )
  if !result.ok {
    harness.stdio.log(
      "refused: " + result.result + " — " + result.details
    )
    return
  }
  const calls = to_string(result.call_sites_updated)
  harness.stdio.log("updated " + calls + " call(s)")
  // fn scale(value: i64, factor: i64, offset: i64) -> i64 { ... }
  // scale(2, 3, 0), scale(4, 5, 0), scale(6, 7, 0)
}
```

`edit_change_signature` takes the complete new parameter list. An entry that
names a current parameter keeps it (`from` renames it, and its uses in the body
follow); any other entry is new and needs `call_value` or `default`; a
parameter left out is removed. `edit_reorder_parameters` permutes parameters
and every call's arguments together. Python keyword arguments are matched by
name.

Every refusal writes nothing and lists the blocking `sites`: a removed
parameter the body still reads (`parameter_in_use`), the function passed as a
value (`value_reference`), a spread or splat call (`unsupported_call_site`),
or a trait, interface, or overridden method (`overrides_present`).

## Verify the result

Refactorings re-parse the rewritten file with tree-sitter before committing, so
a syntactically broken edit surfaces as `result: "conflict"` rather than landing
on disk. To verify behavior after an apply, run the project's own checks — for
the `scale` example above:

```harn,ignore
const check = run_command({ cmd: ["cargo", "check"], cwd: "." })
harness.stdio.log(
  check.exit_code == 0 ? "callers still compile" : check.stderr
)
```

Supported languages for the signature family and return-type rewrites: rust,
python, typescript, tsx, javascript, jsx, go (JavaScript/JSX have no return-type
slot, so `edit_change_return_type` reports `unsupported` there).
