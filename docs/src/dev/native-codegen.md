# Native code generation (experimental)

The `harn-codegen` crate is an **experimental, opt-in** ahead-of-time / JIT
compiler that lowers a subset of Harn to native machine code via
[Cranelift](https://cranelift.dev/). It is a side project, not a shipped
feature.

> **Not part of the distributed binary.** `harn-codegen` is `publish = false`
> and is *not* a dependency of `harn-cli` or `harn-vm`. The crates.io `harn`
> binary never links Cranelift. The crate builds and tests under
> `cargo … --workspace` (so CI keeps it honest), but you opt in explicitly with
> `-p harn-codegen`.

## Why (and why not)

Harn is an orchestration language: real programs are dominated by LLM calls,
tool dispatch, and other I/O, where native code generation buys nothing — the
bottleneck is the network, not interpreter dispatch. So this is **not** a
general "make harnesses faster" feature, and it is deliberately kept out of the
default build.

The honest, narrow niche it targets is the opposite end of the spectrum: small,
hot, side-effect-free **numeric kernels** — scoring functions, parsers,
bit-twiddling, geometry, fixed-point math — that run in tight loops. For those,
compiling the bytecode to machine code removes interpreter overhead entirely.
If you have such a kernel and have measured it as a real cost, this is a tool
worth reaching for.

## The scalar subset

The compiler handles Harn's three unboxed scalar types — `int` (`i64`),
`float` (`f64`), and `bool` — and the operations over them:

- arithmetic `+ - * /` and integer `%` (trap-checked divide-by-zero; `i64::MIN
  / -1` wraps like the VM rather than trapping; an overflowing `+`/`-`/`*`/
  negation *deopts* — see [Overflow fidelity](#overflow-fidelity-guard-and-deopt));
- comparisons `== != < > <= >=` and logical `!`, `&&`, `||`;
- `if`/`else`, `while` loops, ternaries, short-circuit operators;
- `let`/`var` locals and reassignment.

Anything else — strings, lists, dicts, `nil` as a value, closures,
`harness.*`/host calls, `await`, `**` (its result type is value-dependent),
float `%` — is reported as `CodegenError::Unsupported`. Callers treat that as
"this function stays on the interpreter"; it is the expected outcome for most
functions, not an error.

Parameters must carry a scalar type annotation:

```harn,ignore
fn score(hits: int, misses: int) -> int {
  var total = 0
  var i = 0
  while i < hits {
    total = total + 10
    i = i + 1
  }
  return total - misses
}
```

## Pipeline

```text
Harn source ──(harn-vm front-end)──▶ Chunk / bytecode
           │
           ├─ decode   reachable scalar instructions
           ├─ verify   typed control-flow graph (ScalarFunction)
           ├─ lower    Cranelift IR
           └─ backend
                ├─ jit    in-process machine code  ▶ NativeFunction
                └─ aot    relocatable object file  ▶ ObjectArtifact
```

The front end is the real Harn compiler (`harn_vm::compile_source`) — the
native compiler never re-implements lexing, parsing, or bytecode emission, so
it always tracks the language the installed VM actually speaks. It then:

1. **decodes** only *reachable* bytecode (control-flow walk), so the unreachable
   `Nil; Return` epilogue the compiler appends after an explicit `return` never
   disqualifies an otherwise-scalar function;
2. **verifies** with a monotone type-inference fixpoint that assigns one static
   type to every operand-stack position and local slot, rejecting anything not
   provably monomorphic (an `int`-on-one-path/`float`-on-another slot, a
   mismatched control-flow merge);
3. **lowers** to Cranelift IR and hands off to a backend.

A pure-Rust reference interpreter (`harn_codegen::evaluate`) mirrors the VM's
semantics exactly (promote-on-overflow integer arithmetic surfaced as a deopt,
IEEE-754 floats with NaN ordering). It is the differential-test oracle and a
dependency-free fallback.

## Overflow fidelity (guard-and-deopt)

The native code's result is **always** bit-identical to the Harn VM, or an
explicit deopt — never a quietly wrong answer. The one place the monomorphic
representation could diverge from the VM is integer overflow.

The Harn VM does *not* wrap an overflowing integer `+`, `-`, `*`, or unary
negation: it **promotes the result to `float`** (see `int_add` and friends in
`harn-vm`), so `a + b` agrees with `[a, b].sum()` and never silently loses
magnitude. A monomorphic `int` kernel cannot represent that runtime int→float
type change. So instead of wrapping (which would make the JIT disagree with both
the interpreter and the VM), the JIT and the reference interpreter **guard**
those operations and return `NativeOutcome::Deopt(DeoptReason::IntegerOverflow)`
on overflow, signalling the caller to re-run on the interpreter or VM for the
true promoted value. This is the standard guard-and-deopt discipline of
production JITs (V8's Smi overflow path, for instance).

`call` / `evaluate` therefore return a `NativeOutcome` — either `Value(v)` (in
the subset) or `Deopt(reason)` — while genuine runtime errors (integer divide by
zero, which the VM raises too) stay on the `Err(NativeTrap)` channel.
`tests/vm_fidelity.rs` confirms the contract against the real VM: at the exact
inputs where the JIT deopts, the VM does promote to `float`; elsewhere the
values match bit-for-bit.

## Calling convention

Every compiled function is emitted with one uniform C signature, regardless of
its Harn arity or types:

```text
extern "C" fn(args: *const u64, ret: *mut u64, status: *mut u8)
```

Arguments and the result are raw 64-bit slots (`int` keeps its bits, `float`
uses IEEE-754 bits, `bool` is `0`/`1`). This keeps the Rust ↔ native boundary a
single monomorphic function pointer and gives the AOT object one stable,
easily-linked symbol (`harn_scalar_<name>`). The third pointer receives a status
byte: `0` = result in `*ret`; `1` = integer divide-by-zero (surfaces as a
`NativeTrap`, not a hardware trap that would abort the host); `2` = integer
overflow (surfaces as a `NativeOutcome::Deopt`).

## Using it

Library:

```text
use harn_codegen::{compile_named, NativeOutcome, ScalarValue};

let f = compile_named("fn add(a: int, b: int) -> int { return a + b }", "add")?;
match f.call(&[ScalarValue::Int(2), ScalarValue::Int(3)])? {
    NativeOutcome::Value(v) => assert_eq!(v, ScalarValue::Int(5)),
    // Deopt would mean the inputs overflowed and the VM promotes to float;
    // re-run on the interpreter/VM for the true value.
    NativeOutcome::Deopt(reason) => eprintln!("deopt: {reason}"),
}
```

CLI (`harn-nativec`):

```text
# Report whether a function is scalar-eligible and JIT it
harn-nativec kernel.harn score

# Emit a relocatable object you can link with any system linker
harn-nativec kernel.harn score --object score.o

# JIT and run with arguments (cross-checked against the reference interpreter)
harn-nativec kernel.harn score --run 12 3
```

## Tests

The tests are in-process and deterministic — no wall clock, no external
toolchain. `tests/native.rs` is a differential suite that compiles real Harn
source and asserts the JIT agrees with the reference interpreter across input
grids (including overflow deopts, divide-by-zero traps, `i64::MIN / -1`, and NaN
comparisons); `tests/aot.rs` checks object emission without invoking a linker;
`tests/vm_fidelity.rs` runs the same functions on the **real `harn-vm`
interpreter** (through a dev-only current-thread Tokio runtime) to prove the
native compiler's value/deopt/trap boundaries match the VM's actual
promote-on-overflow semantics.
