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
| `parallel_map` | `.parallelMap` |
| `parallel_settle` | `.parallelSettle` |
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
| `thru` | `.thru` |
| `tool` | `.tool` |
| `upto` | `.upto` |
| `guard` | `.guard` |
| `require` | `.require` |
| `ask` | `.ask` |
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

Note: `parallel_map` is lexed as a single keyword, not an identifier followed by `_map`.

### Number literals

```javascript
int_literal   ::= digit+
float_literal ::= digit+ '.' digit+
```

A number followed by `.` where the next character is not a digit is lexed as an integer
followed by the `.` operator (enabling `42.method`).

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

### Special tokens

| Token | Description |
|---|---|
| `.newline` | Line break character |
| `.eof` | End of input |

## Grammar

The grammar is expressed in EBNF. Newlines between statements are implicit separators
(the parser skips them with `skipNewlines()`). The `consume()` helper also skips newlines
before checking the expected token.

### Top-level

```ebnf
program            ::= (top_level | NEWLINE)*
top_level          ::= import_decl
                     | pipeline_decl
                     | statement

import_decl        ::= 'import' STRING_LITERAL
                     | 'import' '{' IDENTIFIER (',' IDENTIFIER)* '}'
                       'from' STRING_LITERAL

pipeline_decl      ::= ['pub'] 'pipeline' IDENTIFIER '(' param_list ')'
                       ['extends' IDENTIFIER] '{' block '}'

param_list         ::= (IDENTIFIER (',' IDENTIFIER)*)?
block              ::= statement*

fn_decl            ::= ['pub'] 'fn' IDENTIFIER [generic_params]
                       '(' fn_param_list ')' ['->' type_expr]
                       [where_clause] '{' block '}'
type_decl          ::= 'type' IDENTIFIER '=' type_expr
enum_decl          ::= ['pub'] 'enum' IDENTIFIER '{'
                       (enum_variant | ',' | NEWLINE)* '}'
enum_variant       ::= IDENTIFIER ['(' fn_param_list ')']
struct_decl        ::= ['pub'] 'struct' IDENTIFIER '{' struct_field* '}'
struct_field       ::= IDENTIFIER ['?'] ':' type_expr
impl_block         ::= 'impl' IDENTIFIER '{' (fn_decl | NEWLINE)* '}'
interface_decl     ::= 'interface' IDENTIFIER [generic_params] '{'
                       interface_method* '}'
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
                     | parallel_map
                     | parallel_settle
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
match_expr         ::= 'match' expression '{' (expression '->' '{' block '}')* '}'
while_loop         ::= 'while' expression '{' block '}'
retry_block        ::= 'retry' ['(' expression ')'] expression? '{' block '}'
parallel_block     ::= 'parallel' '(' expression ')' '{' [IDENTIFIER '->'] block '}'
parallel_map       ::= 'parallel_map' '(' expression ')' '{' IDENTIFIER '->' block '}'
parallel_settle    ::= 'parallel_settle' '(' expression ')' '{' IDENTIFIER '->' block '}'
return_stmt        ::= 'return' [expression]
throw_stmt         ::= 'throw' expression
override_decl      ::= 'override' IDENTIFIER '(' param_list ')' '{' block '}'
try_catch          ::= 'try' '{' block '}'
                       ['catch' [('(' IDENTIFIER [':' type_expr] ')') | IDENTIFIER]
                         '{' block '}']
                       ['finally' '{' block '}']
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
range_expr         ::= ternary_expr [('thru' | 'upto') ternary_expr]
ternary_expr       ::= logical_or ['?' logical_or ':' logical_or]
logical_or         ::= logical_and ('||' logical_and)*
logical_and        ::= equality ('&&' equality)*
equality           ::= comparison (('==' | '!=') comparison)*
comparison         ::= additive
                       (('<' | '>' | '<=' | '>=' | 'in' | 'not in') additive)*
additive           ::= nil_coal_expr (('+' | '-') nil_coal_expr)*
nil_coal_expr      ::= multiplicative ('??' multiplicative)*
multiplicative     ::= unary (('*' | '/' | '%') unary)*
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
                     | parallel_map
                     | parallel_settle
                     | retry_block
                     | if_else
                     | match_expr
                     | ask_expr
                     | deadline_block
                     | 'spawn' '{' block '}'
                     | 'fn' '(' fn_param_list ')' '{' block '}'
                     | 'try' '{' block '}'

ask_expr           ::= 'ask' '{' (IDENTIFIER ':' expression
                       (',' IDENTIFIER ':' expression)*)? '}'

The `ask` expression is syntactic sugar for an LLM call. It builds a dict from
its key-value fields and passes it to the LLM runtime. Common fields include
`system` (system prompt), `user` (user message), `model`, `max_tokens`, and
`provider`. The expression evaluates to the LLM response string.

```harn
let answer = ask {
  system: "You are a helpful assistant.",
  user: "What is 2 + 2?"
}
println(answer)
```

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
| 9 | `*` `/` | Left | Multiplicative |
| 10 | `!` `-` (unary) | Right (prefix) | Unary |
| 11 | `.` `?.` `[]` `[:]` `()` `?` | Left | Postfix |

### Multiline expressions

Binary operators `||`, `&&`, `+`, `*`, `/`, `%`, `|>` and the `.` member
access operator can span multiple lines. The operator at the start of a
continuation line causes the parser to treat it as a continuation of the
previous expression rather than a new statement.

Note: `-` does not support multiline continuation because it is also a
unary negation prefix.

```harn
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

