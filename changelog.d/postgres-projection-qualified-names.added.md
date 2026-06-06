- `std/postgres/query` projection helpers (`uuid_text`, `timestamptz_json`,
  `nullable_timestamptz_json`) now accept table-qualified column names such as
  `timestamptz_json("vaults.created_at")`. Each dot-separated segment is
  validated as an identifier and the output alias is the trailing segment
  (`created_at`), so projections from joined queries compose through
  `columns([...])` without `unsafe_sql(...)` or brace escaping.
