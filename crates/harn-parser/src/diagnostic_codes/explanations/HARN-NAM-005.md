# HARN-NAM-005 — method name does not exist on the receiver type

Raised when `receiver.method()` names a method the receiver's statically known
type does not have — either an interface-constraint method that no bound
declares, or a method that does not exist on a concrete builtin (`string`,
`list`, `set`, `int`, `float`, `bool`) or `struct` receiver.

## How to fix

- Fix the typo — the message suggests the closest available method.
- Call a method that exists on the type (see "available methods" in the help),
  or add it to the type's `impl` block.
- If the receiver is genuinely dynamic, narrow or annotate it so the intended
  type — and its methods — are in scope.
