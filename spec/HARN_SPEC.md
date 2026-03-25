# Harn language specification

Version: 1.0 (derived from implementation, 2026-03-25)

Harn is a pipeline-oriented programming language for orchestrating AI coding agents. It is implemented as a tree-walking interpreter in Swift. Programs consist of named pipelines containing imperative statements, expressions, and calls to registered builtins that perform I/O, LLM calls, and tool execution.

## Lexical rules

### Whitespace

Spaces (`' '`), tabs (`'\t'`), and carriage returns (`'\r'`) are insignificant and skipped between tokens. Newlines (`'\n'`) are significant tokens used as statement separators. The parser skips newlines between statements but they are preserved in the token stream.

### Comments

```
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
| `return` | `.returnKw` |
| `import` | `.importKw` |
| `true` | `.trueKw` |
| `false` | `.falseKw` |
| `nil` | `.nilKw` |
| `try` | `.tryKw` |
| `catch` | `.catchKw` |
| `throw` | `.throwKw` |
| `fn` | `.fnKw` |
| `spawn` | `.spawnKw` |
| `while` | `.whileKw` |

### Identifiers

An identifier starts with a letter or underscore, followed by zero or more letters, digits, or underscores:

```
identifier ::= [a-zA-Z_][a-zA-Z0-9_]*
```

Note: `parallel_map` is lexed as a single keyword, not an identifier followed by `_map`.

### Number literals

```
int_literal   ::= digit+
float_literal ::= digit+ '.' digit+
```

A number followed by `.` where the next character is not a digit is lexed as an integer followed by the `.` operator (enabling `42.method`).

### String literals

#### Single-line strings

```
string_literal ::= '"' (char | escape | interpolation)* '"'
escape         ::= '\' ('n' | 't' | '\\' | '"' | '$')
interpolation  ::= '${' expression '}'
```

A string cannot span multiple lines. An unescaped newline inside a string is a lexer error.

If the string contains at least one `${...}` interpolation, it produces an `interpolatedString` token containing a list of segments (literal text and expression source strings). Otherwise it produces a plain `stringLiteral` token.

Escape sequences: `\n` (newline), `\t` (tab), `\\` (backslash), `\"` (double quote), `\$` (dollar sign). Any other character after `\` produces a literal backslash followed by that character.

#### Multi-line strings

```
multi_line_string ::= '"""' newline? content '"""'
```

Triple-quoted strings can span multiple lines. The optional newline immediately after the opening `"""` is consumed. Common leading whitespace is stripped from all non-empty lines. A trailing newline before the closing `"""` is removed.

Multi-line strings do not support interpolation.

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
| `->` | `.arrow` | Arrow |
| `<=` | `.lte` | Less than or equal |
| `>=` | `.gte` | Greater than or equal |

#### Single-character operators

| Operator | Token | Description |
|---|---|---|
| `=` | `.assign` | Assignment |
| `!` | `.not` | Logical NOT |
| `.` | `.dot` | Member access |
| `+` | `.plus` | Addition / concatenation |
| `-` | `.minus` | Subtraction / negation |
| `*` | `.star` | Multiplication |
| `/` | `.slash` | Division |
| `<` | `.lt` | Less than |
| `>` | `.gt` | Greater than |
| `?` | `.question` | Ternary |

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

The grammar is expressed in EBNF. Newlines between statements are implicit separators (the parser skips them with `skipNewlines()`). The `consume()` helper also skips newlines before checking the expected token.

### Top-level

```ebnf
program       ::= (import_decl | pipeline_decl)*
import_decl   ::= 'import' STRING_LITERAL
pipeline_decl ::= 'pipeline' IDENTIFIER '(' param_list ')' ('extends' IDENTIFIER)? '{' block '}'
param_list    ::= (IDENTIFIER (',' IDENTIFIER)*)?
block         ::= statement*
```

### Statements

