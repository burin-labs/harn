# HARN-LNT-069 — helper can accept narrower capabilities

An ordinary function accepts root `Harness`, but every observed use selects
only one or two direct capability sub-handles. Root authority is appropriate
at entry and orchestration boundaries; reusable helpers should advertise the
smallest coherent capability interface they need.

## How to fix

Replace the root parameter with the nominal `Harness*` types named by the
diagnostic, and pass `harness.fs`, `harness.net`, or the corresponding
sub-handle at each call site.

Keep root `Harness` when the function genuinely coordinates several
capabilities or forwards authority. The lint suppresses itself when authority
escapes or the local syntax cannot prove that narrowing is safe.
