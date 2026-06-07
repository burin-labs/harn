- **Reverted the v0.8.89 "untyped NULL (OID 0)" Postgres `nil` bind.** Binding
  `nil` as an unspecified-type NULL (OID 0) so Postgres infers the slot type
  from context is incompatible with sqlx's binary wire protocol: when a query
  mixes a `nil` param with non-null typed params, Postgres re-infers the whole
  parameter-type list from the OID-0 slot during `Parse`, and the inferred
  types no longer match the client-declared OIDs sqlx encodes the non-null
  params with — yielding `incorrect binary data format in bind parameter N` /
  `insufficient data left in message` (and `could not determine data type` in
  genuinely ambiguous contexts). `nil` once again binds as `None::<String>`
  (Postgres TEXT), the long-stable behavior. The narrow cache-poisoning /
  typed-column cases the OID-0 change targeted are handled at the query layer by
  binding a concrete cast (`$n::text::int`, a `::text` cast, or a non-null
  sentinel) — the correct place to disambiguate a NULL's intended type.
