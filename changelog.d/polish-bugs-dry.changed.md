- **VM and typechecker polish pass: DRY out the 0/1/many type-collapse
  fan-out and remove a per-call hot-path allocation.** The typechecker
  gains `collapse_members` / `collapse_members_opt` helpers that
  centralise the recurring empty→sentinel / single→member / multi→wrap
  pattern; `simplify_union`, `remove_from_union`, `narrow_to_single`,
  `intersect_union_with`, and three inference helpers now share one
  implementation. `json_schema_to_type_expr` gets the same treatment via
  a sibling helper in `type_expr.rs`. `TriggerEvent` exposes
  `qualified_kind()` so the five `format!("{}.{}", provider, kind)`
  open-codes in the dispatcher/audit/predicate paths converge on one
  source. `default_sensitive_path_patterns` becomes a `&'static
  [&'static str]` instead of a `Vec<String>` allocated on every approval
  check, and `is_sensitive_path_candidate` takes a borrowed iterator so
  custom and default patterns avoid cloning. The empty-fence stripper in
  `llm/tools/parse/syntax.rs` caches its `Regex` in a `OnceLock` instead
  of recompiling on every model turn.
