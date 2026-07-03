- **Bare enum-variant match patterns.** `match r { Ok(v) -> { … } Err(e) -> { … } }`
  now works without the `Result.` qualifier — a call-shaped pattern resolves to
  its enum whenever the variant name is declared by exactly one visible enum
  (ambiguity is a compile error asking for the qualified form; non-variant call
  patterns keep expression-equality semantics). Bare patterns bind payloads,
  count toward exhaustiveness, and work on user-declared enums.
