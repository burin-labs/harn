# HARN-OWN-004 — unvalidated boundary value is used directly

## How to fix

- Switch the binding kind (`let` ↔ `mut`) to match its actual use.
- Restructure so owned values do not escape their scope.
