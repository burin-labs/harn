; Keywords
"pipeline" @keyword
"extends" @keyword
"override" @keyword
"let" @keyword
"const" @keyword
"var" @keyword
"if" @keyword.conditional
"else" @keyword.conditional
"for" @keyword.repeat
"in" @keyword.repeat
"while" @keyword.repeat
"break" @keyword.repeat
"continue" @keyword.repeat
"match" @keyword.conditional
"retry" @keyword
"try" @keyword.exception
"catch" @keyword.exception
"throw" @keyword.exception
"throws" @keyword.exception
"finally" @keyword.exception
"return" @keyword.return
"import" @keyword.import
"fn" @keyword.function
"gen" @keyword.function
"spawn" @keyword
"parallel" @keyword
"defer" @keyword
"type" @keyword
"pub" @keyword
"enum" @keyword
"struct" @keyword
"impl" @keyword
"interface" @keyword
"where" @keyword
"yield" @keyword
"emit" @keyword
"deadline" @keyword
"guard" @keyword
"require" @keyword
"mutex" @keyword
"select" @keyword
"from" @keyword
"timeout" @keyword
"default" @keyword
"tool" @keyword
"skill" @keyword
"eval_pack" @keyword
"not" @keyword.operator
"to" @keyword.operator
"exclusive" @keyword.operator

; HITL primitives (reserved keywords parsed as call-like statements)
"ask_user" @keyword
"dual_control" @keyword
"escalate_to" @keyword
"request_approval" @keyword

; Literals
(true) @boolean
(false) @boolean
(nil) @constant.builtin
(integer_literal) @number
(float_literal) @number.float
(duration_literal) @number
(string_delimiter) @punctuation.definition.string
(multiline_string_delimiter) @punctuation.definition.string
(raw_string_delimiter) @punctuation.definition.string
(string_content) @string
(multiline_string_content) @string
(raw_string_content) @string
(string_escape) @string.escape
(string_dollar) @string
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

(tool_declaration
  name: (identifier) @function)

(skill_declaration
  name: (identifier) @function)

(eval_pack_declaration
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

(struct_construct
  type_name: (identifier) @type)

(struct_field
  name: (identifier) @property)

; Impl blocks
(impl_block
  type_name: (identifier) @type)

; Interface declarations
(interface_declaration
  name: (identifier) @type)

(interface_declaration
  (generic_params
    (generic_param
      (identifier) @type)))

(interface_method
  name: (identifier) @function)

(interface_method
  (generic_params
    (generic_param
      (identifier) @type)))

(associated_type_declaration
  name: (identifier) @type)

; Generic params
(generic_params
  (generic_param
    (identifier) @type))

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

; Row-polymorphic shape tails (`...rest`): a bare row-variable identifier
; reads as a type parameter, mirroring generic params and type annotations.
; (Full-type tails like `...dict<string, V>` are already covered by the
; `type_annotation` rule below.)
(row_tail
  type: (type_annotation
    (identifier) @type))

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
"**" @operator
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
