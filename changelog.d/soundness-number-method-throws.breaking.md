- **Calling an unknown method on a number now throws instead of returning `nil`.**
  Every method call on an `int` or `float` (e.g. `(3.14).round(2)`) used to
  evaluate to `nil` because the numeric method dispatcher returned `nil` for all
  names, silently swallowing typos and unsupported calls. It now throws a
  catchable "value of type … has no method" error, matching string / list / dict
  / set dispatch.
