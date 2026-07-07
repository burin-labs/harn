## Grammar

The grammar is expressed in EBNF. Newlines between statements are implicit separators
(the parser skips them with `skipNewlines()`). Semicolons are accepted as alternate
separators in statement-list contexts only. The `consume()` helper also skips newlines
before checking the expected token.

### Top-level

```ebnf
program            ::= top_level_list
top_level_list     ::= (NEWLINE)* [top_level (top_level_sep top_level)* [top_level_sep]] (NEWLINE)*
top_level_sep      ::= NEWLINE+ | ';' NEWLINE*
top_level          ::= import_decl
                     | attributed_decl
                     | pipeline_decl
                     | statement

attributed_decl    ::= attribute+ (pipeline_decl | fn_decl | tool_decl
                                  | skill_decl | eval_pack_decl | struct_decl
                                  | enum_decl | type_decl | interface_decl
                                  | impl_block)
attribute          ::= '@' attr_name ['(' attr_arg (',' attr_arg)* [','] ')']
attr_arg           ::= [attr_name ':'] attr_value
attr_name          ::= IDENTIFIER | 'retry'
attr_value         ::= STRING_LITERAL | RAW_STRING | INT_LITERAL
                     | FLOAT_LITERAL | 'true' | 'false' | 'nil'
                     | IDENTIFIER | '-' INT_LITERAL | '-' FLOAT_LITERAL

import_decl        ::= ['pub'] 'import' STRING_LITERAL
                     | ['pub'] 'import' '{' IDENTIFIER (',' IDENTIFIER)* '}'
                       'from' STRING_LITERAL
                     | ['pub'] 'import' IDENTIFIER ('::' IDENTIFIER)* '::'
                       '{' IDENTIFIER (',' IDENTIFIER)* '}'

pipeline_decl      ::= ['pub'] 'pipeline' IDENTIFIER '(' param_list ')'
                       ['->' type_expr]
                       ['extends' IDENTIFIER] '{' block '}'

param_list         ::= (IDENTIFIER (',' IDENTIFIER)*)?
block              ::= statement_list
statement_list     ::= (NEWLINE)* [statement (statement_sep statement)* [statement_sep]] (NEWLINE)*
statement_sep      ::= NEWLINE+ | ';' NEWLINE*

fn_decl            ::= ['pub'] 'fn' IDENTIFIER [generic_params]
                       '(' fn_param_list ')' ['->' type_expr]
                       [where_clause] '{' block '}'
where_clause       ::= 'where' where_bound (',' where_bound)*
where_bound        ::= IDENTIFIER ':' type_expr ('+' type_expr)*
type_decl          ::= ['pub'] 'type' IDENTIFIER [generic_params] '=' type_expr
enum_decl          ::= ['pub'] 'enum' IDENTIFIER [generic_params] '{'
                       (enum_variant | ',' | NEWLINE)* '}'
enum_variant       ::= IDENTIFIER ['(' fn_param_list ')']
struct_decl        ::= ['pub'] 'struct' IDENTIFIER [generic_params]
                       '{' struct_field* '}'
struct_field       ::= IDENTIFIER ['?'] ':' type_expr
impl_block         ::= 'impl' IDENTIFIER '{' (fn_decl | NEWLINE)* '}'
interface_decl     ::= 'interface' IDENTIFIER [generic_params] '{'
                       (interface_assoc_type | interface_method)* '}'
interface_assoc_type ::= 'type' IDENTIFIER ['=' type_expr]
interface_method   ::= 'fn' IDENTIFIER [generic_params]
                       '(' fn_param_list ')' ['->' type_expr]
```

#### Standard library modules

Imports starting with `std/` load embedded stdlib modules:

- `import "std/text"` — text processing (extract_paths, parse_cells,
  filter_test_cells, truncate_head_tail, detect_compile_error, has_got_want,
  format_test_errors, int_to_string, float_to_string, parse_int_or,
  parse_float_or, truncate_text, truncate_middle, single_line_or,
  prefix_lines, indent)
