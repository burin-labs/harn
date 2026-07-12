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
const total = 1 + 2 \
  + 3 + 4
// equivalent to: const total = 1 + 2 + 3 + 4
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
| `const` | `.constKw` |
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
| `throws` | `.throwsKw` |
| `finally` | `.finally` |
| `fn` | `.fnKw` |
| `emit` | `.emit` |
| `spawn` | `.spawnKw` |
| `while` | `.whileKw` |
| `type` | `.typeKw` |
| `enum` | `.enum` |
| `eval_pack` | `.evalPack` |
| `struct` | `.struct` |
| `interface` | `.interface` |
| `pub` | `.pub` |
| `from` | `.from` |
| `to` | `.to` |
| `tool` | `.tool` |
| `skill` | `.skill` |
| `exclusive` | `.exclusive` |
| `guard` | `.guard` |
| `require` | `.require` |
| `deadline` | `.deadline` |
| `yield` | `.yield` |
| `mutex` | `.mutex` |
| `break` | `.break` |
| `continue` | `.continue` |
| `select` | `.select` |
| `impl` | `.impl` |
| `request_approval` | `.requestApproval` |
| `dual_control` | `.dualControl` |
| `ask_user` | `.askUser` |
| `escalate_to` | `.escalateTo` |

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
const one_day = 1d       // 86400000
const two_weeks = 2w     // 1209600000
```

### String literals

#### Single-line strings

```javascript
string_literal ::= '"' (char | escape | interpolation)* '"'
escape         ::= '\' ('n' | 'r' | 't' | '0' | '\\' | '"' | '$')
interpolation  ::= '${' expression '}'
```

A string cannot span multiple lines. An unescaped newline inside a string is a lexer error.

If the string contains at least one `${...}` interpolation, it produces an
`interpolatedString` token containing a list of segments (literal text and expression
source strings). Otherwise it produces a plain `stringLiteral` token.

Escape sequences: `\n` (newline), `\r` (carriage return), `\t` (tab), `\0` (NUL),
`\\` (backslash), `\"` (double quote), `\$` (dollar sign). Any other character
after `\` produces a literal backslash followed by that character.

#### Raw string literals

```javascript
raw_string_literal ::= 'r"' char* '"'
                     | 'r' '#'+ '"' char* '"' '#'+
```

Raw strings use the `r"..."` prefix. No escape processing or interpolation is
performed inside a raw string -- backslashes, dollar signs, and other characters
are taken literally. Raw strings cannot span multiple lines.

Raw strings are useful for regex patterns and file paths where backslashes are
common:

```harn
const pattern = r"\d+\.\d+"
const path = r"C:\Users\alice\docs"
```

To embed a literal double quote, use the *hashed* form `r#"..."#`. The literal
ends at the first `"` followed by the same number of `#` as the opening
delimiter, so the body may contain any `"` that is not followed by that many
`#`. Add more `#` (`r##"..."##`, etc.) when the body itself contains a `"#`
sequence. This mirrors Rust's raw string syntax and keeps quote-heavy patterns
escape-free:

```harn
// A regex matching a double-quoted string body, with no backslash soup:
const caps = regex_captures(r#""([^"\\]*)""#, "name=\"value\"")
log("first body: ${caps[0].groups[0]}")
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
Use `\${` for a literal `${` sequence inside a multi-line string.

```harn
const name = "world"
const doc = """
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
| `&` | `.amp` | Intersection types |

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

