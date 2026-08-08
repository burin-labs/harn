# HARN-OWN-001 — immutable binding is reassigned

## How to fix

- Declare the binding with `let` (mutable) instead of `const` (immutable) if it really needs to be reassigned.
- Restructure so owned values do not escape their scope.
