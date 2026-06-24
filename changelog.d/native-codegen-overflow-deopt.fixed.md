- **The experimental native compiler (`harn-codegen`) no longer disagrees with
  the VM on integer overflow.** Integer `+`/`-`/`*`/negation now guard against
  `i64` overflow and deopt (`NativeOutcome::Deopt`) exactly where the VM
  promotes the result to `float`, instead of silently wrapping — so a
  JIT-compiled kernel is always bit-identical to the interpreter or signals a
  fall-back, never a quietly wrong answer. Adds a `tests/vm_fidelity.rs`
  differential suite that runs the same functions on the real `harn-vm`
  interpreter to prove the value/deopt/trap boundaries match.