```ebnf
statement ::= let_binding
            | var_binding
            | if_else
            | for_in
            | match_expr
            | while_loop
            | retry_block
            | parallel_block
            | parallel_map_block
            | return_stmt
            | throw_stmt
            | override_decl
            | try_catch
            | fn_decl
            | expression_statement

let_binding    ::= 'let' IDENTIFIER '=' expression
var_binding    ::= 'var' IDENTIFIER '=' expression
if_else        ::= 'if' expression '{' block '}' ('else' (if_else | '{' block '}'))?
for_in         ::= 'for' IDENTIFIER 'in' expression '{' block '}'
match_expr     ::= 'match' expression '{' (expression '->' '{' block '}')* '}'
while_loop     ::= 'while' ('(' expression ')' | expression) '{' block '}'
retry_block    ::= 'retry' ('(' expression ')' | primary) '{' block '}'
parallel_block ::= 'parallel' '(' expression ')' '{' (IDENTIFIER '->')? block '}'
parallel_map   ::= 'parallel_map' '(' expression ')' '{' IDENTIFIER '->' block '}'
return_stmt    ::= 'return' expression?
throw_stmt     ::= 'throw' expression
override_decl  ::= 'override' IDENTIFIER '(' param_list ')' '{' block '}'
try_catch      ::= 'try' '{' block '}' 'catch' ('(' IDENTIFIER ')')? '{' block '}'
fn_decl        ::= 'fn' IDENTIFIER '(' param_list ')' '{' block '}'

expression_statement ::= expression ('=' expression)?
```

The `expression_statement` rule handles both bare expressions (function calls, method calls) and assignments. An assignment is recognized when the left-hand side is an identifier followed by `=`.

### Expressions (by precedence, lowest to highest)

```ebnf
expression       ::= pipe_expr
pipe_expr        ::= ternary_expr ('|>' ternary_expr)*
ternary_expr     ::= nil_coal_expr ('?' nil_coal_expr ':' nil_coal_expr)?
nil_coal_expr    ::= logical_or ('??' logical_or)*
logical_or       ::= logical_and ('||' logical_and)*
logical_and      ::= equality ('&&' equality)*
equality         ::= comparison (('==' | '!=') comparison)*
comparison       ::= additive (('<' | '>' | '<=' | '>=') additive)*
additive         ::= multiplicative (('+' | '-') multiplicative)*
multiplicative   ::= unary (('*' | '/') unary)*
unary            ::= ('!' | '-') unary | postfix
postfix          ::= primary (member_access | subscript_access | call)*
member_access    ::= '.' IDENTIFIER ('(' arg_list ')')?
subscript_access ::= '[' expression ']'
call             ::= '(' arg_list ')'    (* only if postfix base is an identifier *)
```

### Primary expressions

```ebnf
primary ::= STRING_LITERAL
          | INTERPOLATED_STRING
          | INT_LITERAL
          | FLOAT_LITERAL
          | 'true' | 'false' | 'nil'
          | IDENTIFIER
          | '(' expression ')'
          | list_literal
          | dict_or_closure
          | 'parallel' '(' expression ')' '{' ... '}'
          | 'parallel_map' '(' expression ')' '{' ... '}'
          | 'retry' ...
          | 'if' ...
          | 'spawn' '{' block '}'

list_literal    ::= '[' (expression (',' expression)*)? ']'
dict_or_closure ::= '{' '}'                              (* empty dict *)
                   | '{' IDENTIFIER '->' block '}'        (* single-param closure *)
                   | '{' IDENTIFIER (',' IDENTIFIER)* '->' block '}'  (* multi-param closure *)
                   | '{' dict_entries '}'                  (* dict literal *)

dict_entries ::= dict_entry (',' dict_entry)*
dict_entry   ::= (IDENTIFIER | '[' expression ']') ':' expression
arg_list     ::= (expression (',' expression)*)?
```