- `import "std/ansi"` — terminal styling helpers (ansi_enabled,
  ansi_style, ansi_color, ansi_strip, ansi_visible_len, ansi_link,
  ansi_success, ansi_warn, ansi_error)
- `import "std/table"` — deterministic plain-text and Markdown table
  rendering (render_table, render_markdown_table, render_kv_table)
- `import "std/diff"` — line diff, unified diff, colored diff, diff stat,
  and host-backed structural review helpers (diff_lines, unified_diff,
  colorize_diff, diff_summary, render_diff_stat, structural_diff)
- `import "std/edit"` — pure old/new text patch helpers
  (edit_apply_old_new_patch, edit_splice_lines, edit_changed_regions,
  edit_validate_changed_regions)
- `import "std/artifact/web"` — safe helpers for small generated HTML/CSS/JS
  artifacts (web_artifact_extract, web_artifact_text_fallback,
  web_artifact_validate, web_artifact_apply_patch)
- `import "std/ui_resource"` — MCP Apps-compatible `ui://` resource envelopes,
  text/structured fallbacks, host capability negotiation, and message
  envelopes (ui_resource, ui_tool_meta, ui_tool_meta_to_mcp,
  ui_text_fallback, ui_structured_fallback, ui_tool_result,
  ui_tool_result_validate, ui_host_capabilities, ui_host_supports_apps,
  ui_select_for_host, ui_tool_call_envelope, ui_context_update_envelope,
  ui_resource_csp_header, ui_resource_sandbox_attr)
- `import "std/tui"` — terminal presentation helpers (page,
  terminal_width, rule, clear)
- `import "std/collections"` — collection utilities (filter_nil, store_stale,
  store_refresh)
- `import "std/slug"` — memorable non-secret names and slug helpers
  (random_slug, slug_from, deterministic_slug, slug, slugify)
- `import "std/fs"` — file-system convenience helpers built on host
  primitives (ensure_parent_dir, read_json, write_json, read_yaml,
  write_yaml, read_toml, write_toml, write_lines, append_line, touch,
  find_files, relative_path, is_file, is_dir, file_size)
- `import "std/os"` — environment and host diagnostic helpers (os_info,
  env_bool, env_int, env_list, require_env, which, command_exists)
- `import "std/gha"` — GitHub Actions workflow command helpers
  (gha_escape_data, gha_annotation, gha_notice, gha_warning, gha_error,
  gha_env_block, gha_write_output, gha_write_env, gha_append_summary)
- `import "std/async"` — polling and retry helpers (wait_for, retry_until,
  retry_predicate_with_backoff, circuit_call)
- `import "std/signal"` — cooperative process interruption helpers
  (on_interrupt, off_interrupt, interrupted, with_interrupt)
- `import "std/vision"` — deterministic OCR helpers (`ocr(image, options?)`)
- `import "std/web"` — deterministic web-source and grounding helpers
  (web_fetch, web_search, verify_imports, web_grounding_tools,
  web_parse_html, robots_allowed, sitemap_urls)
- `import "std/io"` — terminal IO helpers (is_tty, read_line,
  read_password, write_stderr)
- `import "std/prompt_library"` — reusable prompt fragments, cache metadata,
  tenant-scoped k-means hotspot proposals, and review-queue records
- `import "std/agent_state"` — durable session-scoped state helpers
  (agent_state_init, agent_state_resume, agent_state_write,
  agent_state_read, agent_state_list, agent_state_delete,
  agent_state_handoff)
- `import "std/agent/progress"` — agent progress narration and task-list
  helpers (agent_progress, agent_progress_tool)
- `import "std/agent/scratchpad"` — live session-local agent working memory
  helpers (agent_scratchpad_options, agent_scratchpad_init,
  agent_scratchpad_recitation_fragment, agent_scratchpad_reorganize,
  agent_scratchpad_reorganize_if_due)
