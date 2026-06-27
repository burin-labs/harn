- **`i64::MIN / -1` now promotes to float instead of silently wrapping.**
  Integer division wrapped this lone overflow back to `i64::MIN` — a wrong sign
  and magnitude — while `+`/`-`/`*`/negation already promote to float on
  overflow (the true value is `i64::MAX + 1`). The VM now promotes it, and the
  native code generator deopts to the VM for it (like the other overflowing ops)
  so the interpreter and JIT stay in agreement. `i64::MIN % -1` is `0` and is
  unchanged; division by zero still traps.
