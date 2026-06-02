- VM: a script-controlled string-repeat count no longer crashes the host. `"a" * n`, `s.repeat(n)`,
  `str_pad`, and `pad_left`/`pad_right` now share one allocation guard (16 MiB) and return a clean
  runtime error for oversized output instead of exhausting memory or panicking `capacity overflow`
  (previously only the `repeat()` builtin was guarded).
