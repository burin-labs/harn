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
| `edit_change_signature` | Replace a function's whole parameter list. |
| `edit_add_parameter` | Insert one parameter; fill the argument at every call site. |
| `edit_reorder_parameters` | Permute parameters and every call's arguments together. |
| `edit_change_return_type` | Rewrite a function's declared return type. |
| `edit_inline` | Inline a zero-parameter, single-return function and delete it. |
| `edit_move_decl` | Move a top-level declaration into another file. |

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

## Recipe — extract a function

Pull a contiguous range of statements out of a top-level function. Free
variables of the block (computed from the AST) become parameters; names that
resolve to the module level (other functions, imports) stay referenced rather
than parameterized.

```harn,ignore
import { edit_extract_function } from "std/edit"

pipeline default() {
  hostlib_enable("tools:deterministic")
  // def report(base, qty):
  //     subtotal = base * qty   <- line 1
  //     audit(subtotal)         <- line 2
  //     ...
  let preview = edit_extract_function({
    path: "billing.py",
    range: { start_line: 1, end_line: 2 },
    new_name: "compute_subtotal",
    dry_run: true,
  })
  log(preview.unified_diff[0].diff)
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
and fill the argument at all of its call sites in one atomic transaction.

```harn,ignore
import { edit_add_parameter } from "std/edit"

pipeline default() {
  hostlib_enable("tools:deterministic")
  // fn scale(value: i64, factor: i64) -> i64 { ... }
  // called as scale(2, 3), scale(4, 5), scale(6, 7)
  let result = edit_add_parameter({
    path: "src/lib.rs",
    symbol: { name: "scale" },
    param: "offset: i64",
    default: "0",            // default_fill: inserted at each call site
  })
  if !result.ok {
    log("refused: " + result.result + " — " + (result.details ?? ""))
    return
  }
  log("updated " + to_string(result.summary.files_touched) + " file(s)")
  // fn scale(value: i64, factor: i64, offset: i64) -> i64 { ... }
  // scale(2, 3, 0), scale(4, 5, 0), scale(6, 7, 0)
}
```

`callsite_strategy` controls how callers are handled:

- `default_fill` (default for `add_parameter`) inserts `default` at each call.
- `strict` refuses with `result: "conflict"` when callers exist, so you can
  rewrite the definition only when it is safe.

`edit_change_signature` takes the full `new_params` text for arbitrary changes;
`edit_reorder_parameters` permutes parameters and every call's arguments
together (refusing on an argument-count mismatch).

## Verify the result

Refactorings re-parse the rewritten file with tree-sitter before committing, so
a syntactically broken edit surfaces as `result: "conflict"` rather than landing
on disk. To verify behavior after an apply, run the project's own checks — for
the `scale` example above:

```harn,ignore
let check = run_command({ cmd: ["cargo", "check"], cwd: "." })
log(check.exit_code == 0 ? "callers still compile" : check.stderr)
```

Supported languages for the signature family and return-type rewrites: rust,
python, typescript, tsx, javascript, jsx, go (JavaScript/JSX have no return-type
slot, so `edit_change_return_type` reports `unsupported` there).
