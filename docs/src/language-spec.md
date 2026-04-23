<!-- Generated from spec/HARN_SPEC.md by scripts/sync_language_spec.sh -->

# Harn language specification

Version: 1.0 (derived from implementation, 2026-04-01)

Harn is a pipeline-oriented programming language for orchestrating AI agents.
It is implemented as a Rust workspace with a lexer, parser, type checker,
tree-walking VM, tree-sitter grammar, and CLI/runtime tooling. Programs consist of named pipelines
containing imperative statements, expressions, and calls to registered builtins
that perform I/O, LLM calls, and tool execution.

This file is the canonical language specification. The hosted docs page
`docs/src/language-spec.md` is generated from it by
`scripts/sync_language_spec.sh`.

## Lexical rules

### Whitespace

Spaces (`' '`), tabs (`'\t'`), and carriage returns (`'\r'`) are insignificant and skipped
between tokens. Newlines (`'\n'`) are significant tokens used as statement separators.
The parser skips newlines between statements but they are preserved in the token stream.
Semicolons (`';'`) are also accepted as optional statement separators in statement-list
contexts (top-level items, block statements, tool bodies, and `skill` fields), but they
are non-canonical input syntax. `harn fmt` normalizes them back to newline-separated form.

### Backslash line continuation

A backslash (`\`) immediately before a newline joins the current line with the next.
Both the backslash and the newline are removed from the token stream, so the two
physical lines are treated as a single logical line by the lexer.

```harn
let total = 1 + 2 \
  + 3 + 4
// equivalent to: let total = 1 + 2 + 3 + 4
```

This is useful for breaking long expressions that do not involve a binary operator
eligible for multiline continuation (see "Multiline expressions").

### Comments

```javascript
// Line comment: everything until the next newline is ignored.

/* Block comment: can span multiple lines.
   /* Nesting is supported. */
   Still inside the outer comment. */
```

Block comments track nesting depth, so `/* /* */ */` is valid. An unterminated block comment produces a lexer error.

### Keywords

The following identifiers are reserved:

| Keyword | Token |
|---|---|
| `pipeline` | `.pipeline` |
| `extends` | `.extends` |
| `override` | `.overrideKw` |
| `let` | `.letKw` |
| `var` | `.varKw` |
| `if` | `.ifKw` |
| `else` | `.elseKw` |
| `for` | `.forKw` |
| `in` | `.inKw` |
| `match` | `.matchKw` |
| `retry` | `.retry` |
| `parallel` | `.parallel` |
| `defer` | `.defer` |
| `return` | `.returnKw` |
| `import` | `.importKw` |
| `true` | `.trueKw` |
| `false` | `.falseKw` |
| `nil` | `.nilKw` |
| `try` | `.tryKw` |
| `catch` | `.catchKw` |
| `throw` | `.throwKw` |
| `finally` | `.finally` |
| `fn` | `.fnKw` |
| `spawn` | `.spawnKw` |
| `while` | `.whileKw` |
| `type` | `.typeKw` |
| `enum` | `.enum` |
| `struct` | `.struct` |
| `interface` | `.interface` |
| `pub` | `.pub` |
| `from` | `.from` |
| `to` | `.to` |
| `tool` | `.tool` |
| `exclusive` | `.exclusive` |
| `guard` | `.guard` |
| `require` | `.require` |
| `each` | `.each` |
| `settle` | `.settle` |
| `deadline` | `.deadline` |
| `yield` | `.yield` |
| `mutex` | `.mutex` |
| `break` | `.break` |
| `continue` | `.continue` |
| `select` | `.select` |
| `impl` | `.impl` |

### Identifiers

An identifier starts with a letter or underscore, followed by zero or more letters, digits, or underscores:

```javascript
identifier ::= [a-zA-Z_][a-zA-Z0-9_]*
```

### Number literals

```javascript
int_literal   ::= digit+
float_literal ::= digit+ '.' digit+
```

A number followed by `.` where the next character is not a digit is lexed as an integer
followed by the `.` operator (enabling `42.method`).

### Duration literals

A duration literal is an integer followed immediately (no whitespace) by a time-unit
suffix:

```javascript
duration_literal ::= digit+ ('ms' | 's' | 'm' | 'h' | 'd' | 'w')
```

| Suffix | Unit | Equivalent |
|---|---|---|
| `ms` | milliseconds | -- |
| `s` | seconds | 1000 ms |
| `m` | minutes | 60 s |
| `h` | hours | 60 m |
| `d` | days | 24 h |
| `w` | weeks | 7 d |

Duration literals evaluate to an integer number of milliseconds. They can be used
anywhere an expression is expected:

```harn
sleep(500ms)
deadline 30s { /* ... */ }
let one_day = 1d       // 86400000
let two_weeks = 2w     // 1209600000
```

### String literals

#### Single-line strings

```javascript
string_literal ::= '"' (char | escape | interpolation)* '"'
escape         ::= '\' ('n' | 't' | '\\' | '"' | '$')
interpolation  ::= '${' expression '}'
```

A string cannot span multiple lines. An unescaped newline inside a string is a lexer error.

If the string contains at least one `${...}` interpolation, it produces an
`interpolatedString` token containing a list of segments (literal text and expression
source strings). Otherwise it produces a plain `stringLiteral` token.

Escape sequences: `\n` (newline), `\t` (tab), `\\` (backslash), `\"` (double quote),
`\$` (dollar sign). Any other character after `\` produces a literal backslash
followed by that character.

#### Raw string literals

```javascript
raw_string_literal ::= 'r"' char* '"'
```

Raw strings use the `r"..."` prefix. No escape processing or interpolation is
performed inside a raw string -- backslashes, dollar signs, and other characters
are taken literally. Raw strings cannot span multiple lines.

Raw strings are useful for regex patterns and file paths where backslashes are
common:

```harn
let pattern = r"\d+\.\d+"
let path = r"C:\Users\alice\docs"
```

#### Multi-line strings

```javascript
multi_line_string ::= '"""' newline? content '"""'
```

Triple-quoted strings can span multiple lines. The optional newline immediately after the
opening `"""` is consumed. Common leading whitespace is stripped from all non-empty lines.
A trailing newline before the closing `"""` is removed.

Multi-line strings support `${expression}` interpolation with automatic indent
stripping. If at least one `${...}` interpolation is present, the result is an
`interpolatedString` token; otherwise it is a plain `stringLiteral` token.

```harn
let name = "world"
let doc = """
  Hello, ${name}!
  Today is ${timestamp()}.
"""
```

### Operators

#### Two-character operators (checked first)

| Operator | Token | Description |
|---|---|---|
| `==` | `.eq` | Equality |
| `!=` | `.neq` | Inequality |
| `&&` | `.and` | Logical AND |
| `\|\|` | `.or` | Logical OR |
| `\|>` | `.pipe` | Pipe |
| `??` | `.nilCoal` | Nil coalescing |
| `**` | `.pow` | Exponentiation |
| `?.` | `.questionDot` | Optional property/method chaining |
| `->` | `.arrow` | Arrow |
| `<=` | `.lte` | Less than or equal |
| `>=` | `.gte` | Greater than or equal |
| `+=` | `.plusAssign` | Compound assignment |
| `-=` | `.minusAssign` | Compound assignment |
| `*=` | `.starAssign` | Compound assignment |
| `/=` | `.slashAssign` | Compound assignment |
| `%=` | `.percentAssign` | Compound assignment |

#### Single-character operators

| Operator | Token | Description |
|---|---|---|
| `=` | `.assign` | Assignment |
| `!` | `.not` | Logical NOT |
| `.` | `.dot` | Member access |
| `+` | `.plus` | Addition / concatenation |
| `-` | `.minus` | Subtraction / negation |
| `*` | `.star` | Multiplication / string repetition |
| `/` | `.slash` | Division |
| `<` | `.lt` | Less than |
| `>` | `.gt` | Greater than |
| `%` | `.percent` | Modulo |
| `?` | `.question` | Ternary / Result propagation |
| `\|` | `.bar` | Union types |

#### Keyword operators

| Operator | Description |
|---|---|
| `in` | Membership test (lists, dicts, strings, sets) |
| `not in` | Negated membership test |

### Delimiters

| Delimiter | Token |
|---|---|
| `{` | `.lBrace` |
| `}` | `.rBrace` |
| `(` | `.lParen` |
| `)` | `.rParen` |
| `[` | `.lBracket` |
| `]` | `.rBracket` |
| `,` | `.comma` |
| `:` | `.colon` |
| `;` | `.semicolon` |
| `@` | `.at` (attribute prefix) |

### Special tokens

| Token | Description |
|---|---|
| `.newline` | Line break character |
| `.eof` | End of input |

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
                                  | struct_decl | enum_decl | type_decl
                                  | interface_decl | impl_block)
attribute          ::= '@' IDENTIFIER ['(' attr_arg (',' attr_arg)* [','] ')']
attr_arg           ::= [IDENTIFIER ':'] attr_value
attr_value         ::= STRING_LITERAL | RAW_STRING | INT_LITERAL
                     | FLOAT_LITERAL | 'true' | 'false' | 'nil'
                     | IDENTIFIER | '-' INT_LITERAL | '-' FLOAT_LITERAL

import_decl        ::= 'import' STRING_LITERAL
                     | 'import' '{' IDENTIFIER (',' IDENTIFIER)* '}'
                       'from' STRING_LITERAL

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
type_decl          ::= 'type' IDENTIFIER '=' type_expr
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
  parse_float_or)
- `import "std/collections"` — collection utilities (filter_nil, store_stale,
  store_refresh)
- `import "std/vision"` — deterministic OCR helpers (`ocr(image, options?)`)
- `import "std/prompt_library"` — reusable prompt fragments, cache metadata,
  tenant-scoped k-means hotspot proposals, and review-queue records
- `import "std/agent_state"` — durable session-scoped state helpers
  (agent_state_init, agent_state_resume, agent_state_write,
  agent_state_read, agent_state_list, agent_state_delete,
  agent_state_handoff)

These modules are compiled into the interpreter binary and require no
filesystem access.

### Statements

```ebnf
statement          ::= let_binding
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
var_binding        ::= 'var' binding_pattern [':' type_expr] '=' expression
if_else            ::= 'if' expression '{' block '}'
                       ['else' (if_else | '{' block '}')]
for_in             ::= 'for' binding_pattern 'in' expression '{' block '}'
match_expr         ::= 'match' expression '{' match_arm* '}'
match_arm          ::= expression ['if' expression] '->' '{' block '}'
while_loop         ::= 'while' expression '{' block '}'
retry_block        ::= 'retry' ['(' expression ')'] expression? '{' block '}'
parallel_block     ::= 'parallel' '(' expression ')' '{' [IDENTIFIER '->'] block '}'
parallel_each      ::= 'parallel' 'each' expression '{' IDENTIFIER '->' block '}'
parallel_settle    ::= 'parallel' 'settle' expression '{' IDENTIFIER '->' block '}'
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

generic_params     ::= '<' IDENTIFIER (',' IDENTIFIER)* '>'
where_clause       ::= 'where' IDENTIFIER ':' IDENTIFIER
                       (',' IDENTIFIER ':' IDENTIFIER)*

fn_param_list      ::= (fn_param (',' fn_param)*)? [',' rest_param]
                     | rest_param
fn_param           ::= IDENTIFIER [':' type_expr] ['=' expression]
rest_param         ::= '...' IDENTIFIER

A rest parameter (`...name`) must be the last parameter in the list. At call
time, any arguments beyond the positional parameters are collected into a list
and bound to the rest parameter name. If no extra arguments are provided, the
rest parameter is an empty list.

```harn
fn sum(...nums) {
  var total = 0
  for n in nums {
    total = total + n
  }
  return total
}
sum(1, 2, 3)  // 6

fn log(level, ...parts) {
  println("[${level}] ${join(parts, " ")}")
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
ternary_expr       ::= logical_or ['?' logical_or ':' logical_or]
logical_or         ::= logical_and ('||' logical_and)*
logical_and        ::= equality ('&&' equality)*
equality           ::= comparison (('==' | '!=') comparison)*
comparison         ::= additive
                       (('<' | '>' | '<=' | '>=' | 'in' | 'not in') additive)*
additive           ::= nil_coal_expr (('+' | '-') nil_coal_expr)*
nil_coal_expr      ::= multiplicative ('??' multiplicative)*
multiplicative     ::= power_expr (('*' | '/' | '%') power_expr)*
power_expr         ::= unary ['**' power_expr]
unary              ::= ('!' | '-') unary | postfix
postfix            ::= primary (member_access
                               | optional_member_access
                               | subscript_access
                               | slice_access
                               | call
                               | try_unwrap)*
member_access      ::= '.' IDENTIFIER ['(' arg_list ')']
optional_member_access
                    ::= '?.' IDENTIFIER ['(' arg_list ')']
subscript_access   ::= '[' expression ']'
slice_access       ::= '[' [expression] ':' [expression] ']'
call               ::= '(' arg_list ')'    (* only when postfix base is an identifier *)
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
(e.g., `{name: "x"}` becomes `{"name": "x"}`).
Computed keys use bracket syntax: `{[expr]: value}`.

## Operator precedence table

From lowest to highest binding:

| Precedence | Operators | Associativity | Description |
|---|---|---|---|
| 1 | `\|>` | Left | Pipe |
| 2 | `? :` | Right | Ternary conditional |
| 3 | `\|\|` | Left | Logical OR |
| 4 | `&&` | Left | Logical AND |
| 5 | `==` `!=` | Left | Equality |
| 6 | `<` `>` `<=` `>=` `in` `not in` | Left | Comparison / membership |
| 7 | `+` `-` | Left | Additive |
| 8 | `??` | Left | Nil coalescing |
| 9 | `*` `/` `%` | Left | Multiplicative |
| 10 | `**` | Right | Exponentiation |
| 11 | `!` `-` (unary) | Right (prefix) | Unary |
| 12 | `.` `?.` `[]` `[:]` `()` `?` | Left | Postfix |

### Multiline expressions

Binary operators `|>`, `||`, `&&`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `??`,
`+`, `*`, `/`, `%`, `**`, and the `.` / `?.` member access operators can span
multiple lines. The operator at the start of a continuation line causes the
parser to treat it as a continuation of the previous expression rather than a
new statement.

Note: `-` does not support multiline continuation because it is also a unary
negation prefix. Keyword operators `in`, `not in`, and `to` also require an
explicit backslash continuation.

```harn,ignore
let result = items
  .filter({ x -> x > 0 })
  .map({ x -> x * 2 })

let msg = "hello"
  + " "
  + "world"

let ok = check_a()
  && check_b()
  || fallback()
```

### Pipe placeholder (`_`)

When the right side of `|>` contains `_` identifiers, the expression is
automatically wrapped in a closure where `_` is replaced with the piped
value:

```harn
"hello world" |> split(_, " ")     // desugars to: |> { __pipe -> split(__pipe, " ") }
[3, 1, 2] |> _.sort()             // desugars to: |> { __pipe -> __pipe.sort() }
items |> len(_)                    // desugars to: |> { __pipe -> len(__pipe) }
```

Without `_`, the pipe passes the value as the first argument to a closure
or function.

## Scope rules

Harn uses lexical scoping with a parent-chain environment model.

### Environment

Each `HarnEnvironment` has:

- A `values` dictionary mapping names to `HarnValue`
- A `mutable` set tracking which names were declared with `var`
- An optional `parent` reference

### Variable lookup

`env.get(name)` checks the current scope's `values` first, then walks up the `parent` chain.
Returns `nil` (which becomes `.nilValue`) if not found anywhere.

### Variable definition

- `let name = value` -- defines `name` as immutable in the current scope.
- `var name = value` -- defines `name` as mutable in the current scope.
- `var name = nil` -- leaves `name` widenable until the first non-`nil`
  assignment, which fixes the slot to `T | nil`. The explicit form
  `var name: T | nil = nil` remains valid when you want to pin `T`
  up front.
- `let _ = value` / `var _ = value` -- evaluate `value` and discard it
  without introducing a variable into scope. `_` can be reused any number
  of times in the same scope.

### Variable assignment

`name = value` walks up the scope chain to find the binding. If the binding is found but was
declared with `let`, throws `HarnRuntimeError.immutableAssignment`. If not found in any scope,
throws `HarnRuntimeError.undefinedVariable`.

### Scope creation

New child scopes are created for:

- Pipeline bodies
- `for` loop bodies (loop variable is mutable)
- `while` loop iterations
- `parallel`, `parallel each`, and `parallel settle` task bodies (isolated interpreter per task)
- `try`/`catch` blocks (catch body gets its own child scope with optional error variable)
- Closure invocations (child of the *captured* environment, not the call site)
- `block` nodes

Control flow statements (`if`/`else`, `match`) execute in the current scope without creating a new child scope.

## Destructuring patterns

Destructuring binds multiple variables from a dict or list in a single
`let`, `var`, or `for`-`in` statement.

### Dict destructuring

```harn
let {name, age} = {name: "Alice", age: 30}
// name == "Alice", age == 30
```

Each field name in the pattern extracts the value for the matching key.
If the key is missing from the dict, the variable is bound to `nil`.
Use `_` as a discard binding when you want to ignore an extracted field:

```harn
let {name, debug: _} = {name: "Alice", debug: true}
// name == "Alice"; `_` is not bound
```

### Default values

Pattern fields can specify default values with `= expr` syntax. The
default expression is evaluated when the extracted value is `nil` (i.e.
when the key is missing from the dict or the index is out of bounds for
a list):

```harn
let { name = "workflow", system = "" } = { name: "custom" }
// name == "custom" (key exists), system == "" (default applied)

let [a = 10, b = 20, c = 30] = [1, 2]
// a == 1, b == 2, c == 30 (default applied)
```

Defaults can be combined with field renaming:

```harn
let { name: displayName = "Unknown" } = {}
// displayName == "Unknown"
```

Default expressions are evaluated fresh each time the pattern is matched
(they are not memoized). Rest patterns (`...rest`) do not support
default values.

### List destructuring

```harn
let [first, second, third] = [10, 20, 30]
// first == 10, second == 20, third == 30
```

Elements are bound positionally. If there are more bindings than elements
in the list, the excess bindings receive `nil` (unless a default value is
specified).
Use `_` to discard positions without creating a binding:

```harn
let [_, second, _] = [10, 20, 30]
// second == 20; `_` is not bound
```

### Field renaming

A dict pattern field can be renamed with `key: alias` syntax:

```harn
let {name: user_name} = {name: "Bob"}
// user_name == "Bob"
```

### Rest patterns

A `...rest` element collects remaining items into a new list or dict:

```harn
let [head, ...tail] = [1, 2, 3, 4]
// head == 1, tail == [2, 3, 4]

let {name, ...extras} = {name: "Carol", age: 25, role: "dev"}
// name == "Carol", extras == {age: 25, role: "dev"}
```

If there are no remaining items, the rest variable is bound to `[]` for
list patterns or `{}` for dict patterns. The rest element must appear
last in the pattern.

### For-in destructuring

Destructuring patterns work in `for`-`in` loops to unpack each element:

```harn
let entries = [{name: "X", val: 1}, {name: "Y", val: 2}]
for {name, val} in entries {
  println("${name}=${val}")
}

let pairs = [[1, 2], [3, 4]]
for [a, b] in pairs {
  println("${a}+${b}")
}
```

`_` is also a discard binding in loop patterns, so `for [_, value] in ...`
or `for (_, value) in ...` drops the ignored element instead of binding it.

### Var destructuring

`var` destructuring creates mutable bindings that can be reassigned:

```harn
var {x, y} = {x: 1, y: 2}
x = 10
y = 20
```

Discard bindings remain non-bindings under `var` as well: `var [_, value] =`
still only introduces `value`.

### Type errors

Destructuring a non-dict value with a dict pattern or a non-list value
with a list pattern produces a runtime error. For example,
`let {a} = "hello"` throws `"dict destructuring requires a dict value"`.

## Evaluation order

### Program entry

1. All top-level nodes are scanned. Pipeline declarations are registered by name.
   Import declarations are processed (loaded and evaluated).
2. The entry pipeline is selected: the pipeline named `"default"` if it exists,
   otherwise the first pipeline in the file.
3. The entry pipeline's body is executed.

If no pipeline is found in the file, all top-level statements are compiled
and executed directly as an implicit entry point (script mode). This allows
simple scripts to work without wrapping code in a pipeline block.

### Pipeline parameters

If the pipeline parameter list includes `task`, it is bound to `context.task`.
If it includes `project`, it is bound to `context.projectRoot`.
A `context` dict is always injected with keys `task`, `project_root`, and `task_type`.

### Pipeline return type

Pipelines may declare a return type with the same `-> TypeExpr` syntax
as functions:

```harn
pipeline ghost_text(task) -> {text: string, code: int} {
  return {text: "hello", code: 0}
}
```

The type checker verifies every `return <expr>` statement against the
declared type. Mismatches are reported as `return type doesn't match`
errors.

A declared return type is the typed contract that a host or bridge
(ACP, A2A) can rely on when consuming the pipeline's output.

Public pipelines (`pub pipeline`) without an explicit return type emit
the `pipeline-return-type` lint warning; explicit return types on the
Harn→ACP boundary will be required in a future release.

### Pipeline inheritance

`pipeline child(x) extends parent { ... }`:

- If the child body contains `override` declarations, the resolved body is the parent's
  body plus any non-override statements from the child.
  Override declarations are available for lookup by name.
- If the child body contains no `override` declarations, the child body entirely replaces the parent body.

### Statement execution

Statements execute sequentially. The last expression value in a block is the block's result,
though this is mostly relevant for closures and parallel bodies.

### Import resolution

`import "path"` resolves in this order:

1. If path starts with `std/`, loads embedded stdlib module (e.g. `std/text`)
2. Relative to current file's directory; auto-adds `.harn` extension
3. `.harn/packages/<path>` directories rooted at the nearest ancestor
   package root (the search walks upward and stops at a `.git` boundary).
   Harn materializes this tree from `harn.lock` before import-aware
   commands run.
4. Package manifest `[exports]` mappings under
   `.harn/packages/<package>/harn.toml`
5. Package directories with `lib.harn` entry point

Package manifests can publish stable module entry points without forcing
consumers to import the on-disk file layout directly:

```toml
[exports]
capabilities = "runtime/capabilities.harn"
providers = "runtime/providers.harn"
```

With the example above, `import "acme/capabilities"` resolves to the
declared file inside the installed `acme` package.

Selective imports: `import { name1, name2 } from "module"` imports only
the specified functions. Functions marked `pub` are exported by default;
if no `pub` functions exist, all functions are exported.

Imported pipelines are registered for later invocation.
Non-pipeline top-level statements (fn declarations, let bindings) are executed immediately.

### Static cross-module resolution

`harn check`, `harn run`, `harn bench`, and the LSP build a **module graph**
from the entry file that transitively loads every `import`-reachable
`.harn` module. The graph drives:

- **Typechecker**: when every import in a file resolves, call targets
  that are not builtins, not local declarations, not struct constructors,
  not callable variables, and not introduced by an import produce a
  `call target ... is not defined or imported` **error** (not a lint
  warning). This catches typos and stale imports before the VM loads.
- **Linter**: wildcard imports are resolved via the same graph; the
  `undefined-function` rule can now check against the actual exported
  name set of imported modules rather than silently disabling itself.
- **LSP go-to-definition**: cross-file navigation walks the graph's
  `definition_of` lookup, so any reachable symbol (through any number of
  transitive imports) can be jumped to.

Resolution conservatively **degrades to the pre-v0.7.12 behavior** when
any import in the file is unresolved (missing file, parse error,
non-existent package directory), so a single broken import does not
avalanche into a sea of false-positive undefined-name errors. The
unresolved import itself still surfaces via the runtime loader.

## Runtime values

| Type | Syntax | Description |
|---|---|---|
| `string` | `"text"` | UTF-8 string |
| `bytes` | builtin-produced | Immutable byte buffer |
| `int` | `42` | Platform-width integer |
| `float` | `3.14` | Double-precision float |
| `bool` | `true` / `false` | Boolean |
| `nil` | `nil` | Null value |
| `list` | `[1, 2, 3]` | Ordered collection |
| `dict` | `{key: value}` | String-keyed map |
| `set` | `set(1, 2, 3)` | Unordered collection of unique values |
| `closure` | `{ x -> x + 1 }` | First-class function with captured environment |
| `enum` | `Color.Red` | Enum variant, optionally with associated data |
| `struct` | `Point({x: 3, y: 4})` | Struct instance with named fields |
| `taskHandle` | (from `spawn`) | Opaque handle to an async task |
| `Iter<T>` | `x.iter()` / `iter(x)` | Lazy, single-pass, fused iterator. See [Iterator protocol](#iterator-protocol) |
| `Pair<K, V>` | `pair(k, v)` | Two-element value; access via `.first` / `.second` |

### Truthiness

| Value | Truthy? |
|---|---|
| `bool(false)` | No |
| `nil` | No |
| `int(0)` | No |
| `float(0)` | No |
| `string("")` | No |
| `bytes(b"")` | No |
| `list([])` | No |
| `dict([:])` | No |
| `set()` (empty) | No |
| Everything else | Yes |

### Equality

Values are equal if they have the same type and same contents, with these exceptions:

- `int` and `float` are compared by converting `int` to `float`
- Two closures are never equal
- Two task handles are equal if their IDs match

### Comparison

Only `int`, `float`, and `string` support ordering (`<`, `>`, `<=`, `>=`).
Comparison between other types returns 0 (equal).

## Binary operator semantics

### Arithmetic (`+`, `-`, `*`, `/`)

| Left | Right | `+` | `-` | `*` | `/` |
|---|---|---|---|---|---|
| int | int | int | int | int | int (truncating) |
| float | float | float | float | float | float |
| int | float | float | float | float | float |
| float | int | float | float | float | float |
| string | string | string (concatenation) | **TypeError** | **TypeError** | **TypeError** |
| string | int | **TypeError** | **TypeError** | string (repetition) | **TypeError** |
| int | string | **TypeError** | **TypeError** | string (repetition) | **TypeError** |
| list | list | list (concatenation) | **TypeError** | **TypeError** | **TypeError** |
| dict | dict | dict (merge, right wins) | **TypeError** | **TypeError** | **TypeError** |
| other | other | **TypeError** | **TypeError** | **TypeError** | **TypeError** |

Division by zero returns `nil`.
`string * int` repeats the string; negative or zero counts return `""`.

Type mismatches that are not listed as valid combinations above produce a
`TypeError` at runtime. The type checker reports these as compile-time errors
when operand types are statically known. Use `to_string()` or string
interpolation (`"${expr}"`) for explicit type conversion.

### Modulo (`%`)

`%` is numeric-only. `int % int` returns `int`; any case involving a `float`
returns `float`. Modulo by zero follows the same runtime error path as
division by zero.

### Exponentiation (`**`)

`**` is numeric-only and right-associative, so `2 ** 3 ** 2` evaluates as
`2 ** (3 ** 2)`.

- `int ** int` returns `int` for non-negative exponents that fit in `u32`,
  using wrapping integer exponentiation.
- Negative or very large integer exponents promote to `float`.
- Any case involving a `float` returns `float`.
- Non-numeric operands raise `TypeError`.

### Logical (`&&`, `||`)

Short-circuit evaluation:

- `&&`: if left is falsy, returns `false` without evaluating right.
- `||`: if left is truthy, returns `true` without evaluating right.

### Nil coalescing (`??`)

Short-circuit: if left is not `nil`, returns left without evaluating right.
`??` binds tighter than additive/comparison/logical operators but looser than
multiplicative operators, so `xs?.count ?? 0 > 0` parses as
`(xs?.count ?? 0) > 0`.

### Pipe (`|>`)

`a |> f` evaluates `a`, then:

1. If `f` evaluates to a closure, invokes it with `a` as the single argument.
2. If `f` is an identifier resolving to a builtin, calls the builtin with `[a]`.
3. If `f` is an identifier resolving to a closure variable, invokes it with `a`.
4. Otherwise returns `nil`.

### Ternary (`? :`)

`condition ? trueExpr : falseExpr` evaluates `condition`, then evaluates and returns
either `trueExpr` (if truthy) or `falseExpr`.

### Ranges (`to`, `to … exclusive`)

`a to b` evaluates `a` and `b` (both must be integers) and produces a list of
consecutive integers. The form is **inclusive** by default — `1 to 5` is
`[1, 2, 3, 4, 5]` — because that matches how the expression reads aloud.

Add the trailing modifier `exclusive` to get the half-open form:
`1 to 5 exclusive` is `[1, 2, 3, 4]`.

| Expression           | Value               | Shape      |
|----------------------|---------------------|------------|
| `1 to 5`             | `[1, 2, 3, 4, 5]`   | `[a, b]`   |
| `1 to 5 exclusive`   | `[1, 2, 3, 4]`      | `[a, b)`   |
| `0 to 3`             | `[0, 1, 2, 3]`      | `[a, b]`   |
| `0 to 3 exclusive`   | `[0, 1, 2]`         | `[a, b)`   |

If `b < a`, the result is the empty list. The `range(n)` / `range(a, b)` stdlib
builtins always produce the half-open form, for Python-compatible indexing.

## Control flow

### if/else

```harn
if condition {
  // then
} else if other {
  // else-if
} else {
  // else
}
```

`else if` chains are parsed as a nested `ifElse` node in the else branch.

### for/in

```harn
for item in iterable {
  // body
}
```

If `iterable` is a list, iterates over elements. If `iterable` is a dict, iterates over
entries sorted by key, where each entry is `{key: "...", value: ...}`.
The loop variable is mutable within the loop body.

### while

```harn
while condition {
  // body
}
```

Maximum 10,000 iterations (safety limit). Condition is re-evaluated each iteration.

### match

```harn
match value {
  pattern1 -> { body1 }
  pattern2 if condition -> { body2 }
}
```

Patterns are expressions. Each pattern is evaluated and compared to the match value
using `valuesEqual`. An arm may include an `if` guard after the pattern; when
present, the arm only matches if the pattern matches **and** the guard expression
evaluates to a truthy value. The first matching arm executes.

If no arm matches, a runtime error is thrown (`no matching arm in match expression`).
This makes non-exhaustive matches a hard failure rather than a silent `nil`.

```harn
let x = 5
match x {
  1 -> { "one" }
  n if n > 3 -> { "big: ${n}" }
  _ -> { "other" }
}
// -> "big: 5"
```

### retry

```harn
retry 3 {
  // body that may throw
}
```

Executes the body up to N times. If the body succeeds (no error), returns immediately.
If the body throws, catches the error and retries. `return` statements inside retry
propagate out (are not retried). After all attempts are exhausted, returns `nil`
(does not re-throw the last error).

## Concurrency

### Runtime context

`runtime_context()` returns the current logical runtime context as a dict.
`task_current()` is an alias. This is Harn's stable task/thread identity
surface; OS thread IDs are not exposed as the primary abstraction.

The stable fields are always present, with unavailable values represented as
`nil`: `task_id`, `parent_task_id`, `root_task_id`, `task_name`,
`task_group_id`, `scope_id`, `workflow_id`, `run_id`, `stage_id`, `worker_id`,
`agent_session_id`, `parent_agent_session_id`, `root_agent_session_id`,
`agent_name`, `trigger_id`, `trigger_event_id`, `binding_key`, `tenant_id`,
`provider`, `trace_id`, `span_id`, `scheduler_key`, `runner`,
`capacity_class`, `context_values`, `cancelled`, and `debug`.

`spawn`, `parallel`, `parallel each`, and `parallel settle` create child
logical tasks. A child task receives a deterministic `task_id`, its
`parent_task_id` is the creating task, and its `root_task_id` is inherited from
the root task. `parallel` siblings share a `task_group_id`.

Task-local values are managed with `runtime_context_values()`,
`runtime_context_get(key, default?)`, `runtime_context_set(key, value)`, and
`runtime_context_clear(key)`. Children inherit a snapshot of the parent's
task-local values. Later child writes do not mutate the parent, and later parent
writes do not affect already-created children.

### parallel

```harn
parallel(count) { i ->
  // body executed count times concurrently
}
```

Creates `count` concurrent tasks. Each task gets an isolated interpreter with a child
environment. The optional variable `i` is bound to the task index (0-based).
Returns a list of results in index order.

### parallel each

```harn
parallel each list { item ->
  // body for each item
}
```

Maps over a list concurrently. Each task gets an isolated interpreter.
The variable is bound to the current list element.
Returns a list of results in the original order.

### parallel settle

```harn
parallel settle list { item ->
  // body for each item
}
```

Like `parallel each`, but never throws. Instead, it collects both
successes and failures into a result object with fields:

| Field | Type | Description |
|---|---|---|
| `results` | list | List of `Result` values (one per item), in order |
| `succeeded` | int | Number of `Ok` results |
| `failed` | int | Number of `Err` results |

### defer

```harn
defer {
  // cleanup body
}
```

Registers a block to run when the enclosing scope exits, whether by
normal return or by a thrown error. Multiple `defer` blocks in the same
scope execute in LIFO (last-registered, first-executed) order, similar
to Go's `defer`. The deferred block runs in the scope where it was
declared.

```harn
fn open(path) { path }
fn close(f) { log("closing ${f}") }
let f = open("data.txt")
defer { close(f) }
// ... use f ...
// close(f) runs automatically on scope exit
```

### spawn/await/cancel

```harn
let handle = spawn {
  // async body
}
let result = await(handle)
cancel(handle)
```

`spawn` launches an async task and returns a `taskHandle`.
`await` (a built-in interpreter function, not a keyword) blocks until the task completes
and returns its result. `cancel` cancels the task.

### Synchronization

Harn synchronization primitives are workflow-level parking primitives, not
low-level OS locks or spinlocks. The initial scope is process-local: spawned and
parallel child VMs inherit the same synchronization runtime, while durable
EventLog-backed variants are reserved for explicit future primitives.

`mutex { ... }` acquires the fair process-local default mutex key
`"__default__"` for the lexical block. The permit is released when the block's
scope exits, including `throw`, `return`, `break`, `continue`, and caught
runtime errors.

Named primitives return a permit value or `nil` on timeout:

```harn
let lock = sync_mutex_acquire("state:customer-42", 250ms)
let slot = sync_semaphore_acquire("connector:notion", 4, 1, 2s)
let gate = sync_gate_acquire("workflow-runner", 8, 5s)
```

- `sync_mutex_acquire(key?, timeout?)` acquires one permit from a named FIFO
  mutex. Omitting `key` uses `"__default__"`.
- `sync_semaphore_acquire(key, capacity, permits?, timeout?)` acquires a
  weighted permit from a named FIFO semaphore.
- `sync_gate_acquire(key, limit, timeout?)` acquires one fair-admission slot
  from a named FIFO gate.
- `sync_release(permit)` releases a named permit and returns `true` only for
  the first release.
- `sync_metrics(kind?, key?)` returns observability counters for matching
  primitives. A concrete `(kind, key)` returns a dict; partial or empty
  filters return a list.

Metrics include `acquisition_count`, `timeout_count`, `cancellation_count`,
`release_count`, `current_held`, `current_queue_depth`, `max_queue_depth`,
`total_wait_ms`, and `total_held_ms`.

Acquisition is cancellable: a graceful task cancellation while waiting throws
`kind:cancelled:VM cancelled by host`. Timeouts are deterministic and return
`nil` instead of throwing so authors can choose the fallback policy.

### Scoped shared state

Child tasks created by `spawn`, `parallel`, `parallel each`, and
`parallel settle` execute in isolated interpreter instances. Normal values are
copied into the child at creation time. Later assignment in one task does not
mutate another task's binding. Explicit handles are the shared-data boundary:
channels, atomics, synchronization permits/runtimes, shared cells/maps, and
mailboxes are shared by every task that receives or resolves the handle.

`agent_loop` does not share mutable transcript internals with tasks. A named
`session_id` shares durable transcript history through the agent session store.
`spawn_agent` workers have independent worker/session lineage; use explicit
mailboxes, shared state handles, `agent_state_*`, or host storage for data
exchange outside the transcript.

```harn
let budget = shared_cell({scope: "task_group", key: "tokens", initial: 0})

parallel 10 { i ->
  var updated = false
  while !updated {
    let snap = shared_snapshot(budget)
    updated = shared_cas(budget, snap, snap.value + 1)
  }
}
```

Process-local scopes are explicit:

| Scope | Meaning |
|---|---|
| `task` | Current logical task |
| `task_group` | Current parallel sibling group, or root task outside a group |
| `workflow_run` | Current workflow run when available |
| `agent_session` | Current agent session when available |
| `tenant` | Current tenant id, or `tenant_id` from options |
| `process` | Current VM process |

Durable and external state are not implicit. Use `store_*` or
`agent_state_*` for file/EventLog-backed state and host/connector APIs for
external stores.

Cells:

- `shared_cell(key_or_options, initial?)` opens a scoped cell. Options support
  `scope`, `key`, `initial`, and `tenant_id`.
- `shared_get(cell)` reads the value.
- `shared_snapshot(cell)` returns `{value, version}` for versioned CAS.
- `shared_set(cell, value)` writes with last-write-wins behavior and returns
  the previous value.
- `shared_cas(cell, expected_or_snapshot, value)` writes only when the current
  value matches the expected value, and when a snapshot is supplied, the
  version still matches. It returns `true` on success and `false` on conflict.

Maps:

- `shared_map(key_or_options, initial?)` opens a scoped map.
- `shared_map_get(map, key, default?)`, `shared_map_set(map, key, value)`,
  `shared_map_delete(map, key)`, and `shared_map_entries(map)` are the
  last-write-wins map operations.
- `shared_map_snapshot(map, key)` and
  `shared_map_cas(map, key, expected_or_snapshot, value)` provide
  versioned conflict checks.

`shared_metrics(handle)` reports `read_count`, `write_count`,
`cas_success_count`, `cas_failure_count`, `stale_read_count`, and `version`
for cells and maps.

Use named synchronization around multi-step updates:

```harn
let memo = shared_map({scope: "workflow_run", key: "memo"})
let lock = sync_mutex_acquire("memo:customer-42", 250ms)
guard lock != nil else { throw "state lock timeout" }
try {
  shared_map_set(memo, "customer-42", "summary")
} finally {
  sync_release(lock)
}
```

### Actor mailboxes

Mailboxes are scoped, named inboxes for actor-style communication between
tasks and long-lived workers. They provide targeted messages without using
transcript mutation as the transport.

```harn
let inbox = mailbox_open({scope: "task_group", name: "reviewer", capacity: 32})
spawn {
  mailbox_send("reviewer", {kind: "work", path: "src/main.rs"})
}
let msg = mailbox_receive(inbox)
```

- `mailbox_open(name_or_options, capacity?)` opens or creates an inbox.
- `mailbox_lookup(name_or_handle)` returns a handle or `nil`.
- `mailbox_send(target, value)` returns `false` when the mailbox is absent or
  closed.
- `mailbox_receive(target)` blocks until a message arrives, the mailbox closes,
  or the task is cancelled.
- `mailbox_try_receive(target)` is non-blocking.
- `mailbox_close(target)` closes the inbox to new messages.
- `mailbox_metrics(target)` reports `depth`, `capacity`, `sent_count`,
  `received_count`, `failed_send_count`, and `closed`.

### Channels

Channels provide typed message-passing between concurrent tasks.

```harn
let ch = channel("name", 10)   // buffered channel with capacity 10
send(ch, "hello")               // send a value
let msg = receive(ch)           // blocking receive
```

#### Channel iteration

A `for`-`in` loop over a channel asynchronously receives values until the
channel is closed and drained:

```harn
let ch = channel("stream", 10)
spawn {
  send(ch, "a")
  send(ch, "b")
  close_channel(ch)
}
for item in ch {
  println(item)    // prints "a", then "b"
}
// loop exits after channel is closed and all items are consumed
```

When the channel is closed, remaining buffered items are still delivered.
The loop exits once all items have been consumed.

#### close_channel(ch)

Closes a channel. After closing, `send` returns `false` and no new values
are accepted. Buffered items can still be received.

#### try_receive(ch)

Non-blocking receive. Returns the next value from the channel, or `nil` if
the channel is empty (regardless of whether it is closed).

### select

Multiplexes across multiple channels, executing the body of whichever
channel receives a value first:

```harn
select {
  msg from ch1 {
    log("ch1: ${msg}")
  }
  msg from ch2 {
    log("ch2: ${msg}")
  }
}
```

Each case binds the received value to a variable (`msg`) and executes the
corresponding body. Only one case fires per select.

#### timeout case

```harn
fn handle(msg) { log(msg) }
let ch1 = channel(1)
select {
  msg from ch1 { handle(msg) }
  timeout 5s {
    log("timed out")
  }
}
```

If no channel produces a value within the duration, the timeout body runs.

#### default case (non-blocking)

```harn
fn handle(msg) { log(msg) }
let ch1 = channel(1)
select {
  msg from ch1 { handle(msg) }
  default {
    log("nothing ready")
  }
}
```

If no channel has a value immediately available, the default body runs
without blocking. `timeout` and `default` are mutually exclusive.

#### select() builtin

The statement form desugars to the `select(ch1, ch2, ...)` async builtin,
which returns `{index, value, channel}`. The builtin can be called directly
for dynamic channel lists.

## Error model

### throw

```harn
throw expression
```

Evaluates the expression and throws it as `HarnRuntimeError.thrownError(value)`.
Any value can be thrown (strings, dicts, etc.).

### try/catch/finally

```harn
try {
  // body
} catch (e) {
  // handler
} finally {
  // cleanup — always runs
}
```

If the body throws:

- A `thrownError(value)`: `e` is bound to the thrown value directly.
- Any other runtime error: `e` is bound to the error's `localizedDescription` string.

`return` inside a `try` block propagates out of the enclosing pipeline (is not caught).

The error variable `(e)` is optional: `catch { ... }` is valid without it.

`try { ... } catch (e) { ... }` is also usable as an expression: the value of
the whole form is the tail value of the try body when it succeeds, and the tail
value of the catch handler when an error is caught. This means the natural
`let v = try { risky() } catch (e) { fallback }` binding is supported directly,
without needing to restructure through `Result` helpers. When a typed catch
(`catch (e: AppError) { ... }`) does not match the thrown error's type, the
throw propagates past the expression unchanged — the surrounding `let` never
binds. See the [Try-expression](#try-expression) section below for the
`Result`-wrapping behavior when `catch` is omitted.

### try* (rethrow-into-catch)

`try* EXPR` is a prefix operator that evaluates `EXPR` and rethrows any
thrown error so an enclosing `try { ... } catch (e) { ... }` can handle
it, instead of forcing the caller to manually convert thrown errors
into a `Result` and then `guard is_ok / unwrap`. The lowered form is:

```harn,ignore
{ let _r = try { EXPR }
  guard is_ok(_r) else { throw unwrap_err(_r) }
  unwrap(_r) }
```

On success `try* EXPR` evaluates to `EXPR`'s value with no `Result`
wrapping. The rethrow runs every `finally` block between the rethrow
site and the innermost catch handler exactly once, matching the
`finally` exactly-once guarantee for plain `throw`.

```harn,ignore
fn fetch(prompt) {
  // Without try*: try { llm_call(prompt) } / guard is_ok / unwrap
  let response = try* llm_call(prompt)
  return parse(response)
}

let outcome = try {
  let result = fetch(prompt)
  Ok(result)
} catch (e: ApiError) {
  Err(e.code)
}
```

`try*` requires an enclosing function (`fn`, `tool`, or `pipeline`) so
the rethrow has a body to live in — using it at module top level is a
compile error. The operand is parsed at unary-prefix precedence, so
`try* foo.bar(1)` parses as `try* (foo.bar(1))` and `try* a + b` parses
as `(try* a) + b`. Use parentheses to combine `try*` with binary
operators on its operand. `try*` is distinct from the postfix `?`
operator: `?` early-returns `Result.Err(...)` from a `Result`-returning
function, while `try*` rethrows a thrown value into an enclosing catch.

### finally

The `finally` block is optional and runs regardless of whether the try body
succeeds, throws, or the catch body re-throws. Supported forms:

```harn,ignore
try { ... } catch e { ... } finally { ... }
try { ... } finally { ... }
try { ... } catch e { ... }
```

`return`, `break`, and `continue` inside a try body with a finally block will
execute the finally block before the control flow transfer completes.

The finally block's return value is discarded — the overall expression value
comes from the try or catch body.

## Functions and closures

### fn declarations

```harn
fn name(param1, param2) {
  return param1 + param2
}
```

Declares a named function. Equivalent to `let name = { param1, param2 -> ... }`.
The function captures the lexical scope at definition time.

### Default parameters

Parameters may have default values using `= expr`. Required parameters must
come before optional (defaulted) parameters. Defaults are evaluated fresh at
each call site (not memoized at definition time). Any expression is valid as
a default — not just literals.

```harn
fn greet(name, greeting = "hello") {
  log("${greeting}, ${name}!")
}
greet("world")           // "hello, world!"
greet("world", "hi")     // "hi, world!"

fn config(host = "localhost", port = 8080, debug = false) {
  // all params optional
}

let add = { x, y = 10 -> x + y }  // closures support defaults too
```

Explicit `nil` counts as a provided argument (does NOT trigger the default).
Arguments are positional — fill left to right, only trailing defaults can
be omitted.

### tool declarations

```harn
tool read_file(path: string, encoding: string) -> string {
  description "Read a file from the filesystem"
  read_file(path)
}

tool search(query: string, file_glob: string = "*.py") -> string {
  description "Search files matching an optional glob"
  "..."
}
```

Declares a named tool and registers it with a tool registry. The body is
compiled as a closure and attached as the tool's handler. An optional
`description` metadata string may appear as the first statement in the body.

Annotated tool parameter and return types are lowered into the same schema
model used by runtime validation and structured LLM I/O. Primitive types map to
their JSON Schema equivalents, while nested shapes, `list<T>`,
`dict<string, V>`, and unions produce nested schema objects. Parameters with
default values are emitted as optional schema fields (`required: false`) and
include their `default` value in the generated tool registry entry.

The result of a `tool` declaration is a tool registry dict (the return
value of `tool_define`). Multiple `tool` declarations accumulate into
separate registries; use `tool_registry()` and `tool_define(...)` for
multi-tool registries.

Like `fn`, `tool` may be prefixed with `pub`.

#### Deferred tool loading (`defer_loading`)

A tool registered through `tool_define` may set `defer_loading: true`
in its config dict. Deferred tools keep their schema out of the model's
context on each LLM call until a tool-search call surfaces them.

```harn
fn admin(token) { log(token) }

let registry = tool_registry()
registry = tool_define(registry, "rare_admin_action", "...", {
  parameters: {token: {type: "string"}},
  defer_loading: true,
  handler: { args -> admin(args.token) },
})
```

`defer_loading` is validated as a bool at registration time — typos like
`defer_loading: "yes"` raise at `tool_define` rather than silently
falling back to eager loading.

Deferred tools are only materialised on the wire when the call opts
into `tool_search` (see the `llm_call` option of the same name and
`docs/src/llm-and-agents.md`). Harn supports two native backends plus a
provider-agnostic client fallback:

- **Anthropic Claude Opus/Sonnet 4.0+ and Haiku 4.5+** — Harn emits
  `defer_loading: true` on each deferred tool and prepends the
  `tool_search_tool_{bm25,regex}_20251119` meta-tool. Anthropic keeps
  deferred schemas in the API prefix (prompt caching stays warm) but
  out of the model's context.
- **OpenAI GPT 5.4+ (Responses API)** — Harn emits
  `defer_loading: true` on each deferred tool and prepends
  `{"type": "tool_search", "mode": "hosted"}` to the tools array.
  OpenRouter, Together, Groq, DeepSeek, Fireworks, HuggingFace, and
  local vLLM inherit the capability when their routed model matches
  `gpt-5.4+`.
- **Everyone else (and any of the above on older models)** — Harn
  injects a synthetic `__harn_tool_search` tool and runs the configured
  strategy (BM25, regex, semantic, or host-delegated) in-VM, promoting
  matching deferred tools into the next turn's schema list.

Tool entries may also set `namespace: "<label>"` to group deferred tools
for the OpenAI meta-tool's `namespaces` field. The field is a harmless
passthrough on Anthropic — ignored by the API, preserved in replay.

`mode: "native"` refuses to silently downgrade and errors when the
active (provider, model) pair is not natively capable; `mode: "client"`
forces the fallback everywhere; `mode: "auto"` (default) picks native
when available.

The per-provider / per-model capability table that gates native
`tool_search`, `defer_loading`, prompt caching, and extended thinking
is a shipped TOML matrix overridable per-project via
`[[capabilities.provider.<name>]]` in `harn.toml`. Scripts query the
effective matrix at runtime with:

```harn
let caps = provider_capabilities("anthropic", "claude-opus-4-7")
// {
//   provider, model, native_tools, defer_loading,
//   tool_search: [string], max_tools: int | nil,
//   prompt_caching, thinking,
// }
```

The `provider_capabilities_install(toml_src)` and
`provider_capabilities_clear()` builtins let scripts install and
revert overrides in-process for cases where editing the manifest is
awkward (runtime proxy detection, conformance test setup). See
`docs/src/llm-and-agents.md#capability-matrix--harntoml-overrides`
for the rule schema.

### skill declarations

```harn
pub skill deploy {
  description "Deploy the application to production"
  when_to_use "User says deploy/ship/release"
  invocation "explicit"
  paths ["infra/**", "Dockerfile"]
  allowed_tools ["bash", "git"]
  model "claude-opus-4-7"
  effort "high"
  prompt "Follow the deployment runbook."

  on_activate fn() {
    log("deploy skill activated")
  }
  on_deactivate fn() {
    log("deploy skill deactivated")
  }
}
```

Declares a named skill and registers it with a skill registry. A skill
bundles metadata, tool references, MCP server lists, system-prompt
fragments, and auto-activation rules into a typed unit that hosts can
enumerate, select, and invoke.

Body entries are `<field_name> <expression>` pairs separated by
newlines or semicolons. The field name is an ordinary identifier (no keyword is
reserved), and the value is any expression — string literal, list
literal, identifier reference, dict literal, or fn-literal (for
lifecycle hooks). The compiler lowers the decl to:

```harn,ignore
skill_define(skill_registry(), NAME, { field: value, ... })
```

and binds the resulting registry dict to `NAME`, parallel to how
`tool NAME { ... }` works.

`skill_define` performs light value-shape validation on known keys:
`description`, `when_to_use`, `prompt`, `invocation`, `model`, `effort`
must be strings; `paths`, `allowed_tools`, `mcp` must be lists.
Mistyped values fail at registration rather than at use. Unknown keys
pass through unchanged to support integrator metadata.

Like `fn` and `tool`, `skill` may be prefixed with `pub` to export it
from the module. The registry-dict value is bound as a module-level
variable.

#### Skill registry operations

```harn
let reg = skill_registry()
let reg = skill_define(reg, "review", {
  description: "Code review",
  invocation: "auto",
  paths: ["src/**"],
})
skill_count(reg)           // int
skill_find(reg, "review")  // dict | nil
skill_list(reg)            // list (closure hooks stripped)
skill_select(reg, ["review"])
skill_remove(reg, "review")
skill_describe(reg)        // formatted string
```

`skill_list` strips closure-valued fields (lifecycle hooks) so its
output is safe to serialize. `skill_find` returns the full entry
including closures.

#### `@acp_skill` attribute

Functions can be promoted into skills via the `@acp_skill` attribute:

```harn,ignore
@acp_skill(name: "deploy", when_to_use: "User says deploy", invocation: "explicit")
pub fn deploy_run() { ... }
```

Attribute arguments populate the skill's metadata dict, and the
annotated function is registered as the skill's `on_activate`
lifecycle hook. Like `@acp_tool`, `@acp_skill` only applies to
function declarations; using it on other kinds of item is a compile
error.

### Closures

```harn
let f = { x -> x * 2 }
let g = { a, b -> a + b }
```

First-class values. When invoked, a child environment is created from the *captured*
environment (not the call-site environment), and parameters are bound as immutable bindings.

### Spread in function calls

The spread operator `...` expands a list into individual function arguments.
It can be used in both function calls and method calls:

```harn
fn add(a, b, c) {
  return a + b + c
}

let args = [1, 2, 3]
add(...args)           // equivalent to add(1, 2, 3)
```

Spread arguments can be mixed with regular arguments:

```harn
fn add(a, b, c) { return a + b + c }

let rest = [2, 3]
add(1, ...rest)        // equivalent to add(1, 2, 3)
```

Multiple spreads are allowed in a single call, and they can appear in any
position:

```harn
fn add(a, b, c) { return a + b + c }

let first = [1]
let last = [3]
add(...first, 2, ...last)   // equivalent to add(1, 2, 3)
```

At runtime the VM flattens all spread arguments into the argument list
before invoking the function. If the total number of arguments does not
match the function's parameter count, the usual arity error is produced.

### Return

`return value` inside a function/closure unwinds execution via
`HarnRuntimeError.returnValue`. The closure invocation catches this and returns the value.
`return` inside a pipeline terminates the pipeline.

## Enums

Enums define a type with a fixed set of named variants, each optionally
carrying associated data.

### Enum declaration

```harn
enum Color {
  Red,
  Green,
  Blue
}

enum Shape {
  Circle(float),
  Rectangle(float, float)
}
```

Variants without data are simple tags. Variants with data carry positional
fields specified in parentheses.

### Enum construction

Variants are constructed using dot syntax on the enum name:

```harn
let c = Color.Red
let s = Shape.Circle(5.0)
let r = Shape.Rectangle(3.0, 4.0)
```

### Pattern matching on enums

Enum variants are matched using `EnumName.Variant(binding)` patterns in
`match` expressions:

```harn
match s {
  Shape.Circle(radius) -> { log("circle r=${radius}") }
  Shape.Rectangle(w, h) -> { log("rect ${w}x${h}") }
}
```

A `match` on an enum **must** be exhaustive: a missing variant is a hard
error, not a warning. Add the missing arm or end with a wildcard
`_ -> { … }` arm to opt out. `if/elif/else` chains stay intentionally
partial; opt into exhaustiveness by ending the chain with
`unreachable("…")`.

### Built-in Result enum

Harn provides a built-in generic `Result<T, E>` enum with two variants:

- `Result.Ok(value)` -- represents a successful result
- `Result.Err(error)` -- represents an error

Shorthand constructor functions `Ok(value)` and `Err(value)` are available
as builtins, equivalent to `Result.Ok(value)` and `Result.Err(value)`.

```harn
let ok = Ok(42)
let err = Err("something failed")
let typed_ok: Result<int, string> = ok

// Equivalent long form:
let ok2 = Result.Ok(42)
let err2 = Result.Err("oops")
```

### Result helper functions

| Function | Description |
|---|---|
| `is_ok(r)` | Returns `true` if `r` is `Result.Ok` |
| `is_err(r)` | Returns `true` if `r` is `Result.Err` |
| `unwrap(r)` | Returns the `Ok` value, throws if `r` is `Err` |
| `unwrap_or(r, default)` | Returns the `Ok` value, or `default` if `r` is `Err` |
| `unwrap_err(r)` | Returns the `Err` value, throws if `r` is `Ok` |

### The `?` operator (Result propagation)

The postfix `?` operator unwraps a `Result.Ok` value or propagates a
`Result.Err` from the current function. It is a postfix operator with the
same precedence as `.`, `[]`, and `()`.

```harn
fn divide(a, b) {
  if b == 0 {
    return Err("division by zero")
  }
  return Ok(a / b)
}

fn compute(x) {
  let result = divide(x, 2)?   // unwraps Ok, or returns Err early
  return Ok(result + 10)
}

let r1 = compute(20)   // Result.Ok(20)
let r2 = compute(0)    // would propagate Err from divide
```

The `?` operator requires its operand to be a `Result` value. Applying `?`
to a non-Result value produces a type error at runtime.

Disambiguation: when the parser sees `expr?`, it distinguishes between the
postfix `?` (Result propagation) and the ternary `? :` operator by checking
whether the token following `?` could start a ternary branch expression.

### Pattern matching on Result

```harn
match result {
  Result.Ok(val) -> { log("success: ${val}") }
  Result.Err(err) -> { log("error: ${err}") }
}
```

### Try-expression

The `try` keyword used without a `catch` block acts as a try-expression.
It evaluates the body and wraps the result in a `Result`:

- If the body succeeds, returns `Result.Ok(value)`.
- If the body throws an error, returns `Result.Err(error)`.

```harn
let result = try { json_parse(raw_input) }
// result is Result.Ok(parsed_data) or Result.Err("invalid JSON: ...")
```

The try-expression is the complement of the `?` operator: `try` enters
Result-land by catching errors, while `?` exits Result-land by propagating
errors. Together they form a complete error-handling pipeline:

```harn
fn safe_divide(a, b) {
  let result = try { a / b }
  return result
}

fn compute(x) {
  let val = safe_divide(x, 2)?  // unwrap Ok or propagate Err
  return Ok(val + 10)
}
```

No `catch` or `finally` block is needed for the `Result`-wrapping form. When
`catch` or `finally` follow `try`, the form is a handled `try`/`catch`
expression whose value is the try or catch body's tail value (see
[try/catch/finally](#trycatchfinally)); only the bare `try { ... }` form wraps
in `Result`.

### Result in pipelines

The `?` operator works naturally in pipelines:

```harn
fn fetch_and_parse(url) {
  let response = http_get(url)?
  let data = json_parse(response)?
  return Ok(data)
}
```

## Structs

Structs define named record types with typed fields. Structs may also be
generic.

### Struct declaration

```harn
struct Point {
  x: int
  y: int
}

struct User {
  name: string
  age: int
}

struct Pair<A, B> {
  first: A
  second: B
}
```

Fields are declared with `name: type` syntax, one per line.

### Struct construction

Struct instances can be constructed with the struct name followed by
a named-field body:

```harn
struct Point { x: int, y: int }
struct User { name: string, age: int }
struct Pair<A, B> { first: A, second: B }
let p = Point { x: 3, y: 4 }
let u = User { name: "Alice", age: 30 }
let pair: Pair<int, string> = Pair { first: 1, second: "two" }
```

### Field access

Struct fields are accessed with dot syntax, the same as dict property
access:

```harn
log(p.x)    // 3
log(u.name) // "Alice"
```

## Impl blocks

Impl blocks attach methods to a struct type.

### Syntax

```harn
impl TypeName {
  fn method_name(self, arg) {
    // body -- self refers to the struct instance
  }
}
```

The first parameter of each method must be `self`, which receives the
struct instance the method is called on.

### Method calls

Methods are called using dot syntax on struct instances:

```harn
struct Point {
  x: int
  y: int
}

impl Point {
  fn distance(self) {
    return sqrt(self.x * self.x + self.y * self.y)
  }
  fn translate(self, dx, dy) {
    return Point { x: self.x + dx, y: self.y + dy }
  }
}

let p = Point { x: 3, y: 4 }
log(p.distance())           // 5.0
let p2 = p.translate(10, 20)
log(p2.x)                   // 13
```

When `instance.method(args)` is called, the VM looks up methods registered
by the `impl` block for the instance's struct type. The instance is
automatically passed as the `self` argument.

## Interfaces

Interfaces define a set of method signatures that a struct type must
implement. Harn uses Go-style implicit satisfaction: a struct satisfies
an interface if its impl block contains all the required methods with
compatible signatures. There is no `implements` keyword. Interfaces may
also declare associated types.

### Interface declaration

```harn
interface Displayable {
  fn display(self) -> string
}

interface Serializable {
  fn serialize(self) -> string
  fn byte_size(self) -> int
}

interface Collection {
  type Item
  fn get(self, index: int) -> Item
}
```

Each method signature lists parameters (the first must be `self`) and an
optional return type. Associated types name implementation-defined types
that methods can refer to. The body is omitted -- interfaces only declare
the shape of the methods.

### Implicit satisfaction

A struct satisfies an interface when its `impl` block has all the methods
declared by the interface, with matching parameter counts:

```harn
struct Dog {
  name: string
}

impl Dog {
  fn display(self) -> string {
    return "Dog(${self.name})"
  }
}
```

`Dog` satisfies `Displayable` because it has a `display(self) -> string`
method. No extra annotation is needed.

### Using interfaces as type annotations

Interfaces can be used as parameter types. At compile time, the type
checker verifies that any struct passed to such a parameter satisfies
the interface:

```harn,ignore
fn show(item: Displayable) {
  println(item.display())
}

let d = Dog({name: "Rex"})
show(d)  // OK: Dog satisfies Displayable
```

### Generic constraints with interfaces

Interfaces can be used as generic constraints via `where` clauses:

```harn
fn process<T>(item: T) where T: Displayable {
  println(item.display())
}
```

The type checker verifies at call sites that the concrete type passed
for `T` satisfies `Displayable`. Passing a type that does not satisfy
the constraint produces a compile-time error. Generic parameters must bind
consistently across all arguments in the call, and container bindings such as
`list<T>` propagate the concrete element type instead of collapsing to an
unconstrained generic.

### Subtyping and variance

Harn's subtype relation is *polarity-aware*: each compound type has a
declared variance per slot that determines whether widening (e.g.
`int <: float`) is allowed in that slot, prohibited entirely, or
applied with the direction reversed.

Type parameters on user-defined generics may be marked with `in` or
`out`:

```harn,ignore
type Reader<out T> = fn() -> T          // T appears only in output position
interface Sink<in T> { fn accept(v: T) -> int }
fn map<in A, out B>(value: A) -> B { ... }
```

| Marker | Meaning | Where T may appear |
|---|---|---|
| `out T` | covariant | output positions only |
| `in T` | contravariant | input positions only |
| (none) | invariant (default) | anywhere |

Unannotated parameters default to **invariant**. This is strictly
safer than implicit covariance — `Box<int>` does not flow into
`Box<float>` unless `Box` declares `out T` and the body uses `T`
only in covariant positions.

#### Built-in variance

| Constructor | Variance |
|---|---|
| `iter<T>` | covariant in `T` (read-only) |
| `list<T>` | invariant in `T` (mutable: `push`, index assignment) |
| `dict<K, V>` | invariant in both `K` and `V` (mutable) |
| `Result<T, E>` | covariant in both `T` and `E` |
| `fn(P1, ...) -> R` | parameters **contravariant**, return covariant |
| Shape `{ field: T, ... }` | covariant per field (width subtyping) |

The numeric widening `int <: float` only applies in covariant
positions. In invariant or contravariant positions it is suppressed —
that is what makes `list<int>` to `list<float>` a type error.

#### Function subtyping

For an actual `fn(A) -> R'` to be a subtype of an expected `fn(B) -> R`,
**`B` must be a subtype of `A`** (parameters are contravariant) and
`R'` must be a subtype of `R` (return is covariant). A callback that
accepts a wider input or produces a narrower output is always a valid
substitute.

```harn,ignore
let wide = fn(x: float) { return 0 }
let cb: fn(int) -> int = wide   // OK: float-accepting closure stands in for int-accepting

let narrow = fn(x: int) { return 0 }
let bad: fn(float) -> int = narrow   // ERROR: narrow cannot accept the float a caller may pass
```

#### Declaration-site checking

When a type parameter is marked `in` or `out`, the declaration body
is checked: each occurrence of the parameter must respect the
declared variance. Mismatches are caught at definition time, not at
each use:

```harn,ignore
type Box<out T> = fn(T) -> int
// ERROR: type parameter 'T' is declared 'out' (covariant) but appears
// in a contravariant position in type alias 'Box'
```

## Attributes

Attributes are declarative metadata attached to a top-level declaration
with the `@` prefix. They compile to side-effects (warnings, runtime
registrations) at the attached declaration, and stack so a single decl
can carry multiple. Arguments are restricted to literal values
(strings, numbers, booleans, `nil`, bare identifiers) — no runtime
evaluation, no expressions.

### Syntax

```ebnf
attribute    ::= '@' IDENTIFIER ['(' attr_arg (',' attr_arg)* [','] ')']
attr_arg     ::= [IDENTIFIER ':'] attr_value
attr_value   ::= literal | IDENTIFIER
```

```harn
@deprecated(since: "0.8", use: "compute_v2")
@test
pub fn compute(x: int) -> int { return x + 1 }
```

Attributes attach to the **immediately following** declaration —
either `pipeline`, `fn`, `tool`, `struct`, `enum`, `type`, `interface`,
or `impl`. Attaching to anything else (a `let`, a statement) is a parse
error.

### Standard attributes

#### `@deprecated`

```harn,ignore
@deprecated(since: "0.8", use: "new_fn")
pub fn old_fn() -> int { ... }
```

Emits a type-checker warning at every call site of the attributed
function. Both arguments are optional; when present they are folded
into the warning message.

| Argument | Type | Meaning |
|---|---|---|
| `since` | string | Version that introduced the deprecation |
| `use` | string | Replacement function name (rendered as a help line) |

#### `@test`

```harn,ignore
@test
pipeline test_smoke(task) { ... }
```

Marks a pipeline as a test entry point. The conformance / `harn test`
runner discovers attributed pipelines in addition to the legacy
`test_*` naming convention. Both forms continue to work.

#### `@complexity(allow)`

```harn,ignore
@complexity(allow)
pub fn classify(x: int) -> string {
  if x == 1 { return "one" }
  ...
}
```

Suppresses the `cyclomatic-complexity` lint warning on the attached
function. The bare `allow` identifier is the only currently accepted
form. Use it for functions whose branching is *intrinsic* (parsers,
tier dispatchers, tree-sitter adapters) rather than accidental.

The rule fires when a function's cyclomatic score exceeds the default
threshold of **25**. Projects can override the threshold in
`harn.toml`:

```toml
[lint]
complexity_threshold = 15   # stricter for this project
```

Cyclomatic complexity counts each branching construct (`if`/`else`,
`guard`, `match` arm, `for`, `while`, `try`/`catch`, `ternary`,
`select` case, `retry`) and each short-circuit boolean operator
(`&&`, `||`). Nesting, guard-vs-if, and De Morgan rewrites are all
**score-preserving** — the only way to reduce the count is to
extract helpers or mark the function `@complexity(allow)`.

#### `@acp_tool`

```harn,ignore
@acp_tool(name: "edit", kind: "edit", side_effect_level: "mutation")
pub fn apply_edit(path: string, content: string) -> EditResult { ... }
```

Compiles to the same runtime registration as an imperative
`tool_define(tool_registry(), name, "", { handler, annotations })`
call, with the function bound as the tool's `handler` and every named
attribute argument (other than `name`) lifted into the
`annotations` dict. `name` defaults to the function name when
omitted.

| Argument | Type | Meaning |
|---|---|---|
| `name` | string | Tool name (defaults to fn name) |
| `kind` | string | One of `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `other` |
| `side_effect_level` | string | `none`, `read`, `mutation`, `destructive` |

Other named arguments pass through to the annotations dict unchanged,
so additional `ToolAnnotations` fields can be added without a parser
change.

#### `@invariant`

```harn,ignore
@invariant("fs.writes", "src/**")
tool write_patch() {
  write_file("src/out.txt", "ok")
}
```

Attaches one or more compile-time capability invariants to a `fn`,
`tool`, or `pipeline`. Invariants are only evaluated when the user opts
into them with `harn check --invariants`; plain `harn check` keeps the
baseline type-check + preflight behavior. Each attributed handler is
lowered into a small control-flow graph plus simple data-flow summaries,
then the selected invariant checks run against that IR.

| Invariant | Configuration | Meaning |
|---|---|---|
| `@invariant("fs.writes", "src/**")` | One or more allowed globs, passed positionally or as `path_glob:` / `glob:` / `allow:` | Every reachable file-system write must target a literal path proven to stay within one of the declared globs. |
| `@invariant("budget.remaining", target: "remaining")` | Optional `target:` variable name, default `budget.remaining` | Assignments to the tracked budget value may only preserve it, decrement it, or refresh it from `llm_budget_remaining()`. |
| `@invariant("approval.reachability")` | No extra args | Every reachable side-effecting call must be gated by a prior `request_approval(...)` or enclosed inside a `dual_control(...)` approval scope. |

Invariant violations surface through `harn check --invariants`,
`harn explain --invariant <name> <handler> <file>`, and the LSP. Each
diagnostic carries a concrete CFG path so editors and the CLI can show
how the violating call or assignment is reached.

### Unknown attributes

Unknown attribute names produce a type-checker warning so that
misspellings surface at check time. The attribute itself is otherwise
ignored — code still compiles.

## Type annotations

Harn has an optional, gradual type system. Type annotations are checked at compile time
but do not affect runtime behavior. Omitting annotations is always valid.

### Basic types

```harn
let name: string = "Alice"
let age: int = 30
let rate: float = 3.14
let ok: bool = true
let nothing: nil = nil
```

### The `never` type

`never` is the bottom type — the type of expressions that never produce a
value. It is a subtype of all other types.

Expressions that infer to `never`:

- `throw expr`
- `return expr`
- `break` and `continue`
- A block where every control path exits
- An `if`/`else` where both branches infer to `never`
- Calls to `unreachable()`

`never` is removed from union types: `never | string` simplifies to
`string`. An empty union (all members removed by narrowing) becomes
`never`.

```harn
fn always_throws() -> never {
  throw "this function never returns normally"
}
```

### The `any` type

`any` is the top type and the explicit escape hatch. Every concrete
type is assignable to `any`, and `any` is assignable back to every
concrete type without narrowing. `any` disables type checking in both
directions for the values it flows through.

```harn
fn passthrough(x: any) -> any {
  return x
}

let s: string = passthrough("hello")  // any → string, no narrowing required
let n: int    = passthrough(42)
```

Use `any` deliberately, when you want to opt out of checking — for
example, a generic dispatcher that forwards values through a runtime
protocol you don't want to describe statically. Prefer `unknown` (see
below) for values from untrusted boundaries where callers should be
forced to narrow.

### The `unknown` type

`unknown` is the safe top type. Every concrete type is assignable to
`unknown`, but an `unknown` value is **not** assignable to any
concrete type without narrowing. This is the correct annotation for
values arriving from untrusted boundaries (parsed JSON, LLM responses,
dynamic dicts) where callers should be forced to validate the shape
before use.

```harn
fn describe(v: unknown) -> string {
  // Direct use of `v` as a concrete type is a compile-time error.
  // Narrow via type_of/schema_is first.
  if type_of(v) == "string" {
    return "string: ${v.upper()}"
  }
  if type_of(v) == "int" {
    return "int: ${v + 1}"
  }
  return "other"
}
```

Narrowing rules for `unknown`:

- `type_of(x) == "T"` narrows `x` to `T` on the truthy branch (where
  `T` is one of the type-of protocol names: `string`, `int`, `float`,
  `bool`, `nil`, `list`, `dict`, `closure`, `bytes`).
- `schema_is(x, Shape)` narrows `x` to `Shape` on the truthy branch.
- `guard type_of(x) == "T" else { ... }` narrows `x` to `T` in the
  surrounding scope after the guard.
- The falsy branch keeps `unknown` — subtracting one concrete type
  from an open top still leaves an open top. The checker still tracks
  which concrete `type_of` variants have been ruled out on the current
  flow path, so an exhaustive chain ending in `unreachable()` / `throw`
  can be validated; see the "Exhaustive narrowing on `unknown`"
  subsection of "Flow-sensitive type refinement".

Interop between `any` and `unknown`:

- `unknown` is assignable to `any` (upward to the full escape hatch).
- `any` is assignable to `unknown` (downward — the `any` escape hatch
  lets it flow into anything, including `unknown`).

**When to pick which:**

- **No annotation** — "I haven't annotated this." Callers get no
  checking. Use for internal, unstable code.
- **`unknown`** — "this value could be anything; narrow before use."
  Use at untrusted boundaries and in APIs that hand back open-ended
  data. This is the preferred annotation for LLM / JSON / dynamic
  dict values.
- **`any`** — "stop checking." A last-resort escape hatch. Prefer
  `unknown` unless you have a specific reason to defeat checking
  bidirectionally.

### Union types

```harn
let value: string | nil = nil
let id: int | string = "abc"
```

Union members may also be **literal types** — specific string or int
values used to encode enum-like discriminated sets:

```harn
type Verdict = "pass" | "fail" | "unclear"
type RetryCount = 0 | 1 | 2 | 3

let v: Verdict = "pass"
```

Literal types are assignable to their base type (`"pass"` flows into
`string`), and a base-typed value flows into a literal union (`string`
into `Verdict`). Runtime `schema_is` / `schema_expect` guards and the
parameter-annotation runtime check reject values that violate the
literal set.

A `match` on a literal union must cover every literal or include a
wildcard `_` arm — non-exhaustive `match` is a hard error.

#### Tagged shape unions (discriminated unions)

A union of two or more dict shapes is a *tagged shape union* when the
shapes share a discriminant field. The discriminant is auto-detected:
the first field of the first variant that (a) is non-optional in every
member, (b) has a literal type (`LitString` or `LitInt`), and (c) takes
a distinct literal value per variant qualifies. The field can be named
anything — `kind`, `type`, `op`, `t`, etc. — there is no privileged
spelling.

```harn
type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int}
```

Matching on the discriminant narrows the value to the matching variant
inside each arm; the same narrowing fires under
`if obj.<tag> == "value"` / `else`:

```harn
fn handle(m: Msg) -> string {
  match m.kind {
    "ping" -> { return "ttl=" + to_string(m.ttl) }
    "pong" -> { return to_string(m.latency_ms) + "ms" }
  }
}
```

Such a `match` must cover every variant or include a wildcard `_` arm
— non-exhaustive `match` is a hard error.

#### Distributive generic instantiation

Generic type aliases distribute over closed-union arguments. Writing
`Container<A | B>` is equivalent to `Container<A> | Container<B>` so
each instantiation independently fixes the type parameter. This is what
keeps `processCreate: fn("create") -> nil` flowing into a `list<
ActionContainer<Action>>` element instead of getting rejected by the
contravariance of the function-parameter slot:

```harn
type Action = "create" | "edit"
type ActionContainer<T> = {action: T, process_action: fn(T) -> nil}
```

`ActionContainer<Action>` resolves to `ActionContainer<"create"> |
ActionContainer<"edit">`, and a literal-tagged shape on the right flows
into the matching branch.

### Parameterized types

```harn
let numbers: list<int> = [1, 2, 3]
let headers: dict<string, string> = {content_type: "json"}
```

### Structural types (shapes)

Dict shape types describe the expected fields of a dict value. The type checker
verifies that dict literals have the required fields with compatible types.

```harn
let user: {name: string, age: int} = {name: "Alice", age: 30}
```

Optional fields use `?` and need not be present:

```harn
let config: {host: string, port?: int} = {host: "localhost"}
```

Width subtyping: a dict with extra fields satisfies a shape that requires fewer fields.

```harn
fn greet(u: {name: string}) -> string {
  return "hi ${u["name"]}"
}
greet({name: "Bob", age: 25})  // OK — extra field allowed
```

Nested shapes:

```harn
let data: {user: {name: string}, tags: list} = {user: {name: "X"}, tags: []}
```

Shapes are compatible with `dict` and `dict<string, V>` when all field values match `V`.

### Type aliases

```harn
type Config = {model: string, max_tokens: int}
let cfg: Config = {model: "gpt-4", max_tokens: 100}
```

A type alias can also drive schema validation for structured LLM output
and runtime guards. `schema_of(T)` lowers an alias to a JSON-Schema
dict at compile time:

```harn
type GraderOut = {
  verdict: "pass" | "fail" | "unclear",
  summary: string,
  findings: list<string>,
}

// Use the alias directly wherever a schema dict is expected.
let s = schema_of(GraderOut)
let ok = schema_is({verdict: "pass", summary: "x", findings: []}, GraderOut)

let r = llm_call(prompt, nil, {
  provider: "openai",
  output_schema: GraderOut,     // alias in value position — compiled to schema_of(T)
  schema_retries: 2,
})
```

`llm_call` can also express routing intent without pinning a single
provider/model pair. The `route_policy` option accepts:

- `"manual"` (default): use the normal `provider` / `model` / env resolution.
- `"always(id)"`: pin to a model alias, model id, or `provider:model` selector.
- `"cheapest_over_quality(t)"`: select the lowest-cost available catalog
  candidate whose model tier is at least `t`.
- `"fastest_over_quality(t)"`: select the lowest-latency available catalog
  candidate whose model tier is at least `t`.

The optional `fallback_chain` is an ordered list of provider ids to try when
the selected provider fails availability or transport. Routing decisions are
recorded in LLM transcript events with the selected route plus all considered
alternatives so costs can be re-scored later:

```harn
let r = llm_call(prompt, nil, {
  route_policy: "cheapest_over_quality(mid)",
  fallback_chain: ["local", "ollama", "openai"],
})
```

The emitted schema follows canonical JSON-Schema conventions (objects
with `properties`/`required`, arrays with `items`, literal unions as
`{type, enum}`) so it is compatible with structured-output validators
and with ACP `ToolAnnotations.args` schemas. The compile-time lowering
applies when the alias identifier appears as:

- The argument of `schema_of(T)`.
- The schema argument of `schema_is`, `schema_expect`, `schema_parse`,
  `schema_check`, `is_type`, `json_validate`.
- The value of an `output_schema:` entry in an `llm_call` options dict.

For aliases not known at compile time (e.g. `let T = schema_of(Foo)`
or dynamic construction), passthrough through the runtime `schema_of`
builtin keeps existing schema dicts working.

#### Generic inference via `Schema<T>`

Schema-driven builtins are typed with proper generics so user-defined
wrappers pick up the same narrowing.

- `llm_call<T>(prompt, system, options: {output_schema: Schema<T>, ...})
  -> {data: T, text: string, ...}`
- `llm_completion<T>` has the same signature.
- `schema_parse<T>(value: unknown, schema: Schema<T>) -> Result<T, string>`
- `schema_check<T>(value: unknown, schema: Schema<T>) -> Result<T, string>`
- `schema_expect<T>(value: unknown, schema: Schema<T>) -> T`

`Schema<T>` denotes a runtime schema value whose static shape is `T`.
In a parameter position, matching a `Schema<T>` against an argument
whose value resolves to a type alias (directly, via `schema_of(T)`,
or via an inline JSON-Schema dict literal) binds the type parameter.
A user-defined wrapper such as

```harn,ignore
fn grade<T>(prompt: string, schema: Schema<T>) -> T {
  let r = llm_call(prompt, nil,
    {provider: "mock", output_schema: schema, output_validation: "error",
     response_format: "json"})
  return r.data
}

let out: GraderOut = grade("Grade this", schema_of(GraderOut))
println(out.verdict)
```

narrows `out` to `GraderOut` at the call site without any
`schema_is` / `schema_expect` guard, and without per-wrapper
typechecker support.

`Schema<T>` is a type-level construct. In value positions, the
runtime `schema_of(T)` builtin returns an idiomatic schema dict
whose static type is `Schema<T>`.

### Human-in-the-loop stdlib

Human-in-the-loop is modeled as typed stdlib primitives rather than special
syntax. The runtime owns blocking semantics, timeout behavior, event-log
records, and replay.

- `ask_user<T>(prompt: string, options?: {schema?: Schema<T>, timeout?: duration, default?: T}) -> T`
- `request_approval(action: string, options?: {detail?: any, quorum?: int, reviewers?: list<string>, deadline?: duration})`
  returns `{approved: bool, reviewers: list<string>, approved_at: string, reason: string | nil,
  signatures: list<{reviewer: string, signed_at: string, signature: string}>}`.
- `dual_control<T>(n: int, m: int, action: fn() -> T, approvers?: list<string>) -> T`
- `escalate_to(role: string, reason: string)`
  returns `{request_id: string, role: string, reason: string, trace_id: string,
  status: string, accepted_at: string | nil, reviewer: string | nil}`.
- `hitl_pending(filters?: {since?: string, until?: string, kinds?: list<string>,
  agent?: string, limit?: int})`
  returns `list<{request_id: string, request_kind: string, agent: string,
  prompt: string, trace_id: string, timestamp: string, approvers: list<string>,
  metadata: dict}>`.

Normative behavior:

- `ask_user` appends `hitl.question_asked`, then blocks until the host appends
  a matching response. The default timeout is 24 hours unless `timeout` is
  supplied. If `schema` is present, the answer must satisfy it. If
  the wait times out, Harn appends `hitl.timeout` and either returns
  `options.default` or throws `HumanTimeoutError`.
- `request_approval` appends `hitl.approval_requested` and waits for the
  configured quorum. `deadline` defaults to 24 hours. Denial raises
  `ApprovalDeniedError`. Successful completion returns the approval record,
  including one signed reviewer timestamp receipt per counted approver.
- `dual_control` is an approval-gated wrapper around a closure. The closure is
  not executed until quorum is satisfied. The runtime appends
  `hitl.dual_control_requested`, `hitl.dual_control_approved` /
  `hitl.dual_control_denied`, and `hitl.dual_control_executed`.
- `escalate_to` appends `hitl.escalation_issued` and blocks until the host
  appends `hitl.escalation_accepted`. The request payload includes the active
  capability policy when one is installed so hosts can resolve the requested
  role against the same capability ceiling enforced by the VM. If the host does
  not respond, the dispatch remains paused until manual resume.
- `hitl_pending` reads the durable HITL topics via the active event log,
  returns `[]` when no event log is attached, filters by `since` / `until` /
  `kinds` / `agent` / `limit`, and omits requests that have already reached a
  terminal HITL event.

HITL records live in durable event-log topics:

- `hitl.questions`
- `hitl.approvals`
- `hitl.dual_control`
- `hitl.escalations`

Replay is event-log-driven. During replay, HITL primitives resolve from the
previously recorded HITL response events instead of consulting a live host,
so approval reviewer identities, signed timestamps, and signatures remain
stable across deterministic replay.

### Function type annotations

Parameters and return types can be annotated:

```harn
fn add(a: int, b: int) -> int {
  return a + b
}
```

### Type checking behavior

- Annotations are optional (gradual typing). Untyped values are `None` and skip checks.
- `int` is assignable to `float`.
- Dict literals with string keys infer a structural shape type.
- Dict literals with computed keys infer as generic `dict`.
- Shape-to-shape: all required fields in the expected type must exist with compatible types.
- Shape-to-`dict<K, V>`: all field values must be compatible with `V`.
- Type errors are reported at compile time and halt execution.

### Flow-sensitive type refinement

The type checker performs flow-sensitive type refinement (narrowing) on
union types based on control flow conditions.  Refinements are
bidirectional — both the truthy and falsy paths of a condition are
narrowed.

#### Nil checks

`x != nil` narrows to non-nil in the then-branch and to `nil` in the
else-branch.  `x == nil` applies the inverse.

```harn
fn greet(name: string | nil) -> string {
  if name != nil {
    // name is `string` here
    return "hello ${name}"
  }
  // name is `nil` here
  return "hello stranger"
}
```

#### `type_of()` checks

`type_of(x) == "typename"` narrows to that type in the then-branch and
removes it from the union in the else-branch.

```harn
fn describe(x: string | int) {
  if type_of(x) == "string" {
    log(x)  // x is `string`
  } else {
    log(x)  // x is `int`
  }
}
```

#### Truthiness

A bare identifier in condition position narrows by removing `nil`:

```harn
fn check(x: string | nil) {
  if x {
    log(x)  // x is `string`
  }
}
```

#### Logical operators

- `a && b`: combines both refinements on the truthy path.
- `a || b`: combines both refinements on the falsy path.
- `!cond`: inverts truthy and falsy refinements.

```harn
fn check(x: string | int | nil) {
  if x != nil && type_of(x) == "string" {
    log(x)  // x is `string`
  }
}
```

#### Guard statements

After a `guard` statement, the truthy refinements apply to the outer
scope (since the else-body must exit):

```harn
fn process(x: string | nil) {
  guard x != nil else { return }
  log(x)  // x is `string` here
}
```

#### Early-exit narrowing

When one branch of an `if`/`else` definitely exits (via `return`,
`throw`, `break`, or `continue`), the opposite refinements apply after
the `if`:

```harn
fn process(x: string | nil) {
  if x == nil { return }
  log(x)  // x is `string` — the nil path returned
}
```

#### While loops

The condition's truthy refinements apply inside the loop body.

#### Ternary expressions

The condition's refinements apply to the true and false branches
respectively.

#### Match expressions

When matching a union-typed variable against literal patterns, the
variable's type is narrowed in each arm:

```harn
fn check(x: string | int) {
  match x {
    "hello" -> { log(x) }  // x is `string`
    42 -> { log(x) }       // x is `int`
    _ -> {}
  }
}
```

#### Or-patterns (`pat1 | pat2 -> body`)

A match arm may list two or more alternative patterns separated by `|`;
the shared body runs when any alternative matches. Each alternative
contributes to exhaustiveness coverage independently, so an or-pattern
and a single-literal arm compose naturally:

```harn
fn verdict(v: "pass" | "fail" | "unclear") -> string {
  return match v {
    "pass" -> { "ok" }
    "fail" | "unclear" -> { "not ok" }
  }
}
```

Narrowing inside the or-arm refines the matched variable to the *union
of the alternatives' single-literal narrowings*. On a literal union
this is a sub-union; on a tagged shape union it is a union of the
matching shape variants:

```harn,ignore
type Msg =
  {kind: "ping", ttl: int} |
  {kind: "pong", latency_ms: int} |
  {kind: "close", reason: string}

fn summarise(m: Msg) -> string {
  return match m.kind {
    "ping" | "pong" -> {
      // m is narrowed to {kind:"ping",…} | {kind:"pong",…};
      // the shared `kind` discriminant stays accessible.
      "live:" + m.kind
    }
    "close" -> { "closed:" + m.reason }
  }
}
```

Guards apply to the arm as a whole: `1 | 2 | 3 if n > 2 -> …` runs the
body only when some alternative matched *and* the guard held. A guard
failure falls through to the next arm, exactly like a literal-pattern
arm.

Or-patterns are restricted to literal alternatives (string, int,
float, bool, nil) in this release. Alternatives that introduce
identifier bindings or destructuring patterns are a forward-compatible
extension and currently rejected.

#### `.has()` on shapes

`dict.has("key")` narrows optional shape fields to required:

```harn
fn check(x: {name?: string, age: int}) {
  if x.has("name") {
    log(x)  // x.name is now required (non-optional)
  }
}
```

#### Exhaustiveness checking with `unreachable()`

The `unreachable()` builtin acts as a static exhaustiveness assertion.
When called with a variable argument, the type checker verifies that the
variable has been narrowed to `never` — meaning all possible types have
been handled. If not, a compile-time error reports the remaining types.

```harn
fn process(x: string | int | nil) -> string {
  if type_of(x) == "string" { return "string: ${x}" }
  if type_of(x) == "int" { return "int: ${x}" }
  if x == nil { return "nil" }
  unreachable(x)  // compile-time verified: x is `never` here
}
```

At runtime, `unreachable()` throws `"unreachable code was reached"` as a
safety net. When called without arguments or with a non-variable argument,
no compile-time check is performed.

#### Exhaustive narrowing on `unknown`

The checker tracks the set of concrete `type_of` variants that have been
ruled out on the current flow path for every `unknown`-typed variable.
The falsy branch of `type_of(v) == "T"` still leaves `v` typed `unknown`
(subtracting one concrete type from an open top still leaves an open
top), but the **coverage set** for `v` gains `"T"`.

When control flow reaches a never-returning site — `unreachable()`, a
`throw` statement, or a call to a user-defined function whose return
type is `never` — the checker verifies that the coverage set for every
still-`unknown` variable is either empty or complete. An incomplete
coverage set is treated as a failed exhaustiveness claim and triggers a
warning that names the uncovered concrete variants:

```harn
fn handle(v: unknown) -> string {
  if type_of(v) == "string" { return "s:${v}" }
  if type_of(v) == "int"    { return "i:${v}" }
  unreachable("unknown type_of variant")
  // warning: `unreachable()` reached but `v: unknown` was not fully
  // narrowed — uncovered concrete type(s): float, bool, nil, list,
  // dict, closure, bytes
}
```

Covering all nine `type_of` variants (`int`, `string`, `float`, `bool`,
`nil`, `list`, `dict`, `closure`, `bytes`) silences the warning. Suppression via
an explicit fallthrough `return` is intentional: a plain `return`
doesn't claim exhaustiveness, so partial narrowing followed by a normal
return stays silent. Reaching `throw` or `unreachable()` with no prior
`type_of` narrowing also stays silent — the coverage set must be
non-empty for the warning to fire, which avoids false positives on
unrelated error paths.

Reassigning the variable clears its coverage set, matching the way
narrowing is already invalidated on reassignment.

#### Unreachable code warnings

The type checker warns about code after statements that definitely exit
(via `return`, `throw`, `break`, or `continue`), including composite
exits where both branches of an `if`/`else` exit:

```harn
fn foo(x: bool) {
  if x { return 1 } else { throw "err" }
  log("never reached")  // warning: unreachable code
}
```

#### Reassignment invalidation

When a narrowed variable is reassigned, the narrowing is invalidated and
the original declared type is restored.

#### Mutability

Variables declared with `let` are immutable.  Assigning to a `let`
variable produces a compile-time warning (and a runtime error).

### Runtime parameter type enforcement

In addition to compile-time checking, function parameters with type annotations
are enforced at runtime. When a function is called, the VM verifies that each
annotated parameter matches its declared type before executing the function body.
If the types do not match, a `TypeError` is thrown:

```text
TypeError: parameter 'name' expected string, got int (42)
```

The following types are enforced at runtime: `int`, `float`, `string`, `bytes`,
`bool`, `list`, `dict`, `set`, `nil`, and `closure`. `int` and `float` are mutually
compatible (passing an `int` to a `float` parameter is allowed, and vice versa).
Union types, `list<T>`, `dict<string, V>`, and nested shapes are also checked at
runtime when the parameter annotation can be lowered into a runtime schema.

### Runtime shape validation

Shape-annotated function parameters are validated at runtime. When a function
parameter has a structural type annotation (e.g., `{name: string, age: int}`),
the VM checks that the argument is a dict (or struct instance) with all
required fields and that each field has the expected type.

```harn,ignore
fn process(user: {name: string, age: int}) {
  println("${user.name} is ${user.age}")
}

process({name: "Alice", age: 30})     // OK
process({name: "Alice"})              // Error: parameter 'user': missing field 'age' (int)
process({name: "Alice", age: "old"})  // Error: parameter 'user': field 'age' expected int, got string
```

Shape validation works with both plain dicts and struct instances. Extra
fields are allowed (width subtyping). Optional fields (declared with `?`)
are not required to be present.

## Built-in methods

### String methods

| Method | Signature | Returns |
|---|---|---|
| `count` | `.count` (property) | int -- character count |
| `empty` | `.empty` (property) | bool -- true if empty |
| `contains(sub)` | string | bool |
| `replace(old, new)` | string, string | string |
| `split(sep)` | string | list of strings |
| `trim()` | (none) | string -- whitespace stripped |
| `starts_with(prefix)` | string | bool |
| `ends_with(suffix)` | string | bool |
| `lowercase()` | (none) | string |
| `uppercase()` | (none) | string |
| `substring(start, end?)` | int, int? | string -- character range |

### List methods

| Method | Signature | Returns |
|---|---|---|
| `count` | (property) | int |
| `empty` | (property) | bool |
| `first` | (property) | value or nil |
| `last` | (property) | value or nil |
| `map(closure)` | closure(item) -> value | list |
| `filter(closure)` | closure(item) -> bool | list |
| `reduce(init, closure)` | value, closure(acc, item) -> value | value |
| `find(closure)` | closure(item) -> bool | value or nil |
| `any(closure)` | closure(item) -> bool | bool |
| `all(closure)` | closure(item) -> bool | bool |
| `flat_map(closure)` | closure(item) -> value/list | list (flattened) |

### Dict methods

| Method | Signature | Returns |
|---|---|---|
| `keys()` | (none) | list of strings (sorted) |
| `values()` | (none) | list of values (sorted by key) |
| `entries()` | (none) | list of `{key, value}` dicts (sorted by key) |
| `count` | (property) | int |
| `has(key)` | string | bool |
| `merge(other)` | dict | dict (other wins on conflict) |
| `map_values(closure)` | closure(value) -> value | dict |
| `filter(closure)` | closure(value) -> bool | dict |

### Dict property access

`dict.name` returns the value for key `"name"`, or `nil` if absent.

### Set builtins

Sets are created with the `set()` builtin and are immutable -- mutation
operations return a new set. Sets deduplicate values using structural
equality.

| Function | Signature | Returns |
|---|---|---|
| `set(...)` | values or a list | set -- deduplicated |
| `set_add(s, value)` | set, value | set -- with value added |
| `set_remove(s, value)` | set, value | set -- with value removed |
| `set_contains(s, value)` | set, value | bool |
| `set_union(a, b)` | set, set | set -- all items from both |
| `set_intersect(a, b)` | set, set | set -- items in both |
| `set_difference(a, b)` | set, set | set -- items in a but not b |
| `to_list(s)` | set | list -- convert set to list |

Sets are iterable with `for ... in` and support `len()`.

### Encoding and hashing builtins

| Function | Description |
|---|---|
| `base64_encode(str)` | Returns the base64-encoded version of `str` |
| `base64_decode(str)` | Returns the decoded string from a base64-encoded `str` |
| `base64url_encode(str)` | Returns the URL-safe base64 encoding of `str` using the RFC 4648 alphabet without padding |
| `base64url_decode(str)` | Returns the decoded string from a URL-safe base64 `str` without padding |
| `base32_encode(str)` | Returns the RFC 4648 base32 encoding of `str` |
| `base32_decode(str)` | Returns the decoded string from a base32-encoded `str` |
| `hex_encode(str)` | Returns the lowercase hex encoding of `str` |
| `hex_decode(str)` | Returns the decoded string from a hex-encoded `str` |
| `bytes_from_string(str)` | UTF-8 encodes `str` into `bytes` |
| `bytes_to_string(bytes)` | UTF-8 decodes `bytes` into `string` |
| `bytes_to_string_lossy(bytes)` | Lossy UTF-8 decode of `bytes` |
| `bytes_from_hex(str)` | Parses lowercase/uppercase hex into `bytes` |
| `bytes_to_hex(bytes)` | Hex-encodes `bytes` |
| `bytes_from_base64(str)` | Decodes base64 into `bytes` |
| `bytes_to_base64(bytes)` | Encodes `bytes` as base64 |
| `bytes_len(bytes)` | Returns the length in octets |
| `bytes_concat(a, b)` | Concatenates two byte buffers |
| `bytes_slice(bytes, start, end)` | Returns a clamped slice of a byte buffer |
| `bytes_eq(a, b)` | Constant-time byte equality check |
| `sha256(str)` | Returns the hex-encoded SHA-256 hash of `str` |
| `md5(str)` | Returns the hex-encoded MD5 hash of `str` |
| `jwt_sign(alg, claims, private_key)` | Signs a compact JWT/JWS with a PEM private key. Supports `ES256` and `RS256` |

```harn
let encoded = base64_encode("hello world")  // "aGVsbG8gd29ybGQ="
let decoded = base64_decode(encoded)        // "hello world"
let jwt = base64url_encode("{\"alg\":\"HS256\"}") // no `=` padding
let text = hex_decode("68656c6c6f")         // "hello"
let hash = sha256("hello")                  // hex string
let md5hash = md5("hello")                  // hex string
```

`jwt_sign` requires `claims` to be a dict so it can be serialized as a JSON
claims object. `ES256` expects a P-256 EC private key in PEM form; `RS256`
expects an RSA private key in PEM form. Unsupported algorithms, non-dict
claims, and invalid PEM keys throw runtime errors.

### Regex builtins

| Function | Description |
|---|---|
| `regex_match(pattern, str)` | Returns match data if `str` matches `pattern`, or `nil` |
| `regex_replace(pattern, str, replacement)` | Replaces all matches of `pattern` in `str` |
| `regex_captures(pattern, str)` | Returns a list of capture group dicts for all matches |

#### regex_captures

`regex_captures(pattern, text)` finds all matches of `pattern` in `text`
and returns a list of dicts, one per match. Each dict contains:

- `match`: the full match string
- `groups`: a list of positional capture group strings (from `(...)`)
- Any named capture groups (from `(?P<name>...)`) as additional keys

```harn
let results = regex_captures("(\\w+)@(\\w+)", "alice@example bob@test")
// results == [
//   {match: "alice@example", groups: ["alice", "example"]},
//   {match: "bob@test", groups: ["bob", "test"]}
// ]

let named = regex_captures("(?P<user>\\w+):(?P<role>\\w+)", "alice:admin")
// named == [{match: "alice:admin", groups: ["alice", "admin"], user: "alice", role: "admin"}]
```

Returns an empty list if there are no matches.

Regex patterns are compiled and cached internally using a thread-local
cache. Repeated calls with the same pattern string reuse the compiled
regex, avoiding recompilation overhead. This is a performance optimization
with no API-visible change.

### Connector interop builtins

The orchestrator exposes connector-oriented builtins for manifest-driven
provider integrations.

| Function | Description |
|---|---|
| `connector_call(provider, method, params?)` | Invoke the active outbound connector client for `provider` and return JSON-like result data |
| `secret_get(secret_id)` | Read a secret from the active connector context. Only available while executing a Harn-backed connector export such as `normalize_inbound` or `call` |
| `event_log_emit(topic, kind, payload, headers?)` | Append an event to the active event log from a Harn-backed connector export |
| `metrics_inc(name, amount?)` | Increment a connector-owned Prometheus counter from a Harn-backed connector export |

Harn-backed connector modules are loaded through manifest `[[providers]]`
entries and must export `provider_id()`, `kinds()`, and `payload_schema()`.
Inbound providers also export `normalize_inbound(raw)`, which returns a
`NormalizeResult` v1 dict. The top-level `type` field is one of:

- `"event"` with `event: {kind, dedupe_key, payload, ...}` for one normalized
  event.
- `"batch"` with `events: [{kind, dedupe_key, payload, ...}, ...]` for multiple
  normalized events.
- `"immediate_response"` with `immediate_response: {status, headers?, body?}`
  and optional `event` or `events` fields for ack-first webhook responses that
  may still enqueue normalized events.
- `"reject"` with `status`, `headers?`, and `body?` for explicit verification
  or unsupported-input rejection.

Each normalized event includes `kind`, `dedupe_key`, and `payload` plus optional
metadata such as `occurred_at`, `tenant_id`, `headers`, `batch`, and
`signature_status`. During the transition to NormalizeResult v1, runtimes also
accept the legacy direct event dict shape.

Poll-capable providers export `poll_tick(ctx)`. The orchestrator invokes this
hook for `kind = "poll"` bindings using the binding's `poll` configuration:
`interval`/`interval_ms`/`interval_secs`, optional
`jitter`/`jitter_ms`/`jitter_secs`, `state_key` or `cursor_state_key`,
`tenant_id`, `lease_id`, and `max_batch_size`. `ctx` contains the activated
binding, `binding_id`, RFC3339 `tick_at`, prior `cursor`, prior connector
`state`, `state_key`, optional `tenant_id`, `{id, tenant_id}` lease metadata,
and optional `max_batch_size`. `poll_tick` returns either a list of normalized
event dicts or `{events, cursor?, state?}`. Returned events enter the same
post-normalize dedupe and trigger inbox path as connector ingress events, and
the returned cursor/state is persisted for the next tick.

## Iterator protocol

Harn provides a lazy iterator protocol layered over the eager
collection methods. Eager methods (`list.map`, `list.filter`,
`list.flat_map`, `dict.map_values`, `dict.filter`, etc.) are
unchanged — they return eager collections. Lazy iteration is opt-in
via `.iter()` and the `iter(x)` builtin.

### The `Iter<T>` type

`Iter<T>` is a runtime value representing a lazy, single-pass, fused
iterator over values of type `T`. It is produced by calling `iter(x)`
or `x.iter()` on an iterable source (list, dict, set, string,
generator, channel) or by chaining a combinator on an existing iter.

`iter(x)` / `x.iter()` on a value that is already an `Iter<T>` is a
no-op (returns the iter unchanged).

### The `Pair<K, V>` type

`Pair<K, V>` is a two-element value used by the iterator protocol for
key/value and index/value yields.

- Construction: `pair(a, b)` builtin. Combinators such as `.zip` and
  `.enumerate` and dict iteration produce pairs automatically.
- Access: `.first` and `.second` as properties.
- For-loop destructuring: `for (k, v) in iter_expr { ... }` binds the
  `.first` and `.second` of each `Pair` to `k` and `v`.
- Equality: structural (`pair(1, 2) == pair(1, 2)`).
- Printing: `(a, b)`.

### For-loop integration

`for x in iter_expr` pulls values one at a time from `iter_expr` until
the iter is exhausted.

`for (a, b) in iter_expr` destructures each yielded `Pair` into two
bindings. If a yielded value is not a `Pair`, a runtime error is
raised.

`for entry in some_dict` (no `.iter()`) continues to yield
`{key, value}` dicts in sorted-key order for back-compat. Only
`some_dict.iter()` yields `Pair(key, value)`.

### Semantics

- **Lazy**: combinators allocate a new `Iter` and perform no work;
  values are only produced when a sink (or for-loop) pulls them.
- **Single-pass**: once an item has been yielded, it cannot be
  re-read from the same iter.
- **Fused**: once exhausted, subsequent pulls continue to report
  exhaustion (never panic, never yield again). Re-call `.iter()` on
  the source collection to obtain a fresh iter.
- **Snapshot**: lifting a list/dict/set/string `Rc`-clones the
  backing storage into the iter, so mutating the source after
  `.iter()` does not affect iteration.
- **String iteration**: yields chars (Unicode scalar values), not
  graphemes.
- **Printing**: `log(it)` / `to_string(it)` renders `<iter>` or
  `<iter (exhausted)>` without draining the iter.

### Combinators

Each combinator below is a method on `Iter<T>` and returns a new
`Iter` without consuming items eagerly.

| Method | Signature |
|---|---|
| `.iter()` | `Iter<T> -> Iter<T>` (no-op) |
| `.map(f)` | `Iter<T>, (T) -> U -> Iter<U>` |
| `.filter(p)` | `Iter<T>, (T) -> bool -> Iter<T>` |
| `.flat_map(f)` | `Iter<T>, (T) -> Iter<U> \| list<U> -> Iter<U>` |
| `.take(n)` | `Iter<T>, int -> Iter<T>` |
| `.skip(n)` | `Iter<T>, int -> Iter<T>` |
| `.take_while(p)` | `Iter<T>, (T) -> bool -> Iter<T>` |
| `.skip_while(p)` | `Iter<T>, (T) -> bool -> Iter<T>` |
| `.zip(other)` | `Iter<T>, Iter<U> -> Iter<Pair<T, U>>` |
| `.enumerate()` | `Iter<T> -> Iter<Pair<int, T>>` |
| `.chain(other)` | `Iter<T>, Iter<T> -> Iter<T>` |
| `.chunks(n)` | `Iter<T>, int -> Iter<list<T>>` |
| `.windows(n)` | `Iter<T>, int -> Iter<list<T>>` |

### Sinks

Sinks drive the iter to completion (or until a short-circuit) and
return an eager value.

| Method | Signature |
|---|---|
| `.to_list()` | `Iter<T> -> list<T>` |
| `.to_set()` | `Iter<T> -> set<T>` |
| `.to_dict()` | `Iter<Pair<K, V>> -> dict<K, V>` |
| `.count()` | `Iter<T> -> int` |
| `.sum()` | `Iter<T> -> int \| float` |
| `.min()` | `Iter<T> -> T \| nil` |
| `.max()` | `Iter<T> -> T \| nil` |
| `.reduce(init, f)` | `Iter<T>, U, (U, T) -> U -> U` |
| `.first()` | `Iter<T> -> T \| nil` |
| `.last()` | `Iter<T> -> T \| nil` |
| `.any(p)` | `Iter<T>, (T) -> bool -> bool` |
| `.all(p)` | `Iter<T>, (T) -> bool -> bool` |
| `.find(p)` | `Iter<T>, (T) -> bool -> T \| nil` |
| `.for_each(f)` | `Iter<T>, (T) -> any -> nil` |

### Notes

- `.to_dict()` requires the iter to yield `Pair` values; a runtime
  error is raised otherwise.
- `.min()` / `.max()` return `nil` on an empty iter.
- `.any` / `.all` / `.find` short-circuit as soon as the result is
  determined.
- Numeric ranges (`a to b`, `range(n)`) participate in the lazy iter
  protocol directly; applying any combinator on a `Range` returns a
  lazy `Iter` without materializing the range.

## Method-style builtins

If `obj.method(args)` is called and `obj` is an identifier, the interpreter first checks
for a registered builtin named `"obj.method"`. If found, it is called with just `args`
(not `obj`). This enables namespaced builtins like `experience_bank.save(...)`
and `negative_knowledge.record(...)`.

## Runtime errors

| Error | Description |
|---|---|
| `undefinedVariable(name)` | Variable not found in any scope |
| `undefinedBuiltin(name)` | No registered builtin or user function with this name |
| `immutableAssignment(name)` | Attempted `=` on a `let` binding |
| `typeMismatch(expected, got)` | Type assertion failed |
| `returnValue(value?)` | Internal: used to implement `return` (not a user-facing error) |
| `retryExhausted` | All retry attempts failed |
| `thrownError(value)` | User-thrown error via `throw` |

Most `undefinedBuiltin` errors are now caught statically by the
cross-module typechecker (see [Static cross-module
resolution](#static-cross-module-resolution)) — `harn check` and
`harn run` refuse to start the VM when a file contains a call to a name
that is not a builtin, local declaration, struct constructor, callable
variable, or imported symbol. The runtime check remains as a backstop
for cases where imports could not be resolved at check time.

### Stack traces

Runtime errors include a full call stack trace showing the chain of
function calls that led to the error. The stack trace lists each frame
with its function name, source file, line number, and column:

```text
Error: division by zero
  at divide (script.harn:3:5)
  at compute (script.harn:8:18)
  at default (script.harn:12:10)
```

Stack traces are captured at the point of the error before unwinding, so
they accurately reflect the call chain at the time of failure.

## Persistent store

Six builtins provide a persistent key-value store backed by the resolved Harn
state root (default `.harn/store.json`):

| Function | Description |
|---|---|
| `store_get(key)` | Retrieve value or nil |
| `store_set(key, value)` | Set key, auto-saves to disk |
| `store_delete(key)` | Remove key, auto-saves |
| `store_list()` | List all keys (sorted) |
| `store_save()` | Explicit flush to disk |
| `store_clear()` | Remove all keys, auto-saves |

The store file is created lazily on first mutation. In bridge mode, the
host can override these builtins via the bridge protocol. The state root can
be relocated with `HARN_STATE_DIR`.

## Checkpoint & resume

Checkpoints enable resilient, resumable pipelines. State is persisted to the
resolved Harn state root (default `.harn/checkpoints/<pipeline>.json`) and survives crashes, restarts, and
migration to another machine.

### Core builtins

| Function | Description |
|---|---|
| `checkpoint(key, value)` | Save `value` at `key`; writes to disk immediately |
| `checkpoint_get(key)` | Retrieve saved value, or `nil` if absent |
| `checkpoint_exists(key)` | Return `true` if `key` is present (even if value is `nil`) |
| `checkpoint_delete(key)` | Remove a single key; no-op if absent |
| `checkpoint_clear()` | Remove all checkpoints for this pipeline |
| `checkpoint_list()` | Return sorted list of all checkpoint keys |

`checkpoint_exists` is preferable to `checkpoint_get(key) == nil` when `nil`
is a valid checkpoint value.

### std/checkpoint module

```harn
import { checkpoint_stage, checkpoint_stage_retry } from "std/checkpoint"
```

#### checkpoint_stage(name, fn) -> value

Runs `fn()` and caches the result under `name`. On subsequent calls with the
same name, returns the cached result without running `fn()` again. This is the
primary primitive for building resumable pipelines.

```harn
import { checkpoint_stage } from "std/checkpoint"

fn fetch_dataset(url) { url }
fn clean(data) { data }
fn run_model(cleaned) { cleaned }
fn upload(result) { log(result) }

pipeline process(task) {
  let url = "https://example.com/data.csv"
  let data    = checkpoint_stage("fetch",   fn() { fetch_dataset(url) })
  let cleaned = checkpoint_stage("clean",   fn() { clean(data) })
  let result  = checkpoint_stage("process", fn() { run_model(cleaned) })
  upload(result)
}
```

On first run all three stages execute. On a resumed run (pipeline restarted
after a crash), completed stages are skipped automatically.

#### checkpoint_stage_retry(name, max_retries, fn) -> value

Like `checkpoint_stage`, but retries `fn()` up to `max_retries` times on
failure before propagating the error. Once successful, the result is cached so
retries are never needed on resume.

```harn
import { checkpoint_stage_retry } from "std/checkpoint"

fn fetch_with_timeout(url) { url }

let url = "https://example.com/data.csv"
let data = checkpoint_stage_retry("fetch", 3, fn() { fetch_with_timeout(url) })
log(data)
```

### File location

Checkpoint files are stored at `.harn/checkpoints/<pipeline>.json` relative to
the project root (where `harn.toml` lives), or relative to the source file
directory if no project root is found. Files are plain JSON and can be copied
between machines to migrate pipeline state.

### std/agent_state module

```harn
import "std/agent_state"
```

Provides a durable, session-scoped text/blob store rooted at a
caller-supplied directory.

| Function | Notes |
|---|---|
| `agent_state_init(root, options?)` | Create or reopen a session root under `root/<session_id>/` |
| `agent_state_resume(root, session_id, options?)` | Reopen an existing session; errors when absent |
| `agent_state_write(handle, key, content)` | Atomic temp-write plus rename |
| `agent_state_read(handle, key)` | Returns `string` or `nil` |
| `agent_state_list(handle)` | Deterministic recursive key listing |
| `agent_state_delete(handle, key)` | Deletes a key |
| `agent_state_handoff(handle, summary)` | Writes a JSON handoff envelope to `__handoff.json` |

Keys must be relative paths inside the session root. Absolute paths and
parent-directory escapes are rejected.

## Workspace manifest (`harn.toml`)

Harn projects declare a workspace manifest at the project root named
`harn.toml`. Tooling walks upward from a target `.harn` file looking
for the nearest ancestor manifest and stops at a `.git` boundary so a
stray manifest in a parent project or `$HOME` is never silently picked
up.

### `[check]` — type-checker and preflight

```toml
[check]
host_capabilities_path = "./schemas/host-capabilities.json"
preflight_severity = "warning"          # "error" (default), "warning", "off"
preflight_allow = ["mystery.*", "runtime.task"]

[check.host_capabilities]
project = ["ensure_enriched", "enrich"]
workspace = ["read_text", "write_text"]
```

- `host_capabilities_path` and `[check.host_capabilities]` declare the
  host-call surface that the preflight pass is allowed to assume exists
  at runtime. The CLI flag `--host-capabilities <file>` takes precedence
  for a single invocation. The external file is JSON or TOML with the
  namespaced shape `{ capability: [op, ...], ... }`; nested
  `{ capabilities: { ... } }` wrappers and per-op metadata dictionaries
  are accepted.
- `preflight_severity` downgrades preflight diagnostics to warnings or
  suppresses them entirely. Type-checker and lint diagnostics are
  unaffected — preflight failures are reported under the `preflight`
  category so IDEs and CI filters can route them separately.
- `preflight_allow` suppresses preflight diagnostics tagged with a
  specific host capability. Entries match an exact `capability.operation`
  pair, a `capability.*` wildcard, a bare `capability` name, or a
  blanket `*`.

Preflight capabilities in this section are a **static check surface**
for the Harn type-checker only. They are not the same thing as ACP's
agent/client capability handshake (`agentCapabilities` /
`clientCapabilities`), which is runtime protocol-level negotiation and
lives outside `harn.toml`.

### `[workspace]` — multi-file targets

```toml
[workspace]
pipelines = ["Sources/BurinCore/Resources/pipelines", "scripts"]
```

`harn check --workspace` resolves each path in `pipelines` relative to
the manifest directory and recursively checks every `.harn` file under
each. Positional targets remain additive. The manifest is discovered by
walking upward from the first positional target (or the current working
directory when none is supplied).

### `[[personas]]` — durable agent role manifests

`[[personas]]` entries define durable agent roles in the package/workspace
manifest. Persona v1 is static: tooling parses, validates, lists, and inspects
the contract, while runtime scheduling and handoff execution remain separate
runtime work.

Required fields are `name`, `description`, `entry_workflow`, either `tools` or
`capabilities`, `autonomy_tier`, and `receipt_policy`. Optional fields include
`triggers`, `schedules`, `model_policy`, `budget`, `handoffs`, `context_packs`,
`evals`, `owner`, `version`, `package_source`, and `rollout_policy`.

```toml
[[personas]]
name = "merge_captain"
version = "0.1.0"
description = "Owns pull request readiness, CI triage, merge approvals, and receipts."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github", "ci", "linear", "notion", "slack"]
capabilities = ["git.get_diff", "project.test_commands", "process.exec"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened", "github.check_failed"]
schedules = ["*/30 * * * *"]
handoffs = ["review_captain"]
context_packs = ["repo_policy", "release_rules", "flaky_tests"]
evals = ["merge_safety", "regression_triage", "reviewer_quality"]
budget = { daily_usd = 20.0, frontier_escalations = 3 }
```

Validation rejects missing required fields, malformed or unknown
`capability.operation` entries, invalid cron schedules, unknown handoff targets,
unknown budget/model/source/rollout fields, negative budget amounts, and invalid
rollout percentages. `harn persona list` and `harn persona inspect <name>
--json` expose the resolved schema for hosts such as Harn Cloud and Burin Code.

### `[dependencies]` and `harn.lock` — git-backed package installs

```toml
[dependencies]
harn-openapi = { git = "https://github.com/burin-labs/harn-openapi", rev = "v1.2.3" }
notion-sdk-harn = { git = "https://github.com/burin-labs/notion-sdk-harn", rev = "v1.2.3" }
notion-connector-harn = { git = "https://github.com/burin-labs/notion-connector-harn", rev = "v1.2.3" }
notion = { git = "https://github.com/burin-labs/notion-sdk-harn", rev = "v1.2.3", package = "notion-sdk-harn" }
openapi = { git = "https://github.com/burin-labs/harn-openapi", branch = "main" }
local-fixture = { path = "../fixture-lib" }
```

`[dependencies]` installs package sources into `.harn/packages/` so
imports like `import "notion-sdk-harn"` or `import "notion/providers"`
resolve without filesystem-relative hacks.

- The table key is the local import alias.
- `git` accepts HTTPS, SSH, `file://`, local-repo paths, and GitHub-style
  shorthand URLs.
- Git dependencies must specify either `rev` or `branch`.
- `rev` pins a tag, symbolic ref, or full commit SHA in the manifest.
- `branch` records a moving ref in the manifest, but `harn.lock` still
  pins one resolved commit for reproducible installs.
- `package` documents the upstream package name when the local alias
  differs from the repository name.
- `path` installs a local directory or `.harn` file without using the
  shared git cache. Directory path dependencies are live-linked into
  `.harn/packages/<alias>` when the platform supports symlinks, with a
  copy fallback for restricted filesystems. This makes sibling-repo
  development ergonomic for layouts such as `~/projects/{burin-code,
  harn-cloud,harn-openapi}`: editing `../harn-openapi/lib.harn` is
  immediately visible to imports in the consuming project.

Transitive package dependencies are resolved from installed package
manifests and flattened into the root workspace `.harn/packages/`
directory. For example, a connector package can depend on
`notion-sdk-harn`, and that SDK can depend on `harn-openapi` helpers;
`harn install` records all reachable packages in `harn.lock` and
materializes them from a clean cache. Git-installed packages cannot
declare transitive `path` dependencies, because publishable package
installs must not depend on local sibling directories.

`harn add ../harn-openapi` treats an existing local path as a path
dependency and derives the default alias from that package's
`[package].name` in `harn.toml`, falling back to the directory or file
stem. Use `harn add <alias> --path ../repo` for the legacy explicit
alias form, or `harn add <alias> --git ../repo` when a local git checkout
should be pinned by commit instead of live-linked.

### Package registry index

`harn package search`, `harn package info`, and registry-name
dependencies use a lightweight TOML index. The registry source is chosen
in this order: a command `--registry` flag, `HARN_PACKAGE_REGISTRY`,
`[registry].url` from the nearest manifest, then Harn's default hosted
index URL. Registry URLs may be `https://`, `http://`, `file://`, or a
filesystem path; relative manifest registry paths resolve from the
manifest directory.

Registry package names are either unscoped names such as `acme-lib` or
scoped names such as `@burin/notion-sdk`. Segments must start with an
ASCII alphanumeric character and may then contain ASCII alphanumerics,
`-`, `_`, or `.`. First-party packages should use the `@burin/`
namespace.

Registry entries map discovery names to the existing git-backed package
manager path; they do not introduce a second package install mechanism.
For example, `harn add @burin/notion-sdk@1.2.3` reads the index entry,
writes the equivalent `[dependencies]` git table, updates `harn.lock`,
and materializes the same `.harn/packages/<package>/` tree that a direct
GitHub install would use.

```toml
[registry]
url = "https://packages.harnlang.com/index.toml"
```

Registry index format:

```toml
version = 1

[[package]]
name = "@burin/notion-sdk"
description = "Notion SDK package for Harn connectors"
repository = "https://github.com/burin-labs/notion-sdk-harn"
license = "MIT OR Apache-2.0"
harn = ">=0.7,<0.8"
exports = ["client", "schema"]
connector_contract = "v1"
docs_url = "https://docs.harnlang.com/connectors/notion"
checksum = "sha256:..."
provenance = "https://github.com/burin-labs/notion-sdk-harn/releases/tag/v1.2.3"

[[package.version]]
version = "1.2.3"
git = "https://github.com/burin-labs/notion-sdk-harn"
rev = "v1.2.3"
package = "notion-sdk-harn"
checksum = "sha256:..."
provenance = "https://github.com/burin-labs/notion-sdk-harn/releases/tag/v1.2.3"
```

Package-level metadata includes the registry name, version list,
description, repository, license, Harn compatibility range, exported
modules, connector contract compatibility, docs URL, and optional
checksum/provenance fields. Version entries must specify `git` plus
either `rev` or `branch`; `rev` is preferred for reproducible installs.

Use registry names when developers should discover first-party or
community packages by capability and stable name. Use direct GitHub refs
for local dogfood, private repositories, unreleased commits, or temporary
pins that are not ready for the shared index.

`harn.lock` is a typed TOML file with `version = 1` and one `[[package]]`
entry per dependency. Each git entry records:

- `source`
- `rev_request`
- `commit`
- `content_hash`

`content_hash` is a SHA-256 over the cached package tree. Harn verifies
that hash whenever it reuses a cached package or re-materializes
`.harn/packages/<alias>/`.

For CI and production hosts, `harn install --locked --offline` uses only
the committed `harn.lock` plus the local shared cache; it fails when the
manifest and lockfile disagree or when a locked git package is not
already cached. `harn package cache list`, `clean`, and `verify`
inspect, garbage-collect, and recompute content hashes for cached git
packages. `harn package cache verify --materialized` also verifies
installed `.harn/packages/` contents against the lockfile hashes.

### `[exports]` — stable package module entry points

```toml
[exports]
capabilities = "runtime/capabilities.harn"
providers = "runtime/providers.harn"
```

`[exports]` maps logical import suffixes to package-root-relative module
paths. After `harn install`, consumers import them as
`"<package>/<export>"` instead of coupling to the package's internal
directory layout.

Exports are resolved after the direct `.harn/packages/<path>` lookup, so
packages can still expose raw file trees when they want that behavior.

### `[llm]` — packaged provider extensions

```toml
[llm.providers.my_proxy]
base_url = "https://llm.example.com/v1"
chat_endpoint = "/chat/completions"
completion_endpoint = "/completions"
auth_style = "bearer"
auth_env = "MY_PROXY_API_KEY"
cost_per_1k_in = 0.0002
cost_per_1k_out = 0.0006
latency_p50_ms = 900

[llm.aliases]
my-fast = { id = "vendor/model-fast", provider = "my_proxy" }
```

The `[llm]` table accepts the same schema as `providers.toml`
(`providers`, `aliases`, `inference_rules`, `tier_rules`,
`tier_defaults`, `model_defaults`) but scopes it to the current run.

When Harn starts from a file inside a workspace, it merges:

1. built-in defaults,
2. the global provider file (`HARN_PROVIDERS_CONFIG` or
   `~/.config/harn/providers.toml`),
3. the root project's `[llm]` table.

Installed package manifests do not auto-merge runtime extensions such as
`[llm]`, `[capabilities]`, `[[hooks]]`, or `[[triggers]]` into the host
project. Package code is importable; host runtime configuration remains
root-manifest-owned by default.

### `[lint]` — lint configuration

```toml
[lint]
disabled = ["unused-import"]
require_file_header = false
complexity_threshold = 25
```

- `disabled` silences the listed rules for the whole project.
- `require_file_header` opts into the `require-file-header` rule,
  which checks that each source file begins with a `/** */` HarnDoc
  block whose title matches the filename.
- `complexity_threshold` overrides the default cyclomatic-complexity
  warning threshold (default **25**, chosen to match Clippy's
  `cognitive_complexity` default). Set lower to tighten, higher to
  loosen. Per-function escapes still go through `@complexity(allow)`.

## Sandbox mode

The `harn run` command supports sandbox flags that restrict which builtins
a program may call.

### --deny

```bash
harn run --deny read_file,write_file,exec script.harn
```

Denies the listed builtins. Any call to a denied builtin produces a
runtime error:

```text
Permission denied: builtin 'read_file' is not allowed in sandbox mode
  (use --allow read_file to permit)
```

### --allow

```bash
harn run --allow llm,llm_stream script.harn
```

Allows only the listed builtins plus the core builtins (see below). All
other builtins are denied.

`--deny` and `--allow` cannot be used together; specifying both is an error.

### Core builtins

The following builtins are always allowed, even when using `--allow`:

`println`, `print`, `log`, `type_of`, `to_string`, `to_int`, `to_float`,
`len`, `assert`, `assert_eq`, `assert_ne`, `json_parse`, `json_stringify`,
`runtime_context`, `task_current`, `runtime_context_values`,
`runtime_context_get`, `runtime_context_set`, `runtime_context_clear`

### Propagation

Sandbox restrictions propagate to child VMs created by `spawn`,
`parallel`, and `parallel each`. A child VM inherits the same set of
denied builtins as its parent.

### Handler capability sandbox

When a workflow or handler runs under an active `CapabilityPolicy`,
Harn also enforces `workspace_roots` at runtime for filesystem builtins.
Attempts to read, write, create, copy, stat, list, or delete paths outside
the declared roots fail as typed `tool_rejected` sandbox violations.
Process cwd escapes through `exec_at` / `shell_at` are rejected the same way.

Pure-compute handlers can run through the WASM sandbox entrypoint exposed
by `harn-wasm` as `executePureComponent` and described by
`crates/harn-wasm/wit/harn-pure.wit`. That component surface has no host
imports for filesystem, process, network, clock, random, LLM, or async
effects, so attempted side effects fail inside the component boundary.

Process execution is wrapped in an OS sandbox when Harn can do so for the
current platform. On Linux, Harn installs a seccomp-BPF filter that returns
`EPERM` for denied syscalls and a Landlock filesystem ruleset derived from
the active `CapabilityPolicy`. On OpenBSD, Harn applies `pledge` promises
and `unveil` path permissions derived from the same policy. On macOS, Harn
generates a `sandbox-exec` profile from the active capability policy:
writes are limited to process plumbing locations plus declared
`workspace_roots` only when the policy allows workspace writes, and network
access is allowed only when the policy side-effect ceiling permits
`network`. Unsupported platforms warn once and run the process unsandboxed
by default. Set `HARN_HANDLER_SANDBOX=enforce` to fail closed when no OS
sandbox is available, or `HARN_HANDLER_SANDBOX=off` to disable the process
wrapper.

## Test framework

Harn includes a built-in test runner invoked via `harn test`.

### Running tests

```bash
harn test path/to/tests/         # run all test files in a directory
harn test path/to/test_file.harn # run tests in a single file
```

### Test discovery

The test runner scans `.harn` files for pipelines whose names start with
`test_`. Each such pipeline is executed independently. A test passes if
it completes without error; it fails if it throws or an assertion fails.

```harn
pipeline test_addition() {
  assert_eq(1 + 1, 2)
}

pipeline test_string_concat() {
  let result = "hello" + " " + "world"
  assert_eq(result, "hello world")
}
```

### Assertions

Three assertion builtins are available. They can be called anywhere, but
they are intended for test pipelines and the linter warns on non-test use:

| Function | Description |
|---|---|
| `assert(condition)` | Throws if `condition` is falsy |
| `assert_eq(a, b)` | Throws if `a != b`, showing both values |
| `assert_ne(a, b)` | Throws if `a == b`, showing both values |

### Mock LLM provider

During `harn test`, the `HARN_LLM_PROVIDER` environment variable is
automatically set to `"mock"` unless explicitly overridden. The mock
provider returns deterministic placeholder responses, allowing tests
that call `llm` or `llm_stream` to run without API keys.

### CLI options

| Flag | Description |
|---|---|
| `--filter <pattern>` | Only run tests whose names contain `<pattern>` |
| `--verbose` / `-v` | Show per-test timing and detailed failures |
| `--timing` | Show per-test timing and summary statistics |
| `--timeout <ms>` | Per-test timeout in milliseconds (default 30000) |
| `--parallel` | Run test files concurrently |
| `--junit <path>` | Write JUnit XML report to `<path>` |
| `--record` | Record LLM responses to `.harn-fixtures/` |
| `--replay` | Replay LLM responses from `.harn-fixtures/` |

## Environment variables

The following environment variables configure runtime behavior:

| Variable | Description |
|---|---|
| `HARN_LLM_PROVIDER` | Override the default LLM provider. Any configured provider is accepted. Built-in names include `anthropic` (default), `openai`, `openrouter`, `huggingface`, `ollama`, `local`, and `mock`. |
| `HARN_LLM_TIMEOUT` | LLM request timeout in seconds. Default `120`. |
| `HARN_STATE_DIR` | Override the runtime state root used for store, checkpoint, metadata, and default worktree state. Relative values resolve from the active project/runtime root. |
| `HARN_RUN_DIR` | Override the default persisted run directory. Relative values resolve from the active project/runtime root. |
| `HARN_WORKTREE_DIR` | Override the default worker worktree root. Relative values resolve from the active project/runtime root. |
| `ANTHROPIC_API_KEY` | API key for the Anthropic provider. |
| `OPENAI_API_KEY` | API key for the OpenAI provider. |
| `OPENROUTER_API_KEY` | API key for the OpenRouter provider. |
| `HF_TOKEN` | API key for the HuggingFace provider. |
| `HUGGINGFACE_API_KEY` | Alternate API key name for the HuggingFace provider. |
| `OLLAMA_HOST` | Override the Ollama host. Default `http://localhost:11434`. |
| `LOCAL_LLM_BASE_URL` | Base URL for a local OpenAI-compatible server. Default `http://localhost:8000`. |
| `LOCAL_LLM_MODEL` | Default model ID for the local OpenAI-compatible provider. |

## Known limitations and future work

The following are known limitations in the current implementation that may
be addressed in future versions.

### Type system

- **Definition-site generic checking**: Inside a generic function body,
  type parameters are treated as compatible with any type. The checker
  does not yet restrict method calls on `T` to only those declared in
  the `where` clause interface.
- **No runtime interface enforcement**: Interface satisfaction is checked
  at compile-time only. Passing an untyped value to an interface-typed
  parameter is not caught at runtime.

### Runtime

### Syntax limitations

- **No `impl Interface for Type` syntax**: Interface satisfaction is
  always implicit. There is no way to explicitly declare that a type
  implements an interface.
