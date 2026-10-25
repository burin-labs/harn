- **Provider catalog rows gain a `retires_on` field (#10440).** `ModelDef` and `ModelDeprecation` carry an optional
  `retires_on` ISO date for a retirement a provider has announced while the route is still served, and the catalog JSON
  schema and TypeScript, Swift and Harn bindings expose it.

  Migration: Rust code that builds a `ModelDef` or `ModelDeprecation` with a struct literal must add
  `retires_on: None`. Code that reads the catalog needs no change.