- `import "std/agent/fact"` — typed fact envelopes over durable memory
  (store_fact, recall_facts, invalidate_facts)
- `import "std/agent/probe"` — probe-first verification primitive: run a
  snippet (eval/typecheck) and record the outcome as an Observation fact
  (probe, probe_eval, probe_typecheck)
- `import "std/memory"` — append-only durable memory helpers
  (memory_store, memory_recall, memory_summarize, memory_forget)
- `import "std/trust"` — TrustGraph query and policy helpers
  (query, record, score, policy_for, verify_chain)
- `import "std/corrections"` — replay-for-teaching correction records
  (query, record)
- `import "std/postgres"` — Postgres persistence helpers (pg_pool,
  pg_connect, pg_query, pg_query_one, pg_execute, pg_transaction, pg_close,
  pg_stmt_cache_clear, pg_mock_pool, pg_mock_calls)
- `import "std/llm/handlers"` — LLM call-handler middleware
  (with_circuit_breaker)
- `import "std/llm/prompts"` — deterministic prompt builders and system
  prompt fragment helpers (system_prompt_part, system_preamble,
  system_appendix, with_system_prompt_parts, system_prelude)
- `import "std/personas/prelude"` — reusable persona orchestration helpers
  (verify_then_act, bounded_loop, cheap_classify_then_escalate,
  parallel_sweep_with_circuit_breaker, with_audit_receipt,
  with_approval_gate)
- `import "std/personas/bulletins"` — transparent profile bulletin proposals
  and host-owned decisions (bulletin_propose, bulletin_emit, bulletin_accept,
  bulletin_reject, bulletin_expire, bulletin_supersede, bulletin_decide,
  bulletin_apply_decisions, bulletin_partition, bulletin_active,
  bulletin_render_for_prompt, bulletin_dedupe)

These modules are compiled into the interpreter binary and require no
filesystem access.

### Statements

