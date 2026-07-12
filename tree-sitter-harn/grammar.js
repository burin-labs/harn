/// <reference types="tree-sitter-cli/dsl" />

const KEYWORD_IDENTIFIERS = require("./grammar/keywords");

module.exports = grammar({
  name: "harn",

  extras: ($) => [/[ \t\r]/, /\\\r?\n[ \t]*/, $.comment],

  externals: ($) => [$._block_sep, $._line_sep],

  conflicts: ($) => [
    [$.dict_literal, $.shape_type],
    // `{ ...x }` is ambiguous between a value-level spread inside a
    // dict/closure (`{...a}`, a closure rest param) and a row-tail in a
    // row-polymorphic shape type (`{ ...R }`). GLR resolves it from the
    // surrounding context (annotation position vs. value position), the
    // same way `dict_literal`/`shape_type` is already disambiguated above.
    [$._primary, $.typed_parameter, $.type_annotation],
    [$._statement, $._expression],
    [$._primary, $.type_annotation],
    [$._primary, $.typed_parameter],
    [$.typed_parameter, $.shape_field],
    [$.parallel_expression],
    [$.typed_parameter, $.type_annotation],
    [$.type_arguments, $.type_annotation],
    [$.block],
    [$.closure],
    [$.select_block],
    [$.parallel_each_expression],
    [$.parallel_settle_expression],
    [$._primary, $.struct_construct],
    [$.struct_declaration],
    [$.tool_declaration],
    [$.impl_block],
    [$.interface_declaration],
    [$.match_statement],
    [$.skill_declaration, $.pipe_expression, $.binary_expression, $.method_call, $.property_access],
    [$.skill_declaration, $.pipe_expression, $.nil_coalescing_expression, $.binary_expression, $.method_call, $.property_access],
    [$.eval_pack_declaration, $.pipe_expression, $.binary_expression, $.method_call, $.property_access],
    [$.eval_pack_declaration, $.pipe_expression, $.nil_coalescing_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.binary_expression, $.unary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.nil_coalescing_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.ternary_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.range_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.nil_coalescing_expression, $.binary_expression, $.range_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.ternary_expression, $.nil_coalescing_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.nil_coalescing_expression, $.binary_expression, $.unary_expression, $.method_call, $.property_access],
  ],

  word: ($) => $.identifier,

  rules: {
    source_file: ($) => topLevelSeparated($, $._top_level_item),

    _top_level_item: ($) =>
      choice(
        $.attributed_declaration,
        $.pipeline_declaration,
        $.import_declaration,
        $._statement
      ),

    // Attributes: `@name` or `@name(arg, key: value)`. Stack on the
    // declaration that follows. Mirrors the parser in
    // crates/harn-parser/src/parser.rs (parse_attributed_decl), whose
    // `skip_newlines()` swallows any run of newlines between an attribute and
    // the declaration it decorates.
    //
    // `repeat($._line_sep)` (not `optional`) is required because a comment
    // between the attribute and the declaration (e.g. a doc comment:
    // `@complexity(allow)` / `/** ... */` / `pub fn`) is lexed as an `extras`
    // node sandwiched between TWO separators — the newline after the attribute
    // and the newline after the comment. `optional` only consumed the first,
    // leaving the second to error out before the declaration. The canonical
    // lexer treats comments as trivia, so this construct is valid Harn.
    attributed_declaration: ($) =>
      seq(
        repeat1(seq($.attribute, repeat($._line_sep))),
        choice(
          $.pipeline_declaration,
          $.fn_declaration,
          $.tool_declaration,
          $.skill_declaration,
          $.eval_pack_declaration,
          $.struct_declaration,
          $.enum_declaration,
          $.type_declaration,
          $.interface_declaration,
          $.impl_block
        )
      ),

    attribute: ($) =>
      seq(
        "@",
        field("name", attributeIdentifier($)),
        optional(seq(
          "(",
          optional(commaSep1($.attribute_arg)),
          optional(","),
          ")"
        ))
      ),

    attribute_arg: ($) =>
      choice(
        seq(field("name", attributeIdentifier($)), ":", field("value", $._attribute_value)),
        $._attribute_value
      ),

    // Attribute values mirror the deliberately non-evaluating subset
    // accepted by `parse_attribute_value` in the runtime parser
    // (crates/harn-parser/src/parser/decls.rs): scalar literals, lists,
    // dicts, dotted identifiers used as sentinels (e.g.
    // `github.pr_opened`), and simple call-shaped sentinels such as
    // `schedule("*/30 * * * *")`. Negative integers are also allowed
    // as a single attribute scalar.
    _attribute_value: ($) =>
      choice(
        $.string_literal,
        $.raw_string_literal,
        $.integer_literal,
        $.float_literal,
        $.true,
        $.false,
        $.nil,
        $.list_literal,
        $.dict_literal,
        $.attribute_call,
        $.attribute_path,
        $.identifier,
        seq("-", $.integer_literal)
      ),

    // Dotted-identifier sentinel used in attribute values, e.g.
    // `github.pr_opened`. This intentionally does not extend into
    // `property_access` (which carries _expression on the LHS) because
    // attribute values are syntactically restricted to the literal
    // grammar above.
    attribute_path: ($) =>
      seq($.identifier, repeat1(seq(".", $.identifier))),

    // Call-shaped sentinel used in attribute values, e.g.
    // `schedule("*/30 * * * *")`. Uses a recursive subset of attribute
    // values for arguments to keep the surface aligned with the runtime
    // parser's `parse_attribute_value` recursion.
    attribute_call: ($) =>
      seq(
        choice($.attribute_path, $.identifier),
        "(",
        optional(commaSep1($.attribute_arg)),
        optional(","),
        ")"
      ),

    _newline: (_) => "\n",

    // --- Comments ---

    comment: (_) =>
      token(
        choice(
          seq("//", /[^\n]*/),
          // Shebang line. Lexer only honors at offset 0, but for editor
          // highlighting we accept it anywhere (false positives are harmless).
          seq("#!", /[^\n]*/),
          // Block comments, supports one level of nesting
          seq(
            "/*",
            /[^*]*\*+([^/*][^*]*\*+)*/,
            "/"
          ),
          seq(
            "/*",
            repeat(choice(
              /[^/*]/,
              seq("/", /[^*]/),
              seq("*", /[^/]/),
              seq("/*", /[^*]*\*+([^/*][^*]*\*+)*/, "/")
            )),
            "*/"
          )
        )
      ),

    // --- Top-level declarations ---

    pipeline_declaration: ($) =>
      seq(
        optional("pub"),
        "pipeline",
        field("name", $.identifier),
        "(",
        parameterList($),
        ")",
        optional(seq("->", field("return_type", $.type_annotation))),
        optional(seq("throws", field("throws", $.type_annotation))),
        optional(seq("extends", field("parent", $.identifier))),
        $.block
      ),

    import_declaration: ($) =>
      choice(
        seq(optional("pub"), "import", $.string_literal),
        seq(
          optional("pub"),
          "import",
          "{",
          commaSep1($.identifier),
          optional(","),
          "}",
          "from",
          $.string_literal
        ),
        seq(
          optional("pub"),
          "import",
          "{",
          $._newline,
          $.identifier,
          repeat(seq(",", $._newline, $.identifier)),
          optional(seq(",", $._newline)),
          "}",
          "from",
          $.string_literal
        ),
        seq(
          optional("pub"),
          "import",
          field("path", $.scoped_import_path),
          "::",
          "{",
          commaSep1($.identifier),
          optional(","),
          "}"
        )
      ),

    scoped_import_path: ($) =>
      token(/[a-zA-Z_][a-zA-Z0-9_]*(::[a-zA-Z_][a-zA-Z0-9_]*)+/),

    enum_declaration: ($) =>
      seq(
        optional("pub"),
        "enum",
        field("name", $.identifier),
        optional($.generic_params),
        "{",
        repeat(choice($.enum_variant, ",", $._newline)),
        "}"
      ),

    enum_variant: ($) =>
      seq(
        field("name", $.identifier),
        optional(seq("(", parameterList($), ")"))
      ),

    struct_declaration: ($) =>
      seq(
        optional("pub"),
        "struct",
        field("name", $.identifier),
        optional($.generic_params),
        "{",
        layoutSeparated($, $.struct_field),
        "}"
      ),

    struct_field: ($) =>
      seq(
        field("name", $.identifier),
        optional("?"),
        ":",
        field("type", $.type_annotation)
      ),

    impl_block: ($) =>
      seq(
        "impl",
        field("type_name", $.identifier),
        "{",
        layoutSeparated($, $.fn_declaration),
        "}"
      ),

    // --- Statements ---

    _statement: ($) =>
      choice(
        $.let_binding,
        $.const_binding,
        $.var_binding,
        $.if_statement,
        $.for_statement,
        $.while_statement,
        $.match_statement,
        $.retry_statement,
        $.cost_route_block,
        $.return_statement,
        $.throw_statement,
        $.fn_declaration,
        $.tool_declaration,
        $.skill_declaration,
        $.eval_pack_declaration,
        $.override_declaration,
        $.enum_declaration,
        $.struct_declaration,
        $.impl_block,
        $.interface_declaration,
        $.type_declaration,
        $.parallel_expression,
        $.parallel_each_expression,
        $.parallel_settle_expression,
        $.defer_statement,
        $.deadline_block,
        $.guard_statement,
        $.require_statement,
        $.mutex_block,
        $.scope_block,
        $.select_block,
        $.break_statement,
        $.continue_statement,
        $.yield_expression,
        $.emit_expression,
        $.assignment,
        $.compound_assignment,
        $.expression_statement
      ),

    expression_statement: ($) => prec(1, $._expression),

    let_binding: ($) =>
      seq(
        optional("pub"),
        "let",
        field("name", $._binding_pattern),
        optional(seq(":", field("type", $.type_annotation))),
        "=",
        field("value", choice($.struct_construct, $._expression))
      ),

    var_binding: ($) =>
      seq(
        "var",
        field("name", $._binding_pattern),
        optional(seq(":", field("type", $.type_annotation))),
        "=",
        field("value", choice($.struct_construct, $._expression))
      ),

    // Immutable binding (`const PATTERN [: Type] = EXPR`). The default,
    // immutable binding form; when the initializer is in the pure const-eval
    // subset over a plain identifier it is folded at compile time. A
    // destructuring pattern is permitted (only identifier bindings fold).
    const_binding: ($) =>
      seq(
        optional("pub"),
        "const",
        field("name", $._binding_pattern),
        optional(seq(":", field("type", $.type_annotation))),
        "=",
        field("value", choice($.struct_construct, $._expression))
      ),

    _binding_pattern: ($) =>
      choice(
        $.identifier,
        $.dict_pattern,
        $.list_pattern,
        $.pair_pattern
      ),

    pair_pattern: ($) =>
      seq("(", $.identifier, ",", $.identifier, ")"),

    dict_pattern: ($) =>
      seq("{", optional(commaSep1($.dict_pattern_field)), "}"),

    dict_pattern_field: ($) =>
      choice(
        seq("...", $.identifier),
        seq($.identifier, optional(seq(":", $.identifier)), optional(seq("=", $._expression)))
      ),

    list_pattern: ($) =>
      seq("[", optional(commaSep1($.list_pattern_element)), "]"),

    list_pattern_element: ($) =>
      choice(
        seq("...", $.identifier),
        seq($.identifier, optional(seq("=", $._expression)))
      ),

    assignment: ($) =>
      prec.right(seq(
        field("target", choice(
          $.identifier,
          $.property_access,
          $.subscript_expression
        )),
        "=",
        field("value", $._expression)
      )),

    compound_assignment: ($) =>
      prec.right(seq(
        field("target", choice(
          $.identifier,
          $.property_access,
          $.subscript_expression
        )),
        field("operator", choice("+=", "-=", "*=", "/=", "%=")),
        field("value", $._expression)
      )),

    if_statement: ($) =>
      prec.right(seq(
        "if",
        field("condition", $._expression),
        field("consequence", $.block),
        optional(seq("else", choice($.if_statement, $.block)))
      )),

    for_statement: ($) =>
      seq(
        "for",
        field("variable", $._binding_pattern),
        "in",
        field("iterable", $._expression),
        field("body", $.block)
      ),

    while_statement: ($) =>
      seq("while", field("condition", $._expression), field("body", $.block)),

    match_statement: ($) =>
      seq(
        "match",
        field("value", $._expression),
        "{",
        layoutSeparated($, $.match_arm),
        "}"
      ),

    match_arm: ($) =>
      seq(
        field("pattern", $._match_pattern),
        optional(seq("if", field("guard", $._expression))),
        "->",
        field("body", $.block)
      ),

    // An arm pattern is either a single expression or an or-pattern
    // made of two-plus `|`-separated alternatives. Keeping this as a
    // dedicated rule (rather than inlining `|` in `match_arm`) gives
    // tooling a stable anchor to walk when synthesising new arms.
    _match_pattern: ($) => choice($._expression, $.or_pattern),

    or_pattern: ($) =>
      prec.right(
        seq($._expression, "|", $._expression, repeat(seq("|", $._expression)))
      ),

    retry_statement: ($) =>
      seq("retry", field("count", $._expression), field("body", $.block)),

    cost_route_block: ($) =>
      seq(
        "cost_route",
        "{",
        statementSeparated($, choice($.cost_route_option, $._statement)),
        "}"
      ),

    cost_route_option: ($) =>
      seq(field("name", $.identifier), ":", field("value", $._expression)),

    return_statement: ($) => prec.right(seq("return", optional($._expression))),

    throw_statement: ($) => seq("throw", $._expression),

    break_statement: (_) => "break",

    continue_statement: (_) => "continue",

    deadline_block: ($) =>
      seq("deadline", field("duration", $._expression), field("body", $.block)),

    guard_statement: ($) =>
      seq("guard", field("condition", $._expression), "else", field("else_body", $.block)),

    require_statement: ($) =>
      seq(
        "require",
        field("condition", $._expression),
        optional(seq(",", field("message", $._expression)))
      ),

    mutex_block: ($) =>
      seq(
        "mutex",
        // Optional resource key: `mutex(resource) { ... }`. A bare
        // `mutex { ... }` keys on its lexical call-site instead.
        optional(seq("(", field("key", $._expression), ")")),
        field("body", $.block)
      ),

    // Structured-concurrency nursery: `scope { ... }` joins every task
    // spawned inside it before the block exits. `scope` is a contextual
    // keyword in the Rust parser; mirror it here as a block construct.
    scope_block: ($) =>
      seq("scope", field("body", $.block)),

    select_block: ($) =>
      seq(
        "select",
        "{",
        layoutSeparated($, choice($.select_case, $.select_timeout, $.select_default)),
        "}"
      ),

    select_case: ($) =>
      seq(
        field("variable", $.identifier),
        "from",
        field("channel", $._expression),
        field("body", $.block)
      ),

    select_timeout: ($) =>
      seq("timeout", field("duration", $._expression), field("body", $.block)),

    select_default: ($) =>
      seq("default", field("body", $.block)),

    yield_expression: ($) =>
      prec.right(seq("yield", optional($._expression))),

    emit_expression: ($) =>
      prec.right(seq("emit", $._expression)),

    fn_declaration: ($) =>
      seq(
        optional("pub"),
        optional("gen"),
        "fn",
        field("name", $.identifier),
        optional($.generic_params),
        "(",
        parameterList($),
        ")",
        optional(seq("->", $.type_annotation)),
        optional(seq("throws", field("throws", $.type_annotation))),
        optional($.where_clause),
        field("body", $.block)
      ),

    tool_declaration: ($) =>
      seq(
        optional("pub"),
        "tool",
        field("name", $.identifier),
        "(",
        parameterList($),
        ")",
        optional(seq("->", $.type_annotation)),
        optional(seq("throws", field("throws", $.type_annotation))),
        "{",
        statementSeparated($, choice($.tool_description, $._statement)),
        "}"
      ),

    tool_description: ($) =>
      seq("description", $.string_literal),

    skill_declaration: ($) =>
      seq(
        optional("pub"),
        "skill",
        field("name", $.identifier),
        "{",
        statementSeparated(
          $,
          seq(
            field("field_name", $.identifier),
            field("field_value", $._expression)
          )
        ),
        "}"
      ),

    eval_pack_declaration: ($) =>
      seq(
        optional("pub"),
        "eval_pack",
        choice(
          seq(field("name", $.identifier), optional(field("id", $.string_literal))),
          field("id", $.string_literal)
        ),
        "{",
        statementSeparated(
          $,
          choice(
            seq(field("field_name", $.identifier), ":", field("field_value", $._expression)),
            seq("summarize", $.block),
            $._statement
          )
        ),
        "}"
      ),

    generic_params: ($) =>
      seq("<", commaSep1($.generic_param), ">"),

    generic_param: ($) =>
      seq(
        optional(field("variance", choice("in", "out"))),
        $.identifier
      ),

    // Type-argument lists in generic call expressions. The leading `<`
    // uses `token.immediate` so it must directly follow the function
    // expression without intervening whitespace — this is how generic
    // calls are written in real Harn corpora (`fold<int,int>(...)`,
    // never `fold <int,int>(...)`) and serves as a practical
    // disambiguator from binary `<` comparisons such as `i < 5`. The
    // runtime parser is more permissive (it backtracks on mismatch
    // instead of relying on whitespace), so this is a deliberate
    // tree-sitter-only restriction. Type-annotation generic forms
    // (`Foo<T>`, `List<int>`) have their own rule and are unaffected.
    type_arguments: ($) =>
      seq(alias(token.immediate("<"), "<"), commaSep1($.type_annotation), ">"),

    where_clause: ($) =>
      seq(
        "where",
        commaSep1(seq(
          $.identifier,
          ":",
          $.type_annotation,
          repeat(seq("+", $.type_annotation))
        ))
      ),

    interface_declaration: ($) =>
      seq(
        "interface",
        field("name", $.identifier),
        optional($.generic_params),
        "{",
        layoutSeparated($, choice($.associated_type_declaration, $.interface_method)),
        "}"
      ),

    associated_type_declaration: ($) =>
      seq(
        "type",
        field("name", $.identifier),
        optional(seq("=", field("default", $.type_annotation)))
      ),

    type_declaration: ($) =>
      seq(
        optional("pub"),
        "type",
        field("name", $.identifier),
        optional($.generic_params),
        "=",
        field("type", $.type_annotation)
      ),

    interface_method: ($) =>
      seq(
        "fn",
        field("name", $.identifier),
        optional($.generic_params),
        "(",
        parameterList($),
        ")",
        optional(seq("->", $.type_annotation))
      ),

    override_declaration: ($) =>
      seq(
        "override",
        field("name", $.identifier),
        "(",
        parameterList($),
        ")",
        field("body", $.block)
      ),

    // --- Expressions (precedence climbing) ---

    _expression: ($) =>
      choice(
        $.pipe_expression,
        $.ternary_expression,
        $.nil_coalescing_expression,
        $.binary_expression,
        $.range_expression,
        $.unary_expression,
        $.call_expression,
        $.generic_call_expression,
        $.hitl_expression,
        $.method_call,
        $.subscript_expression,
        $.property_access,
        $.slice_expression,
        $.try_unwrap_expression,
        $.parenthesized_expression,
        $.spawn_expression,
        $.try_expression,
        $.try_star_expression,
        $.deadline_block,
        $.parallel_expression,
        $.parallel_each_expression,
        $.parallel_settle_expression,
        $.if_statement,
        $.retry_statement,
        $.cost_route_block,
        $.match_statement,
        $._primary
      ),

    pipe_expression: ($) =>
      prec.left(1, seq($._expression, repeat($._newline), "|>", repeat($._newline), $._expression)),

    ternary_expression: ($) =>
      prec.right(2, seq($._expression, "?", $._expression, ":", $._expression)),

    // `??` binds tighter than `+ -` but looser than `* / %`, matching the
    // canonical parser (crates/harn-parser parse_nil_coalescing). Keeping the
    // tree-sitter precedence in sync ensures structural tools see the same
    // grouping the interpreter does, e.g. `a ?? b + c` is `(a ?? b) + c` and
    // `a ?? b * c` is `a ?? (b * c)`.
    nil_coalescing_expression: ($) =>
      prec.left(9, seq($._expression, repeat($._newline), "??", repeat($._newline), $._expression)),

    binary_expression: ($) =>
      choice(
        prec.left(4, seq($._expression, repeat($._newline), "||", repeat($._newline), $._expression)),
        prec.left(5, seq($._expression, repeat($._newline), "&&", repeat($._newline), $._expression)),
        prec.left(6, seq($._expression, repeat($._newline), choice("==", "!="), repeat($._newline), $._expression)),
        prec.left(7, seq($._expression, repeat($._newline), choice("<", ">", "<=", ">="), repeat($._newline), $._expression)),
        prec.left(7, seq($._expression, "in", $._expression)),
        prec.left(7, seq($._expression, "not", "in", $._expression)),
        prec.left(8, seq($._expression, repeat($._newline), "+", repeat($._newline), $._expression)),
        prec.left(8, seq($._expression, "-", $._expression)),
        prec.left(10, seq($._expression, repeat($._newline), "*", repeat($._newline), $._expression)),
        prec.left(10, seq($._expression, repeat($._newline), "/", repeat($._newline), $._expression)),
        prec.left(10, seq($._expression, repeat($._newline), "%", repeat($._newline), $._expression)),
        // `**` binds tighter than a unary prefix on its left operand, so
        // `-2 ** 2` is `-(2 ** 2)` (see unary_expression below at prec 11).
        prec.right(12, seq($._expression, repeat($._newline), "**", repeat($._newline), $._expression))
      ),

    range_expression: ($) =>
      prec.left(7, seq($._expression, "to", $._expression, optional("exclusive"))),

    // Unary prefix binds looser than `**` (prec 12) so `-2 ** 2` parses as
    // `-(2 ** 2)`, and tighter than `* / %` (prec 10) so `-a * b` is `(-a) * b`.
    unary_expression: ($) =>
      prec.right(11, seq(choice("!", "-"), $._expression)),

    call_expression: ($) =>
      prec.left(
        13,
        seq(
          field("function", choice(
            $.identifier,
            $.property_access,
            $.subscript_expression,
            $.parenthesized_expression
          )),
          "(",
          repeat(lineBreak($)),
          optional($.argument_list),
          repeat(lineBreak($)),
          ")"
        )
      ),

    // Generic-call form `func<T, U>(args)`. Kept distinct from
    // `call_expression` so that adding generic-type arguments to a callable
    // does not statically out-prioritise binary `<` comparisons whose LHS
    // happens to be a callable expression (`i < 5`, `obj.x < 5`, `(i) < 5`).
    // The leading `<` is captured by the `type_arguments` rule via
    // `token.immediate("<")`, which means the generic-call form requires
    // the `<` to immediately follow the function with no whitespace —
    // matching how generic calls are written in real Harn corpora.
    generic_call_expression: ($) =>
      prec.left(
        13,
        seq(
          field("function", choice(
            $.identifier,
            $.property_access,
            $.subscript_expression,
            $.parenthesized_expression
          )),
          field("type_arguments", $.type_arguments),
          "(",
          repeat(lineBreak($)),
          optional($.argument_list),
          repeat(lineBreak($)),
          ")"
        )
      ),

    // First-class HITL primitives: `ask_user(...)`, `dual_control(...)`,
    // `escalate_to(...)`, `request_approval(...)`. Each is a reserved
    // keyword in the runtime lexer and accepts both positional and
    // named arguments — matching the dispatch in
    // `parse_hitl_expr` in crates/harn-parser/src/parser/expressions.rs.
    hitl_expression: ($) =>
      prec.left(
        13,
        seq(
          field("primitive", choice(
            "ask_user",
            "dual_control",
            "escalate_to",
            "request_approval"
          )),
          "(",
          repeat(lineBreak($)),
          optional(seq(
            $.hitl_arg,
            repeat(seq(",", repeat(lineBreak($)), $.hitl_arg)),
            optional(seq(",", repeat(lineBreak($))))
          )),
          ")"
        )
      ),

    hitl_arg: ($) =>
      choice(
        seq(field("name", $.identifier), ":", field("value", $._expression)),
        $._expression
      ),

    method_call: ($) =>
      prec.left(
        13,
        seq(
          field("object", $._expression),
          repeat($._newline),
          choice(".", "?."),
          field("method", choice($.identifier, $.keyword_identifier)),
          "(",
          repeat(lineBreak($)),
          optional($.argument_list),
          repeat(lineBreak($)),
          ")"
        )
      ),

    property_access: ($) =>
      prec.left(
        13,
        seq(
          field("object", $._expression),
          repeat($._newline),
          choice(".", "?."),
          field("property", choice($.identifier, $.keyword_identifier))
        )
      ),

    subscript_expression: ($) =>
      prec.left(
        13,
        seq(
          field("object", $._expression),
          choice("[", "?[", "?.["),
          $._expression,
          "]"
        )
      ),

    slice_expression: ($) =>
      prec.left(
        13,
        seq(
          field("object", $._expression),
          "[",
          optional(field("start", $._expression)),
          ":",
          optional(field("end", $._expression)),
          "]"
        )
      ),

    try_unwrap_expression: ($) =>
      prec.left(13, seq($._expression, token.immediate("?"))),

    parenthesized_expression: ($) => seq("(", $._expression, ")"),

    spawn_expression: ($) => seq("spawn", $.block),

    try_expression: ($) =>
      prec.right(
        seq(
          "try",
          field("body", $.block),
          optional(
            seq(
              "catch",
              optional(choice(
                seq(
                  "(",
                  field("error_var", $.identifier),
                  optional(seq(":", field("error_type", $.type_annotation))),
                  ")"
                ),
                field("error_var", $.identifier)
              )),
              field("handler", $.block)
            )
          ),
          optional(seq("finally", field("finalizer", $.block)))
        )
      ),

    // try* EXPR — rethrow-into-catch operator (issue #26). Distinct from
    // try { ... } / try { ... } catch above; binds tighter than `try`'s
    // block form because the operand is an expression, not a block.
    // Whitespace between `try` and `*` is allowed but not required —
    // matching the runtime parser, which treats `try*` as a token pair.
    try_star_expression: ($) =>
      prec.right(20, seq("try", "*", field("operand", $._expression))),

    parallel_options: ($) =>
      seq(
        "with",
        "{",
        repeat(choice($._block_sep, $._line_sep)),
        field("key", $.identifier),
        ":",
        field("value", $._expression),
        repeat(seq(
          optional(","),
          repeat(choice($._block_sep, $._line_sep)),
          field("key", $.identifier),
          ":",
          field("value", $._expression),
        )),
        optional(","),
        repeat(choice($._block_sep, $._line_sep)),
        "}"
      ),

    parallel_expression: ($) =>
      seq(
        "parallel",
        field("count", $._expression),
        optional(field("options", $.parallel_options)),
        "{",
        optional(seq(
          repeat(choice($._block_sep, $._line_sep)),
          field("variable", $.identifier),
          "->",
          repeat(choice($._block_sep, $._line_sep))
        )),
        statementSeparated($, $._statement),
        "}"
      ),

    parallel_each_expression: ($) =>
      seq(
        "parallel",
        "each",
        field("list", $._expression),
        optional(field("options", $.parallel_options)),
        "{",
        repeat(choice($._block_sep, $._line_sep)),
        optional(seq(
          field("variable", $.identifier),
          "->",
          repeat(choice($._block_sep, $._line_sep))
        )),
        statementSeparated($, $._statement),
        "}",
        optional(seq("as", field("stream_marker", alias("stream", $.stream_marker))))
      ),

    parallel_settle_expression: ($) =>
      seq(
        "parallel",
        "settle",
        field("list", $._expression),
        optional(field("options", $.parallel_options)),
        "{",
        repeat(choice($._block_sep, $._line_sep)),
        optional(seq(
          field("variable", $.identifier),
          "->",
          repeat(choice($._block_sep, $._line_sep))
        )),
        statementSeparated($, $._statement),
        "}"
      ),

    defer_statement: ($) => seq("defer", $.block),

    // --- Primary expressions ---

    _primary: ($) =>
      choice(
        $.multiline_string_literal,
        $.raw_string_literal,
        $.string_literal,
        $.integer_literal,
        $.float_literal,
        $.duration_literal,
        $.true,
        $.false,
        $.nil,
        $.identifier,
        $.list_literal,
        $.dict_literal,
        $.closure,
        $.fn_expression
      ),

    string_literal: ($) =>
      seq(
        alias('"', $.string_delimiter),
        repeat(choice(
          $.interpolation,
          $.string_content,
          $.string_escape,
          $.string_dollar
        )),
        alias(token.immediate('"'), $.string_delimiter)
      ),

    multiline_string_literal: ($) =>
      seq(
        alias('"""', $.multiline_string_delimiter),
        repeat(choice(
          $.interpolation,
          $.multiline_string_content,
          $.string_dollar
        )),
        alias(token.immediate('"""'), $.multiline_string_delimiter)
      ),

    raw_string_literal: ($) =>
      seq(
        alias('r"', $.raw_string_delimiter),
        optional($.raw_string_content),
        alias(token.immediate('"'), $.raw_string_delimiter)
      ),

    string_content: (_) =>
      token.immediate(prec(1, /[^"\\$\n]+/)),

    multiline_string_content: (_) =>
      token.immediate(prec(1, /([^"$\\]|"[^"]|""[^"]|\\[\s\S])+/)),

    raw_string_content: (_) =>
      token.immediate(prec(1, /[^"\n]+/)),

    string_escape: (_) =>
      token.immediate(/\\[^\n]/),

    string_dollar: (_) =>
      token.immediate("$"),

    interpolation: ($) =>
      seq(token.immediate("${"), $._expression, "}"),

    integer_literal: (_) => /\d+/,

    float_literal: (_) => /\d+\.\d+/,

    duration_literal: (_) => /\d+(ms|s|m|h|d|w)/,

    true: (_) => "true",
    false: (_) => "false",
    nil: (_) => "nil",

    identifier: (_) => /[a-zA-Z_][a-zA-Z0-9_]*/,

    list_literal: ($) =>
      seq(
        "[",
        repeat(choice($._list_element, ",", lineBreak($))),
        "]"
      ),

    _list_element: ($) =>
      choice(
        $.spread_expression,
        $._expression
      ),

    dict_literal: ($) =>
      seq(
        "{",
        repeat(choice($._dict_element, ",", lineBreak($))),
        "}"
      ),

    _dict_element: ($) =>
      choice(
        $.spread_expression,
        $.dict_entry
      ),

    dict_entry: ($) =>
      seq(
        field(
          "key",
          choice(
            $.identifier,
            $.keyword_identifier,
            $.string_literal,
            seq("[", $._expression, "]")
          )
        ),
        ":",
        field("value", $._expression)
      ),

    spread_expression: ($) =>
      seq("...", $._expression),

    closure: ($) =>
      seq(
        "{",
        optional($.parameter_list),
        "->",
        statementSeparated($, $._statement),
        "}"
      ),

    fn_expression: ($) =>
      seq(
        "fn",
        "(",
        parameterList($),
        ")",
        optional(seq("->", field("return_type", $.type_annotation))),
        optional(seq("throws", field("throws", $.type_annotation))),
        field("body", $.block)
      ),

    // --- Shared rules ---

    block: ($) =>
      seq(
        "{",
        statementSeparated($, $._statement),
        "}"
      ),

    parameter_list: ($) =>
      prec.right(seq(
        $.typed_parameter,
        repeat(seq(",", repeat(lineBreak($)), $.typed_parameter)),
        optional(seq(",", repeat(lineBreak($))))
      )),

    typed_parameter: ($) =>
      seq(
        optional("..."),
        field("name", $.identifier),
        optional(seq(":", field("type", $.type_annotation))),
        optional(seq("=", field("default", $._expression)))
      ),

    argument_list: ($) =>
      prec.right(seq(
        $._argument_element,
        repeat(seq(",", repeat(lineBreak($)), $._argument_element)),
        optional(seq(",", repeat(lineBreak($))))
      )),

    keyword_identifier: (_) =>
      choice(...KEYWORD_IDENTIFIERS),

    _argument_element: ($) =>
      choice(
        $.spread_expression,
        $.struct_construct,
        $._expression
      ),

    struct_construct: ($) =>
      seq(
        field("type_name", $.identifier),
        field("fields", $.dict_literal)
      ),

    type_annotation: ($) =>
      choice(
        $.fn_type,
        seq("[", $.type_annotation, "]"),
        seq($.identifier, "<", $.type_annotation, ",", $.type_annotation, ">"),
        seq($.identifier, "<", $.type_annotation, ">"),
        prec.left(1, seq($.type_annotation, "|", $.type_annotation)),
        prec.left(2, seq($.type_annotation, "&", $.type_annotation)),
        // Postfix `?` is sugar for `T | nil`. Higher precedence than
        // `&` and `|` so `A & B?` parses as `A & (B?)`.
        prec.left(3, seq($.type_annotation, "?")),
        $.shape_type,
        $.identifier,
        $.string_literal,
        $.raw_string_literal,
        $.integer_literal,
        seq("-", $.integer_literal)
      ),

    fn_type: ($) =>
      prec.right(seq(
        "fn",
        "(",
        optional(commaSep1($.type_annotation)),
        ")",
        optional(seq("->", $.type_annotation))
      )),

    // Row-polymorphic shape type. After the regular named fields, a shape
    // may carry one or more trailing row tails (`...<type>`), mirroring the
    // value-level spread (`{...a, b: 1}`) and rest params (`fn f(...args)`).
    // Forms accepted (matching the Rust parser):
    //   { id: string, ...rest }      — fields + a row variable
    //   { ...R1, ...R2 }             — row tails only, no explicit fields
    //   { a: int, ...dict<string, V> } — a tail can be any type, not just an ident
    //   { a: int, b?: string }       — plain closed shape (no tails)
    shape_type: ($) =>
      seq(
        "{",
        optional(
          choice(
            // Fields, optionally followed by row tails.
            seq(
              $.shape_field,
              repeat(seq(",", $.shape_field)),
              repeat(seq(",", $.row_tail)),
              optional(",")
            ),
            // Row tails only (no explicit fields).
            seq(
              $.row_tail,
              repeat(seq(",", $.row_tail)),
              optional(",")
            )
          )
        ),
        "}"
      ),

    shape_field: ($) =>
      seq(
        field("name", $.identifier),
        optional("?"),
        ":",
        field("type", $.type_annotation)
      ),

    // Trailing row tail in a row-polymorphic shape: `...<type>`. The tail
    // may be a bare row variable (`...rest`) or any full type annotation
    // (`...dict<string, V>`), so it carries a `type_annotation`.
    row_tail: ($) =>
      seq("...", field("type", $.type_annotation)),
  },
});

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}

function attributeIdentifier($) {
  return choice($.identifier, alias("retry", $.identifier));
}

function lineBreak($) {
  return choice($._line_sep, $._block_sep, $._newline);
}

function parameterList($) {
  return optional(seq(
    repeat(lineBreak($)),
    $.parameter_list,
    repeat(lineBreak($))
  ));
}

function statementSeparator($) {
  return choice(
    seq(";", repeat(lineBreak($))),
    repeat1(lineBreak($))
  );
}

function sequenceNoLeading($, rule, separator) {
  return optional(seq(
    rule,
    repeat(seq(separator($), rule)),
    optional(separator($))
  ));
}

function statementSeparated($, rule) {
  return seq(
    repeat(lineBreak($)),
    sequenceNoLeading($, rule, statementSeparator)
  );
}

function topLevelSeparator($) {
  return choice(
    seq(";", repeat(choice($._line_sep, $._newline))),
    repeat1(choice($._line_sep, $._newline))
  );
}

function topLevelSeparated($, rule) {
  return seq(
    repeat(choice($._line_sep, $._newline)),
    sequenceNoLeading($, rule, topLevelSeparator)
  );
}

function layoutSeparated($, rule) {
  return seq(
    repeat(lineBreak($)),
    optional(seq(
      rule,
      repeat(seq(repeat1(lineBreak($)), rule))
    )),
    repeat(lineBreak($))
  );
}
