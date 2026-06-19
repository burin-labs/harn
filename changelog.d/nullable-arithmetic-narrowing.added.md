- **Arithmetic on a possibly-nil operand is now a compile-time error, with
  control-flow narrowing for assignments.** `x + 1` where `x: int?` is flagged
  (`operand of '+' may be nil`) instead of throwing `nil + 1` at runtime, for
  `+ - * / % **`. A binding proven non-nil by an earlier assignment
  (`x = 5`), a `!= nil` guard, or `??` is narrowed and not flagged — assignment
  now participates in nil-narrowing (vars and `obj.field` paths), matching
  TypeScript/Flow control-flow narrowing. This also sharpens the existing
  nilable property-access diagnostics after an assignment.
