/// <reference types="tree-sitter-cli/dsl" />

module.exports = grammar({
  name: "harn",

  extras: ($) => [/[ \t\r]/, $.comment],

  conflicts: ($) => [
    [$.dict_literal, $.shape_type],
    [$.type_annotation],
    [$._statement, $._expression],
    [$._primary, $.type_annotation],
    [$.typed_parameter, $.shape_field],
  ],

  word: ($) => $.identifier,

  rules: {
    source_file: ($) => repeat($._top_level),

    _top_level: ($) =>
      choice(
        $.pipeline_declaration,
        $.import_declaration,
        $.interface_declaration,
        $.type_declaration,
        $._statement,
        $._newline
      ),

    _newline: (_) => "\n",

    // --- Comments ---

    comment: (_) =>
      token(
        choice(seq("//", /[^\n]*/), seq("/*", /[^*]*\*+([^/*][^*]*\*+)*/, "/"))
      ),

    // --- Top-level declarations ---

    pipeline_declaration: ($) =>
      seq(
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
        seq("import", "{", commaSep1($.identifier), "}", "from", $.string_literal)
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
        $.override_declaration,
        $.parallel_expression,
        $.parallel_map_expression,
        $.assignment,
        $._expression
      ),

    let_binding: ($) =>
      seq(
        "let",
        field("name", $.identifier),
        optional(seq(":", field("type", $.type_annotation))),
        "=",
        field("value", $._expression)
      ),

    var_binding: ($) =>
      seq(
        "var",
        field("name", $.identifier),
        optional(seq(":", field("type", $.type_annotation))),
        "=",
        field("value", $._expression)
      ),

    assignment: ($) =>
      seq(field("target", $.identifier), "=", field("value", $._expression)),

    if_statement: ($) =>
      seq(
        "if",
        field("condition", $._expression),
        field("consequence", $.block),
        optional(seq("else", choice($.if_statement, $.block)))
      ),

    for_statement: ($) =>
      seq(
        "for",
        field("variable", $.identifier),
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
        repeat($.match_arm),
        "}"
      ),

    match_arm: ($) =>
      seq(field("pattern", $._expression), "->", field("body", $.block)),

    retry_statement: ($) =>
      seq("retry", field("count", $._expression), field("body", $.block)),

    try_catch_statement: ($) =>
      seq(
        "try",
        field("body", $.block),
        "catch",
        optional(seq("(", field("error_var", $.identifier), ")")),
        field("handler", $.block)
      ),

    return_statement: ($) => prec.right(seq("return", optional($._expression))),

    throw_statement: ($) => seq("throw", $._expression),

    fn_declaration: ($) =>
      seq(
        optional("pub"),
        "fn",
        field("name", $.identifier),
        "(",
        optional($.parameter_list),
        ")",
        optional(seq("->", $.type_annotation)),
        field("body", $.block)
      ),

    interface_declaration: ($) =>
      seq(
        "interface",
        field("name", $.identifier),
        "{",
        repeat($.interface_method),
        "}"
      ),

    type_declaration: ($) =>
      seq("type", field("name", $.identifier), "=", field("type", $.type_annotation)),

    interface_method: ($) =>
      seq(
        "fn",
        field("name", $.identifier),
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
        $.unary_expression,
        $.call_expression,
        $.method_call,
        $.property_access,
        $.subscript_expression,
        $.slice_expression,
        $.parenthesized_expression,
        $.spawn_expression,
        $.parallel_expression,
        $.parallel_map_expression,
        $.if_statement,
        $.retry_statement,
        $._primary
      ),

    pipe_expression: ($) =>
      prec.left(1, seq($._expression, "|>", $._expression)),

    ternary_expression: ($) =>
      prec.right(2, seq($._expression, "?", $._expression, ":", $._expression)),

    nil_coalescing_expression: ($) =>
      prec.left(3, seq($._expression, "??", $._expression)),

    binary_expression: ($) =>
      choice(
        prec.left(4, seq($._expression, "||", $._expression)),
        prec.left(5, seq($._expression, "&&", $._expression)),
        prec.left(6, seq($._expression, choice("==", "!="), $._expression)),
        prec.left(7, seq($._expression, choice("<", ">", "<=", ">="), $._expression)),
        prec.left(8, seq($._expression, choice("+", "-"), $._expression)),
        prec.left(9, seq($._expression, choice("*", "/"), $._expression))
      ),

    unary_expression: ($) =>
      prec.right(10, seq(choice("!", "-"), $._expression)),

    call_expression: ($) =>
      prec.left(11, seq(field("function", $.identifier), "(", optional($.argument_list), ")")),

    method_call: ($) =>
      prec.left(
        11,
        seq(
          field("object", $._expression),
          choice(".", "?."),
          field("method", $.identifier),
          "(",
          optional($.argument_list),
          ")"
        )
      ),

    property_access: ($) =>
      prec.left(
        11,
        seq(field("object", $._expression), choice(".", "?."), field("property", $.identifier))
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

    parenthesized_expression: ($) => seq("(", $._expression, ")"),

    spawn_expression: ($) => seq("spawn", $.block),

    parallel_expression: ($) =>
      seq(
        "parallel",
        "(",
        field("count", $._expression),
        ")",
        "{",
        optional(seq(field("variable", $.identifier), "->")),
        repeat($._statement),
        "}"
      ),

    parallel_map_expression: ($) =>
      seq(
        "parallel_map",
        "(",
        field("list", $._expression),
        ")",
        "{",
        field("variable", $.identifier),
        "->",
        repeat($._statement),
        "}"
      ),

    // --- Primary expressions ---

    _primary: ($) =>
      choice(
        $.string_literal,
        $.interpolated_string,
        $.integer_literal,
        $.float_literal,
        $.true,
        $.false,
        $.nil,
        $.identifier,
        $.list_literal,
        $.dict_literal,
        $.closure
      ),

    string_literal: (_) =>
      token(seq('"', repeat(choice(/[^"\\$\n]/, /\\[ntr\\"$]/, /\$[^{]/)), '"')),

    interpolated_string: ($) =>
      seq(
        '"',
        repeat(
          choice(
            $.string_content,
            $.interpolation
          )
        ),
        '"'
      ),

    string_content: (_) => token.immediate(prec(1, /[^"\\$\n]+/)),

    interpolation: ($) =>
      seq(token.immediate("${"), $._expression, "}"),

    integer_literal: (_) => /\d+/,

    float_literal: (_) => /\d+\.\d+/,

    true: (_) => "true",
    false: (_) => "false",
    nil: (_) => "nil",

    identifier: (_) => /[a-zA-Z_][a-zA-Z0-9_]*/,

    list_literal: ($) =>
      seq("[", optional(seq($._expression, repeat(seq(",", $._expression)), optional(","))), "]"),

    dict_literal: ($) =>
      seq("{", optional(seq($.dict_entry, repeat(seq(",", $.dict_entry)), optional(","))), "}"),

    dict_entry: ($) =>
      seq(
        field("key", choice($.identifier, seq("[", $._expression, "]"))),
        ":",
        field("value", $._expression)
      ),

    closure: ($) =>
      seq(
        "{",
        optional($.parameter_list),
        "->",
        repeat($._statement),
        "}"
      ),

    // --- Shared rules ---

    block: ($) => seq("{", repeat(choice($._statement, $._newline)), "}"),

    parameter_list: ($) =>
      seq($.typed_parameter, repeat(seq(",", $.typed_parameter))),

    typed_parameter: ($) =>
      seq(
        field("name", $.identifier),
        optional(seq(":", field("type", $.type_annotation)))
      ),

    argument_list: ($) =>
      seq($._expression, repeat(seq(",", $._expression))),

    type_annotation: ($) =>
      choice(
        seq($.identifier, "[", $.type_annotation, ",", $.type_annotation, "]"),
        seq($.identifier, "[", $.type_annotation, "]"),
        prec.left(1, seq($.type_annotation, "|", $.type_annotation)),
        $.shape_type,
        $.identifier
      ),

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
