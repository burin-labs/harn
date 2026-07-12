- **`list<T>` and `dict<K, V>` are now covariant** in their element / value type
  (they were invariant). `list<int>` flows into `list<float>`, and a
  `list<{name: string}>` into a `list<dict>`. This is sound because Harn values
  have copy semantics — a widened binding is an independent copy, so there is no
  shared-mutable-aliasing hole. The `dict` key type stays invariant. The same
  classification applies to the `in`/`out` declaration-site variance checker.

- **Several trigger / trust-graph / project builtins now declare their real
  return type** instead of an opaque `dict`: `trigger_register` / `trigger_fire`
  / `trigger_replay` return `TriggerHandle` / `DispatchHandle`, `trust_record`
  returns `TrustRecord`, `trust_graph_query` returns `TrustScore`,
  `trust_graph_policy_for` returns `CapabilityPolicy`,
  `trust_graph_verify_chain` returns `TrustChainReport`, and
  `project_fingerprint` returns `ProjectFingerprint`. Code that consumed their
  results as a bare `dict` and reached for arbitrary keys should switch to the
  named fields (or narrow with `schema_is`).
