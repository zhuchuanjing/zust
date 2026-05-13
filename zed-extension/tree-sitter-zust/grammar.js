const PREC = {
  assignment: 1,
  range: 2,
  or: 3,
  and: 4,
  bit_or: 5,
  bit_xor: 6,
  bit_and: 7,
  equality: 8,
  comparison: 9,
  shift: 10,
  additive: 11,
  multiplicative: 12,
  cast: 13,
  unary: 14,
  call: 15,
  field: 16,
};

const ASSIGNMENT_OPERATORS = [
  "=",
  "+=",
  "-=",
  "*=",
  "/=",
  "%=",
  "&=",
  "|=",
  "^=",
  "<<=",
  ">>=",
];

const BINARY_OPERATORS = [
  ["||", PREC.or],
  ["&&", PREC.and],
  ["|", PREC.bit_or],
  ["^", PREC.bit_xor],
  ["&", PREC.bit_and],
  ["==", PREC.equality],
  ["!=", PREC.equality],
  ["<", PREC.comparison],
  [">", PREC.comparison],
  ["<=", PREC.comparison],
  [">=", PREC.comparison],
  ["<<", PREC.shift],
  [">>", PREC.shift],
  ["+", PREC.additive],
  ["-", PREC.additive],
  ["*", PREC.multiplicative],
  ["/", PREC.multiplicative],
  ["%", PREC.multiplicative],
];

