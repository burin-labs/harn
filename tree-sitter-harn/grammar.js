/// <reference types="tree-sitter-cli/dsl" />

const KEYWORD_IDENTIFIERS = require("./grammar/keywords");

module.exports = grammar({
  name: "harn",

  extras: ($) => [/[ \t\r]/, $.comment],

  externals: ($) => [$._block_sep, $._line_sep],

  conflicts: ($) => [
    [$.dict_literal, $.shape_type],
    [$._statement, $._expression],
    [$._primary, $.type_annotation],
    [$._primary, $.typed_parameter],
    [$.typed_parameter, $.shape_field],
    [$.parallel_expression],
    [$.typed_parameter, $.type_annotation],
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
    [$.pipe_expression, $.binary_expression, $.unary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.nil_coalescing_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.ternary_expression, $.binary_expression, $.method_call, $.property_access],
    [$.pipe_expression, $.range_expression, $.binary_expression, $.method_call, $.property_access],
  ],

  word: ($) => $.identifier,

  rules: {
    source_file: ($) => repeat($._top_level),

    _top_level: ($) =>
      choice(
        $._top_level_item,
        $._line_sep
      ),

    _top_level_item: ($) =>
      choice(
        $.pipeline_declaration,
        $.import_declaration,
        $._statement
      ),

    _newline: (_) => "\n",

    // --- Comments ---

    comment: (_) =>
      token(
        choice(
          seq("//", /[^\n]*/),
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
        optional($.parameter_list),
        ")",
        optional(seq("extends", field("parent", $.identifier))),
        $.block
      ),

    import_declaration: ($) =>
      choice(
        seq("import", $.string_literal),
        seq(
          "import",
          "{",
          commaSep1($.identifier),
          optional(","),
          "}",
          "from",
          $.string_literal
        ),
        seq(
          "import",
          "{",
          $._newline,
          $.identifier,
          repeat(seq(",", $._newline, $.identifier)),
          optional(seq(",", $._newline)),
          "}",
          "from",
          $.string_literal
        )
      ),

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
        optional(seq("(", optional($.parameter_list), ")"))
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
        $.var_binding,
        $.if_statement,
        $.for_statement,
        $.while_statement,
        $.match_statement,
        $.retry_statement,
        $.try_catch_statement,
        $.return_statement,
        $.throw_statement,
        $.fn_declaration,
        $.tool_declaration,
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
        $.select_block,
        $.break_statement,
        $.continue_statement,
        $.yield_expression,
        $.assignment,
        $.compound_assignment,
        $.expression_statement
      ),

    expression_statement: ($) => prec(1, $._expression),

    let_binding: ($) =>
      seq(
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

    _binding_pattern: ($) =>
      choice(
        $.identifier,
        $.dict_pattern,
        $.list_pattern
      ),

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
        field("pattern", $._expression),
        optional(seq("if", field("guard", $._expression))),
        "->",
        field("body", $.block)
      ),

    retry_statement: ($) =>
      seq("retry", field("count", $._expression), field("body", $.block)),

    try_catch_statement: ($) =>
      seq(
        "try",
        field("body", $.block),
        choice(
          seq(
            "catch",
            optional(seq(
              "(",
              field("error_var", $.identifier),
              optional(seq(":", field("error_type", $.type_annotation))),
              ")"
            )),
            field("handler", $.block),
            optional(seq("finally", field("finalizer", $.block)))
          ),
          seq(
            "finally",
            field("finalizer", $.block)
          )
        )
      ),

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
      seq("mutex", field("body", $.block)),

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

    fn_declaration: ($) =>
      seq(
        optional("pub"),
        "fn",
        field("name", $.identifier),
        optional($.generic_params),
        "(",
        optional($.parameter_list),
        ")",
        optional(seq("->", $.type_annotation)),
        optional($.where_clause),
        field("body", $.block)
      ),

    tool_declaration: ($) =>
      seq(
        optional("pub"),
        "tool",
        field("name", $.identifier),
        "(",
        optional($.parameter_list),
        ")",
        optional(seq("->", $.type_annotation)),
        "{",
        repeat(choice($._block_sep, $._line_sep)),
        optional(seq("description", $.string_literal, repeat(choice($._block_sep, $._line_sep)))),
        layoutSeparated($, $._statement),
        "}"
      ),

    generic_params: ($) =>
      seq("<", commaSep1($.identifier), ">"),

    where_clause: ($) =>
      seq(
        "where",
        commaSep1(seq($.identifier, ":", $.identifier))
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
      seq("type", field("name", $.identifier), "=", field("type", $.type_annotation)),

    interface_method: ($) =>
      seq(
        "fn",
        field("name", $.identifier),
        optional($.generic_params),
        "(",
        optional($.parameter_list),
        ")",
        optional(seq("->", $.type_annotation))
      ),

    override_declaration: ($) =>
      seq(
        "override",
        field("name", $.identifier),
        "(",
        optional($.parameter_list),
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
        $.method_call,
        $.property_access,
        $.subscript_expression,
        $.slice_expression,
        $.try_unwrap_expression,
        $.parenthesized_expression,
        $.spawn_expression,
        $.try_expression,
        $.deadline_block,
        $.parallel_expression,
        $.parallel_each_expression,
        $.parallel_settle_expression,
        $.if_statement,
        $.retry_statement,
        $.match_statement,
        $._primary
      ),

    pipe_expression: ($) =>
      prec.left(1, seq($._expression, repeat($._newline), "|>", repeat($._newline), $._expression)),

    ternary_expression: ($) =>
      prec.right(2, seq($._expression, "?", $._expression, ":", $._expression)),

    nil_coalescing_expression: ($) =>
      prec.left(3, seq($._expression, "??", $._expression)),

    binary_expression: ($) =>
      choice(
        prec.left(4, seq($._expression, repeat($._newline), "||", repeat($._newline), $._expression)),
        prec.left(5, seq($._expression, repeat($._newline), "&&", repeat($._newline), $._expression)),
        prec.left(6, seq($._expression, choice("==", "!="), $._expression)),
        prec.left(7, seq($._expression, choice("<", ">", "<=", ">="), $._expression)),
        prec.left(7, seq($._expression, "in", $._expression)),
        prec.left(7, seq($._expression, "not", "in", $._expression)),
        prec.left(8, seq($._expression, repeat($._newline), "+", repeat($._newline), $._expression)),
        prec.left(8, seq($._expression, "-", $._expression)),
        prec.left(9, seq($._expression, repeat($._newline), "*", repeat($._newline), $._expression)),
        prec.left(9, seq($._expression, repeat($._newline), "/", repeat($._newline), $._expression)),
        prec.left(9, seq($._expression, repeat($._newline), "%", repeat($._newline), $._expression)),
        prec.right(10, seq($._expression, repeat($._newline), "**", repeat($._newline), $._expression))
      ),

    range_expression: ($) =>
      prec.left(7, seq($._expression, "to", $._expression, optional("exclusive"))),

    unary_expression: ($) =>
      prec.right(10, seq(choice("!", "-"), $._expression)),

    call_expression: ($) =>
      prec.left(
        11,
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

    method_call: ($) =>
      prec.left(
        11,
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
        11,
        seq(
          field("object", $._expression),
          repeat($._newline),
          choice(".", "?."),
          field("property", choice($.identifier, $.keyword_identifier))
        )
      ),

    subscript_expression: ($) =>
      prec.left(11, seq(field("object", $._expression), "[", $._expression, "]")),

    slice_expression: ($) =>
      prec.left(
        11,
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
      prec.left(11, seq($._expression, token.immediate("?"))),

    parenthesized_expression: ($) => seq("(", $._expression, ")"),

    spawn_expression: ($) => seq("spawn", $.block),

    try_expression: ($) => prec.right(seq("try", $.block)),

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
        layoutSeparated($, $._statement),
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
        layoutSeparated($, $._statement),
        "}"
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
        layoutSeparated($, $._statement),
        "}"
      ),

    defer_statement: ($) => seq("defer", $.block),

    // --- Primary expressions ---

    _primary: ($) =>
      choice(
        $.interpolated_string,
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

    string_literal: (_) =>
      token(seq('"', repeat(choice(/[^"\\$\n]/, /\\[ntr\\"$]/, /\$[^{]/)), '"')),

    multiline_string_literal: (_) =>
      token(seq('"""', repeat(choice(/[^"]/, /"[^"]/, /""[^"]/)), '"""')),

    raw_string_literal: (_) =>
      token(seq('r"', repeat(/[^"\n]/), '"')),

    interpolated_string: ($) =>
      seq(
        '"',
        repeat(
          choice(
            $.interpolation,
            $.string_content,
            $.string_dollar
          )
        ),
        '"'
      ),

    string_content: (_) =>
      token.immediate(prec(1, /[^"\\$\n]+/)),

    string_dollar: (_) =>
      token.immediate("$"),

    interpolation: ($) =>
      seq(token.immediate("${"), $._expression, "}"),

    integer_literal: (_) => /\d+/,

    float_literal: (_) => /\d+\.\d+/,

    duration_literal: (_) => /\d+(ms|s|m|h)/,

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
        layoutSeparated($, $._statement),
        "}"
      ),

    fn_expression: ($) =>
      seq(
        "fn",
        "(",
        optional($.parameter_list),
        ")",
        field("body", $.block)
      ),

    // --- Shared rules ---

    block: ($) =>
      seq(
        "{",
        layoutSeparated($, $._statement),
        "}"
      ),

    parameter_list: ($) =>
      prec.left(seq(
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
        seq($.identifier, "<", $.type_annotation, ",", $.type_annotation, ">"),
        seq($.identifier, "<", $.type_annotation, ">"),
        prec.left(1, seq($.type_annotation, "|", $.type_annotation)),
        $.shape_type,
        $.identifier
      ),

    fn_type: ($) =>
      prec.right(seq(
        "fn",
        "(",
        optional(commaSep1($.type_annotation)),
        ")",
        optional(seq("->", $.type_annotation))
      )),

    shape_type: ($) =>
      seq(
        "{",
        optional(
          seq(
            $.shape_field,
            repeat(seq(",", $.shape_field)),
            optional(",")
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
  },
});

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}

function lineBreak($) {
  return choice($._line_sep, $._block_sep, $._newline);
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