```ebnf
statement          ::= let_binding
                     | const_binding
                     | var_binding
                     | if_else
                     | for_in
                     | match_expr
                     | while_loop
                     | retry_block
                     | parallel_block
                     | parallel_each
                     | parallel_settle
                     | defer_block
                     | return_stmt
                     | throw_stmt
                     | override_decl
                     | try_catch
                     | fn_decl
                     | enum_decl
                     | struct_decl
                     | impl_block
                     | interface_decl
                     | type_decl
                     | guard_stmt
                     | require_stmt
                     | deadline_block
                     | mutex_block
                     | select_expr
                     | break_stmt
                     | continue_stmt
                     | expression_statement

let_binding        ::= 'let' binding_pattern [':' type_expr] '=' expression
const_binding      ::= 'const' IDENTIFIER [':' type_expr] '=' expression
var_binding        ::= 'var' binding_pattern [':' type_expr] '=' expression
if_else            ::= 'if' expression '{' block '}'
                       ['else' (if_else | '{' block '}')]
for_in             ::= 'for' binding_pattern 'in' expression '{' block '}'
match_expr         ::= 'match' expression '{' match_arm* '}'
match_arm          ::= expression ['if' expression] '->' '{' block '}'
while_loop         ::= 'while' expression '{' block '}'
retry_block        ::= 'retry' ['(' expression ')'] expression? '{' block '}'
parallel_block     ::= 'parallel' '(' expression ')' [parallel_options] '{' [IDENTIFIER '->'] block '}'
parallel_each      ::= 'parallel' 'each' expression [parallel_options] '{' IDENTIFIER '->' block '}'
parallel_settle    ::= 'parallel' 'settle' expression [parallel_options] '{' IDENTIFIER '->' block '}'
parallel_options   ::= 'with' '{' 'max_concurrent' ':' expression '}'
defer_block        ::= 'defer' '{' block '}'
return_stmt        ::= 'return' [expression]
throw_stmt         ::= 'throw' expression
override_decl      ::= 'override' IDENTIFIER '(' param_list ')' '{' block '}'
try_catch          ::= 'try' '{' block '}'
                       ['catch' [('(' IDENTIFIER [':' type_expr] ')') | IDENTIFIER]
                         '{' block '}']
                       ['finally' '{' block '}']
try_star_expr      ::= 'try' '*' unary_expr
guard_stmt         ::= 'guard' expression 'else' '{' block '}'
require_stmt       ::= 'require' expression [',' expression]
deadline_block     ::= 'deadline' primary '{' block '}'
mutex_block        ::= 'mutex' '{' block '}'
select_expr        ::= 'select' '{'
                         (IDENTIFIER 'from' expression '{' block '}'
                         | 'timeout' expression '{' block '}'
                         | 'default' '{' block '}')+
                       '}'
break_stmt         ::= 'break'
continue_stmt      ::= 'continue'

generic_params     ::= '<' generic_param (',' generic_param)* '>'
generic_param      ::= ['in' | 'out'] IDENTIFIER
where_clause       ::= 'where' where_bound (',' where_bound)*
where_bound        ::= IDENTIFIER ':' IDENTIFIER ('+' IDENTIFIER)*

fn_param_list      ::= (fn_param (',' fn_param)*)? [',' rest_param]
                     | rest_param
fn_param           ::= IDENTIFIER [':' type_expr] ['=' expression]
rest_param         ::= '...' IDENTIFIER
type_expr          ::= IDENTIFIER
                     | IDENTIFIER '<' type_expr (',' type_expr)* '>'
                     | '[' type_expr ']'
                     | 'fn' '(' [type_expr (',' type_expr)*] ')' '->' type_expr
                     | '{' shape_field (',' shape_field)* '}'
                     | type_expr '|' type_expr
                     | type_expr '?'
                     | STRING_LITERAL
                     | INT_LITERAL

Postfix `?` is sugar for `T | nil`: `int?` is identical to `int | nil`,
including narrowing rules. `?` binds tighter than `&` and `|`, so
`A & B?` parses as `A & (B | nil)` and `A | B?` flattens to
`A | B | nil`. Note that `??` is the nil-coalescing operator and is
lexed as a single token, so writing two `?`s in a row never stacks
optionals; redundancies in the surface form (`T? | nil`) are
deduplicated to `T | nil` during parsing. The formatter and the
`prefer-optional-shorthand` lint rule rewrite the explicit
`T | nil` form into `T?` whenever the inner type prints as a type
primary (i.e. it isn't itself a union, intersection, or `fn(...)
-> ...` type).

A rest parameter (`...name`) must be the last parameter in the list. At call
time, any arguments beyond the positional parameters are collected into a list
and bound to the rest parameter name. If no extra arguments are provided, the
rest parameter is an empty list. A type annotation on a rest parameter describes
each extra argument, and the binding inside the function has the corresponding
list type: `...nums: int` accepts only integer extras and binds `nums` as
`list<int>`.

```harn
fn sum(...nums) {
  let total = 0
  for n in nums {
    total = total + n
  }
  return total
}
sum(1, 2, 3)  // 6

fn log(level, ...parts) {
  log("[${level}] ${join(parts, " ")}")
}
log("INFO", "server", "started")  // [INFO] server started
```

```text
expression_statement ::= expression
                       | assignable '=' expression
                       | assignable ('+=' | '-=' | '*=' | '/=' | '%=') expression

assignable         ::= IDENTIFIER
                     | postfix_property
                     | postfix_subscript

binding_pattern    ::= IDENTIFIER
                     | '{' dict_pattern_fields '}'
                     | '[' list_pattern_elements ']'

