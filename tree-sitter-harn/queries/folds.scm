; Structural code folding for Harn, following the nvim-treesitter `@fold`
; convention (Neovim, Helix, and Zed all read this file).
;
; A captured node folds from its first line to its last, keeping the first
; line visible as the fold header. Two consequences shape every rule below:
;
;   1. Capture the node that owns its delimiters, so the line carrying the
;      opening brace/bracket becomes the header rather than being hidden.
;   2. Single-line nodes are not foldable, so a capture that can only ever
;      match one line is dead weight. `generic_params`, `type_arguments`,
;      `list_pattern`, and `dict_pattern` are built from `commaSep1`, which
;      admits no line breaks, and are deliberately absent for that reason.
;      `parallel_options` is absent for a related reason: the grammar forbids a
;      line break before `with`, so its fold would always start on the same line
;      as the enclosing `parallel` expression's.

; `block` is the grammar's universal brace-delimited statement body. Function,
; pipeline, and override bodies, match arms, if/else branches, `for`/`while`
; loops, `try`/`catch`/`finally`, `deadline`, `mutex`, `scope`, `retry`,
; `spawn`, `defer`, and every `select` arm all delegate to it, so this single
; capture folds them all. Capturing `block` rather than the enclosing statement
; is also what makes each branch of an if/else or try/catch/finally chain fold
; on its own instead of collapsing the whole chain at once.
(block) @fold

; Declarations and blocks that carry their braces directly instead of
; delegating to `block`.
[
  (struct_declaration)
  (enum_declaration)
  (interface_declaration)
  (impl_block)
  (tool_declaration)
  (skill_declaration)
  (eval_pack_declaration)
  (match_statement)
  (select_block)
  (cost_route_block)
  (parallel_expression)
  (parallel_each_expression)
  (parallel_settle_expression)
  (closure)
] @fold

; Multi-line collections and shape types.
[
  (list_literal)
  (dict_literal)
  (shape_type)
] @fold

; Named-import lists: `import {\n  a,\n  b,\n} from "std/fs"`.
(import_declaration) @fold

; Argument and parameter lists. These nodes cover the elements only — the
; surrounding parentheses belong to the enclosing call or declaration — so the
; fold header is the first element rather than the line carrying `(`.
[
  (argument_list)
  (parameter_list)
] @fold

; Block comments and `"""` strings. Line comments, `r"..."` raw strings, and
; ordinary `"..."` strings cannot span lines, so they are never folded.
[
  (comment)
  (multiline_string_literal)
] @fold
