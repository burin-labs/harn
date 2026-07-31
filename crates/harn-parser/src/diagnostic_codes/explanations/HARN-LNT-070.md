# HARN-LNT-070 — public positional API is ambiguous

A public function has four or more positional parameters with the same type.
At call sites, swapping those values still type-checks and the argument order
does not explain each value's role.

## How to fix

Replace the homogeneous group with one named closed-record parameter. Build
that record with explicit field names at call sites and destructure it inside
the function.

This is informational API guidance, not a blanket arity limit. Private helpers,
heterogeneous signatures, defaults, and rest parameters are not reported.
