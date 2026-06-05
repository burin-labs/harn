; PostgreSQL SQL templates from std/postgres/query.
;
; Harn's `sql(...)` helper takes the SQL template as its first argument.
; `named_sql(...)` takes the template as its third argument after name and mode.

((call_expression
  function: [
    (identifier) @_postgres_sql_fn
    (property_access
      property: (identifier) @_postgres_sql_fn)
  ]
  (argument_list
    .
    [
      (string_literal (string_content) @injection.content)
      (raw_string_literal (raw_string_content) @injection.content)
      (multiline_string_literal (multiline_string_content) @injection.content)
    ]))
  (#eq? @_postgres_sql_fn "sql")
  (#set! injection.language "sql"))

((call_expression
  function: [
    (identifier) @_postgres_sql_fn
    (property_access
      property: (identifier) @_postgres_sql_fn)
  ]
  (argument_list
    .
    (_)
    .
    (_)
    .
    [
      (string_literal (string_content) @injection.content)
      (raw_string_literal (raw_string_content) @injection.content)
      (multiline_string_literal (multiline_string_content) @injection.content)
    ]))
  (#eq? @_postgres_sql_fn "named_sql")
  (#set! injection.language "sql"))