Dict keys written as bare identifiers are converted to string literals (e.g., `{name: "x"}` becomes `{"name": "x"}`). Computed keys use bracket syntax: `{[expr]: value}`.

## Operator precedence table

From lowest to highest binding:

| Precedence | Operators | Associativity | Description |
|---|---|---|---|
| 1 | `\|>` | Left | Pipe |
| 2 | `? :` | Right | Ternary conditional |
| 3 | `??` | Left | Nil coalescing |
| 4 | `\|\|` | Left | Logical OR |
| 5 | `&&` | Left | Logical AND |
| 6 | `==` `!=` | Left | Equality |
| 7 | `<` `>` `<=` `>=` | Left | Comparison |
| 8 | `+` `-` | Left | Additive |
| 9 | `*` `/` | Left | Multiplicative |
| 10 | `!` `-` (unary) | Right (prefix) | Unary |
| 11 | `.` `[]` `()` | Left | Postfix (member, subscript, call) |

## Scope rules

Harn uses lexical scoping with a parent-chain environment model.

### Environment

Each `HarnEnvironment` has:
- A `values` dictionary mapping names to `HarnValue`
- A `mutable` set tracking which names were declared with `var`
- An optional `parent` reference

### Variable lookup

`env.get(name)` checks the current scope's `values` first, then walks up the `parent` chain. Returns `nil` (which becomes `.nilValue`) if not found anywhere.

### Variable definition

- `let name = value` -- defines `name` as immutable in the current scope.
- `var name = value` -- defines `name` as mutable in the current scope.

### Variable assignment

`name = value` walks up the scope chain to find the binding. If the binding is found but was declared with `let`, throws `HarnRuntimeError.immutableAssignment`. If not found in any scope, throws `HarnRuntimeError.undefinedVariable`.

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

## Evaluation order

### Program entry

1. All top-level nodes are scanned. Pipeline declarations are registered by name. Import declarations are processed (loaded and evaluated).
2. The entry pipeline is selected: the pipeline named `"default"` if it exists, otherwise the first pipeline in the file.
3. The entry pipeline's body is executed.

### Pipeline parameters

If the pipeline parameter list includes `task`, it is bound to `context.task`. If it includes `project`, it is bound to `context.projectRoot`. A `context` dict is always injected with keys `task`, `project_root`, and `task_type`.

### Pipeline inheritance

`pipeline child(x) extends parent { ... }`:
- If the child body contains `override` declarations, the resolved body is the parent's body plus any non-override statements from the child. Override declarations are available for lookup by name.
- If the child body contains no `override` declarations, the child body entirely replaces the parent body.

### Statement execution

Statements execute sequentially. The last expression value in a block is the block's result, though this is mostly relevant for closures and parallel bodies.

### Import resolution

`import "path.harn"` loads and parses the file via `HarnLoader`, which searches:
1. `<projectRoot>/.burin/pipelines/<name>.harn`
2. Bundle resources

Imported pipelines are registered for later invocation. Non-pipeline top-level statements (fn declarations, let bindings) are executed immediately.

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
| `closure` | `{ x -> x + 1 }` | First-class function with captured environment |
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
| Everything else | Yes |

### Equality

Values are equal if they have the same type and same contents, with these exceptions:
- `int` and `float` are compared by converting `int` to `float`
- Two closures are never equal
- Two task handles are equal if their IDs match

### Comparison

Only `int`, `float`, and `string` support ordering (`<`, `>`, `<=`, `>=`). Comparison between other types returns 0 (equal).

## Binary operator semantics

### Arithmetic (`+`, `-`, `*`, `/`)

| Left | Right | `+` | `-` | `*` | `/` |
|---|---|---|---|---|---|
| int | int | int | int | int | int (truncating) |
| float | float | float | float | float | float |
| string | any | string (concatenation) | nil | nil | nil |
| list | list | list (concatenation) | nil | nil | nil |
| other | other | string (both `.asString` concatenated) | nil | nil | nil |

