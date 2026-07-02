- Lists now compare lexicographically (element by element, shorter prefix
  first), so multi-key sorts like `xs.sort_by({ x -> [x.a, x.b] })` order by
  the first key then the second instead of silently comparing equal. The same
  order backs `sort`, `min`/`max`, and the relational operators; a `NaN`
  element keeps the pair-style "unordered" semantics.