### Variable assignment

`name = value` walks up the scope chain to find the binding. If the binding is found but was
declared with `let`, throws `HarnRuntimeError.immutableAssignment`. If not found in any scope,
throws `HarnRuntimeError.undefinedVariable`.

### Scope creation

New child scopes are created for:

- Pipeline bodies
- `for` loop bodies (loop variable is mutable)
- `while` loop iterations
- `parallel` and `parallel_map` task bodies (isolated interpreter per task)
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

### List destructuring

```harn
let [first, second, third] = [10, 20, 30]
// first == 10, second == 20, third == 30
```

Elements are bound positionally. If there are more bindings than elements
in the list, the excess bindings receive `nil`.

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

### Var destructuring

`var` destructuring creates mutable bindings that can be reassigned:

```harn
var {x, y} = {x: 1, y: 2}
x = 10
y = 20
```

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
3. `.harn/packages/<path>` directories
4. Package directories with `lib.harn` entry point

Selective imports: `import { name1, name2 } from "module"` imports only
the specified functions. Functions marked `pub` are exported by default;
if no `pub` functions exist, all functions are exported.

Imported pipelines are registered for later invocation.
Non-pipeline top-level statements (fn declarations, let bindings) are executed immediately.

## Runtime values

| Type | Syntax | Description |
|---|---|---|
| `string` | `"text"` | UTF-8 string |
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

### Truthiness

| Value | Truthy? |
|---|---|
| `bool(false)` | No |
| `nil` | No |
| `int(0)` | No |
| `float(0)` | No |
| `string("")` | No |
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
  pattern2 -> { body2 }
}
```

Patterns are expressions. Each pattern is evaluated and compared to the match value
using `valuesEqual`. The first matching arm executes. If no arm matches, the result is `nil`.

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

### parallel

```harn
parallel(count) { i ->
  // body executed count times concurrently
}
```

Creates `count` concurrent tasks. Each task gets an isolated interpreter with a child
environment. The optional variable `i` is bound to the task index (0-based).
Returns a list of results in index order.

### parallel_map

```harn
parallel_map(list) { item ->
  // body for each item
}
```

Maps over a list concurrently. Each task gets an isolated interpreter.
The variable is bound to the current list element.
Returns a list of results in the original order.

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

Parameter types are mapped to JSON Schema types (`string` -> `"string"`,
`int` -> `"integer"`, `float` -> `"number"`, `bool` -> `"boolean"`).
Parameters with default values are emitted as optional schema fields
(`required: false`) and include their `default` value in the generated
tool registry entry.

The result of a `tool` declaration is a tool registry dict (the return
value of `tool_define`). Multiple `tool` declarations accumulate into
separate registries; use `tool_registry()` and `tool_define(...)` for
multi-tool registries.

Like `fn`, `tool` may be prefixed with `pub`.

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
let rest = [2, 3]
add(1, ...rest)        // equivalent to add(1, 2, 3)
```

Multiple spreads are allowed in a single call, and they can appear in any
position:

