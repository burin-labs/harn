# HARN-OWN-002 — mutable binding is never reassigned

## How to fix

- Declare the binding with `const` (immutable) instead of `let` (mutable) since it is never reassigned.
- Restructure so owned values do not escape their scope.
