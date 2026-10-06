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
| `edit_extract_function` | Lift a statement range into a new function; free variables become parameters. |
| `edit_change_signature` | Add, remove, reorder, or rename parameters and rewrite every call site. |
| `edit_add_parameter` | Insert one parameter; fill the argument at every call site. |
| `edit_reorder_parameters` | Permute parameters and every call's arguments together. |
| `edit_change_return_type` | Rewrite a function's declared return type. |
| `edit_inline` | Inline a zero-parameter, single-return function and delete it. |
| `edit_move_decl` | Move a named top-level declaration or Harn binding into another file. |

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

## Recipe — move a Harn setting into a config module

Use `edit_move_decl` when a top-level Harn `const` or `let` belongs in
another module. The move is structural: Harn selects the named binding from the
syntax tree, stages both file changes, and then commits them together. Local
bindings and destructuring patterns are not selected by name.

```harn,ignore
import { edit_move_decl } from "std/edit"

pipeline default(harness: Harness) {
  const preview = edit_move_decl(
    harness.fs,
    harness.random,
    harness.ast,
    {
      path: "src/github.harn",
      symbol: { name: "GITHUB_API_URL" },
      target_file: "src/config.harn",
      dry_run: true,
    },
  )

  harness.stdio.log(preview.unified_diff[0].diff)
}
```

Review the preview, then repeat the call without `dry_run` to commit the move.
See the [`std/edit` structured-refactoring reference](../stdlib/edit.md#structured-refactorings)
for parameters, result fields, and language coverage.

## Recipe — extract a function

Pull a contiguous range of statements out of a top-level function. Free
variables of the block (computed from the AST) become parameters; names that
resolve to the module level (other functions, imports) stay referenced rather
than parameterized.

```harn,ignore
import { edit_extract_function } from "std/edit"

pipeline default(harness: Harness) {
  // def report(base, qty):
  //     subtotal = base * qty   <- line 1
  //     audit(subtotal)         <- line 2
  //     ...
  const preview = edit_extract_function(
    harness.fs,
    harness.random,
    harness.ast,
    {
      path: "billing.py",
      range: { start_line: 1, end_line: 2 },
      new_name: "compute_subtotal",
      dry_run: true,
    },
  )
  harness.stdio.log(preview.unified_diff[0].diff)
  // def compute_subtotal(base, qty):     <- `base`/`qty` captured,
  //     subtotal = base * qty            <-  `audit` left as a free call
  //     audit(subtotal)
}
```

Supported: python, javascript, jsx, typescript, tsx, ruby. The generated
function is `void`; if the block produces a value used afterward, thread it back
by hand.

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
