# HARN-OWN-001 — immutable binding is reassigned

## How to fix

- Switch the binding kind (`let` ↔ `mut`) to match its actual use.
- Restructure so owned values do not escape their scope.