module.exports = grammar({
  name: "zust",

  extras: ($) => [
    /\s/,
    $.line_comment,
    $.block_comment,
  ],

  word: ($) => $.identifier,

  supertypes: ($) => [
    $._statement,
    $._expression,
    $.pattern,
    $.type,
  ],

  conflicts: ($) => [
    [$.map_literal, $.code_block],
    [$.path_expression, $.type_identifier],
    [$.type_identifier, $._expression],
    [$._expression, $._expression_no_range],
    [$.type_identifier, $.const_type_expression],
  ],

  rules: {
    source_file: ($) => repeat($._statement),

    line_comment: () => token(seq("//", /.*/)),

    block_comment: () => token(seq("/*", /[^*]*\*+([^/*][^*]*\*+)*/, "/")),

    _statement: ($) => choice(
      $.function_definition,
      $.struct_definition,
      $.impl_block,
      $.const_item,
      $.static_item,
      $.let_statement,
      $.for_statement,
      $.while_statement,
      $.loop_statement,
      $.break_statement,
      $.continue_statement,
      $.return_statement,
      $.expression_statement,
    ),

    code_block: ($) => seq(
      "{",
      repeat($._statement),
      "}",
    ),

    let_statement: ($) => seq(
      "let",
      field("pattern", $.pattern),
      "=",
      field("value", choice($._expression, $.code_block)),
      optional(";"),
    ),

    break_statement: () => seq("break", ";"),

    continue_statement: () => seq("continue", ";"),

    return_statement: ($) => seq(
      "return",
      optional(field("value", $._expression)),
      ";",
    ),

    while_statement: ($) => seq(
      "while",
      field("condition", $._expression),
      field("body", $.code_block),
    ),

    loop_statement: ($) => seq(
      "loop",
      field("body", $.code_block),
    ),

    for_statement: ($) => seq(
      "for",
      field("pattern", $.pattern),
      "in",
      field("iterable", $._expression),
      field("body", $.code_block),
    ),

    function_definition: ($) => seq(
      optional("pub"),
      "fn",
      field("name", $.identifier),
      field("parameters", $.parameter_list),
      field("body", $.code_block),
    ),

    parameter_list: ($) => seq(
      "(",
      commaSep($.parameter),
      ")",
    ),

    parameter: ($) => seq(
      field("name", $.identifier),
      optional(seq(":", field("type", $.type))),
    ),

    struct_definition: ($) => seq(
      optional("pub"),
      "struct",
      field("name", $.identifier),
      optional(field("type_parameters", $.type_parameter_list)),
      field("body", $.field_declaration_list),
    ),

    field_declaration_list: ($) => seq(
      "{",
      commaSep($.field_declaration),
      "}",
    ),

    field_declaration: ($) => seq(
      field("name", $.identifier),
      ":",
      field("type", $.type),
    ),

    impl_block: ($) => seq(
      "impl",
      field("type", $.type),
      field("body", $.code_block),
    ),

    const_item: ($) => seq(
      optional("pub"),
      "const",
      field("name", $.identifier),
      optional(seq(":", field("type", $.type))),
      "=",
      field("value", $._expression),
      ";",
    ),

    static_item: ($) => seq(
      optional("pub"),
      "static",
      field("name", $.identifier),
      ":",
      field("type", $.type),
      optional(seq("=", field("value", $._expression))),
      ";",
    ),

    expression_statement: ($) => seq(
      field("expression", $._expression),
      optional(";"),
    ),

    pattern: ($) => choice(
      $.identifier_pattern,
      $.typed_pattern,
      $.tuple_pattern,
      $.list_pattern,
      $.wildcard_pattern,
    ),

    wildcard_pattern: () => "_",

    identifier_pattern: ($) => $.identifier,

    typed_pattern: ($) => seq(
      field("name", $.identifier),
      ":",
      field("type", $.type),
    ),

    tuple_pattern: ($) => seq(
      "(",
      $.pattern,
      ",",
      commaSep($.pattern),
      ")",
    ),

    list_pattern: ($) => seq(
      "[",
      commaSep($.pattern),
      "]",
    ),

    type: ($) => choice(
      $.primitive_type,
      $.type_identifier,
      $.generic_type,
      $.vector_type,
    ),

    primitive_type: () => token(choice(
      "bool",
      "string",
      "i8",
      "i16",
      "i32",
      "i64",
      "u8",
      "u16",
      "u32",
      "u64",
      "f16",
      "f32",
      "f64",
    )),

    type_identifier: ($) => choice($.identifier, $.scoped_identifier),

    generic_type: ($) => prec(1, seq(
      field("name", $.type_identifier),
      "<",
      commaSep1($.type_parameter),
      ">",
    )),

    type_parameter_list: ($) => seq(
      "<",
      commaSep1($.type_parameter),
      ">",
    ),

    type_parameter: ($) => choice(
      $.const_type_expression,
      $.type,
    ),

    const_type_expression: ($) => choice(
      $.number_literal,
      $.identifier,
      $.const_type_binary_expression,
      $.const_type_parenthesized_expression,
    ),

    const_type_binary_expression: ($) => choice(
      ...[
        ["+", PREC.additive],
        ["-", PREC.additive],
        ["*", PREC.multiplicative],
        ["/", PREC.multiplicative],
        ["%", PREC.multiplicative],
      ].map(([operator, precedence]) =>
        prec.left(precedence, seq(
          field("left", $.const_type_expression),
          field("operator", operator),
          field("right", $.const_type_expression),
        )),
      ),
    ),

    const_type_parenthesized_expression: ($) => seq(
      "(",
      $.const_type_expression,
      ")",
    ),

    vector_type: ($) => seq(
      "[",
      field("element", $.type),
      ";",
      field("length", $.type_parameter),
      "]",
    ),

    _expression: ($) => choice(
      $.if_expression,
      $.closure_expression,
      $.assignment_expression,
      $.range_expression,
      $.binary_expression,
      $.cast_expression,
      $.unary_expression,
      $.call_expression,
      $.field_expression,
      $.index_expression,
      $.struct_literal,
      $.map_literal,
      $.repeat_literal,
      $.list_literal,
      $.tuple_expression,
      $.path_expression,
      $.identifier,
      $.number_literal,
      $.string_literal,
      $.raw_string_literal,
      $.boolean_literal,
      $.null_literal,
      $.parenthesized_expression,
    ),

    if_expression: ($) => prec.right(seq(
      "if",
      field("condition", $._expression),
      field("consequence", $.code_block),
      optional(seq(
        "else",
        field("alternative", choice($.if_expression, $.code_block)),
      )),
    )),

    closure_expression: ($) => seq(
      "|",
      commaSep(choice($.identifier, $.typed_pattern)),
      "|",
      field("body", $.code_block),
    ),

    assignment_expression: ($) => prec.right(PREC.assignment, seq(
      field("left", choice($.identifier, $.path_expression, $.field_expression, $.index_expression)),
      field("operator", choice(...ASSIGNMENT_OPERATORS)),
      field("right", $._expression),
    )),

    range_expression: ($) => prec.right(PREC.range, seq(
      field("start", $._expression_no_range),
      field("operator", choice("..", "..=")),
      field("end", $._expression),
    )),

    _expression_no_range: ($) => choice(
      $.binary_expression,
      $.cast_expression,
      $.unary_expression,
      $.call_expression,
      $.field_expression,
      $.index_expression,
      $.struct_literal,
      $.map_literal,
      $.repeat_literal,
      $.list_literal,
      $.tuple_expression,
      $.path_expression,
      $.identifier,
      $.number_literal,
      $.string_literal,
      $.raw_string_literal,
      $.boolean_literal,
      $.null_literal,
      $.parenthesized_expression,
      $.if_expression,
      $.closure_expression,
    ),

    binary_expression: ($) => choice(
      ...BINARY_OPERATORS.map(([operator, precedence]) =>
        prec.left(precedence, seq(
          field("left", $._expression),
          field("operator", operator),
          field("right", $._expression),
        )),
      ),
    ),

    cast_expression: ($) => prec.left(PREC.cast, seq(
      field("value", $._expression),
      "as",
      field("type", $.type),
    )),

    unary_expression: ($) => prec.left(PREC.unary, seq(
      field("operator", choice("!", "-", "+")),
      field("argument", $._expression),
    )),

    call_expression: ($) => prec.left(PREC.call, seq(
      field("function", choice($.identifier, $.path_expression, $.field_expression, $.index_expression, $.parenthesized_expression)),
      field("arguments", $.argument_list),
    )),

    argument_list: ($) => seq(
      "(",
      commaSep($._expression),
      ")",
    ),

    field_expression: ($) => prec.left(PREC.field, seq(
      field("value", choice($.identifier, $.path_expression, $.call_expression, $.field_expression, $.index_expression, $.parenthesized_expression)),
      ".",
      field("field", $.identifier),
    )),

    index_expression: ($) => prec.left(PREC.call, seq(
      field("value", choice($.identifier, $.path_expression, $.call_expression, $.field_expression, $.index_expression, $.parenthesized_expression)),
      "[",
      field("index", $._expression),
      "]",
    )),

    struct_literal: ($) => prec(PREC.call, seq(
      field("name", $.type_identifier),
      "{",
      commaSep($.struct_field),
      "}",
    )),

    struct_field: ($) => choice(
      seq(
        field("name", $.identifier),
        ":",
        field("value", $._expression),
      ),
      field("shorthand", $.identifier),
    ),

    map_literal: ($) => seq(
      "{",
      commaSep($.map_entry),
      "}",
    ),

    map_entry: ($) => choice(
      seq(
        field("key", choice($.identifier, $.string_literal)),
        ":",
        field("value", $._expression),
      ),
      field("shorthand", $.identifier),
    ),

    repeat_literal: ($) => seq(
      "[",
      field("value", $._expression),
      ";",
      field("length", $.type_parameter),
      "]",
    ),

    list_literal: ($) => seq(
      "[",
      commaSep($._expression),
      "]",
    ),

    tuple_expression: ($) => seq(
      "(",
      $._expression,
      ",",
      commaSep($._expression),
      ")",
    ),

    parenthesized_expression: ($) => seq(
      "(",
      $._expression,
      ")",
    ),

    path_expression: ($) => choice(
      $.scoped_identifier,
      $.generic_scoped_identifier,
    ),

    scoped_identifier: ($) => seq(
      $.identifier,
      repeat1(seq("::", $.identifier)),
    ),

    generic_scoped_identifier: ($) => seq(
      $.generic_type,
      repeat1(seq("::", $.identifier)),
    ),

    boolean_literal: () => choice("true", "false"),

    null_literal: () => "null",

    number_literal: () => token(choice(
      /0x[0-9a-fA-F]+(?:u8|u16|u32|u64|i8|i16|i32|i64)?/,
      /0b[01]+(?:u8|u16|u32|u64|i8|i16|i32|i64)?/,
      /0o[0-7]+(?:u8|u16|u32|u64|i8|i16|i32|i64)?/,
      /\d+\.\d+(?:[eE][+-]?\d+)?(?:f16|f32|f64)?/,
      /\d+(?:[eE][+-]?\d+)(?:f16|f32|f64)?/,
      /\d+(?:u8|u16|u32|u64|i8|i16|i32|i64|f16|f32|f64)?/,
    )),

    string_literal: () => token(seq(
      '"',
      repeat(choice(
        /[^"\\\n]+/,
        /\\./,
      )),
      '"',
    )),

    raw_string_literal: () => token(seq(
      'r#"',
      repeat(choice(
        /[^"]/,
        /"[^#]/,
      )),
      '"#',
    )),

    identifier: () => token(prec(-1, /[_\p{L}][_\p{L}\p{N}]*/)),
  },
});

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)), optional(","));
}
