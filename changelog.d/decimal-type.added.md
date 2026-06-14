- **Exact `decimal` type for money and precise arithmetic.** A new `decimal`
  value type (96-bit base-10, up to 28–29 significant digits) backed by
  `rust_decimal`, so `decimal("0.1") + decimal("0.2")` is exactly `0.3` instead
  of the binary-float `0.30000000000000004`. Construct via the `decimal(value)`
  builtin (string/int/float/decimal; throws on un-parseable input rather than
  returning `nil`). Arithmetic (`+ - * / %`, unary `-`) promotes `int` operands
  exactly but refuses to mix with `float` (a compile-time error — binary float
  would corrupt exact values); `to_int`/`to_float`/`to_string` convert out.
  Equality and ordering are a clean island: `decimal` compares only against
  `decimal` (scale-insensitive, so `1.5 == 1.50`), and `decimal("1") == 1` is
  `false`. Decimals serialize across the host/JSON boundary as strings to
  preserve precision and bind natively to Postgres `NUMERIC`/`DECIMAL` columns.
