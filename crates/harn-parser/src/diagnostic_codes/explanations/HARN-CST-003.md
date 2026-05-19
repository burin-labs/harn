# HARN-CST-003 — const initializer attempted a sandboxed capability

**Category:** Const-eval sandbox (CST)
**Variant:** `Code::ConstEvalSandboxViolation`

## What it means

The compile-time evaluator denies any expression that would touch the
filesystem, network, environment, the current process, host bindings
(`harness.*`), the orchestration runtime, or any other ambient side
effect. Even a syntactically reachable call into one of those surfaces
is rejected before evaluation runs — the const-eval sandbox refuses to
mediate I/O or non-determinism.

```harn,ignore
// Rejected:
const X = read_file("/etc/passwd")
const Y = env("HOME")
const Z = spawn { ... }
const W = harness.clock.now()
```

## How to fix

- Move the impure expression into a `let` binding inside a pipeline.
- Replace I/O with a literal value or with a pure transformation of
  already-folded constants.

## Stability

This code is stable. The denylist is enforced by allowlist (only listed
pure builtins are accepted), so newly added stdlib functions stay
sandboxed by default.
