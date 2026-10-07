- Code-index `REFS` edges no longer depend on the order files are indexed. A file indexed before the
  file that declares a name it uses now still gets its edge, and editing a declaring file no longer
  drops every `REFS` edge into it, so `REFS` / `REFERENCED_BY` graph queries return every referrer.
  Word matching for `REFS` now uses the code index's own ASCII identifier tokenizer.
