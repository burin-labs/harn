# HARN-LNT-031 — pointless comparison lint

## How to fix

- Apply the lint's auto-fix where one is offered (`harn lint --fix`).
- Suppress the lint with an attribute only when the surrounding code is intentionally non-idiomatic.

## NaN caveat

A self-comparison such as `x == x` or `x != x` is **not** constant when `x` is a
NaN float: `x == x` is `false` and `x != x` is `true` for NaN (and the same
holds element-wise for lists/dicts containing a NaN). The lint therefore only
offers an auto-fix when the operand provably cannot be NaN (a non-float literal,
or a list/dict built only from such literals). When the operand could be a float
the lint still warns — a self-comparison is usually a typo — but leaves the code
untouched and points you at `is_nan(...)`, the idiomatic NaN test.
