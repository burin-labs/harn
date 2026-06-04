# HARN-PAR-006 — integer literal is out of range for int (i64)

An integer literal must fit in a 64-bit signed integer (`int`), i.e. be in the
range `-9223372036854775808 ..= 9223372036854775807`. Harn does not silently
widen an out-of-range integer literal to a float, because that would lose both
the exact value (distinct literals collapse onto the same `float`) and the
`int` type.

## How to fix

- If you meant a floating-point value, write it with a decimal point or
  exponent so it lexes as a `float` (e.g. `9223372036854775808.0`).
- If you need the most negative `int`, build it by arithmetic — the sign is not
  part of the literal, so `9223372036854775808` overflows on its own:
  `-9223372036854775807 - 1`.
- Otherwise the value genuinely does not fit in `int`; rework the computation to
  stay within range.
