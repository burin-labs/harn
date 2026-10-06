- Code-index `CALLS` edges no longer depend on the order files are indexed. Call sites now resolve
  after each batch of index updates, so a call to a function declared in a file indexed later gets its
  edge, a later duplicate declaration removes an edge that relied on the name being unique, and a
  declaration that becomes unique adds one. `CALLS` / `CALLED_BY` graph queries return every caller.
