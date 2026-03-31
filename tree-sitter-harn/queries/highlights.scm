; Keywords
"pipeline" @keyword
"extends" @keyword
"override" @keyword
"let" @keyword
"var" @keyword
"if" @keyword.conditional
"else" @keyword.conditional
"for" @keyword.repeat
"in" @keyword.repeat
"while" @keyword.repeat
"match" @keyword.conditional
"retry" @keyword
"try" @keyword.exception
"catch" @keyword.exception
"throw" @keyword.exception
"finally" @keyword.exception
"return" @keyword.return
"import" @keyword.import
"fn" @keyword.function
"spawn" @keyword
"parallel" @keyword
"parallel_map" @keyword
"parallel_settle" @keyword
"type" @keyword
"pub" @keyword
"enum" @keyword
"struct" @keyword
"impl" @keyword
"interface" @keyword
"where" @keyword
"yield" @keyword
"deadline" @keyword
"guard" @keyword
"mutex" @keyword
"select" @keyword
"from" @keyword
"timeout" @keyword
"default" @keyword
"not" @keyword.operator
"upto" @keyword.operator
"thru" @keyword.operator

; Literals
(true) @boolean
(false) @boolean
(nil) @constant.builtin
(integer_literal) @number
(float_literal) @number.float
(duration_literal) @number
(string_literal) @string
(interpolated_string) @string
(string_content) @string
(interpolation
  "${" @punctuation.special
  "}" @punctuation.special)

; Identifiers
(identifier) @variable

; Function declarations
(fn_declaration
  name: (identifier) @function)

(pipeline_declaration
  name: (identifier) @function)

(override_declaration
  name: (identifier) @function)

; Function calls
(call_expression
  function: (identifier) @function.call)

; Property access
(property_access
  property: (identifier) @property)

; Parameters
(typed_parameter
  name: (identifier) @variable.parameter)

; Type declarations
(type_declaration
  name: (identifier) @type.definition)

; Enum declarations
(enum_declaration
  name: (identifier) @type)

(enum_variant
  name: (identifier) @constant)

; Struct declarations
(struct_declaration
  name: (identifier) @type)

(struct_field
  name: (identifier) @property)

; Impl blocks
(impl_block
  type_name: (identifier) @type)

; Interface declarations
(interface_declaration
  name: (identifier) @type)

(interface_method
  name: (identifier) @function)

; Generic params
(generic_params
  (identifier) @type)

; Where clause
(where_clause
  (identifier) @type)

; Select
(select_case
  variable: (identifier) @variable)

; Type annotations
(type_annotation
  (identifier) @type)

; Shape type fields
(shape_field
  name: (identifier) @property)

; Dict entry keys
(dict_entry
  key: (identifier) @property)

; Operators
"|>" @operator
"??" @operator
"&&" @operator
"||" @operator
"==" @operator
"!=" @operator
"<" @operator
">" @operator
"<=" @operator
">=" @operator
"+" @operator
"-" @operator
"*" @operator
"/" @operator
"%" @operator
"!" @operator
"=" @operator
"+=" @operator
"-=" @operator
"*=" @operator
"/=" @operator
"%=" @operator
"..." @operator
"->" @punctuation.delimiter
"?" @operator
":" @punctuation.delimiter

; Delimiters
"(" @punctuation.bracket
")" @punctuation.bracket
"[" @punctuation.bracket
"]" @punctuation.bracket
"{" @punctuation.bracket
"}" @punctuation.bracket
"," @punctuation.delimiter
"." @punctuation.delimiter
"?." @punctuation.delimiter

; Comments
(comment) @comment