```harn
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

The type checker warns when a `match` on an enum is not exhaustive (i.e.,
does not cover all variants).

### Built-in Result enum

Harn provides a built-in `Result` enum with two variants:

- `Result.Ok(value)` -- represents a successful result
- `Result.Err(error)` -- represents an error

Shorthand constructor functions `Ok(value)` and `Err(value)` are available
as builtins, equivalent to `Result.Ok(value)` and `Result.Err(value)`.

```harn
let ok = Ok(42)
let err = Err("something failed")

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

No `catch` or `finally` block is needed. If `catch` follows `try`, it is
parsed as a `try`/`catch` statement instead.

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

Structs define named record types with typed fields.

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
```

Fields are declared with `name: type` syntax, one per line.

### Struct construction

Declaring a struct produces a constructor function with the same name as
the struct. The constructor takes a dict argument with the field values:

```harn
let p = Point({x: 3, y: 4})
let u = User({name: "Alice", age: 30})
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
    return Point({x: self.x + dx, y: self.y + dy})
  }
}

let p = Point({x: 3, y: 4})
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
compatible signatures. There is no `implements` keyword.

### Interface declaration

```harn
interface Displayable {
  fn display(self) -> string
}

interface Serializable {
  fn serialize(self) -> string
  fn byte_size(self) -> int
}
```

Each method signature lists parameters (the first must be `self`) and an
optional return type. The body is omitted -- interfaces only declare the
shape of the methods.

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

```harn
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
the constraint produces a compile-time warning.

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

### Union types

```harn
let value: string | nil = nil
let id: int | string = "abc"
```

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

#### `.has()` on shapes

`dict.has("key")` narrows optional shape fields to required:

```harn
fn check(x: {name?: string, age: int}) {
  if x.has("name") {
    log(x)  // x.name is now required (non-optional)
  }
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

The following types are enforced at runtime: `int`, `float`, `string`, `bool`,
`list`, `dict`, `set`, `nil`, and `closure`. `int` and `float` are mutually
compatible (passing an `int` to a `float` parameter is allowed, and vice versa).
Union types are not checked at runtime.

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
| `sha256(str)` | Returns the hex-encoded SHA-256 hash of `str` |
| `md5(str)` | Returns the hex-encoded MD5 hash of `str` |

```harn
let encoded = base64_encode("hello world")  // "aGVsbG8gd29ybGQ="
let decoded = base64_decode(encoded)        // "hello world"
let hash = sha256("hello")                  // hex string
let md5hash = md5("hello")                  // hex string
```

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

Six builtins provide a persistent key-value store backed by `.harn/store.json`:

| Function | Description |
|---|---|
| `store_get(key)` | Retrieve value or nil |
| `store_set(key, value)` | Set key, auto-saves to disk |
| `store_delete(key)` | Remove key, auto-saves |
| `store_list()` | List all keys (sorted) |
| `store_save()` | Explicit flush to disk |
| `store_clear()` | Remove all keys, auto-saves |

The store file is created lazily on first mutation. In bridge mode, the
host can override these builtins via the bridge protocol.

## Checkpoint & resume

Checkpoints enable resilient, resumable pipelines. State is persisted to
`.harn/checkpoints/<pipeline>.json` and survives crashes, restarts, and
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

pipeline process(task) {
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
let data = checkpoint_stage_retry("fetch", 3, fn() { fetch_with_timeout(url) })
```

### File location

Checkpoint files are stored at `.harn/checkpoints/<pipeline>.json` relative to
the project root (where `harn.toml` lives), or relative to the source file
directory if no project root is found. Files are plain JSON and can be copied
between machines to migrate pipeline state.

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
`len`, `assert`, `assert_eq`, `assert_ne`, `json_parse`, `json_stringify`

### Propagation

Sandbox restrictions propagate to child VMs created by `spawn`,
`parallel`, and `parallel_map`. A child VM inherits the same set of
denied builtins as its parent.

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

Three assertion builtins are available (they work outside of tests too):

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

- **Shape validation does not check union types**: If a shape field has a
  union type annotation (`field: string | nil`), runtime validation only
  checks the base type name, not the full union.
- **No runtime generic type checking**: `list<int>` annotations are
  checked at compile-time but not at runtime. A `list<int>` parameter
  accepts any list at runtime.

### Syntax limitations

- **No `impl Interface for Type` syntax**: Interface satisfaction is
  always implicit. There is no way to explicitly declare that a type
  implements an interface.
