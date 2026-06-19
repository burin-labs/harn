- **Set membership is now O(1).** `set` values previously stored only a plain
  list and rebuilt a hash index from every element on each `contains` /
  `union` / `intersect` / `difference` / subset/superset/disjoint call — O(n)
  work (plus an allocation) per query. They now carry a resident structural-key
  index alongside the items, so membership is O(1) and the set-algebra builtins
  and methods drop from rebuild-per-call to a single probe per element.
  Observable semantics (insertion-ordered iteration, structural dedup,
  order-independent equality and hashing) are unchanged.
