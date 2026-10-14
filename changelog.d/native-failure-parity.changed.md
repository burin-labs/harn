- Native VM runtime failures keep their identity instead of turning into
  prose. Division by zero stays `VmError::DivisionByZero` at the top level and
  renders as `Division by zero`; the source location comes from the stack trace
  that CLI diagnostics already print, not a `(line N)` suffix. An oversized
  string repetition or pad is the new `VmError::ValueByteLimit` and renders as
  `Value byte limit: repeat output would be N bytes (limit M)`. A
  parameter that fails its declared primitive type is `VmError::TypeError`,
  like a dict-shaped parameter already was, and renders as
  `Type error: function 'f' parameter 'p' expected int, got string (...)`
  instead of `Runtime error: TypeError: ...`.
- The portable-kernel differential now runs every portable runtime failure case
  through the native VM as well and compares the failure class.
