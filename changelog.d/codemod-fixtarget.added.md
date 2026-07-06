- **`harn codemod` rules can now rewrite a single sub-node of a match.** A raw
  `query` atomic matcher (used verbatim, must bind `@__match`) plus a `fixTarget`
  field splice the `fix` over only a named capture's span, leaving the rest of the
  node byte-for-byte intact — closing the data-loss gap where a whole-match
  `pattern`+`fix` dropped un-captured parts (e.g. a binding's `: Type` annotation).
