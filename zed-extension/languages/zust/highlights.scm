[
  (line_comment)
  (block_comment)
] @comment

[
  (string_literal)
  (raw_string_literal)
] @string

(number_literal) @number
(boolean_literal) @boolean
(null_literal) @constant.builtin
(primitive_type) @type.builtin

[
  "as"
  "break"
  "const"
  "continue"
  "else"
  "fn"
  "for"
  "if"
  "impl"
  "in"
  "let"
  "loop"
  "pub"
  "return"
  "static"
  "struct"
  "while"
] @keyword

(function_definition
  name: (identifier) @function)

(parameter
  name: (identifier) @parameter)

(typed_pattern
  name: (identifier) @variable)

(identifier_pattern) @variable
(wildcard_pattern) @variable.special

(struct_definition
  name: (identifier) @type)

(impl_block
  type: (type_identifier) @type)

(field_declaration
  name: (identifier) @property)

(struct_field
  name: (identifier) @property)

(map_entry
  key: (identifier) @property)

(map_entry
  key: (string_literal) @string.special)

(const_item
  name: (identifier) @constant)

(static_item
  name: (identifier) @constant)

(call_expression
  function: (identifier) @function)

(call_expression
  function: (field_expression
    field: (identifier) @function.method))

(field_expression
  field: (identifier) @property)

(scoped_identifier
  (identifier) @namespace)

(type_identifier
  (identifier) @type)