Division by zero returns `nil`.

### Logical (`&&`, `||`)

Short-circuit evaluation:
- `&&`: if left is falsy, returns `false` without evaluating right.
- `||`: if left is truthy, returns `true` without evaluating right.

### Nil coalescing (`??`)

Short-circuit: if left is not `nil`, returns left without evaluating right.

### Pipe (`|>`)

`a |> f` evaluates `a`, then:
1. If `f` evaluates to a closure, invokes it with `a` as the single argument.
2. If `f` is an identifier resolving to a builtin, calls the builtin with `[a]`.
3. If `f` is an identifier resolving to a closure variable, invokes it with `a`.
4. Otherwise returns `nil`.

### Ternary (`? :`)

`condition ? trueExpr : falseExpr` evaluates `condition`, then evaluates and returns either `trueExpr` (if truthy) or `falseExpr`.

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

If `iterable` is a list, iterates over elements. If `iterable` is a dict, iterates over entries sorted by key, where each entry is `{key: "...", value: ...}`. The loop variable is mutable within the loop body.

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

Patterns are expressions. Each pattern is evaluated and compared to the match value using `valuesEqual`. The first matching arm executes. If no arm matches, the result is `nil`.

### retry

```harn
retry 3 {
  // body that may throw
}
```

Executes the body up to N times. If the body succeeds (no error), returns immediately. If the body throws, catches the error and retries. `return` statements inside retry propagate out (are not retried). After all attempts are exhausted, returns `nil` (does not re-throw the last error).

## Concurrency

### parallel

```harn
parallel(count) { i ->
  // body executed count times concurrently
}
```

Creates `count` concurrent tasks. Each task gets an isolated interpreter with a child environment. The optional variable `i` is bound to the task index (0-based). Returns a list of results in index order.

### parallel_map

```harn
parallel_map(list) { item ->
  // body for each item
}
```

Maps over a list concurrently. Each task gets an isolated interpreter. The variable is bound to the current list element. Returns a list of results in the original order.

### spawn/await/cancel

```harn
let handle = spawn {
  // async body
}
let result = await(handle)
cancel(handle)
```

`spawn` launches an async task and returns a `taskHandle`. `await` (a built-in interpreter function, not a keyword) blocks until the task completes and returns its result. `cancel` cancels the task.

## Error model

### throw

```harn
throw expression
```

Evaluates the expression and throws it as `HarnRuntimeError.thrownError(value)`. Any value can be thrown (strings, dicts, etc.).

### try/catch

```harn
try {
  // body
} catch (e) {
  // handler
}
```

If the body throws:
- A `thrownError(value)`: `e` is bound to the thrown value directly.
- Any other runtime error: `e` is bound to the error's `localizedDescription` string.

`return` inside a `try` block propagates out of the enclosing pipeline (is not caught).

The error variable `(e)` is optional: `catch { ... }` is valid without it.

## Functions and closures

### fn declarations

```harn
fn name(param1, param2) {
  return param1 + param2
}
```

Declares a named function. Equivalent to `let name = { param1, param2 -> ... }`. The function captures the lexical scope at definition time.

### Closures

```harn
let f = { x -> x * 2 }
let g = { a, b -> a + b }
```

First-class values. When invoked, a child environment is created from the *captured* environment (not the call-site environment), and parameters are bound as immutable bindings.

### Return

`return value` inside a function/closure unwinds execution via `HarnRuntimeError.returnValue`. The closure invocation catches this and returns the value. `return` inside a pipeline terminates the pipeline.

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

## Method-style builtins

If `obj.method(args)` is called and `obj` is an identifier, the interpreter first checks for a registered builtin named `"obj.method"`. If found, it is called with just `args` (not `obj`). This enables namespaced builtins like `experience_bank.save(...)` and `negative_knowledge.record(...)`.

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
