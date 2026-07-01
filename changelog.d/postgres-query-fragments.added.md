- **Postgres query helpers can now compose trusted SQL fragments structurally.**
  `std/postgres/query` adds `sql_fragment`, `sql_and`, `sql_or`, `sql_not`, and
  `jsonb_object` so Harn data-access modules can build reusable predicates and
  JSON envelope projections without ad hoc string concatenation.
