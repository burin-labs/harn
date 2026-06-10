- **`std/postgres/query` gains `raw_sql(template, params?)` and
  `named_raw_sql(name, mode, template, params?)`.** These build query records
  from literal SQL with **no** `{name}` scanning, so brace-heavy SQL — JSON
  paths (`#>>'{}'`), array literals (`'{a,b}'::text[]`), and
  `jsonb_set(.., '{path}', ..)` — no longer needs `{{`/`}}` doubling.
  Parameters are positional (`$1`, `$2`, ...). The existing `sql(...)` /
  `named_sql(...)` named-placeholder behavior, including `{{`/`}}` escaping and
  `unsafe_sql(...)` fragments, is unchanged.