dict_pattern_fields   ::= dict_pattern_field (',' dict_pattern_field)*
dict_pattern_field    ::= '...' IDENTIFIER
                        | IDENTIFIER [':' IDENTIFIER]

list_pattern_elements ::= list_pattern_element (',' list_pattern_element)*
list_pattern_element  ::= '...' IDENTIFIER
                        | IDENTIFIER
```

The `expression_statement` rule handles both bare expressions (function calls, method calls)
and assignments. An assignment is recognized when the left-hand side is an identifier
followed by `=`.

### Expressions (by precedence, lowest to highest)

```ebnf
expression         ::= pipe_expr
pipe_expr          ::= range_expr ('|>' range_expr)*
range_expr         ::= ternary_expr ['to' ternary_expr ['exclusive']]
ternary_expr       ::= logical_or ['?' ternary_expr ':' ternary_expr]
logical_or         ::= logical_and ('||' logical_and)*
logical_and        ::= equality ('&&' equality)*
equality           ::= comparison (('==' | '!=') comparison)*
comparison         ::= additive
                       (('<' | '>' | '<=' | '>=' | 'in' | 'not in') additive)*
additive           ::= nil_coal_expr (('+' | '-') nil_coal_expr)*
nil_coal_expr      ::= multiplicative ('??' multiplicative)*
multiplicative     ::= unary (('*' | '/' | '%') unary)*
unary              ::= ('!' | '-') unary | power_expr
power_expr         ::= postfix ['**' unary]
postfix            ::= primary (member_access
                               | optional_member_access
                               | subscript_access
                               | optional_subscript_access
                               | slice_access
                               | call
                               | try_unwrap)*
member_access      ::= '.' IDENTIFIER ['(' arg_list ')']
optional_member_access
                    ::= '?.' IDENTIFIER ['(' arg_list ')']
subscript_access   ::= '[' expression ']'
optional_subscript_access
                    ::= '?.[' expression ']'
                       | '?[' expression ']'     (* legacy spelling *)
slice_access       ::= '[' [expression] ':' [expression] ']'
call               ::= [type_args] '(' arg_list ')'
                       (* only when postfix base is an identifier *)
type_args          ::= '<' type_expr (',' type_expr)* '>'
try_unwrap         ::= '?'                 (* expr? on Result *)
```

### Primary expressions

```ebnf
primary            ::= STRING_LITERAL
                     | INTERPOLATED_STRING
                     | INT_LITERAL
                     | FLOAT_LITERAL
                     | DURATION_LITERAL
                     | 'true' | 'false' | 'nil'
                     | IDENTIFIER
                     | '(' expression ')'
                     | list_literal
                     | dict_or_closure
                     | parallel_block
                     | parallel_each
                     | parallel_settle
                     | retry_block
                     | if_else
                     | match_expr
                     | deadline_block
                     | 'spawn' '{' block '}'
                     | 'fn' '(' fn_param_list ')' '{' block '}'
                     | 'try' '{' block '}'

```text
list_literal       ::= '[' (list_element (',' list_element)*)? ']'
list_element       ::= '...' expression | expression

dict_or_closure    ::= '{' '}'
                     | '{' closure_param_list '->' block '}'
                     | '{' dict_entries '}'
closure_param_list ::= fn_param_list

dict_entries       ::= dict_entry (',' dict_entry)*
dict_entry         ::= (IDENTIFIER | STRING_LITERAL | '[' expression ']')
                       ':' expression
                     | '...' expression
arg_list           ::= (arg_element (',' arg_element)*)?
arg_element        ::= '...' expression | expression
```

Dict keys written as bare identifiers are converted to string literals
(e.g., `{name: "x"}` becomes `{"name": "x"}`). String-literal keys may contain
dots or other non-identifier characters; the formatter keeps identifier string
keys bare and keeps non-identifier string keys quoted (for example,
`{"a.b.c": "x", k: "y"}`). Computed keys use bracket syntax: `{[expr]: value}`.
