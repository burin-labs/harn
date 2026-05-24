# HARN-OWN-002 — mutable binding is never reassigned

## How to fix

- Switch the binding kind (`let` ↔ `mut`) to match its actual use.
- Restructure so owned values do not escape their scope.
