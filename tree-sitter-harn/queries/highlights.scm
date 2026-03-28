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
"return" @keyword.return
"import" @keyword.import
"fn" @keyword.function
"spawn" @keyword
"parallel" @keyword
"parallel_map" @keyword
"type" @keyword
"pub" @keyword

; Literals
(true) @boolean
(false) @boolean
(nil) @constant.builtin
(integer_literal) @number
(float_literal) @number.float
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

(method_call
  method: (identifier) @function.method)

; Property access
(property_access
  property: (identifier) @property)

; Parameters
(typed_parameter
  name: (identifier) @variable.parameter)

; Type declarations
(type_declaration
  name: (identifier) @type.definition)

; Type annotations
(type_annotation
  (identifier) @type)

; Shape type fields
(shape_field
  name: (identifier) @property)

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
"!" @operator
"=" @operator
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
